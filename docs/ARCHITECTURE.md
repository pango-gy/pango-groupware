# 아키텍처

이 문서는 pango-groupware의 **설계 계약**입니다. 코드가 이 문서와 다르면 둘 중 하나를 고쳐야 합니다.

## 1. 범위 (MVP)

| 영역 | 포함 | MVP 이후 |
| --- | --- | --- |
| 인증 | 아이디/비밀번호, 세션, 비밀번호 변경, 기기(세션) 관리 | TOTP, OIDC SSO |
| 조직 | 부서 트리, 직책, 구성원, 역할 | 겸직, 조직 개편 이력 |
| 출퇴근·근태 | 출근/퇴근, 근무 형태, 월별 조회·요약, 정정 요청, 관리자 수정·CSV | 위치/Wi-Fi 확인, 교대 근무 |
| 휴가 | 휴가 종류, 연차 자동 부여, 신청·승인·취소, 잔여 조회, 관리자 조정 | 회계연도 기준 부여 |
| 전자결재 | 분류별 양식(필드 정의), 기안, 결재라인(결재·합의·참조), 승인·반려·회수, 결재함, 첨부파일 | 대결·전결 |
| 공지 | 작성·고정·필독, 읽음 확인, 대상 지정, 첨부파일 | 댓글 |
| 캘린더 | 내·전사·부서 일정, 참석자 초대·응답, 참석자 일정 확인, 공휴일·휴가 표시 | 반복 일정, 미리 알림, 생일, 다른 사람의 캘린더 공유 |
| 알림 | 앱 내 알림, 외부 연동(범용 웹훅, 사내 메신저 Buzz DM) | 푸시 중계 서버 |
| 관리 | 설정, 근무 정책, 공휴일, 감사 로그 | 백업 UI |

## 2. 크레이트 구성과 의존 방향

```text
crates/
├── core/    pango-core    도메인 타입과 순수 규칙. IO 없음.
├── api/     pango-api     요청/응답 DTO, 오류 코드, 경로. 서버와 웹이 함께 사용.
├── db/      pango-db      SQLite 스키마, 마이그레이션, 저장소(repository).
├── server/  pango-server  서비스(유스케이스) + HTTP + 실행 파일.
└── web/     pango-web     Dioxus UI. 웹(WASM)과 iOS/Android 앱(Dioxus mobile)이 같은 코드.
xtask/                     웹 번들 빌드 등 개발 작업.
```

의존은 한 방향입니다: `core ← api ← (db, web)`, `core ← db ← server`, `api ← server`.

- **core**: 검증, 권한 판정, 근태 계산, 연차 계산, 결재 상태 전이. 시계와 난수도 주입받으므로 전부 단위 테스트 가능.
- **db**: SQL만 담당. 비즈니스 규칙을 넣지 않음. 함수는 `&mut SqliteConnection`(트랜잭션) 또는 읽기 풀을 받음.
- **server/service**: 유스케이스 하나 = 함수 하나 = 쓰기 트랜잭션 하나. 권한 확인 → 규칙(core) → 저장(db) → 감사 로그·알림까지 같은 트랜잭션.
- **server/http**: 추출(extract)·직렬화·상태 코드 변환만. 로직 없음.

### 계층 규칙

1. 핸들러는 서비스 함수만 호출한다. 저장소를 직접 호출하지 않는다.
2. 권한 판정은 `core::authz`의 함수로만 한다. 핸들러나 SQL에 역할 비교를 흩어 놓지 않는다.
3. 같은 검증을 두 번 쓰지 않는다. 입력 검증은 core의 값 타입 생성자(`Title::parse` 등)에 둔다.
4. 조회 범위 밖의 자원은 403이 아니라 **404**로 응답한다(ID 추측 방지). 범위 안인데 동작이 금지된 경우만 403.

## 3. 공통 규약

### 식별자
UUID v7을 타입별 newtype(`MemberId`, `DocumentId` …)으로 감쌉니다. DB에는 하이픈 포함 소문자 TEXT로 저장합니다.

### 시간
- 시각은 `Timestamp`(UTC, 밀리초 정수)로 저장·전송합니다.
- 날짜는 `YYYY-MM-DD` 문자열(`NaiveDate`)입니다. 연도는 **1900~9999년**만 받습니다. 문자열로 받은 날짜는 `calendar::parse_date`, JSON에서 값으로 읽힌 날짜는 `calendar::check_date`를 거친 뒤에만 저장합니다(둘은 같은 범위). 범위 밖의 날짜(`0202-03-02`, `+10000-01-01`)는 저장 표기가 깨져 그 행을 다시 읽지 못하게 만들기 때문입니다. 연도를 받는 쿼리(`?year=`: 공휴일, 휴가 신청 목록)도 같은 범위이며 `calendar::year_range`로 한 해를 만듭니다(범위 밖은 400 `validation_failed`, `field: year`).
- 입사일은 1950-01-01부터 회사 시간대 기준 오늘의 1년 뒤까지만 새로 정할 수 있고, 퇴사일은 입사일보다 이를 수 없습니다(`member::validate_hired_on`·`validate_retired_on`, 400 `validation_failed` + `field`). `members`의 두 날짜 열은 DB 트리거(`0003_member_date_checks.sql`)로도 표기와 1900년 하한을 강제합니다.
- **근무일(work date)** 은 회사 시간대 기준 날짜에 `day_cutoff`(기본 05:00)를 적용한 값입니다. 새벽 01:00 퇴근은 전날 근무일에 속합니다.
- 현재 시각은 `Clock` 트레이트로 주입합니다. 코드에서 `Utc::now()`를 직접 부르지 않습니다.

### 오류
- core: `DomainError` (`Validation`, `Forbidden`, `NotFound`, `Conflict`, `Unauthenticated`, `RateLimited`).
- HTTP 본문: `{"error": {"code": "snake_case", "message": "…", "field": "…", "reason": "…"}}`.
- 한 요청에서 여러 필드가 잘못될 수 있는 입력(결재 양식의 값, 양식의 필드 정의)은 **찾은 오류를 전부** `fields`에 담습니다: `"fields": [{"key": "items", "row": 1, "column": "amount", "reason": "out_of_range"}, …]`. `row`(0부터)·`column`은 표의 칸일 때만 값이 있고 아니면 `null`입니다. 이때도 `field`·`reason`에는 첫 오류가 들어 있어 `fields`를 모르는 클라이언트가 그대로 동작합니다. 그 밖의 오류에는 `fields`가 없습니다(`ServiceError::InvalidFields`, 최대 100개).
- 오류 코드는 `pango_api::ErrorCode` 한 곳에 정의합니다. 웹 UI의 문구도 이 코드에 매핑합니다.

| 상황 | 상태 |
| --- | --- |
| 입력 형식 오류 | 400 `validation_failed` (+ `field`) |
| 미인증 | 401 |
| 금지된 동작 | 403 |
| 없음 / 범위 밖 | 404 |
| 상태 충돌·버전 불일치·중복 | 409 |
| 본문 초과 | 413 |
| 멱등성 키 재사용(다른 본문) | 422 `idempotency_key_reused` |
| 로그인 시도 제한 | 429 |

