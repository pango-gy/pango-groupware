//! 첨부파일(공지·결재 문서) 화면 조각: 내려받기 목록과 고르기·지우기 편집기.
//!
//! 기능마다 다른 것은 경로뿐이다. `base`는 소유자의 첨부 경로(`/admin/notices/{id}/attachments`,
//! `/approval/documents/{id}/attachments`, API 접두사 제외)이고, 올리기는 `base`에, 지우기·
//! 내려받기는 `{base}/{attachment_id}`에 한다(공지는 내려받기 경로가 달라 따로 받는다).

use dioxus::prelude::*;
use pango_api::attachment::Attachment;
use pango_core::attachment::MAX_PER_OWNER;
use pango_core::id::AttachmentId;

use super::{Button, ButtonKind, DownloadLink, FormError, Icon, IconName};
use crate::api;
use crate::format;

pub use logic::{check_pick, upload_error, PendingFile};

/// 내려받기 목록. `download_base`는 `{download_base}/{attachment_id}`로 받는 경로(API 접두사 제외).
#[component]
pub fn AttachmentLinks(#[props(into)] download_base: String, files: Vec<Attachment>) -> Element {
    if files.is_empty() {
        return rsx! {};
    }
    rsx! {
        section { class: "attachments", "aria-label": "첨부파일",
            h2 { class: "sub-title", "첨부파일 {files.len()}개" }
            ul { class: "attachment-list",
                for file in files {
                    li { key: "{file.id}",
                        DownloadLink {
                            class: "attachment-link",
                            path: format!("{download_base}/{}", file.id),
                            filename: file.filename.clone(),
                            Icon { name: IconName::Document }
                            span { class: "attachment-name", "{file.filename}" }
                            small { class: "attachment-size", "{format::file_size(file.size)}" }
                        }
                    }
                }
            }
        }
    }
}

/// 파일 하나를 올린다.
pub async fn upload_file(base: &str, file: &PendingFile) -> api::ApiResult<Attachment> {
    api::upload(base, &file.name, file.bytes.clone(), &file.key).await
}

/// 모아 둔 파일을 차례로 올린다. `(올리지 못한 파일, 그 안내 문구)`를 돌려준다. 키가 그대로라
/// 남은 파일을 다시 올려도 이미 올라간 것이 두 번 생기지 않는다.
pub async fn upload_pending(
    base: &str,
    files: Vec<PendingFile>,
) -> (Vec<PendingFile>, Vec<String>) {
    let mut left = Vec::new();
    let mut problems = Vec::new();
    for file in files {
        if let Err(error) = upload_file(base, &file).await {
            problems.push(upload_error(&file.name, &error));
            left.push(file);
        }
    }
    (left, problems)
}

/// 첨부파일 고르기·지우기.
///
/// `base`가 있으면(이미 있는 공지·문서) 고르는 즉시 올리고, 지우기는 한 번 더 확인한 뒤 바로
/// 지운다. 둘 다 "저장"과 무관하게 반영된다. `base`가 없으면(새로 만드는 중) `pending`에 모아
/// 두었다가 만든 뒤 [`upload_pending`]으로 올린다.
#[component]
pub fn AttachmentEditor(
    base: Option<String>,
    uploaded: Signal<Vec<Attachment>>,
    pending: Signal<Vec<PendingFile>>,
) -> Element {
    let mut error = use_signal(String::new);
    let mut working = use_signal(|| false);
    let mut confirming = use_signal(|| None::<AttachmentId>);
    // 같은 파일을 다시 고를 수 있도록 고를 때마다 입력 요소를 새로 만든다.
    let mut picker = use_signal(|| 0_u32);
    let immediate = base.is_some();

    let pick = {
        let base = base.clone();
        move |event: FormEvent| {
            let base = base.clone();
            async move {
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
                    match &base {
                        Some(base) => match upload_file(base, &item).await {
                            Ok(created) => uploaded.write().push(created),
                            Err(failure) => problems.push(upload_error(&item.name, &failure)),
                        },
                        None => pending.write().push(item),
                    }
                }
                error.set(problems.join("\n"));
                working.set(false);
            }
        }
    };

    let remove = move |id: AttachmentId| {
        let base = base.clone();
        async move {
            let Some(base) = base else {
                return;
            };
            if working() {
                return;
            }
            working.set(true);
            match api::delete::<()>(&format!("{base}/{id}")).await {
                // 이미 지워졌으면(404) 목록에서도 뺀다.
                Ok(()) | Err(api::ApiError { status: 404, .. }) => {
                    uploaded.write().retain(|file| file.id != id);
                    error.set(String::new());
                }
                Err(failure) => error.set(failure.message()),
            }
            confirming.set(None);
            working.set(false);
        }
    };

    let saved_files = uploaded.read().clone();
    let picked_files = pending.read().clone();
    let total = saved_files.len() + picked_files.len();

    rsx! {
        div { class: "field attachments",
            span { class: "field-label", "첨부파일 {total}개" }
            if immediate {
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
                                    onclick: {
                                        let remove = remove.clone();
                                        move |_| remove(file.id)
                                    },
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
            if total < MAX_PER_OWNER {
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
                p { class: "field-hint", "첨부파일은 {MAX_PER_OWNER}개까지 올릴 수 있어요." }
            }
        }
    }
}

pub mod logic {
    use pango_core::attachment::{FileName, MAX_PER_OWNER};

    use crate::api::ApiError;

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
        if current >= MAX_PER_OWNER {
            return Err(format!("첨부파일은 {MAX_PER_OWNER}개까지 올릴 수 있어요."));
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn picked_files_follow_server_rules() {
            assert_eq!(check_pick("회의록.pdf", 10, 0), Ok(()));
            assert!(check_pick("회의록.pdf", 0, 0).is_err(), "빈 파일");
            assert!(check_pick("../a.txt", 10, 0).is_err(), "경로");
            assert!(check_pick("a.txt", 10, MAX_PER_OWNER).is_err(), "개수");
            assert!(check_pick("a.txt", 10, MAX_PER_OWNER - 1).is_ok());
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
                "‘a.txt’: 첨부파일은 10개까지 올릴 수 있어요."
            );
        }
    }
}
