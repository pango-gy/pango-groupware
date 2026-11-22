//! 비밀번호 해시, 세션 토큰, 내려받기 링크 서명.

use std::sync::Arc;

use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use hmac::{Hmac, Mac};
use pango_core::id::SessionId;
use pango_core::password::PlainPassword;
use pango_core::time::Timestamp;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::config::PasswordParams;
use crate::error::{ServiceError, ServiceResult};

fn random_bytes<const N: usize>() -> ServiceResult<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| ServiceError::internal(e.to_string()))?;
    Ok(bytes)
}

/// 짧게 유효한 내려받기 링크의 서명.
///
/// 앱(Bearer 토큰)은 시스템 브라우저로 파일을 받게 하는데, 브라우저는 토큰을 실어 보낼 수 없다.
/// 그래서 로그인한 세션이 **특정 경로 하나**에 대해 몇 분짜리 서명을 받아 쿼리에 붙인다.
/// 서명은 세션에 묶이므로 로그아웃·기기 로그아웃하면 링크도 함께 쓸 수 없게 된다. 키는 실행할
/// 때마다 새로 만든다(다시 시작하면 발급한 링크는 모두 무효).
#[derive(Clone)]
pub struct DownloadSigner {
    key: Arc<[u8; 32]>,
}

impl std::fmt::Debug for DownloadSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DownloadSigner(***)")
    }
}

impl DownloadSigner {
    pub fn generate() -> ServiceResult<Self> {
        Ok(Self {
            key: Arc::new(random_bytes::<32>()?),
        })
    }

    fn mac(&self, session: SessionId, expires_ms: i64, target: &str) -> [u8; 32] {
        // 키 길이는 고정(32바이트)이라 실패하지 않는다.
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key[..])
            .unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
        mac.update(b"pango-download\0");
        mac.update(session.to_string().as_bytes());
        mac.update(b"\0");
        mac.update(expires_ms.to_string().as_bytes());
        mac.update(b"\0");
        mac.update(target.as_bytes());
        mac.finalize().into_bytes().into()
    }

    /// `세션ID.만료(ms).서명` 형태의 토큰. URL에 그대로 넣을 수 있는 글자만 쓴다.
    pub fn sign(&self, session: SessionId, expires: Timestamp, target: &str) -> String {
        let expires_ms = expires.as_millis();
        format!(
            "{session}.{expires_ms}.{}",
            hex(&self.mac(session, expires_ms, target))
        )
    }

    /// 토큰이 이 `target`에 대해 이 시각에 유효하면 세션 ID.
    pub fn verify(&self, token: &str, target: &str, now: Timestamp) -> Option<SessionId> {
        let mut parts = token.splitn(3, '.');
        let session = SessionId::parse(parts.next()?).ok()?;
        let expires_ms: i64 = parts.next()?.parse().ok()?;
        let given = parts.next()?;
        if now.as_millis() >= expires_ms {
            return None;
        }
        let expected = hex(&self.mac(session, expires_ms, target));
        bool::from(subtle::ConstantTimeEq::ct_eq(
            expected.as_bytes(),
            given.as_bytes(),
        ))
        .then_some(session)
    }
}

/// Argon2id 해시기. 해시 연산은 블로킹 스레드에서, 동시 실행 수를 제한해 돌린다.
#[derive(Clone)]
pub struct PasswordHasher {
    argon: Argon2<'static>,
    permits: Arc<Semaphore>,
    /// 없는 계정으로 로그인할 때도 같은 시간을 쓰기 위한 가짜 해시.
    dummy_hash: Arc<str>,
}

impl PasswordHasher {
    pub fn new(params: &PasswordParams) -> ServiceResult<Self> {
        let argon_params = Params::new(
            params.memory_kib,
            params.iterations,
            params.parallelism,
            None,
        )
        .map_err(|e| ServiceError::internal(e.to_string()))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);
        let dummy_hash = hash_blocking(&argon, "dummy-password-for-timing")?.into();
        Ok(Self {
            argon,
            permits: Arc::new(Semaphore::new(params.max_concurrent.max(1))),
            dummy_hash,
        })
    }

    pub async fn hash(&self, password: &PlainPassword) -> ServiceResult<String> {
        let argon = self.argon.clone();
        let password = password.clone();
        self.run(move || hash_blocking(&argon, password.expose()))
            .await?
    }

    /// 비밀번호가 맞는지. `stored`가 `None`(없는 계정)이면 가짜 해시로 같은 연산을 한 뒤
    /// `false`를 돌려준다.
    pub async fn verify(
        &self,
        password: &PlainPassword,
        stored: Option<&str>,
    ) -> ServiceResult<bool> {
        let argon = self.argon.clone();
        let password = password.clone();
        let exists = stored.is_some();
        let hash: Arc<str> = stored.map_or_else(|| self.dummy_hash.clone(), Arc::from);
        let matched = self
            .run(move || {
                PasswordHash::new(&hash)
                    .map(|parsed| {
                        argon
                            .verify_password(password.expose().as_bytes(), &parsed)
                            .is_ok()
                    })
                    .unwrap_or(false)
            })
            .await?;
        Ok(matched && exists)
    }

    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> ServiceResult<T> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|e| ServiceError::internal(e.to_string()))?;
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|e| ServiceError::internal(e.to_string()))
    }
}

