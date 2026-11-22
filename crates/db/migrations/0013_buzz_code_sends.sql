-- Buzz 확인 코드 재발송 제한을 연결 행과 따로 둔다.
--
-- 제한(60초 간격, 시간당 5번)이 `buzz_accounts` 행에만 있으면 연결을 끊을 때(행 삭제) 함께
-- 사라져, 끊기와 다시 연결을 되풀이해 남의 Buzz 계정에 코드 DM을 끝없이 보낼 수 있었다.
CREATE TABLE buzz_code_sends (
    member_id       TEXT PRIMARY KEY REFERENCES members(id),
    last_sent_at    INTEGER NOT NULL,
    window_start    INTEGER NOT NULL,
    sends_in_window INTEGER NOT NULL CHECK (sends_in_window >= 0)
) STRICT;

INSERT INTO buzz_code_sends (member_id, last_sent_at, window_start, sends_in_window)
SELECT member_id, code_sent_at, send_window_start, sends_in_window
FROM buzz_accounts
WHERE code_sent_at IS NOT NULL AND send_window_start IS NOT NULL;
