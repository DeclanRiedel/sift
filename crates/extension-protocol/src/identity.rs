use std::{fmt, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const MAX_SEGMENT_BYTES: usize = 63;
const MAX_ID_BYTES: usize = 191;

/// Why a namespaced extension identifier was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    Empty,
    TooLong,
    WrongSegmentCount { expected: usize, actual: usize },
    InvalidSegment { index: usize },
    ReservedPublisher,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("identifier cannot be empty"),
            Self::TooLong => f.write_str("identifier exceeds its byte limit"),
            Self::WrongSegmentCount { expected, actual } => {
                write!(f, "expected {expected} identifier segments, got {actual}")
            }
            Self::InvalidSegment { index } => {
                write!(f, "identifier segment {index} is invalid")
            }
            Self::ReservedPublisher => f.write_str("the sift publisher namespace is reserved"),
        }
    }
}

impl std::error::Error for IdError {}

fn valid_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SEGMENT_BYTES {
        return false;
    }
    let is_alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    is_alphanumeric(bytes[0])
        && is_alphanumeric(bytes[bytes.len() - 1])
        && bytes
            .iter()
            .all(|byte| is_alphanumeric(*byte) || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate(value: &str, segments: usize) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(IdError::Empty);
    }
    if value.len() > MAX_ID_BYTES {
        return Err(IdError::TooLong);
    }
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() != segments {
        return Err(IdError::WrongSegmentCount {
            expected: segments,
            actual: parts.len(),
        });
    }
    if let Some(index) = parts.iter().position(|part| !valid_segment(part)) {
        return Err(IdError::InvalidSegment { index });
    }
    Ok(())
}

macro_rules! namespaced_id {
    ($name:ident, $segments:expr) => {
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
        )]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                validate(&value, $segments)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn publisher(&self) -> &str {
                self.0
                    .split('/')
                    .next()
                    .expect("validated id has a publisher")
            }

            pub fn is_first_party(&self) -> bool {
                self.publisher() == "sift"
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

namespaced_id!(ExtensionId, 2);
namespaced_id!(ProviderId, 2);
namespaced_id!(DialectId, 2);
namespaced_id!(ContributionId, 4);

/// A manifest-local identifier or operation action.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct SegmentId(String);

impl SegmentId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        if !valid_segment(&value) {
            return Err(IdError::InvalidSegment { index: 0 });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SegmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SegmentId {
    type Err = IdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for SegmentId {
    type Error = IdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SegmentId> for String {
    fn from(value: SegmentId) -> Self {
        value.0
    }
}

/// A JSON-safe fixed-width representation of an opaque 128-bit wire id.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct WireId(u128);

impl WireId {
    pub const fn from_u128(value: u128) -> Self {
        Self(value)
    }

    pub const fn as_u128(self) -> u128 {
        self.0
    }
}

impl fmt::Display for WireId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl FromStr for WireId {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("wire id must be 32 lowercase hexadecimal characters");
        }
        u128::from_str_radix(value, 16)
            .map(Self)
            .map_err(|_| "wire id is outside the 128-bit range")
    }
}

impl TryFrom<String> for WireId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<WireId> for String {
    fn from(value: WireId) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_ids_reject_noncanonical_input() {
        assert!(ProviderId::new("acme/database").is_ok());
        assert!(ProviderId::new("Acme/database").is_err());
        assert!(ProviderId::new("acme//database").is_err());
        assert!(ContributionId::new("acme/ext/command/run").is_ok());
        assert!(ContributionId::new("acme/ext/run").is_err());
    }

    #[test]
    fn wire_ids_are_fixed_width_lowercase_hex() {
        let id = WireId::from_u128(42);
        let encoded = serde_json::to_string(&id).unwrap();
        assert_eq!(encoded, "\"0000000000000000000000000000002a\"");
        assert_eq!(serde_json::from_str::<WireId>(&encoded).unwrap(), id);
        assert!(serde_json::from_str::<WireId>("\"2a\"").is_err());
        assert!(serde_json::from_str::<WireId>("\"0000000000000000000000000000002A\"").is_err());
    }
}
