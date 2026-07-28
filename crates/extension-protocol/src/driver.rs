use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::WireId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriverMethod {
    Open,
    Ping,
    Schema,
    Begin,
    Commit,
    Rollback,
    Execute,
    Cancel,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionDisposition {
    Reusable,
    Invalidated,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
    pub configuration: serde_json::Value,
    pub credentials: Vec<CredentialField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialField {
    pub name: String,
    #[serde(with = "byte_vec")]
    #[schemars(with = "String")]
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenResponse {
    pub connection: WireId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandleRequest {
    pub handle: WireId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandleResponse {
    pub handle: WireId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PingResponse {
    pub server_version: String,
    pub current_database: String,
    pub current_user: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecuteStart {
    pub query: WireId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PingRequest {
    pub connection: WireId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchemaRequest {
    pub connection: WireId,
    pub scope: DriverSchemaScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverSchemaScope {
    pub depth: DriverSchemaDepth,
    #[serde(default)]
    pub catalog: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub object: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriverSchemaDepth {
    Shallow,
    Deep,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverSchemaSnapshot {
    pub catalogs: Vec<DriverCatalog>,
    pub fetched_at_unix_ms: i64,
    pub scope: DriverSchemaScope,
    #[serde(default)]
    pub incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverCatalog {
    pub name: String,
    pub namespaces: Vec<DriverNamespace>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverNamespace {
    pub name: String,
    pub objects: Vec<DriverSchemaObject>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverSchemaObject {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub columns: Vec<DriverColumn>,
    /// Provider-owned, JSON-safe metadata that core does not interpret.
    #[serde(default)]
    pub attributes: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BeginRequest {
    pub connection: WireId,
    pub isolation: DriverIsolation,
    pub access: DriverAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriverIsolation {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Snapshot,
    Serializable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriverAccess {
    ReadWrite,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecuteDriverRequest {
    pub connection: WireId,
    pub sql: String,
    pub params: Vec<DriverValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    pub connection: WireId,
    pub query: WireId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum DriverValue {
    Null {
        type_name: String,
    },
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    Decimal(String),
    String(String),
    Bytes(
        #[serde(with = "byte_vec")]
        #[schemars(with = "String")]
        Vec<u8>,
    ),
    Json(serde_json::Value),
    Date(String),
    Time(String),
    Timestamp(String),
    TimestampTz(String),
    Uuid(String),
    IntervalMicros(i64),
    Engine {
        type_name: String,
        display: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "frame", content = "body", rename_all = "snake_case")]
pub enum DriverStreamPayload {
    NextResult {
        columns: Vec<DriverColumn>,
    },
    Rows {
        rows: Vec<Vec<DriverValue>>,
    },
    Done {
        affected_rows: Option<u64>,
        warnings: Vec<String>,
    },
    Error {
        code: String,
        message: String,
        disposition: ConnectionDisposition,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverColumn {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
}

mod byte_vec {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use std::fmt::Write;
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
        serializer.serialize_str(&encoded)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() % 2 != 0
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(serde::de::Error::custom(
                "bytes must be lowercase, even-length hexadecimal",
            ));
        }
        (0..encoded.len())
            .step_by(2)
            .map(|index| {
                u8::from_str_radix(&encoded[index..index + 2], 16).map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_bytes_have_one_canonical_json_encoding() {
        let field = CredentialField {
            name: "password".into(),
            value: vec![0, 1, 254, 255],
        };
        let encoded = serde_json::to_string(&field).unwrap();
        assert_eq!(encoded, r#"{"name":"password","value":"0001feff"}"#);
        assert_eq!(
            serde_json::from_str::<CredentialField>(&encoded).unwrap(),
            field
        );
    }
}
