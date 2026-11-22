//! 화면 문구. 서버의 코드 값(오류 코드, 상태 값)을 한국어로 바꾸는 곳은 여기뿐이다.

use pango_api::approval::FormRoute;
use pango_api::leave::{LedgerEntryDto, LedgerKind};
use pango_core::approval::{
    DocumentKind, DocumentStatus, EventKind, FormCategory, FormOrigin, Mailbox, StepKind,
    StepStatus,
};
use pango_core::attendance::{AttendanceEventKind, CorrectionStatus, DayStatus, WorkType};
use pango_core::authz::Role;
use pango_core::calendar_access::CalendarKind;
use pango_core::calendar_event::AttendeeResponse;
use pango_core::error::DomainError;
use pango_core::form_schema::FieldKind;
use pango_core::leave::{LeaveRequestStatus, LeaveUnit};
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
        "filename" => "파일 이름",
        "file" => "파일",
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
        "from" | "to" => "기간",
        "work_date" => "근무일",
        "work_type" => "근무 형태",
        "clock_in_at" => "출근 시각",
        "clock_out_at" => "퇴근 시각",
        "note" => "메모",
        "member_id" => "구성원",
        "memo" => "메모",
        "code" => "코드",
        "effective_on" => "적용일",
        "leave_type_id" => "휴가 종류",
        "form_id" => "양식",
        "description" => "설명",
        "body_template" => "본문 양식",
        "location" => "장소",
        "calendar_id" | "calendars" => "캘린더",
        "attendee_ids" | "member_ids" => "참석자",
        "start_at" | "end_at" => "일정 시간",
        "webhook_url" => "웹훅 주소",
        "webhook_secret" => "서명 키",
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
        (Some("steps"), Some("duplicate")) => "결재선에 중복된 결재자가 있습니다.".into(),
        (Some("steps"), Some("inconsistent")) => {
            "마지막 단계는 결재여야 하며, 본인은 결재자가 될 수 없습니다.".into()
        }
        (Some("references"), Some("inconsistent")) => {
            "본인이나 결재자는 참조자로 지정할 수 없습니다.".into()
        }
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
        (Some("work_date"), Some("inconsistent")) => {
            "출근 시각이 선택한 근무일에 속하지 않습니다.".into()
        }
        (Some("clock_out_at"), Some("inconsistent")) => {
            "퇴근 시각은 출근 시각보다 늦어야 합니다.".into()
        }
        (Some("clock_in_at" | "clock_out_at"), Some("out_of_range")) => {
            "근무는 24시간을 넘을 수 없고, 미래 시각은 입력할 수 없습니다.".into()
        }
        // 서버 `member::validate_hired_on`·`validate_retired_on`의 범위.
        (Some("hired_on"), Some("out_of_range")) => {
            "입사일은 1950. 1. 1.부터 오늘의 1년 뒤까지만 입력할 수 있습니다.".into()
        }
        (Some("retired_on"), Some("inconsistent")) => {
            "퇴사일은 입사일보다 이를 수 없습니다.".into()
        }
        // 일정(서버 `calendar_event::EventTime`·`check_attendees`).
        (Some("end_at"), Some("inconsistent")) => {
            "끝나는 시각은 시작 시각보다 늦어야 합니다.".into()
        }
        (Some("end_at"), Some("out_of_range")) => "일정은 31일까지 잡을 수 있습니다.".into(),
        (Some("end_date"), Some("inconsistent")) => {
            "끝나는 날은 시작하는 날보다 이를 수 없습니다.".into()
        }
        (Some("attendee_ids"), Some("out_of_range")) => {
            "참석자는 50명까지 초대할 수 있습니다.".into()
        }
        (Some("attendee_ids"), Some("inconsistent")) => {
            "참석자는 내 일정에만 초대할 수 있고, 본인은 초대할 수 없습니다.".into()
        }
        // 외부 알림 연동(서버 `outbound::WebhookUrl`).
        (Some("webhook_url"), Some("empty")) => {
            "외부 알림을 켜려면 웹훅 주소를 입력해 주세요.".into()
        }
        (Some("webhook_url"), Some("too_long")) => "웹훅 주소가 너무 길어요.".into(),
        (Some("webhook_url"), _) => {
            "https:// 로 시작하는 주소를 입력해 주세요. 아이디·비밀번호가 들어간 주소는 쓸 수 없어요."
                .into()
        }
        (Some("webhook_secret"), _) => "서명 키를 먼저 만들어 주세요.".into(),
        (Some("pubkey"), _) => {
            "Buzz ID는 npub1로 시작하는 값(또는 64자리 16진수)이에요. Buzz에서 복사한 값을 그대로 붙여 넣어 주세요."
                .into()
        }
        (Some("buzz_code"), _) => "확인 코드는 숫자 6자리예요.".into(),
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
// 아래 `attachment_limit` 문구의 개수와 서버 상수가 어긋나지 않게 한다.
const _: () = assert!(pango_core::attachment::MAX_PER_OWNER == 10);

