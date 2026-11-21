-- 외부 알림의 두 번째 채널: 사내 메신저 Buzz(Nostr 릴레이) DM.
--
-- 봇 비밀키와 릴레이 주소는 서버 실행 설정에만 있다(여기에 두지 않는다). DB에는 켜기·끄기와
-- 구성원별 Buzz 계정 연결(공개키, 확인 코드의 해시)만 둔다.

-- Buzz 채널 켜기·끄기(단일 행). 웹훅 설정과 따로 버전을 가진다.
CREATE TABLE buzz_settings (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    enabled    INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    version    INTEGER NOT NULL DEFAULT 1,
    updated_at INTEGER NOT NULL
) STRICT;

INSERT INTO buzz_settings (id, updated_at)
VALUES (1, CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- 구성원의 Buzz 계정 연결. 구성원마다 한 줄.
--
-- `pending`: 확인 코드를 보냈고 입력을 기다린다. `verified`: 소유를 확인했다(알림을 보낸다).
-- 코드는 해시만 둔다. 한 공개키는 한 구성원만 확인할 수 있다(아래 유일 색인).
-- 재발송 제한: `code_sent_at`(마지막 발송), `send_window_start`·`sends_in_window`(시간당 상한).
CREATE TABLE buzz_accounts (
    member_id         TEXT PRIMARY KEY REFERENCES members(id),
    pubkey            TEXT NOT NULL CHECK (length(pubkey) = 64),
    status            TEXT NOT NULL CHECK (status IN ('pending', 'verified')),
    code_hash         BLOB CHECK (code_hash IS NULL OR length(code_hash) = 32),
    code_expires_at   INTEGER,
    code_attempts     INTEGER NOT NULL DEFAULT 0 CHECK (code_attempts >= 0),
    code_sent_at      INTEGER,
    send_window_start INTEGER,
    sends_in_window   INTEGER NOT NULL DEFAULT 0 CHECK (sends_in_window >= 0),
    -- 봇과 이 공개키 사이 DM의 릴레이 채널 ID(UUID). 처음 보낼 때 열고 저장해 다시 쓴다.
    dm_channel_id     TEXT,
    verified_at       INTEGER,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    CHECK ((status = 'verified') = (verified_at IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX buzz_accounts_verified_pubkey ON buzz_accounts(pubkey)
    WHERE status = 'verified';

-- 아웃박스 채널에 'buzz'를 더한다. SQLite는 CHECK를 바꿀 수 없어 표를 다시 만든다
-- (이 표를 가리키는 외래 키는 없다).
CREATE TABLE outbound_messages_new (
    id              TEXT PRIMARY KEY,
    notification_id TEXT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
    member_id       TEXT NOT NULL REFERENCES members(id),
    channel         TEXT NOT NULL CHECK (channel IN ('webhook', 'buzz')),
    status          TEXT NOT NULL CHECK (status IN ('pending', 'sent', 'failed')),
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL,
    last_attempt_at INTEGER,
    last_error      TEXT CHECK (last_error IS NULL OR length(last_error) <= 200),
    created_at      INTEGER NOT NULL,
    sent_at         INTEGER,
    UNIQUE (notification_id, channel),
    CHECK ((status = 'sent') = (sent_at IS NOT NULL))
) STRICT;
INSERT INTO outbound_messages_new
SELECT id, notification_id, member_id, channel, status, attempts, next_attempt_at,
       last_attempt_at, last_error, created_at, sent_at
FROM outbound_messages;
DROP TABLE outbound_messages;
ALTER TABLE outbound_messages_new RENAME TO outbound_messages;
CREATE INDEX outbound_messages_due ON outbound_messages(status, next_attempt_at);
CREATE INDEX outbound_messages_created ON outbound_messages(created_at);
CREATE INDEX outbound_messages_channel ON outbound_messages(channel, last_attempt_at);
