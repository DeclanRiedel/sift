//! Decode helpers: PG type → [`sift_protocol::TypeRef`], PG row cell →
//! [`sift_protocol::Value`], and PG column → [`sift_protocol::ColumnMetadata`].
//!
//! `Value` lives in the `sift-protocol` crate and `FromSql` lives in
//! `tokio-postgres`; per orphan rules we can't `impl FromSql for Value`
//! directly. We wrap in a [`PgValue`] newtype for the trait impl and unwrap
//! at the call site.

use sift_protocol::{
    ColumnMetadata, Engine, Nullability, PrimitiveType, TypeCategory, TypeRef, Value,
};
use tokio_postgres::types::{FromSql, Kind, Type};
use tokio_postgres::{Column, SimpleQueryRow};

/// Newtype wrapper enabling `impl FromSql`. Unwrap via `.0` at the call site.
pub(crate) struct PgValue(pub Value);

impl<'a> FromSql<'a> for PgValue {
    fn from_sql(
        ty: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(PgValue(decode_value(ty, raw)?))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

/// Dispatch on PG type OID. Unknown types fall through to [`Value::Engine`]
/// with the native name + a placeholder display string; clients render them
/// as opaque text.
fn decode_value(ty: &Type, raw: &[u8]) -> Result<Value, Box<dyn std::error::Error + Sync + Send>> {
    Ok(match *ty {
        Type::BOOL => Value::Bool(bool::from_sql(ty, raw)?),
        Type::INT2 => Value::Int16(i16::from_sql(ty, raw)?),
        Type::INT4 => Value::Int32(i32::from_sql(ty, raw)?),
        Type::INT8 => Value::Int64(i64::from_sql(ty, raw)?),
        Type::FLOAT4 => Value::Float32(f32::from_sql(ty, raw)?),
        Type::FLOAT8 => Value::Float64(f64::from_sql(ty, raw)?),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => {
            Value::Text(String::from_sql(ty, raw)?)
        }
        Type::BYTEA => Value::Blob(Vec::from_sql(ty, raw)?),
        Type::UUID => Value::Uuid(uuid::Uuid::from_sql(ty, raw)?),
        Type::JSON | Type::JSONB => Value::Json(serde_json::Value::from_sql(ty, raw)?),
        Type::DATE => Value::Date(chrono::NaiveDate::from_sql(ty, raw)?),
        Type::TIME => Value::Time(chrono::NaiveTime::from_sql(ty, raw)?),
        Type::TIMESTAMP => Value::Timestamp(chrono::NaiveDateTime::from_sql(ty, raw)?),
        Type::TIMESTAMPTZ => {
            Value::TimestampTz(chrono::DateTime::<chrono::FixedOffset>::from_sql(ty, raw)?.into())
        }
        // Decimal/numeric: tokio-postgres's binary form needs the `bigdecimal`
        // feature to decode cleanly. For Phase 0 we surface the type name via
        // the Engine escape hatch; a typed Decimal lands with FEATURES.md
        // Tier 1 #15 (type-aware rendering).
        _ => Value::Engine {
            engine: Engine::Postgres,
            type_name: ty.name().to_string(),
            display_text: format!("<undecoded {}>", ty.name()),
        },
    })
}

/// Map a PG [`Type`] to our protocol-level [`TypeRef`]. Known primitives
/// collapse to [`TypeRef::Primitive`]; everything else is
/// [`TypeRef::Engine`] carrying the native name verbatim (no LCD flattening).
pub(crate) fn pg_type_to_type_ref(ty: &Type) -> TypeRef {
    let prim = match *ty {
        Type::BOOL => Some(PrimitiveType::Bool),
        Type::INT2 => Some(PrimitiveType::Int16),
        Type::INT4 => Some(PrimitiveType::Int32),
        Type::INT8 => Some(PrimitiveType::Int64),
        Type::FLOAT4 => Some(PrimitiveType::Float32),
        Type::FLOAT8 => Some(PrimitiveType::Float64),
        Type::NUMERIC => Some(PrimitiveType::Decimal),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => Some(PrimitiveType::Text),
        Type::BYTEA => Some(PrimitiveType::Blob),
        Type::DATE => Some(PrimitiveType::Date),
        Type::TIME => Some(PrimitiveType::Time),
        Type::TIMESTAMP => Some(PrimitiveType::Timestamp),
        Type::TIMESTAMPTZ => Some(PrimitiveType::TimestampTz),
        Type::UUID => Some(PrimitiveType::Uuid),
        Type::JSON => Some(PrimitiveType::Json),
        Type::JSONB => Some(PrimitiveType::Jsonb),
        _ => None,
    };
    prim.map(TypeRef::Primitive)
        .unwrap_or_else(|| TypeRef::Engine {
            engine: Engine::Postgres,
            name: ty.name().to_string(),
            category: pg_type_category(ty),
        })
}

fn pg_type_category(ty: &Type) -> TypeCategory {
    // PG's type metadata carries a `Kind` we can inspect — Array, Enum,
    // Composite, Range, Domain, Pseudo. The kind enum is non-exhaustive
    // (upstream may add variants); wildcard falls back to the scalar match.
    match ty.kind() {
        Kind::Array(_) => TypeCategory::Array,
        Kind::Enum(_) => TypeCategory::Enum,
        Kind::Composite(_) => TypeCategory::Composite,
        Kind::Range(_) => TypeCategory::Range,
        Kind::Domain(_) | Kind::Pseudo => match *ty {
            Type::BOOL => TypeCategory::Boolean,
            Type::INT2 | Type::INT4 | Type::INT8 | Type::FLOAT4 | Type::FLOAT8 | Type::NUMERIC => {
                TypeCategory::Numeric
            }
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => TypeCategory::Text,
            Type::BYTEA => TypeCategory::Binary,
            Type::DATE | Type::TIME | Type::TIMESTAMP | Type::TIMESTAMPTZ | Type::INTERVAL => {
                TypeCategory::Temporal
            }
            Type::UUID => TypeCategory::Uuid,
            Type::JSON | Type::JSONB => TypeCategory::Json,
            Type::XML => TypeCategory::Xml,
            _ => TypeCategory::Other,
        },
        // Future Kind variants land here.
        _ => TypeCategory::Other,
    }
}

/// Build [`ColumnMetadata`] from a PG [`Column`]. Facets default to PG-only
/// and minimal; richer facets (OID, enum values) land with the Deep schema
/// pass (FEATURES.md Tier 1 #11 autocomplete).
pub(crate) fn col_to_metadata(col: &Column) -> ColumnMetadata {
    ColumnMetadata {
        name: col.name().to_string(),
        type_ref: pg_type_to_type_ref(col.type_()),
        nullable: Nullability::Unknown,
        auto_increment: false,
        primary_key: false,
        facets: Default::default(),
    }
}

/// Build column metadata from a `simple_query` row. tokio-postgres 0.7.18's
/// `SimpleColumn` carries only the name (no type OID), so we default the
/// type to Text — clients that need richer typing should re-issue via the
/// extended-query path or via the Deep schema pass.
pub(crate) fn simple_query_columns(row: &SimpleQueryRow) -> Vec<ColumnMetadata> {
    row.columns()
        .iter()
        .map(|c| ColumnMetadata {
            name: c.name().to_string(),
            type_ref: TypeRef::Primitive(PrimitiveType::Text),
            nullable: Nullability::Unknown,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::PrimitiveType;

    #[test]
    fn known_types_collapse_to_primitive() {
        assert_eq!(
            pg_type_to_type_ref(&Type::INT4),
            TypeRef::Primitive(PrimitiveType::Int32)
        );
        assert_eq!(
            pg_type_to_type_ref(&Type::TEXT),
            TypeRef::Primitive(PrimitiveType::Text)
        );
        assert_eq!(
            pg_type_to_type_ref(&Type::TIMESTAMPTZ),
            TypeRef::Primitive(PrimitiveType::TimestampTz)
        );
        assert_eq!(
            pg_type_to_type_ref(&Type::JSONB),
            TypeRef::Primitive(PrimitiveType::Jsonb)
        );
    }

    #[test]
    fn unknown_types_carry_native_name_verbatim() {
        // MONEY exists in tokio-postgres core but isn't in our Primitive
        // enum, so it falls through to the Engine escape hatch.
        let r = pg_type_to_type_ref(&Type::MONEY);
        match r {
            TypeRef::Engine {
                engine,
                name,
                category,
            } => {
                assert_eq!(engine, Engine::Postgres);
                assert_eq!(name, "money");
                assert_eq!(category, TypeCategory::Other);
            }
            other => panic!("expected Engine variant, got {other:?}"),
        }
    }

    #[test]
    fn array_types_categorized_as_array() {
        // INT4_ARRAY exists in tokio-postgres core. It maps via the Engine
        // path (no Primitive variant for arrays); category must be Array.
        let r = pg_type_to_type_ref(&Type::INT4_ARRAY);
        match r {
            TypeRef::Engine {
                engine,
                name,
                category,
            } => {
                assert_eq!(engine, Engine::Postgres);
                assert_eq!(category, TypeCategory::Array);
                assert_eq!(name, "_int4"); // PG canonical array type name
            }
            TypeRef::Primitive(_) => panic!("arrays should not map to Primitive"),
        }
    }
}
