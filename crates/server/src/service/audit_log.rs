//! 감사 로그 조회(관리자). 조회 자체는 기록하지 않는다(조회할 때마다 로그가 늘어나면
//! 페이지를 넘기는 동안 목록이 움직인다).

use pango_api::admin::{AuditEntryDto, AuditLogQuery};
use pango_api::Page;
use pango_core::authz;
use pango_core::error::ValidationReason;
use pango_core::id::AuditLogId;
use pango_core::DomainError;
use pango_db::audit::{self, AuditEntry, AuditFilter};
use std::collections::{BTreeMap, BTreeSet};

use pango_core::id::MemberId;
use pango_db::{approval, leave, notices, org, Conn};

use super::support::{member_names, next_page, AuthContext};
use crate::error::ServiceResult;
use crate::state::AppState;

/// 최신순 목록. `from`은 포함, `until`은 제외.
pub async fn list(
    state: &AppState,
    ctx: &AuthContext,
    query: &AuditLogQuery,
) -> ServiceResult<Page<AuditEntryDto>> {
    authz::require_admin(&ctx.actor)?;
    if let (Some(from), Some(until)) = (query.from, query.until) {
        if until < from {
            return Err(DomainError::validation("until", ValidationReason::Inconsistent).into());
        }
    }
    let page = query.page();
    let filter = AuditFilter {
        actor_id: query.actor_id,
        action: query.action.clone(),
        target_type: query.target_type.clone(),
        target_id: query.target_id.clone(),
        from: query.from,
        until: query.until,
        before: page.after::<AuditLogId>()?,
    };
    let mut conn = state.read().await?;
    let (entries, has_more) = audit::list(&mut conn, &filter, page.limit()).await?;
    let items = with_actor_names(&mut conn, entries).await?;
    next_page(items, has_more, |last: &AuditEntryDto| last.id)
}

/// 가장 최근 `limit`건(관리 대시보드). 권한은 호출한 쪽이 확인한다.
pub async fn recent_tx(conn: &mut Conn, limit: u32) -> ServiceResult<Vec<AuditEntryDto>> {
    let (entries, _) = audit::list(conn, &AuditFilter::default(), limit).await?;
    with_actor_names(conn, entries).await
}

async fn with_actor_names(
    conn: &mut Conn,
    entries: Vec<AuditEntry>,
) -> ServiceResult<Vec<AuditEntryDto>> {
    let names = member_names(conn, entries.iter().filter_map(|e| e.actor_id)).await?;
    let targets = target_names(conn, &entries).await?;
    Ok(entries
        .into_iter()
        .map(|e| AuditEntryDto {
            actor_name: e.actor_id.and_then(|id| names.get(&id).cloned()),
            target_name: e
                .target_id
                .as_ref()
                .and_then(|id| targets.get(&(e.target_type.clone(), id.clone())).cloned()),
            id: e.id,
            at: e.at,
            actor_id: e.actor_id,
            action: e.action,
            target_type: e.target_type,
            target_id: e.target_id,
            detail: e.detail,
            ip: e.ip,
        })
        .collect())
}

/// 대상 ID를 사람이 알아볼 이름으로 바꾼다. 종류별로 한 번씩만 조회한다(목록은 최대 200건).
///
/// 이름이 없는 종류(로그인 기기 등)나 이미 지워진 대상은 빠진다. 화면은 그때 종류만 보여 준다.
async fn target_names(
    conn: &mut Conn,
    entries: &[AuditEntry],
) -> ServiceResult<BTreeMap<(String, String), String>> {
    let ids_of = |kind: &str| -> BTreeSet<String> {
        entries
            .iter()
            .filter(|e| e.target_type == kind)
            .filter_map(|e| e.target_id.clone())
            .collect()
    };
    let mut names = BTreeMap::new();
    let mut put = |kind: &str, id: String, name: String| {
        names.insert((kind.to_owned(), id), name);
    };

    let member_ids: Vec<MemberId> = ids_of("member")
        .iter()
        .filter_map(|id| id.parse().ok())
        .collect();
    for (id, name) in member_names(conn, member_ids).await? {
        put("member", id.to_string(), name);
    }
    for id in ids_of("department") {
        if let Ok(parsed) = id.parse() {
            if let Some(department) = org::find_department(conn, parsed).await? {
                put("department", id, department.name);
            }
        }
    }
    for id in ids_of("position") {
        if let Ok(parsed) = id.parse() {
            if let Some(position) = org::find_position(conn, parsed).await? {
                put("position", id, position.name);
            }
        }
    }
    for id in ids_of("notice") {
        if let Ok(parsed) = id.parse() {
            if let Some(notice) = notices::find(conn, parsed).await? {
                put("notice", id, notice.title);
            }
        }
    }
    for id in ids_of("approval_document") {
        if let Ok(parsed) = id.parse() {
            if let Some(document) = approval::find_document(conn, parsed).await? {
                put("approval_document", id, document.title);
            }
        }
    }
    for id in ids_of("approval_form") {
        if let Ok(parsed) = id.parse() {
            if let Some(form) = approval::find_form(conn, parsed).await? {
                put("approval_form", id, form.name);
            }
        }
    }
    for id in ids_of("leave_type") {
        if let Ok(parsed) = id.parse() {
            if let Some(leave_type) = leave::find_type(conn, parsed).await? {
                put("leave_type", id, leave_type.name);
            }
        }
    }
    // 공휴일은 날짜 자체가 이름이다.
    for id in ids_of("holiday") {
        put("holiday", id.clone(), id);
    }
    Ok(names)
}
