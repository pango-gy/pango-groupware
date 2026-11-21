//! 공지 관리: 작성·수정·보관과 읽음 현황.

use dioxus::prelude::*;
use pango_api::notice::{NoticeAttachment, NoticeDetail, NoticeReadReport, NoticeSummary};
use pango_api::org::DepartmentDto;
use pango_api::{Page, Versioned};
use pango_core::authz::AudienceKind;
use pango_core::id::{NoticeAttachmentId, NoticeId};
use pango_core::text::{Body, Title};

use crate::api;
use crate::badges::refresh_badges;
use crate::format;
use crate::org_tree;
use crate::pages::notices::NoticeFlags;
use crate::session::use_session;
use crate::ui::{
    loaded_or_empty, report_error, report_success, resource_view, use_more_pages, Badge, Button,
    ButtonKind, Card, Checkbox, ConfirmModal, EmptyState, FormError, Icon, IconName, LoadMore,
    Meta, Modal, PageHeader, SelectField, TextArea, TextField, Tone,
};
use pango_core::attachment::MAX_PER_NOTICE;

use logic::{
    build_input, check_pick, count_label, upload_error, NoticeForm, PendingFile, LIST_PATH,
};

#[derive(Debug, Clone, PartialEq)]
enum Dialog {
    Closed,
    Create,
    Edit(NoticeSummary),
    Archive(NoticeSummary),
    Reads(NoticeSummary),
}

