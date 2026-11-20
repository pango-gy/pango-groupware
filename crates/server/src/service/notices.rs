//! 공지사항: 대상 범위에 맞는 열람·읽음 확인과 관리자의 작성·수정·보관.

use pango_api::notice::{
    NoticeDetail, NoticeInput, NoticeReadReceipt, NoticeReadReport, NoticeReader, NoticeSummary,
    NoticeUnreadCount,
};
use pango_api::{Page, PageQuery, Versioned};
use pango_core::authz::{self, Actor, Audience, AudienceScope};
use pango_core::error::ConflictReason;
use pango_core::id::{DepartmentId, NoticeId};
use pango_core::member::MemberStatus;
use pango_core::text::{Body, Title};
use pango_core::DomainError;
use pango_db::members::{self, Member, MemberFilter};
use pango_db::notices::{self, Notice, NoticeCursor, NoticeFilter, NoticeListItem, NoticeView};
use pango_db::{org, Conn};

use super::support::{
    audit, department_tree, found, notify, require_current_admin, require_updated, Audit,
    AuthContext, Idempotency, Notify,
};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 검증을 마친 공지 내용.
struct Content {
    title: String,
    body: String,
    pinned: bool,
    must_read: bool,
    audience: Audience,
}

impl Content {
    fn parse(input: &NoticeInput) -> ServiceResult<Self> {
        Ok(Self {
            title: Title::parse(&input.title)?.into_string(),
            body: Body::parse(&input.body)?.into_string(),
            pinned: input.pinned,
            must_read: input.must_read,
            audience: Audience::new(input.audience, input.department_id)?,
        })
    }
}

/// 행위자가 볼 수 있는 공지 범위.
async fn scope_of(conn: &mut Conn, actor: &Actor) -> ServiceResult<AudienceScope> {
    let ancestors = match actor.department_id {
        Some(own) => department_tree(conn).await?.ancestors_inclusive(own),
        None => Vec::new(),
    };
    Ok(authz::audience_scope(actor, &ancestors))
}

fn visible_departments(scope: &AudienceScope) -> Option<Vec<DepartmentId>> {
    match scope {
        AudienceScope::Everything => None,
        AudienceScope::Departments(ids) => Some(ids.iter().copied().collect()),
    }
}

/// 볼 수 있는 공지 하나. 보관됐거나 범위 밖이면 없는 것으로 취급한다.
async fn find_visible(conn: &mut Conn, actor: &Actor, id: NoticeId) -> ServiceResult<NoticeView> {
    let view = found(notices::find_view(conn, id, actor.member_id).await?)?;
    if view.item.archived_at.is_some() {
        return Err(DomainError::NotFound.into());
    }
    authz::view_audience(&scope_of(conn, actor).await?, &view.item.audience)?;
    Ok(view)
}

/// 공지 대상 부서는 있어야 하고 보관되지 않아야 한다. 보관된 부서는 조직도에서
/// 사라진 부서이므로 없는 부서와 같이 취급한다.
async fn check_department(conn: &mut Conn, audience: &Audience) -> ServiceResult<()> {
    if let Some(id) = audience.department_id() {
        found(
            org::find_department(conn, id)
                .await?
                .filter(|d| d.archived_at.is_none()),
        )?;
    }
    Ok(())
}

/// 지금 대상 범위에 속한 재직 구성원(이름순). 부서 대상이면 하위 부서까지.
async fn audience_members(conn: &mut Conn, audience: &Audience) -> ServiceResult<Vec<Member>> {
    let departments = match audience.department_id() {
        Some(id) => Some(
            department_tree(conn)
                .await?
                .descendants_inclusive(id)
                .into_iter()
                .collect(),
        ),
        None => None,
    };
    let filter = MemberFilter {
        statuses: vec![MemberStatus::Active],
        departments,
        ..MemberFilter::default()
    };
    Ok(members::list(conn, &filter).await?)
}

fn summary(item: NoticeListItem) -> NoticeSummary {
    NoticeSummary {
        id: item.id,
        title: item.title,
        author_id: item.author_id,
        author_name: item.author_name,
        pinned: item.pinned,
        must_read: item.must_read,
        audience: item.audience.kind(),
        department_id: item.audience.department_id(),
        department_name: item.department_name,
        published_at: item.published_at,
        read: item.read_at.is_some(),
        archived_at: item.archived_at,
        version: item.version,
    }
}

fn detail(view: NoticeView) -> NoticeDetail {
    NoticeDetail {
        read_at: view.item.read_at,
        summary: summary(view.item),
        body: view.body,
    }
}

async fn load_detail(
    conn: &mut Conn,
    ctx: &AuthContext,
    id: NoticeId,
) -> ServiceResult<NoticeDetail> {
    Ok(detail(found(
        notices::find_view(conn, id, ctx.member_id()).await?,
    )?))
}

