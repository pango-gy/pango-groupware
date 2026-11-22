-- 첨부파일(공지·결재 문서).
--
-- 파일 내용도 이 DB에 둔다(실행 파일 하나 + SQLite 파일 하나로 운영·백업한다). 목록·상세는
-- 내용 없이 읽도록 내용은 따로 둔다. 첨부를 지우면 내용도 함께 지운다.
--
-- 소유자는 공지 또는 결재 문서 중 정확히 하나다(실제 외래 키라 무결성과 연쇄 삭제가 된다).
-- 임시저장 결재 문서를 지우면 그 첨부와 내용도 함께 지워진다. 공지는 지우지 않고 보관한다.
CREATE TABLE attachments (
    id          TEXT PRIMARY KEY,
    notice_id   TEXT REFERENCES notices(id),
    document_id TEXT REFERENCES approval_documents(id) ON DELETE CASCADE,
    filename    TEXT NOT NULL CHECK (length(filename) BETWEEN 1 AND 200),
    size        INTEGER NOT NULL CHECK (size > 0),
    -- 내용의 SHA-256(16진수). 감사 로그와 무결성 확인용.
    sha256      TEXT NOT NULL CHECK (length(sha256) = 64),
    uploaded_by TEXT NOT NULL REFERENCES members(id),
    created_at  INTEGER NOT NULL,
    CHECK ((notice_id IS NULL) <> (document_id IS NULL))
) STRICT;
CREATE INDEX attachments_notice ON attachments(notice_id, created_at)
    WHERE notice_id IS NOT NULL;
CREATE INDEX attachments_document ON attachments(document_id, created_at)
    WHERE document_id IS NOT NULL;

CREATE TABLE attachment_contents (
    attachment_id TEXT PRIMARY KEY REFERENCES attachments(id) ON DELETE CASCADE,
    data          BLOB NOT NULL
) STRICT;
