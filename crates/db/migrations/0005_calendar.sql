-- 캘린더: 내 일정·전사 일정·부서 일정과 참석자.
--
-- 캘린더 ID는 주인에게서 정해진다(pango_core::calendar_access::Calendar::id).
--   * 내 일정(personal):    구성원 ID와 같다.
--   * 부서 일정(department): 부서 ID와 같다.
--   * 전사 일정(company):   고정 ID. 여기서 한 번 만든다.
-- 그래서 내 일정·부서 일정의 행은 그 캘린더에 첫 일정을 만들 때 INSERT … ON CONFLICT DO NOTHING
-- 으로 생기고(쓰기는 직렬화되므로 경합이 없다), 행이 없어도 목록과 권한 판정은 같은 ID를 쓴다.
-- 이름은 저장하지 않는다(부서 이름이 바뀌면 따라가야 하므로 조회할 때 붙인다).
--
-- 공휴일·휴가는 캘린더 화면에 함께 보이지만 여기에 복사하지 않는다(조회할 때 읽는다).

CREATE TABLE calendars (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('personal', 'company', 'department')),
    owner_id      TEXT UNIQUE REFERENCES members(id),
    department_id TEXT UNIQUE REFERENCES departments(id),
    created_at    INTEGER NOT NULL,
    CHECK ((kind = 'personal') = (owner_id IS NOT NULL)),
    CHECK ((kind = 'department') = (department_id IS NOT NULL)),
    CHECK (kind <> 'personal' OR id = owner_id),
    CHECK (kind <> 'department' OR id = department_id)
) STRICT;
CREATE UNIQUE INDEX calendars_company ON calendars(kind) WHERE kind = 'company';

INSERT INTO calendars (id, kind, created_at)
VALUES ('00000000-0000-7000-8000-000000000001', 'company',
        CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- 일정. 종일 일정은 날짜(양 끝 포함)만, 시간 일정은 시각([start_at, end_at))만 가진다.
-- 최대 길이(31일)는 서비스가 확인한다(pango_core::calendar_event::EventTime).
CREATE TABLE calendar_events (
    id          TEXT PRIMARY KEY,
    calendar_id TEXT NOT NULL REFERENCES calendars(id),
    owner_id    TEXT NOT NULL REFERENCES members(id),
    title       TEXT NOT NULL,
    all_day     INTEGER NOT NULL CHECK (all_day IN (0, 1)),
    start_date  TEXT CHECK (start_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    end_date    TEXT CHECK (end_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    start_at    INTEGER,
    end_at      INTEGER,
    location    TEXT,
    description TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    version     INTEGER NOT NULL DEFAULT 1,
    CHECK (
        (all_day = 1
            AND start_date IS NOT NULL AND end_date IS NOT NULL AND start_date <= end_date
            AND start_at IS NULL AND end_at IS NULL)
        OR (all_day = 0
            AND start_at IS NOT NULL AND end_at IS NOT NULL AND start_at < end_at
            AND start_date IS NULL AND end_date IS NULL)
    )
) STRICT;
CREATE INDEX calendar_events_dates ON calendar_events(calendar_id, start_date) WHERE all_day = 1;
CREATE INDEX calendar_events_times ON calendar_events(calendar_id, start_at) WHERE all_day = 0;

-- 참석자(내 일정에만 둔다). 일정을 지우면 함께 지워진다.
CREATE TABLE calendar_attendees (
    event_id     TEXT NOT NULL REFERENCES calendar_events(id) ON DELETE CASCADE,
    member_id    TEXT NOT NULL REFERENCES members(id),
    response     TEXT NOT NULL DEFAULT 'pending'
                 CHECK (response IN ('pending', 'accepted', 'declined')),
    responded_at INTEGER,
    invited_at   INTEGER NOT NULL,
    PRIMARY KEY (event_id, member_id),
    CHECK ((response = 'pending') = (responded_at IS NULL))
) STRICT, WITHOUT ROWID;
CREATE INDEX calendar_attendees_member ON calendar_attendees(member_id, event_id);
