//! 화면 문구. 서버의 코드 값(오류 코드, 상태 값)을 한국어로 바꾸는 곳은 여기뿐이다.

use pango_core::authz::Role;
use pango_core::error::DomainError;
use pango_core::member::MemberStatus;

use crate::ui::Tone;

fn field_label(field: &str) -> &str {
    match field {
        "login_id" => "아이디",
        "password" | "new_password" => "비밀번호",
        "current_password" => "현재 비밀번호",
        "name" => "이름",
        "email" => "이메일",
        "phone" => "전화번호",
        "title" => "제목",
        "body" => "내용",
        "reason" => "사유",
        "comment" => "의견",
        "steps" => "결재선",
        "references" => "참조",
        "date_range" | "start_date" | "end_date" => "기간",
        "unit" => "휴가 단위",
        "month" => "월",
        "timezone" => "시간대",
        "amount" | "amount_q" => "수량",
        "hired_on" => "입사일",
        "retired_on" => "퇴사일",
        "department_id" => "부서",
        "parent_id" => "상위 부서",
        "position_id" => "직책",
        "head_member_id" => "조직장",
        "sort_order" => "정렬 순서",
        "level" => "등급",
        "search" => "검색어",
        "role" => "역할",
        "status" => "상태",
        "company_name" => "회사 이름",
        "day_cutoff" => "근무일 전환 시각",
        "work_start" => "업무 시작 시각",
        "work_end" => "업무 종료 시각",
        "half_day_split" => "반차 기준 시각",
        "quarter_minutes" => "반반차 길이",
        "late_grace_minutes" => "지각 유예 시간",
        "break_tiers" => "휴게 시간",
        "workdays" => "근무 요일",
        "audience" => "공지 대상",
        "date" => "날짜",
        "items" => "항목",
        "year" => "연도",
        "until" => "기간",
        other => other,
    }
}

fn validation_message(field: Option<&str>, reason: Option<&str>) -> String {
    let label = field.map(field_label).unwrap_or("입력값");
    match (field, reason) {
        (Some("current_password"), _) => "현재 비밀번호가 맞지 않습니다.".into(),
        (Some("password" | "new_password"), Some("too_short")) => {
            "비밀번호는 10자 이상이어야 합니다.".into()
        }
        (Some("password" | "new_password"), Some("invalid_format")) => {
            "아이디가 들어 있거나 같은 글자만 반복한 비밀번호는 쓸 수 없습니다.".into()
        }
        (Some("steps"), Some("empty")) => "결재자를 한 명 이상 지정해 주세요.".into(),
        (Some("department_id"), Some("empty")) => "대상 부서를 선택해 주세요.".into(),
        (Some("workdays"), _) => "근무 요일을 하루 이상 선택해 주세요.".into(),
        (Some("timezone"), _) => "시간대 이름이 올바르지 않습니다. 예: Asia/Seoul".into(),
        (Some("day_cutoff"), Some("out_of_range")) => {
            "근무일 전환 시각은 12:00 이전이어야 합니다.".into()
        }
        (Some("work_end"), Some("inconsistent")) => {
            "업무 종료 시각은 시작 시각보다 늦어야 합니다.".into()
        }
        (Some("half_day_split"), Some("out_of_range")) => {
            "반차 기준 시각은 업무 시작과 종료 사이여야 합니다.".into()
        }
        (Some("quarter_minutes"), Some("out_of_range")) => {
            "반반차 길이는 1분 이상이고 근무 시간보다 짧아야 합니다.".into()
        }
        (Some("late_grace_minutes"), Some("out_of_range")) => {
            "지각 유예 시간은 0~120분이어야 합니다.".into()
        }
        (Some("break_tiers"), Some("too_long")) => "휴게 단계는 6개까지 둘 수 있습니다.".into(),
        (Some("break_tiers"), Some("inconsistent")) => {
            "휴게 기준은 위에서부터 커져야 하고, 휴게 시간은 기준보다 짧아야 합니다.".into()
        }
        (Some("break_tiers"), Some("out_of_range")) => {
            "휴게 기준은 1440분(24시간) 이하여야 합니다.".into()
        }
        (_, Some("empty")) => format!("{label}을(를) 입력해 주세요."),
        (_, Some("too_short")) => format!("{label}이(가) 너무 짧습니다."),
        (_, Some("too_long")) => format!("{label}이(가) 너무 깁니다."),
        (_, Some("invalid_character")) => format!("{label}에 쓸 수 없는 문자가 들어 있습니다."),
        (_, Some("duplicate")) => format!("{label}에 중복된 항목이 있습니다."),
        (_, Some("out_of_range")) => format!("{label}이(가) 허용 범위를 벗어났습니다."),
        (_, Some("inconsistent")) => format!("{label}을(를) 다시 확인해 주세요."),
        _ => format!("{label}의 형식이 올바르지 않습니다."),
    }
}

