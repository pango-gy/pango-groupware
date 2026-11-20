//! 부서 트리 계산(순수 함수). 구성원 화면의 부서 필터와 조직 관리 화면이 함께 쓴다.
//!
//! 서버는 부서를 평평한 목록(`parent_id`)으로 준다. 여기서 화면에 그릴 순서와 깊이를 정한다.
//! 데이터가 어긋나도(상위 부서가 목록에 없거나 순환이 있어도) 모든 부서를 정확히 한 번씩 보여 준다.

use std::collections::{BTreeMap, BTreeSet};

use pango_api::org::{DepartmentDto, PositionDto};
use pango_core::id::DepartmentId;

/// 트리 순서로 늘어놓은 부서 한 줄.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepartmentRow {
    pub department: DepartmentDto,
    /// 최상위가 0.
    pub depth: usize,
}

/// 화면에서 들여쓰기할 최대 단계. 더 깊으면 같은 들여쓰기로 보여 준다(좁은 화면 보호).
pub const MAX_INDENT: usize = 6;

impl DepartmentRow {
    pub fn indent(&self) -> usize {
        self.depth.min(MAX_INDENT)
    }
}

fn sort_key(d: &DepartmentDto) -> (i64, &str, DepartmentId) {
    (d.sort_order, d.name.as_str(), d.id)
}

/// 부모 ID → 자식 목록(정렬됨). 부모가 목록에 없거나 자기 자신이면 최상위(`None`)로 본다.
fn children_map(list: &[DepartmentDto]) -> BTreeMap<Option<DepartmentId>, Vec<&DepartmentDto>> {
    let ids: BTreeSet<DepartmentId> = list.iter().map(|d| d.id).collect();
    let mut map: BTreeMap<Option<DepartmentId>, Vec<&DepartmentDto>> = BTreeMap::new();
    for d in list {
        let parent = d.parent_id.filter(|p| *p != d.id && ids.contains(p));
        map.entry(parent).or_default().push(d);
    }
    for children in map.values_mut() {
        children.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    }
    map
}

/// 부서를 트리 순서(깊이 우선, 형제는 `sort_order` → 이름 순)로 늘어놓는다.
///
/// - 상위 부서가 목록에 없는 부서는 최상위에 둔다.
/// - 순환(A→B→A)에 걸려 최상위에서 닿을 수 없는 부서는, 정렬 순서상 첫 부서를 최상위로 삼아 이어 붙인다.
/// - 같은 ID가 두 번 들어와도 한 번만 보여 준다.
pub fn department_tree(list: &[DepartmentDto]) -> Vec<DepartmentRow> {
    let children = children_map(list);
    let mut visited: BTreeSet<DepartmentId> = BTreeSet::new();
    let mut rows = Vec::with_capacity(list.len());
    for root in children.get(&None).map(Vec::as_slice).unwrap_or_default() {
        walk(root, &children, &mut visited, &mut rows);
    }
    // 순환에 갇힌 부서들.
    let mut rest: Vec<&DepartmentDto> = list.iter().filter(|d| !visited.contains(&d.id)).collect();
    rest.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    for d in rest {
        walk(d, &children, &mut visited, &mut rows);
    }
    rows
}

/// `root`부터 깊이 우선으로 내려가며 아직 보지 않은 부서를 붙인다.
/// 재귀 대신 스택을 쓴다(아주 깊은 체인에서도 스택이 넘치지 않게).
fn walk(
    root: &DepartmentDto,
    children: &BTreeMap<Option<DepartmentId>, Vec<&DepartmentDto>>,
    visited: &mut BTreeSet<DepartmentId>,
    rows: &mut Vec<DepartmentRow>,
) {
    let mut stack = vec![(root, 0usize)];
    while let Some((d, depth)) = stack.pop() {
        if !visited.insert(d.id) {
            continue;
        }
        rows.push(DepartmentRow {
            department: d.clone(),
            depth,
        });
        if let Some(kids) = children.get(&Some(d.id)) {
            for kid in kids.iter().rev() {
                stack.push((*kid, depth + 1));
            }
        }
    }
}

/// `id` 부서와 그 하위 부서 전체. 순환이 있어도 끝난다.
pub fn self_and_descendants(list: &[DepartmentDto], id: DepartmentId) -> BTreeSet<DepartmentId> {
    let children = children_map(list);
    let mut found = BTreeSet::new();
    let mut stack = vec![id];
    while let Some(current) = stack.pop() {
        if !found.insert(current) {
            continue;
        }
        if let Some(kids) = children.get(&Some(current)) {
            stack.extend(kids.iter().map(|d| d.id));
        }
    }
    found
}