pub fn error_message(code: &str, field: Option<&str>, reason: Option<&str>) -> String {
    let fixed = match code {
        "validation_failed" => return validation_message(field, reason),
        "network_error" => "서버에 연결할 수 없습니다. 네트워크를 확인해 주세요.",
        "invalid_credentials" => "아이디 또는 비밀번호가 올바르지 않습니다.",
        "mfa_required" => "인증 앱의 6자리 코드를 입력해 주세요.",
        "mfa_invalid" => "인증 코드가 맞지 않거나 이미 쓴 코드예요. 새 코드로 다시 입력해 주세요.",
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
        "attachment_limit" => "첨부파일은 10개까지 올릴 수 있어요.",
        "buzz_unavailable" => "회사에서 Buzz 연동을 켜지 않았어요. 관리자에게 문의해 주세요.",
        "buzz_account_taken" => "이미 다른 구성원이 연결한 Buzz ID예요. 내 Buzz ID가 맞는지 확인해 주세요.",
        "buzz_code_wrong" => "확인 코드가 맞지 않아요. Buzz DM으로 받은 6자리를 다시 확인해 주세요.",
        "buzz_code_expired" => "확인 코드의 유효 시간(10분)이 지났어요. 코드를 다시 받아 주세요.",
        "buzz_code_exhausted" => "확인 코드를 여러 번 틀려 더는 쓸 수 없어요. 코드를 다시 받아 주세요.",
        "buzz_code_missing" => "받은 확인 코드가 없어요. 먼저 Buzz ID를 입력하고 코드를 받아 주세요.",
        "buzz_bot_rejected" => {
            "Buzz가 알림 봇을 받아 주지 않았어요. 관리자에게 Buzz 연동 설정을 확인해 달라고 요청해 주세요."
        }
        "buzz_recipient_rejected" => {
            "Buzz가 이 ID로 보내는 메시지를 거절했어요. 회사 Buzz에 가입한 내 ID가 맞는지 확인해 주세요."
        }
        "buzz_unreachable" => "Buzz에 연결하지 못했어요. 잠시 뒤 다시 시도하고, 계속되면 관리자에게 문의해 주세요.",
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

/// 화면 입력 상자 `field`의 검증 문구. 도메인 오류의 필드 이름(`code` 등)이 아니라 입력 상자를 기준으로 쓴다.
pub fn field_error_message(field: &str, error: &DomainError) -> String {
    match error {
        DomainError::Validation { reason, .. } => {
            validation_message(Some(field), Some(reason.code()))
        }
        _ => validation_message(Some(field), None),
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
    ("member.totp_reset", "2단계 인증 초기화"),
    ("auth.totp_enable", "2단계 인증 켜기"),
    ("auth.totp_disable", "2단계 인증 끄기"),
    ("member.session_revoke", "구성원 기기 로그아웃"),
    ("member.sessions_revoke_all", "구성원 모든 기기 로그아웃"),
    ("department.create", "부서 추가"),
    ("department.update", "부서 수정"),
    ("department.archive", "부서 삭제"),
    ("position.create", "직책 추가"),
    ("position.update", "직책 수정"),
    ("position.archive", "직책 삭제"),
    ("attendance.create", "출퇴근 기록 추가"),
    ("attendance.edit", "출퇴근 기록 수정"),
    ("attendance.export", "근태 내보내기"),
    ("attendance.correction_request", "근태 정정 요청"),
    ("attendance.correction_cancel", "근태 정정 요청 취소"),
    ("attendance.correction", "근태 정정 반영"),
    ("leave.request", "휴가 신청"),
    ("leave.cancel", "휴가 취소"),
    ("leave.admin_cancel", "휴가 취소(관리자)"),
    ("leave.adjust", "휴가 수량 조정"),
    ("leave.accrual_run", "연차 부여 실행"),
    ("leave_type.create", "휴가 종류 추가"),
    ("leave_type.update", "휴가 종류 수정"),
    ("approval_form.create", "결재 양식 추가"),
    ("approval_form.update", "결재 양식 수정"),
    ("approval_form.duplicate", "결재 양식 복제"),
    ("approval_form.catalog_seed", "기본 결재 양식 추가"),
    ("approval.default_reference_skipped", "기본 참조자 제외"),
    ("approval.admin_view", "결재 문서 열람(관리자)"),
    ("approval.admin_reject", "결재 문서 대리 반려(관리자)"),
    ("approval.draft_delete", "임시저장 문서 삭제"),
    ("notice.create", "공지 게시"),
    ("notice.update", "공지 수정"),
    ("notice.archive", "공지 보관"),
    ("approval.attachment.add", "결재 문서 첨부파일 추가"),
    ("approval.attachment.remove", "결재 문서 첨부파일 삭제"),
    (
        "approval.attachment.admin_download",
        "결재 문서 첨부파일 열람(관리자)",
    ),
    ("notice.attachment.add", "공지 첨부파일 추가"),
    ("notice.attachment.remove", "공지 첨부파일 삭제"),
    ("settings.update", "회사 설정 변경"),
    ("integration.webhook_update", "외부 알림 연동 변경"),
    (
        "integration.webhook_secret_rotate",
        "외부 알림 서명 키 교체",
    ),
    ("integration.webhook_test", "외부 알림 테스트 발송"),
    ("integration.buzz_update", "Buzz 연동 변경"),
    ("buzz.link_start", "Buzz 연결 확인 코드 발송"),
    ("buzz.link", "Buzz 계정 연결"),
    ("buzz.unlink", "Buzz 연결 해제"),
    ("buzz.admin_unlink", "Buzz 연결 해제(관리자)"),
    ("buzz.admin_link", "Buzz ID 등록(관리자)"),
    ("holiday.upsert", "공휴일 등록·변경"),
    ("holiday.delete", "공휴일 삭제"),
    ("holiday.import", "공휴일 일괄 등록"),
    ("calendar.event_create", "공유 일정 추가"),
    ("calendar.event_update", "공유 일정 수정"),
    ("calendar.event_delete", "공유 일정 삭제"),
    ("system.initialize", "초기 설정"),
    ("retention.update", "기록 보존 기간 변경"),
    ("retention.purge", "보존 기간 지난 출퇴근 기록 정리"),
];

/// 알림 종류 코드와 문구. 알림 목록에서 제목 위에 붙인다.
pub const NOTIFICATION_KINDS: &[(&str, &str)] = &[
    ("notice.new", "공지"),
    ("approval.todo", "결재 요청"),
    ("approval.reference", "참조"),
    ("approval.approved", "최종 승인"),
    ("approval.rejected", "반려"),
    ("approval.admin_rejected", "관리자 대리 반려"),
    ("approval.withdrawn", "회수"),
    ("attendance.corrected", "근태 정정 반영"),
    ("attendance.clock_out_missing", "퇴근 누락"),
    ("leave.canceled", "휴가 취소"),
    ("calendar.invited", "일정 초대"),
    ("calendar.updated", "일정 변경"),
    ("calendar.canceled", "일정 취소"),
    ("calendar.responded", "참석 응답"),
    ("buzz.linked", "Buzz 연결"),
];

/// 알림 종류 문구. 모르는 종류는 `None`(제목만 보여 준다).
pub fn notification_kind_label(kind: &str) -> Option<&'static str> {
    NOTIFICATION_KINDS
        .iter()
        .find(|(known, _)| *known == kind)
        .map(|(_, label)| *label)
}

/// 관리자(최고 관리자 제외)가 자기 개인 기록을 관리자 권한으로 다루려 할 때의 안내.
/// 서버 `authz::edit_personal_records`가 403으로 막는다.
pub const OWN_RECORD_HINT: &str = "본인 기록은 다른 관리자나 최고 관리자가 처리합니다.";

/// 연차 부여 경계를 넘는 휴가 신청이 잔여 부족으로 거부됐을 때 덧붙이는 안내. 서버는 기간
/// 전체를 첫날에 유효한 부여에서 차감하므로, 화면의 총 잔여가 넉넉해도 거부될 수 있다.
pub const LEAVE_GRANT_BOUNDARY_HINT: &str =
    "연차 유효기간(부여일·소멸일)을 넘는 기간은 나눠서 신청해 주세요.";

/// 문서 이력에서 관리자가 결재자 대신 반려한 줄의 동사.
pub const PROXY_REJECT_VERB: &str = "대리 반려";

/// 결재선에서 관리자가 대신 반려한 단계 아래에 붙는 설명.
pub fn proxy_rejection_note(admin_name: &str) -> String {
    format!("관리자 {admin_name}님이 대리 반려")
}

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
        "calendar_event" => "일정",
        "attendance_correction" => "근태 정정 요청",
        "leave_request" => "휴가 신청",
        "leave_ledger" => "휴가 원장",
        "leave_type" => "휴가 종류",
        "approval_document" => "결재 문서",
        "approval_form" => "결재 양식",
        "notice" => "공지",
        "company_settings" => "회사 설정",
        "outbound_settings" => "외부 알림 연동",
        "buzz_settings" => "Buzz 연동",
        "holiday" => "공휴일",
        other => return other.to_owned(),
    };
    label.to_owned()
}

