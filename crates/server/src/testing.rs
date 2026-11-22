//! 통합 테스트 도우미.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::Router;
use chrono::Duration;
use http::{header, Method, Request, StatusCode};
use pango_core::authz::Role;
use pango_core::id::{DepartmentId, MemberId};
use pango_core::member::MemberStatus;
use pango_core::password::PlainPassword;
use pango_core::time::{ManualClock, Timestamp};
use pango_db::audit::{self, AuditEntry, AuditFilter};
use pango_db::leave;
use pango_db::members::{self, Member};
use pango_db::notifications::{self, Notification};
use pango_db::org::{self, Department};
use pango_db::testing::TestDb;
use serde_json::Value;
use tower::ServiceExt;

use crate::config::Config;
use crate::error::{ServiceError, ServiceResult};
use crate::security::SessionToken;
use crate::service::outbound::{Delivery, OutboundSender, SendError, SendFuture, Sent, Target};
use crate::service::setup::{self, InitInput};
use crate::service::{auth, AuthContext, RequestMeta};
use crate::state::AppState;

pub const OWNER_LOGIN: &str = "owner";
pub const OWNER_PASSWORD: &str = "correct-horse-123";
/// 기본 시작 시각: 2026-03-02(월) 09:00 KST.
pub const START: &str = "2026-03-02T00:00:00Z";
/// [`TestApp::add_member`]로 만든 구성원의 비밀번호.
pub const MEMBER_PASSWORD: &str = "member-pass-123";

/// 테스트에서 만든 구성원과 그 로그인 토큰.
#[derive(Debug, Clone)]
pub struct TestMember {
    pub id: MemberId,
    pub login_id: String,
    pub token: String,
}

pub fn ts(rfc3339: &str) -> Timestamp {
    Timestamp::from_utc(
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .expect("rfc3339")
            .with_timezone(&chrono::Utc),
    )
}

/// 회사 시간대(KST)의 현지 시각 `2026-03-02T09:00:00`을 RFC 3339 표기로.
pub fn kst(local: &str) -> String {
    format!("{local}+09:00")
}

/// KST 현지 시각의 밀리초 타임스탬프(요청 본문의 시각 값).
pub fn millis(local: &str) -> i64 {
    ts(&kst(local)).as_millis()
}

/// 응답 본문의 `id`.
pub fn id_of(body: &Value) -> String {
    body["id"].as_str().expect("id").to_owned()
}

/// 테스트용 서버. 실제 SQLite 파일과 수동 시계를 쓴다.
pub struct TestApp {
    pub state: AppState,
    pub clock: Arc<ManualClock>,
    pub owner_id: MemberId,
    router: Router,
    _db: TestDb,
}

/// 응답: 상태 코드, 헤더, JSON 본문(비어 있으면 `Null`).
#[derive(Debug)]
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: http::HeaderMap,
    pub body: Value,
}

impl TestResponse {
    pub fn error_code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }

    /// 상태 코드를 확인하고 본문을 돌려준다.
    #[track_caller]
    pub fn expect(self, status: StatusCode) -> Value {
        assert_eq!(
            self.status, status,
            "unexpected status, body: {}",
            self.body
        );
        self.body
    }

    /// 오류 상태와 코드를 함께 확인한다.
    #[track_caller]
    pub fn expect_error(self, status: StatusCode, code: &str) {
        assert_eq!(
            (self.status, self.error_code()),
            (status, code),
            "body: {}",
            self.body
        );
    }
}

/// 요청 빌더.
pub struct TestRequest<'a> {
    app: &'a TestApp,
    builder: http::request::Builder,
    body: Option<Vec<u8>>,
}

