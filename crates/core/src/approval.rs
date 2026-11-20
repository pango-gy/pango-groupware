//! 전자결재 상태 기계. 결재선 검증, 문서 상태 전이, 열람 판정.
//!
//! 이 모듈은 IO를 하지 않는다. 서비스 계층은 문서를 읽어 [`Document::restore`]로 되살리고,
//! 전이 함수를 부른 뒤 [`Transition`]이 `Applied`일 때만 저장·알림·연결 효과를 실행한다.
//!
//! 전이 함수의 공통 약속:
//! - 같은 요청이 다시 와도 결과가 같다(자연 멱등). 반복은 [`Transition::NoOp`]이다.
//! - `Err`나 `NoOp`을 돌려줄 때는 문서를 전혀 바꾸지 않는다.
//! - 행위자가 그 동작의 당사자가 아니면 문서 상태보다 먼저 `Forbidden`으로 거절한다.
//!   (문서가 보이지 않아야 하는 사람은 그 전에 [`view`]에서 `NotFound`로 걸러진다.)

use std::fmt;

use crate::authz::Actor;
use crate::error::{ConflictReason, DomainError, DomainResult, ValidationReason};
use crate::id::MemberId;
use crate::string_enum;
use crate::text::{Comment, Reason};
use crate::time::Timestamp;

/// 결재선에 둘 수 있는 최대 단계 수.
pub const MAX_STEPS: usize = 10;
/// 문서 하나에 둘 수 있는 최대 참조자 수.
pub const MAX_REFERENCES: usize = 30;

string_enum! {
    /// 문서 상태. `draft → in_progress → approved | rejected | withdrawn`.
    #[derive(PartialOrd, Ord)]
    pub enum DocumentStatus("status") {
        Draft => "draft",
        InProgress => "in_progress",
        Approved => "approved",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
}

impl DocumentStatus {
    /// 종료된 문서는 더 이상 상태가 바뀌지 않는다.
    pub const fn is_closed(self) -> bool {
        matches!(self, Self::Approved | Self::Rejected | Self::Withdrawn)
    }
}

string_enum! {
    /// 단계 종류.
    #[derive(PartialOrd, Ord)]
    pub enum StepKind("step_kind") {
        /// 결재.
        Approve => "approve",
        /// 합의.
        Agree => "agree",
    }
}

string_enum! {
    /// 단계 상태.
    #[derive(PartialOrd, Ord)]
    pub enum StepStatus("step_status") {
        /// 아직 차례가 오지 않음.
        Waiting => "waiting",
        /// 지금 처리할 차례.
        Pending => "pending",
        Approved => "approved",
        Rejected => "rejected",
    }
}

string_enum! {
    /// 문서 종류. `General` 외에는 최종 결과에 따라 서비스 계층이 연결된 효과를 실행한다.
    #[derive(PartialOrd, Ord)]
    pub enum DocumentKind("document_kind") {
        General => "general",
        /// 휴가 신청: 승인 시 확정, 반려·회수 시 잔여 복원.
        Leave => "leave",
        /// 근태 정정: 승인 시 기록 정정.
        AttendanceCorrection => "attendance_correction",
    }
}

impl DocumentKind {
    /// 연결된 기능(휴가 신청, 근태 정정)만 문서를 만드는 시스템 양식의 종류인지.
    pub const fn is_system(self) -> bool {
        !matches!(self, Self::General)
    }
}

string_enum! {
    /// 양식 분류. 기안 화면이 이 순서([`FormCategory::ALL`])로 묶어 보여 준다.
    #[derive(PartialOrd, Ord)]
    pub enum FormCategory("category") {
        General => "general",
        Hr => "hr",
        Attendance => "attendance",
        Trip => "trip",
        Accounting => "accounting",
        Support => "support",
        Welfare => "welfare",
        /// 어느 분류에도 넣지 않은 양식.
        Etc => "etc",
    }
}

impl FormCategory {
    /// 화면에 보여 줄 이름.
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "일반",
            Self::Hr => "인사",
            Self::Attendance => "근태",
            Self::Trip => "출장",
            Self::Accounting => "회계",
            Self::Support => "지원",
            Self::Welfare => "복지",
            Self::Etc => "기타",
        }
    }
}

string_enum! {
    /// 양식이 어디서 왔는지.
    #[derive(PartialOrd, Ord)]
    pub enum FormOrigin("origin") {
        /// 제품이 기본으로 넣는 양식. 없는 것만 채워 넣고, 관리자가 고친 내용은 덮어쓰지 않는다.
        Catalog => "catalog",
        /// 관리자가 만든 양식.
        Custom => "custom",
    }
}

string_enum! {
    /// 문서함. 목록 필터(SQL)는 서버 쪽에 있고 여기서는 이름만 정한다.
    #[derive(PartialOrd, Ord)]
    pub enum Mailbox("mailbox") {
        /// 내가 결재할 차례인 문서.
        Todo => "todo",
        /// 내 차례가 아직 오지 않은 문서.
        Upcoming => "upcoming",
        /// 내가 기안한 문서.
        Drafted => "drafted",
        /// 내가 처리(승인·반려)한 문서.
        Done => "done",
        /// 내가 참조자인 문서.
        Reference => "reference",
    }
}

fn invalid(field: &'static str, reason: ValidationReason) -> DomainError {
    DomainError::validation(field, reason)
}

/// 결재선의 한 단계(입력).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineStep {
    pub approver: MemberId,
    pub kind: StepKind,
}

/// 검증된 결재선. 이 타입이 있으면 아래 규칙이 모두 지켜진 것이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalLine {
    drafter: MemberId,
    steps: Vec<LineStep>,
    references: Vec<MemberId>,
}

impl ApprovalLine {
    /// 규칙과 위반 시 오류(`Validation { field, reason }`):
    ///
    /// | 규칙 | field | reason |
    /// | --- | --- | --- |
    /// | 단계가 1개 이상 | `steps` | `Empty` |
    /// | 단계가 [`MAX_STEPS`]개 이하 | `steps` | `TooLong` |
    /// | 기안자는 결재자가 될 수 없음 | `steps` | `Inconsistent` |
    /// | 같은 사람이 두 단계에 나올 수 없음 | `steps` | `Duplicate` |
    /// | 마지막 단계는 결재(`Approve`) | `steps` | `Inconsistent` |
    /// | 참조자가 [`MAX_REFERENCES`]명 이하 | `references` | `TooLong` |
    /// | 참조자 중복 없음 | `references` | `Duplicate` |
    /// | 참조자는 기안자·결재자가 아님 | `references` | `Inconsistent` |
    ///
    /// 마지막이 합의면 문서를 최종 확정하는 사람이 없으므로 받지 않는다.
    /// 기안자·결재자는 이미 문서를 볼 수 있으므로 참조자로 다시 넣는 것은 입력 실수로 본다.
    pub fn new(
        drafter: MemberId,
        steps: Vec<LineStep>,
        references: Vec<MemberId>,
    ) -> DomainResult<Self> {
        // 개수부터 확인해, 거대한 입력에 대해 아래의 제곱 시간 비교를 하지 않는다.
        if steps.is_empty() {
            return Err(invalid("steps", ValidationReason::Empty));
        }
        if steps.len() > MAX_STEPS {
            return Err(invalid(
                "steps",
                ValidationReason::TooLong { max: MAX_STEPS },
            ));
        }
        if references.len() > MAX_REFERENCES {
            return Err(invalid(
                "references",
                ValidationReason::TooLong {
                    max: MAX_REFERENCES,
                },
            ));
        }
        if steps.iter().any(|step| step.approver == drafter) {
            return Err(invalid("steps", ValidationReason::Inconsistent));
        }
        if has_duplicates(steps.iter().map(|step| step.approver)) {
            return Err(invalid("steps", ValidationReason::Duplicate));
        }
        if steps.last().map(|step| step.kind) != Some(StepKind::Approve) {
            return Err(invalid("steps", ValidationReason::Inconsistent));
        }
        if has_duplicates(references.iter().copied()) {
            return Err(invalid("references", ValidationReason::Duplicate));
        }
        let on_line =
            |member: &MemberId| *member == drafter || steps.iter().any(|s| s.approver == *member);
        if references.iter().any(on_line) {
            return Err(invalid("references", ValidationReason::Inconsistent));
        }
        Ok(Self {
            drafter,
            steps,
            references,
        })
    }

    pub fn drafter(&self) -> MemberId {
        self.drafter
    }

    pub fn steps(&self) -> &[LineStep] {
        &self.steps
    }

    pub fn references(&self) -> &[MemberId] {
        &self.references
    }
}

/// 개수 상한을 확인한 뒤에만 부른다(최대 30개라 단순 비교로 충분하다).
fn has_duplicates(members: impl Iterator<Item = MemberId> + Clone) -> bool {
    members
        .clone()
        .enumerate()
        .any(|(index, member)| members.clone().take(index).any(|m| m == member))
}

/// 문서에 붙은 결재 단계.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// 1부터 시작하는 순서.
    pub order: u8,
    pub approver: MemberId,
    pub kind: StepKind,
    pub status: StepStatus,
    pub acted_at: Option<Timestamp>,
    /// 승인 의견 또는 반려 사유.
    pub comment: Option<Comment>,
}

/// 전이로 생긴 사실. 서비스 계층이 알림과 연결 효과(휴가 확정·복원, 근태 정정)에 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentEvent {
    Submitted {
        first_approver: MemberId,
    },
    /// `next_approver`가 없으면 마지막 단계였고, 바로 뒤에 `Completed`가 따라온다.
    StepApproved {
        order: u8,
        by: MemberId,
        next_approver: Option<MemberId>,
    },
    /// 최종 승인.
    Completed,
    Rejected {
        order: u8,
        by: MemberId,
    },
    /// 관리자가 진행 중인 단계를 대신 반려로 닫았다([`Document::force_reject`]).
    /// `pending_approver`는 처리하지 못한 채 건너뛴 그 단계의 결재자다.
    ForceRejected {
        order: u8,
        pending_approver: MemberId,
    },
    Withdrawn,
}

impl DocumentEvent {
    /// 문서를 종료시키는 사건인지. 문서 하나에 한 번만 일어난다.
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Rejected { .. } | Self::ForceRejected { .. } | Self::Withdrawn
        )
    }
}

string_enum! {
    /// 문서 이력(`approval_events`)에 남기는 사건 종류. [`DocumentEvent`]마다 하나씩이며,
    /// 관리자의 대리 반려([`DocumentEvent::ForceRejected`])도 `Rejected`로 남긴다(이력의
    /// 행위자가 그 단계의 결재자가 아닌 관리자라는 점으로 구별된다).
    #[derive(PartialOrd, Ord)]
    pub enum EventKind("event_kind") {
        Submitted => "submitted",
        /// 한 단계의 승인(결재·합의).
        Approved => "approved",
        /// 최종 승인.
        Completed => "completed",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
}

impl DocumentEvent {
    /// 이력에 남길 종류.
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::Submitted { .. } => EventKind::Submitted,
            Self::StepApproved { .. } => EventKind::Approved,
            Self::Completed => EventKind::Completed,
            Self::Rejected { .. } | Self::ForceRejected { .. } => EventKind::Rejected,
            Self::Withdrawn => EventKind::Withdrawn,
        }
    }
}

