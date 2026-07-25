//! Typed wrappers for the Loro collaboration protocol.
//!
//! CRDT bytes — snapshots, updates, version vectors, frontiers, and cursors —
//! travel inside JSON messages as standard padded RFC 4648 base64 strings. Each
//! payload gets its own newtype so a frontier can never be passed where an
//! update is expected. Opaque identifiers (replica, room connection, room
//! result) are likewise distinct types.
//!
//! This module stays pure serde data (ADR-003): no Tokio, filesystem, or
//! network dependency, and no Loro dependency — the bytes are opaque here and
//! only `sift-doc` interprets them.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Define a base64-over-JSON byte newtype with serde and a `string` JSON schema.
macro_rules! base64_bytes {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
        pub struct $name(pub Vec<u8>);

        impl $name {
            pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
                Self(bytes.into())
            }
            pub fn as_bytes(&self) -> &[u8] {
                &self.0
            }
            pub fn into_bytes(self) -> Vec<u8> {
                self.0
            }
            pub fn len(&self) -> usize {
                self.0.len()
            }
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
            /// The base64 text form used on the wire.
            pub fn to_base64(&self) -> String {
                B64.encode(&self.0)
            }
        }

        impl From<Vec<u8>> for $name {
            fn from(bytes: Vec<u8>) -> Self {
                Self(bytes)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&B64.encode(&self.0))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                let bytes = B64
                    .decode(text.as_bytes())
                    .map_err(serde::de::Error::custom)?;
                Ok(Self(bytes))
            }
        }

        impl schemars::JsonSchema for $name {
            fn schema_name() -> String {
                stringify!($name).to_string()
            }
            fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
                let mut schema = <String as schemars::JsonSchema>::json_schema(gen);
                if let schemars::schema::Schema::Object(ref mut obj) = schema {
                    obj.metadata().description = Some(
                        "standard padded RFC 4648 base64 of Loro CRDT bytes".to_string(),
                    );
                }
                schema
            }
        }
    };
}

base64_bytes! {
    /// Encoded Loro version vector describing everything a replica has seen.
    DocumentVersion
}
base64_bytes! {
    /// Encoded Loro frontiers pinning an exact document version.
    DocumentFrontier
}
base64_bytes! {
    /// A Loro update or snapshot-chunk payload.
    CrdtUpdate
}
base64_bytes! {
    /// Encoded Loro snapshot (full history plus state).
    CrdtSnapshot
}
base64_bytes! {
    /// Encoded stable Loro cursor for a presence anchor.
    CrdtCursor
}

/// A replica's durable, random, non-zero peer id (a Loro `PeerID`, `u64`).
///
/// Serialized as a decimal string because the full `u64` range exceeds the
/// JavaScript safe-integer limit; a JSON number would silently lose precision
/// in browser clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReplicaId(pub u64);

impl std::fmt::Display for ReplicaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for ReplicaId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for ReplicaId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse()
            .map(ReplicaId)
            .map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for ReplicaId {
    fn schema_name() -> String {
        "ReplicaId".to_string()
    }
    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        <String as schemars::JsonSchema>::json_schema(gen)
    }
}

/// Opaque identifier for a room-owned shared connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomConnectionId(pub Uuid);

impl std::fmt::Display for RoomConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque identifier for a transient shared result. Deliberately distinct from
/// the driver `CursorId`, which is never exposed to room members.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomResultId(pub Uuid);

impl std::fmt::Display for RoomResultId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_newtype_round_trips_through_json() {
        let update = CrdtUpdate::new(vec![0u8, 1, 2, 250, 255]);
        let json = serde_json::to_string(&update).unwrap();
        assert_eq!(json, r#""AAEC+v8=""#); // padded standard base64
        let back: CrdtUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(back, update);
    }

    #[test]
    fn distinct_newtypes_are_not_interchangeable_but_share_encoding() {
        // Same bytes, same wire form, different Rust types.
        let bytes = vec![9u8, 8, 7];
        let a = serde_json::to_string(&DocumentVersion::new(bytes.clone())).unwrap();
        let b = serde_json::to_string(&DocumentFrontier::new(bytes)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn replica_id_serializes_as_string_to_survive_javascript() {
        let id = ReplicaId(0xFFFF_FFFF_FFFF_FF00);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""18446744073709551360""#);
        let back: ReplicaId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn rejects_non_base64_payload() {
        assert!(serde_json::from_str::<CrdtUpdate>(r#""not base64!!""#).is_err());
    }
}
