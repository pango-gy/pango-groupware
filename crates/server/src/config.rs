//! 실행 설정. 회사별 업무 설정(근무 시간 등)은 DB에 있고, 여기는 서버 동작만 다룬다.

use chrono::Duration;

#[derive(Debug, Clone)]
pub struct PasswordParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    /// 동시에 실행할 해시 연산 수. 해시는 메모리를 많이 쓰므로 제한한다.
    pub max_concurrent: usize,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// 마지막 사용 후 이 시간이 지나면 세션이 만료된다.
    pub session_idle: Duration,
    /// 로그인 후 이 시간이 지나면 사용 중이어도 만료된다.
    pub session_absolute: Duration,
    /// IP당 창 안에서 허용하는 로그인 시도 수.
    pub login_ip_limit: i64,
    pub login_ip_window: Duration,
    /// 아이디당 창 안에서 허용하는 로그인 실패 수. 넘으면 창이 끝날 때까지 잠긴다.
    pub login_account_limit: i64,
    pub login_account_window: Duration,
    pub password: PasswordParams,
    /// 세션 쿠키에 `Secure`를 붙일지. HTTPS가 아닌 로컬 개발에서만 끈다.
    pub cookie_secure: bool,
    /// 리버스 프록시의 `X-Forwarded-For`를 믿을지.
    pub trust_proxy: bool,
    /// 서비스의 공개 주소(예: `https://erp.example.com`). 지정하면 `Origin` 헤더를 이 값과
    /// 정확히 비교한다. 없으면 요청의 호스트와 비교한다(프록시가 `Host`를 바꾸면 맞지 않는다).
    pub public_origin: Option<String>,
    /// 요청 본문 최대 크기(바이트).
    pub max_body_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            session_idle: Duration::days(14),
            session_absolute: Duration::days(90),
            login_ip_limit: 60,
            login_ip_window: Duration::minutes(1),
            login_account_limit: 5,
            login_account_window: Duration::minutes(15),
            // OWASP 권장 최소값(19 MiB, 2회)보다 여유 있게.
            password: PasswordParams {
                memory_kib: 32 * 1024,
                iterations: 3,
                parallelism: 1,
                max_concurrent: 4,
            },
            cookie_secure: true,
            trust_proxy: false,
            public_origin: None,
            max_body_bytes: 256 * 1024,
        }
    }
}

impl Config {
    /// 테스트용: 해시 비용을 최소로 낮춘다. 운영에 쓰면 안 된다.
    pub fn for_tests() -> Self {
        Self {
            password: PasswordParams {
                memory_kib: 8,
                iterations: 1,
                parallelism: 1,
                max_concurrent: 16,
            },
            cookie_secure: false,
            ..Self::default()
        }
    }
}
