//! 서비스 공통 도구: 요청 맥락, 트랜잭션, 감사 로그, 회사 설정, 알림.

use chrono::NaiveDate;
use pango_core::attendance::{BreakTier, WorkPolicy};
use pango_core::authz::Actor;
use pango_core::calendar::{DateRange, WorkCalendar};
use pango_core::error::ConflictReason;
use pango_core::id::{AuditLogId, MemberId, NotificationId, SessionId};
use pango_core::org::DepartmentTree;
use pango_core::time::{parse_timezone, Timestamp, WorkCalendarZone};
use pango_core::DomainError;
use pango_db::audit::AuditEntry;
use pango_db::idempotency::{self, StoredResponse};
use pango_db::members::Member;
use pango_db::notifications::Notification;
use pango_db::sessions::SessionVia;
use pango_db::settings::CompanySettings;
use pango_db::{audit, notifications, org, settings, Conn, ReadConn};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 요청의 출처 정보. 감사 로그와 세션 목록에 남긴다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestMeta {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// 인증된 요청의 맥락.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub actor: Actor,
    pub member: Member,
    pub session_id: SessionId,
    pub via: SessionVia,
    pub meta: RequestMeta,
}

impl AuthContext {
    pub fn member_id(&self) -> MemberId {
        self.actor.member_id
    }
}

/// 클라이언트가 보낸 멱등성 키와 요청 지문.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Idempotency {
    pub key: String,
    pub fingerprint: [u8; 32],
}

impl AppState {
    pub async fn read(&self) -> ServiceResult<ReadConn> {
        Ok(self.db.read().await?)
    }

    /// 쓰기 트랜잭션 안에서 `op`를 실행한다. `op`가 실패하면 전부 롤백된다.
    pub async fn write<T>(
        &self,
        op: impl AsyncFnOnce(&mut Conn) -> ServiceResult<T>,
    ) -> ServiceResult<T> {
        let mut tx = self.db.begin_write().await?;
        let output = op(&mut tx).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(output)
    }

    /// [`AppState::write`]와 같지만, 멱등성 키가 있으면 첫 성공 응답을 저장해 두고
    /// 같은 요청이 다시 오면 `op`를 실행하지 않고 그 응답을 돌려준다.
    ///
    /// 키 조회·실행·저장이 한 트랜잭션이고 쓰기는 직렬화되므로, 같은 키의 요청이
    /// 동시에 와도 `op`는 한 번만 실행된다. 실패한 요청은 저장하지 않는다(재시도 가능).
    pub async fn write_idempotent<T: Serialize + DeserializeOwned>(
        &self,
        member_id: MemberId,
        idempotency: Option<&Idempotency>,
        op: impl AsyncFnOnce(&mut Conn) -> ServiceResult<T>,
    ) -> ServiceResult<T> {
        let Some(idem) = idempotency else {
            return self.write(op).await;
        };
        let now = self.now();
        self.write(async |conn: &mut Conn| {
            if let Some(stored) = idempotency::find(conn, member_id, &idem.key).await? {
                if stored.fingerprint != idem.fingerprint {
                    return Err(ServiceError::IdempotencyKeyReused);
                }
                return serde_json::from_str(&stored.body).map_err(ServiceError::internal);
            }
            let output = op(conn).await?;
            let body = serde_json::to_string(&output).map_err(ServiceError::internal)?;
            let stored = StoredResponse {
                fingerprint: idem.fingerprint,
                status: 200,
                body,
            };
            idempotency::insert(conn, member_id, &idem.key, &stored, now).await?;
            Ok(output)
        })
        .await
    }
}

/// 낙관적 잠금 갱신 결과를 오류로 바꾼다. 호출 전에 대상의 존재는 이미 확인했어야 한다.
pub fn require_updated(updated: bool) -> ServiceResult<()> {
    if updated {
        Ok(())
    } else {
        Err(DomainError::Conflict(ConflictReason::VersionConflict).into())
    }
}

/// 조회 결과가 없으면 `NotFound`.
pub fn found<T>(value: Option<T>) -> ServiceResult<T> {
    value.ok_or(ServiceError::Domain(DomainError::NotFound))
}

/// 감사 로그에 남길 행위. `action`은 `대상.동작` 형식(예: `member.create`).
pub struct Audit<'a> {
    pub action: &'static str,
    pub target_type: &'static str,
    pub target_id: Option<String>,
    pub detail: Option<serde_json::Value>,
    /// 시스템 작업이면 `None`.
    pub ctx: Option<&'a AuthContext>,
}

