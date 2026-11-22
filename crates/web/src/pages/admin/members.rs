//! 구성원 관리: 목록·추가·정보 수정·상태/역할 변경·비밀번호 초기화.

use chrono::NaiveDate;
use dioxus::prelude::*;
use pango_api::auth::Me;
use pango_api::org::{
    ChangeRoleRequest, DepartmentDto, IssuedCredentials, MemberDetail, PositionDto,
};
use pango_api::Empty;
use pango_core::authz::Role;
use pango_core::id::MemberId;
use pango_core::member::MemberStatus;

use crate::api::{self, ApiResult};
use crate::format;
use crate::org_tree;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    loaded_or_empty, report_error, report_success, resource_view, today, Avatar, Badge, Button,
    ButtonKind, Card, Checkbox, ConfirmModal, DateField, EmptyState, FormError, Icon, IconName,
    Meta, Modal, PageHeader, SecretReveal, SelectField, TextField, Tone,
};

use self::logic::{FormErrors, MemberActions, ProfileForm, StatusForm};

/// 서버 오류를 폼에 반영한다. 검증 오류는 해당 칸(입사일·퇴사일) 또는 폼 위에, 그 밖은 토스트로.
fn show_failure(failure: &api::ApiError, mut errors: Signal<FormErrors>) {
    if failure.code == "validation_failed" {
        errors.set(logic::route_error(
            failure.field.as_deref(),
            failure.message(),
        ));
    } else {
        errors.set(FormErrors::default());
        report_error(failure);
    }
}

/// 화면 검증 오류를 폼에 반영한다.
fn show_invalid(invalid: &pango_core::error::DomainError, mut errors: Signal<FormErrors>) {
    errors.set(logic::route_error(
        logic::error_field(invalid),
        text::domain_error_message(invalid),
    ));
}

pub mod logic {
    //! 구성원 관리 화면의 판단과 변환(순수 함수).

    use chrono::{Months, NaiveDate};
    use pango_api::auth::Me;
    use pango_api::org::{
        ChangeStatusRequest, CreateMemberRequest, MemberDetail, UpdateMemberRequest,
    };
    use pango_core::authz::Role;
    use pango_core::error::{DomainError, DomainResult, ValidationReason};
    use pango_core::id::{DepartmentId, PositionId};
    use pango_core::member::{
        validate_hired_on, validate_retired_on, MemberStatus, EARLIEST_HIRE_DATE,
        HIRE_DATE_LEAD_MONTHS,
    };
    use pango_core::text::{Email, LoginId, PersonName, Phone};

    use crate::format::{self, optional_date};