/// 선택 상자에 쓰는 들여쓴 이름. 들여쓰기는 줄바꿈 없는 공백이라 `<option>`에서도 보인다.
pub fn indented_name(name: &str, depth: usize) -> String {
    if depth == 0 {
        return name.to_owned();
    }
    format!(
        "{}└ {name}",
        "\u{00A0}\u{00A0}".repeat(depth.min(MAX_INDENT))
    )
}

/// 부서 선택 상자의 항목 `(값, 문구)`. 첫 항목은 `none_label`(값 `""`).
pub fn department_options(list: &[DepartmentDto], none_label: &str) -> Vec<(String, String)> {
    std::iter::once((String::new(), none_label.to_owned()))
        .chain(department_tree(list).into_iter().map(|row| {
            (
                row.department.id.to_string(),
                indented_name(&row.department.name, row.depth),
            )
        }))
        .collect()
}

/// 구성원 목록 조회 쿼리 `(이름, 값)`(`GET /members`, `GET /admin/members`). 빈 값과 형식이
/// 틀린 부서 ID는 보내지 않는다.
pub fn member_query(
    search: &str,
    department: &str,
    include_inactive: bool,
) -> Vec<(&'static str, String)> {
    let mut pairs = Vec::new();
    let search = search.trim();
    if !search.is_empty() {
        pairs.push(("search", search.to_owned()));
    }
    if let Ok(id) = DepartmentId::parse(department) {
        pairs.push(("department_id", id.to_string()));
    }
    if include_inactive {
        pairs.push(("include_inactive", "true".to_owned()));
    }
    pairs
}

/// 상위 부서 선택 항목. 수정 중인 부서 자신과 그 하위 부서는 고를 수 없다(순환 방지).
pub fn parent_options(
    list: &[DepartmentDto],
    editing: Option<DepartmentId>,
) -> Vec<(String, String)> {
    let excluded = editing
        .map(|id| self_and_descendants(list, id))
        .unwrap_or_default();
    std::iter::once((String::new(), "없음 (최상위)".to_owned()))
        .chain(
            department_tree(list)
                .into_iter()
                .filter(|row| !excluded.contains(&row.department.id))
                .map(|row| {
                    (
                        row.department.id.to_string(),
                        indented_name(&row.department.name, row.depth),
                    )
                }),
        )
        .collect()
}

