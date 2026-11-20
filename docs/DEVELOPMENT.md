# 개발 가이드

설계 계약은 [ARCHITECTURE.md](ARCHITECTURE.md)입니다. 이 문서는 **기능을 추가하는 방법**을 정리합니다.

## 명령

```bash
cargo test --workspace                                   # 전체 테스트
cargo clippy --workspace --all-targets -- -D warnings    # 린트 (경고 0)
cargo fmt --all                                          # 포맷
```

## 기능 하나를 추가하는 순서

기준 구현은 인증 기능입니다. 새 기능은 같은 모양으로 만듭니다.

| 단계 | 위치 | 기준 구현 |
| --- | --- | --- |
| 1. 규칙 | `crates/core/src/<기능>.rs` | `attendance.rs`, `leave.rs`, `approval.rs` |
| 2. 요청/응답 타입 | `crates/api/src/<기능>.rs` | `auth.rs`, `org.rs` |
| 3. 저장소 | `crates/db/src/<기능>.rs` (+ `tests/`) | `members.rs`, `sessions.rs`, `tests/spine.rs` |
| 4. 유스케이스 | `crates/server/src/service/<기능>.rs` | `service/auth.rs` |
| 5. HTTP | `crates/server/src/http/<기능>.rs` | `http/auth.rs` |
| 6. 통합 테스트 | `crates/server/tests/<기능>.rs` | `tests/auth.rs` |

모듈을 만들면 `lib.rs`/`mod.rs`의 `pub mod` 목록과 `http/mod.rs`의 `api_routes()`에 한 줄씩 추가합니다.

## 규칙

### 계층
- **핸들러**(`http/`)는 추출 → 서비스 호출 → `Json` 반환만 합니다. 조건문이 필요하면 서비스로 옮깁니다.
- **서비스**는 첫 줄에서 권한을 확인합니다. 역할을 직접 비교하지 말고 `pango_core::authz` 함수를 씁니다. 필요한 판정이 없으면 `authz.rs`에 함수와 테스트를 추가합니다.
- **저장소**(`db/`)에는 규칙을 넣지 않습니다. SQL과 행 ↔ 값 변환만 둡니다. 값 변환은 `codec` 모듈을 씁니다.
- 서비스는 `pango-api` 타입을 직접 돌려줍니다. 중간 타입을 따로 만들지 않습니다.

### 쓰기
- 쓰기 유스케이스는 `state.write(async |conn| { … })` 한 번입니다. 그 안의 모든 변경은 함께 커밋되거나 함께 롤백됩니다.
- 구성원 수에 비례하는 긴 조회는 `state.write` 안에서 하지 않습니다. `state.read_snapshot()`에서 계산하고(구성원마다 조회하지 말고 묶어 읽기), 꼭 써야 하는 것만 짧은 `state.write`로 씁니다.
- 생성 요청은 `IdempotentJson` 추출기 + `state.write_idempotent(…)`를 씁니다.
- 수정 요청은 `version`을 받고, 저장소의 `update`가 `false`를 돌려주면 `support::require_updated`로 409를 냅니다.
- 관리자 쓰기는 트랜잭션 **안의 첫 줄**에서 `support::require_current_admin(conn, ctx)`로 권한을 다시 확인하고, 대상에 대한 판정(`authz::edit_personal_records` 등)에는 거기서 받은 `Actor`를 씁니다. `ctx.actor`는 인증 시점의 값이라 그사이 해제된 관리자를 걸러 내지 못합니다. 요청자 본인의 소속·입사일이 필요한 쓰기는 `support::current_member`로 다시 읽습니다.
- 계정·권한·기록을 바꾸면 같은 트랜잭션에서 `support::audit`을 호출합니다. 비밀번호·토큰은 `detail`에 넣지 않습니다.
- 알림은 `support::notify`로 만들고, 같은 사건이 두 번 알림을 만들지 않도록 `dedupe_key`를 줍니다.
- 현재 시각은 `state.now()`만 씁니다.

### 공용 도우미
같은 일을 새로 쓰지 말고 아래를 씁니다. 없는 것이 두 곳 이상에서 필요해지면 여기에 추가합니다.