impl TestRequest<'_> {
    pub fn bearer(mut self, token: &str) -> Self {
        self.builder = self
            .builder
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.builder = self.builder.header(name, value);
        self
    }

    pub fn json(mut self, body: &Value) -> Self {
        self.builder = self
            .builder
            .header(header::CONTENT_TYPE, "application/json");
        self.body = Some(body.to_string().into_bytes());
        self
    }

    pub fn raw_body(mut self, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        self.builder = self.builder.header(header::CONTENT_TYPE, content_type);
        self.body = Some(body.into());
        self
    }

    pub async fn send(self) -> TestResponse {
        let request = self
            .builder
            .body(Body::from(self.body.unwrap_or_default()))
            .expect("build request");
        let response = self
            .app
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("infallible");
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, 16 * 1024 * 1024)
            .await
            .expect("read body");
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        TestResponse {
            status: parts.status,
            headers: parts.headers,
            body,
        }
    }
}

impl TestApp {
    /// 초기화가 끝난(회사 설정 + 최고 관리자) 서버.
    pub async fn new() -> Self {
        Self::with_config(Config::for_tests()).await
    }

    pub async fn with_config(config: Config) -> Self {
        Self::initialized(config, None).await
    }

    /// 외부 알림 발송기를 `sender`(보통 [`FakeSender`])로 바꾼 서버.
    pub async fn with_outbound_sender(sender: Arc<dyn OutboundSender>) -> Self {
        Self::initialized(Config::for_tests(), Some(sender)).await
    }

    /// 설정과 외부 알림 발송기를 함께 정한 서버.
    pub async fn with_config_and_sender(config: Config, sender: Arc<dyn OutboundSender>) -> Self {
        Self::initialized(config, Some(sender)).await
    }

    async fn initialized(config: Config, sender: Option<Arc<dyn OutboundSender>>) -> Self {
        let app = Self::build(config, sender).await;
        let output = setup::initialize(
            &app.state,
            InitInput {
                company_name: "팡고".into(),
                timezone: "Asia/Seoul".into(),
                owner_login_id: OWNER_LOGIN.into(),
                owner_name: "대표".into(),
                owner_password: Some(OWNER_PASSWORD.into()),
            },
        )
        .await
        .expect("initialize");
        Self {
            owner_id: output.owner_id,
            ..app
        }
    }

    /// 초기화 전 상태의 서버.
    pub async fn uninitialized(config: Config) -> Self {
        Self::build(config, None).await
    }

    async fn build(config: Config, sender: Option<Arc<dyn OutboundSender>>) -> Self {
        let db = TestDb::new().await;
        let clock = Arc::new(ManualClock::new(ts(START)));
        let mut state = AppState::new(db.db.clone(), clock.clone(), config).expect("state");
        if let Some(sender) = sender {
            state = state.with_outbound_sender(sender);
        }
        let router = crate::http::router(state.clone());
        Self {
            state,
            clock,
            owner_id: MemberId::new(),
            router,
            _db: db,
        }
    }

    pub fn advance(&self, duration: Duration) {
        self.clock.advance(duration);
    }

    pub fn set_time(&self, rfc3339: &str) {
        self.clock.set(ts(rfc3339));
    }