/// 오류 코드를 사용자 문구로 바꾼다. 모르는 코드는 일반 문구로 처리한다.
pub fn error_message(code: &str, field: Option<&str>, reason: Option<&str>) -> String {
    let fixed = match code {
        "validation_failed" => return validation_message(field, reason),
        "network_error" => "서버에 연결할 수 없습니다. 네트워크를 확인해 주세요.",
        "invalid_credentials" => "아이디 또는 비밀번호가 올바르지 않습니다.",
        "too_many_attempts" => "시도가 너무 많습니다. 잠시 후 다시 시도해 주세요.",
        "unauthenticated" => "로그인이 필요합니다.",
        "password_change_required" => "비밀번호를 먼저 변경해 주세요.",
        "forbidden" => "이 작업을 할 권한이 없습니다.",
        "not_found" => "대상을 찾을 수 없습니다.",
        "version_conflict" => "다른 사람이 먼저 수정했습니다. 새로 고친 뒤 다시 시도해 주세요.",
        "already_exists" => "이미 있는 항목입니다.",
        "in_use" => "사용 중이라 처리할 수 없습니다.",
        "login_id_taken" => "이미 사용 중인 아이디입니다.",
        "last_owner" => "최고 관리자는 최소 한 명 있어야 합니다.",
        "member_inactive" => "재직 중이 아닌 구성원이 포함되어 있습니다.",
        "password_reused" => "현재 비밀번호와 다른 비밀번호를 입력해 주세요.",
        "department_cycle" => "부서를 자신의 하위 부서 아래로 옮길 수 없습니다.",
        "department_too_deep" => "부서 단계가 너무 깊습니다.",
        "department_not_empty" => "구성원이나 하위 부서가 있는 부서는 삭제할 수 없습니다.",
        "not_clocked_in" => "출근 기록이 없습니다.",
        "already_clocked_out" => "이미 퇴근 처리되었습니다.",
        "clock_out_before_clock_in" => "퇴근 시각이 출근 시각보다 빠릅니다.",
        "not_working_member" => "재직 중인 구성원만 출퇴근을 기록할 수 있습니다.",
        "insufficient_leave_balance" => "남은 연차가 부족합니다.",
        "leave_overlap" => "이미 신청한 휴가와 기간이 겹칩니다.",
        "leave_no_working_days" => "선택한 기간에 근무일이 없습니다.",
        "leave_type_inactive" => "사용할 수 없는 휴가 종류입니다.",
        "leave_already_started" => {
            "이미 시작된 휴가는 취소할 수 없습니다. 관리자에게 문의해 주세요."
        }
        "document_not_editable" => "수정할 수 없는 문서입니다.",
        "document_already_closed" => "이미 처리가 끝난 문서입니다.",
        "not_current_approver" => "지금은 결재할 차례가 아닙니다.",
        "withdraw_too_late" => "결재가 진행되어 회수할 수 없습니다.",
        "form_inactive" => "사용할 수 없는 양식입니다.",
        "notice_archived" => "삭제된 공지입니다.",
        "payload_too_large" => "내용이 너무 큽니다.",
        "cross_origin_rejected" => "허용되지 않은 요청입니다. 페이지를 새로 고쳐 주세요.",
        "idempotency_key_reused" => "요청이 중복되었습니다. 페이지를 새로 고쳐 주세요.",
        _ => "요청을 처리하지 못했습니다. 잠시 후 다시 시도해 주세요.",
    };
    fixed.to_owned()
}

