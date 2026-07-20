use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CsvConflictPolicy {
    #[default]
    Abort,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CsvImportRequest {
    pub table: String,
    #[schemars(with = "Vec<u8>")]
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    #[serde(default = "default_true")]
    pub header: bool,
    #[serde(default = "default_delimiter")]
    pub delimiter: char,
    #[serde(default = "default_null_value")]
    pub null_value: Option<String>,
    #[serde(default)]
    pub create_table: bool,
    #[serde(default)]
    pub conflict_policy: CsvConflictPolicy,
}

fn default_true() -> bool {
    true
}

fn default_delimiter() -> char {
    ','
}

fn default_null_value() -> Option<String> {
    Some("NULL".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InferredCsvType {
    Boolean,
    Int64,
    Decimal,
    Date,
    TimestampTz,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InferredCsvColumn {
    pub name: String,
    pub inferred_type: InferredCsvType,
    pub nullable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CsvImportResponse {
    pub table: String,
    pub columns: Vec<InferredCsvColumn>,
    pub table_created: bool,
    pub rows_inserted: u64,
    pub rows_skipped: u64,
}