pub fn work_type_label(work_type: WorkType) -> &'static str {
    match work_type {
        WorkType::Office => "사무실",
        WorkType::Remote => "재택",
        WorkType::Field => "외근",
    }
}

pub fn day_status_label(status: DayStatus) -> &'static str {
    match status {
        DayStatus::Scheduled => "예정",
        DayStatus::Working => "근무 중",
        DayStatus::Normal => "정상",
        DayStatus::Late => "지각",
        DayStatus::EarlyLeave => "조퇴",
        DayStatus::LateAndEarlyLeave => "지각·조퇴",
        DayStatus::MissingClockOut => "퇴근 누락",
        DayStatus::Absent => "결근",
        DayStatus::OnLeave => "휴가",
        DayStatus::DayOff => "휴무",
    }
}

/// 상태 배지 색. 색만으로 구분하지 않도록 배지에는 항상 [`day_status_label`]을 함께 쓴다.
pub fn day_status_tone(status: DayStatus) -> Tone {
    match status {
        DayStatus::Normal => Tone::Success,
        DayStatus::Late | DayStatus::EarlyLeave | DayStatus::LateAndEarlyLeave => Tone::Warning,
        DayStatus::Absent | DayStatus::MissingClockOut => Tone::Danger,
        DayStatus::Working | DayStatus::OnLeave => Tone::Info,
        DayStatus::Scheduled | DayStatus::DayOff => Tone::Neutral,
    }
}

pub fn attendance_event_label(kind: AttendanceEventKind) -> &'static str {
    match kind {
        AttendanceEventKind::ClockIn => "출근",
        AttendanceEventKind::ClockOut => "퇴근",
        AttendanceEventKind::AdminCreate => "관리자 추가",
        AttendanceEventKind::AdminEdit => "관리자 수정",
        AttendanceEventKind::Correction => "정정 승인",
    }
}

pub fn correction_status_label(status: CorrectionStatus) -> &'static str {
    match status {
        CorrectionStatus::Pending => "결재 중",
        CorrectionStatus::Approved => "반영됨",
        CorrectionStatus::Rejected => "반려",
        CorrectionStatus::Canceled => "취소",
    }
}

/// 색만으로 구분하지 않도록 배지에는 항상 [`correction_status_label`]을 함께 쓴다.
pub fn correction_status_tone(status: CorrectionStatus) -> Tone {
    match status {
        CorrectionStatus::Pending => Tone::Warning,
        CorrectionStatus::Approved => Tone::Success,
        CorrectionStatus::Rejected => Tone::Danger,
        CorrectionStatus::Canceled => Tone::Neutral,
    }
}

/// 개인 신청(휴가·근태 정정)의 결재선 오류. 서버는 최종 결재자가 신청자의 관할 조직장이나
/// 관리자가 아니어도 `steps`의 `inconsistent`로 답하므로, 그 규칙까지 함께 알려 준다.
const PERSONAL_LINE_INCONSISTENT: &str = "마지막 결재자는 내 조직장 또는 관리자여야 합니다. 마지막 단계는 결재여야 하고, 본인은 결재자가 될 수 없습니다.";