### 동시성
- SQLite는 WAL 모드. **쓰기 연결은 1개**(writer 풀), 읽기는 별도 풀. 쓰기 트랜잭션은 직렬화되므로 `SQLITE_BUSY`가 나지 않습니다.
- 쓰기 연결을 쥐고 있는 동안은 다른 모든 쓰기가 기다립니다. 그래서 **긴 읽기는 쓰기 트랜잭션 안에서 하지 않습니다.** 구성원 수만큼 계산하는 조회(근태 CSV 내보내기, 전원 연차 잔여)는 읽기 스냅숏(`state.read_snapshot()`, 한 시점의 데이터)에서 계산하고, 구성원마다 조회하지 않도록 묶어 읽습니다. 쓰기 트랜잭션은 써야 할 것(감사 로그 한 줄, 아직 없는 자동 부여)이 있을 때만 짧게 엽니다.
- 수정 가능한 집합체는 `version` 열을 가집니다. 수정 요청은 읽었던 `version`을 보내고, 다르면 409 `version_conflict`.
- 유일성(근무일당 출근 1건, 결재 단계당 처리 1건)은 **DB 제약**으로도 보장합니다.

### 멱등성
- 생성 요청(`POST`)은 `Idempotency-Key` 헤더를 받습니다. 같은 구성원·같은 키·같은 본문이면 저장해 둔 첫 응답을 그대로 돌려줍니다. 본문이 다르면 422.
- 상태 전이(승인·반려·취소·출근·퇴근)는 **자연 멱등**입니다. 이미 그 상태면 성공으로 응답하고 부수효과(알림·차감)를 반복하지 않습니다.
- 자동 부여(연차)는 `grant_key` 유일 제약으로 한 번만 반영됩니다. 키는 부여의 **차수**입니다(`monthly:1`~`monthly:11`, `annual:1`~). 날짜가 아니므로 입사일을 고쳐도 같은 차수는 다시 들어가지 않습니다. 잔여 조회·휴가 신청이 본인의 부여를 그때그때 반영하고, 서버의 주기 작업이 회사 시간대 기준 하루에 한 번 재직 구성원 전원의 부여를 반영합니다.
- `GET /home`과 `GET /admin/dashboard`는 아무것도 쓰지 않습니다. 홈의 연차 잔여는 저장된 원장 그대로이므로, 오늘 생긴 부여는 그날의 주기 작업(또는 잔여 조회) 뒤에 보입니다.

### 페이지네이션
`?limit=`(기본 50, 최대 200) + `?cursor=`(불투명 문자열). 응답은 `{"items": […], "next_cursor": "…"|null}`.

### 감사 로그
권한·계정·기록을 바꾸는 모든 서비스 함수는 같은 트랜잭션에서 `audit_log`에 한 줄을 남깁니다. 비밀번호·토큰은 절대 남기지 않습니다.

## 4. 인증

- **세션 토큰**: 256비트 난수. DB에는 SHA-256 해시만 저장. 웹은 `HttpOnly; Secure; SameSite=Strict` 쿠키, 모바일 앱은 로그인 응답 본문으로 받은 토큰을 `Authorization: Bearer`로 보낸다(iOS 키체인 보관, `token_in_body`).
- 유휴 만료 14일, 절대 만료 90일. 비밀번호 변경 시 다른 세션 전부 폐기. 비활성화·퇴사 처리 시 전부 폐기.
- 쿠키 인증으로 들어온 변경 요청은 `Sec-Fetch-Site`/`Origin`을 검사해 교차 출처를 거부합니다. 운영에서는 `--public-origin`으로 서비스 주소를 지정해 `Origin`을 정확히 비교합니다.
- 비밀번호: Argon2id. 길이 10~128자, 로그인 아이디와 같으면 거부.
- 로그인 시도: 아이디당 15분 안에 5회까지(성공하면 초기화). 한도를 넘기면 그 순간부터 15분 잠깁니다. IP당 분당 60회 제한(IPv6는 /64 단위). 없는 아이디도 같은 시간(더미 해시 검증)과 같은 오류로 응답합니다.
- 관리자가 발급·초기화한 비밀번호는 임시 비밀번호이며, 로그인 후 변경하기 전까지 다른 API를 쓸 수 없습니다(`password_change_required`).

## 5. 권한 모델

### 역할
| 역할 | 설명 |
| --- | --- |
| `owner` | 최고 관리자. 모든 권한 + 관리자 임명·해제. 최소 1명 유지. |
| `admin` | 관리자. 구성원·근태·휴가·결재 양식·공지·설정 관리. 역할 변경은 불가. |
| `member` | 일반 구성원. |

### 조직장(파생 권한)
부서의 `head`로 지정된 구성원은 **그 부서와 하위 부서 전체**에 대해 조직장입니다. 역할이 아니라 조직 구조에서 계산됩니다.

### 조회·동작 범위
| 동작 | 본인 | 조직장(관할 부서) | admin/owner |
| --- | --- | --- | --- |
| 출퇴근 기록·근태 조회 | ○ | ○ | ○ |
| 출퇴근 기록 직접 생성·수정 | × (정정 요청) | × | ○ 다른 사람의 기록만 (사유 필수, 이력 보존) |
| 연차 수동 조정·휴가의 관리자 취소 | × | × | ○ 다른 사람의 것만 |
| 휴가 잔여·내역 조회 | ○ | ○ | ○ |
| 휴가 일정(달력) 조회 | 같은 부서 | ○ | ○ |
| 결재 문서 열람 | 기안자·결재선·참조자 | 문서의 당사자일 때만 | ○ (감사 로그 기록) |
| 진행 중 문서의 대리 반려 | × | × | ○ (본인이 기안한 문서 제외, 사유 필수, 감사 로그 기록) |
| 공지 작성 | × | × | ○ |
| 구성원 관리 | × | × | ○ |

**관리자의 본인 기록**: 위 두 줄의 "개인 기록을 관리자 권한으로 직접 다루는" 동작(`authz::edit_personal_records`)은 대상이 **다른 사람**이어야 합니다. `admin`이 자기 출퇴근 기록을 만들거나 고치고, 자기 연차를 조정하고, 자기 휴가를 관리자 권한으로 취소하려 하면 403이며 다른 관리자나 최고 관리자가 대신 처리합니다. `owner`만 예외로 본인 기록도 다룰 수 있습니다(최종 권한자이고 모든 동작이 감사 로그에 남습니다). 본인 자격의 동작(정정 요청, 본인 휴가 취소)은 누구든 일반 구성원과 같은 규칙으로 합니다.

