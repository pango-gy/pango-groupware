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
- 생성 요청은 `IdempotentJson` 추출기 + `state.write_idempotent(…)`를 씁니다.
- 수정 요청은 `version`을 받고, 저장소의 `update`가 `false`를 돌려주면 `support::require_updated`로 409를 냅니다.
- 계정·권한·기록을 바꾸면 같은 트랜잭션에서 `support::audit`을 호출합니다. 비밀번호·토큰은 `detail`에 넣지 않습니다.
- 알림은 `support::notify`로 만들고, 같은 사건이 두 번 알림을 만들지 않도록 `dedupe_key`를 줍니다.
- 현재 시각은 `state.now()`만 씁니다.

### 입력
- 문자열 입력은 `pango_core::text`의 타입(`Title::parse` 등)으로 검증한 뒤 저장합니다.
- 다른 구성원·부서 등의 ID를 받으면 **존재와 상태를 확인**합니다. 볼 수 없는 대상은 404입니다.
- 목록은 `PageQuery`의 `limit()`(최대 200)을 따릅니다.

### 테스트
`crates/server/src/testing.rs`의 `TestApp`을 씁니다(`add_member`, `add_department`, `advance`, `set_time`).

기능마다 다음을 **이름 있는 테스트**로 확인합니다: 해피 · 네거티브 · 경계값 · 엣지/코너 · 동시성 · 멱등성 · 순서 역전 · 부분 실패(롤백) · 기형/악의적 입력 · 권한(본인/동료/조직장/관리자/타 부서).

- 거부된 요청이 **아무것도 바꾸지 않았는지**까지 확인합니다.
- 응답에 다른 사람의 정보나 비밀 값이 섞이지 않았는지 확인합니다.
- 동시성 테스트는 `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`로 실제 병렬 요청을 보냅니다.
