-- 외부 알림 채널: 앱 내 알림을 외부 메신저(웹훅)로도 보낸다.
--
-- 알림을 만드는 트랜잭션이 보낼 일(outbound_messages)을 한 줄 남기고, 서버의 발송 작업이 따로
-- 보낸다(아웃박스). 외부 발송이 실패해도 앱 내 알림과 업무 기록은 그대로다.

-- 연동 설정(단일 행). 서명 키는 HMAC 키라 원문으로 둔다. API는 이 값을 돌려주지 않는다
-- (새로 만들 때 한 번만 보여 준다).
CREATE TABLE outbound_settings (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    webhook_enabled INTEGER NOT NULL DEFAULT 0 CHECK (webhook_enabled IN (0, 1)),
    webhook_url     TEXT,
    webhook_secret  TEXT,
    version         INTEGER NOT NULL DEFAULT 1,
    updated_at      INTEGER NOT NULL,
    -- 켜려면 주소와 서명 키가 모두 있어야 한다.
    CHECK (webhook_enabled = 0 OR (webhook_url IS NOT NULL AND webhook_secret IS NOT NULL))
) STRICT;

INSERT INTO outbound_settings (id, updated_at)
VALUES (1, CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- 구성원별 외부 알림 수신 여부. 행이 없으면 받는다(기본값).
CREATE TABLE member_notification_prefs (
    member_id        TEXT PRIMARY KEY REFERENCES members(id),
    external_enabled INTEGER NOT NULL CHECK (external_enabled IN (0, 1)),
    updated_at       INTEGER NOT NULL
) STRICT;

-- 보낼 일(아웃박스). 알림·채널마다 한 줄.
--
-- 발송 작업은 `pending`이면서 `next_attempt_at`이 지난 줄을 집을 때 `attempts`를 올리고
-- `next_attempt_at`을 다음 재시도 시각으로 미뤄 둔 뒤 보낸다. 보내는 도중 서버가 꺼져도
-- 그 시각까지는 다시 집히지 않으므로, 많아야 한 번 더 보낼 뿐 같은 줄을 계속 보내지 않는다.
-- 알림이 지워지면(보관된 공지의 읽지 않은 알림 등) 보낼 일도 함께 지운다.
CREATE TABLE outbound_messages (
    id              TEXT PRIMARY KEY,
    notification_id TEXT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
    member_id       TEXT NOT NULL REFERENCES members(id),
    channel         TEXT NOT NULL CHECK (channel IN ('webhook')),
    status          TEXT NOT NULL CHECK (status IN ('pending', 'sent', 'failed')),
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL,
    last_attempt_at INTEGER,
    -- 짧은 사유만(상태 코드, 연결 실패 등). 응답 본문·주소·비밀 값은 넣지 않는다.
    last_error      TEXT CHECK (last_error IS NULL OR length(last_error) <= 200),
    created_at      INTEGER NOT NULL,
    sent_at         INTEGER,
    UNIQUE (notification_id, channel),
    CHECK ((status = 'sent') = (sent_at IS NOT NULL))
) STRICT;
CREATE INDEX outbound_messages_due ON outbound_messages(status, next_attempt_at);
CREATE INDEX outbound_messages_created ON outbound_messages(created_at);