/// 직책을 높은 등급부터(같으면 이름 순) 늘어놓는다. 서버와 같은 순서다.
pub fn sorted_positions(list: &[PositionDto]) -> Vec<PositionDto> {
    let mut sorted = list.to_vec();
    sorted.sort_by(|a, b| {
        b.level
            .cmp(&a.level)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    sorted
}

/// 직책 선택 상자의 항목 `(값, 문구)`. 첫 항목은 `none_label`(값 `""`).
pub fn position_options(list: &[PositionDto], none_label: &str) -> Vec<(String, String)> {
    std::iter::once((String::new(), none_label.to_owned()))
        .chain(
            sorted_positions(list)
                .into_iter()
                .map(|p| (p.id.to_string(), p.name)),
        )
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn member_query_omits_empty_and_invalid_values() {
        assert!(member_query("  ", "", false).is_empty());
        assert!(member_query("", "not-an-id", false).is_empty());
        let id = DepartmentId::new();
        assert_eq!(
            member_query(" 홍 ", &id.to_string(), true),
            vec![
                ("search", "홍".to_owned()),
                ("department_id", id.to_string()),
                ("include_inactive", "true".to_owned()),
            ]
        );
    }

    pub(crate) fn dept(name: &str, parent: Option<DepartmentId>, sort_order: i64) -> DepartmentDto {
        DepartmentDto {
            id: DepartmentId::new(),
            parent_id: parent,
            name: name.into(),
            head_member_id: None,
            head_name: None,
            sort_order,
            member_count: 0,
            version: 1,
        }
    }

    fn names(rows: &[DepartmentRow]) -> Vec<(String, usize)> {
        rows.iter()
            .map(|r| (r.department.name.clone(), r.depth))
            .collect()
    }

    fn pairs(list: &[(&str, usize)]) -> Vec<(String, usize)> {
        list.iter().map(|(n, d)| ((*n).to_owned(), *d)).collect()
    }

    #[test]
    fn empty_input_gives_empty_tree() {
        assert!(department_tree(&[]).is_empty());
        assert_eq!(
            department_options(&[], "전체"),
            vec![(String::new(), "전체".into())]
        );
    }

    #[test]
    fn orders_by_sort_order_then_name_depth_first() {
        let root_b = dept("경영지원", None, 2);
        let root_a = dept("개발", None, 1);
        let child_2 = dept("프론트엔드", Some(root_a.id), 0);
        let child_1 = dept("백엔드", Some(root_a.id), 0);
        let grandchild = dept("플랫폼", Some(child_1.id), 0);
        let list = vec![grandchild, root_b.clone(), child_2, root_a.clone(), child_1];
        assert_eq!(
            names(&department_tree(&list)),
            pairs(&[
                ("개발", 0),
                ("백엔드", 1),
                ("플랫폼", 2),
                ("프론트엔드", 1),
                ("경영지원", 0),
            ])
        );
    }

    #[test]
    fn negative_sort_order_comes_first() {
        let a = dept("가", None, 0);
        let b = dept("나", None, -5);
        assert_eq!(
            names(&department_tree(&[a, b])),
            pairs(&[("나", 0), ("가", 0)])
        );
    }

    #[test]
    fn unknown_parent_and_self_parent_are_roots() {
        let orphan = dept("고아", Some(DepartmentId::new()), 0);
        let mut selfish = dept("자기참조", None, 1);
        selfish.parent_id = Some(selfish.id);
        let rows = department_tree(&[orphan, selfish]);
        assert_eq!(names(&rows), pairs(&[("고아", 0), ("자기참조", 0)]));
    }

    #[test]
    fn deep_chain_keeps_depth_and_clamps_indent() {
        let mut list = vec![dept("d0", None, 0)];
        for i in 1..500 {
            let parent = list[i - 1].id;
            list.push(dept(&format!("d{i}"), Some(parent), 0));
        }
        list.reverse();
        let rows = department_tree(&list);
        assert_eq!(rows.len(), 500);
        assert!(rows.iter().enumerate().all(|(i, r)| r.depth == i));
        assert_eq!(rows[499].indent(), MAX_INDENT);
        assert_eq!(rows[2].indent(), 2);
    }

    #[test]
    fn cycle_is_shown_once_without_looping() {
        let mut a = dept("A", None, 0);
        let mut b = dept("B", None, 1);
        let c = dept("C", None, 2);
        a.parent_id = Some(b.id);
        b.parent_id = Some(a.id);
        let child_of_b = dept("B-1", Some(b.id), 0);
        let rows = department_tree(&[a.clone(), b.clone(), c, child_of_b]);
        assert_eq!(
            names(&rows),
            pairs(&[("C", 0), ("A", 0), ("B", 1), ("B-1", 2)])
        );
        assert_eq!(self_and_descendants(&[a.clone(), b.clone()], a.id).len(), 2);
    }

    #[test]
    fn duplicate_ids_appear_once() {
        let a = dept("A", None, 0);
        let rows = department_tree(&[a.clone(), a]);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn parent_options_exclude_self_and_descendants() {
        let root = dept("본부", None, 0);
        let team = dept("팀", Some(root.id), 0);
        let part = dept("파트", Some(team.id), 0);
        let other = dept("다른 본부", None, 1);
        let list = vec![root.clone(), team.clone(), part.clone(), other.clone()];

        let values = |editing| -> Vec<String> {
            parent_options(&list, editing)
                .into_iter()
                .map(|(v, _)| v)
                .collect()
        };
        assert_eq!(
            values(Some(team.id)),
            vec![String::new(), root.id.to_string(), other.id.to_string()]
        );
        assert_eq!(
            values(Some(root.id)),
            vec![String::new(), other.id.to_string()]
        );
        assert_eq!(
            values(None).len(),
            5,
            "새 부서는 모든 부서 아래에 둘 수 있다"
        );
        assert_eq!(
            values(Some(DepartmentId::new())).len(),
            5,
            "목록에 없는 부서를 수정하면 아무것도 빼지 않는다"
        );
    }

    fn position(name: &str, level: i64) -> PositionDto {
        PositionDto {
            id: pango_core::id::PositionId::new(),
            name: name.into(),
            level,
            version: 1,
        }
    }

    #[test]
    fn positions_go_from_highest_level() {
        let list = [
            position("사원", 10),
            position("팀장", 50),
            position("대리", 10),
        ];
        let names: Vec<String> = sorted_positions(&list)
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["팀장", "대리", "사원"]);
        assert!(sorted_positions(&[]).is_empty());
        let options = position_options(&list, "없음");
        assert_eq!(options.len(), 4);
        assert_eq!(options[0], (String::new(), "없음".into()));
        assert_eq!(options[1].1, "팀장");
    }

    #[test]
    fn options_are_indented() {
        let root = dept("본부", None, 0);
        let team = dept("팀", Some(root.id), 0);
        let options = department_options(&[root, team], "전체 부서");
        assert_eq!(options[0], (String::new(), "전체 부서".into()));
        assert_eq!(options[1].1, "본부");
        assert_eq!(options[2].1, "\u{00A0}\u{00A0}└ 팀");
        assert_eq!(
            indented_name("x", 100).matches('\u{00A0}').count(),
            MAX_INDENT * 2
        );
    }
}