**권한은 쓰기 트랜잭션 안에서 다시 읽습니다.** 인증(트랜잭션 밖)과 커밋 사이에 역할이 해제되거나 계정이 정지될 수 있으므로, 모든 관리자 쓰기 유스케이스는 트랜잭션 첫 줄에서 `support::require_current_admin`으로 지금의 권한을 확인하고(해제됐으면 403, 정지됐으면 401) 대상에 대한 판정도 그 값으로 합니다. 본인의 소속·입사일로 판정하는 신청(휴가 신청, 근태 정정 요청)은 `support::current_member`로 다시 읽은 값으로 최종 결재자 확인과 연차 부여를 합니다.

`GET /api/v1/me`는 `capabilities`(화면 노출용 불리언 묶음)를 함께 돌려줍니다. UI는 이 값으로 메뉴를 그리지만, **서버는 매 요청마다 다시 판정**합니다.

## 6. 도메인 규칙 요약

### 출퇴근
- 구성원·근무일당 기록 1건. 출근 → 퇴근 순서. 퇴근 후 재출근은 MVP에서 지원하지 않습니다(정정 요청으로 처리).
- 출근 재요청은 기존 기록을 돌려줍니다(멱등). 퇴근 재요청도 같습니다.
- 퇴근하지 않은 기록은 출근 후 24시간이 지나면 `missing_clock_out`으로 분류합니다. 그 전까지는 근무일이 바뀌어도(야간 근무) 퇴근할 수 있습니다.
- 근무 시간 = 퇴근 − 출근 − 휴게(4시간 이상 30분, 8시간 이상 60분. 설정 가능).
- 지각: 출근 > 시작 시각 + 유예. 조퇴: 퇴근 < 종료 시각. 휴가(반차)가 있으면 기준 시각을 조정합니다.
- 관리자 수정과 정정 승인은 `attendance_events`에 이전/이후 값을 남깁니다.
- 정정 요청은 오늘 근무일까지, 31일 전까지의 근무일에 대해 올릴 수 있고, 같은 근무일에 결재 중인 요청은 하나뿐입니다. 값은 요청할 때와 **최종 승인할 때** 두 번 검증합니다. 승인 시점의 기록이 요청 당시와 다르면(그사이 관리자가 고쳤거나 본인이 퇴근을 찍음) 반영하지 않고 승인이 409 `version_conflict`로 실패하며, 결재자는 반려로 끝냅니다.
- 정정 요청의 취소는 곧 문서 회수입니다(누군가 결재한 뒤에는 `withdraw_too_late`). 남은 결재자가 처리할 수 없게 된 요청은 관리자가 문서를 대리 반려해 끝내고, 그 뒤에는 같은 근무일을 다시 요청할 수 있습니다.

### 휴가
- 수량 단위는 **1/4일**(`LeaveAmount`, 정수). 연차 1일 = 4, 반차 = 2, 반반차 = 1.
- 잔여 = 원장(`leave_ledger`) 합계. 부여·사용·복원·조정·소멸 모두 원장에 한 줄씩 추가합니다(수정·삭제 없음).
- 연차 자동 부여(입사일 기준, 근로기준법 제60조):
  - 1년 미만: 1개월 개근 시 1일, 최대 11일.
  - 1년 이상: 15일. 3년 이상부터 2년마다 1일 가산, 최대 25일.
- **입사일을 고쳐도 이미 들어간 부여는 다시 쓰지 않습니다.** 원장은 추가 전용이라 지난 부여의 부여일·만료일·수량은 받은 그대로 남고, 고친 입사일은 아직 없는 차수(앞당겼다면 새로 생긴 근속 연수의 부여)와 앞으로의 부여에만 적용됩니다. 그 결과가 실제와 다르면(예: 입사일을 몇 년 앞당겨 두 해의 연차가 함께 유효해진 경우) 관리자가 수동 조정으로 바로잡습니다.
- 신청 일수는 근무 요일과 공휴일을 제외하고 셉니다. 기간이 겹치는 신청은 거부합니다.
- 신청 시점에 잔여를 **예약(차감)** 하고, 반려·취소·회수 시 복원합니다. 예약은 신청당 원장 한 줄이며 기준일은 **첫 근무일**입니다.
- 차감하는 신청은 두 조건을 모두 만족해야 접수됩니다(`leave::check_reservable`, 아니면 409 `insufficient_leave_balance`): ① 총 수량이 첫 근무일 기준 잔여 안에 들 것, ② 쉬는 근무일 **하루하루가 그날 유효한 부여로** 덮일 것(날짜순으로 하루치씩 차감해 확인). ②가 없으면 부여가 소멸한 뒤이거나 다음 부여를 아직 받지 않은 날까지 걸친 신청이 첫날 잔여만으로 통과합니다.
  - 한계: 부여 경계(기념일)를 넘는 신청도 전부 첫날에 유효하던 부여에서 차감됩니다. 첫날의 부여만으로 전체를 낼 수 없으면 경계 뒤의 부여가 넉넉해도 거부되므로, 그때는 경계에서 둘로 나눠 신청합니다.
- 취소와 결재 문서는 함께 움직입니다. 대기 중인 신청의 취소는 곧 문서 **회수**이며(누군가 결재한 뒤에는 회수할 수 없으므로 본인의 취소는 `withdraw_too_late`로 거부, 반려로 종료), **관리자의 취소**는 회수할 수 없는 문서를 대리 반려로 끝냅니다(신청은 문서와 같이 `rejected`, 잔여 복원. 그래서 관리자 취소는 `withdraw_too_late`가 되지 않습니다). 승인된 휴가의 취소는 문서를 승인된 기록으로 둔 채 신청만 취소하고 잔여를 복원합니다. 본인은 휴가 시작 전에만, 관리자는 언제든 취소할 수 있습니다.

