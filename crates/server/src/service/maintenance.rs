//! 주기적 정리 작업: 만료된 세션, 지난 시도 제한 기록, 오래된 멱등성 키.

use chrono::Duration;
use pango_db::{idempotency, sessions, Conn};

use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 멱등성 키 보관 기간. 이 시간이 지난 재전송은 새 요청으로 처리된다.
const IDEMPOTENCY_RETENTION: Duration = Duration::hours(24);
/// 폐기·만료된 세션을 지우기 전 보관 기간(사고 조사용).
const SESSION_RETENTION: Duration = Duration::days(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cleanup {
    pub sessions: u64,
    pub throttle_buckets: u64,
    pub idempotency_keys: u64,
}

/// 한 번 정리한다. 여러 번 실행해도 안전하다.
pub async fn run_once(state: &AppState) -> ServiceResult<Cleanup> {
    let now = state.now();
    let before = |age: Duration| {
        now.checked_sub(age)
            .ok_or_else(|| ServiceError::internal("cleanup cutoff underflow"))
    };
    let session_cutoff = before(SESSION_RETENTION)?;
    // 가장 긴 창(계정 잠금)이 지난 기록만 지운다. 더 일찍 지우면 잠금이 풀려 버린다.
    let throttle_cutoff = before(
        state
            .config
            .login_account_window
            .max(state.config.login_ip_window),
    )?;
    let idempotency_cutoff = before(IDEMPOTENCY_RETENTION)?;
    state
        .write(async |conn: &mut Conn| {
            Ok(Cleanup {
                sessions: sessions::purge(conn, session_cutoff).await?,
                throttle_buckets: sessions::throttle_purge(conn, throttle_cutoff).await?,
                idempotency_keys: idempotency::purge(conn, idempotency_cutoff).await?,
            })
        })
        .await
}

/// 서버가 떠 있는 동안 주기적으로 정리한다. 실패해도 서버는 계속 돈다.
pub async fn run_forever(state: AppState, every: std::time::Duration) {
    let mut ticker = tokio::time::interval(every);
    loop {
        ticker.tick().await;
        match run_once(&state).await {
            Ok(cleanup) => tracing::debug!(?cleanup, "maintenance done"),
            Err(error) => tracing::warn!(%error, "maintenance failed"),
        }
    }
}