    /// 화면에서 보여 줄 관리 동작. 서버의 `authz::manage_account`·`manage_other_account`·
    /// `change_role`과 같은 규칙이다(숨김은 편의일 뿐, 서버가 다시 판정한다).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct MemberActions {
        pub edit_profile: bool,
        pub change_status: bool,
        pub change_role: bool,
        pub reset_password: bool,
    }

    pub fn allowed_actions(me: &Me, target: &MemberDetail) -> MemberActions {
        let is_self = me.id == target.summary.id;
        // 관리자는 일반 구성원 계정만, 최고 관리자는 모든 계정을 관리한다.
        let manage = me.capabilities.admin
            && me.role.is_admin()
            && (me.role == Role::Owner || target.role == Role::Member);
        MemberActions {
            edit_profile: manage,
            change_status: manage && !is_self,
            reset_password: manage && !is_self,
            change_role: me.capabilities.manage_roles && me.role == Role::Owner && !is_self,
        }
    }

    /// 동작이 막힌 이유(안내 문구). 막힌 것이 없으면 `None`.
    pub fn restriction(me: &Me, target: &MemberDetail) -> Option<&'static str> {
        if me.id == target.summary.id {
            Some("본인 계정의 상태·역할·비밀번호는 여기서 바꿀 수 없어요.")
        } else if me.role != Role::Owner && target.role != Role::Member {
            Some("관리자 계정은 최고 관리자만 관리할 수 있어요.")
        } else {
            None
        }
    }

    fn optional_text<T>(
        raw: &str,
        parse: impl FnOnce(&str) -> DomainResult<T>,
        into: impl FnOnce(T) -> String,
    ) -> DomainResult<Option<String>> {
        format::non_empty(raw)
            .map(|value| parse(value).map(into))
            .transpose()
    }

    fn optional_id<T>(
        raw: &str,
        field: &'static str,
        parse: impl FnOnce(&str) -> DomainResult<T>,
    ) -> DomainResult<Option<T>> {
        format::non_empty(raw)
            .map(|value| {
                parse(value)
                    .map_err(|_| DomainError::validation(field, ValidationReason::InvalidFormat))
            })
            .transpose()
    }

    fn required_date(raw: &str, field: &'static str) -> DomainResult<NaiveDate> {
        optional_date(field, raw)?.ok_or(DomainError::validation(field, ValidationReason::Empty))
    }

    /// 새로 정하는 입사일의 입력 범위 `(min, max)`: 1950-01-01부터 회사 기준 오늘의 1년 뒤까지.
    /// 서버 `member::validate_hired_on`과 같다.
    pub fn hired_on_bounds(today: NaiveDate) -> (NaiveDate, NaiveDate) {
        let latest = today
            .checked_add_months(Months::new(HIRE_DATE_LEAD_MONTHS))
            .unwrap_or(today);
        (EARLIEST_HIRE_DATE, latest)
    }

    /// 입사일 확인. 서버처럼 이미 저장된 값을 그대로 두는 수정은 확인하지 않는다(예전 값 때문에
    /// 다른 정보조차 고칠 수 없게 되지 않도록).
    fn checked_hired_on(
        hired_on: NaiveDate,
        current: Option<NaiveDate>,
        today: NaiveDate,
    ) -> DomainResult<NaiveDate> {
        if current == Some(hired_on) {
            Ok(hired_on)
        } else {
            validate_hired_on(hired_on, today)
        }
    }

    /// 폼에 나눠 보여 줄 오류. 날짜 오류는 그 입력 칸 아래에, 그 밖은 폼 위에 둔다.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct FormErrors {
        pub top: String,
        pub hired_on: String,
        pub retired_on: String,
    }

    /// 검증 오류 문구를 `field`에 따라 자리에 놓는다(서버 오류의 `field`와 화면 검증의 필드가 같다).
    pub fn route_error(field: Option<&str>, message: String) -> FormErrors {
        match field {
            Some("hired_on") => FormErrors {
                hired_on: message,
                ..FormErrors::default()
            },
            Some("retired_on") => FormErrors {
                retired_on: message,
                ..FormErrors::default()
            },
            _ => FormErrors {
                top: message,
                ..FormErrors::default()
            },
        }
    }

    /// 화면 검증 오류의 필드.
    pub fn error_field(error: &DomainError) -> Option<&'static str> {
        match error {
            DomainError::Validation { field, .. } => Some(field),
            _ => None,
        }
    }

    /// 구성원 추가·정보 수정 폼. 모든 값은 입력 그대로의 문자열이다.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ProfileForm {
        pub login_id: String,
        pub name: String,
        pub email: String,
        pub phone: String,
        /// 부서 ID 또는 `""`(없음).
        pub department_id: String,
        /// 직책 ID 또는 `""`(없음).
        pub position_id: String,
        /// `YYYY-MM-DD`.
        pub hired_on: String,
    }

    /// 검증을 마친 공통 프로필 값.
    struct Profile {
        name: String,
        email: Option<String>,
        phone: Option<String>,
        department_id: Option<DepartmentId>,
        position_id: Option<PositionId>,
        hired_on: NaiveDate,
    }

    impl ProfileForm {
        pub fn from_detail(detail: &MemberDetail) -> Self {
            let s = &detail.summary;
            Self {
                login_id: detail.login_id.clone(),
                name: s.name.clone(),
                email: s.email.clone().unwrap_or_default(),
                phone: s.phone.clone().unwrap_or_default(),
                department_id: s.department_id.map(|id| id.to_string()).unwrap_or_default(),
                position_id: s.position_id.map(|id| id.to_string()).unwrap_or_default(),
                hired_on: detail.hired_on.to_string(),
            }
        }

        /// 서버와 같은 규칙(`pango_core::text`, `member::validate_hired_on`)으로 미리 확인한다.
        /// 빈 선택 값은 `None`. `current_hired_on`은 수정할 때 저장된 입사일이다.
        fn profile(
            &self,
            current_hired_on: Option<NaiveDate>,
            today: NaiveDate,
        ) -> DomainResult<Profile> {
            Ok(Profile {
                name: PersonName::parse(&self.name)?.into_string(),
                email: optional_text(&self.email, Email::parse, |e| e.as_str().to_owned())?,
                phone: optional_text(&self.phone, Phone::parse, |p| p.as_str().to_owned())?,
                department_id: optional_id(
                    &self.department_id,
                    "department_id",
                    DepartmentId::parse,
                )?,
                position_id: optional_id(&self.position_id, "position_id", PositionId::parse)?,
                hired_on: checked_hired_on(
                    required_date(&self.hired_on, "hired_on")?,
                    current_hired_on,
                    today,
                )?,
            })
        }

        /// `today`는 회사 시간대의 오늘이다.
        pub fn to_create(&self, today: NaiveDate) -> DomainResult<CreateMemberRequest> {
            let login_id = LoginId::parse(self.login_id.trim())?;
            let p = self.profile(None, today)?;
            Ok(CreateMemberRequest {
                login_id: login_id.as_str().to_owned(),
                name: p.name,
                email: p.email,
                phone: p.phone,
                department_id: p.department_id,
                position_id: p.position_id,
                hired_on: p.hired_on,
            })
        }

        pub fn to_update(
            &self,
            version: i64,
            current_hired_on: NaiveDate,
            today: NaiveDate,
        ) -> DomainResult<UpdateMemberRequest> {
            let p = self.profile(Some(current_hired_on), today)?;
            Ok(UpdateMemberRequest {
                version,
                name: p.name,
                email: p.email,
                phone: p.phone,
                department_id: p.department_id,
                position_id: p.position_id,
                hired_on: p.hired_on,
            })
        }
    }

    /// 상태 변경 폼.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StatusForm {
        pub status: MemberStatus,
        /// 퇴사일 `YYYY-MM-DD`. 비우면 서버가 오늘로 기록한다.
        pub retired_on: String,
    }

    impl StatusForm {
        pub fn from_detail(detail: &MemberDetail) -> Self {
            Self {
                status: detail.summary.status,
                retired_on: detail.retired_on.map(|d| d.to_string()).unwrap_or_default(),
            }
        }

        /// 현재 상태와 다른 값을 골랐는지(같은 퇴사 상태에서 퇴사일만 바꾸는 경우 포함).
        pub fn is_change(&self, current: &MemberDetail) -> bool {
            if self.status != current.summary.status {
                return true;
            }
            self.status == MemberStatus::Retired
                && !self.retired_on.trim().is_empty()
                && optional_date("retired_on", &self.retired_on).ok().flatten()
                    != current.retired_on
        }

        /// 퇴사일은 퇴사로 바꿀 때만 보낸다(그 밖에 보내면 서버가 거부한다). 입사일보다 이를 수
        /// 없다(`member::validate_retired_on`). 비우면 서버가 오늘로 기록한다.
        pub fn to_request(
            &self,
            version: i64,
            hired_on: NaiveDate,
        ) -> DomainResult<ChangeStatusRequest> {
            let retired_on = if self.status == MemberStatus::Retired {
                optional_date("retired_on", &self.retired_on)?
                    .map(|date| validate_retired_on(date, hired_on))
                    .transpose()?
            } else {
                None
            };
            Ok(ChangeStatusRequest {
                version,
                status: self.status,
                retired_on,
            })
        }
    }

    /// 확인 단계가 필요한 상태 변경인지: 로그인할 수 없게 되면(세션이 모두 폐기된다).
    pub fn needs_confirmation(current: MemberStatus, next: MemberStatus) -> bool {
        current != next && !next.can_sign_in()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pango_api::org::MemberSummary;
        use pango_core::authz::Capabilities;
        use pango_core::id::MemberId;

        fn me(role: Role) -> Me {
            Me {
                id: MemberId::new(),
                login_id: "me".into(),
                name: "나".into(),
                email: None,
                phone: None,
                role,
                department_id: None,
                department_name: None,
                position_id: None,
                position_name: None,
                capabilities: Capabilities {
                    admin: role.is_admin(),
                    manage_roles: role == Role::Owner,
                    team: false,
                },
                must_change_password: false,
                company_name: "회사".into(),
                timezone: "Asia/Seoul".into(),
            }
        }

        fn target(role: Role) -> MemberDetail {
            MemberDetail {
                summary: MemberSummary {
                    id: MemberId::new(),
                    name: "대상".into(),
                    email: None,
                    phone: None,
                    department_id: None,
                    department_name: None,
                    position_id: None,
                    position_name: None,
                    status: MemberStatus::Active,
                },
                login_id: "target".into(),
                role,
                hired_on: NaiveDate::from_ymd_opt(2024, 3, 2).unwrap_or_default(),
                retired_on: None,
                must_change_password: false,
                version: 3,
            }
        }

        fn myself(me: &Me) -> MemberDetail {
            let mut t = target(me.role);
            t.summary.id = me.id;
            t
        }

        const ALL: MemberActions = MemberActions {
            edit_profile: true,
            change_status: true,
            change_role: true,
            reset_password: true,
        };
        const NONE: MemberActions = MemberActions {
            edit_profile: false,
            change_status: false,
            change_role: false,
            reset_password: false,
        };
        const MANAGE: MemberActions = MemberActions {
            change_role: false,
            ..ALL
        };

        #[test]
        fn owner_manages_everyone_else_fully() {
            let owner = me(Role::Owner);
            for role in Role::ALL.iter().copied() {
                assert_eq!(allowed_actions(&owner, &target(role)), ALL, "{role:?}");
                assert_eq!(restriction(&owner, &target(role)), None);
            }
        }

        #[test]
        fn owner_on_self_can_only_edit_profile() {
            let owner = me(Role::Owner);
            assert_eq!(
                allowed_actions(&owner, &myself(&owner)),
                MemberActions {
                    edit_profile: true,
                    ..NONE
                }
            );
            assert!(restriction(&owner, &myself(&owner)).is_some());
        }

        #[test]
        fn admin_manages_members_but_not_roles() {
            let admin = me(Role::Admin);
            assert_eq!(allowed_actions(&admin, &target(Role::Member)), MANAGE);
            assert_eq!(restriction(&admin, &target(Role::Member)), None);
        }

        #[test]
        fn admin_cannot_touch_admins_owners_or_self() {
            let admin = me(Role::Admin);
            for role in [Role::Admin, Role::Owner] {
                assert_eq!(allowed_actions(&admin, &target(role)), NONE, "{role:?}");
                assert!(restriction(&admin, &target(role)).is_some());
            }
            // 본인도 관리자 계정이므로 정보 수정까지 막힌다(서버 `manage_account`와 같다).
            assert_eq!(allowed_actions(&admin, &myself(&admin)), NONE);
        }

        #[test]
        fn member_gets_nothing() {
            let member = me(Role::Member);
            for role in Role::ALL.iter().copied() {
                assert_eq!(allowed_actions(&member, &target(role)), NONE);
            }
            assert_eq!(allowed_actions(&member, &myself(&member)), NONE);
        }

        #[test]
        fn capabilities_are_required_too() {
            let mut owner = me(Role::Owner);
            owner.capabilities.manage_roles = false;
            assert!(!allowed_actions(&owner, &target(Role::Member)).change_role);
            owner.capabilities.admin = false;
            assert_eq!(
                allowed_actions(&owner, &target(Role::Member)),
                MemberActions { ..NONE }
            );
        }

        /// 테스트의 회사 기준 오늘.
        fn today() -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, 11, 20).unwrap_or_default()
        }

        fn d(value: &str) -> NaiveDate {
            value.parse().unwrap_or_default()
        }

        fn filled() -> ProfileForm {
            ProfileForm {
                login_id: " Gildong.Hong ".into(),
                name: "  홍길동 ".into(),
                email: "".into(),
                phone: "   ".into(),
                department_id: String::new(),
                position_id: String::new(),
                hired_on: "2026-03-02".into(),
            }
        }

        #[test]
        fn create_request_normalizes_and_drops_empty_values() {
            let request = filled().to_create(today()).expect("valid");
            assert_eq!(request.login_id, "gildong.hong");
            assert_eq!(request.name, "홍길동");
            assert_eq!(request.email, None);
            assert_eq!(request.phone, None);
            assert_eq!(request.department_id, None);
            assert_eq!(request.position_id, None);
            assert_eq!(
                request.hired_on,
                NaiveDate::from_ymd_opt(2026, 3, 2).unwrap_or_default()
            );
        }

        fn field_of(result: DomainResult<impl Sized>) -> Option<&'static str> {
            match result {
                Err(DomainError::Validation { field, .. }) => Some(field),
                _ => None,
            }
        }

        #[test]
        fn create_request_rejects_malformed_input() {
            let cases: Vec<(ProfileForm, &str)> = vec![
                (
                    ProfileForm {
                        login_id: "ab".into(),
                        ..filled()
                    },
                    "login_id",
                ),
                (
                    ProfileForm {
                        login_id: "한글아이디".into(),
                        ..filled()
                    },
                    "login_id",
                ),
                (
                    ProfileForm {
                        login_id: "_abc".into(),
                        ..filled()
                    },
                    "login_id",
                ),
                (
                    ProfileForm {
                        login_id: "a".repeat(33),
                        ..filled()
                    },
                    "login_id",
                ),
                (
                    ProfileForm {
                        name: "  ".into(),
                        ..filled()
                    },
                    "name",
                ),
                (
                    ProfileForm {
                        name: "가".repeat(51),
                        ..filled()
                    },
                    "name",
                ),
                (
                    ProfileForm {
                        name: "홍\u{202E}길동".into(),
                        ..filled()
                    },
                    "name",
                ),
                (
                    ProfileForm {
                        email: "not-an-email".into(),
                        ..filled()
                    },
                    "email",
                ),
                (
                    ProfileForm {
                        phone: "12".into(),
                        ..filled()
                    },
                    "phone",
                ),
                (
                    ProfileForm {
                        department_id: "x".into(),
                        ..filled()
                    },
                    "department_id",
                ),
                (
                    ProfileForm {
                        position_id: "1".into(),
                        ..filled()
                    },
                    "position_id",
                ),
                (
                    ProfileForm {
                        hired_on: "".into(),
                        ..filled()
                    },
                    "hired_on",
                ),
                (
                    ProfileForm {
                        hired_on: "2026-02-30".into(),
                        ..filled()
                    },
                    "hired_on",
                ),
                (
                    ProfileForm {
                        hired_on: "2026/03/02".into(),
                        ..filled()
                    },
                    "hired_on",
                ),
            ];
            for (form, field) in cases {
                assert_eq!(field_of(form.to_create(today())), Some(field), "{form:?}");
            }
        }

        #[test]
        fn boundary_lengths_are_accepted() {
            let ok = ProfileForm {
                login_id: "abc".into(),
                name: "가".repeat(50),
                ..filled()
            };
            assert!(ok.to_create(today()).is_ok());
            assert!(ProfileForm {
                login_id: "a".repeat(32),
                ..filled()
            }
            .to_create(today())
            .is_ok());
        }

        #[test]
        fn update_round_trips_detail_and_carries_version() {
            let mut detail = target(Role::Member);
            let dept = DepartmentId::new();
            detail.summary.department_id = Some(dept);
            detail.summary.email = Some("a@example.com".into());
            let form = ProfileForm::from_detail(&detail);
            let request = form
                .to_update(detail.version, detail.hired_on, today())
                .expect("valid");
            assert_eq!(request.version, 3);
            assert_eq!(request.department_id, Some(dept));
            assert_eq!(request.email.as_deref(), Some("a@example.com"));
            assert_eq!(request.hired_on, detail.hired_on);
            // 수정에서는 아이디를 보지 않는다.
            assert!(ProfileForm {
                login_id: String::new(),
                ..form
            }
            .to_update(1, detail.hired_on, today())
            .is_ok());
        }

        #[test]
        fn status_request_sends_retired_on_only_when_retiring() {
            let detail = target(Role::Member);
            let mut form = StatusForm::from_detail(&detail);
            assert!(!form.is_change(&detail));

            form.status = MemberStatus::Suspended;
            form.retired_on = "2026-03-31".into();
            assert!(form.is_change(&detail));
            assert_eq!(
                form.to_request(3, detail.hired_on)
                    .expect("valid")
                    .retired_on,
                None
            );

            form.status = MemberStatus::Retired;
            let request = form.to_request(3, detail.hired_on).expect("valid");
            assert_eq!(request.retired_on, NaiveDate::from_ymd_opt(2026, 3, 31));
            assert_eq!(request.version, 3);

            form.retired_on = String::new();
            assert_eq!(
                form.to_request(3, detail.hired_on)
                    .expect("valid")
                    .retired_on,
                None
            );
            form.retired_on = "31/03/2026".into();
            assert_eq!(
                field_of(form.to_request(3, detail.hired_on)),
                Some("retired_on")
            );
        }

        #[test]
        fn hired_on_follows_the_server_bounds() {
            let (min, max) = hired_on_bounds(today());
            assert_eq!(min, d("1950-01-01"));
            assert_eq!(max, d("2027-11-20"));
            // 윤일에서 1년 뒤는 2월 28일이다(서버와 같은 달 단위 계산).
            assert_eq!(hired_on_bounds(d("2028-02-29")).1, d("2029-02-28"));
            for ok in ["1950-01-01", "2026-11-20", "2027-11-20"] {
                let form = ProfileForm {
                    hired_on: ok.into(),
                    ..filled()
                };
                assert!(form.to_create(today()).is_ok(), "{ok}");
            }
            for bad in ["1949-12-31", "2027-11-21", "1900-01-01", "2100-01-01"] {
                let form = ProfileForm {
                    hired_on: bad.into(),
                    ..filled()
                };
                assert_eq!(
                    form.to_create(today()).err(),
                    Some(DomainError::validation(
                        "hired_on",
                        ValidationReason::OutOfRange
                    )),
                    "{bad}"
                );
            }
            // 표기 범위를 벗어난 해(0202년)는 형식 오류지만 같은 칸에 붙는다.
            let typo = ProfileForm {
                hired_on: "0202-03-02".into(),
                ..filled()
            };
            assert_eq!(field_of(typo.to_create(today())), Some("hired_on"));
        }

        #[test]
        fn unchanged_hired_on_is_not_rechecked_on_update() {
            let mut detail = target(Role::Member);
            detail.hired_on = d("1949-06-01");
            let form = ProfileForm::from_detail(&detail);
            assert!(form.to_update(3, detail.hired_on, today()).is_ok());
            let moved = ProfileForm {
                hired_on: "1949-06-02".into(),
                ..form
            };
            assert_eq!(
                field_of(moved.to_update(3, detail.hired_on, today())),
                Some("hired_on")
            );
        }

        #[test]
        fn retired_on_cannot_precede_hired_on() {
            let detail = target(Role::Member);
            let mut form = StatusForm::from_detail(&detail);
            form.status = MemberStatus::Retired;
            form.retired_on = detail.hired_on.to_string();
            assert!(form.to_request(3, detail.hired_on).is_ok(), "당일 퇴사");
            form.retired_on = "2024-03-01".into();
            assert_eq!(
                form.to_request(3, detail.hired_on).err(),
                Some(DomainError::validation(
                    "retired_on",
                    ValidationReason::Inconsistent
                ))
            );
            // 퇴사가 아니면 퇴사일을 보지 않는다.
            form.status = MemberStatus::Suspended;
            assert!(form.to_request(3, detail.hired_on).is_ok());
        }

        #[test]
        fn date_errors_land_next_to_their_fields() {
            assert_eq!(
                route_error(Some("hired_on"), "a".into()),
                FormErrors {
                    hired_on: "a".into(),
                    ..FormErrors::default()
                }
            );
            assert_eq!(route_error(Some("retired_on"), "b".into()).retired_on, "b");
            assert_eq!(route_error(Some("name"), "c".into()).top, "c");
            assert_eq!(route_error(None, "d".into()).top, "d");
            assert_eq!(
                error_field(&DomainError::validation(
                    "hired_on",
                    ValidationReason::OutOfRange
                )),
                Some("hired_on")
            );
            assert_eq!(error_field(&DomainError::Forbidden), None);
        }

        #[test]
        fn retired_member_can_change_only_the_date() {
            let mut detail = target(Role::Member);
            detail.summary.status = MemberStatus::Retired;
            detail.retired_on = NaiveDate::from_ymd_opt(2026, 3, 31);
            let mut form = StatusForm::from_detail(&detail);
            assert!(!form.is_change(&detail));
            form.retired_on = "2026-04-01".into();
            assert!(form.is_change(&detail));
            form.retired_on = String::new();
            assert!(!form.is_change(&detail), "비우면 기존 퇴사일을 유지한다");
        }

        #[test]
        fn only_sign_in_blocking_changes_need_confirmation() {
            use MemberStatus::*;
            assert!(needs_confirmation(Active, Suspended));
            assert!(needs_confirmation(Active, Retired));
            assert!(needs_confirmation(Suspended, Retired));
            assert!(!needs_confirmation(Suspended, Active));
            assert!(!needs_confirmation(Retired, Active));
            assert!(!needs_confirmation(Retired, Retired));
        }
    }
}