fn hash_blocking(argon: &Argon2<'_>, password: &str) -> ServiceResult<String> {
    let salt = SaltString::encode_b64(&random_bytes::<16>()?)
        .map_err(|e| ServiceError::internal(e.to_string()))?;
    argon
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| ServiceError::internal(e.to_string()))
}

/// 세션 토큰 원문. 클라이언트에게만 주고, 서버에는 [`SessionToken::hash`]만 남긴다.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken(String);

impl SessionToken {
    const BYTES: usize = 32;

    pub fn generate() -> ServiceResult<Self> {
        Ok(Self(hex(&random_bytes::<{ Self::BYTES }>()?)))
    }

    /// 형식(소문자 16진수 64자)이 맞는 값만 받는다. 맞지 않으면 DB를 조회할 필요도 없다.
    pub fn parse(raw: &str) -> Option<Self> {
        let valid = raw.len() == Self::BYTES * 2
            && raw
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        valid.then(|| Self(raw.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_bytes()).into()
    }
}

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionToken(***)")
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 0x0f)]])
        .map(char::from)
        .collect()
}

/// 외부 알림 웹훅의 서명 키: 256비트 난수의 16진수(64자). 받는 쪽은 이 문자열의 바이트를
/// 그대로 HMAC 키로 쓴다.
pub fn new_webhook_secret() -> ServiceResult<String> {
    Ok(hex(&random_bytes::<32>()?))
}

/// 웹훅 서명 `sha256=<hex>`: `"{timestamp}.{body}"`의 HMAC-SHA256(키 = 서명 키 문자열).
/// 시각을 함께 서명하므로 받는 쪽은 오래된 요청의 재전송을 거를 수 있다.
pub fn webhook_signature(secret: &str, timestamp: i64, body: &[u8]) -> String {
    // HMAC은 어떤 길이의 키든 받는다(`new_from_slice`는 실패하지 않는다).
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC accepts keys of any length"));
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("sha256={}", hex(&mac.finalize().into_bytes()))
}