### 공지·알림
- 공지를 게시하면 대상 범위의 재직 구성원에게 `notice.new` 알림이 가고, 알림에는 공지 제목이 들어 있습니다. 수정은 다시 알리지 않습니다.
- 공지를 **보관**하거나 수정으로 **대상을 좁히면**, 그 공지를 더는 볼 수 없게 된 구성원의 **읽지 않은** `notice.new` 알림을 같은 트랜잭션에서 지웁니다(볼 수 없는 공지의 제목이 알림함에 남지 않도록). 볼 수 있는지는 열람과 같은 규칙(`authz::audience_scope`)으로 정하고, 이미 읽은 알림은 받은 기록이므로 그대로 둡니다.
- **첨부파일**(공지·결재 문서 공통)은 내용까지 SQLite에 둡니다(`attachments` + 내용만 담는 `attachment_contents`. 목록·상세는 내용을 읽지 않음). 소유자는 `notice_id`·`document_id` 중 정확히 하나(실제 외래 키)라서 임시저장 문서를 지우면 첨부도 연쇄로 지워집니다. 이름·빈 파일·개수·해시 같은 공통 처리는 `service::attachments`, 내려받기 응답은 `http::download`에 있고, 권한·상태 확인과 감사 로그 이름은 각 기능 서비스가 정합니다.
  - **결재 문서**: 기안자가 임시저장 상태에서만 붙이고 뺍니다(`delete_document`와 같은 확인: 볼 수 없으면 404, 남의 문서면 403, 상신했으면 409 `document_not_editable`). 상신하면 결재자가 본 그대로 고정되고, 회수·반려·승인 뒤에도 읽기 전용입니다. 내려받기는 문서를 볼 수 있는 사람(`rules::view`)만 되고, 당사자가 아닌 관리자가 받으면 `approval.attachment.admin_download`를 남깁니다. 웹의 새 문서는 고른 파일이 있으면 임시저장으로 만들고 파일을 올린 뒤 상신합니다. 실행 파일 하나 + DB 파일 하나로 백업하는 원칙을 지키는 대신, 첨부만큼 DB가 커집니다.
  - 올리기는 관리자만, 보관되지 않은 공지에, 공지마다 10개(`attachment::MAX_PER_NOTICE`)까지입니다. 본문은 파일 내용 그대로(`application/octet-stream`), 이름은 `X-File-Name` 헤더(퍼센트 인코딩 UTF-8)로 받습니다. 이름을 쿼리 문자열로 받지 않는 것은 경로가 접근 로그에 남기 때문입니다. 크기 상한은 이 경로에만 따로 두며(`--max-attachment-mb`, `PANGO_MAX_ATTACHMENT_MB`, 기본 10 MiB) 다른 경로의 본문 상한은 그대로입니다. 이름은 `attachment::FileName`으로 검증합니다(경로 구분자·`.`만 있는 이름·제어 문자 거부). `Idempotency-Key`를 주면 한 번만 올라갑니다.
  - 내려받기는 공지 상세를 볼 수 있는 사람만 됩니다(보관됐거나 범위 밖이면 404, 다른 공지의 첨부 ID도 404). 올린 사람이 정한 형식을 믿지 않고 항상 `application/octet-stream` + `Content-Disposition: attachment` + CSP `sandbox`로 보내, HTML·SVG가 이 출처에서 열려 스크립트가 실행되지 않게 합니다.
  - 지우기는 내용까지 실제로 지웁니다. 잘못 올린 파일(개인정보 등)을 없앨 수 있어야 하므로 보관된 공지에서도 됩니다. 올리기·지우기는 감사 로그(`notice.attachment.add`·`.remove`)에 이름·크기·SHA-256으로 남습니다.
  - 웹은 쿠키로 인증되므로 내려받기를 일반 링크로 엽니다. 앱(Bearer 토큰)은 시스템 브라우저가 토큰을 실을 수 없으므로 아래의 **내려받기 링크**를 씁니다.
- **내려받기 링크**(앱용, 공지 첨부와 근태 CSV): 로그인한 세션이 `POST /download-links`로 경로 하나에 대한 5분짜리 서명(`세션ID.만료.HMAC`)을 받아 쿼리 끝(`download_token`)에 붙이고, 앱은 서버 주소를 붙여 시스템 브라우저로 엽니다(웹뷰는 외부 주소 이동을 시스템 브라우저로 넘긴다). 서명은 토큰 앞까지의 경로·쿼리 전체와 세션에 걸리므로 다른 파일·기간으로 바꿔 쓸 수 없고, 로그아웃·기기 로그아웃하면 함께 무효가 됩니다. 키는 실행할 때마다 새로 만듭니다. 서명할 수 있는 경로는 내려받기 경로(공지·결재 문서 첨부, 근태 CSV)뿐이고, 열 때 권한은 원래 경로가 다시 확인합니다. `DownloadAuth` 추출기는 쿼리에 `download_token`이 있으면 그것으로만 인증합니다(시스템 브라우저에 남은 만료된 세션 쿠키가 링크를 막지 않게). 서버 로그에는 경로만 남기지만(쿼리 제외) 앞단 프록시의 접근 로그에는 토큰이 남을 수 있어 몇 분 안에 만료되게 했습니다.


### 외부 알림(웹훅)
앱 내 알림을 외부로도 보냅니다. 채널은 **서명한 JSON을 POST하는 범용 웹훅**과 **사내 메신저 Buzz DM**(아래) 둘이고, 같은 아웃박스·발송 작업·재시도 규칙을 씁니다.

- **아웃박스**: `support::notify`가 알림을 **새로 만들었을 때만**(`dedupe_key`로 건너뛰면 아니다) 같은 트랜잭션에서 `outbound_messages`에 한 줄을 남깁니다. 조건은 연동이 켜져 있고, 받는 사람이 재직 중이고, 외부 알림을 끄지 않았을 것(`member_notification_prefs`, 행이 없으면 받음). 알림·채널마다 한 줄(유일 제약)이라 같은 알림이 두 번 나가지 않습니다. 알림이 지워지면(보관된 공지의 읽지 않은 알림) 보낼 일도 함께 지워집니다(`ON DELETE CASCADE`).
- **발송 작업**(`outbound::run_forever`, 5초마다): 쓰기 트랜잭션에서 `pending`이고 `next_attempt_at`이 지난 줄을 최대 20개 집으며 `attempts`를 올리고 `next_attempt_at`을 다음 재시도 시각으로 미룹니다. 커밋한 뒤 **트랜잭션 밖에서** 동시에 보내고, 결과를 짧은 트랜잭션으로 남깁니다. 집기가 직렬화된 쓰기 안에서 일어나므로 작업이 겹쳐도 같은 줄을 두 번 집지 않고, 보내는 중 서버가 멈추면 그 시각에 **한 번 더** 보낼 뿐입니다(그래서 받는 쪽은 `X-Pango-Delivery`로 중복을 거릅니다). 집은 뒤 연동이 꺼졌거나 받는 사람이 퇴사·수신 거부했으면 보내지 않고 `failed`로 끝냅니다.
- **재시도**(`pango_core::outbound`): 2xx는 `sent`. 408·429·5xx·연결 실패·시간 초과는 30초, 1분, 2분 … 두 배씩(최대 1시간) 미뤄 다시 보내고, 8번 시도하면 `failed`. 그 밖의 응답(3xx·4xx)은 바로 `failed`. `last_error`에는 `http_404`, `timeout` 같은 짧은 코드만 남깁니다(응답 본문·주소·비밀 값 없음). 외부 발송의 실패는 앱 알림과 업무 트랜잭션에 영향을 주지 않습니다.
- **정리**: 30일 지난 `sent`·`failed` 줄은 주기 정리 작업이 지웁니다.
- **연결할 주소(SSRF 방어)**: 관리자가 넣는 주소는 `https`만 받고(개발용 `localhost`·`127.0.0.1`·`[::1]`만 `http`), 사용자 정보·조각은 거부합니다(`WebhookUrl::parse`). 보낼 때는 발송기의 DNS 해석기가 이름을 풀어 `outbound::ip_allowed`를 통과한 IP만 돌려주므로 확인한 주소로만 연결합니다(DNS 재바인딩 방지). IP를 직접 쓴 주소도 같은 규칙으로 확인하고, 시스템 프록시와 리다이렉트는 쓰지 않습니다. 클라우드 메타데이터·link-local(`169.254.0.0/16`, `fe80::/10`, `fd00:ec2::254`)·미지정·멀티캐스트·브로드캐스트·예약(`240.0.0.0/4`) 대역은 **항상** 막고, 루프백·사설망(`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`)·CGNAT(`100.64/10`)는 서버 실행 옵션 `--outbound-allow-private`(`PANGO_OUTBOUND_ALLOW_PRIVATE`)을 켰을 때만 엽니다(관리자 화면에서는 바꿀 수 없음). 그래서 개발용 `http://localhost` 주소도 이 옵션 없이는 실제로 보내지 않습니다. IPv4를 감싼 IPv6(`::ffff:…`, `64:ff9b::/96`)는 안의 IPv4로 판정합니다. 막히면 재시도 없이 `failed`, 사유는 `blocked_address`뿐입니다(해석한 IP를 드러내지 않음).
- **서명 키**: 256비트 난수의 16진수 64자. DB에 원문으로 두고(HMAC 키이므로) API는 `has_secret`만 돌려줍니다. 새로 만들 때 응답으로 **한 번만** 보여 주고, 감사 로그에는 바꿨다는 사실만 남깁니다. 감사 로그의 주소는 출처(`https://host`)만 남깁니다(경로·쿼리에 받는 쪽 토큰이 있을 수 있음).

