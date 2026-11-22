-- AI 앱(Claude 등)용 원격 MCP의 OAuth 2.1 인가 서버.
--
-- 승인 한 번이 로그인 세션 하나(`sessions`, Bearer)다. 토큰은 그 세션에 매달려 있어서, 세션이
-- 만료·폐기되면(로그아웃, 기기 로그아웃, 비밀번호 변경, 비활성화) MCP 연결도 함께 끊긴다.
-- 토큰과 코드는 해시만 저장한다. 오래된 세션을 정리하면 그 코드·토큰도 함께 지워진다.

-- 동적 등록(RFC 7591)한 공개 클라이언트. ID는 이름·주소로 정해지므로 같은 등록이 줄을 늘리지 않는다.
CREATE TABLE oauth_clients (
    client_id     TEXT PRIMARY KEY,
    client_name   TEXT NOT NULL,
    redirect_uris TEXT NOT NULL, -- JSON 배열
    created_at    INTEGER NOT NULL
) STRICT;

-- 동의 화면을 여는 동안의 인가 요청. 브라우저 쿠키의 nonce와 맞아야 승인할 수 있다.
CREATE TABLE oauth_requests (
    id             TEXT PRIMARY KEY,
    nonce_hash     BLOB NOT NULL CHECK (length(nonce_hash) = 32),
    client_id      TEXT NOT NULL REFERENCES oauth_clients(client_id),
    redirect_uri   TEXT NOT NULL,
    scope          TEXT NOT NULL,
    state          TEXT,
    code_challenge TEXT NOT NULL,
    expires_at     INTEGER NOT NULL
) STRICT;

-- 인가 코드(1분, 한 번만).
CREATE TABLE oauth_codes (
    code_hash      BLOB PRIMARY KEY CHECK (length(code_hash) = 32),
    client_id      TEXT NOT NULL REFERENCES oauth_clients(client_id),
    session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    redirect_uri   TEXT NOT NULL,
    scope          TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    expires_at     INTEGER NOT NULL,
    used_at        INTEGER
) STRICT, WITHOUT ROWID;

-- 접근 토큰(짧게)과 갱신 토큰(한 번 쓰면 새로 받는다). 이미 쓴 갱신 토큰이 다시 오면 탈취로 보고
-- 세션을 끊는다.
CREATE TABLE oauth_tokens (
    token_hash BLOB PRIMARY KEY CHECK (length(token_hash) = 32),
    kind       TEXT NOT NULL CHECK (kind IN ('access', 'refresh')),
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    client_id  TEXT NOT NULL REFERENCES oauth_clients(client_id),
    scope      TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
) STRICT, WITHOUT ROWID;
CREATE INDEX oauth_tokens_session ON oauth_tokens(session_id);
CREATE INDEX oauth_tokens_expires ON oauth_tokens(expires_at);
