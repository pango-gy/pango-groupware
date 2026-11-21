//! 주기적 작업: 정리(만료된 세션, 지난 시도 제한 기록, 오래된 멱등성 키, 끝난 외부 알림 발송
//! 기록)와 하루 한 번의 연차 자동 부여.

use chrono::{Duration, NaiveDate};
use pango_api::leave::AccrualRunResult;
use pango_db::{idempotency, outbound, sessions, settings, Conn};

use super::leave;
use super::support::Company;
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 멱등성 키 보관 기간. 이 시간이 지난 재전송은 새 요청으로 처리된다.
const IDEMPOTENCY_RETENTION: Duration = Duration::hours(24);
/// 폐기·만료된 세션을 지우기 전 보관 기간(사고 조사용).
const SESSION_RETENTION: Duration = Duration::days(30);
/// 끝난(보냄·실패) 외부 알림 발송 기록의 보관 기간.
const OUTBOUND_RETENTION: Duration = Duration::days(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cleanup {
    pub sessions: u64,
    pub throttle_buckets: u64,
    pub idempotency_keys: u64,
    pub outbound_messages: u64,
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
    let outbound_cutoff = before(OUTBOUND_RETENTION)?;
    state
        .write(async |conn: &mut Conn| {
            Ok(Cleanup {
                sessions: sessions::purge(conn, session_cutoff).await?,
                throttle_buckets: sessions::throttle_purge(conn, throttle_cutoff).await?,
                idempotency_keys: idempotency::purge(conn, idempotency_cutoff).await?,
                outbound_messages: outbound::purge(conn, outbound_cutoff).await?,
            })
        })
        .await
}

/// 연차 자동 부여를 회사 시간대의 하루에 한 번 실행하기 위한 상태.
///
/// 부여 자체는 `grant_key`로 멱등이라 여러 번 돌아도 결과가 같다. 그래도 매시간 전 구성원을
/// 훑지 않도록 마지막으로 실행한 날짜를 메모리에 기억한다. 서버를 다시 띄우면 한 번 더
/// 실행되지만(시작 직후 1회) 그것도 아무것도 바꾸지 않는다.
///
/// 잔여 조회·휴가 신청은 그때그때 본인의 부여를 반영하므로 이 작업이 없어도 틀리지 않는다.
/// 이 작업은 **쓰지 않는 조회**(홈 화면)와 한동안 접속하지 않은 구성원의 원장을 맞춰 둔다.
#[derive(Debug, Default)]
pub struct DailyAccrual {
    last_run: Option<NaiveDate>,
}

impl DailyAccrual {
    /// 오늘(회사 시간대) 아직 실행하지 않았으면 실행하고 결과를 돌려준다. 이미 했으면 `None`.
    ///
    /// 실패하면 날짜를 기억하지 않으므로 다음 주기에 다시 시도한다. 회사 설정이 아직 없으면
    /// (초기화 전) 아무것도 하지 않는다.
    pub async fn run_if_due(
        &mut self,
        state: &AppState,
    ) -> ServiceResult<Option<AccrualRunResult>> {
        let Some(settings) = settings::get(&mut *state.read().await?).await? else {
            return Ok(None);
        };
        let today = Company::from_settings(settings)
            .map_err(ServiceError::internal)?
            .today(state.now());
        if self.last_run == Some(today) {
            return Ok(None);
        }
        let result = leave::run_scheduled_accrual(state, today).await?;
        self.last_run = Some(today);
        Ok(Some(result))
    }
}

/// 서버가 떠 있는 동안 주기적으로 정리하고, 날짜가 바뀌면 연차를 부여한다.
/// 실패해도 서버는 계속 돈다.
pub async fn run_forever(state: AppState, every: std::time::Duration) {
    let mut ticker = tokio::time::interval(every);
    let mut accrual = DailyAccrual::default();
    loop {
        ticker.tick().await;
        match run_once(&state).await {
            Ok(cleanup) => tracing::debug!(?cleanup, "maintenance done"),
            Err(error) => tracing::warn!(%error, "maintenance failed"),
        }
        match accrual.run_if_due(&state).await {
            Ok(Some(result)) => tracing::info!(?result, "daily leave accrual done"),
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "daily leave accrual failed"),
        }
    }
}