/// 전이 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "NoOp이면 저장·알림·연결 효과를 건너뛰어야 한다"]
pub enum Transition {
    /// 문서가 바뀌었다. 사건 목록은 일어난 순서다.
    Applied(Vec<DocumentEvent>),
    /// 이미 반영된 요청의 반복. 문서는 그대로이고 부수효과를 실행하면 안 된다.
    NoOp,
}

impl Transition {
    pub fn events(&self) -> &[DocumentEvent] {
        match self {
            Self::Applied(events) => events,
            Self::NoOp => &[],
        }
    }

    pub const fn is_noop(&self) -> bool {
        matches!(self, Self::NoOp)
    }
}

/// 구성원이 문서에서 맡은 자리.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Participant {
    Drafter,
    Approver { order: u8, status: StepStatus },
    Reference,
}

/// 결재 문서. 필드는 전이 함수로만 바뀐다.
///
/// 불변식([`Document::restore`]가 확인하고 모든 전이가 지킨다):
/// 1. 결재선은 [`ApprovalLine::new`]의 규칙을 만족하고, `order`는 1부터 빈틈없이 이어진다.
/// 2. 단계 상태의 배열 모양은 문서 상태로 정해진다.
///    - `Draft`, `Withdrawn`: 모두 `Waiting`.
///    - `InProgress`: `Approved`* → `Pending` 1개 → `Waiting`*.
///    - `Approved`: 모두 `Approved`.
///    - `Rejected`: `Approved`* → `Rejected` 1개 → `Waiting`*.
/// 3. `Waiting`·`Pending` 단계에는 `acted_at`과 의견이 없다. `Approved`·`Rejected` 단계에는
///    `acted_at`이 있고, `Rejected` 단계에는 비어 있지 않은 사유가 있다.
/// 4. `submitted_at`은 `Draft`가 아닐 때만, `completed_at`은 종료된 문서에만 있다.
/// 5. 시각은 뒤로 가지 않는다: `submitted_at` ≤ 단계별 `acted_at`(순서대로) 이고,
///    `completed_at`은 승인·반려 문서에서는 마지막으로 처리된 단계의 `acted_at`과 같고
///    회수 문서에서는 `submitted_at` 이상이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    kind: DocumentKind,
    status: DocumentStatus,
    drafter: MemberId,
    steps: Vec<Step>,
    references: Vec<MemberId>,
    submitted_at: Option<Timestamp>,
    completed_at: Option<Timestamp>,
}

fn waiting_steps(line: &[LineStep]) -> Vec<Step> {
    // 단계 수는 MAX_STEPS 이하이므로 u8 범위를 넘지 않는다.
    line.iter()
        .zip(1u8..)
        .map(|(step, order)| Step {
            order,
            approver: step.approver,
            kind: step.kind,
            status: StepStatus::Waiting,
            acted_at: None,
            comment: None,
        })
        .collect()
}

impl Document {
    /// 새 임시저장 문서. 기안자는 결재선에 들어 있는 기안자다.
    pub fn draft(kind: DocumentKind, line: ApprovalLine) -> Self {
        Self {
            kind,
            status: DocumentStatus::Draft,
            drafter: line.drafter,
            steps: waiting_steps(&line.steps),
            references: line.references,
            submitted_at: None,
            completed_at: None,
        }
    }

    /// 저장된 값으로 문서를 되살린다. DB 값도 믿지 않고 불변식 전체를 다시 확인한다.
    ///
    /// 결재선 규칙 위반은 [`ApprovalLine::new`]와 같은 오류를, 그 밖의 손상은
    /// `Validation { field, reason: Inconsistent }`를 돌려준다. `field`는 어긋난 곳
    /// (`steps`, `submitted_at`, `completed_at`)이다. 손상된 문서로는 어떤 전이도 할 수 없다.
    pub fn restore(
        kind: DocumentKind,
        status: DocumentStatus,
        drafter: MemberId,
        steps: Vec<Step>,
        references: Vec<MemberId>,
        submitted_at: Option<Timestamp>,
        completed_at: Option<Timestamp>,
    ) -> DomainResult<Self> {
        if steps.len() > MAX_STEPS {
            return Err(invalid(
                "steps",
                ValidationReason::TooLong { max: MAX_STEPS },
            ));
        }
        let line_steps = steps
            .iter()
            .map(|s| LineStep {
                approver: s.approver,
                kind: s.kind,
            })
            .collect();
        let line = ApprovalLine::new(drafter, line_steps, references)?;

        let corrupt = |field| invalid(field, ValidationReason::Inconsistent);
        if steps
            .iter()
            .zip(1u8..)
            .any(|(step, expected)| step.order != expected)
        {
            return Err(corrupt("steps"));
        }

        let fields_ok = |step: &Step| match step.status {
            StepStatus::Waiting | StepStatus::Pending => {
                step.acted_at.is_none() && step.comment.is_none()
            }
            StepStatus::Approved => step.acted_at.is_some(),
            StepStatus::Rejected => {
                step.acted_at.is_some()
                    && step
                        .comment
                        .as_ref()
                        .is_some_and(|c| !c.as_str().is_empty())
            }
        };
        if !steps.iter().all(fields_ok) {
            return Err(corrupt("steps"));
        }

        let approved_prefix = steps
            .iter()
            .take_while(|s| s.status == StepStatus::Approved)
            .count();
        let mut tail = steps.iter().skip(approved_prefix);
        let pivot = tail.next().map(|step| step.status);
        let tail_waiting = tail.all(|step| step.status == StepStatus::Waiting);
        let shape_ok = match status {
            DocumentStatus::Draft | DocumentStatus::Withdrawn => {
                approved_prefix == 0 && pivot == Some(StepStatus::Waiting) && tail_waiting
            }
            DocumentStatus::InProgress => pivot == Some(StepStatus::Pending) && tail_waiting,
            DocumentStatus::Approved => pivot.is_none(),
            DocumentStatus::Rejected => pivot == Some(StepStatus::Rejected) && tail_waiting,
        };
        if !shape_ok {
            return Err(corrupt("steps"));
        }

        if submitted_at.is_some() == (status == DocumentStatus::Draft) {
            return Err(corrupt("submitted_at"));
        }
        if completed_at.is_some() != status.is_closed() {
            return Err(corrupt("completed_at"));
        }
        let mut latest = submitted_at;
        for acted_at in steps.iter().filter_map(|step| step.acted_at) {
            if latest.is_some_and(|previous| acted_at < previous) {
                return Err(corrupt("steps"));
            }
            latest = Some(acted_at);
        }
        let completed_ok = match status {
            DocumentStatus::Approved | DocumentStatus::Rejected => completed_at == latest,
            DocumentStatus::Withdrawn => completed_at >= submitted_at,
            DocumentStatus::Draft | DocumentStatus::InProgress => true,
        };
        if !completed_ok {
            return Err(corrupt("completed_at"));
        }

        Ok(Self {
            kind,
            status,
            drafter,
            steps,
            references: line.references,
            submitted_at,
            completed_at,
        })
    }

    pub fn kind(&self) -> DocumentKind {
        self.kind
    }

    pub fn status(&self) -> DocumentStatus {
        self.status
    }