pub async fn audit(conn: &mut Conn, now: Timestamp, record: Audit<'_>) -> ServiceResult<()> {
    let entry = AuditEntry {
        id: AuditLogId::new(),
        at: now,
        actor_id: record.ctx.map(AuthContext::member_id),
        action: record.action.to_owned(),
        target_type: record.target_type.to_owned(),
        target_id: record.target_id,
        detail: record.detail,
        ip: record.ctx.and_then(|ctx| ctx.meta.ip.clone()),
    };
    Ok(audit::insert(conn, &entry).await?)
}

/// 앱 내 알림을 만든다. `dedupe_key`가 같으면 한 번만 만들어진다.
pub struct Notify<'a> {
    pub to: MemberId,
    pub kind: &'static str,
    pub title: String,
    pub body: String,
    pub link: Option<String>,
    pub dedupe_key: Option<&'a str>,
}

pub async fn notify(conn: &mut Conn, now: Timestamp, message: Notify<'_>) -> ServiceResult<()> {
    let notification = Notification {
        id: NotificationId::new(),
        member_id: message.to,
        kind: message.kind.to_owned(),
        title: message.title,
        body: message.body,
        link: message.link,
        dedupe_key: message.dedupe_key.map(str::to_owned),
        read_at: None,
        created_at: now,
    };
    notifications::insert(conn, &notification).await?;
    Ok(())
}

/// 구성원의 권한 판정용 정보를 만든다(조직장 관할 부서 포함).
pub async fn build_actor(conn: &mut Conn, member: &Member) -> ServiceResult<Actor> {
    let headed = org::departments_headed_by(conn, member.id).await?;
    let managed_departments = if headed.is_empty() {
        Default::default()
    } else {
        department_tree(conn).await?.managed_by(headed)
    };
    Ok(Actor {
        member_id: member.id,
        role: member.role,
        department_id: member.department_id,
        managed_departments,
    })
}

pub async fn department_tree(conn: &mut Conn) -> ServiceResult<DepartmentTree> {
    let departments = org::list_departments(conn).await?;
    // 저장된 트리가 깨져 있으면(순환 등) 요청을 처리할 수 없다.
    DepartmentTree::new(departments.iter().map(|d| d.node())).map_err(ServiceError::internal)
}

/// 회사 설정과 거기서 만든 도메인 값.
#[derive(Debug, Clone)]
pub struct Company {
    pub settings: CompanySettings,
    pub zone: WorkCalendarZone,
    pub policy: WorkPolicy,
}

impl Company {
    /// 설정이 없으면(초기화 전) 내부 오류.
    pub async fn load(conn: &mut Conn) -> ServiceResult<Self> {
        let settings = settings::get(conn)
            .await?
            .ok_or_else(|| ServiceError::internal("company settings are not initialized"))?;
        Self::from_settings(settings).map_err(ServiceError::internal)
    }

    /// 설정값이 도메인 규칙에 맞는지 확인하며 변환한다. 설정 저장 전 검증에도 쓴다.
    pub fn from_settings(settings: CompanySettings) -> Result<Self, DomainError> {
        // 근무일 전환 시각이 근무 시작보다 늦으면, 정시 출근이 전날 근무일로 잡혀 버린다.
        if settings.day_cutoff > settings.work_start {
            return Err(DomainError::validation(
                "day_cutoff",
                pango_core::error::ValidationReason::Inconsistent,
            ));
        }
        let zone = WorkCalendarZone::new(parse_timezone(&settings.timezone)?, settings.day_cutoff)?;
        let tiers = settings
            .break_tiers
            .iter()
            .map(|&(min_span_minutes, break_minutes)| BreakTier {
                min_span_minutes,
                break_minutes,
            })
            .collect();
        let policy = WorkPolicy::new(
            settings.work_start,
            settings.work_end,
            settings.late_grace_minutes,
            settings.half_day_split,
            settings.quarter_minutes,
            tiers,
        )?;
        Ok(Self {
            settings,
            zone,
            policy,
        })
    }

    pub fn today(&self, now: Timestamp) -> NaiveDate {
        self.zone.calendar_date(now)
    }

    pub fn work_date(&self, now: Timestamp) -> NaiveDate {
        self.zone.work_date(now)
    }

