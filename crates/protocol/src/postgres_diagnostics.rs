use serde::{Deserialize, Serialize};

/// One read-only, bounded snapshot. Lag values are PostgreSQL's nullable
/// observations, not elapsed time measured by Sift.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresReplicationReport {
    pub senders: Vec<PostgresReplicationSender>,
    pub receiver: Option<PostgresWalReceiver>,
    pub slots: Vec<PostgresReplicationSlot>,
    pub senders_truncated: bool,
    pub slots_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresReplicationSender {
    pub pid: i64,
    pub application_name: String,
    pub state: String,
    pub sync_state: String,
    pub write_lag_ms: Option<i64>,
    pub flush_lag_ms: Option<i64>,
    pub replay_lag_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresWalReceiver {
    pub status: String,
    pub received_lsn: Option<String>,
    pub latest_end_lsn: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresReplicationSlot {
    pub name: String,
    pub slot_type: String,
    pub database: Option<String>,
    pub active: bool,
    pub restart_lsn: Option<String>,
    pub confirmed_flush_lsn: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresStatisticsQuery {
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Cumulative counters since PostgreSQL's statistics reset. No rate or cache
/// conclusion is inferred from one snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresStatisticsReport {
    pub database: PostgresDatabaseStatistics,
    pub tables: Vec<PostgresTableStatistics>,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresDatabaseStatistics {
    pub database: String,
    pub backends: i64,
    pub commits: i64,
    pub rollbacks: i64,
    pub blocks_read: i64,
    pub blocks_hit: i64,
    pub tuples_returned: i64,
    pub tuples_fetched: i64,
    pub tuples_inserted: i64,
    pub tuples_updated: i64,
    pub tuples_deleted: i64,
    pub stats_reset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresTableStatistics {
    pub schema: String,
    pub table: String,
    pub sequential_scans: i64,
    pub index_scans: Option<i64>,
    pub live_tuples_estimate: i64,
    pub dead_tuples_estimate: i64,
    pub last_vacuum: Option<String>,
    pub last_autovacuum: Option<String>,
    pub last_analyze: Option<String>,
    pub last_autoanalyze: Option<String>,
}
