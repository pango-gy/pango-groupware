//! 감사 로그 조회(관리자). 조회 자체는 기록하지 않는다(조회할 때마다 로그가 늘어나면
//! 페이지를 넘기는 동안 목록이 움직인다).

use std::collections::{BTreeMap, BTreeSet};

use pango_api::admin::{AuditEntryDto, AuditLogQuery};
use pango_api::Page;
use pango_core::authz;
use pango_core::error::ValidationReason;
use pango_core::id::{AuditLogId, MemberId};
use pango_core::DomainError;
use pango_db::audit::{self, AuditEntry, AuditFilter};
use pango_db::{members, Conn};

use super::support::AuthContext;
use crate::error::{ServiceError, ServiceResult};
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
    Page::next(items, has_more, |last: &AuditEntryDto| last.id).map_err(ServiceError::internal)
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
    let actor_ids: Vec<MemberId> = entries
        .iter()
        .filter_map(|e| e.actor_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let names: BTreeMap<MemberId, String> = members::find_many(conn, &actor_ids)
        .await?
        .into_iter()
        .map(|m| (m.id, m.name))
        .collect();
    Ok(entries
        .into_iter()
        .map(|e| AuditEntryDto {
            actor_name: e.actor_id.and_then(|id| names.get(&id).cloned()),
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
