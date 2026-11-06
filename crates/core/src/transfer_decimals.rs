//! Exact bounded decimal coefficients for portable transfer expressions.
use num_bigint::{BigInt, Sign};
use sift_protocol::Value;

const INVALID: &str = "decimal expression requires bounded base-10 text or an integer";
const OVERFLOW: &str = "decimal expression exceeds 38 digits or scale 18";

pub(crate) struct Decimal {
    coefficient: BigInt,
    scale: u8,
}
fn power(scale: u8) -> BigInt {
    BigInt::from(10u8).pow(u32::from(scale))
}
fn magnitude(value: &BigInt) -> BigInt {
    if value.sign() == Sign::Minus {
        -value
    } else {
        value.clone()
    }
}
fn rounded(numerator: BigInt, denominator: BigInt) -> BigInt {
    let mut quotient = &numerator / &denominator;
    let remainder = &numerator % &denominator;
    if magnitude(&remainder) * 2u8 >= magnitude(&denominator) {
        if numerator.sign() == denominator.sign() {
            quotient += 1u8;
        } else {
            quotient -= 1u8;
        }
    }
    quotient
}
impl Decimal {
    fn normalized(mut coefficient: BigInt, mut scale: u8) -> Result<Self, &'static str> {
        if coefficient == BigInt::from(0u8) {
            scale = 0;
        }
        while scale > 0 && &coefficient % 10u8 == BigInt::from(0u8) {
            coefficient /= 10u8;
            scale -= 1;
        }
        if scale > 18 || coefficient.to_str_radix(10).trim_start_matches('-').len() > 38 {
            return Err(OVERFLOW);
        }
        Ok(Self { coefficient, scale })
    }
    pub(crate) fn parse(value: Value) -> Result<Self, &'static str> {
        let text = match value {
            Value::Text(text) => text,
            Value::Int16(value) => value.to_string(),
            Value::Int32(value) => value.to_string(),
            Value::Int64(value) => value.to_string(),
            _ => return Err(INVALID),
        };
        if text.len() > 128 {
            return Err(INVALID);
        }
        let unsigned = text.strip_prefix(['-', '+']).unwrap_or(&text);
        let (whole, fraction) = match unsigned.split_once('.') {
            Some((whole, fraction)) if !fraction.is_empty() => (whole, fraction),
            Some(_) => return Err(INVALID),
            None => (unsigned, ""),
        };
        if whole.is_empty()
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|ch| ch.is_ascii_digit())
        {
            return Err(INVALID);
        }
        let fraction = fraction.trim_end_matches('0');
        let digits = format!("{}{fraction}", whole.trim_start_matches('0'));
        let digits = digits.trim_start_matches('0');
        if digits.is_empty() {
            return Self::normalized(BigInt::from(0u8), 0);
        }
        if digits.len() > 38 || fraction.len() > 18 {
            return Err(OVERFLOW);
        }
        let coefficient = BigInt::parse_bytes(digits.as_bytes(), 10).ok_or(INVALID)?;
        Self::normalized(
            if text.starts_with('-') {
                -coefficient
            } else {
                coefficient
            },
            fraction.len() as u8,
        )
    }
    pub(crate) fn negate(mut self) -> Self {
        self.coefficient = -self.coefficient;
        self
    }
    pub(crate) fn abs(mut self) -> Self {
        self.coefficient = magnitude(&self.coefficient);
        self
    }
    pub(crate) fn add(self, right: Self) -> Result<Self, &'static str> {
        let scale = self.scale.max(right.scale);
        Self::normalized(
            self.coefficient * power(scale - self.scale)
                + right.coefficient * power(scale - right.scale),
            scale,
        )
    }
    pub(crate) fn multiply(self, right: Self) -> Result<Self, &'static str> {
        Self::normalized(
            self.coefficient * right.coefficient,
            self.scale + right.scale,
        )
    }
    pub(crate) fn divide(self, right: Self, scale: u8) -> Result<Self, &'static str> {
        if scale > 18 {
            return Err(OVERFLOW);
        }
        if right.coefficient == BigInt::from(0u8) {
            return Err("decimal expression division by zero");
        }
        let shift = i16::from(scale) + i16::from(right.scale) - i16::from(self.scale);
        let (numerator, denominator) = if shift >= 0 {
            (self.coefficient * power(shift as u8), right.coefficient)
        } else {
            (self.coefficient, right.coefficient * power((-shift) as u8))
        };
        Self::normalized(rounded(numerator, denominator), scale)
    }
    pub(crate) fn round(self, scale: u8) -> Result<Self, &'static str> {
        if scale > 18 {
            return Err(OVERFLOW);
        }
        if scale >= self.scale {
            return Ok(self);
        }
        Self::normalized(rounded(self.coefficient, power(self.scale - scale)), scale)
    }
    pub(crate) fn render(self) -> String {
        let raw = self.coefficient.to_str_radix(10);
        if self.scale == 0 {
            return raw;
        }
        let negative = raw.starts_with('-');
        let digits = raw.trim_start_matches('-');
        let digits = format!("{:0>width$}", digits, width = usize::from(self.scale) + 1);
        let split = digits.len() - usize::from(self.scale);
        format!(
            "{}{}.{}",
            if negative { "-" } else { "" },
            &digits[..split],
            &digits[split..]
        )
    }
}