요청(연결 5초, 전체 10초, 응답 본문은 읽지 않음):

```http
POST {웹훅 주소}
Content-Type: application/json
X-Pango-Event: notification            (테스트 발송은 test)
X-Pango-Delivery: 0193f2c4-…           (아웃박스 ID. 재시도해도 같다)
X-Pango-Timestamp: 1767225600          (유닉스 초)
X-Pango-Signature: sha256=<hex>        (HMAC-SHA256(key = 서명 키 문자열, "{timestamp}.{본문}"))

{
  "event": "notification",
  "delivery_id": "0193f2c4-…",
  "kind": "approval.todo",
  "title": "결재 요청: 출장비 정산",
  "body": "김팡고 님이 결재를 요청했어요.",
  "link": "https://gw.example.com/approval/documents/0193…",
  "recipient": { "login_id": "kim", "name": "김팡고", "email": "kim@example.com" },
  "created_at": 1767225600000
}
```

본문은 앱 알림함에 이미 보이는 값(종류·제목·본문·링크)과 메신저에서 받는 사람을 찾는 데 필요한 값(아이디·이름·이메일)만 담습니다. 휴가 사유나 문서 내용처럼 알림에 없는 값은 넣지 않습니다. `link`는 `--public-origin`이 있으면 절대 주소, 없으면 앱 안의 경로입니다.

받는 쪽의 서명 확인 예(Python):

```python
import hashlib, hmac, time

def verify(secret: str, headers, body: bytes) -> bool:
    ts = headers["X-Pango-Timestamp"]
    if abs(time.time() - int(ts)) > 300:      # 5분보다 오래된 요청은 버린다(재전송 방지)
        return False
    expected = "sha256=" + hmac.new(secret.encode(), f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, headers["X-Pango-Signature"])
```

관리 API(관리자만, 다른 역할은 403, 쓰기는 트랜잭션 안에서 권한을 다시 읽고 감사 로그를 남김):

| 경로 | 본문 → 응답 |
| --- | --- |
| `GET /admin/integrations/webhook` | → `{enabled, url, has_secret, version, updated_at, stats: {pending, sent_24h, failed_24h, last_error, last_error_at}}` |
| `PUT /admin/integrations/webhook` | `{version, enabled, url}` → 위와 같음. 켜려면 주소(`webhook_url` `empty`)와 서명 키(`webhook_secret` `empty`)가 있어야 함. 버전이 낡으면 409 |
| `POST /admin/integrations/webhook/secret` | `{version}` → `{secret, settings}`. 키는 이 응답에서만 보임 |
| `POST /admin/integrations/webhook/test` | → `{delivered, status, error}`. 저장한 주소로(꺼져 있어도) 요청한 관리자에게 바로 보냄. 10초에 한 번(429), 감사 로그 `integration.webhook_test` |
| `GET·PUT /me/notification-preferences` `GET·POST·DELETE /me/buzz` `POST /me/buzz/verify` | `{external_enabled}` → `{external_available, external_enabled}`(본인 것만). 웹훅·Buzz 둘 다에 적용 |

#### Buzz DM 채널
사내 메신저 Buzz(Nostr 릴레이)의 DM으로도 보냅니다. 운영 설정은 [BUZZ.md](BUZZ.md)에 있습니다.

- **설정**: 릴레이 주소와 봇 비밀키는 서버 실행 설정(`PANGO_BUZZ_RELAY_URL`, `PANGO_BUZZ_BOT_KEY_FILE` 또는 `PANGO_BUZZ_BOT_KEY`)에만 둡니다. DB에는 켜기·끄기(`buzz_settings`, 버전·감사 로그)와 구성원 연결(`buzz_accounts`)만 있고, 관리자 화면은 상태(설정 여부, 릴레이 주소, 봇 npub, 연결한 구성원 수, 발송 현황)만 보여 줍니다. 비밀키(`nostr::BotKey`)는 `Debug`·오류·로그에 드러나지 않습니다. 릴레이 주소에도 웹훅과 같은 주소 확인(SSRF 방어)을 씁니다.
- **아웃박스**: 같은 `outbound_messages`에 `channel = 'buzz'` 줄을 남깁니다. 조건은 Buzz 채널이 켜져 있고, 받는 사람이 Buzz 계정 확인을 마쳤고(`verified`), 재직 중이고, 외부 알림을 끄지 않았을 것. 웹훅과 따로 판단하므로 둘 다 갈 수 있습니다. 보낼 때 연결을 다시 확인해, 그사이 끊었으면 `recipient_unlinked`로 끝냅니다. 발송기(`OutboundSender`)는 보낼 곳(`Target::Webhook | Target::Buzz`)에 따라 `ChannelSender`가 채널별 발송기로 나눕니다.
- **요청**: `POST {릴레이}/events`에 봇 키로 서명한 이벤트(NIP-01 ID, BIP340 Schnorr 서명)를 보냅니다. 인증은 NIP-98 `Authorization: Nostr <base64(kind 27235)>`이고 태그는 `u` = `{릴레이}/events`, `method` = `POST`, `payload` = 본문 SHA-256(늘 넣음)입니다.
  1. DM 열기 — kind 41010, 내용 없음, `["p", 받는 사람 hex]`. 이미 있으면 같은 DM. 응답 `message`의 `response:{"channel_id": …}`를 `buzz_accounts.dm_channel_id`에 저장해 다음부터 다시 씁니다.
  2. 메시지 — kind 9, 내용 `제목\n짧은 본문\n링크`(빈 부분은 뺌, 8KiB 이하), 태그 `["h", 채널]`, `["ms", 초 아래 밀리초 0~999]`(Buzz SDK `build_message`와 같음).
  저장해 둔 DM에 보내다 거절되면(401 제외) 한 번 다시 열어 봅니다. 401·403·그 밖의 4xx·`accepted: false`는 바로 실패, 408·429·5xx·연결 실패는 웹훅과 같은 간격으로 다시 보냅니다. 응답 본문은 64KiB까지만 읽고 저장하지 않습니다.
