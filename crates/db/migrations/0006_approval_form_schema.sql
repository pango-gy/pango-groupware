-- 결재 양식에 분류와 필드 정의를 붙인다.
--
--   category               양식 분류(pango_core::approval::FormCategory).
--   schema                 필드 정의 JSON(pango_core::form_schema::FormSchema).
--   schema_version         필드 정의를 고칠 때마다 1씩 오른다. 문서는 낸 시점의 정의를 payload에
--                          함께 저장하므로, 정의가 바뀌어도 예전 문서는 그때 모양대로 보인다.
--   use_body               자유 입력 본문을 쓰는지(쓰면 필수).
--   default_reference_ids  상신할 때 자동으로 붙는 참조자(JSON 배열, 없으면 NULL).
--   origin                 'catalog' = 제품 기본 양식, 'custom' = 관리자가 만든 양식.
--
-- 기본 양식은 서버가 시작할 때 code로 없는 것만 채운다(service::approval::ensure_form_catalog_tx).
-- 양식은 지우지 않고 끄기만 하므로(active = 0) 꺼 둔 기본 양식이 다시 생기지 않는다.

ALTER TABLE approval_forms ADD COLUMN category TEXT NOT NULL DEFAULT 'general'
    CHECK (category IN ('general', 'hr', 'attendance', 'trip', 'accounting', 'support', 'welfare', 'etc'));
ALTER TABLE approval_forms ADD COLUMN schema TEXT NOT NULL DEFAULT '{"fields":[]}';
ALTER TABLE approval_forms ADD COLUMN schema_version INTEGER NOT NULL DEFAULT 1
    CHECK (schema_version >= 1);
ALTER TABLE approval_forms ADD COLUMN use_body INTEGER NOT NULL DEFAULT 1
    CHECK (use_body IN (0, 1));
ALTER TABLE approval_forms ADD COLUMN default_reference_ids TEXT;
ALTER TABLE approval_forms ADD COLUMN origin TEXT NOT NULL DEFAULT 'custom'
    CHECK (origin IN ('catalog', 'custom'));

-- 최초 설정이 만들어 둔 세 양식은 기본 양식이다.
UPDATE approval_forms SET origin = 'catalog'
 WHERE (code = 'general' AND kind = 'general')
    OR (code = 'leave' AND kind = 'leave')
    OR (code = 'attendance_correction' AND kind = 'attendance_correction');
UPDATE approval_forms SET category = 'hr' WHERE code = 'leave' AND kind = 'leave';
UPDATE approval_forms SET category = 'attendance'
 WHERE code = 'attendance_correction' AND kind = 'attendance_correction';

CREATE INDEX approval_documents_form ON approval_documents(form_id);
