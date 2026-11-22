//! 퇴근 누락 알림: 출근만 하고 퇴근을 찍지 않은 구성원에게 업무 종료 1시간 뒤 한 번 알린다.
//!
//! 알림은 `support::notify`로 만들므로 앱 알림함과 외부 채널(웹훅·Buzz)로 함께 간다. 기록마다
//! `dedupe_key`가 있어 몇 번을 돌려도 한 번만 간다. 최근 근무일 두 개만 보므로 서버를 오래
//! 끈 뒤 켜도 지난 기록에 한꺼번에 알리지 않는다.

use chrono::Days;
use pango_core::attendance::clock_out_reminder_at;
use pango_core::calendar::DateRange;
use pango_core::member::MemberStatus;
use pango_core::time::Timestamp;
use pango_db::{attendance, Conn};

use super::support::{members_by_id, notify, Company, Notify};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 알림 종류.
pub const CLOCK_OUT_MISSING: &str = "attendance.clock_out_missing";

/// 보낼 차례가 된 퇴근 누락 알림을 만든다. 새로 만든 알림 수(이미 보낸 것 제외)를 돌려준다.
pub async fn remind_clock_outs(state: &AppState) -> ServiceResult<u64> {
    let now = state.now();
    // 대개 보낼 것이 없으므로 읽기 연결로 먼저 보고, 있을 때만 쓰기를 연다.
    if due_reminders(&mut *state.read().await?, now)
        .await?
        .is_empty()
    {
        return Ok(0);
    }
    state
        .write(async |conn: &mut Conn| {
            // 그사이 퇴근했거나 비활성화됐을 수 있으므로 쓰기 트랜잭션 안에서 다시 고른다.
            let due = due_reminders(conn, now).await?;
            let mut created = 0;
            for (member_id, record_id, title) in &due {
                let new = notify(
                    conn,
                    now,
                    Notify {
                        to: *member_id,
                        kind: CLOCK_OUT_MISSING,
                        title: title.clone(),
                        body: "퇴근할 때 꼭 퇴근을 남겨 주세요. 이미 퇴근했다면 근태에서 정정을 요청해 주세요."
                            .to_owned(),
                        link: Some("/attendance".to_owned()),
                        dedupe_key: Some(&format!("{CLOCK_OUT_MISSING}:{record_id}")),
                    },
                )
                .await?;
                created += u64::from(new);
            }
            Ok(created)
        })
        .await
}

/// `(구성원, 기록, 알림 제목)`. 재직 중인 구성원의 열린 기록 중 알림 시각이 지난 것.
async fn due_reminders(
    conn: &mut Conn,
    now: Timestamp,
) -> ServiceResult<
    Vec<(
        pango_core::id::MemberId,
        pango_core::id::AttendanceRecordId,
        String,
    )>,
> {
    // 초기화 전이면 보낼 것이 없다.
    let Some(company) = Company::load_if_initialized(conn).await? else {
        return Ok(Vec::new());
    };
    let today = company.work_date(now);
    let start = today
        .checked_sub_days(Days::new(1))
        .ok_or_else(|| ServiceError::internal("date underflow"))?;
    let open = attendance::list_open(conn, DateRange::new(start, today)?).await?;
    let owners = members_by_id(conn, open.iter().map(|row| row.record.member_id)).await?;
    Ok(open
        .into_iter()
        .filter(|row| {
            owners
                .get(&row.record.member_id)
                .is_some_and(|member| member.status == MemberStatus::Active)
        })
        .filter(|row| {
            let record = &row.record;
            clock_out_reminder_at(
                &company.zone,
                record.work_date,
                company.settings.work_end,
                record.clock_in_at,
            ) <= now
        })
        .map(|row| {
            let date = row.record.work_date.format("%-m월 %-d일");
            (
                row.record.member_id,
                row.record.id,
                format!("{date} 퇴근 기록이 없어요"),
            )
        })
        .collect())
}

/// 서버가 떠 있는 동안 주기적으로 퇴근 누락 알림을 보낸다. 실패해도 서버는 계속 돈다.
pub async fn run_forever(state: AppState, every: std::time::Duration) {
    let mut ticker = tokio::time::interval(every);
    loop {
        ticker.tick().await;
        match remind_clock_outs(&state).await {
            Ok(0) => {}
            Ok(sent) => tracing::info!(sent, "clock-out reminders created"),
            Err(error) => tracing::warn!(%error, "clock-out reminders failed"),
        }
    }
}