- **구성원 연결**(Buzz는 이메일로 사용자를 찾을 수 없음): 구성원이 `npub1…`(또는 16진수)를 넣으면 봇이 DM으로 6자리 코드를 **바로**(아웃박스를 거치지 않고) 보냅니다. 코드는 해시만 저장하고 10분 유효(만료 시각 포함 거부), 5번 틀리면 끝, 다시 보내기는 60초 간격·시간당 5번(`pango_core::buzz`). 제한은 발송 전에 쓰기 트랜잭션에서 걸므로 동시에 눌러도 한 번만 보냅니다. 한 Buzz 계정은 한 구성원만 확인할 수 있습니다(부분 유일 색인). 관리자는 연결을 끊을 수만 있고 대신 연결할 수는 없습니다. 감사 로그의 공개키는 `npub1abcd…wxyz`로 줄여 남깁니다.
- **실패 안내**(연결 중): 401·403 → 502 `buzz_bot_rejected`(관리자 문의), 그 밖의 4xx·거절 → 502 `buzz_recipient_rejected`, 연결 실패·5xx·429·막힌 주소 → 502 `buzz_unreachable`.

| 경로 | 본문 → 응답 |
| --- | --- |
| `GET /admin/integrations/buzz` | → `{configured, enabled, relay_url, bot_npub, linked_members, version, updated_at, stats}` |
| `PUT /admin/integrations/buzz` | `{version, enabled}` → 위와 같음. 실행 설정이 없으면 켤 수 없음(409 `buzz_unavailable`) |
| `GET·DELETE /admin/members/{id}/buzz` | → `{status, npub_short}`. 해제만(감사 로그 `buzz.admin_unlink`) |
| `GET /me/buzz` | → `{available, status: none·pending·verified, npub, code_expires_at, attempts_left, resend_available_at, bot_npub}` |
| `POST /me/buzz` | `{pubkey}` → 위와 같음. 코드 발송(429 제한, 409 `buzz_account_taken`·`buzz_unavailable`, 502 발송 실패) |
| `POST /me/buzz/verify` | `{code}` → 위와 같음. 409 `buzz_code_wrong`·`buzz_code_expired`·`buzz_code_exhausted`·`buzz_code_missing`·`buzz_account_taken` |
| `DELETE /me/buzz` | → 위와 같음(`status: none`) |

### 캘린더
- 캘린더는 세 종류입니다. 권한은 `pango_core::calendar_access`(`can_read`·`can_write`·`event_role`)에서만 판정합니다.

  | 캘린더 | 열람 | 쓰기 |
  | --- | --- | --- |
  | 내 일정(`personal`) | 본인만. 관리자·최고 관리자도 볼 수 없음 | 본인 |
  | 전사 일정(`company`) | 모두 | 관리자 |
  | 부서 일정(`department`) | 그 부서와 하위 부서 소속, 관할 조직장, 관리자 | 관할 조직장(상위 부서 조직장 포함), 관리자 |

  볼 수 없는 캘린더·일정은 404, 보이지만 쓸 수 없으면 403입니다. 보관된 부서의 캘린더와 일정은 없는 것으로 봅니다. 쓰기 권한은 쓰기 트랜잭션 안에서 다시 읽은 값으로 판정합니다(`support::current_actor`).
- **캘린더 ID**는 주인에게서 정해집니다: 내 일정 = 구성원 ID, 부서 일정 = 부서 ID, 전사 일정 = 고정 ID `00000000-0000-7000-8000-000000000001`(마이그레이션 `0005_calendar.sql`이 행을 만듦). 내 일정·부서 일정의 `calendars` 행은 첫 일정을 만들 때 `INSERT … ON CONFLICT DO NOTHING`으로 생기므로, 행이 없어도 목록과 권한 판정은 같은 ID를 씁니다. 이름은 저장하지 않고 조회할 때 붙입니다(부서 이름 변경을 따라가도록).
- **일정 시간**: 종일 일정은 회사 시간대와 무관한 날짜(`start_date`~`end_date`, 양 끝 포함, `end_date` 생략 시 하루), 시간 일정은 `start_at < end_at`인 시각입니다. 둘을 섞어 보내면 400(`inconsistent`). 길이는 최대 31일(종일 31개 날짜, 시간 31×24시간)이고 날짜·시각은 1900~9999년만 받습니다. 제목은 `Title`(200자), 장소 200자(한 줄), 설명 5,000자.
- **조회**: 캘린더 화면은 양 끝 포함 62일까지 한 번에 읽고, 여러 날에 걸친 일정도 한 번만 돌려줍니다(배치는 화면이 함). 시간 일정은 회사 시간대의 자정(근무일 전환 시각이 아님)으로 날짜를 나눕니다. 초대받은 다른 사람의 일정은 `calendars`에 내 일정이 포함될 때 함께 나옵니다.
- **함께 보이는 정보**(읽기 전용, 일정 표에 복사하지 않음): 회사 공휴일, 그리고 `authz::view_leave_schedule`로 볼 수 있는 구성원의 결재 중·승인 휴가(휴가 달력과 같은 `leave::schedule_tx`, 이름·날짜·단위·상태만, 사유·종류 없음). 생일은 저장된 값이 없어 다루지 않습니다.
- **참석자**는 내 일정에만 둡니다(최대 50명, 본인·중복 불가, 공유 캘린더에 보내면 400). 새로 초대하는 사람은 디렉터리에서 볼 수 있어야 하고(아니면 404) 재직 중이어야 합니다(409 `member_inactive`). 이미 초대된 사람은 그사이 정지·퇴사해도 목록에 남을 수 있습니다. 참석자는 일정을 보고 응답(`accepted`/`declined`)만 할 수 있고 고칠 수 없습니다(403). 같은 응답을 다시 보내면 아무것도 바꾸지 않습니다. 참석자에서 빠지면 그 일정은 다시 404입니다.
- **알림**: 초대 `calendar.invited`(일정·참석자마다 한 번, 빠졌다 다시 초대돼도 다시 보내지 않음), 시간·장소 변경 `calendar.updated`(남은 참석자, 버전마다), 참석자 제외·일정 삭제 `calendar.canceled`, 응답 변경 `calendar.responded`(주인에게, 일정·참석자·응답마다 한 번).
- **삭제**는 되돌릴 수 없는 실제 삭제입니다(일정은 업무 기록이 아님, 참석자도 함께 지워짐). 전사·부서 일정의 생성·수정·삭제는 감사 로그(`calendar.event_create|update|delete`)에 남기고, 개인 일정은 남기지 않습니다.
- **참석자 일정 확인**(`GET /calendar/conflicts`, 7일·50명 이하): 구성원마다 본인 일정, 거절하지 않은 초대, 승인된 휴가를 바쁜 시간으로 돌려줍니다. 요청자가 주인이거나 초대받은 일정만 제목이 있는 `event`이고, 나머지는 시간만 있는 `busy`입니다. 휴가는 휴가 일정을 볼 수 있는 구성원이면 `leave`(단위만), 아니면 종일 `busy`입니다. 전사·부서 일정은 개인의 바쁜 시간으로 치지 않습니다.
- 반복 일정과 미리 알림은 MVP 범위가 아닙니다.