/// 한 번만 보여 줄 임시 비밀번호. 대화 상자를 닫으면 바로 버린다(로그·디버그 출력 금지).
#[derive(Clone, PartialEq)]
struct Issued {
    name: String,
    login_id: String,
    password: String,
}

impl From<IssuedCredentials> for Issued {
    fn from(issued: IssuedCredentials) -> Self {
        Self {
            name: issued.member.summary.name,
            login_id: issued.member.login_id,
            password: issued.temporary_password,
        }
    }
}

#[component]
pub fn AdminMembers() -> Element {
    let session = use_session();
    let mut search = use_signal(String::new);
    let mut department = use_signal(String::new);
    let mut include_inactive = use_signal(|| false);
    let mut creating = use_signal(|| false);
    let mut selected = use_signal(|| None::<MemberDetail>);
    let mut issued = use_signal(|| None::<Issued>);

    let departments =
        use_resource(|| async { api::get::<Vec<DepartmentDto>>("/departments").await });
    let positions = use_resource(|| async { api::get::<Vec<PositionDto>>("/positions").await });
    let mut members = use_resource(move || async move {
        let pairs = org_tree::member_query(&search(), &department(), include_inactive());
        api::get::<Vec<MemberDetail>>(&format!("/admin/members{}", api::query_string(&pairs))).await
    });

    let Some(me) = session.me() else {
        return rsx! {};
    };
    let department_list = loaded_or_empty(&departments);
    let position_list = loaded_or_empty(&positions);
    let department_filter = org_tree::department_options(&department_list, "전체 부서");

    rsx! {
        PageHeader {
            title: "구성원 관리",
            description: "계정을 만들고 소속·상태·권한을 관리해요.",
            actions: rsx! {
                Button { onclick: move |_| creating.set(true),
                    Icon { name: IconName::Plus }
                    "구성원 추가"
                }
            },
        }
        Card {
            div { class: "toolbar",
                div { class: "toolbar-wide",
                    TextField {
                        label: "검색",
                        value: search(),
                        kind: "search",
                        placeholder: "이름 또는 아이디",
                        maxlength: 100,
                        oninput: move |value| search.set(value),
                    }
                }
                SelectField {
                    label: "부서",
                    value: department(),
                    options: department_filter,
                    onchange: move |value| department.set(value),
                }
                div { class: "toolbar-end",
                    Checkbox {
                        label: "퇴사·정지 포함",
                        checked: include_inactive(),
                        onchange: move |value| include_inactive.set(value),
                    }
                }
            }
        {resource_view(members, move |list: Vec<MemberDetail>| rsx! {
            if list.is_empty() {
                EmptyState { title: "조건에 맞는 구성원이 없어요.", icon: IconName::People }
            } else {
                Fragment {
                    div { class: "table-wrap only-desktop",
                        table { class: "table",
                            thead {
                                tr {
                                    th { "이름" }
                                    th { "아이디" }
                                    th { "부서" }
                                    th { "직책" }
                                    th { "역할" }
                                    th { "상태" }
                                    th { "입사일" }
                                }
                            }
                            tbody {
                                for member in list.clone() {
                                    tr { key: "{member.summary.id}",
                                        td {
                                            button {
                                                class: "link-btn",
                                                r#type: "button",
                                                onclick: {
                                                    let member = member.clone();
                                                    move |_| selected.set(Some(member.clone()))
                                                },
                                                "{member.summary.name}"
                                            }
                                        }
                                        td { class: "mono", "{member.login_id}" }
                                        td { {member.summary.department_name.clone().unwrap_or_else(|| "-".into())} }
                                        td { {member.summary.position_name.clone().unwrap_or_else(|| "-".into())} }
                                        td {
                                            Badge { tone: text::role_tone(member.role), {text::role_label(member.role)} }
                                        }
                                        td {
                                            Badge { tone: text::member_status_tone(member.summary.status),
                                                {text::member_status_label(member.summary.status)}
                                            }
                                        }
                                        td { {format::date_ymd(member.hired_on)} }
                                    }
                                }
                            }
                        }
                    }
                    ul { class: "list only-mobile",
                        for member in list {
                            li { key: "{member.summary.id}",
                                button {
                                    class: "list-row list-row-btn",
                                    r#type: "button",
                                    onclick: {
                                        let member = member.clone();
                                        move |_| selected.set(Some(member.clone()))
                                    },
                                    Avatar { name: member.summary.name.clone(), seed: member.summary.id.to_string() }
                                    span { class: "list-row-main",
                                        strong { "{member.summary.name}" }
                                        Meta {
                                            items: vec![
                                                member.login_id.to_string(),
                                                member.summary.department_name.clone().unwrap_or_default(),
                                                member.summary.position_name.clone().unwrap_or_default(),
                                            ],
                                        }
                                    }
                                    span { class: "list-row-side",
                                        Badge { tone: text::member_status_tone(member.summary.status),
                                            {text::member_status_label(member.summary.status)}
                                        }
                                        if member.role != Role::Member {
                                            Badge { tone: text::role_tone(member.role), {text::role_label(member.role)} }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })}
        }
        if creating() {
            MemberCreateModal {
                departments: department_list.clone(),
                positions: position_list.clone(),
                onclose: move |_| creating.set(false),
                oncreated: move |credentials: IssuedCredentials| {
                    creating.set(false);
                    issued.set(Some(Issued::from(credentials)));
                    members.restart();
                },
            }
        }
        if let Some(member) = selected() {
            MemberDetailModal {
                me: me.clone(),
                member,
                departments: department_list,
                positions: position_list,
                onchanged: move |_| members.restart(),
                onissued: move |credentials: IssuedCredentials| issued.set(Some(Issued::from(credentials))),
                onclose: move |_| selected.set(None),
            }
        }
        if let Some(secret) = issued() {
            SecretReveal {
                title: "임시 비밀번호",
                label: "임시 비밀번호",
                secret: secret.password.clone(),
                onclose: move |_| issued.set(None),
                p { class: "secret-intro",
                    strong { "{secret.name}" }
                    " ({secret.login_id}) 님의 임시 비밀번호예요. 처음 로그인하면 새 비밀번호로 바꾸게 돼요."
                }
            }
        }
    }
}

/// 구성원 추가·수정 폼의 입력 칸.
#[component]
fn ProfileFields(
    form: Signal<ProfileForm>,
    departments: Vec<DepartmentDto>,
    positions: Vec<PositionDto>,
    /// 아이디 입력 칸(추가할 때만).
    #[props(default)]
    with_login_id: bool,
    #[props(default)] disabled: bool,
    /// 회사 기준 오늘(입사일 입력 범위의 기준).
    today: NaiveDate,
    /// 입사일 칸 아래에 보일 오류.
    #[props(into, default)]
    hired_on_error: String,
) -> Element {
    let mut form = form;
    let current = form();
    let (min_hired, max_hired) = logic::hired_on_bounds(today);
    rsx! {
        if with_login_id {
            TextField {
                label: "아이디",
                value: current.login_id.clone(),
                autocomplete: "off",
                hint: "영문 소문자·숫자와 . _ - 3~32자. 로그인할 때 써요.",
                required: true,
                maxlength: 32,
                oninput: move |value| form.write().login_id = value,
            }
        }
        TextField {
            label: "이름",
            value: current.name.clone(),
            required: true,
            disabled,
            maxlength: 50,
            oninput: move |value| form.write().name = value,
        }
        div { class: "form-row",
            TextField {
                label: "이메일",
                kind: "email",
                value: current.email.clone(),
                disabled,
                maxlength: 254,
                oninput: move |value| form.write().email = value,
            }
            TextField {
                label: "전화번호",
                kind: "tel",
                value: current.phone.clone(),
                disabled,
                maxlength: 20,
                oninput: move |value| form.write().phone = value,
            }
        }
        div { class: "form-row",
            SelectField {
                label: "부서",
                value: current.department_id.clone(),
                options: org_tree::department_options(&departments, "없음"),
                disabled,
                onchange: move |value| form.write().department_id = value,
            }
            SelectField {
                label: "직책",
                value: current.position_id.clone(),
                options: org_tree::position_options(&positions, "없음"),
                disabled,
                onchange: move |value| form.write().position_id = value,
            }
        }
        DateField {
            label: "입사일",
            value: current.hired_on.clone(),
            min: min_hired.to_string(),
            max: max_hired.to_string(),
            hint: "1950. 1. 1.부터 오늘의 1년 뒤까지 입력할 수 있어요.",
            error: hired_on_error,
            required: true,
            disabled,
            oninput: move |value| form.write().hired_on = value,
        }
    }
}

#[component]
fn MemberCreateModal(
    departments: Vec<DepartmentDto>,
    positions: Vec<PositionDto>,
    onclose: EventHandler<()>,
    oncreated: EventHandler<IssuedCredentials>,
) -> Element {
    let today = today(use_session().zone());
    let form = use_signal(ProfileForm::default);
    let mut errors = use_signal(FormErrors::default);
    let mut busy = use_signal(|| false);

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let request = match form.read().to_create(today) {
            Ok(request) => request,
            Err(invalid) => {
                show_invalid(&invalid, errors);
                return;
            }
        };
        busy.set(true);
        errors.set(FormErrors::default());
        // 멱등성 키를 보내지 않는다. 서버가 임시 비밀번호가 담긴 응답을 저장하지 않으려고
        // 키를 쓰지 않기 때문이다. 같은 요청을 다시 보내면 `login_id_taken`으로 거부된다.
        let result = api::post::<_, IssuedCredentials>("/admin/members", &request).await;
        busy.set(false);
        match result {
            Ok(credentials) => {
                report_success("구성원을 추가했어요.");
                oncreated.call(credentials);
            }
            Err(failure) => show_failure(&failure, errors),
        }
    };

    rsx! {
        Modal { title: "구성원 추가", onclose,
            form { class: "form", onsubmit: submit, novalidate: true,
                FormError { message: errors().top }
                ProfileFields {
                    form,
                    departments,
                    positions,
                    with_login_id: true,
                    today,
                    hired_on_error: errors().hired_on,
                }
                p { class: "field-hint", "역할은 구성원으로 만들어져요. 임시 비밀번호는 추가한 직후 한 번만 보여 드려요." }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Grey, onclick: move |_| onclose.call(()), "취소" }
                    Button { submit: true, busy: busy(), "추가" }
                }
            }
        }
    }
}

/// 확인 단계를 기다리는 동작.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Status,
    ResetPassword,
}

async fn fetch_member(id: MemberId) -> ApiResult<MemberDetail> {
    api::get::<MemberDetail>(&format!("/admin/members/{id}")).await
}

#[component]
fn MemberDetailModal(
    me: Me,
    member: MemberDetail,
    departments: Vec<DepartmentDto>,
    positions: Vec<PositionDto>,
    /// 서버에서 구성원이 바뀌었을 때(목록을 다시 불러온다).
    onchanged: EventHandler<()>,
    onissued: EventHandler<IssuedCredentials>,
    onclose: EventHandler<()>,
) -> Element {
    let mut current = use_signal(|| member.clone());
    let mut form = use_signal(|| ProfileForm::from_detail(&member));
    let mut status = use_signal(|| StatusForm::from_detail(&member));
    let mut role = use_signal(|| member.role);
    let today = today(use_session().zone());
    let mut errors = use_signal(FormErrors::default);
    let mut busy = use_signal(|| false);
    let mut pending = use_signal(|| None::<Pending>);

    // 서버가 돌려준 최신 상태로 화면의 모든 폼을 맞춘다.
    let mut apply = move |detail: MemberDetail| {
        form.set(ProfileForm::from_detail(&detail));
        status.set(StatusForm::from_detail(&detail));
        role.set(detail.role);
        current.set(detail);
        onchanged.call(());
    };
    // 실패 처리. 다른 사람이 먼저 고쳤으면 최신 값을 다시 읽는다.
    let fail = move |failure: api::ApiError| async move {
        show_failure(&failure, errors);
        if failure.code == "version_conflict" {
            let id = current.peek().summary.id;
            match fetch_member(id).await {
                Ok(fresh) => apply(fresh),
                Err(reload) => report_error(&reload),
            }
        }
    };

    let save_profile = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let (id, version, hired_on) = {
            let m = current.read();
            (m.summary.id, m.version, m.hired_on)
        };
        let request = match form.read().to_update(version, hired_on, today) {
            Ok(request) => request,
            Err(invalid) => {
                show_invalid(&invalid, errors);
                return;
            }
        };
        busy.set(true);
        errors.set(FormErrors::default());
        let result = api::patch::<_, MemberDetail>(&format!("/admin/members/{id}"), &request).await;
        busy.set(false);
        match result {
            Ok(detail) => {
                report_success("정보를 저장했어요.");
                apply(detail);
            }
            Err(failure) => fail(failure).await,
        }
    };

    let run_status = move || async move {
        if busy() {
            return;
        }
        let (id, version, hired_on) = {
            let m = current.read();
            (m.summary.id, m.version, m.hired_on)
        };
        let request = match status.read().to_request(version, hired_on) {
            Ok(request) => request,
            Err(invalid) => {
                pending.set(None);
                show_invalid(&invalid, errors);
                return;
            }
        };
        busy.set(true);
        errors.set(FormErrors::default());
        let result =
            api::post::<_, MemberDetail>(&format!("/admin/members/{id}/status"), &request).await;
        busy.set(false);
        pending.set(None);
        match result {
            Ok(detail) => {
                report_success("상태를 바꿨어요.");
                apply(detail);
            }
            Err(failure) => fail(failure).await,
        }
    };

    let change_role = move |_| async move {
        if busy() {
            return;
        }
        let (id, version) = {
            let m = current.read();
            (m.summary.id, m.version)
        };
        let request = ChangeRoleRequest {
            version,
            role: role(),
        };
        busy.set(true);
        errors.set(FormErrors::default());
        let result =
            api::post::<_, MemberDetail>(&format!("/admin/members/{id}/role"), &request).await;
        busy.set(false);
        match result {
            Ok(detail) => {
                report_success("역할을 바꿨어요.");
                apply(detail);
            }
            Err(failure) => fail(failure).await,
        }
    };

    let reset_password = move || async move {
        if busy() {
            return;
        }
        let id = current.peek().summary.id;
        busy.set(true);
        let result = api::post::<_, IssuedCredentials>(
            &format!("/admin/members/{id}/reset-password"),
            &Empty {},
        )
        .await;
        busy.set(false);
        pending.set(None);
        match result {
            Ok(credentials) => {
                apply(credentials.member.clone());
                onissued.call(credentials);
            }
            Err(failure) => fail(failure).await,
        }
    };

    let detail = current();
    let actions: MemberActions = logic::allowed_actions(&me, &detail);
    let restriction = logic::restriction(&me, &detail);
    let next_status = status.read().status;
    let status_changed = status.read().is_change(&detail);
    let status_options: Vec<(String, String)> = MemberStatus::ALL
        .iter()
        .map(|s| {
            (
                s.as_str().to_owned(),
                text::member_status_label(*s).to_owned(),
            )
        })
        .collect();
    let role_options: Vec<(String, String)> = Role::ALL
        .iter()
        .copied()
        .map(|r| (r.as_str().to_owned(), text::role_label(r).to_owned()))
        .collect();

    rsx! {
        Modal { title: "{detail.summary.name}", onclose,
            div { class: "member-detail",
                div { class: "member-detail-head",
                    Avatar { name: detail.summary.name.clone(), seed: detail.summary.id.to_string(), large: true }
                    div {
                        p { class: "mono", "{detail.login_id}" }
                        div { class: "badge-row",
                            Badge { tone: text::role_tone(detail.role), {text::role_label(detail.role)} }
                            Badge { tone: text::member_status_tone(detail.summary.status),
                                {text::member_status_label(detail.summary.status)}
                            }
                            if detail.must_change_password {
                                Badge { tone: Tone::Warning, "임시 비밀번호 사용 중" }
                            }
                        }
                        if let Some(retired_on) = detail.retired_on {
                            p { class: "field-hint", "퇴사일 {format::date_ymd(retired_on)}" }
                        }
                    }
                }
                FormError { message: errors().top }
                if let Some(reason) = restriction {
                    p { class: "callout callout-info", role: "note", "{reason}" }
                }

                section { class: "member-section",
                    h3 { class: "sub-title", "기본 정보" }
                    form { class: "form", onsubmit: save_profile, novalidate: true,
                        ProfileFields {
                            form,
                            departments,
                            positions,
                            disabled: !actions.edit_profile,
                            today,
                            hired_on_error: errors().hired_on,
                        }
                        if actions.edit_profile {
                            div { class: "modal-actions",
                                Button { submit: true, busy: busy(), "저장" }
                            }
                        }
                    }
                }

                if actions.change_status {
                    section { class: "member-section",
                        h3 { class: "sub-title", "계정 상태" }
                        div { class: "form-row",
                            SelectField {
                                label: "상태",
                                value: next_status.as_str(),
                                options: status_options,
                                onchange: move |value: String| {
                                    if let Ok(next) = MemberStatus::parse(&value) {
                                        status.write().status = next;
                                    }
                                },
                            }
                            if next_status == MemberStatus::Retired {
                                DateField {
                                    label: "퇴사일",
                                    value: status.read().retired_on.clone(),
                                    min: detail.hired_on.to_string(),
                                    hint: "비워 두면 오늘 날짜로 기록돼요. 입사일보다 이를 수 없어요.",
                                    error: errors().retired_on,
                                    oninput: move |value| status.write().retired_on = value,
                                }
                            }
                        }
                        div { class: "modal-actions",
                            Button {
                                kind: ButtonKind::Grey,
                                disabled: !status_changed,
                                busy: busy(),
                                onclick: move |_| async move {
                                    let current_status = current.peek().summary.status;
                                    if logic::needs_confirmation(current_status, status.peek().status) {
                                        pending.set(Some(Pending::Status));
                                    } else {
                                        run_status().await;
                                    }
                                },
                                "상태 변경"
                            }
                        }
                    }
                }

                if actions.change_role {
                    section { class: "member-section",
                        h3 { class: "sub-title", "역할" }
                        SelectField {
                            label: "역할",
                            value: role().as_str(),
                            options: role_options,
                            onchange: move |value: String| {
                                if let Ok(next) = Role::parse(&value) {
                                    role.set(next);
                                }
                            },
                        }
                        p { class: "field-hint", "관리자는 구성원·근태·휴가·결재 양식·공지를 관리해요. 최고 관리자는 역할도 바꿀 수 있어요." }
                        div { class: "modal-actions",
                            Button {
                                kind: ButtonKind::Grey,
                                disabled: role() == detail.role,
                                busy: busy(),
                                onclick: change_role,
                                "역할 변경"
                            }
                        }
                    }
                }

                if actions.reset_password {
                    section { class: "member-section",
                        h3 { class: "sub-title", "비밀번호" }
                        p { class: "field-hint", "임시 비밀번호를 새로 만들어요. 구성원은 다음 로그인 때 비밀번호를 바꿔야 해요." }
                        div { class: "modal-actions",
                            Button {
                                kind: ButtonKind::Grey,
                                busy: busy(),
                                onclick: move |_| pending.set(Some(Pending::ResetPassword)),
                                "비밀번호 초기화"
                            }
                        }
                    }
                }
                MemberBuzzSection { member_id: detail.summary.id, name: detail.summary.name.clone() }
            }
        }
        {match pending() {
            Some(Pending::Status) => rsx! {
                ConfirmModal {
                    title: "{text::member_status_label(next_status)}(으)로 바꿀까요?",
                    confirm_label: "{text::member_status_label(next_status)} 처리",
                    danger: true,
                    busy: busy(),
                    onconfirm: move |_| run_status(),
                    oncancel: move |_| pending.set(None),
                    p {
                        strong { "{detail.summary.name}" }
                        " 님은 "
                        {text::member_status_effect(next_status)}
                    }
                }
            },
            Some(Pending::ResetPassword) => rsx! {
                ConfirmModal {
                    title: "비밀번호를 초기화할까요?",
                    confirm_label: "초기화",
                    danger: true,
                    busy: busy(),
                    onconfirm: move |_| reset_password(),
                    oncancel: move |_| pending.set(None),
                    p {
                        strong { "{detail.summary.name}" }
                        " 님의 지금 비밀번호는 더 이상 쓸 수 없게 돼요. 새 임시 비밀번호는 다음 화면에서 한 번만 보여 드려요."
                    }
                }
            },
            None => rsx! {},
        }}
    }
}

/// 구성원의 Buzz 연결. 관리자는 Buzz ID를 등록(본인 확인 없이 바로 연결)하거나 끊을 수 있다.
#[component]
fn MemberBuzzSection(member_id: pango_core::id::MemberId, name: String) -> Element {
    let path = format!("/admin/members/{member_id}/buzz");
    let mut link = use_resource({
        let path = path.clone();
        move || {
            let path = path.clone();
            async move { api::get::<pango_api::integration::MemberBuzzDto>(&path).await }
        }
    });
    let mut confirm = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut pubkey = use_signal(String::new);
    let mut error = use_signal(String::new);
    let assign = {
        let path = path.clone();
        move |event: FormEvent| {
            let path = path.clone();
            async move {
                event.prevent_default();
                if busy() {
                    return;
                }
                busy.set(true);
                error.set(String::new());
                let input = pango_api::integration::BuzzAssignInput {
                    pubkey: pubkey.read().trim().to_owned(),
                };
                match api::put::<_, pango_api::integration::MemberBuzzDto>(&path, &input).await {
                    Ok(_) => {
                        report_success("Buzz ID를 연결했어요.");
                        pubkey.set(String::new());
                        link.restart();
                    }
                    Err(failure) => error.set(failure.message()),
                }
                busy.set(false);
            }
        }
    };
    let current = match &*link.read() {
        Some(Ok(current)) => current.clone(),
        _ => return rsx! {},
    };
    if current.status == pango_api::integration::BuzzLinkStatus::None {
        return rsx! {
            section { class: "member-section",
                h3 { class: "sub-title", "Buzz 연결" }
                p { class: "field-hint",
                    "연결 안 됨. Buzz ID를 넣으면 본인 확인 없이 바로 연결되고, 구성원에게 연결 알림이 가요."
                }
                form { class: "form", onsubmit: assign,
                    FormError { message: error() }
                    TextField {
                        label: "Buzz ID",
                        value: pubkey(),
                        placeholder: "npub1…",
                        autocomplete: "off",
                        oninput: move |value| pubkey.set(value),
                    }
                    div { class: "modal-actions",
                        Button { submit: true, busy: busy(), disabled: pubkey.read().trim().is_empty(), "연결" }
                    }
                }
            }
        };
    }
    let unlink = move |_| {
        let path = path.clone();
        async move {
            if busy() {
                return;
            }
            busy.set(true);
            match api::delete::<pango_api::integration::MemberBuzzDto>(&path).await {
                Ok(_) => {
                    report_success("Buzz 연결을 끊었어요.");
                    link.restart();
                }
                Err(error) => report_error(&error),
            }
            confirm.set(false);
            busy.set(false);
        }
    };
    let state = if current.status == pango_api::integration::BuzzLinkStatus::Verified {
        "연결됨"
    } else {
        "확인 기다림"
    };
    rsx! {
        section { class: "member-section",
            h3 { class: "sub-title", "Buzz 연결" }
            p { class: "field-hint",
                "{state} · "
                span { class: "mono-wrap", {current.npub_short.clone().unwrap_or_default()} }
            }
            p { class: "field-hint", "끊으면 Buzz로 알림이 가지 않아요. 다른 ID로 바꾸려면 끊은 뒤 다시 등록해요." }
            div { class: "modal-actions",
                Button { kind: ButtonKind::Grey, busy: busy(), onclick: move |_| confirm.set(true), "Buzz 연결 해제" }
            }
        }
        if confirm() {
            ConfirmModal {
                title: "Buzz 연결을 끊을까요?",
                confirm_label: "연결 해제",
                danger: true,
                busy: busy(),
                onconfirm: unlink,
                oncancel: move |_| confirm.set(false),
                p {
                    strong { "{name}" }
                    " 님에게 Buzz로 가던 알림이 멈춰요. 앱 알림은 그대로 가요."
                }
            }
        }
    }
}