    pub fn request(&self, method: Method, path: &str) -> TestRequest<'_> {
        let uri = format!("{}{path}", pango_api::API_PREFIX);
        TestRequest {
            app: self,
            builder: Request::builder().method(method).uri(uri),
            body: None,
        }
    }

    /// API 접두사 없이 루트 경로로 보낸다(`/mcp`, `/oauth/…`, `/.well-known/…`).
    pub fn root(&self, method: Method, path: &str) -> TestRequest<'_> {
        TestRequest {
            app: self,
            builder: Request::builder().method(method).uri(path),
            body: None,
        }
    }

    pub fn get(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::GET, path)
    }

    pub fn post(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::POST, path)
    }

    pub fn patch(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::PATCH, path)
    }

    pub fn put(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::PUT, path)
    }

    pub fn delete(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::DELETE, path)
    }

    /// 로그인해 Bearer 토큰을 받는다.
    pub async fn login(&self, login_id: &str, password: &str) -> String {
        let body = serde_json::json!({
            "login_id": login_id, "password": password, "token_in_body": true
        });
        let response = self
            .post("/auth/login")
            .json(&body)
            .send()
            .await
            .expect(StatusCode::OK);
        response["token"].as_str().expect("token").to_owned()
    }

    pub async fn login_owner(&self) -> String {
        self.login(OWNER_LOGIN, OWNER_PASSWORD).await
    }

    /// 부서를 DB에 직접 만든다(관리 API를 거치지 않는 테스트 준비용).
    pub async fn add_department(
        &self,
        name: &str,
        parent_id: Option<DepartmentId>,
        head_member_id: Option<MemberId>,
    ) -> DepartmentId {
        let department = Department {
            id: DepartmentId::new(),
            parent_id,
            name: name.to_owned(),
            head_member_id,
            sort_order: 0,
            archived_at: None,
            version: 1,
        };
        let mut tx = self.state.db.begin_write().await.expect("begin");
        org::insert_department(&mut tx, &department, self.state.now())
            .await
            .expect("department");
        tx.commit().await.expect("commit");
        department.id
    }

    /// 부서의 조직장을 지정한다.
    pub async fn set_department_head(&self, department_id: DepartmentId, head: Option<MemberId>) {
        let mut tx = self.state.db.begin_write().await.expect("begin");
        let mut department = org::find_department(&mut tx, department_id)
            .await
            .expect("find")
            .expect("department");
        department.head_member_id = head;
        assert!(
            org::update_department(&mut tx, &department, self.state.now())
                .await
                .expect("update")
        );
        tx.commit().await.expect("commit");
    }

    /// 구성원을 DB에 직접 만들고 로그인까지 해 둔다. 입사일은 2024-03-02.
    ///
    /// 계정도 입사일 0시(UTC)에 만든 것으로 저장한다. 그래야 시계를 앞뒤로 옮겨 지난 날짜를
    /// 판정하는 테스트가 계정 생성 전이라는 이유로 근태 추적 기간 밖에 떨어지지 않는다.
    /// 최근에 만든 계정이 필요하면 [`Self::add_member_created_at`]을 쓴다.
    pub async fn add_member(
        &self,
        login_id: &str,
        role: Role,
        department_id: Option<DepartmentId>,
    ) -> TestMember {
        let created_at = Timestamp::from_utc(
            "2024-03-02T00:00:00Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .expect("time"),
        );
        self.add_member_created_at(login_id, role, department_id, created_at)
            .await
    }

    /// [`Self::add_member`]와 같되 계정 생성 시각을 `created_at`으로 저장한다.
    pub async fn add_member_created_at(
        &self,
        login_id: &str,
        role: Role,
        department_id: Option<DepartmentId>,
        created_at: Timestamp,
    ) -> TestMember {
        let member = Member {
            id: MemberId::new(),
            login_id: login_id.to_owned(),
            name: format!("{login_id} 님"),
            email: None,
            phone: None,
            department_id,
            position_id: None,
            role,
            status: MemberStatus::Active,
            hired_on: "2024-03-02".parse().expect("date"),
            retired_on: None,
            must_change_password: false,
            created_at,
            version: 1,
        };
        let password = PlainPassword::for_verification(MEMBER_PASSWORD).expect("password");
        let hash = self.state.passwords.hash(&password).await.expect("hash");
        let mut tx = self.state.db.begin_write().await.expect("begin");
        members::insert(&mut tx, &member, &hash, created_at)
            .await
            .expect("member");
        tx.commit().await.expect("commit");
        let token = self.login(login_id, MEMBER_PASSWORD).await;
        TestMember {
            id: member.id,
            login_id: login_id.to_owned(),
            token,
        }
    }

    /// 토큰을 인증해 요청 맥락을 만든다(HTTP 추출기가 하는 일과 같다).
    ///
    /// 인증을 통과한 **뒤** 권한이나 소속이 바뀌는 경우를 재현할 때 쓴다: 이 맥락을 받아 두고
    /// [`Self::alter_member`]로 DB를 바꾼 다음 서비스 함수를 직접 부르면, 서비스는 낡은 맥락을
    /// 들고 쓰기 트랜잭션에 들어간다. HTTP로는 요청마다 다시 인증하므로 이 틈을 만들 수 없다.
    pub async fn auth_context(&self, token: &str) -> AuthContext {
        let token = SessionToken::parse(token).expect("session token");
        auth::authenticate(&self.state, &token, RequestMeta::default())
            .await
            .expect("authenticate")
    }

    /// 구성원을 DB에서 직접 고친다(역할 해제, 정지, 부서 이동 등).
    pub async fn alter_member(&self, id: MemberId, change: impl FnOnce(&mut Member)) {
        let mut tx = self.state.db.begin_write().await.expect("begin");
        let mut member = members::find(&mut tx, id)
            .await
            .expect("find")
            .expect("member");
        change(&mut member);
        assert!(members::update(&mut tx, &member, self.state.now())
            .await
            .expect("update"));
        tx.commit().await.expect("commit");
    }

    /// 구성원의 상태(정지·퇴사 등)를 DB에서 직접 바꾼다.
    pub async fn set_status(&self, id: MemberId, status: MemberStatus) {
        self.alter_member(id, |member| member.status = status).await;
    }

    /// 로그인을 시도하고 상태 코드만 돌려준다(실패를 확인할 때).
    pub async fn try_login(&self, login_id: &str, password: &str) -> StatusCode {
        let body = serde_json::json!({
            "login_id": login_id, "password": password, "token_in_body": true
        });
        self.post("/auth/login").json(&body).send().await.status
    }

    /// 결재 문서 상세(`GET /approval/documents/{id}`, 200이어야 한다).
    pub async fn document(&self, token: &str, id: &str) -> Value {
        self.get(&format!("/approval/documents/{id}"))
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK)
    }

    /// 기안 화면의 양식 목록(`GET /approval/forms`)을 분류 묶음을 풀어 한 줄로 돌려준다.
    pub async fn approval_forms(&self, token: &str) -> Vec<Value> {
        let groups = self
            .get("/approval/forms")
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK);
        groups
            .as_array()
            .expect("groups")
            .iter()
            .flat_map(|group| group["forms"].as_array().expect("forms").clone())
            .collect()
    }

    /// 사용 중인 양식 하나(`code`로 찾는다). 없으면 실패한다.
    pub async fn approval_form(&self, token: &str, code: &str) -> Value {
        self.approval_forms(token)
            .await
            .into_iter()
            .find(|form| form["code"] == code)
            .unwrap_or_else(|| panic!("no active form {code}"))
    }

    /// 근태 기록 하나의 변경 이력(`GET /attendance/records/{id}/events`, 200이어야 한다).
    pub async fn attendance_events(&self, token: &str, record_id: &str) -> Vec<Value> {
        self.get(&format!("/attendance/records/{record_id}/events"))
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK)
            .as_array()
            .expect("events")
            .clone()
    }

    /// 월별 근태 조회에서 `date`(`YYYY-MM-DD`) 하루의 항목.
    pub async fn attendance_day(&self, token: &str, date: &str) -> Value {
        let records = self
            .get(&format!("/attendance/records?month={}", &date[..7]))
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK);
        records["days"]
            .as_array()
            .expect("days")
            .iter()
            .find(|d| d["date"] == date)
            .unwrap_or_else(|| panic!("no entry for {date}"))
            .clone()
    }

    /// `action`인 감사 로그(최신순, 최대 200줄).
    pub async fn audit_entries(&self, action: &str) -> Vec<AuditEntry> {
        let filter = AuditFilter {
            action: Some(action.to_owned()),
            ..AuditFilter::default()
        };
        let mut conn = self.state.read().await.expect("read");
        audit::list(&mut conn, &filter, 200).await.expect("audit").0
    }

    /// `prefix`로 시작하는 감사 로그 동작 이름(최신순, 최대 200줄). 빈 문자열이면 전부.
    pub async fn audit_actions(&self, prefix: &str) -> Vec<String> {
        let mut conn = self.state.read().await.expect("read");
        let (entries, _) = audit::list(&mut conn, &AuditFilter::default(), 200)
            .await
            .expect("audit");
        entries
            .into_iter()
            .map(|e| e.action)
            .filter(|a| a.starts_with(prefix))
            .collect()
    }

    /// 구성원이 받은 알림(최신순, 최대 200개).
    pub async fn notifications_of(&self, member: MemberId) -> Vec<Notification> {
        let mut conn = self.state.read().await.expect("read");
        notifications::list(&mut conn, member, false, None, 200)
            .await
            .expect("notifications")
            .0
    }

    /// 구성원이 받은 `kind` 알림의 수.
    pub async fn notification_count(&self, member: MemberId, kind: &str) -> usize {
        let items = self.notifications_of(member).await;
        items.iter().filter(|n| n.kind == kind).count()
    }

    /// 구성원 원장의 자동 부여 키(기록순).
    pub async fn grant_keys(&self, member: MemberId) -> Vec<String> {
        let mut conn = self.state.read().await.expect("read");
        leave::list_ledger(&mut conn, member)
            .await
            .expect("ledger")
            .into_iter()
            .filter_map(|record| record.grant_key)
            .collect()
    }

    /// DB를 직접 센다(API를 거치지 않고 실제로 남은 것을 확인한다). `sql`은 정수 하나를 돌려준다.
    pub async fn count(&self, sql: &str) -> i64 {
        let mut conn = self.state.read().await.expect("read");
        sqlx::query_scalar(sql)
            .fetch_one(&mut *conn)
            .await
            .expect("count")
    }

    /// 테스트 DB에 직접 SQL을 실행한다(장애 주입용 트리거 등).
    pub async fn execute(&self, sql: &str) {
        let mut tx = self.state.db.begin_write().await.expect("begin");
        sqlx::query(sql).execute(&mut *tx).await.expect("execute");
        tx.commit().await.expect("commit");
    }
}

