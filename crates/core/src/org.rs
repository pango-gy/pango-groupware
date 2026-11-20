//! 부서 트리. 순환·깊이 검증과 하위 부서 계산.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{ConflictReason, DomainError, DomainResult};
use crate::id::DepartmentId;

/// 루트를 깊이 1로 셀 때 허용하는 최대 깊이.
pub const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepartmentNode {
    pub id: DepartmentId,
    pub parent_id: Option<DepartmentId>,
}

/// 검증된 부서 트리(숲). 만들 때 순환과 깊이를 확인한다.
#[derive(Debug, Clone, Default)]
pub struct DepartmentTree {
    parents: BTreeMap<DepartmentId, Option<DepartmentId>>,
    children: BTreeMap<DepartmentId, Vec<DepartmentId>>,
}

impl DepartmentTree {
    /// 없는 부모를 가리키는 노드는 `NotFound`, 순환은 `DepartmentCycle`,
    /// 너무 깊으면 `DepartmentTooDeep`.
    pub fn new(nodes: impl IntoIterator<Item = DepartmentNode>) -> DomainResult<Self> {
        let parents: BTreeMap<_, _> = nodes.into_iter().map(|n| (n.id, n.parent_id)).collect();
        let mut children: BTreeMap<DepartmentId, Vec<DepartmentId>> = BTreeMap::new();
        for (&id, &parent) in &parents {
            if let Some(parent) = parent {
                if !parents.contains_key(&parent) {
                    return Err(DomainError::NotFound);
                }
                children.entry(parent).or_default().push(id);
            }
        }
        let tree = Self { parents, children };
        for &id in tree.parents.keys() {
            tree.depth_of(id)?;
        }
        Ok(tree)
    }

    pub fn contains(&self, id: DepartmentId) -> bool {
        self.parents.contains_key(&id)
    }

    /// 루트부터 센 깊이(루트 = 1).
    fn depth_of(&self, id: DepartmentId) -> DomainResult<usize> {
        let mut depth = 1;
        let mut cursor = id;
        while let Some(parent) = self.parents.get(&cursor).copied().flatten() {
            depth += 1;
            // 깊이 제한을 넘도록 올라갔다면 순환이거나 너무 깊은 것이다.
            if depth > self.parents.len() {
                return Err(ConflictReason::DepartmentCycle.into());
            }
            cursor = parent;
        }
        if depth > MAX_DEPTH {
            return Err(ConflictReason::DepartmentTooDeep.into());
        }
        Ok(depth)
    }

    /// 자신을 포함한 조상 목록(자신 → 루트 순).
    pub fn ancestors_inclusive(&self, id: DepartmentId) -> Vec<DepartmentId> {
        let mut result = Vec::new();
        let mut cursor = self.contains(id).then_some(id);
        while let Some(current) = cursor {
            result.push(current);
            cursor = self.parents.get(&current).copied().flatten();
        }
        result
    }

    /// 자신을 포함한 하위 부서 전체.
    pub fn descendants_inclusive(&self, id: DepartmentId) -> BTreeSet<DepartmentId> {
        let mut result = BTreeSet::new();
        let mut stack: Vec<_> = self.contains(id).then_some(id).into_iter().collect();
        while let Some(current) = stack.pop() {
            if result.insert(current) {
                stack.extend(self.children.get(&current).into_iter().flatten().copied());
            }
        }
        result
    }

    /// `id`를 `new_parent` 아래로 옮길 수 있는지 확인한다.
    pub fn check_move(
        &self,
        id: DepartmentId,
        new_parent: Option<DepartmentId>,
    ) -> DomainResult<()> {
        if !self.contains(id) {
            return Err(DomainError::NotFound);
        }
        let Some(parent) = new_parent else {
            return Ok(());
        };
        if !self.contains(parent) {
            return Err(DomainError::NotFound);
        }
        let subtree = self.descendants_inclusive(id);
        if subtree.contains(&parent) {
            return Err(ConflictReason::DepartmentCycle.into());
        }
        let subtree_height = subtree
            .iter()
            .map(|&d| {
                self.ancestors_inclusive(d)
                    .iter()
                    .take_while(|&&a| a != id)
                    .count()
                    + 1
            })
            .max()
            .unwrap_or(1);
        if self.ancestors_inclusive(parent).len() + subtree_height > MAX_DEPTH {
            return Err(ConflictReason::DepartmentTooDeep.into());
        }
        Ok(())
    }

    /// 새 부서를 `parent` 아래에 만들 수 있는지 확인한다.
    pub fn check_insert(&self, parent: Option<DepartmentId>) -> DomainResult<()> {
        let Some(parent) = parent else { return Ok(()) };
        if !self.contains(parent) {
            return Err(DomainError::NotFound);
        }
        if self.ancestors_inclusive(parent).len() + 1 > MAX_DEPTH {
            return Err(ConflictReason::DepartmentTooDeep.into());
        }
        Ok(())
    }

