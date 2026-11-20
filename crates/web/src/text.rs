//! 화면 문구. 서버의 코드 값(오류 코드, 상태 값)을 한국어로 바꾸는 곳은 여기뿐이다.

use pango_api::leave::LedgerKind;
use pango_core::authz::Role;
use pango_core::leave::{LeaveRequestStatus, LeaveUnit};
use pango_core::member::MemberStatus;

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
        "memo" => "메모",
        "code" => "코드",
        "effective_on" => "적용일",
        "leave_type_id" => "휴가 종류",
        "year" => "연도",
        "sort_order" => "정렬 순서",
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

pub fn leave_unit_label(unit: LeaveUnit) -> &'static str {
    match unit {
        LeaveUnit::FullDay => "종일",
        LeaveUnit::AmHalf => "오전 반차",
        LeaveUnit::PmHalf => "오후 반차",
        LeaveUnit::AmQuarter => "오전 반반차",
        LeaveUnit::PmQuarter => "오후 반반차",
    }
}

/// 달력 칸처럼 좁은 곳에 쓰는 짧은 단위 표기.
pub fn leave_unit_short(unit: LeaveUnit) -> &'static str {
    match unit {
        LeaveUnit::FullDay => "종일",
        LeaveUnit::AmHalf => "오전½",
        LeaveUnit::PmHalf => "오후½",
        LeaveUnit::AmQuarter => "오전¼",
        LeaveUnit::PmQuarter => "오후¼",
    }
}

pub fn leave_status_label(status: LeaveRequestStatus) -> &'static str {
    match status {
        LeaveRequestStatus::Pending => "결재 중",
        LeaveRequestStatus::Approved => "승인",
        LeaveRequestStatus::Rejected => "반려",
        LeaveRequestStatus::Canceled => "취소",
    }
}

pub fn ledger_kind_label(kind: LedgerKind) -> &'static str {
    match kind {
        LedgerKind::Grant => "부여",
        LedgerKind::Use => "사용",
        LedgerKind::Restore => "복원",
        LedgerKind::Adjust => "조정",
    }
}

/// 휴가 취소 실패 문구. 취소는 곧 결재 문서 회수이므로 회수 오류를 휴가 말로 바꾼다.
pub fn leave_cancel_error_message(code: &str, field: Option<&str>, reason: Option<&str>) -> String {
    match code {
        "withdraw_too_late" => {
            "결재가 이미 진행되어 취소할 수 없습니다. 결재자에게 반려를 요청해 주세요.".into()
        }
        "document_already_closed" => "이미 처리가 끝난 신청이라 취소할 수 없습니다.".into(),
        _ => error_message(code, field, reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pango_core::error::ConflictReason;

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

    #[test]
    fn every_leave_enum_value_has_a_label() {
        for unit in LeaveUnit::ALL {
            assert!(!leave_unit_label(unit).is_empty(), "{unit:?}");
            assert!(!leave_unit_short(unit).is_empty(), "{unit:?}");
        }
        for status in LeaveRequestStatus::ALL {
            assert!(!leave_status_label(status).is_empty(), "{status:?}");
        }
        for kind in [
            LedgerKind::Grant,
            LedgerKind::Use,
            LedgerKind::Restore,
            LedgerKind::Adjust,
        ] {
            assert!(!ledger_kind_label(kind).is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn leave_cancel_errors_speak_about_leave() {
        assert!(leave_cancel_error_message("withdraw_too_late", None, None).contains("취소"));
        assert_eq!(
            leave_cancel_error_message("leave_already_started", None, None),
            error_message("leave_already_started", None, None)
        );
        assert_eq!(
            error_message("validation_failed", Some("memo"), Some("empty")),
            "메모을(를) 입력해 주세요."
        );
    }
}