/// [`FakeSender`]가 새로 연 것처럼 돌려주는 Buzz DM 채널 ID.
pub const FAKE_DM: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

/// 외부 알림 발송을 기록하는 가짜 발송기. 정해 둔 응답을 차례로 돌려주고, 다 쓰면 200으로 답한다.
#[derive(Default)]
pub struct FakeSender {
    calls: Mutex<Vec<Delivery>>,
    responses: Mutex<VecDeque<Result<u16, SendError>>>,
    /// 응답 전에 기다릴 시간(동시 발송 테스트용).
    delay: Option<std::time::Duration>,
}

impl FakeSender {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn with_delay(delay: std::time::Duration) -> Arc<Self> {
        Arc::new(Self {
            delay: Some(delay),
            ..Self::default()
        })
    }

    /// 다음 발송의 응답을 정해 둔다.
    pub fn respond(&self, response: Result<u16, SendError>) {
        self.responses.lock().expect("lock").push_back(response);
    }

    pub fn calls(&self) -> Vec<Delivery> {
        self.calls.lock().expect("lock").clone()
    }
}

impl OutboundSender for FakeSender {
    fn send<'a>(&'a self, delivery: &'a Delivery) -> SendFuture<'a> {
        Box::pin(async move {
            self.calls.lock().expect("lock").push(delivery.clone());
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            let response = self
                .responses
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or(Ok(200));
            // Buzz는 연 DM의 채널 ID를 돌려준다(없으면 새로 연 것처럼 고정값).
            let dm_channel = match &delivery.target {
                Target::Buzz(buzz) => Some(
                    buzz.dm_channel
                        .clone()
                        .unwrap_or_else(|| FAKE_DM.to_owned()),
                ),
                Target::Webhook(_) => None,
            };
            response.map(|status| Sent { status, dm_channel })
        })
    }
}

