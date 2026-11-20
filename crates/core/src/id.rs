//! 타입별 식별자. 모두 UUID v7이며, 서로 다른 종류의 ID를 섞어 쓰면 컴파일되지 않는다.

use crate::error::{DomainError, ValidationReason};

macro_rules! define_id {
    ($($(#[$meta:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(uuid::Uuid);

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        // 역직렬화도 `parse`를 거쳐 표준 형식만 받는다.
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                Self::parse(&raw).map_err(serde::de::Error::custom)
            }
        }

        impl $name {
            /// 시간순으로 정렬되는 새 ID.
            pub fn new() -> Self {
                Self(uuid::Uuid::now_v7())
            }

            pub const fn from_uuid(value: uuid::Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(&self) -> uuid::Uuid {
                self.0
            }

            /// 하이픈이 있는 소문자 표준 형식만 받는다. 표기가 여러 개면 같은 ID가
            /// DB에서 다른 문자열로 저장될 수 있기 때문이다.
            pub fn parse(value: &str) -> Result<Self, DomainError> {
                const INVALID: DomainError =
                    DomainError::validation("id", ValidationReason::InvalidFormat);
                if value.len() != 36 || value.bytes().any(|b| b.is_ascii_uppercase()) {
                    return Err(INVALID);
                }
                uuid::Uuid::try_parse(value).map(Self).map_err(|_| INVALID)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.as_hyphenated().fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = DomainError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }
    )+};
}

define_id!(
    MemberId,
    DepartmentId,
    PositionId,
    SessionId,
    AttendanceRecordId,
    AttendanceEventId,
    LeaveTypeId,
    LeaveRequestId,
    LeaveLedgerEntryId,
    FormId,
    DocumentId,
    ApprovalStepId,
    NoticeId,
    NotificationId,
    AuditLogId,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_display_and_parse() {
        let id = MemberId::new();
        assert_eq!(MemberId::parse(&id.to_string()), Ok(id));
    }

    #[test]
    fn new_ids_sort_by_creation_order() {
        let ids: Vec<_> = (0..200).map(|_| DocumentId::new()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn rejects_non_canonical_forms() {
        let id = MemberId::new().to_string();
        let cases = [
            String::new(),
            id.to_uppercase(),
            id.replace('-', ""),
            format!("{{{id}}}"),
            format!("urn:uuid:{id}"),
            format!(" {id}"),
            format!("{id}\n"),
            "not-a-uuid".to_owned(),
            "1' OR '1'='1".to_owned(),
            "0".repeat(36),
        ];
        for case in cases {
            assert!(MemberId::parse(&case).is_err(), "accepted {case:?}");
        }
    }

    #[test]
    fn serializes_as_plain_string() {
        let id = NoticeId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(
            serde_json::from_str::<NoticeId>(&json).expect("deserialize"),
            id
        );
    }

    #[test]
    fn deserialization_rejects_non_canonical_forms() {
        let upper = format!("\"{}\"", MemberId::new().to_string().to_uppercase());
        assert!(serde_json::from_str::<MemberId>(&upper).is_err());
        assert!(serde_json::from_str::<MemberId>("123").is_err());
        assert!(serde_json::from_str::<MemberId>("null").is_err());
    }
}