| 무엇 | 어디 |
| --- | --- |
| 목록 페이지 | `support::next_page(items, has_more, \|last\| 정렬 키)`로 만들고, 받은 커서는 `PageQuery::after::<정렬 키>()`로 읽습니다(16진수 JSON 커서 하나뿐, 형식이 틀리면 400 `cursor`). 필터가 있는 쿼리 DTO는 `PageQuery::of(self.limit, &self.cursor)`로 `page()`를 만듭니다. |
| 구성원 묶어 읽기 | `support::members_by_id`(ID → 구성원), `support::member_names`(ID → 이름) |
| 개인 기록의 대상 | `support::personal_records_member(conn, ctx, member_id)` — 생략하면 본인, 볼 수 없으면 404 |
| 본인 것만 | `authz::require_own(actor, owner_id, 거부 오류)` — 볼 수 없는 자원이면 `NotFound`, 볼 수는 있으면 `Forbidden` |
| 부서 | `support::department_subtree`(하위 부서까지), `support::department_tree`/`department_tree_of`, `support::department_names`, `org::active_department`/`active_position`(보관됐으면 404), 재직자는 `MemberFilter::active()` |
| 건수 | `support::count_u32`(저장소의 `i64` 건수 → 응답의 `u32`) |
| 수정 내역 | `support::Changes`(감사 로그 `{"changed": {필드: {"from", "to"}}}`) |
| 입력 오류의 필드 | `DomainError::with_field("요청 필드")` |
| 여러 필드의 입력 오류 | `ServiceError::InvalidFields(Vec<FieldErrorDto>)` — 찾은 오류를 전부 `fields`에 싣는다(결재 양식의 값·필드 정의) |
| 연도 쿼리 | `calendar::year_range(year)`(1900~9999년) |
| 저장소 | `codec::placeholders(n)`(`IN (?, …)`), `codec::narrow::<u8>(열, 값)`(좁은 정수 읽기), 유일 제약 위반 판정은 저장소 함수(`members::is_duplicate_login_id` 등)로 |

### 입력
- 문자열 입력은 `pango_core::text`의 타입(`Title::parse` 등)으로 검증한 뒤 저장합니다.
- 다른 구성원·부서 등의 ID를 받으면 **존재와 상태를 확인**합니다. 볼 수 없는 대상은 404입니다.
- 목록은 `PageQuery`의 `limit()`(최대 200)을 따릅니다.

### 테스트
`crates/server/src/testing.rs`의 `TestApp`을 씁니다(`add_member`, `add_department`, `advance`, `set_time`). 테스트 파일마다 도우미를 다시 쓰지 말고 여기 있는 것을 씁니다.

- 준비·변경: `alter_member`, `set_status`, `set_department_head`, `auth_context`, `org_world()`(최고 관리자·관리자·조직장·구성원)
- 요청: `try_login`(상태 코드만), `document`(결재 문서 상세), `approval_forms`·`approval_form`(기안 화면의 양식 목록·양식 하나), `attendance_day`, `attendance_events`
- DB 확인: `audit_entries(action)`, `audit_actions(prefix)`, `notifications_of`, `notification_count`, `grant_keys`, `count(sql)`, `execute(sql)`(장애 주입용 트리거 등)
- 값: `ts`, `kst`, `millis`(KST 현지 시각 → 밀리초), `id_of`(응답의 `id`)

기능마다 다음을 **이름 있는 테스트**로 확인합니다: 해피 · 네거티브 · 경계값 · 엣지/코너 · 동시성 · 멱등성 · 순서 역전 · 부분 실패(롤백) · 기형/악의적 입력 · 권한(본인/동료/조직장/관리자/타 부서).

- 거부된 요청이 **아무것도 바꾸지 않았는지**까지 확인합니다.
- 응답에 다른 사람의 정보나 비밀 값이 섞이지 않았는지 확인합니다.
- 동시성 테스트는 `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`로 실제 병렬 요청을 보냅니다.

## 웹 화면을 추가하는 순서

웹 UI는 `crates/web`(Dioxus 0.7, WASM)입니다. 기준 구현은 `pages/profile.rs`, `pages/people.rs`, `pages/auth.rs`입니다.

