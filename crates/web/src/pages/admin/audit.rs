//! 감사 로그 조회.

use dioxus::prelude::*;
use pango_api::admin::AuditEntryDto;
use pango_api::org::MemberSummary;
use pango_api::Page;

use crate::api;
use crate::format;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    resource_view, use_more_pages, Button, ButtonKind, Card, EmptyState, FormError, IconName,
    LoadMore, MemberPicker, Meta, Modal, PageHeader, PickedMember, SelectField, TextField,
};

use logic::{actor_text, detail_full, detail_preview, query_pairs, target_parts, AuditFilters};

/// 상세 열에 보여 줄 최대 글자 수. 넘으면 "전체 보기"로 연다.
const PREVIEW_CHARS: usize = 80;

#[component]
pub fn AdminAudit() -> Element {
    let session = use_session();
    let tz = session.zone();
    let mut from = use_signal(String::new);
    let mut until = use_signal(String::new);
    let mut action = use_signal(String::new);
    let mut actor = use_signal(|| None::<MemberSummary>);
    let mut filter_error = use_signal(String::new);
    let mut query = use_signal(|| api::query_string(&logic::base_pairs()));
    let mut detail = use_signal(|| None::<String>);
    let more = use_more_pages::<AuditEntryDto>();
    let entries = use_resource(move || async move {
        let query = query();
        more.reset();
        api::get::<Page<AuditEntryDto>>(&format!("/admin/audit-log{query}")).await
    });

    let apply = move |event: FormEvent| {
        event.prevent_default();
        let filters = AuditFilters {
            from: from(),
            until: until(),
            action: action(),
            actor: actor().map(|member| member.id),
        };
        match query_pairs(&filters, tz) {
            Ok(pairs) => {
                filter_error.set(String::new());
                query.set(api::query_string(&pairs));
            }
            Err(message) => filter_error.set(message),
        }
    };

    let action_options: Vec<(String, String)> = std::iter::once((String::new(), "전체".to_owned()))
        .chain(
            text::AUDIT_ACTIONS
                .iter()
                .map(|(code, label)| ((*code).to_owned(), (*label).to_owned())),
        )
        .collect();

    rsx! {
        PageHeader { title: "감사 로그", description: "관리 작업과 로그인 기록이에요. 최신순으로 보여요." }
        Card {
            form { onsubmit: apply,
                FormError { message: filter_error() }
                div { class: "toolbar",
                    TextField {
                        label: "시작일",
                        kind: "date",
                        value: from(),
                        oninput: move |value| from.set(value),
                    }
                    TextField {
                        label: "종료일",
                        kind: "date",
                        value: until(),
                        hint: "종료일 당일까지 포함해요.",
                        oninput: move |value| until.set(value),
                    }
                    SelectField {
                        label: "동작",
                        value: action(),
                        options: action_options,
                        onchange: move |value| action.set(value),
                    }
                    if let Some(member) = actor() {
                        div { class: "field",
                            span { class: "field-label", "행위자" }
                            PickedMember {
                                name: member.name,
                                clear_label: "행위자 선택 해제",
                                onclear: move |_| actor.set(None),
                            }
                        }
                    } else {
                        MemberPicker { label: "행위자", onpick: move |member| actor.set(Some(member)) }
                    }
                    div { class: "toolbar-end",
                        Button { submit: true, "조회" }
                    }
                }
            }
        {resource_view(entries, move |page: Page<AuditEntryDto>| {
            let cursor = more.cursor(&page);
            let items: Vec<AuditEntryDto> = more.all(page.items, |item| item.id);
            rsx! {
                if items.is_empty() {
                    EmptyState { title: "조건에 맞는 기록이 없어요.", icon: IconName::List }
                } else {
                    Fragment {
                        div { class: "table-wrap",
                            table { class: "table table-stack",
                                thead {
                                    tr {
                                        th { scope: "col", "시각" }
                                        th { scope: "col", "행위자" }
                                        th { scope: "col", "동작" }
                                        th { scope: "col", "대상" }
                                        th { scope: "col", "상세" }
                                    }
                                }
                                tbody {
                                    for entry in items {
                                        AuditRow {
                                            key: "{entry.id}",
                                            when: format::datetime(entry.at, tz),
                                            entry,
                                            onopen: move |full| detail.set(Some(full)),
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(cursor) = cursor {
                            LoadMore {
                                busy: more.busy(),
                                onclick: move |_| {
                                    let query = query();
                                    more.load(
                                        cursor.clone(),
                                        move |c| api::with_cursor(format!("/admin/audit-log{query}"), Some(c)),
                                    )
                                },
                            }
                        }
                    }
                }
            }
        })}
        }
        if let Some(full) = detail() {
            Modal { title: "상세 내용", onclose: move |_| detail.set(None),
                pre { class: "code-block", code { "{full}" } }
            }
        }
    }
}

#[component]
fn AuditRow(entry: AuditEntryDto, when: String, onopen: EventHandler<String>) -> Element {
    let (preview, truncated) = detail_preview(entry.detail.as_ref(), PREVIEW_CHARS);
    let full = entry.detail.as_ref().map(detail_full).unwrap_or_default();
    rsx! {
        tr {
            td { "data-label": "시각", class: "nowrap", "{when}" }
            td { "data-label": "행위자", {actor_text(&entry)} }
            td { "data-label": "동작", title: "{entry.action}", {text::audit_action_label(&entry.action)} }
            td { "data-label": "대상",
                Meta { items: target_parts(&entry) }
            }
            td { "data-label": "상세",
                code { class: "audit-detail", "{preview}" }
                if truncated {
                    Button {
                        kind: ButtonKind::Grey,
                        small: true,
                        onclick: move |_| onopen.call(full.clone()),
                        "전체 보기"
                    }
                }
            }
        }
    }
}

pub mod logic {
    use chrono::NaiveDate;
    use chrono_tz::Tz;
    use pango_api::admin::AuditEntryDto;
    use pango_core::id::MemberId;
    use pango_core::time::{MinuteOfDay, Timestamp, WorkCalendarZone};
    use serde_json::Value;

    use crate::text;

    const PAGE_SIZE: u32 = 50;

    /// 화면의 조회 조건. 날짜는 입력 상자 값(`YYYY-MM-DD`) 그대로.
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct AuditFilters {
        pub from: String,
        pub until: String,
        pub action: String,
        pub actor: Option<MemberId>,
    }

    pub fn base_pairs() -> Vec<(&'static str, String)> {
        vec![("limit", PAGE_SIZE.to_string())]
    }

    fn day_start(date: NaiveDate, tz: Tz) -> Option<Timestamp> {
        WorkCalendarZone::new(tz, MinuteOfDay::MIDNIGHT)
            .ok()
            .map(|zone| zone.instant_at(date, MinuteOfDay::MIDNIGHT))
    }

    fn date_of(raw: &str, label: &str) -> Result<Option<NaiveDate>, String> {
        crate::format::optional_date("date", raw)
            .map_err(|_| format!("{label}을 YYYY-MM-DD 형식으로 입력해 주세요."))
    }

    /// 조회 조건을 API 쿼리(`키, 값`)로 바꾼다. 값은 아직 인코딩하지 않는다.
    ///
    /// 날짜는 회사 시간대의 하루로 본다: 시작일 00:00부터 종료일 **다음 날** 00:00 전까지
    /// (API의 `until`은 그 시각을 포함하지 않는다).
    pub fn query_pairs(
        filters: &AuditFilters,
        tz: Tz,
    ) -> Result<Vec<(&'static str, String)>, String> {
        let from = date_of(&filters.from, "시작일")?;
        let until = date_of(&filters.until, "종료일")?;
        if let (Some(from), Some(until)) = (from, until) {
            if until < from {
                return Err("종료일이 시작일보다 빨라요.".to_owned());
            }
        }
        let mut pairs = base_pairs();
        if let Some(from) = from {
            let at = day_start(from, tz).ok_or("시작일을 확인해 주세요.")?;
            pairs.push(("from", at.as_millis().to_string()));
        }
        if let Some(until) = until {
            let at = until
                .succ_opt()
                .and_then(|next| day_start(next, tz))
                .ok_or("종료일을 확인해 주세요.")?;
            pairs.push(("until", at.as_millis().to_string()));
        }
        let action = filters.action.trim();
        if !action.is_empty() {
            pairs.push(("action", action.to_owned()));
        }
        if let Some(actor) = filters.actor {
            pairs.push(("actor_id", actor.to_string()));
        }
        Ok(pairs)
    }

    /// 상세 JSON을 한 줄로 줄여 보여 줄 문구와, 잘렸는지.
    pub fn detail_preview(detail: Option<&Value>, max_chars: usize) -> (String, bool) {
        let Some(detail) = detail else {
            return ("-".to_owned(), false);
        };
        let compact = detail.to_string();
        if compact.chars().count() <= max_chars {
            return (compact, false);
        }
        let mut short: String = compact.chars().take(max_chars).collect();
        short.push('…');
        (short, true)
    }

    /// "전체 보기"에 쓰는 들여쓴 JSON.
    pub fn detail_full(detail: &Value) -> String {
        serde_json::to_string_pretty(detail).unwrap_or_else(|_| detail.to_string())
    }

    pub fn actor_text(entry: &AuditEntryDto) -> String {
        match (&entry.actor_name, entry.actor_id) {
            (Some(name), _) => name.clone(),
            (None, Some(_)) => "알 수 없는 구성원".to_owned(),
            (None, None) => "시스템".to_owned(),
        }
    }

    /// 대상의 종류와 (있으면) 이름. 내부 ID는 사람이 읽을 정보가 아니므로 보여 주지 않는다
    /// (필요하면 상세 보기의 원본에 있다). 화면은 둘을 간격을 둔 별도 요소로 그린다.
    pub fn target_parts(entry: &AuditEntryDto) -> Vec<String> {
        std::iter::once(text::audit_target_label(&entry.target_type))
            .chain(entry.target_name.clone())
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pango_core::id::AuditLogId;
        use serde_json::json;

        fn seoul() -> Tz {
            "Asia/Seoul".parse().expect("tz")
        }

        fn millis(rfc: &str) -> String {
            chrono::DateTime::parse_from_rfc3339(rfc)
                .expect("rfc3339")
                .timestamp_millis()
                .to_string()
        }

        #[test]
        fn dates_become_company_zone_day_bounds() {
            let filters = AuditFilters {
                from: "2026-03-02".into(),
                until: "2026-03-02".into(),
                ..AuditFilters::default()
            };
            assert_eq!(
                query_pairs(&filters, seoul()),
                Ok(vec![
                    ("limit", "50".into()),
                    ("from", millis("2026-03-01T15:00:00Z")),
                    ("until", millis("2026-03-02T15:00:00Z")),
                ])
            );
        }

        #[test]
        fn optional_filters_are_added_only_when_set() {
            assert_eq!(
                query_pairs(&AuditFilters::default(), seoul()),
                Ok(base_pairs())
            );
            let actor = MemberId::new();
            let filters = AuditFilters {
                action: "  notice.create ".into(),
                actor: Some(actor),
                ..AuditFilters::default()
            };
            assert_eq!(
                query_pairs(&filters, Tz::UTC),
                Ok(vec![
                    ("limit", "50".into()),
                    ("action", "notice.create".into()),
                    ("actor_id", actor.to_string()),
                ])
            );
        }

        #[test]
        fn bad_ranges_are_rejected_before_sending() {
            let reversed = AuditFilters {
                from: "2026-03-03".into(),
                until: "2026-03-02".into(),
                ..AuditFilters::default()
            };
            assert_eq!(
                query_pairs(&reversed, seoul()),
                Err("종료일이 시작일보다 빨라요.".into())
            );
            for bad in ["2026-3-2", "2026-02-30", "x", "1' OR '1'='1"] {
                let filters = AuditFilters {
                    from: bad.into(),
                    ..AuditFilters::default()
                };
                assert!(query_pairs(&filters, seoul()).is_err(), "{bad}");
            }
        }

        fn entry(detail: Option<Value>) -> AuditEntryDto {
            AuditEntryDto {
                id: AuditLogId::new(),
                at: Timestamp::from_millis(0).expect("ts"),
                actor_id: None,
                actor_name: None,
                action: "notice.create".into(),
                target_type: "notice".into(),
                target_id: Some("abc".into()),
                target_name: None,
                detail,
                ip: None,
            }
        }

        #[test]
        fn detail_is_compact_and_truncated_by_chars() {
            assert_eq!(detail_preview(None, 10), ("-".into(), false));
            let small = json!({ "a": 1 });
            assert_eq!(
                detail_preview(Some(&small), 80),
                ("{\"a\":1}".into(), false)
            );
            let long = json!({ "name": "가".repeat(100) });
            let (preview, truncated) = detail_preview(Some(&long), 20);
            assert!(truncated);
            assert_eq!(preview.chars().count(), 21);
            assert!(preview.ends_with('…'));
            let html = json!({ "title": "<script>alert(1)</script>" });
            assert!(
                detail_full(&html).contains("<script>"),
                "글자 그대로 둔다(텍스트로 그린다)"
            );
            assert!(detail_full(&small).contains('\n'));
        }

        #[test]
        fn actor_and_target_text() {
            let mut e = entry(None);
            assert_eq!(actor_text(&e), "시스템");
            e.actor_id = Some(MemberId::new());
            assert_eq!(actor_text(&e), "알 수 없는 구성원");
            e.actor_name = Some("홍길동".into());
            assert_eq!(actor_text(&e), "홍길동");
            assert_eq!(target_parts(&e), vec!["공지"], "내부 ID는 보여 주지 않는다");
            e.target_name = Some("워크숍 안내".into());
            assert_eq!(target_parts(&e), vec!["공지", "워크숍 안내"]);
            e.target_name = None;
            e.target_id = None;
            e.target_type = "custom".into();
            assert_eq!(target_parts(&e), vec!["custom"]);
        }
    }
}
