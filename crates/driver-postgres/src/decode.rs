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

/// Dispatch on PG type OID. Unknown types fall through to [`Value::Native`]
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
        Type::XML => native_value(ty, decode_utf8(raw)?),
        Type::JSONPATH => {
            let (&version, text) = raw.split_first().ok_or("invalid jsonpath payload")?;
            if version != 1 {
                return Err(format!("unsupported jsonpath version {version}").into());
            }
            native_value(ty, decode_utf8(text)?)
        }
        Type::INET | Type::CIDR => native_value(ty, decode_network(raw)?),
        Type::MACADDR | Type::MACADDR8 => native_value(ty, decode_mac(raw)?),
        Type::MONEY => native_value(ty, decode_money(raw)?),
        Type::TIMETZ => native_value(ty, decode_timetz(raw)?),
        _ if matches!(ty.kind(), Kind::Array(_)) => decode_array(ty, raw)?,
        _ if matches!(ty.kind(), Kind::Range(_)) => decode_range(ty, raw)?,
        _ => Value::Native {
            provider_id: Engine::Postgres.provider_id(),
            type_name: ty.name().to_string(),
            display_text: format!("<undecoded {}>", ty.name()),
        },
    })
}

fn native_value(ty: &Type, display_text: String) -> Value {
    Value::Native {
        provider_id: Engine::Postgres.provider_id(),
        type_name: ty.name().to_string(),
        display_text,
    }
}

fn decode_utf8(raw: &[u8]) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    Ok(std::str::from_utf8(raw)?.to_owned())
}

fn decode_mac(raw: &[u8]) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    if !matches!(raw.len(), 6 | 8) {
        return Err("invalid macaddr payload".into());
    }
    Ok(raw
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":"))
}

fn decode_money(raw: &[u8]) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    let units = i64::from_be_bytes(raw.try_into().map_err(|_| "invalid money payload")?);
    Ok(format!("{units} minor units"))
}

fn decode_network(raw: &[u8]) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    if raw.len() < 4 {
        return Err("invalid network payload".into());
    }
    let family = raw[0];
    let bits = raw[1];
    let is_cidr = raw[2] != 0;
    let length = usize::from(raw[3]);
    if raw.len() != 4 + length {
        return Err("network payload length mismatch".into());
    }
    let (address, full_bits) = match (family, length) {
        (2, 4) => (
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(raw[4], raw[5], raw[6], raw[7])),
            32,
        ),
        (3, 16) => {
            let octets: [u8; 16] = raw[4..].try_into()?;
            (std::net::IpAddr::V6(std::net::Ipv6Addr::from(octets)), 128)
        }
        _ => return Err("unsupported network address family".into()),
    };
    if bits > full_bits {
        return Err("invalid network prefix length".into());
    }
    if is_cidr || bits != full_bits {
        Ok(format!("{address}/{bits}"))
    } else {
        Ok(address.to_string())
    }
}

fn decode_timetz(raw: &[u8]) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    if raw.len() != 12 {
        return Err("invalid timetz payload".into());
    }
    let micros = i64::from_be_bytes(raw[..8].try_into()?);
    let seconds_west = i32::from_be_bytes(raw[8..].try_into()?);
    let seconds = u32::try_from(micros.div_euclid(1_000_000))?;
    let nanos = u32::try_from(micros.rem_euclid(1_000_000))? * 1_000;
    let time = chrono::NaiveTime::from_num_seconds_from_midnight_opt(seconds, nanos)
        .ok_or("timetz time outside one day")?;
    let offset =
        chrono::FixedOffset::east_opt(seconds_west.checked_neg().ok_or("invalid timetz offset")?)
            .ok_or("invalid timetz offset")?;
    Ok(format!("{}{}", time.format("%H:%M:%S%.f"), offset))
}

