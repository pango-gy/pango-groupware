//! Nostr 이벤트 서명(NIP-01, BIP340 Schnorr)과 HTTP 인증 머리글(NIP-98).
//!
//! Buzz 봇 계정으로 DM을 열고 메시지를 보낼 때 쓴다. 비밀키는 [`BotKey`]에만 두고 `Debug`·
//! 오류 문구 어디에도 드러나지 않는다.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use k256::schnorr::SigningKey;
use pango_core::nostr::{decode_key, to_hex, PublicKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// NIP-98 HTTP 인증 이벤트 종류.
pub const KIND_HTTP_AUTH: u32 = 27235;

/// 봇 비밀키. 서버 실행 설정으로만 받는다(DB·관리자 화면에는 두지 않는다).
#[derive(Clone)]
pub struct BotKey {
    signing: SigningKey,
}

impl std::fmt::Debug for BotKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BotKey({})", self.public_key().short())
    }
}

/// 비밀키를 읽지 못했다. 입력값은 오류에 담지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Buzz bot key must be an nsec1… string or 64 hex characters for a valid secp256k1 key")]
pub struct InvalidBotKey;

impl BotKey {
    /// `nsec1…` 또는 64자 16진수.
    pub fn parse(raw: &str) -> Result<Self, InvalidBotKey> {
        let bytes = decode_key(raw, "nsec", "bot_key").map_err(|_| InvalidBotKey)?;
        let signing = SigningKey::from_bytes(&bytes).map_err(|_| InvalidBotKey)?;
        Ok(Self { signing })
    }

    pub fn public_key(&self) -> PublicKey {
        let bytes: [u8; 32] = self.signing.verifying_key().to_bytes().into();
        PublicKey::from_bytes(bytes)
    }

    /// 이벤트를 서명한다. `aux`는 BIP340의 보조 난수(32바이트).
    pub fn sign(&self, unsigned: UnsignedEvent, aux: &[u8; 32]) -> Result<Event, SignError> {
        let pubkey = self.public_key().to_hex();
        let id = event_id(
            &pubkey,
            unsigned.created_at,
            unsigned.kind,
            &unsigned.tags,
            &unsigned.content,
        );
        let signature = self.signing.sign_raw(&id, aux).map_err(|_| SignError)?;
        Ok(Event {
            id: to_hex(&id),
            pubkey,
            created_at: unsigned.created_at,
            kind: unsigned.kind,
            tags: unsigned.tags,
            content: unsigned.content,
            sig: to_hex(&signature.to_bytes()),
        })
    }