/// 근태 정정 요청·취소 실패 문구.
pub fn correction_error_message(code: &str, field: Option<&str>, reason: Option<&str>) -> String {
    match (code, field, reason) {
        ("validation_failed", Some("steps"), Some("inconsistent")) => {
            PERSONAL_LINE_INCONSISTENT.into()
        }
        ("validation_failed", Some("work_date"), Some("out_of_range")) => {
            "정정은 오늘부터 31일 전까지의 근무일만 요청할 수 있습니다.".into()
        }
        ("already_exists", _, _) => {
            "이 날짜에 결재 중인 정정 요청이 이미 있습니다. 정정 요청 내역에서 확인해 주세요."
                .into()
        }
        ("withdraw_too_late", _, _) => {
            "결재가 이미 진행되어 취소할 수 없습니다. 결재자에게 반려를 요청해 주세요.".into()
        }
        ("document_already_closed", _, _) => "이미 처리가 끝난 요청이라 취소할 수 없습니다.".into(),
        ("version_conflict", _, _) => {
            "그사이 요청 상태가 바뀌었습니다. 목록을 새로 고쳤으니 다시 확인해 주세요.".into()
        }
        _ => error_message(code, field, reason),
    }
}

/// `DayStatus`에는 `ALL`이 없어 테스트·화면이 함께 쓰는 목록을 둔다. 새 상태가 생기면
/// `day_status_label`의 `match`가 먼저 컴파일 오류를 내므로 여기도 함께 고친다.
#[cfg(test)]
pub const DAY_STATUSES: [DayStatus; 10] = [
    DayStatus::Scheduled,
    DayStatus::Working,
    DayStatus::Normal,
    DayStatus::Late,
    DayStatus::EarlyLeave,
    DayStatus::LateAndEarlyLeave,
    DayStatus::MissingClockOut,
    DayStatus::Absent,
    DayStatus::OnLeave,
    DayStatus::DayOff,
];
pub fn document_status_label(status: DocumentStatus) -> &'static str {
    match status {
        DocumentStatus::Draft => "임시저장",
        DocumentStatus::InProgress => "진행 중",
        DocumentStatus::Approved => "승인",
        DocumentStatus::Rejected => "반려",
        DocumentStatus::Withdrawn => "회수",
    }
}

/// 상태 배지의 색. 색만으로 상태를 알리지 않도록 배지에는 항상 [`document_status_label`]을 함께 쓴다.
pub fn document_status_tone(status: DocumentStatus) -> Tone {
    match status {
        DocumentStatus::Draft | DocumentStatus::Withdrawn => Tone::Neutral,
        DocumentStatus::InProgress => Tone::Info,
        DocumentStatus::Approved => Tone::Success,
        DocumentStatus::Rejected => Tone::Danger,
    }
}

pub fn step_kind_label(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Approve => "결재",
        StepKind::Agree => "합의",
    }
}

pub fn step_status_label(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Waiting => "대기",
        StepStatus::Pending => "결재 차례",
        StepStatus::Approved => "승인",
        StepStatus::Rejected => "반려",
    }
}

pub fn step_status_tone(status: StepStatus) -> Tone {
    match status {
        StepStatus::Waiting => Tone::Neutral,
        StepStatus::Pending => Tone::Warning,
        StepStatus::Approved => Tone::Success,
        StepStatus::Rejected => Tone::Danger,
    }
}

pub fn document_kind_label(kind: DocumentKind) -> &'static str {
    match kind {
        DocumentKind::General => "일반",
        DocumentKind::Leave => "휴가 신청",
        DocumentKind::AttendanceCorrection => "근태 정정",
    }
}

pub fn mailbox_label(mailbox: Mailbox) -> &'static str {
    match mailbox {
        Mailbox::Todo => "결재 대기",
        Mailbox::Upcoming => "결재 예정",
        Mailbox::Drafted => "기안함",
        Mailbox::Done => "완료",
        Mailbox::Reference => "참조",
    }
}

/// 문서 이력 한 줄의 동사. `홍길동 · 상신`처럼 행위자 뒤에 붙인다.
pub fn event_kind_label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Submitted => "상신",
        EventKind::Approved => "승인",
        EventKind::Completed => "최종 승인",
        EventKind::Rejected => "반려",
        EventKind::Withdrawn => "회수",
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

/// 캘린더 종류. 부서 일정은 화면에서 부서 이름(`CalendarDto::name`)을 우선 쓴다.
pub fn calendar_kind_label(kind: CalendarKind) -> &'static str {
    match kind {
        CalendarKind::Personal => "내 일정",
        CalendarKind::Department => "부서 일정",
        CalendarKind::Company => "전사 일정",
    }
}

/// 참석자 응답.
pub fn attendee_response_label(response: AttendeeResponse) -> &'static str {
    match response {
        AttendeeResponse::Pending => "응답 대기",
        AttendeeResponse::Accepted => "참석",
        AttendeeResponse::Declined => "불참",
    }
}