fn decode_array(ty: &Type, raw: &[u8]) -> Result<Value, Box<dyn std::error::Error + Sync + Send>> {
    let Kind::Array(member) = ty.kind() else {
        return Err("array type metadata missing element type".into());
    };
    let mut cursor = BinaryCursor::new(raw);
    let dimensions = usize::try_from(cursor.i32()?).map_err(|_| "negative array dimensions")?;
    let _has_null = cursor.i32()?;
    let _element_oid = cursor.u32()?;
    if dimensions > 8 {
        return Err("array has too many dimensions".into());
    }
    let mut lengths = Vec::with_capacity(dimensions);
    let mut total = 1usize;
    for _ in 0..dimensions {
        let length = usize::try_from(cursor.i32()?).map_err(|_| "negative array length")?;
        let _lower_bound = cursor.i32()?;
        total = total.checked_mul(length).ok_or("array size overflow")?;
        if total > 1_000_000 {
            return Err("array is too large to decode".into());
        }
        lengths.push(length);
    }
    let mut values = Vec::with_capacity(total);
    for _ in 0..total {
        let length = cursor.i32()?;
        if length == -1 {
            values.push("NULL".to_string());
        } else {
            let bytes =
                cursor.bytes(usize::try_from(length).map_err(|_| "invalid array value length")?)?;
            values.push(value_text(&decode_value(member, bytes)?));
        }
    }
    cursor.finish()?;
    let mut index = 0;
    let display = format_array_dimension(&values, &lengths, 0, &mut index);
    Ok(native_value(ty, display))
}

fn format_array_dimension(
    values: &[String],
    lengths: &[usize],
    depth: usize,
    index: &mut usize,
) -> String {
    if depth == lengths.len() {
        let value = values.get(*index).cloned().unwrap_or_default();
        *index += 1;
        return value;
    }
    let parts = (0..lengths[depth])
        .map(|_| format_array_dimension(values, lengths, depth + 1, index))
        .collect::<Vec<_>>();
    format!("{{{}}}", parts.join(","))
}

fn decode_range(ty: &Type, raw: &[u8]) -> Result<Value, Box<dyn std::error::Error + Sync + Send>> {
    let Kind::Range(member) = ty.kind() else {
        return Err("range type metadata missing element type".into());
    };
    let mut cursor = BinaryCursor::new(raw);
    let flags = cursor.u8()?;
    if flags & 0x01 != 0 {
        cursor.finish()?;
        return Ok(native_value(ty, "empty".into()));
    }
    let lower = if flags & 0x08 != 0 {
        None
    } else {
        Some(decode_range_bound(member, &mut cursor)?)
    };
    let upper = if flags & 0x10 != 0 {
        None
    } else {
        Some(decode_range_bound(member, &mut cursor)?)
    };
    cursor.finish()?;
    let open = if flags & 0x02 != 0 { '[' } else { '(' };
    let close = if flags & 0x04 != 0 { ']' } else { ')' };
    Ok(native_value(
        ty,
        format!(
            "{open}{},{upper}{close}",
            lower.unwrap_or_default(),
            upper = upper.unwrap_or_default()
        ),
    ))
}

fn decode_range_bound(
    member: &Type,
    cursor: &mut BinaryCursor<'_>,
) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    let length = usize::try_from(cursor.i32()?).map_err(|_| "invalid range bound length")?;
    Ok(value_text(&decode_value(member, cursor.bytes(length)?)?))
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Null | Value::TypedNull { .. } => "NULL".into(),
        Value::Bool(value) => value.to_string(),
        Value::Int16(value) => value.to_string(),
        Value::Int32(value) => value.to_string(),
        Value::Int64(value) => value.to_string(),
        Value::Float32(value) => value.to_string(),
        Value::Float64(value) => value.to_string(),
        Value::Decimal(value) | Value::Text(value) => value.clone(),
        Value::Blob(value) => format!("<{} bytes>", value.len()),
        Value::Date(value) => value.to_string(),
        Value::Time(value) => value.to_string(),
        Value::Timestamp(value) => value.to_string(),
        Value::TimestampTz(value) => value.to_rfc3339(),
        Value::Interval(value) => format!("{} ms", value.num_milliseconds()),
        Value::Uuid(value) => value.to_string(),
        Value::Json(value) => value.to_string(),
        Value::Native { display_text, .. } => display_text.clone(),
    }
}

