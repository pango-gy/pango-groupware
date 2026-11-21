-- 공지 첨부파일.
--
-- 파일 내용도 이 DB에 둔다(실행 파일 하나 + SQLite 파일 하나로 운영·백업한다). 목록·상세는
-- 내용 없이 읽도록 내용은 따로 둔다. 첨부를 지우면 내용도 함께 지운다.
CREATE TABLE notice_attachments (
    id          TEXT PRIMARY KEY,
    notice_id   TEXT NOT NULL REFERENCES notices(id),
    filename    TEXT NOT NULL CHECK (length(filename) BETWEEN 1 AND 200),
    size        INTEGER NOT NULL CHECK (size > 0),
    -- 내용의 SHA-256(16진수). 감사 로그와 무결성 확인용.
    sha256      TEXT NOT NULL CHECK (length(sha256) = 64),
    uploaded_by TEXT NOT NULL REFERENCES members(id),
    created_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX notice_attachments_notice ON notice_attachments(notice_id, created_at);

CREATE TABLE notice_attachment_contents (
    attachment_id TEXT PRIMARY KEY REFERENCES notice_attachments(id) ON DELETE CASCADE,
    data          BLOB NOT NULL
) STRICT;
