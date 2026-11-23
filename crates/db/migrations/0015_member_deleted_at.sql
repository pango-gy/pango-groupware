-- 삭제된 구성원 계정 표시.
-- 감사 로그·결재·근태·휴가 원장의 참조 무결성(추가 전용 테이블 포함)을 유지하면서
-- 계정을 목록·로그인에서 완전히 제거하고 로그인 이메일을 다시 쓸 수 있게 반환한다.
ALTER TABLE members ADD COLUMN deleted_at INTEGER;
CREATE INDEX members_deleted_at ON members(deleted_at) WHERE deleted_at IS NULL;
