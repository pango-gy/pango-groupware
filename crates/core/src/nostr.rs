//! Nostr 키 표기(NIP-19 `npub`/`nsec`, 64자 16진수). 사내 메신저 Buzz는 Nostr 릴레이라 사용자가
//! 32바이트 공개키로 구분된다.
//!
//! 서명은 여기서 하지 않는다(서버의 `nostr` 모듈). 여기는 입력을 검증하고 한 가지 표기로 맞춘다.

use bech32::{Bech32, Hrp};

use crate::error::{DomainError, DomainResult, ValidationReason};

/// 표기 오류. 어느 필드였는지는 [`DomainError::with_field`]로 바꾼다.
fn invalid(field: &'static str) -> DomainError {
    DomainError::validation(field, ValidationReason::InvalidFormat)
}

fn hex32(raw: &str) -> Option<[u8; 32]> {
    if raw.len() != 64 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, pair) in raw.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(pair).ok()?;
        out[index] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(out)
}

/// `hrp`(`npub`·`nsec`)로 시작하는 bech32 문자열 또는 64자 16진수를 32바이트로 읽는다.
/// 대소문자를 섞은 bech32, 다른 접두사, 길이가 다른 값, 체크섬 오류는 거부한다.
pub fn decode_key(raw: &str, hrp: &str, field: &'static str) -> DomainResult<[u8; 32]> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DomainError::validation(field, ValidationReason::Empty));
    }
    // bech32 문자열은 최대 90자. 키는 63자이므로 그보다 긴 입력은 볼 필요가 없다.
    if trimmed.len() > 100 {
        return Err(invalid(field));
    }
    if let Some(bytes) = hex32(trimmed) {
        return Ok(bytes);
    }
    let (found, data) = bech32::decode(trimmed).map_err(|_| invalid(field))?;
    if !found.as_str().eq_ignore_ascii_case(hrp) {
        return Err(invalid(field));
    }
    data.try_into().map_err(|_| invalid(field))
}

pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 0x0f)]])
        .map(char::from)
        .collect()
}

/// Nostr 공개키(x 좌표 32바이트). 저장·비교는 소문자 16진수로 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    pub const FIELD: &'static str = "pubkey";

    /// `npub1…` 또는 64자 16진수. 점(secp256k1 곡선 위의 점)인지까지는 여기서 보지 않는다
    /// (서명할 때·받는 쪽이 확인한다).
    pub fn parse(raw: &str) -> DomainResult<Self> {
        decode_key(raw, "npub", Self::FIELD).map(Self)
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    pub fn to_npub(&self) -> String {
        let hrp = Hrp::parse_unchecked("npub");
        // 32바이트는 bech32 길이 한도 안이라 실패하지 않는다.
        bech32::encode::<Bech32>(hrp, &self.0).unwrap_or_default()
    }

    /// 화면·감사 로그에 쓰는 줄인 표기: `npub1abcd…wxyz`(앞 10자, 뒤 4자).
    pub fn short(&self) -> String {
        let npub = self.to_npub();
        let head: String = npub.chars().take(10).collect();
        let tail: String = npub
            .chars()
            .skip(npub.chars().count().saturating_sub(4))
            .collect();
        format!("{head}…{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NIP-19 문서의 예.
    const NPUB: &str = "npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg";
    const HEX: &str = "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e";

    #[test]
    fn npub_and_hex_name_the_same_key() {
        let from_npub = PublicKey::parse(NPUB).expect("npub");
        let from_hex = PublicKey::parse(HEX).expect("hex");
        assert_eq!(from_npub, from_hex);
        assert_eq!(from_npub.to_hex(), HEX);
        assert_eq!(from_npub.to_npub(), NPUB);
        assert_eq!(
            PublicKey::parse(&HEX.to_uppercase()).expect("upper"),
            from_hex
        );
        assert_eq!(
            PublicKey::parse(&format!("  {NPUB}\n")).expect("trim"),
            from_hex
        );
        assert_eq!(
            PublicKey::parse(&NPUB.to_uppercase()).expect("upper bech32"),
            from_hex
        );
        assert_eq!(from_hex.short(), "npub10elfc…jptg");
    }

    #[test]
    fn malformed_keys_are_rejected() {
        let nsec = "nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5";
        let mut wrong_checksum = NPUB.to_owned();
        wrong_checksum.pop();
        wrong_checksum.push('h');
        let mixed_case = format!("{}{}", &NPUB[..10].to_uppercase(), &NPUB[10..]);
        for bad in [
            nsec,
            &wrong_checksum,
            &mixed_case,
            &HEX[..63],
            &format!("{HEX}0"),
            &format!("{}g", &HEX[..63]),
            "npub1",
            "note10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg",
            "hello",
            &"a".repeat(500),
        ] {
            assert_eq!(
                PublicKey::parse(bad),
                Err(DomainError::validation(
                    "pubkey",
                    ValidationReason::InvalidFormat
                )),
                "{bad}"
            );
        }
        assert_eq!(
            PublicKey::parse(" "),
            Err(DomainError::validation("pubkey", ValidationReason::Empty))
        );
    }

    #[test]
    fn secret_keys_decode_from_nsec_or_hex() {
        // NIP-19 문서의 nsec 예.
        let nsec = "nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5";
        let hex = "67dea2ed018072d675f5415ecfaed7d2597555e202d85b3d65ea4e58d2d92ffa";
        assert_eq!(to_hex(&decode_key(nsec, "nsec", "key").expect("nsec")), hex);
        assert_eq!(to_hex(&decode_key(hex, "nsec", "key").expect("hex")), hex);
        assert!(
            decode_key(NPUB, "nsec", "key").is_err(),
            "npub은 비밀키가 아니다"
        );
    }
}
