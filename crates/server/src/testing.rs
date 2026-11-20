//! 통합 테스트 도우미.

use std::sync::Arc;

use axum::body::Body;
use axum::Router;
use chrono::Duration;
use http::{header, Method, Request, StatusCode};
use pango_core::authz::Role;
use pango_core::id::{DepartmentId, MemberId};
use pango_core::member::MemberStatus;
use pango_core::password::PlainPassword;
use pango_core::time::{ManualClock, Timestamp};
use pango_db::members::{self, Member};
use pango_db::org::{self, Department};
use pango_db::testing::TestDb;
use serde_json::Value;
use tower::ServiceExt;

use crate::config::Config;
use crate::service::setup::{self, InitInput};
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
        let app = Self::uninitialized(config).await;
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
        let db = TestDb::new().await;
        let clock = Arc::new(ManualClock::new(ts(START)));
        let state = AppState::new(db.db.clone(), clock.clone(), config).expect("state");
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
    pub async fn add_member(
        &self,
        login_id: &str,
        role: Role,
        department_id: Option<DepartmentId>,
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
            version: 1,
        };
        let password = PlainPassword::for_verification(MEMBER_PASSWORD).expect("password");
        let hash = self.state.passwords.hash(&password).await.expect("hash");
        let mut tx = self.state.db.begin_write().await.expect("begin");
        members::insert(&mut tx, &member, &hash, self.state.now())
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
}