impl TestApp {
    /// 웹훅 연동을 DB에서 바로 켠다(주소와 서명 키 `test-secret`).
    pub async fn enable_webhook(&self, url: &str) {
        self.execute(&format!(
            "UPDATE outbound_settings SET webhook_enabled = 1, webhook_url = '{url}',
                 webhook_secret = 'test-secret', version = version + 1 WHERE id = 1"
        ))
        .await;
    }

    /// Buzz 채널을 DB에서 바로 켠다(실행 설정은 따로 줘야 한다).
    pub async fn enable_buzz(&self) {
        self.execute("UPDATE buzz_settings SET enabled = 1, version = version + 1 WHERE id = 1")
            .await;
    }

    /// 구성원의 Buzz 계정 연결을 확인을 마친 상태로 바로 넣는다.
    pub async fn link_buzz(&self, member: MemberId, pubkey_hex: &str) {
        let now = self.state.now().as_millis();
        self.execute(&format!(
            "INSERT INTO buzz_accounts (member_id, pubkey, status, verified_at, created_at, updated_at)
             VALUES ('{member}', '{pubkey_hex}', 'verified', {now}, {now}, {now})"
        ))
        .await;
    }

    /// 아웃박스의 `(채널, 상태, 마지막 사유)`를 만든 순서대로.
    pub async fn outbox_by_channel(&self) -> Vec<(String, String, Option<String>)> {
        let mut conn = self.state.read().await.expect("read");
        sqlx::query_as(
            "SELECT channel, status, last_error FROM outbound_messages ORDER BY created_at, channel",
        )
        .fetch_all(&mut *conn)
        .await
        .expect("outbox")
    }