    /// 조직장이 맡은 부서들로부터 관할 부서(하위 포함)를 계산한다.
    pub fn managed_by(
        &self,
        headed: impl IntoIterator<Item = DepartmentId>,
    ) -> BTreeSet<DepartmentId> {
        headed
            .into_iter()
            .flat_map(|id| self.descendants_inclusive(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<DepartmentId> {
        (0..n).map(|_| DepartmentId::new()).collect()
    }

    fn node(id: DepartmentId, parent_id: Option<DepartmentId>) -> DepartmentNode {
        DepartmentNode { id, parent_id }
    }

    /// d[0] ─ d[1] ─ d[3]
    ///      └ d[2]        ; d[4] 는 별도 루트
    fn sample() -> (DepartmentTree, Vec<DepartmentId>) {
        let d = ids(5);
        let tree = DepartmentTree::new([
            node(d[0], None),
            node(d[1], Some(d[0])),
            node(d[2], Some(d[0])),
            node(d[3], Some(d[1])),
            node(d[4], None),
        ])
        .expect("valid tree");
        (tree, d)
    }

    fn chain(len: usize) -> Vec<DepartmentNode> {
        let d = ids(len);
        d.iter()
            .enumerate()
            .map(|(i, &id)| node(id, i.checked_sub(1).map(|p| d[p])))
            .collect()
    }

    #[test]
    fn empty_tree_is_valid() {
        let tree = DepartmentTree::new([]).expect("empty");
        assert!(tree.descendants_inclusive(DepartmentId::new()).is_empty());
        assert!(tree.ancestors_inclusive(DepartmentId::new()).is_empty());
    }

    #[test]
    fn descendants_and_ancestors() {
        let (tree, d) = sample();
        assert_eq!(
            tree.descendants_inclusive(d[0]),
            BTreeSet::from([d[0], d[1], d[2], d[3]])
        );
        assert_eq!(tree.descendants_inclusive(d[3]), BTreeSet::from([d[3]]));
        assert_eq!(tree.ancestors_inclusive(d[3]), vec![d[3], d[1], d[0]]);
        assert_eq!(tree.ancestors_inclusive(d[4]), vec![d[4]]);
    }

    #[test]
    fn construction_rejects_cycles() {
        let d = ids(3);
        let self_loop = DepartmentTree::new([node(d[0], Some(d[0]))]);
        assert_eq!(
            self_loop.unwrap_err(),
            ConflictReason::DepartmentCycle.into()
        );
        let ring = DepartmentTree::new([
            node(d[0], Some(d[2])),
            node(d[1], Some(d[0])),
            node(d[2], Some(d[1])),
        ]);
        assert_eq!(ring.unwrap_err(), ConflictReason::DepartmentCycle.into());
    }

    #[test]
    fn construction_rejects_dangling_parent() {
        let result = DepartmentTree::new([node(DepartmentId::new(), Some(DepartmentId::new()))]);
        assert_eq!(result.unwrap_err(), DomainError::NotFound);
    }

    #[test]
    fn depth_boundary() {
        assert!(DepartmentTree::new(chain(MAX_DEPTH)).is_ok());
        assert_eq!(
            DepartmentTree::new(chain(MAX_DEPTH + 1)).unwrap_err(),
            ConflictReason::DepartmentTooDeep.into()
        );
    }

    #[test]
    fn move_into_own_subtree_is_a_cycle() {
        let (tree, d) = sample();
        assert_eq!(
            tree.check_move(d[0], Some(d[3])),
            Err(ConflictReason::DepartmentCycle.into())
        );
        assert_eq!(
            tree.check_move(d[1], Some(d[1])),
            Err(ConflictReason::DepartmentCycle.into())
        );
        assert_eq!(tree.check_move(d[3], Some(d[2])), Ok(()));
        assert_eq!(tree.check_move(d[1], None), Ok(()));
        assert_eq!(
            tree.check_move(d[1], Some(d[1 - 1])),
            Ok(()),
            "같은 부모로의 이동은 허용"
        );
    }

    #[test]
    fn move_with_unknown_ids_is_not_found() {
        let (tree, d) = sample();
        assert_eq!(
            tree.check_move(DepartmentId::new(), Some(d[0])),
            Err(DomainError::NotFound)
        );
        assert_eq!(
            tree.check_move(d[0], Some(DepartmentId::new())),
            Err(DomainError::NotFound)
        );
    }

    #[test]
    fn move_respects_depth_including_moved_subtree_height() {
        let nodes = chain(MAX_DEPTH - 1);
        let deepest = nodes[MAX_DEPTH - 2].id;
        let (a, b) = (DepartmentId::new(), DepartmentId::new());
        let mut all = nodes;
        all.extend([node(a, None), node(b, Some(a))]);
        let tree = DepartmentTree::new(all).expect("valid");
        // 높이 2인 서브트리를 깊이 MAX-1 아래에 붙이면 MAX+1 이 된다.
        assert_eq!(
            tree.check_move(a, Some(deepest)),
            Err(ConflictReason::DepartmentTooDeep.into())
        );
        // 높이 1인 잎은 붙일 수 있다.
        assert_eq!(tree.check_move(b, Some(deepest)), Ok(()));
    }

    #[test]
    fn insert_checks_parent_and_depth() {
        let nodes = chain(MAX_DEPTH);
        let (shallow, deepest) = (nodes[0].id, nodes[MAX_DEPTH - 1].id);
        let tree = DepartmentTree::new(nodes).expect("valid");
        assert_eq!(tree.check_insert(None), Ok(()));
        assert_eq!(tree.check_insert(Some(shallow)), Ok(()));
        assert_eq!(
            tree.check_insert(Some(deepest)),
            Err(ConflictReason::DepartmentTooDeep.into())
        );
        assert_eq!(
            tree.check_insert(Some(DepartmentId::new())),
            Err(DomainError::NotFound)
        );
    }

    #[test]
    fn managed_by_unions_subtrees_and_ignores_unknown() {
        let (tree, d) = sample();
        assert_eq!(
            tree.managed_by([d[1], d[4]]),
            BTreeSet::from([d[1], d[3], d[4]])
        );
        assert!(tree.managed_by([DepartmentId::new()]).is_empty());
        assert!(tree.managed_by([]).is_empty());
    }
}