    pub fn drafter(&self) -> MemberId {
        self.drafter
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn references(&self) -> &[MemberId] {
        &self.references
    }

    pub fn submitted_at(&self) -> Option<Timestamp> {
        self.submitted_at
    }

    pub fn completed_at(&self) -> Option<Timestamp> {
        self.completed_at
    }

    /// 지금 처리할 차례인 결재자. 진행 중이 아니면 없다.
    pub fn current_approver(&self) -> Option<MemberId> {
        self.steps
            .iter()
            .find(|s| s.status == StepStatus::Pending)
            .map(|s| s.approver)
    }

    /// 승인 또는 반려로 처리된 단계 수.
    pub fn acted_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Approved | StepStatus::Rejected))
            .count()
    }

    /// 구성원이 이 문서에서 맡은 자리. 결재선 규칙상 한 사람은 한 자리만 가진다.
    pub fn participant_role(&self, member: MemberId) -> Option<Participant> {
        if member == self.drafter {
            return Some(Participant::Drafter);
        }
        if let Some(step) = self.steps.iter().find(|s| s.approver == member) {
            return Some(Participant::Approver {
                order: step.order,
                status: step.status,
            });
        }
        self.references
            .contains(&member)
            .then_some(Participant::Reference)
    }

    /// 기록할 시각. 서버 시계가 뒤로 가더라도 문서 안의 시각 순서는 지킨다.
    /// 시계 문제로 결재를 막는 것보다 직전 기록 시각으로 맞추는 편이 안전하다.
    fn effective_time(&self, now: Timestamp) -> Timestamp {
        self.steps
            .iter()
            .filter_map(|s| s.acted_at)
            .chain(self.submitted_at)
            .fold(now, Timestamp::max)
    }

    fn require_drafter(&self, actor: MemberId) -> DomainResult<()> {
        if actor == self.drafter {
            Ok(())
        } else {
            Err(DomainError::Forbidden)
        }
    }

    /// 상신. 기안자만 할 수 있다.
    ///
    /// 이미 진행 중이면 `NoOp`이다. 누군가 처리한 뒤에 도착한 재시도도 "이미 상신됨"이
    /// 사실이므로 같은 답을 준다. 종료된 문서는 `DocumentAlreadyClosed`.
    pub fn submit(&mut self, actor: MemberId, now: Timestamp) -> DomainResult<Transition> {
        self.require_drafter(actor)?;
        match self.status {
            DocumentStatus::Draft => {}
            DocumentStatus::InProgress => return Ok(Transition::NoOp),
            _ => return Err(ConflictReason::DocumentAlreadyClosed.into()),
        }
        // 불변식상 단계는 항상 1개 이상이다. 그래도 없다면 손상된 문서이므로 바꾸지 않는다.
        let Some(first) = self.steps.first_mut() else {
            return Err(invalid("steps", ValidationReason::Empty));
        };
        first.status = StepStatus::Pending;
        self.status = DocumentStatus::InProgress;
        self.submitted_at = Some(now);
        Ok(Transition::Applied(vec![DocumentEvent::Submitted {
            first_approver: first.approver,
        }]))
    }

    /// 결재선 교체. 기안자가 임시저장 상태에서만 할 수 있다.
    ///
    /// 기안자가 아니면 `Forbidden`, 임시저장이 아니면 `DocumentNotEditable`,
    /// 다른 기안자의 결재선이면 `Validation { steps, Inconsistent }`.
    /// 같은 결재선이면 `NoOp`, 바뀌었으면 사건 없는 `Applied`다.
    pub fn replace_line(
        &mut self,
        actor: MemberId,
        line: ApprovalLine,
    ) -> DomainResult<Transition> {
        self.require_drafter(actor)?;
        if self.status != DocumentStatus::Draft {
            return Err(ConflictReason::DocumentNotEditable.into());
        }
        if line.drafter != self.drafter {
            return Err(invalid("steps", ValidationReason::Inconsistent));
        }
        let steps = waiting_steps(&line.steps);
        if steps == self.steps && line.references == self.references {
            return Ok(Transition::NoOp);
        }
        self.steps = steps;
        self.references = line.references;
        Ok(Transition::Applied(Vec::new()))
    }

    /// 승인(결재·합의 공통). 현재 단계의 결재자만 할 수 있다.
    ///
    /// | 상황 | 결과 |
    /// | --- | --- |
    /// | 결재선에 없는 사람(기안자·참조자·관리자 포함) | `Forbidden` |
    /// | 자신의 단계를 이미 승인했고 문서가 진행 중이거나 승인됨 | `NoOp` |
    /// | 임시저장이거나, 아직 차례가 아님 | `NotCurrentApprover` |
    /// | 반려·회수로 종료된 문서 | `DocumentAlreadyClosed` |
    pub fn approve(
        &mut self,
        actor: MemberId,
        now: Timestamp,
        comment: Option<Comment>,
    ) -> DomainResult<Transition> {
        let at = self.effective_time(now);
        let mut from_actor = self
            .steps
            .iter_mut()
            .skip_while(|step| step.approver != actor);
        let Some(current) = from_actor.next() else {
            return Err(DomainError::Forbidden);
        };
        match (self.status, current.status) {
            (DocumentStatus::InProgress, StepStatus::Pending) => {}
            (DocumentStatus::InProgress | DocumentStatus::Approved, StepStatus::Approved) => {
                return Ok(Transition::NoOp);
            }
            (DocumentStatus::Draft | DocumentStatus::InProgress, _) => {
                return Err(ConflictReason::NotCurrentApprover.into());
            }
            _ => return Err(ConflictReason::DocumentAlreadyClosed.into()),
        }
        current.status = StepStatus::Approved;
        current.acted_at = Some(at);
        current.comment = comment;
        let order = current.order;
        let next_approver = from_actor.next().map(|next| {
            next.status = StepStatus::Pending;
            next.approver
        });
        let mut events = vec![DocumentEvent::StepApproved {
            order,
            by: actor,
            next_approver,
        }];
        if next_approver.is_none() {
            self.status = DocumentStatus::Approved;
            self.completed_at = Some(at);
            events.push(DocumentEvent::Completed);
        }
        Ok(Transition::Applied(events))
    }

    /// 반려. 현재 단계의 결재자만 할 수 있고 사유가 필요하다. 문서는 즉시 종료되며
    /// 뒤 단계는 `Waiting`으로 남는다.
    ///
    /// | 상황 | 결과 |
    /// | --- | --- |
    /// | 결재선에 없는 사람 | `Forbidden` |
    /// | 자신이 반려해 종료된 문서 | `NoOp` (사유는 처음 것을 유지) |
    /// | 임시저장이거나, 차례가 아니거나, 이미 승인한 뒤(진행 중) | `NotCurrentApprover` |
    /// | 그 밖의 종료된 문서 | `DocumentAlreadyClosed` |
    pub fn reject(
        &mut self,
        actor: MemberId,
        now: Timestamp,
        reason: Reason,
    ) -> DomainResult<Transition> {
        let at = self.effective_time(now);
        let Some(current) = self.steps.iter_mut().find(|step| step.approver == actor) else {
            return Err(DomainError::Forbidden);
        };
        match (self.status, current.status) {
            (DocumentStatus::InProgress, StepStatus::Pending) => {}
            (DocumentStatus::Rejected, StepStatus::Rejected) => return Ok(Transition::NoOp),
            (DocumentStatus::Draft | DocumentStatus::InProgress, _) => {
                return Err(ConflictReason::NotCurrentApprover.into());
            }
            _ => return Err(ConflictReason::DocumentAlreadyClosed.into()),
        }
        // 사유와 의견은 길이·문자 규칙이 같아 항상 변환되지만, 실패하더라도 문서를 바꾸기 전이다.
        let comment = Comment::parse(reason.as_str())?;
        current.status = StepStatus::Rejected;
        current.acted_at = Some(at);
        current.comment = Some(comment);
        let order = current.order;
        self.status = DocumentStatus::Rejected;
        self.completed_at = Some(at);
        Ok(Transition::Applied(vec![DocumentEvent::Rejected {
            order,
            by: actor,
        }]))
    }

    /// 관리자의 대리 반려. 진행 중인 문서를 **지금 차례인 단계에서** 반려로 닫는다.
    ///
    /// 차례인 결재자가 정지·퇴사하면 그 문서는 아무도 처리할 수 없다(기안자는 누군가 결재한
    /// 뒤라 회수하지 못하고, 다른 사람은 차례가 아니다). 그러면 연결된 휴가의 예약 잔여나 근태
    /// 정정 요청이 영원히 묶이므로, 관리자가 결재자를 대신해 문서를 끝낼 수 있게 한다.
    ///
    /// 결과는 결재자가 직접 반려한 것과 **같은 모양**이다: 차례였던 단계가 `Rejected`(사유·시각
    /// 기록)가 되고 뒤 단계는 `Waiting`으로 남으며 문서는 `Rejected`로 종료된다. 그래서
    /// [`Document::restore`]의 불변식을 그대로 만족한다. 누가 실제로 닫았는지는 문서가 아니라
    /// 이력(`approval_events`의 행위자)과 감사 로그가 말해 준다.
    ///
    /// 이 함수는 **누가** 부를 수 있는지는 판정하지 않는다(행위자는 결재선의 사람이 아니다).
    /// 호출하는 쪽이 먼저 [`crate::authz::override_document`]로 확인한다.
    ///
    /// | 상황 | 결과 |
    /// | --- | --- |
    /// | 진행 중 | 반려로 종료 (`ForceRejected`) |
    /// | 이미 반려로 종료됨 | `NoOp` (사유·시각은 처음 것을 유지) |
    /// | 임시저장 | `DocumentNotEditable` (아직 올리지 않은 글은 닫을 것이 없다) |
    /// | 승인·회수로 종료됨 | `DocumentAlreadyClosed` |
    pub fn force_reject(&mut self, now: Timestamp, reason: Reason) -> DomainResult<Transition> {
        match self.status {
            DocumentStatus::InProgress => {}
            DocumentStatus::Rejected => return Ok(Transition::NoOp),
            DocumentStatus::Draft => return Err(ConflictReason::DocumentNotEditable.into()),
            DocumentStatus::Approved | DocumentStatus::Withdrawn => {
                return Err(ConflictReason::DocumentAlreadyClosed.into());
            }
        }
        let at = self.effective_time(now);
        // 불변식상 진행 중인 문서에는 차례인 단계가 정확히 하나 있다. 없다면 손상된 문서이므로
        // 바꾸지 않는다.
        let Some(current) = self
            .steps
            .iter_mut()
            .find(|step| step.status == StepStatus::Pending)
        else {
            return Err(invalid("steps", ValidationReason::Inconsistent));
        };
        // 실패하더라도 문서를 바꾸기 전이다.
        let comment = Comment::parse(reason.as_str())?;
        current.status = StepStatus::Rejected;
        current.acted_at = Some(at);
        current.comment = Some(comment);
        let (order, pending_approver) = (current.order, current.approver);
        self.status = DocumentStatus::Rejected;
        self.completed_at = Some(at);
        Ok(Transition::Applied(vec![DocumentEvent::ForceRejected {
            order,
            pending_approver,
        }]))
    }

    /// 회수. 기안자가 아무도 처리하기 전에만 할 수 있다.
    ///
    /// 임시저장 문서는 회수가 아니라 삭제 대상이므로 `DocumentNotEditable`.
    /// 누군가 처리했으면 `WithdrawTooLate`, 이미 회수됐으면 `NoOp`,
    /// 승인·반려로 끝났으면 `DocumentAlreadyClosed`.
    pub fn withdraw(&mut self, actor: MemberId, now: Timestamp) -> DomainResult<Transition> {
        self.require_drafter(actor)?;
        match self.status {
            DocumentStatus::InProgress if self.acted_count() == 0 => {}
            DocumentStatus::InProgress => return Err(ConflictReason::WithdrawTooLate.into()),
            DocumentStatus::Draft => return Err(ConflictReason::DocumentNotEditable.into()),
            DocumentStatus::Withdrawn => return Ok(Transition::NoOp),
            DocumentStatus::Approved | DocumentStatus::Rejected => {
                return Err(ConflictReason::DocumentAlreadyClosed.into());
            }
        }
        let at = self.effective_time(now);
        for step in &mut self.steps {
            step.status = StepStatus::Waiting;
        }
        self.status = DocumentStatus::Withdrawn;
        self.completed_at = Some(at);
        Ok(Transition::Applied(vec![DocumentEvent::Withdrawn]))
    }
}

/// 문서를 열람할 수 있는지. 볼 수 없으면 존재를 숨기려고 항상 `NotFound`다.
///
/// - 임시저장 문서는 기안자만 본다. 아직 올리지 않은 글이므로 관리자와 결재선의 사람도 볼 수 없다.
/// - 그 밖에는 기안자, 결재선 전원(차례가 오지 않은 사람 포함), 참조자, 관리자가 본다.
///
/// 관리자는 열람만 한다. 결재선에 없으면 승인·반려는 `Forbidden`이다.
pub fn view(actor: &Actor, document: &Document) -> DomainResult<()> {
    let role = document.participant_role(actor.member_id);
    let visible = if document.status == DocumentStatus::Draft {
        role == Some(Participant::Drafter)
    } else {
        role.is_some() || actor.is_admin()
    };
    if visible {
        Ok(())
    } else {
        Err(DomainError::NotFound)
    }
}

/// 문서 번호. `2026-000123`처럼 연도 4자리와 그 해의 일련번호 6자리다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentNumber {
    year: i32,
    seq: u32,
}

impl DocumentNumber {
    pub const MIN_YEAR: i32 = 2000;
    pub const MAX_YEAR: i32 = 9999;
    pub const MAX_SEQ: u32 = 999_999;

    const FIELD: &'static str = "document_number";

    /// 범위를 벗어나면 `OutOfRange`. 자릿수가 넘치면 번호가 정렬되지 않으므로 잘라 쓰지 않는다.
    pub fn new(year: i32, seq: u32) -> DomainResult<Self> {
        let year_ok = (Self::MIN_YEAR..=Self::MAX_YEAR).contains(&year);
        let seq_ok = (1..=Self::MAX_SEQ).contains(&seq);
        if year_ok && seq_ok {
            Ok(Self { year, seq })
        } else {
            Err(invalid(Self::FIELD, ValidationReason::OutOfRange))
        }
    }