pub fn role_label(role: Role) -> &'static str {
    match role {
        Role::Owner => "최고 관리자",
        Role::Admin => "관리자",
        Role::Member => "구성원",
    }
}

pub fn member_status_label(status: MemberStatus) -> &'static str {
    match status {
        MemberStatus::Active => "재직",
        MemberStatus::Suspended => "정지",
        MemberStatus::Retired => "퇴사",
    }
}

/// 화면에서 미리 검증한 결과(`pango_core`의 검증 오류)를 서버 오류와 같은 문구로 바꾼다.
pub fn domain_error_message(error: &DomainError) -> String {
    match error {
        DomainError::Validation { field, reason } => {
            validation_message(Some(*field), Some(reason.code()))
        }
        other => error_message(other.code(), None, None),
    }
}

pub fn role_tone(role: Role) -> Tone {
    match role {
        Role::Owner => Tone::Warning,
        Role::Admin => Tone::Info,
        Role::Member => Tone::Neutral,
    }
}

pub fn member_status_tone(status: MemberStatus) -> Tone {
    match status {
        MemberStatus::Active => Tone::Success,
        MemberStatus::Suspended => Tone::Warning,
        MemberStatus::Retired => Tone::Neutral,
    }
}

/// 상태를 바꾸기 전에 확인 단계에서 보여 줄 영향.
pub fn member_status_effect(status: MemberStatus) -> &'static str {
    match status {
        MemberStatus::Active => "다시 로그인할 수 있게 됩니다.",
        MemberStatus::Suspended => {
            "로그인이 막히고 모든 기기에서 즉시 로그아웃됩니다. 조직장으로 지정된 부서가 있으면 해제됩니다. 근태·휴가·결재 기록은 그대로 남습니다."
        }
        MemberStatus::Retired => {
            "로그인이 막히고 모든 기기에서 즉시 로그아웃됩니다. 조직장으로 지정된 부서가 있으면 해제되고 퇴사일이 기록됩니다. 근태·휴가·결재 기록은 그대로 남습니다."
        }
    }
}

/// 감사 로그 동작 코드와 문구. 동작 필터의 선택지도 이 목록을 쓴다.
pub const AUDIT_ACTIONS: &[(&str, &str)] = &[
    ("auth.login", "로그인"),
    ("auth.logout", "로그아웃"),
    ("auth.password_change", "비밀번호 변경"),
    ("auth.session_revoke", "기기 로그아웃"),
    ("member.create", "구성원 추가"),
    ("member.update", "구성원 정보 수정"),
    ("member.role", "역할 변경"),
    ("member.status", "재직 상태 변경"),
    ("member.password_reset", "비밀번호 초기화"),
    ("department.create", "부서 추가"),
    ("department.update", "부서 수정"),
    ("department.archive", "부서 삭제"),
    ("position.create", "직책 추가"),
    ("position.update", "직책 수정"),
    ("position.archive", "직책 삭제"),
    ("attendance.create", "출퇴근 기록 추가"),
    ("attendance.edit", "출퇴근 기록 수정"),
    ("attendance.export", "근태 내보내기"),
    ("notice.create", "공지 게시"),
    ("notice.update", "공지 수정"),
    ("notice.archive", "공지 보관"),
    ("settings.update", "회사 설정 변경"),
    ("holiday.upsert", "공휴일 등록·변경"),
    ("holiday.delete", "공휴일 삭제"),
    ("holiday.import", "공휴일 일괄 등록"),
    ("system.initialize", "초기 설정"),
];

/// 감사 로그 동작 문구. 모르는 코드는 코드를 그대로 보여 준다(새 동작이 생겨도 기록은 보여야 한다).
pub fn audit_action_label(code: &str) -> String {
    AUDIT_ACTIONS
        .iter()
        .find(|(known, _)| *known == code)
        .map_or_else(|| code.to_owned(), |(_, label)| (*label).to_owned())
}