### 전자결재
- 문서 상태: `draft → in_progress → approved | rejected | withdrawn`.
- 결재선은 순차 단계입니다. 단계 종류: `approve`(결재), `agree`(합의). 참조자는 단계가 아니며 열람만 합니다.
- 현재 단계의 결재자만 처리할 수 있습니다. 반려는 즉시 문서를 종료합니다.
- 회수는 기안자가 **아무도 처리하기 전**에만 할 수 있습니다.
- **관리자의 대리 반려**: 차례인 결재자가 정지·퇴사하면 그 문서는 아무도 끝낼 수 없습니다(회수는 이미 늦었고 다른 사람은 차례가 아님). 그래서 관리자는 `POST /admin/approval/documents/{id}/reject`(`{reason}`)로 진행 중인 문서를 **지금 차례인 단계에서** 반려로 끝낼 수 있습니다(`Document::force_reject`). 결재자가 재직 중이어도 쓸 수 있습니다.
  - 결과는 결재자가 직접 반려한 것과 같습니다: 그 단계가 `rejected`(사유 기록), 뒤 단계는 `waiting`, 문서는 `rejected`. 연결 효과(휴가 잔여 복원, 근태 정정 요청 종료)도 같은 트랜잭션에서 실행합니다.
  - 누가 끝냈는지는 이력(`approval_events`의 `rejected` 행위자 = 관리자)과 감사 로그 `approval.admin_reject`(단계, 건너뛴 결재자, 사유)에 남습니다. 기안자(`approval.rejected`)와 건너뛴 결재자(`approval.admin_rejected`)에게 알립니다.
  - 관리자만 할 수 있고(`authz::override_document`), **본인이 기안한 문서에는 쓸 수 없습니다**(403, 최고 관리자 포함). 임시저장 문서는 404, 승인·회수로 끝난 문서는 409 `document_already_closed`, 이미 반려된 문서는 아무것도 바꾸지 않고 성공합니다(자연 멱등).
- 기안자는 자신을 결재자로 넣을 수 없습니다. 같은 사람이 결재선에 두 번 나올 수 없습니다.
- 휴가 신청·근태 정정 같은 **개인 신청의 최종 결재자는 신청자의 관할 조직장 또는 관리자**여야 합니다. 결재선은 신청자가 고칠 수 있으므로, 신청할 때와 최종 승인할 때 모두 확인합니다(동료끼리 서로 승인하는 것을 막기 위해). 앞 단계(합의)에는 누구든 넣을 수 있습니다.
- 휴가 신청·근태 정정은 **시스템 양식의 결재 문서**입니다. 최종 승인·반려·회수 시 같은 트랜잭션에서 연결된 효과(휴가 확정/복원, 기록 정정)를 실행합니다.

#### 양식
- 양식은 **분류**(`FormCategory`: `general` 일반 · `hr` 인사 · `attendance` 근태 · `trip` 출장 · `accounting` 회계 · `support` 지원 · `welfare` 복지 · `etc` 기타)와 **필드 정의**(`pango_core::form_schema::FormSchema`)를 가집니다. `use_body`가 참이면 자유 입력 본문이 필수이고, 거짓이면 본문 없이 필드만으로 문서를 냅니다(본문도 필수 필드도 없는 양식은 만들 수 없음).
- 필드 정의: `{"fields": [{"key", "label", "kind", "required", "help"?, …종류별 속성}]}`. 필드는 최대 40개, `key`는 소문자로 시작하는 `snake_case`(40자 이하, 양식 안에서 유일)입니다.

  | `kind` | 속성 | 값 |
  | --- | --- | --- |
  | `text` / `textarea` | `max_len`(기본이자 최대 200 / 5,000) | 문자열(`text`는 한 줄). `text` 규칙으로 정규화 |
  | `number` | `min`, `max`(생략 가능, ±10^15 안) | 정수 |
  | `money` | | 0~10^12의 정수(원) |
  | `date` | | `"YYYY-MM-DD"` |
  | `date_range` | | `{"start", "end"}` 양 끝 포함, 최대 366일 |
  | `time_range` | | `{"start": "HH:MM", "end": "HH:MM"}` 같은 날, 시작 < 끝 |
  | `select` | `options`(1~30개, 각 50자 이하, 중복 없음) | 선택지 중 하나 |
  | `checkbox` | | 불리언(필수면 반드시 `true`) |
  | `member` | | 재직 중인 구성원의 ID |
  | `table` | `columns`(1~8개, 종류는 text·number·money·date·select) | 행의 배열(`{열 key: 값}`), 최대 50행. 필수면 1행 이상 |

