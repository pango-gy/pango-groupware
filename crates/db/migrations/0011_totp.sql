-- 2단계 인증(TOTP)과 복구 코드.
--
-- 구성원마다 한 줄. `confirmed_at`이 NULL이면 켜는 중(인증 앱에 넣고 첫 코드를 아직 확인하지
-- 않음)이라 로그인에는 쓰지 않는다. `last_step`은 마지막으로 받은 코드의 단계라서, 같은 코드를
-- 다시 쓰지 못한다.
--
-- 비밀키는 인증 앱과 같은 값을 계산해야 하므로 해시가 아니라 그대로 둔다. DB 사본(백업)이
-- 새면 2단계 인증도 뚫리므로 백업은 암호화해 보관한다(docs/DEPLOY.md).
CREATE TABLE member_totp (
    member_id    TEXT PRIMARY KEY REFERENCES members(id),
    secret       BLOB NOT NULL CHECK (length(secret) = 20),
    confirmed_at INTEGER,
    last_step    INTEGER,
    created_at   INTEGER NOT NULL
) STRICT;

-- 복구 코드(해시만). 한 번 쓰면 `used_at`이 남고 다시 쓸 수 없다.
CREATE TABLE member_recovery_codes (
    member_id TEXT NOT NULL REFERENCES members(id),
    code_hash BLOB NOT NULL CHECK (length(code_hash) = 32),
    used_at   INTEGER,
    PRIMARY KEY (member_id, code_hash)
) STRICT, WITHOUT ROWID;