    /// 새 보조 난수로 서명한다.
    pub fn sign_random(&self, unsigned: UnsignedEvent) -> Result<Event, SignError> {
        let mut aux = [0u8; 32];
        getrandom::fill(&mut aux).map_err(|_| SignError)?;
        self.sign(unsigned, &aux)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("failed to sign a Nostr event")]
pub struct SignError;

/// 서명 전 이벤트.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedEvent {
    pub created_at: i64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

/// 서명한 이벤트(NIP-01 JSON 모양 그대로).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

/// NIP-01 이벤트 ID: `[0, pubkey, created_at, kind, tags, content]`를 공백 없는 JSON으로 적어
/// SHA-256. 문자열은 JSON 규칙대로 이스케이프한다(serde_json은 `"`·`\`·제어 문자만 바꾸고
/// 한글 등은 UTF-8 그대로 둔다. 릴레이가 쓰는 `nostr` 크레이트도 serde_json으로 같은 값을 만든다).
pub fn event_id(
    pubkey_hex: &str,
    created_at: i64,
    kind: u32,
    tags: &[Vec<String>],
    content: &str,
) -> [u8; 32] {
    let canonical = serde_json::json!([0, pubkey_hex, created_at, kind, tags, content]);
    Sha256::digest(canonical.to_string().as_bytes()).into()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    to_hex(&Sha256::digest(bytes))
}

/// NIP-98 `Authorization` 머리글 값: `Nostr <base64(서명한 kind 27235 이벤트)>`.
/// `url`은 요청 주소 그대로, `payload`에는 본문의 SHA-256을 늘 넣는다.
pub fn http_auth_header(
    key: &BotKey,
    url: &str,
    method: &str,
    body: &[u8],
    created_at: i64,
) -> Result<String, SignError> {
    let event = key.sign_random(UnsignedEvent {
        created_at,
        kind: KIND_HTTP_AUTH,
        tags: vec![
            vec!["u".into(), url.into()],
            vec!["method".into(), method.into()],
            vec!["payload".into(), sha256_hex(body)],
        ],
        content: String::new(),
    })?;
    let json = serde_json::to_string(&event).map_err(|_| SignError)?;
    Ok(format!("Nostr {}", BASE64.encode(json)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::schnorr::{Signature, VerifyingKey};

    fn bytes32(hex: &str) -> [u8; 32] {
        decode_key(hex, "nsec", "x").expect("hex")
    }

    /// BIP340 공식 테스트 벡터(bip-0340/test-vectors.csv 0~3번).
    #[test]
    fn bip340_signing_vectors() {
        let cases = [
            (
                "0000000000000000000000000000000000000000000000000000000000000003",
                "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "e907831f80848d1069a5371b402410364bdf1c5f8307b0084c55f1ce2dca821525f66a4a85ea8b71e482a74f382d2ce5ebeee8fdb2172f477df4900d310536c0",
            ),
            (
                "b7e151628aed2a6abf7158809cf4f3c762e7160f38b4da56a784d9045190cfef",
                "dff1d77f2a671c5f36183726db2341be58feae1da2deced843240f7b502ba659",
                "0000000000000000000000000000000000000000000000000000000000000001",
                "243f6a8885a308d313198a2e03707344a4093822299f31d0082efa98ec4e6c89",
                "6896bd60eeae296db48a229ff71dfe071bde413e6d43f917dc8dcf8c78de33418906d11ac976abccb20b091292bff4ea897efcb639ea871cfa95f6de339e4b0a",
            ),
            (
                "c90fdaa22168c234c4c6628b80dc1cd129024e088a67cc74020bbea63b14e5c9",
                "dd308afec5777e13121fa72b9cc1b7cc0139715309b086c960e18fd969774eb8",
                "c87aa53824b4d7ae2eb035a2b5bbbccc080e76cdc6d1692c4b0b62d798e6d906",
                "7e2d58d8b3bcdf1abadec7829054f90dda9805aab56c77333024b9d0a508b75c",
                "5831aaeed7b44bb74e5eab94ba9d4294c49bcf2a60728d8b4c200f50dd313c1bab745879a5ad954a72c45a91c3a51d3c7adea98d82f8481e0e1e03674a6f3fb7",
            ),
            (
                "0b432b2677937381aef05bb02a66ecd012773062cf3fa2549e44f58ed2401710",
                "25d1dff95105f5253c4022f628a996ad3a0d95fbf21d468a1b33f8c160d8f517",
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "7eb0509757e246f19449885651611cb965ecc1a187dd51b64fda1edc9637d5ec97582b9cb13db3933705b32ba982af5af25fd78881ebb32771fc5922efc66ea3",
            ),
        ];
        for (secret, pubkey, aux, message, signature) in cases {
            let key = BotKey::parse(secret).expect("key");
            assert_eq!(key.public_key().to_hex(), pubkey);
            let sig = key
                .signing
                .sign_raw(&bytes32(message), &bytes32(aux))
                .expect("sign");
            assert_eq!(to_hex(&sig.to_bytes()), signature);
            let verifying = VerifyingKey::from_bytes(&bytes32(pubkey)).expect("pubkey");
            verifying
                .verify_raw(&bytes32(message), &sig)
                .expect("verify");
        }
    }

    /// 파이썬으로 계산한 NIP-01 ID(`json.dumps(…, separators=(",", ":"), ensure_ascii=False)`).
    #[test]
    fn event_id_matches_the_nip01_serialization() {
        let pubkey = "dff1d77f2a671c5f36183726db2341be58feae1da2deced843240f7b502ba659";
        let tags = vec![
            vec![
                "h".to_owned(),
                "c0ffee00-0000-4000-8000-000000000001".to_owned(),
            ],
            vec!["ms".to_owned(), "123".to_owned()],
        ];
        let id = event_id(
            pubkey,
            1_767_225_600,
            9,
            &tags,
            "결재 요청: \"출장비\"\n줄\\바꿈\t탭",
        );
        assert_eq!(
            to_hex(&id),
            "3375748bce020de1c45c6b26c7f1738533ee16e58c9a990cdc0dba6dc23a3df1"
        );
        let empty = event_id(pubkey, 0, 1, &[], "");
        assert_eq!(
            to_hex(&empty),
            "2c02985e2b3f880d71ee605649b1cb187c676eae5312a51ec819ce3f5f918e79"
        );
    }

    #[test]
    fn signed_events_verify_and_tampering_breaks_them() {
        let key = BotKey::parse("b7e151628aed2a6abf7158809cf4f3c762e7160f38b4da56a784d9045190cfef")
            .expect("key");
        let event = key
            .sign_random(UnsignedEvent {
                created_at: 1_767_225_600,
                kind: 41010,
                tags: vec![vec!["p".into(), "ab".repeat(32)]],
                content: String::new(),
            })
            .expect("sign");
        let verifying = VerifyingKey::from_bytes(&bytes32(&event.pubkey)).expect("pubkey");
        let id = event_id(
            &event.pubkey,
            event.created_at,
            event.kind,
            &event.tags,
            &event.content,
        );
        assert_eq!(to_hex(&id), event.id);
        let sig_bytes = decode_sig(&event.sig);
        let sig = Signature::try_from(sig_bytes.as_slice()).expect("sig");
        verifying.verify_raw(&id, &sig).expect("verify");
        let other = event_id(
            &event.pubkey,
            event.created_at + 1,
            event.kind,
            &event.tags,
            "",
        );
        assert!(verifying.verify_raw(&other, &sig).is_err());
    }

    fn decode_sig(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    }

    #[test]
    fn bot_keys_never_show_the_secret() {
        let secret = "b7e151628aed2a6abf7158809cf4f3c762e7160f38b4da56a784d9045190cfef";
        let key = BotKey::parse(secret).expect("key");
        let shown = format!("{key:?}");
        assert!(
            !shown.contains(secret) && !shown.contains("b7e15162"),
            "{shown}"
        );
        assert!(shown.starts_with("BotKey(npub1"));
        for bad in [
            "",
            "npub1dlcawlu4wywt7dscxundmy2phev8atsa5t0vakzryl3h4gptkfvsnu8pyd",
            &"0".repeat(64),
            &"f".repeat(64),
            "not-a-key",
        ] {
            let error = BotKey::parse(bad).expect_err(bad);
            assert!(!error.to_string().contains(bad) || bad.is_empty());
        }
    }

    #[test]
    fn http_auth_carries_url_method_and_payload_hash() {
        let key = BotKey::parse("b7e151628aed2a6abf7158809cf4f3c762e7160f38b4da56a784d9045190cfef")
            .expect("key");
        let header = http_auth_header(&key, "https://buzz.example.com/events", "POST", b"{}", 100)
            .expect("header");
        let json = BASE64
            .decode(header.strip_prefix("Nostr ").expect("scheme"))
            .expect("base64");
        let event: Event = serde_json::from_slice(&json).expect("event");
        assert_eq!(event.kind, KIND_HTTP_AUTH);
        assert_eq!(event.created_at, 100);
        assert_eq!(
            event.tags,
            vec![
                vec!["u".to_owned(), "https://buzz.example.com/events".to_owned()],
                vec!["method".to_owned(), "POST".to_owned()],
                vec!["payload".to_owned(), sha256_hex(b"{}")],
            ]
        );
        assert_eq!(event.pubkey, key.public_key().to_hex());
    }
}