#[component]
pub fn AdminNotices() -> Element {
    let session = use_session();
    let mut dialog = use_signal(|| Dialog::Closed);
    let mut archiving = use_signal(|| false);
    let more = use_more_pages::<NoticeSummary>();
    let mut list = use_resource(move || async move {
        more.reset();
        api::get::<Page<NoticeSummary>>(LIST_PATH).await
    });
    let departments =
        use_resource(|| async { api::get::<Vec<DepartmentDto>>("/departments").await });
    let tz = session.zone();

    // 대상 부서 선택지. 조직도와 같은 순서·들여쓰기로 보인다.
    let department_options =
        org_tree::department_options(&loaded_or_empty(&departments), "부서 선택");

    let saved = move |_| {
        dialog.set(Dialog::Closed);
        list.restart();
        refresh_badges();
    };
    let archive = move |notice: NoticeSummary| async move {
        if archiving() {
            return;
        }
        archiving.set(true);
        let path = format!("/admin/notices/{}?version={}", notice.id, notice.version);
        match api::delete::<NoticeDetail>(&path).await {
            Ok(_) => {
                report_success("공지를 보관했어요.");
                dialog.set(Dialog::Closed);
                list.restart();
                refresh_badges();
            }
            Err(error) => {
                report_error(&error);
                if error.code == "version_conflict" {
                    dialog.set(Dialog::Closed);
                    list.restart();
                }
            }
        }
        archiving.set(false);
    };

    rsx! {
        PageHeader {
            title: "공지 관리",
            description: "공지를 게시하고 누가 읽었는지 확인해요.",
            actions: rsx! {
                Button { onclick: move |_| dialog.set(Dialog::Create), "새 공지" }
            },
        }
        {resource_view(list, move |page: Page<NoticeSummary>| {
            let cursor = more.cursor(&page);
            let items: Vec<NoticeSummary> = more.all(page.items, |item| item.id);
            rsx! {
                if items.is_empty() {
                    EmptyState { title: "작성한 공지가 없어요.", icon: IconName::Megaphone }
                } else {
                    Card {
                        ul { class: "list",
                            for notice in items {
                                li { key: "{notice.id}", class: "list-row list-row-wrap",
                                    div { class: "list-row-main",
                                        span { class: "row-title",
                                            if notice.archived_at.is_some() {
                                                Badge { tone: Tone::Neutral, "보관됨" }
                                            }
                                            NoticeFlags { pinned: notice.pinned, must_read: notice.must_read }
                                            strong { "{notice.title}" }
                                        }
                                        Meta {
                                            items: vec![
                                                notice.author_name.clone(),
                                                format::datetime(notice.published_at, tz),
                                                crate::pages::notices::logic::audience_text(&notice).to_string(),
                                            ],
                                        }
                                    }
                                    div { class: "row-actions",
                                        Button {
                                            kind: ButtonKind::Grey,
                                            small: true,
                                            onclick: {
                                                let notice = notice.clone();
                                                move |_| dialog.set(Dialog::Reads(notice.clone()))
                                            },
                                            "읽음 현황"
                                        }
                                        if notice.archived_at.is_none() {
                                            Button {
                                                kind: ButtonKind::Grey,
                                                small: true,
                                                onclick: {
                                                    let notice = notice.clone();
                                                    move |_| dialog.set(Dialog::Edit(notice.clone()))
                                                },
                                                "수정"
                                            }
                                            Button {
                                                kind: ButtonKind::Grey,
                                                small: true,
                                                onclick: {
                                                    let notice = notice.clone();
                                                    move |_| dialog.set(Dialog::Archive(notice.clone()))
                                                },
                                                "보관"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(cursor) = cursor {
                            LoadMore {
                                busy: more.busy(),
                                onclick: move |_| {
                                    more.load(cursor.clone(), |c| api::with_cursor(LIST_PATH.to_owned(), Some(c)))
                                },
                            }
                        }
                    }
                }
            }
        })}
        match dialog() {
            Dialog::Closed => rsx! {},
            Dialog::Create => rsx! {
                Modal { title: "새 공지", onclose: move |_| dialog.set(Dialog::Closed),
                    NoticeEditor {
                        initial: NoticeForm::default(),
                        target: None,
                        files: Vec::new(),
                        departments: department_options,
                        onclose: move |_| dialog.set(Dialog::Closed),
                        onsaved: saved,
                    }
                }
            },
            Dialog::Edit(notice) => rsx! {
                Modal { title: "공지 수정", onclose: move |_| dialog.set(Dialog::Closed),
                    EditLoader {
                        id: notice.id,
                        departments: department_options,
                        onclose: move |_| dialog.set(Dialog::Closed),
                        onsaved: saved,
                    }
                }
            },
            Dialog::Archive(notice) => rsx! {
                ConfirmModal {
                    title: "공지 보관",
                    message: format!(
                        "‘{}’ 공지를 보관해요. 구성원 목록에서 사라지고 다시 게시할 수 없어요.",
                        notice.title
                    ),
                    confirm_label: "보관",
                    danger: true,
                    busy: archiving(),
                    onconfirm: move |_| archive(notice.clone()),
                    oncancel: move |_| dialog.set(Dialog::Closed),
                }
            },
            Dialog::Reads(notice) => rsx! {
                Modal { title: "읽음 현황", onclose: move |_| dialog.set(Dialog::Closed),
                    ReadReport { id: notice.id, title: notice.title.clone() }
                }
            },
        }
    }
}

/// 수정할 공지의 본문을 읽어 와 편집기를 연다(목록에는 본문이 없다).
#[component]
fn EditLoader(
    id: NoticeId,
    departments: Vec<(String, String)>,
    onclose: EventHandler<()>,
    onsaved: EventHandler<()>,
) -> Element {
    let detail =
        use_resource(
            move || async move { api::get::<NoticeDetail>(&format!("/notices/{id}")).await },
        );
    resource_view(detail, move |detail: NoticeDetail| {
        rsx! {
            NoticeEditor {
                key: "{detail.summary.version}",
                initial: NoticeForm::from_detail(&detail),
                target: Some((detail.summary.id, detail.summary.version)),
                files: detail.attachments.clone(),
                departments,
                onclose,
                onsaved,
            }
        }
    })
}

#[component]
fn NoticeEditor(
    initial: NoticeForm,
    /// 수정이면 `(ID, 읽어 온 버전)`, 새 공지면 `None`.
    target: Option<(NoticeId, i64)>,
    /// 이미 올린 첨부파일(수정일 때).
    files: Vec<NoticeAttachment>,
    departments: Vec<(String, String)>,
    onclose: EventHandler<()>,
    onsaved: EventHandler<()>,
) -> Element {
    let mut form = use_signal(|| initial.clone());
    let uploaded = use_signal(|| files.clone());
    // 새 공지에 고른 파일. 게시가 끝난 뒤 차례로 올린다.
    let mut pending = use_signal(Vec::<PendingFile>::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    // 편집기를 열 때 한 번 만든다. 두 번 눌러도 공지는 하나만 생긴다.
    let key = use_signal(api::new_idempotency_key);

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let input = match build_input(&form.read()) {
            Ok(input) => input,
            Err(message) => {
                error.set(message);
                return;
            }
        };
        busy.set(true);
        error.set(String::new());
        let result = match target {
            Some((id, version)) => {
                let request = Versioned {
                    version,
                    data: input,
                };
                api::patch::<_, NoticeDetail>(&format!("/admin/notices/{id}"), &request).await
            }
            None => api::post_once::<_, NoticeDetail>("/admin/notices", &input, &key()).await,
        };
        let result = match result {
            Ok(created) if target.is_none() => {
                let files = pending.read().clone();
                let (left, problems) = upload_pending(created.summary.id, files).await;
                pending.set(left);
                Ok(problems)
            }
            Ok(_) => Ok(Vec::new()),
            Err(failure) => Err(failure),
        };
        busy.set(false);
        match result {
            Ok(problems) if !problems.is_empty() => {
                // 공지는 이미 게시됐다. 창을 열어 두고, 다시 "게시"를 누르면 같은 멱등성 키로
                // 같은 공지를 돌려받아 남은 파일만 다시 올린다.
                error.set(format!(
                    "공지는 게시했지만 파일 {}개를 올리지 못했어요. 다시 게시를 누르면 남은 파일만 올려요.\n{}",
                    problems.len(),
                    problems.join("\n")
                ));
            }
            Ok(_) => {
                report_success(if target.is_some() {
                    "공지를 수정했어요."
                } else {
                    "공지를 게시했어요."
                });
                onsaved.call(());
            }
            Err(failure) if failure.code == "version_conflict" => {
                // 남이 먼저 고쳤다. 안내하고 목록을 다시 불러온다.
                report_error(&failure);
                onsaved.call(());
            }
            Err(failure) => error.set(failure.message()),
        }
    };

    let current = form.read().clone();
    let audience_options = vec![
        (
            AudienceKind::All.as_str().to_owned(),
            "전체 구성원".to_owned(),
        ),
        (
            AudienceKind::Department.as_str().to_owned(),
            "특정 부서".to_owned(),
        ),
    ];
    let (body_count, body_over) = count_label(&current.body, Body::RULE.max_chars);

    rsx! {
        form { class: "form", onsubmit: submit,
            FormError { message: error() }
            TextField {
                label: "제목",
                value: current.title.clone(),
                required: true,
                maxlength: Title::RULE.max_chars as i64,
                oninput: move |value| form.write().title = value,
            }
            div { class: "field-with-count",
                TextArea {
                    label: "내용",
                    value: current.body.clone(),
                    rows: 10,
                    required: true,
                    maxlength: Body::RULE.max_chars as i64,
                    oninput: move |value| form.write().body = value,
                }
                span {
                    class: if body_over { "char-count char-count-over" } else { "char-count" },
                    "aria-live": "polite",
                    "{body_count}"
                }
            }
            div { class: "form-row",
                SelectField {
                    label: "대상",
                    value: current.audience.as_str(),
                    options: audience_options,
                    onchange: move |value: String| {
                        if let Ok(kind) = AudienceKind::parse(&value) {
                            form.write().audience = kind;
                        }
                    },
                }
                if current.audience == AudienceKind::Department {
                    SelectField {
                        label: "대상 부서",
                        value: current.department.clone(),
                        options: departments,
                        required: true,
                        onchange: move |value| form.write().department = value,
                    }
                }
            }
            if current.audience == AudienceKind::Department {
                p { class: "field-hint", "하위 부서 구성원도 함께 받아요." }
            }
            Checkbox {
                label: "목록 위에 고정",
                checked: current.pinned,
                onchange: move |value| form.write().pinned = value,
            }
            Checkbox {
                label: "필독",
                hint: "구성원에게 읽음 확인을 받아요.",
                checked: current.must_read,
                onchange: move |value| form.write().must_read = value,
            }
            AttachmentEditor { notice: target.map(|(id, _)| id), uploaded, pending }
            div { class: "modal-actions",
                Button { kind: ButtonKind::Grey, onclick: move |_| onclose.call(()), "취소" }
                Button { submit: true, busy: busy(),
                    if target.is_some() { "저장" } else { "게시" }
                }
            }
        }
    }
}

/// 새 공지에 고른 파일을 차례로 올린다. `(올리지 못한 파일, 그 안내 문구)`를 돌려준다.
async fn upload_pending(id: NoticeId, files: Vec<PendingFile>) -> (Vec<PendingFile>, Vec<String>) {
    let mut left = Vec::new();
    let mut problems = Vec::new();
    for file in files {
        if let Err(error) = upload_file(id, &file).await {
            problems.push(upload_error(&file.name, &error));
            left.push(file);
        }
    }
    (left, problems)
}

async fn upload_file(id: NoticeId, file: &PendingFile) -> api::ApiResult<NoticeAttachment> {
    api::upload(
        &format!("/admin/notices/{id}/attachments"),
        &file.name,
        file.bytes.clone(),
        &file.key,
    )
    .await
}

/// 첨부파일 고르기·지우기.
///
/// 수정 중인 공지(`notice`가 있음)는 고르는 즉시 올리고, 지우기는 한 번 더 확인한 뒤 바로 지운다.
/// 둘 다 "저장"과 무관하게 반영된다. 새 공지는 `pending`에 모아 두었다가 게시한 뒤 올린다.
#[component]
fn AttachmentEditor(
    notice: Option<NoticeId>,
    uploaded: Signal<Vec<NoticeAttachment>>,
    pending: Signal<Vec<PendingFile>>,
) -> Element {
    let mut error = use_signal(String::new);
    let mut working = use_signal(|| false);
    let mut confirming = use_signal(|| None::<NoticeAttachmentId>);
    // 같은 파일을 다시 고를 수 있도록 고를 때마다 입력 요소를 새로 만든다.
    let mut picker = use_signal(|| 0_u32);

    let pick = move |event: FormEvent| async move {
        let chosen = event.files();
        picker += 1;
        if chosen.is_empty() || working() {
            return;
        }
        working.set(true);
        let mut problems = Vec::new();
        for file in chosen {
            let name = file.name();
            let count = uploaded.read().len() + pending.read().len();
            if let Err(message) = check_pick(&name, file.size(), count) {
                problems.push(message);
                continue;
            }
            let bytes = match file.read_bytes().await {
                Ok(bytes) => bytes.to_vec(),
                Err(_) => {
                    problems.push(format!("‘{name}’ 파일을 읽지 못했어요."));
                    continue;
                }
            };
            let item = PendingFile {
                key: api::new_idempotency_key(),
                name,
                bytes,
            };
            match notice {
                Some(id) => match upload_file(id, &item).await {
                    Ok(created) => uploaded.write().push(created),
                    Err(failure) => problems.push(upload_error(&item.name, &failure)),
                },
                None => pending.write().push(item),
            }
        }
        error.set(problems.join("\n"));
        working.set(false);
    };

    let remove = move |id: NoticeAttachmentId| async move {
        let Some(notice) = notice else {
            return;
        };
        if working() {
            return;
        }
        working.set(true);
        let path = format!("/admin/notices/{notice}/attachments/{id}");
        match api::delete::<()>(&path).await {
            // 이미 지워졌으면(404) 목록에서도 뺀다.
            Ok(()) | Err(api::ApiError { status: 404, .. }) => {
                uploaded.write().retain(|file| file.id != id);
                error.set(String::new());
            }
            Err(failure) => error.set(failure.message()),
        }
        confirming.set(None);
        working.set(false);
    };

    let saved_files = uploaded.read().clone();
    let picked_files = pending.read().clone();
    let total = saved_files.len() + picked_files.len();

    rsx! {
        div { class: "field attachments",
            span { class: "field-label", "첨부파일 {total}개" }
            if notice.is_some() {
                p { class: "field-hint", "파일은 고르면 바로 올라가고, 지우면 바로 지워져요." }
            }
            FormError { message: error() }
            if total > 0 {
                ul { class: "attachment-list",
                    for file in saved_files {
                        li { key: "{file.id}", class: "attachment-row",
                            Icon { name: IconName::Document }
                            span { class: "attachment-name", "{file.filename}" }
                            small { class: "attachment-size", "{format::file_size(file.size)}" }
                            if confirming() == Some(file.id) {
                                Button {
                                    kind: ButtonKind::Danger,
                                    small: true,
                                    busy: working(),
                                    onclick: move |_| remove(file.id),
                                    "삭제"
                                }
                                Button {
                                    kind: ButtonKind::Grey,
                                    small: true,
                                    onclick: move |_| confirming.set(None),
                                    "취소"
                                }
                            } else {
                                Button {
                                    kind: ButtonKind::Grey,
                                    small: true,
                                    disabled: working(),
                                    onclick: move |_| confirming.set(Some(file.id)),
                                    "지우기"
                                }
                            }
                        }
                    }
                    for (index, file) in picked_files.into_iter().enumerate() {
                        li { key: "{file.key}", class: "attachment-row",
                            Icon { name: IconName::Document }
                            span { class: "attachment-name", "{file.name}" }
                            small { class: "attachment-size", "{format::file_size(file.bytes.len() as u64)}" }
                            Button {
                                kind: ButtonKind::Grey,
                                small: true,
                                disabled: working(),
                                onclick: move |_| {
                                    pending.write().remove(index);
                                },
                                "빼기"
                            }
                        }
                    }
                }
            }
            if total < MAX_PER_NOTICE {
                label {
                    key: "{picker}",
                    class: "btn btn-grey btn-sm attachment-pick",
                    "aria-busy": working(),
                    if working() {
                        span { class: "spinner", "aria-hidden": "true" }
                    }
                    Icon { name: IconName::Plus }
                    "파일 추가"
                    input {
                        r#type: "file",
                        multiple: true,
                        disabled: working(),
                        "aria-label": "첨부파일 추가",
                        onchange: pick,
                    }
                }
            } else {
                p { class: "field-hint", "첨부파일은 {MAX_PER_NOTICE}개까지 올릴 수 있어요." }
            }
        }
    }
}

#[component]
fn ReadReport(id: NoticeId, title: String) -> Element {
    let session = use_session();
    let report = use_resource(move || async move {
        api::get::<NoticeReadReport>(&format!("/admin/notices/{id}/reads")).await
    });
    let tz = session.zone();
    rsx! {
        p { class: "sub-title wrap-anywhere", "{title}" }
        {resource_view(report, move |report: NoticeReadReport| rsx! {
            section { class: "read-report",
                h3 { class: "sub-title", "읽음 {report.read.len()}명" }
                if report.read.is_empty() {
                    p { class: "field-hint", "아직 읽은 사람이 없어요." }
                }
                ul { class: "list",
                    for reader in report.read {
                        li { key: "{reader.member_id}", class: "list-row",
                            span { class: "list-row-main", strong { "{reader.name}" } }
                            span { class: "list-row-side",
                                {reader.read_at.map(|at| format::datetime(at, tz)).unwrap_or_default()}
                            }
                        }
                    }
                }
            }
            section { class: "read-report",
                h3 { class: "sub-title", "안 읽음 {report.unread.len()}명" }
                if report.unread.is_empty() {
                    p { class: "field-hint", "대상자가 모두 읽었어요." }
                }
                ul { class: "list",
                    for reader in report.unread {
                        li { key: "{reader.member_id}", class: "list-row",
                            span { class: "list-row-main", strong { "{reader.name}" } }
                        }
                    }
                }
            }
        })}
    }
}

pub mod logic {
    use pango_api::notice::{NoticeDetail, NoticeInput};
    use pango_core::attachment::{FileName, MAX_PER_NOTICE};
    use pango_core::authz::{Audience, AudienceKind};
    use pango_core::id::DepartmentId;
    use pango_core::text::{Body, Title};

    use crate::api::ApiError;
    use crate::text;

    pub const LIST_PATH: &str = "/admin/notices?limit=30";

    /// 올리기 전의 파일. `key`는 고를 때 만든 멱등성 키라 다시 보내도 한 번만 올라간다.
    #[derive(Debug, Clone, PartialEq)]
    pub struct PendingFile {
        pub key: String,
        pub name: String,
        pub bytes: Vec<u8>,
    }

    /// 고른 파일을 올리기 전에 서버와 같은 규칙(이름, 빈 파일, 개수)을 확인한다.
    /// 크기 상한은 서버 설정이라 서버의 응답으로 안내한다.
    pub fn check_pick(name: &str, size: u64, current: usize) -> Result<(), String> {
        if current >= MAX_PER_NOTICE {
            return Err(format!("첨부파일은 {MAX_PER_NOTICE}개까지 올릴 수 있어요."));
        }
        if FileName::parse(name).is_err() {
            return Err(format!("‘{name}’은(는) 쓸 수 없는 파일 이름이에요."));
        }
        if size == 0 {
            return Err(format!("‘{name}’ 파일이 비어 있어요."));
        }
        Ok(())
    }

    /// 올리기 실패 안내.
    pub fn upload_error(name: &str, error: &ApiError) -> String {
        match error.code.as_str() {
            "payload_too_large" => format!("‘{name}’ 파일이 허용 크기보다 커요."),
            _ => format!("‘{name}’: {}", error.message()),
        }
    }

    /// 편집 중인 공지. 대상 부서는 선택 상자의 값(문자열) 그대로 둔다.
    #[derive(Debug, Clone, PartialEq)]
    pub struct NoticeForm {
        pub title: String,
        pub body: String,
        pub pinned: bool,
        pub must_read: bool,
        pub audience: AudienceKind,
        pub department: String,
    }

    impl Default for NoticeForm {
        fn default() -> Self {
            Self {
                title: String::new(),
                body: String::new(),
                pinned: false,
                must_read: false,
                audience: AudienceKind::All,
                department: String::new(),
            }
        }
    }

    impl NoticeForm {
        pub fn from_detail(detail: &NoticeDetail) -> Self {
            Self {
                title: detail.summary.title.clone(),
                body: detail.body.clone(),
                pinned: detail.summary.pinned,
                must_read: detail.summary.must_read,
                audience: detail.summary.audience,
                department: detail
                    .summary
                    .department_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            }
        }
    }

    /// 폼을 요청으로 바꾼다. 서버와 같은 규칙(제목·본문 길이, 대상과 부서의 짝)을 먼저 확인한다.
    ///
    /// 대상이 전체면 골라 둔 부서가 있어도 보내지 않고, 부서 대상이면 부서가 반드시 있어야 한다.
    pub fn build_input(form: &NoticeForm) -> Result<NoticeInput, String> {
        let title = Title::parse(&form.title).map_err(|e| text::domain_error_message(&e))?;
        let body = Body::parse(&form.body).map_err(|e| text::domain_error_message(&e))?;
        let department_id = match form.audience {
            AudienceKind::All => None,
            AudienceKind::Department => match form.department.trim() {
                "" => None,
                raw => Some(DepartmentId::parse(raw).map_err(|_| {
                    text::error_message("validation_failed", Some("department_id"), Some("empty"))
                })?),
            },
        };
        let audience = Audience::new(form.audience, department_id)
            .map_err(|e| text::domain_error_message(&e))?;
        Ok(NoticeInput {
            title: title.into_string(),
            body: body.into_string(),
            pinned: form.pinned,
            must_read: form.must_read,
            audience: audience.kind(),
            department_id: audience.department_id(),
        })
    }

    /// 글자 수 표시(`12 / 20000자`)와 한도를 넘었는지. 서버처럼 문자(코드 포인트) 단위로 센다.
    pub fn count_label(value: &str, max: usize) -> (String, bool) {
        let count = value.chars().count();
        (format!("{count} / {max}자"), count > max)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn filled() -> NoticeForm {
            NoticeForm {
                title: "  사내 공지  ".into(),
                body: "첫 줄\r\n둘째 줄".into(),
                ..NoticeForm::default()
            }
        }

        #[test]
        fn all_audience_never_sends_a_department() {
            let form = NoticeForm {
                department: DepartmentId::new().to_string(),
                ..filled()
            };
            let input = build_input(&form).expect("valid");
            assert_eq!(input.audience, AudienceKind::All);
            assert_eq!(input.department_id, None);
            assert_eq!(input.title, "사내 공지", "서버처럼 앞뒤 공백을 지운다");
            assert_eq!(input.body, "첫 줄\n둘째 줄");
        }

        #[test]
        fn department_audience_requires_a_valid_department() {
            let id = DepartmentId::new();
            let form = NoticeForm {
                audience: AudienceKind::Department,
                department: id.to_string(),
                ..filled()
            };
            let input = build_input(&form).expect("valid");
            assert_eq!(input.audience, AudienceKind::Department);
            assert_eq!(input.department_id, Some(id));

            for department in ["", "   ", "not-an-id", "'; DROP TABLE notices; --"] {
                let form = NoticeForm {
                    audience: AudienceKind::Department,
                    department: department.into(),
                    ..filled()
                };
                assert_eq!(
                    build_input(&form),
                    Err("대상 부서를 선택해 주세요.".into()),
                    "{department:?}"
                );
            }
        }

        #[test]
        fn title_and_body_follow_server_rules() {
            let empty_title = NoticeForm {
                title: "   ".into(),
                ..filled()
            };
            assert_eq!(
                build_input(&empty_title),
                Err("제목을(를) 입력해 주세요.".into())
            );
            let long_title = NoticeForm {
                title: "가".repeat(Title::RULE.max_chars + 1),
                ..filled()
            };
            assert!(build_input(&long_title).is_err());
            let exact_title = NoticeForm {
                title: "가".repeat(Title::RULE.max_chars),
                ..filled()
            };
            assert!(build_input(&exact_title).is_ok());
            let control = NoticeForm {
                title: "제목\u{0007}".into(),
                ..filled()
            };
            assert!(build_input(&control).is_err());
            let long_body = NoticeForm {
                body: "a".repeat(Body::RULE.max_chars + 1),
                ..filled()
            };
            assert_eq!(
                build_input(&long_body),
                Err("내용이(가) 너무 깁니다.".into())
            );
            let html = NoticeForm {
                body: "<img src=x onerror=alert(1)>".into(),
                ..filled()
            };
            assert_eq!(
                build_input(&html).expect("text is allowed").body,
                "<img src=x onerror=alert(1)>",
                "HTML은 글자 그대로 보낸다(화면은 텍스트로만 그린다)"
            );
        }

        #[test]
        fn picked_files_follow_server_rules() {
            assert_eq!(check_pick("회의록.pdf", 10, 0), Ok(()));
            assert!(check_pick("회의록.pdf", 0, 0).is_err(), "빈 파일");
            assert!(check_pick("../a.txt", 10, 0).is_err(), "경로");
            assert!(check_pick("a.txt", 10, MAX_PER_NOTICE).is_err(), "개수");
            assert!(check_pick("a.txt", 10, MAX_PER_NOTICE - 1).is_ok());
        }

        #[test]
        fn upload_errors_name_the_file() {
            let error = |code: &str| ApiError {
                status: 413,
                code: code.into(),
                field: None,
                reason: None,
                fields: Vec::new(),
            };
            assert_eq!(
                upload_error("큰파일.zip", &error("payload_too_large")),
                "‘큰파일.zip’ 파일이 허용 크기보다 커요."
            );
            assert_eq!(
                upload_error("a.txt", &error("attachment_limit")),
                "‘a.txt’: 첨부파일은 공지마다 10개까지 올릴 수 있어요."
            );
        }

        #[test]
        fn char_count_uses_code_points() {
            assert_eq!(count_label("", 10), ("0 / 10자".into(), false));
            assert_eq!(count_label("가나다", 3), ("3 / 3자".into(), false));
            assert_eq!(count_label("😀😀😀😀", 3), ("4 / 3자".into(), true));
        }
    }
}