struct BinaryCursor<'a> {
    raw: &'a [u8],
    offset: usize,
}

impl<'a> BinaryCursor<'a> {
    fn new(raw: &'a [u8]) -> Self {
        Self { raw, offset: 0 }
    }
    fn bytes(
        &mut self,
        length: usize,
    ) -> Result<&'a [u8], Box<dyn std::error::Error + Sync + Send>> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or("binary payload overflow")?;
        let bytes = self
            .raw
            .get(self.offset..end)
            .ok_or("truncated binary payload")?;
        self.offset = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8, Box<dyn std::error::Error + Sync + Send>> {
        Ok(self.bytes(1)?[0])
    }
    fn i32(&mut self) -> Result<i32, Box<dyn std::error::Error + Sync + Send>> {
        Ok(i32::from_be_bytes(self.bytes(4)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32, Box<dyn std::error::Error + Sync + Send>> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into()?))
    }
    fn finish(self) -> Result<(), Box<dyn std::error::Error + Sync + Send>> {
        if self.offset == self.raw.len() {
            Ok(())
        } else {
            Err("trailing binary payload bytes".into())
        }
    }
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

    for idx in 0..ndigits {
        if numeric_group(raw, idx) >= 10_000 {
            return Err("invalid numeric digit group".into());
        }
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
        raw.min(ndigits.saturating_add(MAX_INT_GROUPS))
            .min(MAX_INT_GROUPS)
    };
    let mut out = String::with_capacity(
        usize::from(sign == SIGN_NEG)
            + int_group_count.saturating_mul(4).max(1)
            + usize::from(dscale > 0)
            + dscale,
    );
    if sign == SIGN_NEG {
        out.push('-');
    }
    let mut wrote_int = false;
    for idx in 0..int_group_count {
        let group = if idx < ndigits {
            numeric_group(raw, idx)
        } else {
            0
        };
        if !wrote_int {
            if group == 0 && idx + 1 < int_group_count {
                continue;
            }
            append_group_unpadded(&mut out, group);
            wrote_int = true;
        } else {
            append_group_padded(&mut out, group);
        }
    }
    if !wrote_int {
        out.push('0');
    }