/// 감사 로그 대상 종류 문구. 모르는 종류는 그대로.
pub fn audit_target_label(target_type: &str) -> String {
    let label = match target_type {
        "member" => "구성원",
        "department" => "부서",
        "position" => "직책",
        "session" => "로그인 기기",
        "attendance_record" => "출퇴근 기록",
        "notice" => "공지",
        "company_settings" => "회사 설정",
        "holiday" => "공휴일",
        other => return other.to_owned(),
    };
    label.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pango_core::error::ConflictReason;

    #[test]
    fn audit_codes_have_labels_and_unknown_codes_fall_back() {
        assert_eq!(audit_action_label("notice.create"), "공지 게시");
        assert_eq!(audit_action_label("holiday.import"), "공휴일 일괄 등록");
        assert_eq!(audit_action_label("future.thing"), "future.thing");
        assert_eq!(audit_action_label(""), "");
        assert_eq!(audit_target_label("company_settings"), "회사 설정");
        assert_eq!(audit_target_label("<b>x</b>"), "<b>x</b>");
        let mut codes: Vec<_> = AUDIT_ACTIONS.iter().map(|(code, _)| *code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), AUDIT_ACTIONS.len(), "코드가 중복되지 않는다");
    }

    #[test]
    fn policy_fields_have_labels() {
        assert_eq!(
            error_message("validation_failed", Some("work_end"), Some("inconsistent")),
            "업무 종료 시각은 시작 시각보다 늦어야 합니다."
        );
        assert!(
            error_message("validation_failed", Some("department_id"), Some("empty"))
                .contains("대상 부서")
        );
        assert_eq!(
            error_message(
                "validation_failed",
                Some("quarter_minutes"),
                Some("invalid_format")
            ),
            "반반차 길이의 형식이 올바르지 않습니다."
        );
    }

    #[test]
    fn every_conflict_code_has_a_specific_message() {
        let fallback = error_message("__unknown__", None, None);
        for reason in ConflictReason::ALL {
            assert_ne!(
                error_message(reason.code(), None, None),
                fallback,
                "{}",
                reason.code()
            );
        }
    }

    #[test]
    fn client_validation_uses_server_wording() {
        use pango_core::error::ValidationReason;
        let error = DomainError::validation("hired_on", ValidationReason::Empty);
        assert_eq!(domain_error_message(&error), "입사일을(를) 입력해 주세요.");
        assert_eq!(
            domain_error_message(&DomainError::Forbidden),
            error_message("forbidden", None, None)
        );
    }

    #[test]
    fn statuses_and_roles_have_distinct_labels() {
        let labels: std::collections::BTreeSet<_> = MemberStatus::ALL
            .iter()
            .map(|s| member_status_label(*s))
            .collect();
        assert_eq!(labels.len(), MemberStatus::ALL.len());
        let roles: std::collections::BTreeSet<_> = Role::ALL.into_iter().map(role_label).collect();
        assert_eq!(roles.len(), Role::ALL.len());
        assert_ne!(
            member_status_tone(MemberStatus::Active),
            member_status_tone(MemberStatus::Suspended)
        );
        assert_ne!(
            member_status_effect(MemberStatus::Suspended),
            member_status_effect(MemberStatus::Retired)
        );
    }

    #[test]
    fn validation_messages_name_the_field() {
        assert_eq!(
            error_message("validation_failed", Some("title"), Some("empty")),
            "제목을(를) 입력해 주세요."
        );
        assert_eq!(
            error_message(
                "validation_failed",
                Some("current_password"),
                Some("invalid_format")
            ),
            "현재 비밀번호가 맞지 않습니다."
        );
        assert!(error_message("validation_failed", None, None).contains("입력값"));
        assert!(
            error_message("validation_failed", Some("unknown_field"), Some("weird"))
                .contains("unknown_field")
        );
    }
}
