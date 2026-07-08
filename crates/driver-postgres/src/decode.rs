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
        Type::NUMERIC => Value::Decimal(decode_numeric(raw)?),
        Type::INTERVAL => decode_interval(raw)?,
        _ => Value::Engine {
            engine: Engine::Postgres,
            type_name: ty.name().to_string(),
            display_text: format!("<undecoded {}>", ty.name()),
        },
    })
}

fn decode_numeric(raw: &[u8]) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    const SIGN_POS: u16 = 0x0000;
    const SIGN_NEG: u16 = 0x4000;
    const SIGN_NAN: u16 = 0xC000;

    if raw.len() < 8 || raw.len() % 2 != 0 {
        return Err("invalid numeric payload".into());
    }
    let ndigits = i16::from_be_bytes([raw[0], raw[1]]) as usize;
    let weight = i16::from_be_bytes([raw[2], raw[3]]);
    let sign = u16::from_be_bytes([raw[4], raw[5]]);
    let dscale = u16::from_be_bytes([raw[6], raw[7]]) as usize;
    if raw.len() != 8 + ndigits * 2 {
        return Err("numeric payload length mismatch".into());
    }
    if sign == SIGN_NAN {
        return Ok("NaN".to_string());
    }
    if sign != SIGN_POS && sign != SIGN_NEG {
        return Err("invalid numeric sign".into());
    }

    let groups: Vec<u16> = raw[8..]
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();
    if groups.iter().any(|group| *group >= 10_000) {
        return Err("invalid numeric digit group".into());
    }

    // Bound `weight` against `ndigits`. A hostile payload with e.g.
    // weight=32766 would otherwise allocate ~130 KiB of "0000" digits
    // before the trim step. PG never emits weight > ndigits - 1 for a
    // valid numeric; clamp to that.
    let int_group_count = {
        let raw = (i32::from(weight) + 1).max(0) as usize;
        // Leave some headroom: PG limits total digits to NUMERIC_MAX_PRECISION
        // (1000); at 4 digits per group that's 250 groups.
        const MAX_INT_GROUPS: usize = 512;
        raw.min(ndigits.saturating_add(MAX_INT_GROUPS)).min(MAX_INT_GROUPS)
    };
    let mut int_part = String::new();
    for idx in 0..int_group_count {
        let group = groups.get(idx).copied().unwrap_or(0);
        if idx == 0 {
            int_part.push_str(&group.to_string());
        } else {
            int_part.push_str(&format!("{group:04}"));
        }
    }
    let int_part = int_part.trim_start_matches('0');
    let mut out = if int_part.is_empty() {
        "0".to_string()
    } else {
        int_part.to_string()
    };

    let mut frac = String::new();
    if weight < 0 {
        // Same bound as the integer side: cap attacker-controlled zero
        // padding.
        let zero_groups = ((-i32::from(weight) - 1).max(0) as usize).min(512);
        for _ in 0..zero_groups {
            frac.push_str("0000");
        }
        for group in &groups {
            frac.push_str(&format!("{group:04}"));
        }
    } else {
        for group in groups.iter().skip(int_group_count) {
            frac.push_str(&format!("{group:04}"));
        }
    }
    if dscale > 0 {
        if frac.len() < dscale {
            frac.extend(std::iter::repeat('0').take(dscale - frac.len()));
        }
        frac.truncate(dscale);
        out.push('.');
        out.push_str(&frac);
    }
    if sign == SIGN_NEG && out != "0" && !out.starts_with('-') {
        out.insert(0, '-');
    }
    Ok(out)
}

fn decode_interval(raw: &[u8]) -> Result<Value, Box<dyn std::error::Error + Sync + Send>> {
    if raw.len() != 16 {
        return Err("invalid interval payload".into());
    }
    let micros = i64::from_be_bytes(raw[0..8].try_into()?);
    let days = i32::from_be_bytes(raw[8..12].try_into()?);
    let months = i32::from_be_bytes(raw[12..16].try_into()?);
    if months != 0 {
        return Ok(Value::Engine {
            engine: Engine::Postgres,
            type_name: "interval".to_string(),
            display_text: format!("{months} months {days} days {micros} microseconds"),
        });
    }
    let duration = chrono::Duration::days(i64::from(days))
        .checked_add(&chrono::Duration::microseconds(micros))
        .ok_or("interval duration overflow")?;
    Ok(Value::Interval(duration))
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
        Type::INTERVAL => Some(PrimitiveType::Interval),
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

    #[test]
    fn decodes_numeric_binary_payload() {
        let raw = [
            0x00, 0x03, // ndigits
            0x00, 0x01, // weight
            0x00, 0x00, // positive
            0x00, 0x02, // dscale
            0x00, 0x01, // 1
            0x09, 0x29, // 2345
            0x1A, 0x2C, // 6700
        ];
        assert_eq!(decode_numeric(&raw).unwrap(), "12345.67");

        let raw = [
            0x00, 0x01, // ndigits
            0xFF, 0xFF, // weight -1
            0x40, 0x00, // negative
            0x00, 0x04, // dscale
            0x00, 0x0C, // 12
        ];
        assert_eq!(decode_numeric(&raw).unwrap(), "-0.0012");
    }

    #[test]
    fn decodes_month_free_interval_payload() {
        let mut raw = [0_u8; 16];
        raw[0..8].copy_from_slice(&1_000_000_i64.to_be_bytes());
        raw[8..12].copy_from_slice(&2_i32.to_be_bytes());
        raw[12..16].copy_from_slice(&0_i32.to_be_bytes());
        assert_eq!(
            decode_interval(&raw).unwrap(),
            Value::Interval(chrono::Duration::days(2) + chrono::Duration::seconds(1))
        );
    }
}
