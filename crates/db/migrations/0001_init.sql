-- pango-groupware 초기 스키마.
--
-- 규약
--   * 시각: INTEGER (UTC 유닉스 밀리초). 날짜: TEXT 'YYYY-MM-DD'.
--   * ID: TEXT (UUID v7, 하이픈 포함 소문자).
--   * 수정 가능한 집합체는 version 열을 가진다(낙관적 잠금).
--   * 원장·이력 테이블은 트리거로 수정·삭제를 막는다(추가 전용).

----------------------------------------------------------------------
-- 설정
----------------------------------------------------------------------
CREATE TABLE company_settings (
    id                 INTEGER PRIMARY KEY CHECK (id = 1),
    company_name       TEXT    NOT NULL,
    timezone           TEXT    NOT NULL,
    day_cutoff_min     INTEGER NOT NULL CHECK (day_cutoff_min BETWEEN 0 AND 720),
    work_start_min     INTEGER NOT NULL CHECK (work_start_min BETWEEN 0 AND 1439),
    work_end_min       INTEGER NOT NULL CHECK (work_end_min BETWEEN 0 AND 1439),
    late_grace_min     INTEGER NOT NULL CHECK (late_grace_min BETWEEN 0 AND 120),
    half_day_split_min INTEGER NOT NULL CHECK (half_day_split_min BETWEEN 0 AND 1439),
    quarter_min        INTEGER NOT NULL CHECK (quarter_min BETWEEN 1 AND 720),
    break_tiers        TEXT    NOT NULL, -- JSON [[span_min, break_min], ...]
    workdays           INTEGER NOT NULL CHECK (workdays BETWEEN 1 AND 127),
    version            INTEGER NOT NULL DEFAULT 1,
    updated_at         INTEGER NOT NULL,
    CHECK (work_start_min < work_end_min)
) STRICT;