    /// 아웃박스의 `(상태, 시도 수, 마지막 사유)`를 만든 순서대로.
    pub async fn outbox(&self) -> Vec<(String, i64, Option<String>)> {
        let mut conn = self.state.read().await.expect("read");
        sqlx::query_as(
            "SELECT status, attempts, last_error FROM outbound_messages ORDER BY created_at, id",
        )
        .fetch_all(&mut *conn)
        .await
        .expect("outbox")
    }
}

/// 최고 관리자, 관리자, 조직장(개발팀 head), 개발팀 구성원이 있는 회사.
pub struct OrgWorld {
    pub app: TestApp,
    pub owner: String,
    pub admin: TestMember,
    pub manager: TestMember,
    pub member: TestMember,
    pub dept: DepartmentId,
}

pub async fn org_world() -> OrgWorld {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let dept = app.add_department("개발팀", None, None).await;
    let admin = app.add_member("admin", Role::Admin, None).await;
    let manager = app.add_member("manager", Role::Member, Some(dept)).await;
    let member = app.add_member("member", Role::Member, Some(dept)).await;
    app.set_department_head(dept, Some(manager.id)).await;
    OrgWorld {
        app,
        owner,
        admin,
        manager,
        member,
        dept,
    }
}

/// 서비스 호출이 권한 문제로 거부됐는지: `"forbidden"`, `"unauthenticated"`, 성공이면 `"ok"`,
/// 다른 오류면 그 오류의 표기.
pub fn denial<T>(result: &ServiceResult<T>) -> String {
    match result {
        Ok(_) => "ok".to_owned(),
        Err(ServiceError::Domain(pango_core::DomainError::Forbidden)) => "forbidden".to_owned(),
        Err(ServiceError::Domain(pango_core::DomainError::Unauthenticated)) => {
            "unauthenticated".to_owned()
        }
        Err(other) => other.to_string(),
    }
}