    let mut frac = String::with_capacity(dscale.max(4));
    if weight < 0 {
        // Same bound as the integer side: cap attacker-controlled zero
        // padding.
        let zero_groups = ((-i32::from(weight) - 1).max(0) as usize).min(512);
        for _ in 0..zero_groups {
            frac.push_str("0000");
        }
        for idx in 0..ndigits {
            append_group_padded(&mut frac, numeric_group(raw, idx));
        }
    } else {
        for idx in int_group_count..ndigits {
            append_group_padded(&mut frac, numeric_group(raw, idx));
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
    if out == "-0" {
        out.remove(0);
    }
    Ok(out)
}

fn numeric_group(raw: &[u8], idx: usize) -> u16 {
    let offset = 8 + idx * 2;
    u16::from_be_bytes([raw[offset], raw[offset + 1]])
}

fn append_group_unpadded(out: &mut String, group: u16) {
    use std::fmt::Write as _;
    let _ = write!(out, "{group}");
}

fn append_group_padded(out: &mut String, group: u16) {
    out.push(char::from(b'0' + ((group / 1000) % 10) as u8));
    out.push(char::from(b'0' + ((group / 100) % 10) as u8));
    out.push(char::from(b'0' + ((group / 10) % 10) as u8));
    out.push(char::from(b'0' + (group % 10) as u8));
}

fn decode_interval(raw: &[u8]) -> Result<Value, Box<dyn std::error::Error + Sync + Send>> {
    if raw.len() != 16 {
        return Err("invalid interval payload".into());
    }
    let micros = i64::from_be_bytes(raw[0..8].try_into()?);
    let days = i32::from_be_bytes(raw[8..12].try_into()?);
    let months = i32::from_be_bytes(raw[12..16].try_into()?);
    if months != 0 {
        return Ok(Value::Native {
            provider_id: Engine::Postgres.provider_id(),
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
/// [`TypeRef::Native`] carrying the native name verbatim (no LCD flattening).
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
        .unwrap_or_else(|| TypeRef::Native {
            provider_id: Engine::Postgres.provider_id(),
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
        // enum, so it falls through to the provider-native escape hatch.
        let r = pg_type_to_type_ref(&Type::MONEY);
        match r {
            TypeRef::Native {
                provider_id,
                name,
                category,
            } => {
                assert_eq!(provider_id, Engine::Postgres.provider_id());
                assert_eq!(name, "money");
                assert_eq!(category, TypeCategory::Other);
            }
            other => panic!("expected Native variant, got {other:?}"),
        }
    }

    #[test]
    fn array_types_categorized_as_array() {
        // INT4_ARRAY exists in tokio-postgres core. It maps via the Native
        // path (no Primitive variant for arrays); category must be Array.
        let r = pg_type_to_type_ref(&Type::INT4_ARRAY);
        match r {
            TypeRef::Native {
                provider_id,
                name,
                category,
            } => {
                assert_eq!(provider_id, Engine::Postgres.provider_id());
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

    fn native_text(value: Value) -> String {
        match value {
            Value::Native { display_text, .. } => display_text,
            other => panic!("expected native value, got {other:?}"),
        }
    }

    #[test]
    fn decodes_network_mac_money_and_timetz_payloads() {
        assert_eq!(
            native_text(decode_value(&Type::CIDR, &[2, 24, 1, 4, 192, 168, 1, 0]).unwrap()),
            "192.168.1.0/24"
        );
        assert_eq!(
            native_text(
                decode_value(&Type::MACADDR, &[0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]).unwrap()
            ),
            "00:1a:2b:3c:4d:5e"
        );
        assert_eq!(
            native_text(decode_value(&Type::MONEY, &1234_i64.to_be_bytes()).unwrap()),
            "1234 minor units"
        );
        let mut timetz = Vec::new();
        timetz.extend_from_slice(&(45_296_123_456_i64).to_be_bytes());
        timetz.extend_from_slice(&(-7_200_i32).to_be_bytes());
        assert_eq!(
            native_text(decode_value(&Type::TIMETZ, &timetz).unwrap()),
            "12:34:56.123456+02:00"
        );
    }

    #[test]
    fn decodes_jsonpath_array_and_range_payloads() {
        assert_eq!(
            native_text(decode_value(&Type::JSONPATH, b"\x01$.account.id").unwrap()),
            "$.account.id"
        );

        let mut array = Vec::new();
        array.extend_from_slice(&1_i32.to_be_bytes());
        array.extend_from_slice(&0_i32.to_be_bytes());
        array.extend_from_slice(&23_u32.to_be_bytes());
        array.extend_from_slice(&2_i32.to_be_bytes());
        array.extend_from_slice(&1_i32.to_be_bytes());
        for value in [7_i32, 9_i32] {
            array.extend_from_slice(&4_i32.to_be_bytes());
            array.extend_from_slice(&value.to_be_bytes());
        }
        assert_eq!(
            native_text(decode_value(&Type::INT4_ARRAY, &array).unwrap()),
            "{7,9}"
        );

        let mut range = vec![0x02];
        range.extend_from_slice(&4_i32.to_be_bytes());
        range.extend_from_slice(&1_i32.to_be_bytes());
        range.extend_from_slice(&4_i32.to_be_bytes());
        range.extend_from_slice(&5_i32.to_be_bytes());
        assert_eq!(
            native_text(decode_value(&Type::INT4_RANGE, &range).unwrap()),
            "[1,5)"
        );
    }
}