- **값 검증**(`FormSchema::validate_values`)은 임시저장·수정·상신 모두에서 합니다. 양식에 없는 키는 `unknown_field`, 필수 누락은 `empty`, 선택지에 없는 값은 `not_an_option`, 없거나 재직 중이 아닌 구성원은 `not_found`·`member_inactive`이며 모든 오류를 한 번에 돌려줍니다(위 `fields`). 없는 키·`null`·공백뿐인 문자열·빈 표는 "입력하지 않음"입니다.
- **계산값**: 금액 열이 있는 표는 열별 합계(`{"totals": {열 key: 합계}}`), 기간은 일수(`{"days": n}`)를 서버가 계산해 필드 키 아래에 둡니다.
- **문서는 낸 시점의 양식을 갖고 다닙니다.** 일반 양식 문서의 `payload`는 `{"form_schema_version", "category", "use_body", "schema", "values", "computed"}`이고, 상세 응답은 이것을 `form_snapshot`(분류 이름과 `member` 필드의 구성원 이름 포함)으로 내려 줍니다. 그래서 양식을 고쳐도 이미 낸 문서는 낼 때의 모양대로 보입니다. 시스템 양식 문서의 `payload`는 그 기능이 정한 그대로이고 `form_snapshot`이 없습니다. 이 기능 이전의 일반 문서는 필드 없는 양식(`schema_version` 0)으로 보입니다.
- 필드 정의를 고치면 `schema_version`이 1 오릅니다(같은 정의를 다시 보내면 그대로). **임시저장 문서는 상신할 때 지금의 정의로 다시 검증**하고 그 정의를 갖습니다. 맞지 않으면 상신이 400으로 실패하고 기안자가 고쳐야 합니다.
- **기본 참조자**(`default_reference_ids`, 30명 이하): 상신할 때 문서의 참조자에 더합니다(예: 회계 양식 → 재무 담당자). 이미 당사자인 사람은 건너뛰고, 재직 중이 아니거나 참조자 한도를 넘는 사람은 상신을 막지 않고 빼되 감사 로그 `approval.default_reference_skipped`에 남깁니다.
- **기본 양식(카탈로그)**: `crates/server/src/service/form_catalog.json`의 27개 양식(일반 3 · 인사 9 · 근태 3 · 출장 2 · 회계 2 · 지원 5 · 복지 3, 시스템 양식 2개 포함)을 최초 설정이 넣습니다. 서버는 시작할 때마다 `code`로 **없는 것만** 채우므로(`ensure_form_catalog`, 넣었으면 감사 로그 `approval_form.catalog_seed`) 판올림으로 생긴 양식이 기존 회사에도 들어갑니다. 관리자가 고친 양식은 덮어쓰지 않고, 양식은 지우지 않고 끄기만 하므로 꺼 둔 기본 양식이 다시 생기지 않습니다. 양식의 출처는 `origin`(`catalog`/`custom`)에 남습니다.
- **양식 관리**(관리자): 만들기·고치기에서 분류·필드 정의·본문 사용·기본 참조자를 정합니다(수정은 보낸 것만 바뀜). 필드 정의의 오류 위치는 `schema.fields[0].key` 같은 경로입니다. `POST /admin/approval/forms/{id}/duplicate`(본문 `{}`)는 같은 내용의 **꺼진** 사본을 `<code>_copy_N` 코드로 만듭니다. 시스템 양식은 끌 수 없고 분류·필드 정의·본문 사용·기본 참조자를 바꾸거나 복제할 수 없습니다(409 `in_use`).
- `GET /approval/forms`는 사용 중인 양식을 분류별로 묶어 줍니다: `[{category, category_label, forms: [{id, code, name, description, kind, body_template, use_body, schema, schema_version, system, route}]}]`. 시스템 양식은 `system: true`이고 `route`(`leave_new`·`attendance`, 일반은 `approval_new`)가 문서를 만드는 화면을 가리킵니다. 문서함은 `?form_id=`·`?category=`(양식의 지금 분류)로 거를 수 있습니다.

## 7. HTTP API

접두사 `/api/v1`. 인증이 필요 없는 것은 `POST /auth/login`, `GET /health`뿐입니다.

| 영역 | 경로 |
| --- | --- |
| 인증 | `POST /auth/login` `POST /auth/logout` `GET /me` `POST /me/password` `GET /me/sessions` `DELETE /me/sessions/{id}` |
| 홈 | `GET /home` |
| 조직 | `GET /departments` `GET /positions` `GET /members` `GET /members/{id}` |
| 출퇴근 | `GET /attendance/today` `POST /attendance/clock-in` `POST /attendance/clock-out` `GET /attendance/records` `GET /attendance/summary` `GET·POST /attendance/corrections` `POST /attendance/corrections/{id}/cancel` |
| 휴가 | `GET /leave/types` `GET /leave/balance` `GET /leave/ledger` `GET /leave/requests` `POST /leave/requests` `POST /leave/requests/{id}/cancel` `GET /leave/calendar` |
| 조직장 | `GET /team/attendance` `GET /team/leave` `GET /team/members` |
| 결재 | `GET /approval/forms` `GET /approval/counts` `GET /approval/line-suggestion` `GET /approval/documents` `POST /approval/documents` `GET·PATCH·DELETE /approval/documents/{id}` `POST /approval/documents/{id}/{submit,approve,reject,withdraw}` `POST /admin/approval/documents/{id}/reject` `POST /admin/approval/forms/{id}/duplicate` |
| 공지 | `GET /notices` `GET /notices/{id}` `POST /notices/{id}/read` `GET /notices/{id}/attachments/{attachment_id}` `POST /admin/notices/{id}/attachments` `DELETE /admin/notices/{id}/attachments/{attachment_id}` |
| 캘린더 | `GET /calendar/calendars` `GET /calendar/view` `GET /calendar/conflicts` `POST /calendar/events` `GET·PATCH·DELETE /calendar/events/{id}` `POST /calendar/events/{id}/respond` |
| 알림 | `GET /notifications` `POST /notifications/{id}/read` `POST /notifications/read-all` `GET·PUT /me/notification-preferences` |
| 관리 | `/admin/members…` `/admin/departments…` `/admin/positions…` `/admin/attendance…` `/admin/leave…` `/admin/approval/forms…` `/admin/notices…` `/admin/holidays…` `/admin/settings` `/admin/integrations/webhook…` `/admin/integrations/buzz` `/admin/members/{id}/buzz` `/admin/audit-log` `/admin/dashboard` |

## 8. 화면 구성 (UI/UX)

레퍼런스는 **flex**(flex.team)의 정보 구조와 상호작용입니다: 카드형 홈, 큰 출퇴근 버튼, 결재함 탭. 브랜드·자산은 가져오지 않고 자체 디자인 토큰을 씁니다.

- **데스크톱(≥ 960px)**: 좌측 사이드바 + 본문.
- **모바일**: 상단 제목 + 하단 탭(홈·근태·결재·휴가·더보기). iOS/Android 앱도 같은 화면.

| 대상 | 메뉴 |
| --- | --- |
| 모든 구성원 | 홈, 근태, 휴가, 결재, 공지, 알림, 구성원(조직도), 내 정보 |
| 조직장 | + 팀(오늘 출근 현황, 팀 근태, 팀 휴가 달력) |
| admin / owner | + 관리(대시보드, 구성원, 조직, 근태, 휴가, 결재 양식, 공지, 근무 정책·공휴일, 외부 알림 연동, 감사 로그, 설정) |

## 9. 테스트 원칙

테스트를 먼저 씁니다. 기능마다 아래 분류를 점검합니다.

| 분류 | 예 |
| --- | --- |
| 해피 | 정상 출근 → 퇴근 |
| 네거티브 | 출근 없이 퇴근, 권한 없는 수정 |
| 경계값 | 유예 시간 정확히 0초/1초 초과, 연차 0·잔여와 같은 수량 |
| 엣지·코너 | 자정·근무일 전환 시각, 윤년 2/29 입사, 부서 트리 순환 |
| 동시성 | 같은 구성원의 동시 출근, 같은 단계 동시 승인 |
| 멱등성 | 같은 키 재전송, 승인 두 번 |
| 순서 역전 | 반려 후 승인, 회수 후 승인, 퇴근 후 출근 |
| 부분 실패 | 효과 실행 중 오류 시 전체 롤백 |
| 기형·악의적 입력 | 초대형 본문, 제어문자, SQL/스크립트 조각, 잘못된 UTF-8, 타인 ID |

- core: 단위 테스트 + 속성 테스트(proptest).
- server: 실제 SQLite 파일을 쓰는 통합 테스트. HTTP 계층까지 `tower::ServiceExt::oneshot`으로 검증.
- 보안에 닿는 변경은 허용·거부·실패 경로를 모두 테스트합니다.
