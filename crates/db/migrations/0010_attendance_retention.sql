-- 출퇴근 기록 보존 기간.
--
-- 관리자가 정한 해(3~10년)가 지난 근무일의 출퇴근 기록과 그 수정 이력을 하루 한 번 지운다.
-- 기본값은 NULL(지우지 않음). 근로기준법의 3년 보존 의무보다 짧게는 정할 수 없다.
--
-- 휴가 원장(leave_ledger)과 결재 문서는 지우지 않는다. 휴가 잔여는 원장 전체로 계산하므로 오래된
-- 줄을 지우면 지금의 잔여가 바뀔 수 있고, 결재 문서는 그 자체가 결재 기록이다.
CREATE TABLE retention_settings (
    id               INTEGER PRIMARY KEY CHECK (id = 1),
    attendance_years INTEGER CHECK (attendance_years IS NULL OR attendance_years BETWEEN 3 AND 10),
    version          INTEGER NOT NULL DEFAULT 1,
    updated_at       INTEGER NOT NULL
) STRICT;

INSERT INTO retention_settings (id, updated_at)
VALUES (1, CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- 보존 기간 정리 중임을 알리는 표시. 정리 트랜잭션이 한 줄을 넣고, 지운 뒤 같은 트랜잭션에서
-- 뺀다. 이 줄이 있을 때만 출퇴근 이력을 지울 수 있다(그 밖의 삭제는 여전히 막힌다).
CREATE TABLE retention_purge_guard (
    id INTEGER PRIMARY KEY CHECK (id = 1)
) STRICT;

DROP TRIGGER attendance_events_no_delete;
CREATE TRIGGER attendance_events_no_delete BEFORE DELETE ON attendance_events
WHEN NOT EXISTS (SELECT 1 FROM retention_purge_guard)
BEGIN SELECT RAISE(ABORT, 'attendance_events is append-only'); END;
