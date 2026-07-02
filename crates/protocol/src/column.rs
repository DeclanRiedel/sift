//! Column metadata + the type-system escape hatch.
//!
//! [`TypeRef`] is the design point that avoids JDBC's lowest-common-
//! denominator trap on types: rather than flattening every native type
//! into one enumeration, it carries either a [`PrimitiveType`] (the IDE's
//! own render vocab) or the engine-native name verbatim with a category
//! hint for renderer dispatch.

use crate::Engine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnMetadata {
    pub name: String,
    pub type_ref: TypeRef,
    pub nullable: Nullability,
    pub auto_increment: bool,
    pub primary_key: bool,
    /// Engine-specific facets. At most one is `Some` for any given column;
    /// the active one matches the connection's engine.
    #[serde(default)]
    pub facets: EngineColumnFacets,
}

impl ColumnMetadata {
    pub fn new(name: impl Into<String>, type_ref: TypeRef) -> Self {
        Self {
            name: name.into(),
            type_ref,
            nullable: Nullability::Unknown,
            auto_increment: false,
            primary_key: false,
            facets: EngineColumnFacets::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Nullability {
    Nullable,
    NotNullable,
    Unknown,
}

/// The IDE's union type vocabulary.
///
/// - `Primitive` = a well-known type the IDE renders natively across
///   engines. The enum is small by design; it does not try to capture every
///   engine's native types.
/// - `Engine` = escape hatch for types that don't map cleanly: `varchar(max)`,
///   `tsvector`, `sql_variant`, `xml`, `hstore`, `jsonb`, `citext`, etc.
///   The native name is carried verbatim; the client renders it as opaque
///   text with the engine as a hint, using `category` to pick a renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TypeRef {
    Primitive(PrimitiveType),
    Engine {
        engine: Engine,
        name: String,
        category: TypeCategory,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveType {
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    Decimal,
    Bool,
    Text,
    Blob,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
    Uuid,
    Json,
    Jsonb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeCategory {
    Numeric,
    Text,
    Binary,
    Temporal,
    Boolean,
    Uuid,
    Json,
    Composite,
    Enum,
    Array,
    Range,
    Geometric,
    BitString,
    NetworkAddress,
    Xml,
    Other,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineColumnFacets {
    #[serde(default)]
    pub postgres: Option<PgColumnFacets>,
    #[serde(default)]
    pub sql_server: Option<MssqlColumnFacets>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgColumnFacets {
    /// PG type OID (e.g. `int4` = 23). Useful for clients that want to
    /// dispatch on the engine's native classification.
    pub oid: Option<u32>,
    /// Array dimensionality; 0 = scalar.
    pub array_dims: u8,
    /// True if this column is a SQL identity column (`GENERATED ... AS IDENTITY`).
    pub is_identity: bool,
    /// Enum values, if the column type is an enum.
    pub enum_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MssqlColumnFacets {
    /// TDS column type name (e.g. `SQLVARCHAR`, `SQLBIT`).
    pub tds_type: Option<String>,
    /// Collation name, for textual columns.
    pub collation: Option<String>,
    /// Declared max length, in bytes for binary types, chars for text types.
    pub max_length: Option<u32>,
}
