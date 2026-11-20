-- 근태 정정 요청.
--
-- 구성원은 자기 출퇴근 기록을 직접 고칠 수 없고, 고칠 값을 적어 결재를 올린다.
-- 요청 1건 = 결재 문서 1건(document_id UNIQUE). 최종 승인되면 같은 트랜잭션에서
-- attendance_records가 고쳐지고 attendance_events에 'correction' 이력이 남는다.

CREATE TABLE attendance_corrections (
    id                  TEXT PRIMARY KEY,
    member_id           TEXT NOT NULL REFERENCES members(id),
    work_date           TEXT NOT NULL
                            CHECK (work_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    -- 요청한 값.
    clock_in_at         INTEGER NOT NULL,
    clock_out_at        INTEGER,
    work_type           TEXT NOT NULL CHECK (work_type IN ('office', 'remote', 'field')),
    reason              TEXT NOT NULL,
    -- 요청할 때 본 기록의 version. 그날 기록이 없었으면 NULL.
    -- 승인 시점의 기록이 이와 다르면(그사이 누가 고쳤으면) 반영하지 않는다.
    base_record_version INTEGER,
    status              TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'canceled')),
    document_id         TEXT NOT NULL UNIQUE REFERENCES approval_documents(id),
    created_at          INTEGER NOT NULL,
    decided_at          INTEGER,
    version             INTEGER NOT NULL DEFAULT 1,
    CHECK (clock_out_at IS NULL OR clock_out_at > clock_in_at),
    CHECK ((status = 'pending') = (decided_at IS NULL))
) STRICT;
CREATE INDEX attendance_corrections_member ON attendance_corrections(member_id, created_at);
-- 같은 구성원·근무일에 결재 중인 요청은 하나뿐이다. 동시에 올려도 하나만 남는다.
CREATE UNIQUE INDEX attendance_corrections_one_pending
    ON attendance_corrections(member_id, work_date) WHERE status = 'pending';