async fn page(
    conn: &mut Conn,
    filter: &NoticeFilter<'_>,
    query: &PageQuery,
) -> ServiceResult<Page<NoticeSummary>> {
    let after = query.after::<NoticeCursor>()?;
    let (items, has_more) = notices::list(conn, filter, after, query.limit()).await?;
    let items = items.into_iter().map(summary).collect();
    Page::next(items, has_more, |last: &NoticeSummary| NoticeCursor {
        pinned: last.pinned,
        published_at: last.published_at,
        id: last.id,
    })
    .map_err(ServiceError::internal)
}

/// 내가 볼 수 있는 공지 목록. 고정 공지 먼저, 그다음 최신순.
pub async fn list(
    state: &AppState,
    ctx: &AuthContext,
    query: &PageQuery,
) -> ServiceResult<Page<NoticeSummary>> {
    list_tx(&mut *state.read().await?, &ctx.actor, query).await
}

/// [`list`]의 본체. 홈 화면의 최근 공지도 이것으로 읽으므로 보이는 범위가 항상 같다.
pub async fn list_tx(
    conn: &mut Conn,
    actor: &Actor,
    query: &PageQuery,
) -> ServiceResult<Page<NoticeSummary>> {
    let scope = scope_of(conn, actor).await?;
    let departments = visible_departments(&scope);
    let filter = NoticeFilter {
        reader: actor.member_id,
        visible_departments: departments.as_deref(),
        include_archived: false,
    };
    page(conn, &filter, query).await
}

/// 공지 상세. 열람만으로는 읽음 처리하지 않는다(필독 확인은 명시적인 동작이어야 한다).
pub async fn get(state: &AppState, ctx: &AuthContext, id: NoticeId) -> ServiceResult<NoticeDetail> {
    let mut conn = state.read().await?;
    find_visible(&mut conn, &ctx.actor, id).await.map(detail)
}

/// 읽음 확인. 여러 번(동시에) 보내도 한 줄만 남고 첫 확인 시각이 유지된다.
pub async fn mark_read(
    state: &AppState,
    ctx: &AuthContext,
    id: NoticeId,
) -> ServiceResult<NoticeReadReceipt> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            find_visible(conn, &ctx.actor, id).await?;
            notices::mark_read(conn, id, ctx.member_id(), now).await?;
            let view = found(notices::find_view(conn, id, ctx.member_id()).await?)?;
            let read_at = view
                .item
                .read_at
                .ok_or_else(|| ServiceError::internal("read receipt missing after insert"))?;
            Ok(NoticeReadReceipt {
                notice_id: id,
                read_at,
            })
        })
        .await
}

/// 배지용: 볼 수 있는 공지 중 읽지 않은 수와 그중 필독 수.
pub async fn unread_count(state: &AppState, ctx: &AuthContext) -> ServiceResult<NoticeUnreadCount> {
    unread_count_tx(&mut *state.read().await?, &ctx.actor).await
}

/// [`unread_count`]의 본체. 홈 화면도 같은 값을 쓴다.
pub async fn unread_count_tx(conn: &mut Conn, actor: &Actor) -> ServiceResult<NoticeUnreadCount> {
    let scope = scope_of(conn, actor).await?;
    let departments = visible_departments(&scope);
    let filter = NoticeFilter {
        reader: actor.member_id,
        visible_departments: departments.as_deref(),
        include_archived: false,
    };
    let (unread, must_read) = notices::count_unread(conn, &filter).await?;
    let count = |n: i64| u32::try_from(n).map_err(ServiceError::internal);
    Ok(NoticeUnreadCount {
        unread: count(unread)?,
        must_read_unread: count(must_read)?,
    })
}

/// 관리자 목록: 대상과 무관하게 보관된 공지까지.
pub async fn admin_list(
    state: &AppState,
    ctx: &AuthContext,
    query: &PageQuery,
) -> ServiceResult<Page<NoticeSummary>> {
    authz::require_admin(&ctx.actor)?;
    let filter = NoticeFilter {
        reader: ctx.member_id(),
        visible_departments: None,
        include_archived: true,
    };
    page(&mut *state.read().await?, &filter, query).await
}