    /// 문자열 표기를 바로 만든다.
    pub fn format(year: i32, seq: u32) -> DomainResult<String> {
        Self::new(year, seq).map(|number| number.to_string())
    }

    /// `format`이 만드는 표기만 받는다(부호, 공백, 자릿수 생략, 전각 숫자 모두 거부).
    pub fn parse(value: &str) -> DomainResult<Self> {
        let malformed = invalid(Self::FIELD, ValidationReason::InvalidFormat);
        let all_digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
        let Some((year, seq)) = value.split_once('-') else {
            return Err(malformed);
        };
        if year.len() != 4 || seq.len() != 6 || !all_digits(year) || !all_digits(seq) {
            return Err(malformed);
        }
        match (year.parse(), seq.parse()) {
            (Ok(year), Ok(seq)) => Self::new(year, seq),
            _ => Err(malformed),
        }
    }

    pub const fn year(&self) -> i32 {
        self.year
    }

    pub const fn seq(&self) -> u32 {
        self.seq
    }
}

impl fmt::Display for DocumentNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:06}", self.year, self.seq)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;

    use super::*;
    use crate::authz::Role;

    use StepKind::{Agree, Approve};

    const NOT_CURRENT: DomainError = DomainError::Conflict(ConflictReason::NotCurrentApprover);
    const CLOSED: DomainError = DomainError::Conflict(ConflictReason::DocumentAlreadyClosed);
    const NOT_EDITABLE: DomainError = DomainError::Conflict(ConflictReason::DocumentNotEditable);
    const TOO_LATE: DomainError = DomainError::Conflict(ConflictReason::WithdrawTooLate);

    fn ts(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis).expect("valid timestamp")
    }

    fn member(n: u128) -> MemberId {
        MemberId::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn reason(text: &str) -> Reason {
        Reason::parse(text).expect("valid reason")
    }

    fn comment(text: &str) -> Comment {
        Comment::parse(text).expect("valid comment")
    }

    fn actor(member_id: MemberId, role: Role) -> Actor {
        Actor {
            member_id,
            role,
            department_id: None,
            managed_departments: BTreeSet::new(),
        }
    }

    const DRAFTER: u128 = 1;
    const REFERENCE: u128 = 50;
    const OUTSIDER: u128 = 99;

    /// 결재자는 `member(101)`, `member(102)` … 순서다.
    fn approver(order: u128) -> MemberId {
        member(100 + order)
    }

    fn line_steps(kinds: &[StepKind]) -> Vec<LineStep> {
        kinds
            .iter()
            .zip(1u128..)
            .map(|(&kind, n)| LineStep {
                approver: approver(n),
                kind,
            })
            .collect()
    }

    fn line(kinds: &[StepKind]) -> ApprovalLine {
        ApprovalLine::new(member(DRAFTER), line_steps(kinds), vec![member(REFERENCE)])
            .expect("valid line")
    }

    fn draft(kinds: &[StepKind]) -> Document {
        Document::draft(DocumentKind::General, line(kinds))
    }

    fn submitted(kinds: &[StepKind]) -> Document {
        let mut doc = draft(kinds);
        applied(doc.submit(member(DRAFTER), ts(1_000)));
        doc
    }

    fn applied(result: DomainResult<Transition>) -> Vec<DocumentEvent> {
        match result {
            Ok(Transition::Applied(events)) => events,
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    /// `restore`에 넣을 원시 값. 손상된 상태를 만들기 위해 필드를 직접 고친다.
    #[derive(Debug, Clone)]
    struct Raw {
        kind: DocumentKind,
        status: DocumentStatus,
        drafter: MemberId,
        steps: Vec<Step>,
        references: Vec<MemberId>,
        submitted_at: Option<Timestamp>,
        completed_at: Option<Timestamp>,
    }

    impl Raw {
        fn of(doc: &Document) -> Self {
            Self {
                kind: doc.kind(),
                status: doc.status(),
                drafter: doc.drafter(),
                steps: doc.steps().to_vec(),
                references: doc.references().to_vec(),
                submitted_at: doc.submitted_at(),
                completed_at: doc.completed_at(),
            }
        }

        fn restore(self) -> DomainResult<Document> {
            Document::restore(
                self.kind,
                self.status,
                self.drafter,
                self.steps,
                self.references,
                self.submitted_at,
                self.completed_at,
            )
        }
    }

    fn assert_restorable(doc: &Document) {
        assert_eq!(Raw::of(doc).restore().as_ref(), Ok(doc));
    }

    /// 실패한 전이는 정해진 오류를 내고 문서를 전혀 바꾸지 않는다.
    fn assert_refused(
        doc: &mut Document,
        expected: DomainError,
        op: impl FnOnce(&mut Document) -> DomainResult<Transition>,
    ) {
        let before = doc.clone();
        assert_eq!(op(doc), Err(expected));
        assert_eq!(
            *doc, before,
            "failed transition must not change the document"
        );
    }

    /// 반복 요청은 `NoOp`이고 문서를 전혀 바꾸지 않는다.
    fn assert_noop(doc: &mut Document, op: impl FnOnce(&mut Document) -> DomainResult<Transition>) {
        let before = doc.clone();
        let result = op(doc).expect("repeat must succeed");
        assert!(result.is_noop());
        assert!(result.events().is_empty());
        assert_eq!(*doc, before, "no-op must not change the document");
    }

    fn statuses(doc: &Document) -> Vec<StepStatus> {
        doc.steps().iter().map(|step| step.status).collect()
    }

    // ---- 열거형 ----

    #[test]
    fn enum_strings_round_trip_and_match_serde() {
        macro_rules! check {
            ($($name:ident),+) => {$(
                for &value in $name::ALL {
                    assert_eq!($name::parse(value.as_str()), Ok(value));
                    let json = serde_json::to_string(&value).expect("serialize");
                    assert_eq!(json, format!("\"{}\"", value.as_str()));
                    assert_eq!(serde_json::from_str::<$name>(&json).expect("deserialize"), value);
                }
                assert!($name::parse("").is_err());
                assert!($name::parse("Approved").is_err());
            )+};
        }
        check!(DocumentStatus, StepKind, StepStatus, DocumentKind, Mailbox);
    }

    #[test]
    fn enum_wire_strings_are_fixed() {
        fn strings<T: Copy>(all: &[T], as_str: fn(T) -> &'static str) -> Vec<&'static str> {
            all.iter().map(|value| as_str(*value)).collect()
        }
        assert_eq!(
            strings(DocumentStatus::ALL, DocumentStatus::as_str),
            ["draft", "in_progress", "approved", "rejected", "withdrawn"]
        );
        assert_eq!(
            strings(StepKind::ALL, StepKind::as_str),
            ["approve", "agree"]
        );
        assert_eq!(
            strings(StepStatus::ALL, StepStatus::as_str),
            ["waiting", "pending", "approved", "rejected"]
        );
        assert_eq!(
            strings(DocumentKind::ALL, DocumentKind::as_str),
            ["general", "leave", "attendance_correction"]
        );
        assert_eq!(
            strings(Mailbox::ALL, Mailbox::as_str),
            ["todo", "upcoming", "drafted", "done", "reference"]
        );
        assert_eq!(
            strings(EventKind::ALL, EventKind::as_str),
            [
                "submitted",
                "approved",
                "completed",
                "rejected",
                "withdrawn"
            ]
        );
        // 선언 순서 비교는 매크로 밖에서 파생한 것이 그대로 남아 있어야 한다.
        assert!(DocumentStatus::Draft < DocumentStatus::Withdrawn);
    }

    #[test]
    fn only_final_statuses_are_closed() {
        let closed: Vec<_> = DocumentStatus::ALL
            .iter()
            .copied()
            .filter(|status| status.is_closed())
            .collect();
        assert_eq!(
            closed,
            [
                DocumentStatus::Approved,
                DocumentStatus::Rejected,
                DocumentStatus::Withdrawn
            ]
        );
    }

    // ---- 결재선 ----

    #[test]
    fn line_rejects_empty_steps() {
        assert_eq!(
            ApprovalLine::new(member(DRAFTER), vec![], vec![]),
            Err(invalid("steps", ValidationReason::Empty))
        );
    }

    #[test]
    fn line_accepts_exactly_max_steps_and_rejects_one_more() {
        let at_limit = line_steps(&[Approve; MAX_STEPS]);
        assert!(ApprovalLine::new(member(DRAFTER), at_limit, vec![]).is_ok());

        let over_limit = line_steps(&[Approve; MAX_STEPS + 1]);
        assert_eq!(
            ApprovalLine::new(member(DRAFTER), over_limit, vec![]),
            Err(invalid(
                "steps",
                ValidationReason::TooLong { max: MAX_STEPS }
            ))
        );
    }

    #[test]
    fn line_accepts_exactly_max_references_and_rejects_one_more() {
        let references = |count: u128| (0..count).map(|n| member(1_000 + n)).collect::<Vec<_>>();
        let steps = || line_steps(&[Approve]);
        assert!(ApprovalLine::new(member(DRAFTER), steps(), references(30)).is_ok());
        assert_eq!(
            ApprovalLine::new(member(DRAFTER), steps(), references(31)),
            Err(invalid(
                "references",
                ValidationReason::TooLong {
                    max: MAX_REFERENCES
                }
            ))
        );
    }

    #[test]
    fn line_rejects_drafter_as_approver() {
        let steps = vec![
            LineStep {
                approver: member(DRAFTER),
                kind: Agree,
            },
            LineStep {
                approver: approver(1),
                kind: Approve,
            },
        ];
        assert_eq!(
            ApprovalLine::new(member(DRAFTER), steps, vec![]),
            Err(invalid("steps", ValidationReason::Inconsistent))
        );
    }

    #[test]
    fn line_rejects_same_approver_twice() {
        let steps = vec![
            LineStep {
                approver: approver(1),
                kind: Agree,
            },
            LineStep {
                approver: approver(2),
                kind: Approve,
            },
            LineStep {
                approver: approver(1),
                kind: Approve,
            },
        ];
        assert_eq!(
            ApprovalLine::new(member(DRAFTER), steps, vec![]),
            Err(invalid("steps", ValidationReason::Duplicate))
        );
    }

    #[test]
    fn line_rejects_agree_as_last_step() {
        for kinds in [&[Agree][..], &[Approve, Agree][..]] {
            assert_eq!(
                ApprovalLine::new(member(DRAFTER), line_steps(kinds), vec![]),
                Err(invalid("steps", ValidationReason::Inconsistent))
            );
        }
    }

    #[test]
    fn line_rejects_duplicate_references() {
        let references = vec![member(REFERENCE), member(REFERENCE)];
        assert_eq!(
            ApprovalLine::new(member(DRAFTER), line_steps(&[Approve]), references),
            Err(invalid("references", ValidationReason::Duplicate))
        );
    }

    #[test]
    fn line_rejects_reference_who_is_approver_or_drafter() {
        for overlapping in [approver(1), member(DRAFTER)] {
            assert_eq!(
                ApprovalLine::new(member(DRAFTER), line_steps(&[Approve]), vec![overlapping]),
                Err(invalid("references", ValidationReason::Inconsistent))
            );
        }
    }

    // ---- 정상 흐름 ----

    #[test]
    fn draft_starts_with_all_steps_waiting() {
        let doc = draft(&[Agree, Approve]);
        assert_eq!(doc.status(), DocumentStatus::Draft);
        assert_eq!(statuses(&doc), [StepStatus::Waiting; 2]);
        assert_eq!(
            doc.steps().iter().map(|s| s.order).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(doc.current_approver(), None);
        assert_eq!(doc.acted_count(), 0);
        assert_eq!((doc.submitted_at(), doc.completed_at()), (None, None));
        assert_restorable(&doc);
    }

    #[test]
    fn submit_makes_first_step_pending() {
        let mut doc = draft(&[Approve, Approve]);
        let events = applied(doc.submit(member(DRAFTER), ts(1_000)));
        assert_eq!(
            events,
            [DocumentEvent::Submitted {
                first_approver: approver(1)
            }]
        );
        assert_eq!(doc.status(), DocumentStatus::InProgress);
        assert_eq!(statuses(&doc), [StepStatus::Pending, StepStatus::Waiting]);
        assert_eq!(doc.current_approver(), Some(approver(1)));
        assert_eq!(doc.submitted_at(), Some(ts(1_000)));
        assert_restorable(&doc);
    }

    #[test]
    fn single_step_document_completes_on_first_approve() {
        let mut doc = submitted(&[Approve]);
        let events = applied(doc.approve(approver(1), ts(2_000), Some(comment("확인했습니다"))));
        assert_eq!(
            events,
            [
                DocumentEvent::StepApproved {
                    order: 1,
                    by: approver(1),
                    next_approver: None
                },
                DocumentEvent::Completed,
            ]
        );
        assert_eq!(doc.status(), DocumentStatus::Approved);
        assert_eq!(doc.completed_at(), Some(ts(2_000)));
        assert_eq!(doc.current_approver(), None);
        let step = doc.steps().first().expect("one step");
        assert_eq!(step.acted_at, Some(ts(2_000)));
        assert_eq!(step.comment, Some(comment("확인했습니다")));
        assert_restorable(&doc);
    }

    #[test]
    fn three_step_document_is_approved_in_order() {
        let mut doc = submitted(&[Approve, Approve, Approve]);

        let first = applied(doc.approve(approver(1), ts(2_000), None));
        assert_eq!(
            first,
            [DocumentEvent::StepApproved {
                order: 1,
                by: approver(1),
                next_approver: Some(approver(2))
            }]
        );
        assert_eq!(doc.status(), DocumentStatus::InProgress);
        assert_eq!(doc.current_approver(), Some(approver(2)));
        assert_eq!(doc.acted_count(), 1);
        assert_restorable(&doc);

        let second = applied(doc.approve(approver(2), ts(3_000), None));
        assert_eq!(
            second,
            [DocumentEvent::StepApproved {
                order: 2,
                by: approver(2),
                next_approver: Some(approver(3))
            }]
        );
        assert_eq!(
            statuses(&doc),
            [
                StepStatus::Approved,
                StepStatus::Approved,
                StepStatus::Pending
            ]
        );
        assert_restorable(&doc);

        let third = applied(doc.approve(approver(3), ts(4_000), None));
        assert_eq!(
            third,
            [
                DocumentEvent::StepApproved {
                    order: 3,
                    by: approver(3),
                    next_approver: None
                },
                DocumentEvent::Completed,
            ]
        );
        assert_eq!(doc.status(), DocumentStatus::Approved);
        assert_eq!(doc.completed_at(), Some(ts(4_000)));
        assert_eq!(doc.acted_count(), 3);
        assert_restorable(&doc);
    }

    #[test]
    fn agree_steps_advance_like_approve_steps() {
        let mut doc = submitted(&[Agree, Approve, Agree, Approve]);
        for order in 1..=3u128 {
            let events = applied(doc.approve(approver(order), ts(2_000), None));
            assert_eq!(events.len(), 1, "agreement must not complete the document");
            assert_eq!(doc.status(), DocumentStatus::InProgress);
        }
        let events = applied(doc.approve(approver(4), ts(2_000), None));
        assert_eq!(events.last(), Some(&DocumentEvent::Completed));
        assert_eq!(doc.status(), DocumentStatus::Approved);
    }

    #[test]
    fn agree_step_can_reject() {
        let mut doc = submitted(&[Agree, Approve]);
        let events = applied(doc.reject(approver(1), ts(2_000), reason("예산 초과")));
        assert_eq!(
            events,
            [DocumentEvent::Rejected {
                order: 1,
                by: approver(1)
            }]
        );
        assert_eq!(doc.status(), DocumentStatus::Rejected);
    }

    #[test]
    fn reject_closes_document_and_leaves_later_steps_waiting() {
        let mut doc = submitted(&[Approve, Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        let events = applied(doc.reject(approver(2), ts(3_000), reason("근거 부족")));
        assert_eq!(
            events,
            [DocumentEvent::Rejected {
                order: 2,
                by: approver(2)
            }]
        );
        assert_eq!(doc.status(), DocumentStatus::Rejected);
        assert_eq!(
            statuses(&doc),
            [
                StepStatus::Approved,
                StepStatus::Rejected,
                StepStatus::Waiting
            ]
        );
        assert_eq!(doc.completed_at(), Some(ts(3_000)));
        assert_eq!(doc.current_approver(), None);
        assert_eq!(doc.acted_count(), 2);
        let rejected = doc.steps().get(1).expect("second step");
        assert_eq!(rejected.comment, Some(comment("근거 부족")));
        assert_restorable(&doc);
    }

    #[test]
    fn withdraw_before_anyone_acts_closes_document() {
        let mut doc = submitted(&[Approve, Approve]);
        let events = applied(doc.withdraw(member(DRAFTER), ts(1_500)));
        assert_eq!(events, [DocumentEvent::Withdrawn]);
        assert_eq!(doc.status(), DocumentStatus::Withdrawn);
        assert_eq!(statuses(&doc), [StepStatus::Waiting; 2]);
        assert_eq!(doc.completed_at(), Some(ts(1_500)));
        assert_eq!(doc.current_approver(), None);
        assert_restorable(&doc);
    }

    #[test]
    fn replace_line_swaps_steps_and_references_while_draft() {
        let mut doc = draft(&[Approve]);
        let new_line = ApprovalLine::new(
            member(DRAFTER),
            vec![
                LineStep {
                    approver: approver(7),
                    kind: Agree,
                },
                LineStep {
                    approver: approver(8),
                    kind: Approve,
                },
            ],
            vec![],
        )
        .expect("valid line");
        assert_eq!(
            doc.replace_line(member(DRAFTER), new_line.clone()),
            Ok(Transition::Applied(vec![]))
        );
        assert_eq!(
            doc.steps().iter().map(|s| s.approver).collect::<Vec<_>>(),
            [approver(7), approver(8)]
        );
        assert!(doc.references().is_empty());
        assert_restorable(&doc);

        assert_noop(&mut doc, |d| d.replace_line(member(DRAFTER), new_line));
    }

    #[test]
    fn clock_going_backwards_does_not_reorder_recorded_times() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(500), None));
        applied(doc.approve(approver(2), ts(100), None));
        let times: Vec<_> = doc.steps().iter().map(|s| s.acted_at).collect();
        assert_eq!(times, [Some(ts(1_000)), Some(ts(1_000))]);
        assert_eq!(doc.completed_at(), Some(ts(1_000)));
        assert_restorable(&doc);

        let mut doc = submitted(&[Approve]);
        applied(doc.withdraw(member(DRAFTER), ts(10)));
        assert_eq!(doc.completed_at(), Some(ts(1_000)));
        assert_restorable(&doc);
    }

    // ---- 권한 없는 행위자 ----

    #[test]
    fn non_participant_cannot_do_anything() {
        for mut doc in [draft(&[Approve]), submitted(&[Approve])] {
            let outsider = member(OUTSIDER);
            assert_refused(&mut doc, DomainError::Forbidden, |d| {
                d.submit(outsider, ts(2_000))
            });
            assert_refused(&mut doc, DomainError::Forbidden, |d| {
                d.approve(outsider, ts(2_000), None)
            });
            assert_refused(&mut doc, DomainError::Forbidden, |d| {
                d.reject(outsider, ts(2_000), reason("사유"))
            });
            assert_refused(&mut doc, DomainError::Forbidden, |d| {
                d.withdraw(outsider, ts(2_000))
            });
            assert_refused(&mut doc, DomainError::Forbidden, |d| {
                d.replace_line(outsider, line(&[Approve, Approve]))
            });
        }
    }

    #[test]
    fn drafter_cannot_approve_or_reject_own_document() {
        let mut doc = submitted(&[Approve]);
        let drafter = member(DRAFTER);
        assert_refused(&mut doc, DomainError::Forbidden, |d| {
            d.approve(drafter, ts(2_000), None)
        });
        assert_refused(&mut doc, DomainError::Forbidden, |d| {
            d.reject(drafter, ts(2_000), reason("사유"))
        });
    }

    #[test]
    fn reference_cannot_approve_or_reject() {
        let mut doc = submitted(&[Approve]);
        let reference = member(REFERENCE);
        assert_refused(&mut doc, DomainError::Forbidden, |d| {
            d.approve(reference, ts(2_000), None)
        });
        assert_refused(&mut doc, DomainError::Forbidden, |d| {
            d.reject(reference, ts(2_000), reason("사유"))
        });
    }

    #[test]
    fn admin_not_on_line_can_view_but_not_act() {
        let mut doc = submitted(&[Approve]);
        for role in [Role::Admin, Role::Owner] {
            let admin = actor(member(OUTSIDER), role);
            assert_eq!(view(&admin, &doc), Ok(()));
            assert_refused(&mut doc, DomainError::Forbidden, |d| {
                d.approve(admin.member_id, ts(2_000), None)
            });
            assert_refused(&mut doc, DomainError::Forbidden, |d| {
                d.reject(admin.member_id, ts(2_000), reason("사유"))
            });
            assert_refused(&mut doc, DomainError::Forbidden, |d| {
                d.withdraw(admin.member_id, ts(2_000))
            });
        }
    }

    #[test]
    fn approvers_cannot_submit_withdraw_or_edit_line() {
        let mut doc = submitted(&[Approve]);
        assert_refused(&mut doc, DomainError::Forbidden, |d| {
            d.submit(approver(1), ts(2_000))
        });
        assert_refused(&mut doc, DomainError::Forbidden, |d| {
            d.withdraw(approver(1), ts(2_000))
        });
        assert_refused(&mut doc, DomainError::Forbidden, |d| {
            d.replace_line(approver(1), line(&[Approve]))
        });
    }

    // ---- 멱등성 ----

    #[test]
    fn double_submit_is_noop() {
        let mut doc = submitted(&[Approve, Approve]);
        assert_noop(&mut doc, |d| d.submit(member(DRAFTER), ts(9_000)));
        assert_eq!(doc.submitted_at(), Some(ts(1_000)));
    }

    #[test]
    fn submit_retry_after_a_step_was_approved_is_still_noop() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        assert_noop(&mut doc, |d| d.submit(member(DRAFTER), ts(9_000)));
    }

    #[test]
    fn double_approve_is_noop_at_every_position() {
        let mut doc = submitted(&[Approve, Agree, Approve]);
        for order in 1..=3u128 {
            applied(doc.approve(approver(order), ts(2_000), Some(comment("첫 의견"))));
            // 직전에 승인한 사람과, 그보다 앞서 승인한 사람 모두의 재시도.
            for earlier in 1..=order {
                assert_noop(&mut doc, |d| {
                    d.approve(approver(earlier), ts(9_000), Some(comment("다른 의견")))
                });
            }
        }
        assert_eq!(doc.status(), DocumentStatus::Approved);
        assert_eq!(doc.completed_at(), Some(ts(2_000)));
    }

    #[test]
    fn double_reject_is_noop_and_keeps_first_reason() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.reject(approver(1), ts(2_000), reason("첫 사유")));
        assert_noop(&mut doc, |d| {
            d.reject(approver(1), ts(9_000), reason("다른 사유"))
        });
        assert_eq!(
            doc.steps().first().and_then(|s| s.comment.clone()),
            Some(comment("첫 사유"))
        );
    }

    #[test]
    fn double_withdraw_is_noop() {
        let mut doc = submitted(&[Approve]);
        applied(doc.withdraw(member(DRAFTER), ts(2_000)));
        assert_noop(&mut doc, |d| d.withdraw(member(DRAFTER), ts(9_000)));
        assert_eq!(doc.completed_at(), Some(ts(2_000)));
    }

    // ---- 순서 뒤집힘 ----

    #[test]
    fn later_approver_cannot_act_before_their_turn() {
        let mut doc = submitted(&[Approve, Approve, Approve]);
        for early in [approver(2), approver(3)] {
            assert_refused(&mut doc, NOT_CURRENT, |d| d.approve(early, ts(2_000), None));
            assert_refused(&mut doc, NOT_CURRENT, |d| {
                d.reject(early, ts(2_000), reason("사유"))
            });
        }
    }

    #[test]
    fn approve_and_reject_on_draft_are_refused() {
        let mut doc = draft(&[Approve, Approve]);
        for on_line in [approver(1), approver(2)] {
            assert_refused(&mut doc, NOT_CURRENT, |d| {
                d.approve(on_line, ts(2_000), None)
            });
            assert_refused(&mut doc, NOT_CURRENT, |d| {
                d.reject(on_line, ts(2_000), reason("사유"))
            });
        }
    }

    #[test]
    fn approve_after_reject_is_refused_for_everyone_on_the_line() {
        let mut doc = submitted(&[Approve, Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        applied(doc.reject(approver(2), ts(3_000), reason("사유")));
        // 앞서 승인한 사람, 반려한 본인, 차례가 오지 않은 사람.
        for order in 1..=3u128 {
            assert_refused(&mut doc, CLOSED, |d| {
                d.approve(approver(order), ts(4_000), None)
            });
        }
    }

    #[test]
    fn reject_after_own_approval_is_refused() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        assert_refused(&mut doc, NOT_CURRENT, |d| {
            d.reject(approver(1), ts(3_000), reason("번복"))
        });
    }

    #[test]
    fn reject_after_final_approval_is_refused() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        applied(doc.approve(approver(2), ts(3_000), None));
        for order in 1..=2u128 {
            assert_refused(&mut doc, CLOSED, |d| {
                d.reject(approver(order), ts(4_000), reason("번복"))
            });
        }
    }

    #[test]
    fn reject_by_others_after_rejection_is_refused() {
        let mut doc = submitted(&[Approve, Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        applied(doc.reject(approver(2), ts(3_000), reason("사유")));
        for order in [1u128, 3] {
            assert_refused(&mut doc, CLOSED, |d| {
                d.reject(approver(order), ts(4_000), reason("사유"))
            });
        }
    }

    #[test]
    fn withdraw_after_first_approval_is_too_late() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        assert_refused(&mut doc, TOO_LATE, |d| {
            d.withdraw(member(DRAFTER), ts(3_000))
        });
    }

    #[test]
    fn withdraw_after_completion_or_rejection_is_refused() {
        let mut approved = submitted(&[Approve]);
        applied(approved.approve(approver(1), ts(2_000), None));
        assert_refused(&mut approved, CLOSED, |d| {
            d.withdraw(member(DRAFTER), ts(3_000))
        });

        let mut rejected = submitted(&[Approve]);
        applied(rejected.reject(approver(1), ts(2_000), reason("사유")));
        assert_refused(&mut rejected, CLOSED, |d| {
            d.withdraw(member(DRAFTER), ts(3_000))
        });
    }

    #[test]
    fn withdraw_on_draft_is_refused() {
        let mut doc = draft(&[Approve]);
        assert_refused(&mut doc, NOT_EDITABLE, |d| {
            d.withdraw(member(DRAFTER), ts(2_000))
        });
    }

    #[test]
    fn approve_and_reject_after_withdraw_are_refused() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.withdraw(member(DRAFTER), ts(2_000)));
        for order in 1..=2u128 {
            assert_refused(&mut doc, CLOSED, |d| {
                d.approve(approver(order), ts(3_000), None)
            });
            assert_refused(&mut doc, CLOSED, |d| {
                d.reject(approver(order), ts(3_000), reason("사유"))
            });
        }
    }

    #[test]
    fn submit_after_document_closed_is_refused() {
        let mut withdrawn = submitted(&[Approve]);
        applied(withdrawn.withdraw(member(DRAFTER), ts(2_000)));
        assert_refused(&mut withdrawn, CLOSED, |d| {
            d.submit(member(DRAFTER), ts(3_000))
        });

        let mut approved = submitted(&[Approve]);
        applied(approved.approve(approver(1), ts(2_000), None));
        assert_refused(&mut approved, CLOSED, |d| {
            d.submit(member(DRAFTER), ts(3_000))
        });

        let mut rejected = submitted(&[Approve]);
        applied(rejected.reject(approver(1), ts(2_000), reason("사유")));
        assert_refused(&mut rejected, CLOSED, |d| {
            d.submit(member(DRAFTER), ts(3_000))
        });
    }

    #[test]
    fn replace_line_after_submit_is_refused() {
        let mut in_progress = submitted(&[Approve]);
        assert_refused(&mut in_progress, NOT_EDITABLE, |d| {
            d.replace_line(member(DRAFTER), line(&[Approve, Approve]))
        });

        let mut withdrawn = submitted(&[Approve]);
        applied(withdrawn.withdraw(member(DRAFTER), ts(2_000)));
        assert_refused(&mut withdrawn, NOT_EDITABLE, |d| {
            d.replace_line(member(DRAFTER), line(&[Approve, Approve]))
        });
    }

    #[test]
    fn replace_line_with_another_drafters_line_is_refused() {
        let mut doc = draft(&[Approve]);
        let foreign = ApprovalLine::new(member(OUTSIDER), line_steps(&[Approve]), vec![])
            .expect("valid line");
        assert_refused(
            &mut doc,
            invalid("steps", ValidationReason::Inconsistent),
            |d| d.replace_line(member(DRAFTER), foreign),
        );
    }

    // ---- 열람 ----

    #[test]
    fn participant_role_reports_each_position() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        assert_eq!(
            doc.participant_role(member(DRAFTER)),
            Some(Participant::Drafter)
        );
        assert_eq!(
            doc.participant_role(approver(1)),
            Some(Participant::Approver {
                order: 1,
                status: StepStatus::Approved
            })
        );
        assert_eq!(
            doc.participant_role(approver(2)),
            Some(Participant::Approver {
                order: 2,
                status: StepStatus::Pending
            })
        );
        assert_eq!(
            doc.participant_role(member(REFERENCE)),
            Some(Participant::Reference)
        );
        assert_eq!(doc.participant_role(member(OUTSIDER)), None);
    }

    #[test]
    fn submitted_document_is_visible_to_participants_and_admins_only() {
        let doc = submitted(&[Approve, Approve]);
        // 차례가 오지 않은 결재자(2단계)도 볼 수 있다.
        for visible in [member(DRAFTER), approver(1), approver(2), member(REFERENCE)] {
            assert_eq!(view(&actor(visible, Role::Member), &doc), Ok(()));
        }
        assert_eq!(view(&actor(member(OUTSIDER), Role::Admin), &doc), Ok(()));
        assert_eq!(view(&actor(member(OUTSIDER), Role::Owner), &doc), Ok(()));
        assert_eq!(
            view(&actor(member(OUTSIDER), Role::Member), &doc),
            Err(DomainError::NotFound)
        );
    }

    #[test]
    fn manager_of_some_department_gets_no_extra_visibility() {
        let doc = submitted(&[Approve]);
        let mut manager = actor(member(OUTSIDER), Role::Member);
        manager
            .managed_departments
            .insert(crate::id::DepartmentId::new());
        assert_eq!(view(&manager, &doc), Err(DomainError::NotFound));
    }

    #[test]
    fn draft_is_visible_to_drafter_only() {
        let doc = draft(&[Approve]);
        assert_eq!(view(&actor(member(DRAFTER), Role::Member), &doc), Ok(()));
        let hidden_from = [
            actor(approver(1), Role::Member),
            actor(member(REFERENCE), Role::Member),
            actor(member(OUTSIDER), Role::Admin),
            actor(member(OUTSIDER), Role::Owner),
            actor(approver(1), Role::Admin),
        ];
        for other in hidden_from {
            assert_eq!(view(&other, &doc), Err(DomainError::NotFound));
        }
    }

    #[test]
    fn closed_documents_stay_visible_to_participants() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.reject(approver(1), ts(2_000), reason("사유")));
        for visible in [member(DRAFTER), approver(1), approver(2), member(REFERENCE)] {
            assert_eq!(view(&actor(visible, Role::Member), &doc), Ok(()));
        }
        assert_eq!(
            view(&actor(member(OUTSIDER), Role::Member), &doc),
            Err(DomainError::NotFound)
        );
    }

    // ---- 손상되거나 악의적인 저장 값 ----

    fn two_step_in_progress() -> Raw {
        Raw::of(&submitted(&[Approve, Approve]))
    }

    fn step_mut(raw: &mut Raw, index: usize) -> &mut Step {
        raw.steps.get_mut(index).expect("step exists")
    }

    #[test]
    fn restore_accepts_every_reachable_state() {
        let mut doc = draft(&[Agree, Approve]);
        assert_restorable(&doc);
        applied(doc.submit(member(DRAFTER), ts(1_000)));
        assert_restorable(&doc);
        applied(doc.approve(approver(1), ts(2_000), Some(comment(""))));
        assert_restorable(&doc);
        applied(doc.approve(approver(2), ts(3_000), None));
        assert_restorable(&doc);
    }

    #[test]
    fn restore_rejects_two_pending_steps() {
        let mut raw = two_step_in_progress();
        step_mut(&mut raw, 1).status = StepStatus::Pending;
        assert_eq!(
            raw.restore(),
            Err(invalid("steps", ValidationReason::Inconsistent))
        );
    }

    #[test]
    fn restore_rejects_in_progress_without_pending_step() {
        let mut raw = two_step_in_progress();
        step_mut(&mut raw, 0).status = StepStatus::Waiting;
        assert!(raw.restore().is_err());
    }

    #[test]
    fn restore_rejects_pending_step_after_waiting_step() {
        let mut raw = two_step_in_progress();
        step_mut(&mut raw, 0).status = StepStatus::Waiting;
        step_mut(&mut raw, 1).status = StepStatus::Pending;
        assert!(raw.restore().is_err());
    }

    #[test]
    fn restore_rejects_approved_document_with_waiting_step() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        applied(doc.approve(approver(2), ts(3_000), None));
        let mut raw = Raw::of(&doc);
        let last = step_mut(&mut raw, 1);
        last.status = StepStatus::Waiting;
        last.acted_at = None;
        assert!(raw.restore().is_err());
    }

    #[test]
    fn restore_rejects_approved_document_claimed_from_in_progress_steps() {
        let mut raw = two_step_in_progress();
        raw.status = DocumentStatus::Approved;
        raw.completed_at = Some(ts(2_000));
        assert!(raw.restore().is_err());
    }

    #[test]
    fn restore_rejects_gaps_and_duplicates_in_order() {
        for orders in [[1u8, 3], [1, 1], [0, 1], [2, 1], [2, 3]] {
            let mut raw = two_step_in_progress();
            for (step, order) in raw.steps.iter_mut().zip(orders) {
                step.order = order;
            }
            assert_eq!(
                raw.restore(),
                Err(invalid("steps", ValidationReason::Inconsistent)),
                "orders {orders:?}"
            );
        }
    }

    #[test]
    fn restore_rejects_more_than_max_steps() {
        let mut raw = Raw::of(&submitted(&[Approve; MAX_STEPS]));
        let extra = Step {
            order: 11,
            approver: approver(11),
            kind: Approve,
            status: StepStatus::Waiting,
            acted_at: None,
            comment: None,
        };
        raw.steps.push(extra);
        assert_eq!(
            raw.restore(),
            Err(invalid(
                "steps",
                ValidationReason::TooLong { max: MAX_STEPS }
            ))
        );
    }

    #[test]
    fn restore_rejects_rejected_document_without_rejected_step() {
        let mut raw = two_step_in_progress();
        raw.status = DocumentStatus::Rejected;
        raw.completed_at = Some(ts(2_000));
        assert!(raw.clone().restore().is_err());

        for step in &mut raw.steps {
            step.status = StepStatus::Waiting;
        }
        assert!(raw.restore().is_err());
    }

    #[test]
    fn restore_rejects_two_rejected_steps_and_rejection_without_reason() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.reject(approver(1), ts(2_000), reason("사유")));
        let rejected = Raw::of(&doc);

        let mut two_rejections = rejected.clone();
        let template = two_rejections.steps.first().expect("first step").clone();
        let second = step_mut(&mut two_rejections, 1);
        second.status = StepStatus::Rejected;
        second.acted_at = template.acted_at;
        second.comment = template.comment;
        assert!(two_rejections.restore().is_err());

        for blank in [None, Some(comment(""))] {
            let mut no_reason = rejected.clone();
            step_mut(&mut no_reason, 0).comment = blank;
            assert!(no_reason.restore().is_err());
        }
    }

    #[test]
    fn restore_rejects_in_progress_with_zero_steps() {
        let mut raw = two_step_in_progress();
        raw.steps.clear();
        assert_eq!(
            raw.restore(),
            Err(invalid("steps", ValidationReason::Empty))
        );
    }

    #[test]
    fn restore_rejects_acted_steps_on_draft_or_withdrawn_documents() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        let acted = Raw::of(&doc);

        let mut as_draft = acted.clone();
        as_draft.status = DocumentStatus::Draft;
        as_draft.submitted_at = None;
        assert!(as_draft.restore().is_err());

        let mut as_withdrawn = acted;
        as_withdrawn.status = DocumentStatus::Withdrawn;
        as_withdrawn.completed_at = Some(ts(3_000));
        assert!(as_withdrawn.restore().is_err());
    }

    #[test]
    fn restore_rejects_unacted_steps_carrying_results() {
        let mut with_time = two_step_in_progress();
        step_mut(&mut with_time, 1).acted_at = Some(ts(2_000));
        assert!(with_time.restore().is_err());

        let mut with_comment = two_step_in_progress();
        step_mut(&mut with_comment, 0).comment = Some(comment("미리 쓴 의견"));
        assert!(with_comment.restore().is_err());
    }

    #[test]
    fn restore_rejects_approved_step_without_time() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        let mut raw = Raw::of(&doc);
        step_mut(&mut raw, 0).acted_at = None;
        assert!(raw.restore().is_err());
    }

    #[test]
    fn restore_rejects_inconsistent_timestamps() {
        let field = |name| Err(invalid(name, ValidationReason::Inconsistent));

        let mut draft_with_submit_time = Raw::of(&draft(&[Approve]));
        draft_with_submit_time.submitted_at = Some(ts(1_000));
        assert_eq!(draft_with_submit_time.restore(), field("submitted_at"));

        let mut in_progress_without_submit_time = two_step_in_progress();
        in_progress_without_submit_time.submitted_at = None;
        assert_eq!(
            in_progress_without_submit_time.restore(),
            field("submitted_at")
        );

        let mut in_progress_with_completion = two_step_in_progress();
        in_progress_with_completion.completed_at = Some(ts(2_000));
        assert_eq!(in_progress_with_completion.restore(), field("completed_at"));

        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        applied(doc.approve(approver(2), ts(3_000), None));
        let approved = Raw::of(&doc);

        let mut without_completion = approved.clone();
        without_completion.completed_at = None;
        assert_eq!(without_completion.restore(), field("completed_at"));

        let mut completion_mismatch = approved.clone();
        completion_mismatch.completed_at = Some(ts(9_000));
        assert_eq!(completion_mismatch.restore(), field("completed_at"));

        let mut acted_before_submit = approved.clone();
        acted_before_submit.submitted_at = Some(ts(2_500));
        assert_eq!(acted_before_submit.restore(), field("steps"));

        let mut steps_out_of_time_order = approved;
        step_mut(&mut steps_out_of_time_order, 0).acted_at = Some(ts(3_500));
        assert_eq!(steps_out_of_time_order.restore(), field("steps"));

        let mut withdrawn = submitted(&[Approve]);
        applied(withdrawn.withdraw(member(DRAFTER), ts(2_000)));
        let mut withdrawn_before_submit = Raw::of(&withdrawn);
        withdrawn_before_submit.completed_at = Some(ts(500));
        assert_eq!(withdrawn_before_submit.restore(), field("completed_at"));
    }

    #[test]
    fn restore_rejects_invalid_lines() {
        let mut drafter_on_line = two_step_in_progress();
        drafter_on_line.drafter = approver(2);
        assert!(drafter_on_line.restore().is_err());

        let mut duplicate_approver = two_step_in_progress();
        step_mut(&mut duplicate_approver, 1).approver = approver(1);
        assert!(duplicate_approver.restore().is_err());

        let mut last_agree = two_step_in_progress();
        step_mut(&mut last_agree, 1).kind = Agree;
        assert!(last_agree.restore().is_err());

        let mut reference_on_line = two_step_in_progress();
        reference_on_line.references.push(approver(1));
        assert!(reference_on_line.restore().is_err());

        let mut too_many_references = two_step_in_progress();
        too_many_references.references = (0..31).map(|n| member(1_000 + n)).collect();
        assert!(too_many_references.restore().is_err());
    }

    // ---- 문서 번호 ----

    #[test]
    fn document_number_formats_with_zero_padding() {
        assert_eq!(
            DocumentNumber::format(2026, 123).as_deref(),
            Ok("2026-000123")
        );
        assert_eq!(
            DocumentNumber::format(2000, 1).as_deref(),
            Ok("2000-000001")
        );
        assert_eq!(
            DocumentNumber::format(9999, 999_999).as_deref(),
            Ok("9999-999999")
        );
    }

    #[test]
    fn document_number_rejects_out_of_range_parts() {
        let out_of_range = Err(invalid("document_number", ValidationReason::OutOfRange));
        for (year, seq) in [
            (2026, 0),
            (2026, 1_000_000),
            (1999, 1),
            (10_000, 1),
            (-2026, 1),
        ] {
            assert_eq!(DocumentNumber::new(year, seq), out_of_range, "{year}-{seq}");
            assert!(DocumentNumber::format(year, seq).is_err());
        }
    }

    #[test]
    fn document_number_parse_is_strict() {
        let parsed = DocumentNumber::parse("2026-000123").expect("valid number");
        assert_eq!((parsed.year(), parsed.seq()), (2026, 123));

        let malformed = [
            "",
            "2026",
            "2026-",
            "2026-123",
            "2026-0000123",
            "026-000123",
            "02026-000123",
            "2026_000123",
            "2026-000123 ",
            " 2026-000123",
            "2026-00012a",
            "2026-+00123",
            "-026-000123",
            "2026-000-23",
            "２０２６-000123",
            "2026-00012３",
        ];
        for value in malformed {
            assert_eq!(
                DocumentNumber::parse(value),
                Err(invalid("document_number", ValidationReason::InvalidFormat)),
                "{value:?}"
            );
        }
        for value in ["2026-000000", "1999-000001"] {
            assert_eq!(
                DocumentNumber::parse(value),
                Err(invalid("document_number", ValidationReason::OutOfRange)),
                "{value:?}"
            );
        }
    }

    // ---- 관리자의 대리 반려 ----

    #[test]
    fn force_reject_closes_at_the_pending_step_like_a_rejection() {
        let mut doc = submitted(&[Agree, Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        let events = applied(doc.force_reject(ts(3_000), reason("결재자 퇴사")));
        assert_eq!(
            events,
            vec![DocumentEvent::ForceRejected {
                order: 2,
                pending_approver: approver(2),
            }]
        );
        assert!(events[0].is_terminal());
        assert_eq!(events[0].kind(), EventKind::Rejected);
        assert_eq!(doc.status(), DocumentStatus::Rejected);
        assert_eq!(doc.completed_at(), Some(ts(3_000)));
        assert_eq!(doc.current_approver(), None);
        let statuses: Vec<_> = doc.steps().iter().map(|s| s.status).collect();
        assert_eq!(
            statuses,
            [
                StepStatus::Approved,
                StepStatus::Rejected,
                StepStatus::Waiting
            ]
        );
        let closed = &doc.steps()[1];
        assert_eq!(closed.acted_at, Some(ts(3_000)));
        assert_eq!(closed.comment, Some(comment("결재자 퇴사")));
        assert_eq!(doc.steps()[2].acted_at, None);
        // 저장했다 다시 읽어도 유효한 문서다.
        assert_eq!(Raw::of(&doc).restore(), Ok(doc));
    }

    #[test]
    fn force_reject_works_before_anyone_acted_and_at_the_last_step() {
        let mut fresh = submitted(&[Approve, Approve]);
        let events = applied(fresh.force_reject(ts(2_000), reason("사유")));
        assert_eq!(
            events,
            vec![DocumentEvent::ForceRejected {
                order: 1,
                pending_approver: approver(1),
            }]
        );
        assert_eq!(Raw::of(&fresh).restore(), Ok(fresh));

        let mut last = submitted(&[Approve, Approve]);
        applied(last.approve(approver(1), ts(2_000), None));
        let events = applied(last.force_reject(ts(3_000), reason("사유")));
        assert_eq!(
            events,
            vec![DocumentEvent::ForceRejected {
                order: 2,
                pending_approver: approver(2),
            }]
        );
        assert_eq!(Raw::of(&last).restore(), Ok(last));
    }

    #[test]
    fn force_reject_is_refused_on_drafts_and_documents_closed_otherwise() {
        let mut doc = draft(&[Approve]);
        let before = doc.clone();
        assert_eq!(doc.force_reject(ts(1), reason("사유")), Err(NOT_EDITABLE));
        assert_eq!(doc, before);

        let mut approved = submitted(&[Approve]);
        applied(approved.approve(approver(1), ts(2_000), None));
        let before = approved.clone();
        assert_eq!(
            approved.force_reject(ts(3_000), reason("사유")),
            Err(CLOSED)
        );
        assert_eq!(approved, before);

        let mut withdrawn = submitted(&[Approve]);
        applied(withdrawn.withdraw(member(DRAFTER), ts(2_000)));
        let before = withdrawn.clone();
        assert_eq!(
            withdrawn.force_reject(ts(3_000), reason("사유")),
            Err(CLOSED)
        );
        assert_eq!(withdrawn, before);
    }

    #[test]
    fn repeated_force_reject_is_a_noop_and_keeps_the_first_reason() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        applied(doc.force_reject(ts(3_000), reason("첫 사유")));
        let before = doc.clone();
        assert_eq!(
            doc.force_reject(ts(9_000), reason("다른 사유")),
            Ok(Transition::NoOp)
        );
        assert_eq!(doc, before);
        assert_eq!(doc.steps()[1].comment, Some(comment("첫 사유")));

        // 결재자가 직접 반려한 문서도 이미 원하는 상태이므로 반복과 같이 답한다.
        let mut rejected = submitted(&[Approve]);
        applied(rejected.reject(approver(1), ts(2_000), reason("직접 반려")));
        let before = rejected.clone();
        assert_eq!(
            rejected.force_reject(ts(3_000), reason("사유")),
            Ok(Transition::NoOp)
        );
        assert_eq!(rejected, before);
    }

    #[test]
    fn nobody_on_the_line_can_act_after_a_force_reject() {
        let mut doc = submitted(&[Agree, Approve]);
        applied(doc.approve(approver(1), ts(2_000), None));
        applied(doc.force_reject(ts(3_000), reason("사유")));
        let before = doc.clone();
        // 건너뛴 결재자의 뒤늦은 승인, 앞 단계의 재요청, 기안자의 회수·재상신 모두 거부된다.
        assert_eq!(doc.approve(approver(2), ts(4_000), None), Err(CLOSED));
        assert_eq!(doc.approve(approver(1), ts(4_000), None), Err(CLOSED));
        assert_eq!(doc.reject(approver(1), ts(4_000), reason("x")), Err(CLOSED));
        assert_eq!(doc.withdraw(member(DRAFTER), ts(4_000)), Err(CLOSED));
        assert_eq!(doc.submit(member(DRAFTER), ts(4_000)), Err(CLOSED));
        assert_eq!(doc, before);
        // 건너뛴 결재자 본인의 반려 재요청만은 "이미 반려됨"이 사실이므로 반복으로 답한다.
        assert_eq!(
            doc.reject(approver(2), ts(4_000), reason("x")),
            Ok(Transition::NoOp)
        );
        assert_eq!(doc, before);
    }

    #[test]
    fn force_reject_keeps_time_order_when_the_clock_goes_backwards() {
        let mut doc = submitted(&[Approve, Approve]);
        applied(doc.approve(approver(1), ts(5_000), None));
        applied(doc.force_reject(ts(10), reason("사유")));
        assert_eq!(doc.steps()[1].acted_at, Some(ts(5_000)));
        assert_eq!(doc.completed_at(), Some(ts(5_000)));
        assert_eq!(Raw::of(&doc).restore(), Ok(doc));
    }

    // ---- 속성 기반 ----

    #[derive(Debug, Clone, Copy)]
    enum Op {
        Submit,
        Approve,
        Reject,
        Withdraw,
        /// 관리자의 대리 반려. 행위자와 무관하다.
        ForceReject,
    }

    fn op_strategy() -> impl Strategy<Value = (Op, usize, i64)> {
        let op = prop_oneof![
            2 => Just(Op::Submit),
            6 => Just(Op::Approve),
            1 => Just(Op::Reject),
            1 => Just(Op::Withdraw),
            1 => Just(Op::ForceReject),
        ];
        // 시각은 가끔 뒤로 간다(음수 증분).
        (op, 0usize..8, -500i64..2_000)
    }

    fn kinds_strategy() -> impl Strategy<Value = Vec<StepKind>> {
        prop::collection::vec(prop_oneof![Just(Approve), Just(Agree)], 0..5).prop_map(
            |mut kinds| {
                kinds.push(Approve);
                kinds
            },
        )
    }

    proptest! {
        #[test]
        fn document_number_round_trips(year in 2000i32..=9999, seq in 1u32..=999_999) {
            let number = DocumentNumber::new(year, seq).expect("in range");
            let text = number.to_string();
            prop_assert_eq!(text.len(), 11);
            prop_assert_eq!(DocumentNumber::parse(&text), Ok(number));
        }

        #[test]
        fn document_number_parse_never_panics(value in "\\PC{0,16}") {
            if let Ok(number) = DocumentNumber::parse(&value) {
                prop_assert_eq!(number.to_string(), value);
            }
        }

        #[test]
        fn random_operation_sequences_keep_invariants(
            kinds in kinds_strategy(),
            ops in prop::collection::vec(op_strategy(), 0..=30),
        ) {
            let mut doc = draft(&kinds);
            // 기안자, 결재자 전원, 참조자, 외부인.
            let mut actors = vec![member(DRAFTER)];
            actors.extend(doc.steps().iter().map(|step| step.approver));
            actors.extend([member(REFERENCE), member(OUTSIDER)]);

            let mut clock = 1_000i64;
            let mut terminal_events = 0usize;
            for (op, actor_index, delta) in ops {
                clock += delta;
                let now = ts(clock);
                let by = *actors.get(actor_index % actors.len()).expect("non-empty actors");
                let before = doc.clone();
                let result = match op {
                    Op::Submit => doc.submit(by, now),
                    Op::Approve => doc.approve(by, now, None),
                    Op::Reject => doc.reject(by, now, reason("사유")),
                    Op::Withdraw => doc.withdraw(by, now),
                    Op::ForceReject => doc.force_reject(now, reason("대리 반려")),
                };
                let forced = matches!(op, Op::ForceReject);

                // (a) 어떤 전이 뒤에도 불변식이 성립한다.
                let restored = Raw::of(&doc).restore();
                prop_assert_eq!(restored.as_ref(), Ok(&doc));
                // (b) 종료된 문서는 다시 바뀌지 않는다.
                if before.status().is_closed() {
                    prop_assert_eq!(&doc, &before);
                }
                // (c) 승인된 문서는 모든 단계가 순서대로, 시각이 뒤로 가지 않게 승인됐다.
                if doc.status() == DocumentStatus::Approved {
                    prop_assert!(doc.steps().iter().all(|s| s.status == StepStatus::Approved));
                    let times: Vec<_> = doc.steps().iter().map(|s| s.acted_at).collect();
                    prop_assert!(times.iter().all(Option::is_some));
                    prop_assert!(times.windows(2).all(|pair| pair[0] <= pair[1]));
                }
                // (d) 실패와 반복은 문서를 바꾸지 않고, 반영된 전이는 반드시 바꾼다.
                match &result {
                    Ok(Transition::Applied(events)) => {
                        prop_assert_ne!(&doc, &before);
                        terminal_events += events.iter().filter(|e| e.is_terminal()).count();
                    }
                    Ok(Transition::NoOp) | Err(_) => prop_assert_eq!(&doc, &before),
                }
                // (e) 종료 사건은 문서가 종료될 때 정확히 한 번 나온다.
                prop_assert!(terminal_events <= 1);
                prop_assert_eq!(terminal_events == 1, doc.status().is_closed());
                // (f) 대리 반려는 진행 중인 문서를 언제나 반려로 닫고, 그 밖에는 아무것도 바꾸지
                // 않는다. 닫힌 자리는 그때 차례였던 단계다.
                if forced {
                    match before.status() {
                        DocumentStatus::InProgress => {
                            prop_assert_eq!(doc.status(), DocumentStatus::Rejected);
                            let closed = doc
                                .steps()
                                .iter()
                                .find(|step| step.status == StepStatus::Rejected)
                                .map(|step| step.approver);
                            prop_assert_eq!(closed, before.current_approver());
                            prop_assert_eq!(doc.acted_count(), before.acted_count() + 1);
                        }
                        DocumentStatus::Rejected => {
                            prop_assert_eq!(&result, &Ok(Transition::NoOp));
                        }
                        _ => prop_assert!(result.is_err()),
                    }
                }
                // 외부인·참조자는 (결재선의 동작으로는) 무엇도 할 수 없다.
                if !forced && (by == member(OUTSIDER) || by == member(REFERENCE)) {
                    prop_assert_eq!(result, Err(DomainError::Forbidden));
                }
            }
        }
    }
}