```bash
cargo xtask web --dev        # 번들 빌드 → crates/server/web-dist
cargo test -p pango-web      # 순수 함수 테스트(네이티브에서 실행)
cargo clippy -p pango-web --target wasm32-unknown-unknown -- -D warnings
```

| 무엇 | 어디 |
| --- | --- |
| 경로 | `route.rs`의 `Route` (이미 전체 경로가 정의돼 있음) |
| 메뉴 | `layout.rs`의 `main_nav` / `team_nav` / `admin_nav` |
| 화면 | `pages/<이름>.rs`, 관리자 화면은 `pages/admin/<이름>.rs` |
| 공용 컴포넌트 | `ui/mod.rs` (`Button`, `Card`, `Badge`, `TextField`, `SelectField`, `TextArea`, `Modal`, `Tabs`, `Stat`, `StatLink`, `EmptyState`, `ErrorState`, `PageHeader`, `MonthSwitcher`, `MonthCalendar`, `MemberPicker`, `resource_view`, `loaded_or_empty`, `use_more_pages`, `use_initial_tab`), 결재선은 `ui/approval_line.rs` (`ApprovalLineEditor`, `use_line_draft`) |
| 로그인·회사 설정 | `session.rs` (`use_session`: 로그인 상태), `company.rs` (`use_company`: 근무 요일·근무일 전환 시각) |
| 문구 | `text.rs` (오류 코드·상태 값 → 한국어). 화면 파일에 상태 값 문구를 흩어 놓지 않는다 |
| 표시 변환 | `format.rs` (시각·날짜·시간 길이), `calendar.rs` (달력 계산) |
| 스타일 | `assets/style.css`. 색·간격은 토큰(`var(--…)`)만 사용 |

### 규칙
- **화면은 조립만 한다.** 계산·분류·검증이 필요하면 순수 함수로 빼서(`format.rs`, `text.rs`, 또는 화면 파일의 `mod logic`) 테스트를 붙인다.
- 서버 호출은 `api::get/post/post_once/patch/put/delete`만 쓴다. 요청/응답 타입은 `pango-api`의 것을 그대로 쓴다.
- 조회는 `use_resource` + `resource_view`로 세 상태(불러오는 중·실패·성공)를 처리한다. 빈 목록은 `EmptyState`.
- 변경 요청은 진행 중 `busy`로 버튼을 막고, 실패는 `report_error`, 성공은 `report_success` 후 목록을 `restart()`한다.
- 401(세션 만료)·403(권한 변경)은 `api`가 한곳에서 로그인 상태에 반영한다. 화면은 오류 문구(`error.message()`)만 보여 준다.
- 다른 화면의 특정 탭으로 보낼 때는 경로를 늘리지 않고 `StatLink { tab }` / `use_initial_tab`을 쓴다.
- 생성 폼은 폼을 열 때 `api::new_idempotency_key()`를 하나 만들어 `post_once`에 쓴다(두 번 눌러도 한 번만 생성).
- 수정 폼은 읽어 온 `version`을 그대로 보낸다. `version_conflict`는 문구로 안내하고 다시 불러온다.
- 권한에 따라 버튼을 숨기되, **숨김은 편의일 뿐**이다. 서버 오류(403/404)도 정상 흐름으로 처리한다.
- 삭제·반려·취소 같은 되돌리기 어려운 동작은 확인 단계를 둔다(`Modal`).
- 사용자 입력에 `maxlength`를 주고, 서버 검증 오류는 해당 필드 근처나 폼 상단(`FormError`)에 보여 준다.
- 세로로 스크롤되는 영역을 만들면 `style.css` 상단의 "스크롤 영역 공통 처리" 선택자 목록에 추가한다(`scrollbar-gutter: stable` — 스크롤바가 생겨도 내용이 밀리지 않게).
- 모바일(폭 959px 이하)에서 가로 스크롤이 생기지 않아야 한다. 표는 `.table-wrap`으로 감싸거나 목록(`.list`)으로 바꾼다.
- 훅(`use_*`)은 컴포넌트 본문 최상단에서만 부른다. 이벤트 핸들러 안에서는 `report_error` 같은 비-훅 함수를 쓴다.