/// 요청 지문: 같은 멱등성 키로 다른 요청이 왔는지 구분하는 데 쓴다.
pub fn fingerprint(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        // 길이를 함께 넣어 경계가 다른 입력이 같은 지문이 되지 않게 한다.
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// 관리자가 전달할 임시 비밀번호. 헷갈리는 글자(0/O, 1/l/I)를 뺀 16자.
pub fn generate_temporary_password() -> ServiceResult<String> {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    const LEN: usize = 16;
    // 편향 없이 고르기 위해, 알파벳 길이의 배수 범위를 벗어난 바이트는 버린다.
    let limit = (256 / ALPHABET.len() * ALPHABET.len()) as u8;
    let mut out = String::with_capacity(LEN);
    while out.len() < LEN {
        for byte in random_bytes::<32>()? {
            if byte < limit && out.len() < LEN {
                out.push(char::from(ALPHABET[usize::from(byte) % ALPHABET.len()]));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_tokens_bind_session_target_and_expiry() {
        let signer = DownloadSigner::generate().expect("signer");
        let session = SessionId::new();
        let at = |ms| Timestamp::from_millis(ms).expect("ts");
        let token = signer.sign(session, at(10_000), "/notices/a/attachments/b");
        assert_eq!(
            signer.verify(&token, "/notices/a/attachments/b", at(9_999)),
            Some(session)
        );
        assert_eq!(
            signer.verify(&token, "/notices/a/attachments/b", at(10_000)),
            None,
            "만료"
        );
        assert_eq!(
            signer.verify(&token, "/notices/a/attachments/c", at(0)),
            None,
            "다른 경로"
        );
        // 만료 시각이나 세션을 바꾸면 서명이 맞지 않는다.
        let forged = token.replacen("10000", "99999", 1);
        assert_eq!(
            signer.verify(&forged, "/notices/a/attachments/b", at(0)),
            None
        );
        let other = format!("{}{}", SessionId::new(), &token[36..]);
        assert_eq!(
            signer.verify(&other, "/notices/a/attachments/b", at(0)),
            None
        );
        // 다른 키(다시 시작한 서버)로는 통과하지 않는다.
        let restarted = DownloadSigner::generate().expect("signer");
        assert_eq!(
            restarted.verify(&token, "/notices/a/attachments/b", at(0)),
            None
        );
        for junk in ["", "a.b", "x.1.y", &format!("{session}.notanumber.ab")] {
            assert_eq!(
                signer.verify(junk, "/notices/a/attachments/b", at(0)),
                None,
                "{junk}"
            );
        }
    }
    use crate::config::Config;
    use pango_core::text::LoginId;
    use std::collections::HashSet;

    /// 받는 쪽 구현이 맞춰 볼 수 있는 기준값(파이썬 `hmac.new(key, msg, sha256)`으로 계산).
    #[test]
    fn webhook_signature_matches_a_known_answer() {
        let secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            webhook_signature(secret, 1_767_225_600, br#"{"event":"test"}"#),
            "sha256=10413878eae66899e60a011545cd94274b162b2d328404748184e6d77858de0d"
        );
        assert_eq!(
            webhook_signature("key", 0, b""),
            "sha256=85841b4efc3cd7776c3c8f9b7cca9e281c550e5d19889d78e9e669c6337f000d"
        );
        // 본문·시각·키 중 하나만 달라도 서명이 바뀐다.
        let base = webhook_signature("key", 1, b"a");
        assert_ne!(base, webhook_signature("key", 2, b"a"));
        assert_ne!(base, webhook_signature("key", 1, b"b"));
        assert_ne!(base, webhook_signature("kez", 1, b"a"));
    }

    #[test]
    fn webhook_secrets_are_random_256_bit_hex() {
        let first = new_webhook_secret().expect("secret");
        let second = new_webhook_secret().expect("secret");
        assert_eq!(first.len(), 64);
        assert!(first
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(first, second);
    }

    fn hasher() -> PasswordHasher {
        PasswordHasher::new(&Config::for_tests().password).expect("hasher")
    }

    fn pw(raw: &str) -> PlainPassword {
        PlainPassword::for_verification(raw).expect("password")
    }

    #[tokio::test]
    async fn hash_then_verify() {
        let hasher = hasher();
        let hash = hasher
            .hash(&pw("correct horse battery"))
            .await
            .expect("hash");
        assert!(hash.starts_with("$argon2id$"));
        assert!(hasher
            .verify(&pw("correct horse battery"), Some(&hash))
            .await
            .expect("verify"));
        assert!(!hasher
            .verify(&pw("correct horse batterx"), Some(&hash))
            .await
            .expect("verify"));
    }

    #[tokio::test]
    async fn same_password_hashes_differently() {
        let hasher = hasher();
        let a = hasher.hash(&pw("same-password-1")).await.expect("hash");
        let b = hasher.hash(&pw("same-password-1")).await.expect("hash");
        assert_ne!(a, b, "솔트가 매번 달라야 한다");
    }

    #[tokio::test]
    async fn missing_account_never_verifies_even_with_dummy_password() {
        let hasher = hasher();
        assert!(!hasher
            .verify(&pw("dummy-password-for-timing"), None)
            .await
            .expect("verify"));
    }

    #[tokio::test]
    async fn corrupt_stored_hash_is_a_mismatch_not_a_panic() {
        let hasher = hasher();
        for stored in ["", "plain-text", "$argon2id$v=19$broken", "$2b$10$notargon"] {
            assert!(!hasher
                .verify(&pw("whatever-pass"), Some(stored))
                .await
                .expect("verify"));
        }
    }

    #[tokio::test]
    async fn hashes_policy_checked_passwords() {
        let login = LoginId::parse("gildong").expect("login");
        let password = PlainPassword::parse_new("유니코드 비밀번호 🔐 ok", &login).expect("valid");
        let hasher = hasher();
        let hash = hasher.hash(&password).await.expect("hash");
        assert!(hasher.verify(&password, Some(&hash)).await.expect("verify"));
    }

    #[test]
    fn tokens_are_unique_hex_and_parse_strictly() {
        let tokens: HashSet<_> = (0..100)
            .map(|_| SessionToken::generate().expect("token").expose().to_owned())
            .collect();
        assert_eq!(tokens.len(), 100);
        let token = SessionToken::generate().expect("token");
        assert_eq!(SessionToken::parse(token.expose()), Some(token.clone()));
        let upper = token.expose().to_uppercase();
        for bad in [
            "",
            "abc",
            upper.as_str(),
            &format!("{}0", token.expose()),
            &"g".repeat(64),
            &" ".repeat(64),
        ] {
            assert!(SessionToken::parse(bad).is_none(), "accepted {bad:?}");
        }
        assert!(!format!("{token:?}").contains(token.expose()));
    }

    #[test]
    fn token_hash_is_stable_and_differs_per_token() {
        let a = SessionToken::generate().expect("token");
        let b = SessionToken::generate().expect("token");
        assert_eq!(a.hash(), a.hash());
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn fingerprint_separates_part_boundaries() {
        assert_ne!(fingerprint(&[b"ab", b"c"]), fingerprint(&[b"a", b"bc"]));
        assert_ne!(fingerprint(&[b"abc"]), fingerprint(&[b"abc", b""]));
        assert_eq!(
            fingerprint(&[b"POST", b"/x", b"{}"]),
            fingerprint(&[b"POST", b"/x", b"{}"])
        );
    }

    #[test]
    fn temporary_passwords_satisfy_policy_and_vary() {
        let login = LoginId::parse("gildong").expect("login");
        let generated: HashSet<_> = (0..50)
            .map(|_| generate_temporary_password().expect("password"))
            .collect();
        assert_eq!(generated.len(), 50);
        for password in &generated {
            assert_eq!(password.len(), 16);
            assert!(!password.contains(['0', 'O', '1', 'l', 'I', 'o']));
            assert!(PlainPassword::parse_new(password, &login).is_ok());
        }
    }
}
