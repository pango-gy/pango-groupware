//! 2단계 인증: TOTP(RFC 6238, HMAC-SHA1, 6자리, 30초)와 복구 코드.
//!
//! 인증 앱(Google Authenticator 등)과 맞추기 위해 표준 기본값만 쓴다. 비밀키는 160비트 난수이고
//! base32(RFC 4648, 패딩 없음)로 보여 준다.

use hmac::{Hmac, Mac};
use pango_core::time::Timestamp;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::error::{ServiceError, ServiceResult};

/// 한 단계 길이(초).
pub const STEP_SECS: i64 = 30;
/// 앞뒤로 받아 줄 단계 수(기기 시계 오차).
const SKEW_STEPS: i64 = 1;
pub const SECRET_LEN: usize = 20;
/// 처음 켤 때 주는 복구 코드 수.
pub const RECOVERY_CODES: usize = 10;

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    for chunk in bytes.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let bits = u64::from_be_bytes([0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4]]);
        let chars = (chunk.len() * 8).div_ceil(5);
        for i in 0..chars {
            let index = (bits >> (35 - i * 5)) & 0x1f;
            out.push(char::from(BASE32[index as usize]));
        }
    }
    out
}

fn random<const N: usize>() -> ServiceResult<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| ServiceError::internal(e.to_string()))?;
    Ok(bytes)
}

pub fn generate_secret() -> ServiceResult<[u8; SECRET_LEN]> {
    random::<SECRET_LEN>()
}

/// RFC 4226 HOTP(6자리).
fn hotp(secret: &[u8], counter: u64) -> u32 {
    // HMAC은 어떤 길이의 키든 받는다.
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(secret)
        .unwrap_or_else(|_| unreachable!("HMAC accepts keys of any length"));
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[19] & 0x0f);
    let value = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    value % 1_000_000
}

/// 지금 단계의 코드(6자리). 서버는 쓰지 않고, 테스트와 운영 점검 도구가 쓴다.
pub fn current_code(secret: &[u8], now: Timestamp) -> String {
    let step = u64::try_from(step_at(now)).unwrap_or_default();
    format!("{:06}", hotp(secret, step))
}

/// base32(대소문자 무시, 패딩 없음)를 되돌린다. 잘못된 글자가 있으면 `None`.
pub fn decode_base32(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let (mut buffer, mut bits) = (0u64, 0u32);
    for c in text.bytes() {
        let value = BASE32.iter().position(|b| *b == c.to_ascii_uppercase())?;
        buffer = (buffer << 5) | value as u64;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn step_at(now: Timestamp) -> i64 {
    now.as_millis().div_euclid(1000).div_euclid(STEP_SECS)
}

/// 입력한 6자리 코드를 확인한다. 맞으면 그 단계를 돌려준다. `last_step` 이하의 단계는 받지 않는다
/// (같은 코드를 두 번 쓰지 못하게).
pub fn verify(secret: &[u8], code: &str, now: Timestamp, last_step: Option<i64>) -> Option<i64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let given: u32 = code.parse().ok()?;
    let current = step_at(now);
    (current - SKEW_STEPS..=current + SKEW_STEPS)
        .filter(|step| last_step.is_none_or(|last| *step > last))
        .find(|step| u64::try_from(*step).is_ok_and(|counter| hotp(secret, counter) == given))
}

/// 인증 앱에 넣는 주소(`otpauth://totp/…`). 앱 안의 QR·링크로 쓴다.
pub fn otpauth_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    let encode = |value: &str| -> String {
        value
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    };
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits=6&period={STEP_SECS}",
        encode(issuer),
        encode(account),
        base32(secret),
        encode(issuer)
    )
}

/// 복구 코드(`abcd-efgh`, 소문자 base32 8자). 보여 줄 값과 저장할 해시.
pub fn generate_recovery_codes() -> ServiceResult<Vec<(String, [u8; 32])>> {
    (0..RECOVERY_CODES)
        .map(|_| {
            let raw = base32(&random::<5>()?).to_lowercase();
            let code = format!("{}-{}", &raw[..4], &raw[4..]);
            let hash = recovery_hash(&code).ok_or_else(|| ServiceError::internal("bad code"))?;
            Ok((code, hash))
        })
        .collect()
}

/// 입력한 복구 코드의 해시. 대소문자·공백·하이픈 차이는 무시한다. 형식이 아니면 `None`.
pub fn recovery_hash(input: &str) -> Option<[u8; 32]> {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    if cleaned.len() != 8
        || !cleaned
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"pango-recovery\0");
    hasher.update(cleaned.as_bytes());
    Some(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_millis(secs * 1000).expect("ts")
    }

    /// RFC 6238 부록 B의 SHA1 시험값(8자리의 끝 6자리).
    #[test]
    fn matches_rfc_6238_vectors() {
        let secret = b"12345678901234567890";
        for (secs, code) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert_eq!(
                format!("{:06}", hotp(secret, (secs / 30) as u64)),
                code,
                "{secs}"
            );
            assert!(verify(secret, code, at(secs), None).is_some(), "{secs}");
        }
    }

    #[test]
    fn verify_allows_one_step_of_skew_and_rejects_reuse() {
        let secret = b"12345678901234567890";
        let step = step_at(at(59));
        assert_eq!(
            verify(secret, "287082", at(59 + 30), None),
            Some(step),
            "한 단계 늦게"
        );
        assert_eq!(
            verify(secret, "287082", at(59 + 60), None),
            None,
            "두 단계는 안 됨"
        );
        assert_eq!(
            verify(secret, "287082", at(59), Some(step)),
            None,
            "다시 쓰기"
        );
        assert_eq!(
            verify(secret, "287 082", at(59), None),
            Some(step),
            "공백 허용"
        );
        for bad in ["", "28708", "2870822", "abcdef", "28708x"] {
            assert_eq!(verify(secret, bad, at(59), None), None, "{bad}");
        }
    }

    #[test]
    fn base32_round_trips() {
        for bytes in [&b""[..], b"f", b"foobar", b"12345678901234567890"] {
            assert_eq!(decode_base32(&base32(bytes)).as_deref(), Some(bytes));
        }
        assert_eq!(decode_base32("mzxw6ytboi").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(decode_base32("MZ1"), None);
    }

    #[test]
    fn base32_matches_rfc_4648_without_padding() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(
            base32(b"12345678901234567890"),
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
        );
    }

    #[test]
    fn recovery_codes_are_unique_and_normalized() {
        let codes = generate_recovery_codes().expect("codes");
        assert_eq!(codes.len(), RECOVERY_CODES);
        let (code, hash) = &codes[0];
        assert_eq!(code.len(), 9);
        assert_eq!(recovery_hash(&code.to_uppercase()), Some(*hash));
        assert_eq!(recovery_hash(&code.replace('-', " ")), Some(*hash));
        assert_eq!(recovery_hash("123456"), None);
        let unique: std::collections::BTreeSet<_> = codes.iter().map(|(c, _)| c).collect();
        assert_eq!(unique.len(), RECOVERY_CODES);
    }

    #[test]
    fn otpauth_uri_escapes_names() {
        let uri = otpauth_uri("우리 회사", "kim", b"12345678901234567890");
        assert!(uri.starts_with("otpauth://totp/%EC%9A%B0"));
        assert!(uri.contains("secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"));
    }
}
