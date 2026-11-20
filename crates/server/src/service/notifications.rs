//! 앱 내 알림: 본인 알림만 보고 읽음 처리한다.

use pango_api::notification::{NotificationDto, NotificationQuery, ReadAllResult, UnreadCount};
use pango_api::Page;
use pango_core::id::{MemberId, NotificationId};
use pango_core::DomainError;
use pango_db::notifications::{self, Notification};
use pango_db::Conn;

use super::support::{count_u32, next_page, AuthContext};
use crate::error::ServiceResult;
use crate::state::AppState;

fn dto(notification: Notification) -> NotificationDto {
    NotificationDto {
        id: notification.id,
        kind: notification.kind,
        title: notification.title,
        body: notification.body,
        link: notification.link,
        read_at: notification.read_at,
        created_at: notification.created_at,
    }
}

/// 내 알림, 최신순.
pub async fn list(
    state: &AppState,
    ctx: &AuthContext,
    query: &NotificationQuery,
) -> ServiceResult<Page<NotificationDto>> {
    let page = query.page();
    let before = page.after::<NotificationId>()?;
    let (items, has_more) = notifications::list(
        &mut *state.read().await?,
        ctx.member_id(),
        query.unread_only,
        before,
        page.limit(),
    )
    .await?;
    let items = items.into_iter().map(dto).collect();
    next_page(items, has_more, |last: &NotificationDto| last.id)
}

pub async fn unread_count(state: &AppState, ctx: &AuthContext) -> ServiceResult<UnreadCount> {
    unread_count_tx(&mut *state.read().await?, ctx.member_id()).await
}

/// 한 구성원의 읽지 않은 알림 수. 홈 화면도 같은 값을 쓴다.
pub async fn unread_count_tx(conn: &mut Conn, member_id: MemberId) -> ServiceResult<UnreadCount> {
    let count = notifications::count_unread(conn, member_id).await?;
    Ok(UnreadCount {
        unread: count_u32(count)?,
    })
}

/// 알림 하나를 읽음 처리한다. 이미 읽었으면 그대로 성공, 남의 알림이면 없는 것으로 취급한다.
pub async fn mark_read(
    state: &AppState,
    ctx: &AuthContext,
    id: NotificationId,
) -> ServiceResult<()> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            if notifications::mark_read(conn, ctx.member_id(), id, now).await? {
                Ok(())
            } else {
                Err(DomainError::NotFound.into())
            }
        })
        .await
}

/// 내 알림을 모두 읽음 처리한다. 쓰기가 직렬화되므로 동시에 보내도 각 알림은 한 번만 센다.
pub async fn mark_all_read(state: &AppState, ctx: &AuthContext) -> ServiceResult<ReadAllResult> {
    let now = state.now();
    let updated = state
        .write(async |conn: &mut Conn| {
            Ok(notifications::mark_all_read(conn, ctx.member_id(), now).await?)
        })
        .await?;
    Ok(ReadAllResult { updated })
}