/// 공지를 게시하고 대상 범위의 재직 구성원에게 알린다(작성자 본인 제외).
///
/// 알림은 공지·구성원마다 `dedupe_key`가 있어, 같은 요청이 다시 실행되더라도 두 번
/// 만들어지지 않는다.
pub async fn create(
    state: &AppState,
    ctx: &AuthContext,
    input: &NoticeInput,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<NoticeDetail> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let content = Content::parse(input)?;
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            check_department(conn, &content.audience).await?;
            let notice = Notice {
                id: NoticeId::new(),
                title: content.title.clone(),
                body: content.body.clone(),
                author_id: ctx.member_id(),
                pinned: content.pinned,
                must_read: content.must_read,
                audience: content.audience,
                published_at: now,
                archived_at: None,
                version: 1,
            };
            notices::insert(conn, &notice, now).await?;

            let dedupe_key = format!("notice.new:{}", notice.id);
            let body = if notice.must_read {
                "필독 공지가 게시되었습니다."
            } else {
                "새 공지가 게시되었습니다."
            };
            for member in audience_members(conn, &notice.audience).await? {
                if member.id == notice.author_id {
                    continue;
                }
                notify(
                    conn,
                    now,
                    Notify {
                        to: member.id,
                        kind: "notice.new",
                        title: notice.title.clone(),
                        body: body.to_owned(),
                        link: Some(format!("/notices/{}", notice.id)),
                        dedupe_key: Some(&dedupe_key),
                    },
                )
                .await?;
            }
            audit(
                conn,
                now,
                Audit {
                    action: "notice.create",
                    target_type: "notice",
                    target_id: Some(notice.id.to_string()),
                    detail: Some(audience_detail(&notice)),
                    ctx: Some(ctx),
                },
            )
            .await?;
            load_detail(conn, ctx, notice.id).await
        })
        .await
}

fn audience_detail(notice: &Notice) -> serde_json::Value {
    serde_json::json!({
        "audience": notice.audience.kind().as_str(),
        "department_id": notice.audience.department_id(),
        "pinned": notice.pinned,
        "must_read": notice.must_read,
    })
}

/// 공지 수정. 보관된 공지는 고칠 수 없다. 다시 알리지 않는다.
pub async fn update(
    state: &AppState,
    ctx: &AuthContext,
    id: NoticeId,
    request: &Versioned<NoticeInput>,
) -> ServiceResult<NoticeDetail> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let content = Content::parse(&request.data)?;
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            let existing = found(notices::find(conn, id).await?)?;
            if existing.archived_at.is_some() {
                return Err(DomainError::Conflict(ConflictReason::NoticeArchived).into());
            }
            check_department(conn, &content.audience).await?;
            let updated = Notice {
                title: content.title.clone(),
                body: content.body.clone(),
                pinned: content.pinned,
                must_read: content.must_read,
                audience: content.audience,
                version: request.version,
                ..existing
            };
            require_updated(notices::update(conn, &updated, now).await?)?;
            audit(
                conn,
                now,
                Audit {
                    action: "notice.update",
                    target_type: "notice",
                    target_id: Some(id.to_string()),
                    detail: Some(audience_detail(&updated)),
                    ctx: Some(ctx),
                },
            )
            .await?;
            load_detail(conn, ctx, id).await
        })
        .await
}

/// 공지 보관(삭제 대신). 이미 보관됐으면 버전과 무관하게 그대로 성공한다(자연 멱등).
pub async fn archive(
    state: &AppState,
    ctx: &AuthContext,
    id: NoticeId,
    version: i64,
) -> ServiceResult<NoticeDetail> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            let existing = found(notices::find(conn, id).await?)?;
            if existing.archived_at.is_none() {
                let archived = Notice {
                    archived_at: Some(now),
                    version,
                    ..existing
                };
                require_updated(notices::update(conn, &archived, now).await?)?;
                audit(
                    conn,
                    now,
                    Audit {
                        action: "notice.archive",
                        target_type: "notice",
                        target_id: Some(id.to_string()),
                        detail: None,
                        ctx: Some(ctx),
                    },
                )
                .await?;
            }
            load_detail(conn, ctx, id).await
        })
        .await
}

/// 읽음 현황: 읽은 사람 전부와, 지금 대상 범위의 재직 구성원 중 읽지 않은 사람.
pub async fn read_report(
    state: &AppState,
    ctx: &AuthContext,
    id: NoticeId,
) -> ServiceResult<NoticeReadReport> {
    authz::require_admin(&ctx.actor)?;
    let mut conn = state.read().await?;
    let notice = found(notices::find(&mut conn, id).await?)?;
    let reads = notices::list_reads(&mut conn, id).await?;
    let audience = audience_members(&mut conn, &notice.audience).await?;
    let unread = audience
        .into_iter()
        .filter(|member| reads.iter().all(|read| read.member_id != member.id))
        .map(|member| NoticeReader {
            member_id: member.id,
            name: member.name,
            read_at: None,
        })
        .collect();
    let read = reads
        .into_iter()
        .map(|read| NoticeReader {
            member_id: read.member_id,
            name: read.name,
            read_at: Some(read.read_at),
        })
        .collect();
    Ok(NoticeReadReport { read, unread })
}