CREATE TABLE holidays (
    date TEXT PRIMARY KEY CHECK (date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    name TEXT NOT NULL
) STRICT;

----------------------------------------------------------------------
-- 조직
----------------------------------------------------------------------
CREATE TABLE departments (
    id             TEXT PRIMARY KEY,
    parent_id      TEXT REFERENCES departments(id),
    name           TEXT NOT NULL,
    head_member_id TEXT REFERENCES members(id),
    sort_order     INTEGER NOT NULL DEFAULT 0,
    archived_at    INTEGER,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    version        INTEGER NOT NULL DEFAULT 1,
    CHECK (parent_id IS NULL OR parent_id <> id)
) STRICT;
CREATE INDEX departments_parent ON departments(parent_id);
CREATE INDEX departments_head ON departments(head_member_id) WHERE head_member_id IS NOT NULL;

CREATE TABLE positions (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    -- 숫자가 클수록 상위 직책. 목록 정렬과 결재선 추천에 쓴다.
    level       INTEGER NOT NULL DEFAULT 0 CHECK (level BETWEEN 0 AND 1000),
    archived_at INTEGER,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    version     INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE UNIQUE INDEX positions_active_name ON positions(name) WHERE archived_at IS NULL;

CREATE TABLE members (
    id                   TEXT PRIMARY KEY,
    login_id             TEXT NOT NULL UNIQUE,
    name                 TEXT NOT NULL,
    email                TEXT,
    phone                TEXT,
    department_id        TEXT REFERENCES departments(id),
    position_id          TEXT REFERENCES positions(id),
    role                 TEXT NOT NULL CHECK (role IN ('member', 'admin', 'owner')),
    status               TEXT NOT NULL CHECK (status IN ('active', 'suspended', 'retired')),
    hired_on             TEXT NOT NULL,
    retired_on           TEXT,
    password_hash        TEXT NOT NULL,
    must_change_password INTEGER NOT NULL DEFAULT 0 CHECK (must_change_password IN (0, 1)),
    password_changed_at  INTEGER,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL,
    version              INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE INDEX members_department ON members(department_id);
CREATE INDEX members_status ON members(status);

CREATE TABLE sessions (
    id                  TEXT PRIMARY KEY,
    member_id           TEXT NOT NULL REFERENCES members(id),
    -- 토큰 원문은 저장하지 않는다. SHA-256 해시만 둔다.
    token_hash          BLOB NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    via                 TEXT NOT NULL CHECK (via IN ('cookie', 'bearer')),
    user_agent          TEXT,
    ip                  TEXT,
    created_at          INTEGER NOT NULL,
    last_seen_at        INTEGER NOT NULL,
    idle_expires_at     INTEGER NOT NULL,
    absolute_expires_at INTEGER NOT NULL,
    revoked_at          INTEGER
) STRICT;
CREATE INDEX sessions_member ON sessions(member_id);

-- 키(IP, 로그인 아이디)별 시도 횟수(고정 창). 계정 잠금도 이 표로 처리한다.
-- 없는 아이디에도 똑같이 적용되므로, 잠금 여부로 계정 존재를 알아낼 수 없다.
CREATE TABLE login_throttle (
    bucket       TEXT PRIMARY KEY,
    window_start INTEGER NOT NULL,
    attempts     INTEGER NOT NULL CHECK (attempts >= 0)
) STRICT;

----------------------------------------------------------------------
-- 출퇴근
----------------------------------------------------------------------
CREATE TABLE attendance_records (
    id           TEXT PRIMARY KEY,
    member_id    TEXT NOT NULL REFERENCES members(id),
    work_date    TEXT NOT NULL,
    clock_in_at  INTEGER NOT NULL,
    clock_out_at INTEGER,
    work_type    TEXT NOT NULL CHECK (work_type IN ('office', 'remote', 'field')),
    note         TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    version      INTEGER NOT NULL DEFAULT 1,
    -- 구성원·근무일당 1건. 동시 출근 요청이 와도 하나만 남는다.
    UNIQUE (member_id, work_date),
    CHECK (clock_out_at IS NULL OR clock_out_at >= clock_in_at)
) STRICT;
CREATE INDEX attendance_records_date ON attendance_records(work_date);
CREATE INDEX attendance_records_open ON attendance_records(member_id, clock_in_at)
    WHERE clock_out_at IS NULL;

CREATE TABLE attendance_events (
    id         TEXT PRIMARY KEY,
    record_id  TEXT NOT NULL REFERENCES attendance_records(id),
    member_id  TEXT NOT NULL REFERENCES members(id),
    kind       TEXT NOT NULL CHECK (kind IN
                   ('clock_in', 'clock_out', 'admin_create', 'admin_edit', 'correction')),
    before     TEXT, -- JSON
    after      TEXT NOT NULL, -- JSON
    reason     TEXT,
    actor_id   TEXT NOT NULL REFERENCES members(id),
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX attendance_events_record ON attendance_events(record_id, created_at);

----------------------------------------------------------------------
-- 휴가
----------------------------------------------------------------------
CREATE TABLE leave_types (
    id              TEXT PRIMARY KEY,
    code            TEXT NOT NULL UNIQUE,
    name            TEXT NOT NULL,
    -- 1이면 연차 잔여에서 차감한다(연차·반차). 0이면 차감 없음(경조사·공가 등).
    deducts_balance INTEGER NOT NULL CHECK (deducts_balance IN (0, 1)),
    allow_partial   INTEGER NOT NULL CHECK (allow_partial IN (0, 1)),
    active          INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    sort_order      INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    version         INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE leave_requests (
    id            TEXT PRIMARY KEY,
    member_id     TEXT NOT NULL REFERENCES members(id),
    leave_type_id TEXT NOT NULL REFERENCES leave_types(id),
    start_date    TEXT NOT NULL,
    end_date      TEXT NOT NULL,
    unit          TEXT NOT NULL CHECK (unit IN
                      ('full_day', 'am_half', 'pm_half', 'am_quarter', 'pm_quarter')),
    amount_q      INTEGER NOT NULL CHECK (amount_q > 0),
    reason        TEXT NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'canceled')),
    document_id   TEXT NOT NULL UNIQUE REFERENCES approval_documents(id),
    created_at    INTEGER NOT NULL,
    decided_at    INTEGER,
    version       INTEGER NOT NULL DEFAULT 1,
    CHECK (start_date <= end_date)
) STRICT;
CREATE INDEX leave_requests_member ON leave_requests(member_id, start_date);
CREATE INDEX leave_requests_dates ON leave_requests(start_date, end_date);

-- 휴가 원장. 잔여는 이 테이블의 합으로만 계산한다.
CREATE TABLE leave_ledger (
    id           TEXT PRIMARY KEY,
    member_id    TEXT NOT NULL REFERENCES members(id),
    kind         TEXT NOT NULL CHECK (kind IN ('grant', 'use', 'restore', 'adjust')),
    amount_q     INTEGER NOT NULL,
    valid_from   TEXT,
    expires_on   TEXT,
    effective_on TEXT,
    request_id   TEXT REFERENCES leave_requests(id),
    -- 자동 부여의 멱등성 키 (예: 'annual:2026-03-02').
    grant_key    TEXT,
    memo         TEXT,
    actor_id     TEXT REFERENCES members(id),
    created_at   INTEGER NOT NULL,
    CHECK (kind <> 'grant' OR (amount_q > 0 AND valid_from IS NOT NULL)),
    CHECK (kind <> 'use' OR (amount_q > 0 AND effective_on IS NOT NULL AND request_id IS NOT NULL)),
    CHECK (kind <> 'restore' OR request_id IS NOT NULL),
    CHECK (kind <> 'adjust' OR (amount_q <> 0 AND effective_on IS NOT NULL))
) STRICT;
CREATE INDEX leave_ledger_member ON leave_ledger(member_id, created_at);
-- 같은 자동 부여는 한 번만.
CREATE UNIQUE INDEX leave_ledger_grant_key ON leave_ledger(member_id, grant_key)
    WHERE grant_key IS NOT NULL;
-- 신청 1건당 사용 1줄, 복원 1줄.
CREATE UNIQUE INDEX leave_ledger_request_kind ON leave_ledger(request_id, kind)
    WHERE request_id IS NOT NULL;

----------------------------------------------------------------------
-- 전자결재
----------------------------------------------------------------------
CREATE TABLE approval_forms (
    id            TEXT PRIMARY KEY,
    code          TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    kind          TEXT NOT NULL CHECK (kind IN ('general', 'leave', 'attendance_correction')),
    body_template TEXT NOT NULL DEFAULT '',
    active        INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    sort_order    INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    version       INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE approval_documents (
    id                    TEXT PRIMARY KEY,
    -- 상신 시점에 부여한다. 임시저장 문서는 NULL.
    doc_no                TEXT UNIQUE,
    form_id               TEXT NOT NULL REFERENCES approval_forms(id),
    kind                  TEXT NOT NULL CHECK (kind IN ('general', 'leave', 'attendance_correction')),
    drafter_id            TEXT NOT NULL REFERENCES members(id),
    -- 기안 당시 소속(이후 조직 개편과 무관하게 보존).
    drafter_department_id TEXT REFERENCES departments(id),
    title                 TEXT NOT NULL,
    body                  TEXT NOT NULL,
    payload               TEXT, -- JSON: 시스템 양식의 구조화된 내용
    status                TEXT NOT NULL CHECK (status IN
                              ('draft', 'in_progress', 'approved', 'rejected', 'withdrawn')),
    submitted_at          INTEGER,
    completed_at          INTEGER,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    version               INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE INDEX approval_documents_drafter ON approval_documents(drafter_id, created_at);
CREATE INDEX approval_documents_status ON approval_documents(status, created_at);

CREATE TABLE approval_steps (
    id          TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES approval_documents(id) ON DELETE CASCADE,
    step_order  INTEGER NOT NULL CHECK (step_order BETWEEN 1 AND 10),
    approver_id TEXT NOT NULL REFERENCES members(id),
    kind        TEXT NOT NULL CHECK (kind IN ('approve', 'agree')),
    status      TEXT NOT NULL CHECK (status IN ('waiting', 'pending', 'approved', 'rejected')),
    acted_at    INTEGER,
    comment     TEXT,
    UNIQUE (document_id, step_order),
    UNIQUE (document_id, approver_id)
) STRICT;
CREATE INDEX approval_steps_approver ON approval_steps(approver_id, status);
-- 문서당 진행 중인 단계는 하나뿐이다.
CREATE UNIQUE INDEX approval_steps_one_pending ON approval_steps(document_id)
    WHERE status = 'pending';

CREATE TABLE approval_references (
    document_id TEXT NOT NULL REFERENCES approval_documents(id) ON DELETE CASCADE,
    member_id   TEXT NOT NULL REFERENCES members(id),
    read_at     INTEGER,
    PRIMARY KEY (document_id, member_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX approval_references_member ON approval_references(member_id);

CREATE TABLE approval_events (
    id          TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES approval_documents(id),
    actor_id    TEXT NOT NULL REFERENCES members(id),
    kind        TEXT NOT NULL CHECK (kind IN
                    ('submitted', 'approved', 'completed', 'rejected', 'withdrawn')),
    step_order  INTEGER,
    comment     TEXT,
    created_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX approval_events_document ON approval_events(document_id, created_at);

-- 연도별 문서 번호 발급.
CREATE TABLE doc_counters (
    year     INTEGER PRIMARY KEY,
    last_seq INTEGER NOT NULL CHECK (last_seq >= 0)
) STRICT;

----------------------------------------------------------------------
-- 공지·알림
----------------------------------------------------------------------
CREATE TABLE notices (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    body          TEXT NOT NULL,
    author_id     TEXT NOT NULL REFERENCES members(id),
    pinned        INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    must_read     INTEGER NOT NULL DEFAULT 0 CHECK (must_read IN (0, 1)),
    audience      TEXT NOT NULL CHECK (audience IN ('all', 'department')),
    department_id TEXT REFERENCES departments(id),
    published_at  INTEGER NOT NULL,
    archived_at   INTEGER,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    version       INTEGER NOT NULL DEFAULT 1,
    CHECK ((audience = 'department') = (department_id IS NOT NULL))
) STRICT;
CREATE INDEX notices_published ON notices(published_at);

CREATE TABLE notice_reads (
    notice_id TEXT NOT NULL REFERENCES notices(id) ON DELETE CASCADE,
    member_id TEXT NOT NULL REFERENCES members(id),
    read_at   INTEGER NOT NULL,
    PRIMARY KEY (notice_id, member_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE notifications (
    id         TEXT PRIMARY KEY,
    member_id  TEXT NOT NULL REFERENCES members(id),
    kind       TEXT NOT NULL,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL DEFAULT '',
    link       TEXT,
    -- 같은 사건으로 알림이 두 번 생기지 않게 하는 키.
    dedupe_key TEXT,
    read_at    INTEGER,
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX notifications_member ON notifications(member_id, created_at);
CREATE UNIQUE INDEX notifications_dedupe ON notifications(member_id, dedupe_key)
    WHERE dedupe_key IS NOT NULL;

----------------------------------------------------------------------
-- 감사 로그·멱등성
----------------------------------------------------------------------
CREATE TABLE audit_log (
    id          TEXT PRIMARY KEY,
    at          INTEGER NOT NULL,
    -- 시스템 작업(초기화, 자동 부여)은 NULL.
    actor_id    TEXT REFERENCES members(id),
    action      TEXT NOT NULL,
    target_type TEXT NOT NULL,
    target_id   TEXT,
    detail      TEXT, -- JSON. 비밀번호·토큰은 절대 넣지 않는다.
    ip          TEXT
) STRICT;
CREATE INDEX audit_log_at ON audit_log(at);
CREATE INDEX audit_log_target ON audit_log(target_type, target_id);

CREATE TABLE idempotency_keys (
    member_id   TEXT NOT NULL REFERENCES members(id),
    key         TEXT NOT NULL,
    -- 요청 경로 + 본문의 SHA-256. 같은 키로 다른 요청이 오면 거부한다.
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    status      INTEGER NOT NULL,
    response    TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (member_id, key)
) STRICT, WITHOUT ROWID;
CREATE INDEX idempotency_keys_created ON idempotency_keys(created_at);

----------------------------------------------------------------------
-- 추가 전용 테이블 보호
----------------------------------------------------------------------
CREATE TRIGGER audit_log_no_update BEFORE UPDATE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;
CREATE TRIGGER audit_log_no_delete BEFORE DELETE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;

CREATE TRIGGER leave_ledger_no_update BEFORE UPDATE ON leave_ledger
BEGIN SELECT RAISE(ABORT, 'leave_ledger is append-only'); END;
CREATE TRIGGER leave_ledger_no_delete BEFORE DELETE ON leave_ledger
BEGIN SELECT RAISE(ABORT, 'leave_ledger is append-only'); END;

CREATE TRIGGER attendance_events_no_update BEFORE UPDATE ON attendance_events
BEGIN SELECT RAISE(ABORT, 'attendance_events is append-only'); END;
CREATE TRIGGER attendance_events_no_delete BEFORE DELETE ON attendance_events
BEGIN SELECT RAISE(ABORT, 'attendance_events is append-only'); END;

CREATE TRIGGER approval_events_no_update BEFORE UPDATE ON approval_events
BEGIN SELECT RAISE(ABORT, 'approval_events is append-only'); END;
CREATE TRIGGER approval_events_no_delete BEFORE DELETE ON approval_events
BEGIN SELECT RAISE(ABORT, 'approval_events is append-only'); END;
