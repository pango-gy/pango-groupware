-- 구성원 날짜 열의 마지막 방어선.
--
-- hired_on / retired_on은 'YYYY-MM-DD' 고정 폭으로 저장하고, 읽을 때 같은 규칙으로 검증한다
-- (codec::date, 1900~9999년). '0202-03-02'나 '+10000-01-01' 같은 값이 한 번 들어가면 그 행을
-- 다시 읽지 못해 구성원 목록 전체가 실패한다. 입력 검증(pango_core::member)이 먼저 막지만,
-- 어떤 경로로든 그런 값이 저장되지 않도록 DB에서도 거부한다.
--
-- 이미 저장된 행은 건드리지 않는다(UPDATE OF 이므로 다른 열의 수정도 막지 않는다).

CREATE TRIGGER members_dates_insert BEFORE INSERT ON members
WHEN NEW.hired_on NOT GLOB '[1-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]'
  OR NEW.hired_on < '1900-01-01'
  OR date(NEW.hired_on) IS NOT NEW.hired_on
  OR (NEW.retired_on IS NOT NULL AND (
        NEW.retired_on NOT GLOB '[1-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]'
     OR NEW.retired_on < '1900-01-01'
     OR date(NEW.retired_on) IS NOT NEW.retired_on))
BEGIN SELECT RAISE(ABORT, 'members: hired_on/retired_on must be YYYY-MM-DD from 1900-01-01'); END;

CREATE TRIGGER members_dates_update BEFORE UPDATE OF hired_on, retired_on ON members
WHEN NEW.hired_on NOT GLOB '[1-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]'
  OR NEW.hired_on < '1900-01-01'
  OR date(NEW.hired_on) IS NOT NEW.hired_on
  OR (NEW.retired_on IS NOT NULL AND (
        NEW.retired_on NOT GLOB '[1-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]'
     OR NEW.retired_on < '1900-01-01'
     OR date(NEW.retired_on) IS NOT NEW.retired_on))
BEGIN SELECT RAISE(ABORT, 'members: hired_on/retired_on must be YYYY-MM-DD from 1900-01-01'); END;
