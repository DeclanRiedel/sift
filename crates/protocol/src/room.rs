use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomPresence {
    pub attachment_id: i64,
    pub principal_id: i64,
    pub client_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoomQueryStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomQueryResult {
    pub room_id: i64,
    pub actor_principal_id: i64,
    pub connection_profile_id: Option<i64>,
    pub sql_text: String,
    pub row_count: Option<i64>,
    pub status: RoomQueryStatus,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoomClientMessage {
    Reauthenticate { access_token: crate::RedactedString },
    Attach { client_id: String },
    Detach,
    PresencePing,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoomServerMessage {
    Authenticated {
        expires_at: chrono::DateTime<chrono::Utc>,
    },
    Attached {
        attachment_id: i64,
        presence: Vec<RoomPresence>,
    },
    Presence {
        presence: Vec<RoomPresence>,
    },
    QueryResult {
        result: RoomQueryResult,
    },
    Error {
        message: String,
    },
    RateLimited {
        retry_after_ms: u64,
    },
}
