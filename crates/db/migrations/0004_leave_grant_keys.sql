-- 자동 부여의 멱등성 키를 날짜에서 차수로 바꾼다.
--
--   'monthly:YYYY-MM-DD' → 'monthly:m'  (m = 입사 후 개월 수, 1..11)
--   'annual:YYYY-MM-DD'  → 'annual:n'   (n = 근속 연수, 1..80)
--
-- 날짜 키는 입사일을 고치면 전부 달라져서, 이미 받은 부여 옆에 한 벌이 더 들어갔다. 차수는
-- 입사일이 바뀌어도 같다(pango_core::leave::accrual_grants).
--
-- 차수는 부여일(valid_from)과 구성원의 현재 입사일(hired_on)의 연·월 차이로 구한다. 부여일은
-- 입사일에 개월 수를 더해 만든 날짜라(말일은 그 달의 말일로 맞춘다) 일(day)은 볼 필요가 없다.
--
-- 옛 키를 그대로 두는 줄:
--   - 차수가 범위 밖이거나 연 부여인데 연 단위로 떨어지지 않는 줄(입사일을 크게 고친 흔적).
--   - 같은 구성원에게 같은 차수가 되는 줄이 여럿일 때 하나를 뺀 나머지(이미 생긴 중복 부여).
--     부여일의 '일'이 입사일과 같은 줄을, 그것도 같으면 먼저 들어간 줄을 그 차수로 삼는다.
--     하나는 반드시 차수 키를 가져야 다음 자동 부여가 같은 차수를 또 넣지 않는다.
--   - 그 차수 키가 이미 있는 구성원의 줄.
-- 남은 옛 키는 어떤 새 키와도 겹치지 않으므로 해가 없다. 수량·날짜는 어느 줄도 바꾸지 않는다.
--
-- 원장은 추가 전용이라(UPDATE 금지 트리거) 이 한 번의 UPDATE 동안만 트리거를 내렸다가
-- 같은 정의로 다시 만든다. 마이그레이션은 한 트랜잭션이므로 중간 상태가 남지 않는다.

CREATE TEMP TABLE leave_grant_rekey AS
WITH dated AS (
    SELECT l.rowid AS ledger_rowid,
           l.member_id,
           substr(l.grant_key, 1, instr(l.grant_key, ':') - 1) AS accrual,
           (CAST(substr(l.valid_from, 1, 4) AS INTEGER) - CAST(substr(m.hired_on, 1, 4) AS INTEGER)) * 12
               + CAST(substr(l.valid_from, 6, 2) AS INTEGER)
               - CAST(substr(m.hired_on, 6, 2) AS INTEGER) AS months,
           substr(l.valid_from, 9, 2) = substr(m.hired_on, 9, 2) AS same_day
    FROM leave_ledger l
    JOIN members m ON m.id = l.member_id
    WHERE l.kind = 'grant'
      AND l.valid_from GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
      AND m.hired_on GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
      AND (l.grant_key GLOB 'monthly:[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
        OR l.grant_key GLOB 'annual:[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]')
),
keyed AS (
    SELECT ledger_rowid,
           member_id,
           same_day,
           CASE
               WHEN accrual = 'monthly' AND months BETWEEN 1 AND 11
                   THEN 'monthly:' || months
               WHEN accrual = 'annual' AND months BETWEEN 12 AND 960 AND months % 12 = 0
                   THEN 'annual:' || (months / 12)
           END AS new_key
    FROM dated
),
ranked AS (
    SELECT ledger_rowid,
           member_id,
           new_key,
           ROW_NUMBER() OVER (
               PARTITION BY member_id, new_key
               ORDER BY same_day DESC, ledger_rowid
           ) AS position
    FROM keyed
    WHERE new_key IS NOT NULL
)
SELECT ledger_rowid, new_key
FROM ranked
WHERE position = 1
  AND NOT EXISTS (
      SELECT 1 FROM leave_ledger taken
      WHERE taken.member_id = ranked.member_id AND taken.grant_key = ranked.new_key
  );

DROP TRIGGER leave_ledger_no_update;

UPDATE leave_ledger
SET grant_key = (
    SELECT new_key FROM leave_grant_rekey WHERE ledger_rowid = leave_ledger.rowid
)
WHERE rowid IN (SELECT ledger_rowid FROM leave_grant_rekey);

CREATE TRIGGER leave_ledger_no_update BEFORE UPDATE ON leave_ledger
BEGIN SELECT RAISE(ABORT, 'leave_ledger is append-only'); END;

DROP TABLE leave_grant_rekey;
