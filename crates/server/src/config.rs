//! 실행 설정. 회사별 업무 설정(근무 시간 등)은 DB에 있고, 여기는 서버 동작만 다룬다.

use chrono::Duration;

use crate::nostr::BotKey;

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
    /// 첨부파일 하나의 최대 크기(바이트). 첨부 올리기 경로에만 [`Self::max_body_bytes`] 대신 쓴다.
    pub max_attachment_bytes: usize,
    /// 외부 알림 웹훅을 루프백·사설망·CGNAT 주소로도 보낼지. 받는 서버를 같은 사설망에 둘 때만
    /// 켠다(서버 실행 옵션으로만, 관리자 화면에서는 바꿀 수 없다). 메타데이터·link-local 등은
    /// 켜도 막는다([`pango_core::outbound::ip_allowed`]).
    pub outbound_allow_private: bool,
    /// 사내 메신저 Buzz 연결(릴레이 주소 + 봇 비밀키). 없으면 Buzz 채널을 쓸 수 없다.
    pub buzz: Option<BuzzConfig>,
}

/// Buzz 연결 설정. 봇 비밀키는 실행 설정(환경 변수·파일)으로만 받고 DB에 두지 않는다.
#[derive(Debug, Clone)]
pub struct BuzzConfig {
    /// 릴레이의 출처(`https://buzz.example.com`). NIP-98의 `u` 태그는 `{relay_url}/events`다.
    pub relay_url: String,
    pub bot_key: BotKey,
}

impl BuzzConfig {
    /// 실행 설정 값으로 만든다. 아무것도 없으면 `None`(Buzz를 쓰지 않음). 키는 `key`(값) 또는
    /// `key_file`(Secrets Manager 등이 마운트한 파일의 경로) 중 하나만 받는다. 오류 문구에
    /// 키 값은 넣지 않는다.
    pub fn from_settings(
        relay_url: Option<&str>,
        key: Option<&str>,
        key_file: Option<&std::path::Path>,
    ) -> Result<Option<Self>, String> {
        let file_key = match key_file {
            None => None,
            Some(path) => Some(std::fs::read_to_string(path).map_err(|error| {
                format!(
                    "Buzz 봇 키 파일을 읽지 못했습니다({}): {error}",
                    path.display()
                )
            })?),
        };
        let key = match (key.filter(|k| !k.trim().is_empty()), file_key) {
            (Some(_), Some(_)) => {
                return Err(
                    "PANGO_BUZZ_BOT_KEY와 PANGO_BUZZ_BOT_KEY_FILE 중 하나만 지정하세요.".into(),
                )
            }
            (Some(key), None) => Some(key.to_owned()),
            (None, file) => file,
        };
        let relay_url = relay_url.filter(|url| !url.trim().is_empty());
        match (relay_url, key) {
            (None, None) => Ok(None),
            (Some(_), None) => Err("Buzz 릴레이 주소가 있으면 봇 키도 지정해야 합니다.".into()),
            (None, Some(_)) => Err("Buzz 봇 키가 있으면 릴레이 주소도 지정해야 합니다.".into()),
            (Some(url), Some(key)) => {
                let relay_url = pango_core::buzz::relay_origin(url)
                    .map_err(|_| "Buzz 릴레이 주소는 https 주소여야 합니다.".to_owned())?;
                let bot_key = BotKey::parse(&key).map_err(|error| error.to_string())?;
                Ok(Some(Self { relay_url, bot_key }))
            }
        }
    }
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
            max_attachment_bytes: 10 * 1024 * 1024,
            outbound_allow_private: false,
            buzz: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "b7e151628aed2a6abf7158809cf4f3c762e7160f38b4da56a784d9045190cfef";

    #[test]
    fn buzz_needs_both_url_and_key() {
        assert!(BuzzConfig::from_settings(None, None, None)
            .expect("none")
            .is_none());
        let config = BuzzConfig::from_settings(Some("wss://buzz.example.com"), Some(KEY), None)
            .expect("ok")
            .expect("some");
        assert_eq!(config.relay_url, "https://buzz.example.com");
        assert!(!format!("{config:?}").contains(KEY));
        assert!(BuzzConfig::from_settings(Some("https://buzz.example.com"), None, None).is_err());
        assert!(BuzzConfig::from_settings(None, Some(KEY), None).is_err());
        assert!(
            BuzzConfig::from_settings(Some("http://buzz.example.com"), Some(KEY), None).is_err()
        );
        let bad =
            BuzzConfig::from_settings(Some("https://buzz.example.com"), Some("nope-secret"), None)
                .expect_err("bad key");
        assert!(!bad.contains("nope-secret"), "{bad}");
    }

    #[test]
    fn buzz_key_can_come_from_a_file() {
        let dir = tempfile_dir();
        let path = dir.join("bot.key");
        std::fs::write(&path, format!("{KEY}\n")).expect("write");
        let config = BuzzConfig::from_settings(Some("https://buzz.example.com"), None, Some(&path))
            .expect("ok")
            .expect("some");
        assert_eq!(
            config.bot_key.public_key(),
            BotKey::parse(KEY).expect("key").public_key()
        );
        assert!(
            BuzzConfig::from_settings(Some("https://buzz.example.com"), Some(KEY), Some(&path))
                .is_err(),
            "둘 다 주면 거부"
        );
        let missing = dir.join("missing.key");
        assert!(
            BuzzConfig::from_settings(Some("https://buzz.example.com"), None, Some(&missing))
                .is_err()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pango-config-{}", uuid_like()));
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    fn uuid_like() -> String {
        pango_core::id::MemberId::new().to_string()
    }
}
