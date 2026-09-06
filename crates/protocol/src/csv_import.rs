use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CsvConflictPolicy {
    #[default]
    Abort,
    Skip,
    /// Continue and retain a row-number/error report for every rejected row.
    Quarantine,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CsvQuarantinedRow {
    pub row_number: u64,
    pub reason: String,
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
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub resume_from_row: u64,
    #[serde(default)]
    pub type_mappings: std::collections::BTreeMap<String, String>,
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
    /// Number of source data rows validated, including skipped conflicts.
    #[serde(default)]
    pub rows_validated: u64,
    /// First source row not covered by completed writes/skips. Dry runs retain
    /// the request cursor because validation does not import any rows.
    #[serde(default)]
    pub resume_from_row: u64,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quarantined_rows: Vec<CsvQuarantinedRow>,
}
