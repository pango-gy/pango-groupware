//! 제품이 기본으로 넣는 결재 양식(카탈로그).
//!
//! 양식의 내용은 코드가 아니라 데이터(`form_catalog.json`)다. 양식을 더하거나 필드를 고치려면
//! 그 파일만 고친다. 이미 운영 중인 회사에는 **없는 양식만** 채워 넣는다
//! ([`ensure_form_catalog_tx`]): 관리자가 고친 양식은 덮어쓰지 않고, 꺼 둔 양식도 다시 만들지
//! 않는다(양식은 지우지 않고 끄기만 하므로 `code`가 남아 있다). 그래서 이 파일에서 이미 배포된
//! 양식의 필드를 고쳐도 기존 회사의 양식은 바뀌지 않는다.

use pango_core::approval::{DocumentKind, FormCategory, FormOrigin};
use pango_core::form_schema::FormSchema;
use pango_core::id::FormId;
use pango_core::time::Timestamp;
use pango_db::approval::{self, Form};
use pango_db::Conn;
use serde::Deserialize;

use super::support::{audit, Audit};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

const CATALOG_JSON: &str = include_str!("form_catalog.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogForm {
    code: String,
    name: String,
    category: FormCategory,
    kind: DocumentKind,
    description: String,
    use_body: bool,
    schema: FormSchema,
}

fn catalog() -> ServiceResult<Vec<CatalogForm>> {
    serde_json::from_str(CATALOG_JSON).map_err(ServiceError::internal)
}

/// 카탈로그의 양식 중 아직 없는 것을 넣고, 넣은 양식의 코드를 돌려준다. 몇 번을 불러도 같은
/// 코드는 한 번만 들어간다(유일 제약). 최초 설정과 서버 시작이 부른다.
pub async fn ensure_form_catalog_tx(conn: &mut Conn, now: Timestamp) -> ServiceResult<Vec<String>> {
    let mut inserted = Vec::new();
    for (entry, order) in catalog()?.into_iter().zip(0i64..) {
        let schema = entry
            .schema
            .normalized()
            .map_err(|_| ServiceError::internal("form catalog has an invalid schema"))?;
        let form = Form {
            description: entry.description,
            // 사이에 양식을 끼워 넣을 수 있게 띄워 둔다.
            sort_order: order * 10,
            category: entry.category,
            schema,
            use_body: entry.use_body,
            origin: FormOrigin::Catalog,
            ..Form::plain(FormId::new(), &entry.code, &entry.name, entry.kind)
        };
        if approval::insert_form_if_absent(conn, &form, now).await? {
            inserted.push(form.code);
        }
    }
    Ok(inserted)
}

/// 서버를 시작할 때 부른다: 판올림으로 새로 생긴 기본 양식을 기존 회사에 채워 넣는다.
/// 넣은 것이 있으면 감사 로그(`approval_form.catalog_seed`)에 코드를 남긴다.
pub async fn ensure_form_catalog(state: &AppState) -> ServiceResult<Vec<String>> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let inserted = ensure_form_catalog_tx(conn, now).await?;
            if !inserted.is_empty() {
                audit(
                    conn,
                    now,
                    Audit {
                        action: "approval_form.catalog_seed",
                        target_type: "approval_form",
                        target_id: None,
                        detail: Some(serde_json::json!({ "codes": inserted })),
                        ctx: None,
                    },
                )
                .await?;
            }
            Ok(inserted)
        })
        .await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use pango_core::form_schema::FieldKind;
    use pango_core::text::{Code, Comment, LabelName};

    use super::super::approval::{ATTENDANCE_CORRECTION_FORM_CODE, LEAVE_FORM_CODE};
    use super::*;

    fn entries() -> Vec<CatalogForm> {
        catalog().expect("catalog parses")
    }

    #[test]
    fn every_entry_is_valid_as_written() {
        let entries = entries();
        let mut codes = BTreeSet::new();
        for entry in &entries {
            let code = entry.code.as_str();
            assert_eq!(Code::parse(code).expect("code").as_str(), code);
            assert!(codes.insert(code), "duplicate code {code}");
            assert_eq!(
                LabelName::parse(&entry.name).expect("name").as_str(),
                entry.name
            );
            assert!(Comment::parse(&entry.description).is_ok(), "{code}");
            // 저장되는 모양 그대로 적혀 있어야 한다(기본값·정규화가 파일과 DB에서 어긋나지 않게).
            assert_eq!(
                entry.schema.clone().normalized().as_ref(),
                Ok(&entry.schema),
                "{code}"
            );
        }
    }

    #[test]
    fn covers_every_category_and_the_reference_forms() {
        let entries = entries();
        assert_eq!(entries.len(), 27);
        for category in FormCategory::ALL {
            let count = entries.iter().filter(|e| e.category == *category).count();
            assert_eq!(count == 0, *category == FormCategory::Etc, "{category}");
        }
        // 분류 순서대로 적혀 있다(정렬 순서가 곧 화면 순서).
        let categories: Vec<_> = entries.iter().map(|e| e.category).collect();
        assert!(categories.is_sorted());
        let names: BTreeSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        for name in [
            "업무기안",
            "업무협조",
            "품의서",
            "휴가 신청서",
            "재택근무신청서",
            "결근사유서",
            "휴직원",
            "복직원",
            "사직서",
            "시말서",
            "경위서",
            "채용요청",
            "휴일/연장 근무신청서",
            "탄력근무신청서",
            "근태 정정 신청서",
            "국내출장신청",
            "해외출장신청",
            "지출결의서",
            "추가예산신청",
            "비품/사무용품 신청서",
            "도서구입신청",
            "명함신청",
            "증명서 발급신청",
            "사원증 발급신청",
            "경조금 지급신청",
            "경조화 신청",
            "학자금 지원신청",
        ] {
            assert!(names.contains(name), "{name}");
        }
    }

    #[test]
    fn system_forms_keep_their_codes_and_have_no_fields() {
        let entries = entries();
        let system: Vec<_> = entries
            .iter()
            .filter(|entry| entry.kind.is_system())
            .map(|entry| (entry.code.as_str(), entry.kind, entry.schema.fields.len()))
            .collect();
        assert_eq!(
            system,
            [
                (LEAVE_FORM_CODE, DocumentKind::Leave, 0),
                (
                    ATTENDANCE_CORRECTION_FORM_CODE,
                    DocumentKind::AttendanceCorrection,
                    0
                ),
            ]
        );
    }

    #[test]
    fn forms_without_a_body_ask_for_something() {
        for entry in entries().iter().filter(|entry| !entry.use_body) {
            assert!(
                entry.schema.fields.iter().any(|field| field.required),
                "{} has neither a body nor a required field",
                entry.code
            );
        }
    }

    #[test]
    fn expense_report_has_the_usage_table_with_a_money_column() {
        let entries = entries();
        let expense = entries
            .iter()
            .find(|entry| entry.code == "expense_report")
            .expect("expense_report");
        assert_eq!(expense.category, FormCategory::Accounting);
        let keys: Vec<&str> = expense
            .schema
            .fields
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(keys, ["spent_on", "pay_type", "items", "memo"]);
        let FieldKind::Table { columns } = &expense.schema.fields[2].kind else {
            panic!("items is a table");
        };
        let columns: Vec<(&str, &str)> = columns
            .iter()
            .map(|column| (column.key.as_str(), column.kind.code()))
            .collect();
        assert_eq!(
            columns,
            [
                ("used_on", "date"),
                ("vendor", "text"),
                ("account", "select"),
                ("amount", "money"),
                ("note", "text"),
            ]
        );
    }
}