    /// 주어진 기간의 근무 달력(근무 요일 + 공휴일).
    pub async fn calendar(&self, conn: &mut Conn, range: DateRange) -> ServiceResult<WorkCalendar> {
        let holidays = settings::list_holidays(conn, range).await?;
        Ok(WorkCalendar::new(
            self.settings.workdays,
            holidays.into_iter().map(|h| h.date),
        ))
    }
}

/// CSV 한 줄을 덧붙인다(RFC 4180, 줄 끝 CRLF).
///
/// 내보낸 파일은 스프레드시트에서 열리므로, `=`·`+`·`-`·`@`·탭·CR로 시작하는 칸은 앞에 `'`를
/// 붙여 수식으로 실행되지 않게 한다(CSV 수식 주입 방어). 쉼표·따옴표·줄바꿈이 있는 칸은
/// 따옴표로 감싸고 안의 따옴표는 두 번 쓴다.
pub fn push_csv_row<S: AsRef<str>>(out: &mut String, cells: impl IntoIterator<Item = S>) {
    for (index, cell) in cells.into_iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let cell = cell.as_ref();
        let neutralized = cell.starts_with(['=', '+', '-', '@', '\t', '\r']);
        let quoted = cell.contains([',', '"', '\n', '\r']);
        if quoted {
            out.push('"');
        }
        if neutralized {
            out.push('\'');
        }
        out.push_str(&cell.replace('"', "\"\""));
        if quoted {
            out.push('"');
        }
    }
    out.push_str("\r\n");
}

/// 쓰기 트랜잭션 안에서 행위자의 권한 정보를 다시 읽는다.
///
/// 인증(트랜잭션 밖)과 커밋 사이에 행위자의 역할이 바뀌거나 계정이 정지됐을 수 있다.
/// 권한을 주고받는 관리 동작은 이 값으로 판정해, 방금 강등된 관리자의 요청이 이전 권한으로
/// 처리되지 않게 한다. 쓰기는 직렬화되므로 커밋 시점까지 이 값이 유지된다.
pub async fn current_actor(conn: &mut Conn, ctx: &AuthContext) -> ServiceResult<Actor> {
    let member = pango_db::members::find(conn, ctx.member_id())
        .await?
        .filter(|member| member.status.can_sign_in())
        .ok_or(ServiceError::Domain(DomainError::Unauthenticated))?;
    build_actor(conn, &member).await
}

/// 수정 요청의 변경 내역. 감사 로그 `detail`에 `{"changed": {"필드": {"from": …, "to": …}}}`로
/// 남기고, 비어 있으면 수정할 것이 없다는 뜻이다(버전을 올리지 않는다).
#[derive(Debug, Default)]
pub struct Changes(serde_json::Map<String, serde_json::Value>);

impl Changes {
    /// 값이 다를 때만 기록한다. 비밀 값(비밀번호 등)은 넣지 않는다.
    pub fn field<T: Serialize + PartialEq>(&mut self, name: &str, from: &T, to: &T) -> &mut Self {
        if from != to {
            self.0.insert(
                name.to_owned(),
                serde_json::json!({ "from": from, "to": to }),
            );
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_detail(self) -> serde_json::Value {
        serde_json::json!({ "changed": self.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str]) -> String {
        let mut out = String::new();
        push_csv_row(&mut out, cells);
        out
    }

    #[test]
    fn csv_cells_that_start_a_formula_are_neutralized() {
        assert_eq!(
            row(&["=1+1", "+cmd", "-2", "@SUM(A1)"]),
            "'=1+1,'+cmd,'-2,'@SUM(A1)\r\n"
        );
        assert_eq!(row(&["\tx", "\rx"]), "'\tx,\"'\rx\"\r\n");
        assert_eq!(row(&["a=b", "1-2", "메일@회사"]), "a=b,1-2,메일@회사\r\n");
    }

    #[test]
    fn csv_cells_with_separators_are_quoted() {
        assert_eq!(
            row(&["a,b", "say \"hi\"", "line\nbreak", ""]),
            "\"a,b\",\"say \"\"hi\"\"\",\"line\nbreak\",\r\n"
        );
        assert_eq!(
            row(&[r#"=HYPERLINK("http://evil","x")"#]),
            "\"'=HYPERLINK(\"\"http://evil\"\",\"\"x\"\")\"\r\n"
        );
        assert_eq!(row(&[]), "\r\n");
    }
}