pub fn attendee_response_tone(response: AttendeeResponse) -> Tone {
    match response {
        AttendeeResponse::Pending => Tone::Warning,
        AttendeeResponse::Accepted => Tone::Success,
        AttendeeResponse::Declined => Tone::Neutral,
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

pub fn leave_status_tone(status: LeaveRequestStatus) -> Tone {
    match status {
        LeaveRequestStatus::Pending => Tone::Warning,
        LeaveRequestStatus::Approved => Tone::Success,
        LeaveRequestStatus::Rejected => Tone::Danger,
        LeaveRequestStatus::Canceled => Tone::Neutral,
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

/// 원장 한 줄의 설명(사용 가능 기간, 적용일 등). 종류마다 다르다.
pub fn ledger_detail(entry: &LedgerEntryDto) -> String {
    let ymd = crate::format::date_ymd;
    match entry.kind {
        LedgerKind::Grant => match (entry.valid_from, entry.expires_on) {
            (Some(from), Some(until)) => format!("{} ~ {} 사용 가능", ymd(from), ymd(until)),
            (Some(from), None) => format!("{}부터 사용 가능 · 소멸 없음", ymd(from)),
            (None, Some(until)) => format!("{}까지 사용 가능", ymd(until)),
            (None, None) => "소멸 없음".into(),
        },
        LedgerKind::Use => entry
            .effective_on
            .map(|d| format!("{} 휴가", ymd(d)))
            .unwrap_or_else(|| "휴가 사용".into()),
        LedgerKind::Restore => "신청 취소·반려로 복원".into(),
        LedgerKind::Adjust => entry
            .effective_on
            .map(|d| format!("{} 적용 · 관리자 조정", ymd(d)))
            .unwrap_or_else(|| "관리자 조정".into()),
    }
}

/// 원장 한 줄의 색: 늘어나면 초록, 사용은 파랑, 조정은 회색.
pub fn ledger_kind_tone(kind: LedgerKind) -> Tone {
    match kind {
        LedgerKind::Grant | LedgerKind::Restore => Tone::Success,
        LedgerKind::Use => Tone::Info,
        LedgerKind::Adjust => Tone::Neutral,
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

// ───────────── 결재 양식 ─────────────

/// 양식 분류의 이름. 서버 응답의 `category_label`과 같은 값이다.
pub fn form_category_label(category: FormCategory) -> &'static str {
    match category {
        FormCategory::General => "일반",
        FormCategory::Hr => "인사",
        FormCategory::Attendance => "근태",
        FormCategory::Trip => "출장",
        FormCategory::Accounting => "회계",
        FormCategory::Support => "지원",
        FormCategory::Welfare => "복지",
        FormCategory::Etc => "기타",
    }
}

/// 양식의 출처: 제품이 넣은 기본 양식인지, 관리자가 만든 양식인지.
pub fn form_origin_label(origin: FormOrigin) -> &'static str {
    match origin {
        FormOrigin::Catalog => "기본",
        FormOrigin::Custom => "사용자",
    }
}

/// 시스템 양식을 고르면 가게 되는 화면의 안내. 일반 양식은 안내가 없다.
pub fn form_route_hint(route: FormRoute) -> Option<&'static str> {
    match route {
        FormRoute::ApprovalNew => None,
        FormRoute::LeaveNew => Some("휴가 신청 화면에서 작성해요"),
        FormRoute::Attendance => Some("근태 화면에서 정정을 요청해요"),
    }
}

/// 필드 종류의 이름(양식 관리의 필드 편집).
pub fn field_kind_label(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Text { .. } => "한 줄 글",
        FieldKind::Textarea { .. } => "여러 줄 글",
        FieldKind::Number { .. } => "숫자",
        FieldKind::Money => "금액",
        FieldKind::Date => "날짜",
        FieldKind::DateRange => "기간",
        FieldKind::TimeRange => "시간대",
        FieldKind::Select { .. } => "선택",
        FieldKind::Checkbox => "체크",
        FieldKind::Member => "구성원",
        FieldKind::Table { .. } => "표",
    }
}

/// 양식이 바뀌어 화면의 필드에 붙일 수 없는 오류가 왔을 때의 안내.
pub const FORM_CHANGED_HINT: &str =
    "양식이 바뀌어 일부 항목을 확인할 수 없어요. 화면을 새로 고친 뒤 다시 작성해 주세요.";

/// 숫자 필드의 허용 범위 안내. 범위가 없으면 빈 문자열.
pub fn number_range_hint(min: Option<i64>, max: Option<i64>) -> String {
    match (min, max) {
        (Some(min), Some(max)) => format!("{min}부터 {max}까지 입력할 수 있어요."),
        (Some(min), None) => format!("{min} 이상으로 입력해 주세요."),
        (None, Some(max)) => format!("{max} 이하로 입력해 주세요."),
        (None, None) => String::new(),
    }
}

/// 양식 필드 값의 오류 문구. `reason`은 화면 검증(`FieldReason::code`)이나 서버 `error.fields`의
/// 사유 코드다. 모르는 코드는 일반 문구로 처리한다.
pub fn form_value_error(kind: &FieldKind, reason: &str) -> String {
    let fixed = match (reason, kind) {
        ("empty", FieldKind::Checkbox) => "체크해 주세요.",
        ("empty", FieldKind::Table { .. }) => "한 행 이상 입력해 주세요.",
        ("empty", FieldKind::Select { .. }) => "하나를 골라 주세요.",
        ("empty", FieldKind::Member) => "구성원을 골라 주세요.",
        ("empty", FieldKind::Date) => "날짜를 골라 주세요.",
        ("empty", FieldKind::DateRange) => "시작일과 종료일을 골라 주세요.",
        ("empty", FieldKind::TimeRange) => "시작 시각과 종료 시각을 입력해 주세요.",
        ("empty", _) => "꼭 입력해야 하는 항목이에요.",
        ("too_long", FieldKind::Table { .. }) => "행은 50개까지 넣을 수 있어요.",
        ("too_long", _) => "글자 수가 너무 많아요.",
        ("invalid_character", _) => "쓸 수 없는 문자가 들어 있어요.",
        ("invalid_format", FieldKind::Number { .. }) => "소수점 없이 숫자만 입력해 주세요.",
        ("invalid_format", FieldKind::Money) => "금액은 숫자만 입력해 주세요.",
        ("invalid_format", FieldKind::Date) => "날짜를 다시 골라 주세요.",
        ("invalid_format", FieldKind::DateRange) => "시작일과 종료일을 모두 골라 주세요.",
        ("invalid_format", FieldKind::TimeRange) => "시작 시각과 종료 시각을 모두 입력해 주세요.",
        ("invalid_format", FieldKind::Member) => "구성원을 다시 골라 주세요.",
        ("invalid_format", FieldKind::Text { .. }) => "한 줄로 입력해 주세요.",
        ("out_of_range", FieldKind::Number { min, max }) => {
            let hint = number_range_hint(*min, *max);
            return if hint.is_empty() {
                "숫자가 너무 크거나 작아요.".to_owned()
            } else {
                hint
            };
        }
        ("out_of_range", FieldKind::Money) => "0원부터 1조 원까지 입력할 수 있어요.",
        ("out_of_range", FieldKind::DateRange) => "기간은 366일까지 고를 수 있어요.",
        ("out_of_range", _) => "입력할 수 있는 범위를 벗어났어요.",
        ("inconsistent", FieldKind::DateRange) => "종료일이 시작일보다 빨라요.",
        ("inconsistent", FieldKind::TimeRange) => "종료 시각은 시작 시각보다 늦어야 해요.",
        ("not_an_option", _) => "고를 수 없는 값이에요. 다시 골라 주세요.",
        ("not_found", _) => "찾을 수 없는 구성원이에요. 다시 골라 주세요.",
        ("member_inactive", _) => "재직 중이 아닌 구성원이에요. 다른 사람을 골라 주세요.",
        ("unknown_field", _) => "양식에 없는 항목이에요. 화면을 새로 고쳐 주세요.",
        _ => "입력한 내용을 다시 확인해 주세요.",
    };
    fixed.to_owned()
}

/// 표의 한 행 전체에 붙는 오류 문구(칸에 붙일 수 없는 오류).
pub fn form_row_error(reason: &str) -> &'static str {
    match reason {
        "unknown_field" => "양식에 없는 칸이 있어요. 행을 지우고 다시 입력해 주세요.",
        _ => "이 행을 다시 확인해 주세요.",
    }
}

/// 양식의 필드 정의 오류 문구. `part`는 오류 위치의 마지막 이름(`key`, `label`, `options` …),
/// `reason`은 사유 코드다.
pub fn schema_error_message(part: &str, reason: &str) -> String {
    let fixed = match (part, reason) {
        ("fields", "too_long") => "필드는 40개까지 만들 수 있어요.",
        ("key", "empty") => "키를 입력해 주세요.",
        ("key", "too_long") => "키는 40자까지 쓸 수 있어요.",
        ("key", "invalid_character") => "키는 영문 소문자·숫자·밑줄(_)만 쓸 수 있어요.",
        ("key", "invalid_format") => "키는 영문 소문자로 시작해야 해요.",
        ("key", "duplicate") => "같은 키가 이미 있어요. 다른 키를 써 주세요.",
        ("label", "empty") => "이름을 입력해 주세요.",
        ("label", "too_long") => "이름은 50자까지 쓸 수 있어요.",
        ("help", "too_long") => "안내 문구는 200자까지 쓸 수 있어요.",
        ("max_len", "invalid_format") => "최대 글자 수는 숫자로 입력해 주세요.",
        ("max_len", _) => "최대 글자 수는 1부터 종류의 기본값(한 줄 200, 여러 줄 5,000)까지예요.",
        ("min" | "max", "invalid_format") => "소수점 없이 숫자만 입력해 주세요.",
        ("min", "inconsistent") => "최솟값이 최댓값보다 커요.",
        ("min" | "max", "out_of_range") => "숫자가 너무 크거나 작아요.",
        ("options", "empty") => "선택지를 하나 이상 입력해 주세요.",
        ("options", "too_long") => "선택지는 30개까지, 하나에 50자까지 쓸 수 있어요.",
        ("options", "duplicate") => "같은 선택지가 두 번 들어 있어요.",
        ("columns", "empty") => "열을 하나 이상 만들어 주세요.",
        ("columns", "too_long") => "열은 8개까지 만들 수 있어요.",
        ("kind", _) => "표의 열에는 쓸 수 없는 종류예요.",
        (_, "invalid_character") => "쓸 수 없는 문자가 들어 있어요.",
        _ => "다시 확인해 주세요.",
    };
    fixed.to_owned()
}

/// 외부 알림 발송 실패 사유(서버의 짧은 코드)를 문구로. 모르는 코드는 그대로 보여 준다.
pub fn delivery_error_label(reason: &str) -> String {
    if let Some(status) = reason.strip_prefix("http_") {
        return format!("받는 쪽이 {status} 상태로 응답했어요");
    }
    let fixed = match reason {
        "timeout" => "응답을 기다리다 시간이 지났어요",
        "connect_failed" => "받는 쪽에 연결하지 못했어요",
        "request_failed" => "요청을 보내지 못했어요",
        "invalid_url" => "주소를 해석하지 못했어요",
        "blocked_address" => "허용되지 않은 주소예요",
        "encode_failed" => "보낼 내용을 만들지 못했어요",
        "integration_disabled" => "연동이 꺼져 있어 보내지 않았어요",
        "recipient_inactive" => "받는 사람이 재직 중이 아니라 보내지 않았어요",
        "recipient_opted_out" => "받는 사람이 외부 알림을 꺼서 보내지 않았어요",
        "interrupted" => "보내는 도중 서버가 멈춰 끝내지 못했어요",
        "recipient_unlinked" => "받는 사람이 Buzz 연결을 끊어 보내지 않았어요",
        "buzz_rejected" => "Buzz가 메시지를 받지 않았어요",
        "buzz_bad_response" => "Buzz의 응답을 이해하지 못했어요",
        "sign_failed" => "메시지에 서명하지 못했어요",
        other => return other.to_owned(),
    };
    fixed.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pango_core::error::ConflictReason;

    #[test]
    fn delivery_errors_read_as_sentences() {
        assert_eq!(
            delivery_error_label("http_404"),
            "받는 쪽이 404 상태로 응답했어요"
        );
        assert_eq!(
            delivery_error_label("timeout"),
            "응답을 기다리다 시간이 지났어요"
        );
        assert_eq!(delivery_error_label("future_reason"), "future_reason");
        assert_eq!(
            delivery_error_label("blocked_address"),
            "허용되지 않은 주소예요"
        );
        assert!(
            validation_message(Some("webhook_url"), Some("invalid_format")).contains("https://")
        );
        assert!(validation_message(Some("webhook_url"), Some("empty")).contains("켜려면"));
        assert!(validation_message(Some("webhook_secret"), Some("empty")).contains("서명 키"));
    }

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

    /// 서버가 남기는 감사 로그 동작 코드. `crates/server/src/service`의 `Audit { action: "…" }`와
    /// `audit_request`·`audit_correction`에 넘기는 문자열이다. 서버에 동작이 생기거나 없어지면
    /// 여기와 [`AUDIT_ACTIONS`]를 함께 고친다(`grep -rn 'action: "' crates/server/src/service`).
    const BACKEND_AUDIT_ACTIONS: &[&str] = &[
        "approval.admin_reject",
        "approval.admin_view",
        "approval.draft_delete",
        "approval.default_reference_skipped",
        "approval_form.catalog_seed",
        "approval_form.create",
        "approval_form.duplicate",
        "approval_form.update",
        "attendance.correction",
        "attendance.correction_cancel",
        "attendance.correction_request",
        "attendance.create",
        "attendance.edit",
        "attendance.export",
        "calendar.event_create",
        "calendar.event_delete",
        "calendar.event_update",
        "auth.login",
        "auth.logout",
        "auth.password_change",
        "auth.totp_disable",
        "auth.totp_enable",
        "auth.session_revoke",
        "department.archive",
        "department.create",
        "department.update",
        "holiday.delete",
        "holiday.import",
        "holiday.upsert",
        "buzz.admin_link",
        "buzz.admin_unlink",
        "buzz.link",
        "buzz.link_start",
        "buzz.unlink",
        "integration.buzz_update",
        "integration.webhook_secret_rotate",
        "integration.webhook_test",
        "integration.webhook_update",
        "leave.accrual_run",
        "leave.adjust",
        "leave.admin_cancel",
        "leave.cancel",
        "leave.request",
        "leave_type.create",
        "leave_type.update",
        "member.create",
        "member.password_reset",
        "member.totp_reset",
        "member.session_revoke",
        "member.sessions_revoke_all",
        "member.role",
        "member.status",
        "member.update",
        "notice.archive",
        "approval.attachment.add",
        "approval.attachment.admin_download",
        "approval.attachment.remove",
        "notice.attachment.add",
        "notice.attachment.remove",
        "notice.create",
        "notice.update",
        "retention.purge",
        "retention.update",
        "position.archive",
        "position.create",
        "position.update",
        "settings.update",
        "system.initialize",
    ];

    /// 서버가 만드는 알림 종류. `crates/server/src/service`의 `Notify { kind: "…" }`와
    /// `approval::notify_event_tx`, `notices::NEW_NOTICE`, `calendar`의 `INVITED`·`UPDATED`·
    /// `CANCELED`·`RESPONDED` 문자열이다.
    const BACKEND_NOTIFICATION_KINDS: &[&str] = &[
        "approval.admin_rejected",
        "approval.approved",
        "approval.reference",
        "approval.rejected",
        "approval.todo",
        "approval.withdrawn",
        "attendance.clock_out_missing",
        "attendance.corrected",
        "buzz.linked",
        "calendar.canceled",
        "calendar.invited",
        "calendar.responded",
        "calendar.updated",
        "leave.canceled",
        "notice.new",
    ];

    fn sorted_codes(list: &[(&str, &str)]) -> Vec<String> {
        let mut codes: Vec<String> = list.iter().map(|(code, _)| (*code).to_owned()).collect();
        codes.sort_unstable();
        codes
    }

    #[test]
    fn every_backend_audit_action_has_a_label() {
        for code in BACKEND_AUDIT_ACTIONS {
            let label = audit_action_label(code);
            assert_ne!(label, *code, "{code}에 문구가 없다");
            assert!(!label.trim().is_empty(), "{code}");
        }
        // 서버에 없는 코드가 남아 있지 않다(필터 선택지에 쓰인다).
        let mut backend: Vec<String> = BACKEND_AUDIT_ACTIONS
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
        backend.sort_unstable();
        assert_eq!(sorted_codes(AUDIT_ACTIONS), backend);
        assert_eq!(
            audit_action_label("approval.admin_reject"),
            "결재 문서 대리 반려(관리자)"
        );
    }

    #[test]
    fn every_backend_notification_kind_has_a_label() {
        for kind in BACKEND_NOTIFICATION_KINDS {
            let label = notification_kind_label(kind);
            assert!(label.is_some_and(|l| !l.trim().is_empty()), "{kind}");
        }
        let mut backend: Vec<String> = BACKEND_NOTIFICATION_KINDS
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
        backend.sort_unstable();
        assert_eq!(sorted_codes(NOTIFICATION_KINDS), backend);
        assert_eq!(
            notification_kind_label("approval.admin_rejected"),
            Some("관리자 대리 반려")
        );
        assert_eq!(notification_kind_label("future.kind"), None);
        assert_eq!(notification_kind_label(""), None);
    }

    #[test]
    fn member_date_errors_are_specific() {
        let hired = error_message("validation_failed", Some("hired_on"), Some("out_of_range"));
        assert!(hired.contains("1950"), "{hired}");
        assert_eq!(
            error_message(
                "validation_failed",
                Some("retired_on"),
                Some("inconsistent")
            ),
            "퇴사일은 입사일보다 이를 수 없습니다."
        );
        // 그 밖의 이유는 공통 문구.
        assert_eq!(
            error_message("validation_failed", Some("hired_on"), Some("empty")),
            "입사일을(를) 입력해 주세요."
        );
        assert_eq!(
            proxy_rejection_note("김관리"),
            "관리자 김관리님이 대리 반려"
        );
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
    fn every_attendance_enum_value_has_a_distinct_label() {
        let work: Vec<_> = WorkType::ALL.iter().copied().map(work_type_label).collect();
        let status: Vec<_> = DAY_STATUSES.into_iter().map(day_status_label).collect();
        let events: Vec<_> = AttendanceEventKind::ALL
            .iter()
            .map(|kind| attendance_event_label(*kind))
            .collect();
        for labels in [&work, &status, &events] {
            assert!(labels.iter().all(|label| !label.trim().is_empty()));
            let mut unique = labels.to_vec();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), labels.len(), "{labels:?}");
        }
        assert_eq!(work.len(), WorkType::ALL.len());
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn attendance_validation_messages_are_specific() {
        assert_eq!(
            error_message("validation_failed", Some("work_date"), Some("inconsistent")),
            "출근 시각이 선택한 근무일에 속하지 않습니다."
        );
        assert!(error_message(
            "validation_failed",
            Some("clock_out_at"),
            Some("out_of_range")
        )
        .contains("24시간"));
        assert_eq!(
            error_message("validation_failed", Some("note"), Some("too_long")),
            "메모이(가) 너무 깁니다."
        );
        assert!(
            error_message("validation_failed", Some("from"), Some("out_of_range"))
                .starts_with("기간")
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
        // 입력 상자 이름으로 문구를 만든다(도메인 필드 이름과 다를 수 있다).
        let error = DomainError::validation("text", ValidationReason::TooLong { max: 10 });
        assert_eq!(
            field_error_message("comment", &error),
            "의견이(가) 너무 깁니다."
        );
        assert_eq!(
            field_error_message("reason", &DomainError::Forbidden),
            "사유의 형식이 올바르지 않습니다."
        );
    }

    #[test]
    fn statuses_and_roles_have_distinct_labels() {
        let labels: std::collections::BTreeSet<_> = MemberStatus::ALL
            .iter()
            .map(|s| member_status_label(*s))
            .collect();
        assert_eq!(labels.len(), MemberStatus::ALL.len());
        let roles: std::collections::BTreeSet<_> =
            Role::ALL.iter().copied().map(role_label).collect();
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

    #[test]
    fn every_leave_enum_value_has_a_label() {
        for unit in LeaveUnit::ALL.iter().copied() {
            assert!(!leave_unit_label(unit).is_empty(), "{unit:?}");
            assert!(!leave_unit_short(unit).is_empty(), "{unit:?}");
        }
        for status in LeaveRequestStatus::ALL.iter().copied() {
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
    fn every_approval_value_has_a_label() {
        for status in DocumentStatus::ALL {
            assert!(!document_status_label(*status).is_empty());
            let _ = document_status_tone(*status);
        }
        for kind in StepKind::ALL {
            assert!(!step_kind_label(*kind).is_empty());
        }
        for status in StepStatus::ALL {
            assert!(!step_status_label(*status).is_empty());
            let _ = step_status_tone(*status);
        }
        for kind in DocumentKind::ALL {
            assert!(!document_kind_label(*kind).is_empty());
        }
        for mailbox in Mailbox::ALL {
            assert!(!mailbox_label(*mailbox).is_empty());
        }
        for kind in EventKind::ALL {
            assert!(!event_kind_label(*kind).is_empty());
        }
        for unit in LeaveUnit::ALL.iter().copied() {
            assert!(!leave_unit_label(unit).is_empty());
        }
    }

    #[test]
    fn every_correction_status_has_a_distinct_label_and_tone() {
        let labels: std::collections::BTreeSet<_> = CorrectionStatus::ALL
            .iter()
            .map(|s| correction_status_label(*s))
            .collect();
        assert_eq!(labels.len(), CorrectionStatus::ALL.len());
        assert!(labels.iter().all(|label| !label.is_empty()));
        let tones: Vec<_> = CorrectionStatus::ALL
            .iter()
            .map(|s| correction_status_tone(*s))
            .collect();
        for (i, a) in tones.iter().enumerate() {
            for b in &tones[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn correction_errors_explain_the_rules() {
        let line =
            correction_error_message("validation_failed", Some("steps"), Some("inconsistent"));
        assert!(line.contains("조직장 또는 관리자"), "{line}");
        assert!(correction_error_message(
            "validation_failed",
            Some("work_date"),
            Some("out_of_range")
        )
        .contains("31일"));
        for code in [
            "already_exists",
            "withdraw_too_late",
            "document_already_closed",
            "version_conflict",
        ] {
            assert_ne!(
                correction_error_message(code, None, None),
                error_message(code, None, None),
                "{code}"
            );
        }
        // 그 밖은 공통 문구를 쓴다.
        assert_eq!(
            correction_error_message("validation_failed", Some("steps"), Some("empty")),
            error_message("validation_failed", Some("steps"), Some("empty"))
        );
        assert_eq!(
            correction_error_message("forbidden", None, None),
            error_message("forbidden", None, None)
        );
    }

    #[test]
    fn correction_audit_codes_have_labels() {
        for code in [
            "attendance.correction_request",
            "attendance.correction_cancel",
            "attendance.correction",
            "leave.request",
        ] {
            assert_ne!(audit_action_label(code), code);
        }
        assert_eq!(
            audit_target_label("attendance_correction"),
            "근태 정정 요청"
        );
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

    #[test]
    fn approval_line_errors_are_specific() {
        let fallback = error_message("validation_failed", Some("steps"), Some("weird"));
        for reason in ["empty", "duplicate", "inconsistent"] {
            assert_ne!(
                error_message("validation_failed", Some("steps"), Some(reason)),
                fallback
            );
        }
    }

    #[test]
    fn status_tones_cover_every_status() {
        for status in DAY_STATUSES {
            let tone = day_status_tone(status);
            assert!(!day_status_label(status).is_empty());
            match status {
                DayStatus::Late | DayStatus::EarlyLeave | DayStatus::LateAndEarlyLeave => {
                    assert_eq!(tone, Tone::Warning)
                }
                DayStatus::Absent | DayStatus::MissingClockOut => {
                    assert_eq!(tone, Tone::Danger)
                }
                DayStatus::Normal => assert_eq!(tone, Tone::Success),
                _ => assert!(matches!(tone, Tone::Neutral | Tone::Info)),
            }
        }
    }

    #[test]
    fn every_status_has_a_distinct_tone_and_label() {
        let tones: Vec<_> = LeaveRequestStatus::ALL
            .iter()
            .map(|s| leave_status_tone(*s))
            .collect();
        for (i, a) in tones.iter().enumerate() {
            for b in &tones[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
