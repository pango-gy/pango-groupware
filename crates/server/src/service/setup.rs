//! 최초 설정: 회사 정보와 최고 관리자 계정을 만든다.

use chrono::NaiveDate;
use pango_core::authz::Role;
use pango_core::calendar::WorkWeek;
use pango_core::error::ConflictReason;
use pango_core::id::MemberId;
use pango_core::member::MemberStatus;
use pango_core::password::PlainPassword;
use pango_core::text::{LabelName, LoginId, PersonName};
use pango_core::time::{parse_timezone, MinuteOfDay, Timestamp};
use pango_core::DomainError;
use pango_db::members::{self, Member};
use pango_db::settings::{self, CompanySettings};
use pango_db::Conn;

use super::support::{audit, Audit, Company};
use crate::error::ServiceResult;
use crate::security::generate_temporary_password;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct InitInput {
    pub company_name: String,
    pub timezone: String,
    pub owner_login_id: String,
    pub owner_name: String,
    /// 생략하면 임시 비밀번호를 만들어 돌려준다(첫 로그인 때 변경 필요).
    pub owner_password: Option<String>,
}

pub struct InitOutput {
    pub owner_id: MemberId,
    /// 비밀번호를 직접 지정하지 않았을 때만 들어 있다. 한 번만 보여 준다.
    pub temporary_password: Option<String>,
}

fn default_settings(
    company_name: String,
    timezone: String,
    now: Timestamp,
) -> ServiceResult<CompanySettings> {
    let at = |hour| MinuteOfDay::from_hm(hour, 0);
    Ok(CompanySettings {
        company_name,
        timezone,
        day_cutoff: at(5)?,
        work_start: at(9)?,
        work_end: at(18)?,
        late_grace_minutes: 0,
        half_day_split: at(14)?,
        quarter_minutes: 120,
        break_tiers: vec![(240, 30), (480, 60)],
        workdays: WorkWeek::MON_TO_FRI,
        version: 1,
        updated_at: now,
    })
}

/// 한 번만 실행할 수 있다. 이미 초기화돼 있으면 `Conflict(AlreadyExists)`.
pub async fn initialize(state: &AppState, input: InitInput) -> ServiceResult<InitOutput> {
    let now = state.now();
    let company_name = LabelName::parse(&input.company_name)?.into_string();
    parse_timezone(&input.timezone)?;
    let login_id = LoginId::parse(&input.owner_login_id)?;
    let name = PersonName::parse(&input.owner_name)?.into_string();

    let (password, temporary_password) = match &input.owner_password {
        Some(raw) => (PlainPassword::parse_new(raw, &login_id)?, None),
        None => {
            let generated = generate_temporary_password()?;
            (
                PlainPassword::for_verification(&generated)?,
                Some(generated),
            )
        }
    };
    let password_hash = state.passwords.hash(&password).await?;
    let settings = default_settings(company_name, input.timezone, now)?;
    let hired_on: NaiveDate = Company::from_settings(settings.clone())?.today(now);

    let owner = Member {
        id: MemberId::new(),
        login_id: login_id.as_str().to_owned(),
        name,
        email: None,
        phone: None,
        department_id: None,
        position_id: None,
        role: Role::Owner,
        status: MemberStatus::Active,
        hired_on,
        retired_on: None,
        must_change_password: temporary_password.is_some(),
        created_at: now,
        version: 1,
    };

    state
        .write(async |conn: &mut Conn| {
            if settings::get(conn).await?.is_some() {
                return Err(DomainError::Conflict(ConflictReason::AlreadyExists).into());
            }
            settings::insert(conn, &settings).await?;
            members::insert(conn, &owner, &password_hash, now).await?;
            super::form_catalog::ensure_form_catalog_tx(conn, now).await?;
            super::leave::seed_defaults_tx(conn, now).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "system.initialize",
                    target_type: "member",
                    target_id: Some(owner.id.to_string()),
                    detail: None,
                    ctx: None,
                },
            )
            .await
        })
        .await?;
    Ok(InitOutput {
        owner_id: owner.id,
        temporary_password,
    })
}

pub async fn is_initialized(state: &AppState) -> ServiceResult<bool> {
    Ok(settings::get(&mut *state.read().await?).await?.is_some())
}

/// [`recover_owner`]의 결과. 임시 비밀번호는 한 번만 보여 준다.
pub struct RecoverOutput {
    pub temporary_password: String,
    pub totp_removed: bool,
    pub revoked_sessions: u64,
}

/// 최고 관리자 계정 복구(서버 호스트에서 실행하는 명령 전용).
///
/// 관리자는 최고 관리자의 비밀번호·2단계 인증을 초기화할 수 없고 최고 관리자는 본인을 초기화할
/// 수 없으므로, 최고 관리자가 한 명뿐인데 비밀번호나 인증 앱을 잃으면 웹에서는 되살릴 길이 없다.
/// 데이터 디렉터리에 접근할 수 있는 사람만 부를 수 있다는 것이 권한 확인이다.
///
/// 임시 비밀번호를 주고(첫 로그인 때 변경), 2단계 인증을 끄고, 모든 세션을 끊는다.
/// 최고 관리자가 아니면 `Forbidden`, 없는 아이디면 `NotFound`.
pub async fn recover_owner(state: &AppState, login_id: &str) -> ServiceResult<RecoverOutput> {
    let login_id = LoginId::parse(login_id)?;
    let temporary_password = generate_temporary_password()?;
    let hash = state
        .passwords
        .hash(&PlainPassword::for_verification(&temporary_password)?)
        .await?;
    let now = state.now();
    let (totp_removed, revoked_sessions) = state
        .write(async |conn: &mut Conn| {
            let credentials = members::find_credentials(conn, login_id.as_str())
                .await?
                .ok_or(DomainError::NotFound)?;
            let owner = credentials.member;
            if owner.role != Role::Owner {
                return Err(DomainError::Forbidden.into());
            }
            members::set_password(conn, owner.id, &hash, true, now).await?;
            let totp_removed = pango_db::totp::remove(conn, owner.id).await?;
            let revoked_sessions =
                pango_db::sessions::revoke_all(conn, owner.id, None, now).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "system.owner_recover",
                    target_type: "member",
                    target_id: Some(owner.id.to_string()),
                    detail: Some(serde_json::json!({
                        "totp_removed": totp_removed,
                        "revoked_sessions": revoked_sessions,
                    })),
                    ctx: None,
                },
            )
            .await?;
            Ok((totp_removed, revoked_sessions))
        })
        .await?;
    Ok(RecoverOutput {
        temporary_password,
        totp_removed,
        revoked_sessions,
    })
}
