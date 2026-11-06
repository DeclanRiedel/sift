//! Bounded, deterministic text expressions for tabular uploads. No SQL or I/O.
use sift_protocol::Value;
use std::collections::BTreeSet;

pub const MAX_CELL_BYTES: usize = 1024 * 1024;
const INVALID: &str = "invalid or excessive transfer expression";

#[derive(Clone, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expression {
    Decimal {
        input: Box<Self>,
    },
    Negate {
        input: Box<Self>,
    },
    Abs {
        input: Box<Self>,
    },
    Round {
        input: Box<Self>,
        scale: u8,
    },
    Add {
        left: Box<Self>,
        right: Box<Self>,
    },
    Subtract {
        left: Box<Self>,
        right: Box<Self>,
    },
    Multiply {
        left: Box<Self>,
        right: Box<Self>,
    },
    Divide {
        left: Box<Self>,
        right: Box<Self>,
        scale: u8,
    },
    Column {
        source: String,
    },
    Literal {
        value: String,
    },
    Null {},
    Text {
        input: Box<Self>,
    },
    Trim {
        input: Box<Self>,
    },
    Upper {
        input: Box<Self>,
    },
    Lower {
        input: Box<Self>,
    },
    Concat {
        inputs: Vec<Self>,
    },
    Coalesce {
        inputs: Vec<Self>,
    },
    Replace {
        input: Box<Self>,
        from: String,
        to: String,
    },
    Substring {
        input: Box<Self>,
        start: usize,
        length: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Decimal(Box<Self>),
    Negate(Box<Self>),
    Abs(Box<Self>),
    Round(Box<Self>, u8),
    Add(Box<Self>, Box<Self>),
    Subtract(Box<Self>, Box<Self>),
    Multiply(Box<Self>, Box<Self>),
    Divide(Box<Self>, Box<Self>, u8),
    Column(usize),
    Literal(String),
    Null,
    Text(Box<Self>),
    Trim(Box<Self>),
    Upper(Box<Self>),
    Lower(Box<Self>),
    Concat(Vec<Self>),
    Coalesce(Vec<Self>),
    Replace(Box<Self>, String, String),
    Substring(Box<Self>, usize, usize),
}

impl Expression {
    pub fn validate(&self) -> Result<usize, &'static str> {
        fn visit(value: &Expression, depth: usize, nodes: &mut usize) -> Result<(), &'static str> {
            *nodes += 1;
            if depth > 12 || *nodes > 128 {
                return Err(INVALID);
            }
            match value {
                Expression::Column { source } if source.len() > 4096 => Err(INVALID),
                Expression::Literal { value } if value.len() > 4096 => Err(INVALID),
                Expression::Column { .. } | Expression::Literal { .. } | Expression::Null {} => {
                    Ok(())
                }
                Expression::Decimal { input }
                | Expression::Negate { input }
                | Expression::Abs { input }
                | Expression::Text { input }
                | Expression::Trim { input }
                | Expression::Upper { input }
                | Expression::Lower { input } => visit(input, depth + 1, nodes),
                Expression::Round { input, scale } => {
                    if *scale > 18 {
                        return Err(INVALID);
                    }
                    visit(input, depth + 1, nodes)
                }
                Expression::Add { left, right }
                | Expression::Subtract { left, right }
                | Expression::Multiply { left, right }
                | Expression::Divide { left, right, .. } => {
                    if matches!(value, Expression::Divide { scale, .. } if *scale > 18) {
                        return Err(INVALID);
                    }
                    visit(left, depth + 1, nodes)?;
                    visit(right, depth + 1, nodes)
                }
                Expression::Concat { inputs } | Expression::Coalesce { inputs } => {
                    if inputs.is_empty() || inputs.len() > 32 {
                        return Err(INVALID);
                    }
                    for input in inputs {
                        visit(input, depth + 1, nodes)?;
                    }
                    Ok(())
                }
                Expression::Replace { input, from, to } => {
                    if from.is_empty() || from.len() > 4096 || to.len() > 4096 {
                        return Err(INVALID);
                    }
                    visit(input, depth + 1, nodes)
                }
                Expression::Substring {
                    input,
                    start,
                    length,
                } => {
                    if *start > MAX_CELL_BYTES || *length > MAX_CELL_BYTES {
                        return Err(INVALID);
                    }
                    visit(input, depth + 1, nodes)
                }
            }
        }
        let mut nodes = 0;
        visit(self, 1, &mut nodes)?;
        Ok(nodes)
    }

    pub fn resolve(&self, headers: &[String]) -> Result<Resolved, &'static str> {
        let resolve = |input: &Expression| input.resolve(headers).map(Box::new);
        Ok(match self {
            Self::Decimal { input } => Resolved::Decimal(resolve(input)?),
            Self::Negate { input } => Resolved::Negate(resolve(input)?),
            Self::Abs { input } => Resolved::Abs(resolve(input)?),
            Self::Round { input, scale } => Resolved::Round(resolve(input)?, *scale),
            Self::Add { left, right } => Resolved::Add(resolve(left)?, resolve(right)?),
            Self::Subtract { left, right } => Resolved::Subtract(resolve(left)?, resolve(right)?),
            Self::Multiply { left, right } => Resolved::Multiply(resolve(left)?, resolve(right)?),
            Self::Divide { left, right, scale } => {
                Resolved::Divide(resolve(left)?, resolve(right)?, *scale)
            }
            Self::Column { source } => {
                let mut matches = headers
                    .iter()
                    .enumerate()
                    .filter(|(_, name)| *name == source);
                let index = matches
                    .next()
                    .ok_or("expression references an unknown source")?
                    .0;
                if matches.next().is_some() {
                    return Err("expression source header is ambiguous");
                }
                Resolved::Column(index)
            }
            Self::Literal { value } => Resolved::Literal(value.clone()),
            Self::Null {} => Resolved::Null,
            Self::Text { input } => Resolved::Text(resolve(input)?),
            Self::Trim { input } => Resolved::Trim(resolve(input)?),
            Self::Upper { input } => Resolved::Upper(resolve(input)?),
            Self::Lower { input } => Resolved::Lower(resolve(input)?),
            Self::Concat { inputs } => Resolved::Concat(
                inputs
                    .iter()
                    .map(|input| input.resolve(headers))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Coalesce { inputs } => Resolved::Coalesce(
                inputs
                    .iter()
                    .map(|input| input.resolve(headers))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Replace { input, from, to } => {
                Resolved::Replace(resolve(input)?, from.clone(), to.clone())
            }
            Self::Substring {
                input,
                start,
                length,
            } => Resolved::Substring(resolve(input)?, *start, *length),
        })
    }
}

pub struct Budget {
    nodes: usize,
    bytes: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            nodes: 10_000_000,
            bytes: 256 * 1024 * 1024,
        }
    }
}
impl Budget {
    fn step(&mut self) -> Result<(), &'static str> {
        self.nodes = self
            .nodes
            .checked_sub(1)
            .ok_or("transfer expression work budget exceeded")?;
        Ok(())
    }
    fn copy(&mut self, bytes: usize) -> Result<(), &'static str> {
        if bytes > MAX_CELL_BYTES {
            return Err("transfer expression cell exceeds 1 MiB");
        }
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .ok_or("transfer expression work budget exceeded")?;
        Ok(())
    }
}
fn append(output: &mut String, value: &str, budget: &mut Budget) -> Result<(), &'static str> {
    if value.len() > MAX_CELL_BYTES.saturating_sub(output.len()) {
        return Err("transfer expression cell exceeds 1 MiB");
    }
    budget.copy(value.len())?;
    output.push_str(value);
    Ok(())
}
fn text(value: Value, budget: &mut Budget) -> Result<String, &'static str> {
    let value = match value {
        Value::Text(value) => return Ok(value),
        Value::Bool(value) => value.to_string(),
        Value::Int16(value) => value.to_string(),
        Value::Int32(value) => value.to_string(),
        Value::Int64(value) => value.to_string(),
        Value::Float32(value) if value.is_finite() => value.to_string(),
        Value::Float64(value) if value.is_finite() => value.to_string(),
        _ => return Err("text expression requires a supported finite scalar"),
    };
    budget.copy(value.len())?;
    Ok(value)
}
impl Resolved {
    pub fn direct_column(&self) -> Option<usize> {
        match self {
            Self::Column(index) => Some(*index),
            _ => None,
        }
    }
    pub fn references(&self, indices: &mut BTreeSet<usize>) {
        match self {
            Self::Column(index) => {
                indices.insert(*index);
            }
            Self::Literal(_) | Self::Null => {}
            Self::Add(left, right)
            | Self::Subtract(left, right)
            | Self::Multiply(left, right)
            | Self::Divide(left, right, _) => {
                left.references(indices);
                right.references(indices);
            }
            Self::Decimal(input)
            | Self::Negate(input)
            | Self::Abs(input)
            | Self::Round(input, _)
            | Self::Text(input)
            | Self::Trim(input)
            | Self::Upper(input)
            | Self::Lower(input)
            | Self::Replace(input, _, _)
            | Self::Substring(input, _, _) => input.references(indices),
            Self::Concat(inputs) | Self::Coalesce(inputs) => {
                for input in inputs {
                    input.references(indices);
                }
            }
        }
    }
    pub fn evaluate(&self, row: &[Value], budget: &mut Budget) -> Result<Value, &'static str> {
        budget.step()?;
        match self {
            Self::Decimal(input)
            | Self::Negate(input)
            | Self::Abs(input)
            | Self::Round(input, _) => {
                let value = input.evaluate(row, budget)?;
                if matches!(value, Value::Null) {
                    return Ok(Value::Null);
                }
                let value = crate::transfer_decimals::Decimal::parse(value)?;
                let value = match self {
                    Self::Negate(_) => value.negate(),
                    Self::Abs(_) => value.abs(),
                    Self::Round(_, scale) => value.round(*scale)?,
                    _ => value,
                }
                .render();
                budget.copy(value.len())?;
                Ok(Value::Text(value))
            }
            Self::Add(left, right)
            | Self::Subtract(left, right)
            | Self::Multiply(left, right)
            | Self::Divide(left, right, _) => {
                let left = left.evaluate(row, budget)?;
                if matches!(left, Value::Null) {
                    return Ok(Value::Null);
                }
                let right = right.evaluate(row, budget)?;
                if matches!(right, Value::Null) {
                    return Ok(Value::Null);
                }
                let left = crate::transfer_decimals::Decimal::parse(left)?;
                let right = crate::transfer_decimals::Decimal::parse(right)?;
                let value = match self {
                    Self::Add(_, _) => left.add(right)?,
                    Self::Subtract(_, _) => left.add(right.negate())?,
                    Self::Multiply(_, _) => left.multiply(right)?,
                    Self::Divide(_, _, scale) => left.divide(right, *scale)?,
                    _ => unreachable!(),
                }
                .render();
                budget.copy(value.len())?;
                Ok(Value::Text(value))
            }
            Self::Column(index) => {
                let value = row
                    .get(*index)
                    .ok_or("expression source row is inconsistent")?;
                budget.copy(match value {
                    Value::Text(value) => value.len(),
                    Value::Blob(value) => value.len(),
                    _ => 32,
                })?;
                Ok(value.clone())
            }
            Self::Literal(value) => {
                budget.copy(value.len())?;
                Ok(Value::Text(value.clone()))
            }
            Self::Null => Ok(Value::Null),
            Self::Concat(inputs) => {
                let mut output = String::new();
                for input in inputs {
                    let value = input.evaluate(row, budget)?;
                    if matches!(value, Value::Null) {
                        return Ok(Value::Null);
                    }
                    append(&mut output, &text(value, budget)?, budget)?;
                }
                Ok(Value::Text(output))
            }
            Self::Coalesce(inputs) => {
                for input in inputs {
                    let value = input.evaluate(row, budget)?;
                    if !matches!(value, Value::Null) {
                        return text(value, budget).map(Value::Text);
                    }
                }
                Ok(Value::Null)
            }
            Self::Text(input)
            | Self::Trim(input)
            | Self::Upper(input)
            | Self::Lower(input)
            | Self::Replace(input, _, _)
            | Self::Substring(input, _, _) => {
                let value = input.evaluate(row, budget)?;
                if matches!(value, Value::Null) {
                    return Ok(Value::Null);
                }
                let value = text(value, budget)?;
                let mut output = String::new();
                match self {
                    Self::Text(_) => return Ok(Value::Text(value)),
                    Self::Trim(_) => append(&mut output, value.trim(), budget)?,
                    Self::Upper(_) => {
                        for ch in value.chars().flat_map(char::to_uppercase) {
                            append(&mut output, ch.encode_utf8(&mut [0; 4]), budget)?;
                        }
                    }
                    Self::Lower(_) => {
                        for ch in value.chars().flat_map(char::to_lowercase) {
                            append(&mut output, ch.encode_utf8(&mut [0; 4]), budget)?;
                        }
                    }
                    Self::Replace(_, from, to) => {
                        let mut last = 0;
                        for (index, matched) in value.match_indices(from) {
                            append(&mut output, &value[last..index], budget)?;
                            append(&mut output, to, budget)?;
                            last = index + matched.len();
                        }
                        append(&mut output, &value[last..], budget)?;
                    }
                    Self::Substring(_, start, length) => {
                        for ch in value.chars().skip(*start).take(*length) {
                            append(&mut output, ch.encode_utf8(&mut [0; 4]), budget)?;
                        }
                    }
                    _ => unreachable!(),
                }
                Ok(Value::Text(output))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expression(value: serde_json::Value) -> Resolved {
        let value: Expression = serde_json::from_value(value).unwrap();
        value.validate().unwrap();
        value
            .resolve(&["name".into(), "tail".into(), "id".into(), "blob".into()])
            .unwrap()
    }
    #[test]
    fn decimal_expressions_are_exact_bounded_and_null_propagating() {
        let literal = |value: &str| serde_json::json!({"op":"literal","value":value});
        for (operation, left, right, scale, expected) in [
            ("add", "0.1", "0.2", None, "0.3"),
            ("subtract", "1.00", "2.25", None, "-1.25"),
            ("multiply", "-12.5", "0.08", None, "-1"),
            ("divide", "1", "8", Some(2), "0.13"),
            ("divide", "-1", "8", Some(2), "-0.13"),
            ("divide", "1", "-8", Some(2), "-0.13"),
            ("divide", "-1", "-8", Some(2), "0.13"),
            ("divide", "1", "3", Some(18), "0.333333333333333333"),
            ("divide", "0.01", "2", Some(0), "0"),
            (
                "subtract",
                "99999999999999999999999999999999999999",
                "99999999999999999999999999999999999999",
                None,
                "0",
            ),
            (
                "multiply",
                "99999999999999999999999999999999999999",
                "1",
                None,
                "99999999999999999999999999999999999999",
            ),
        ] {
            let mut expr =
                serde_json::json!({"op":operation,"left":literal(left),"right":literal(right)});
            if let Some(scale) = scale {
                expr["scale"] = serde_json::json!(scale);
            }
            assert_eq!(
                expression(expr)
                    .evaluate(&[], &mut Budget::default())
                    .unwrap(),
                Value::Text(expected.into())
            );
        }
        for (op, input, scale, expected) in [
            ("decimal", "+000.0100", None, "0.01"),
            ("decimal", "-000.000", None, "0"),
            ("negate", "-12.5", None, "12.5"),
            ("abs", "-12.5", None, "12.5"),
            ("round", "-1.25", Some(1), "-1.3"),
            ("round", "1.25", Some(1), "1.3"),
            ("round", "-0.04", Some(1), "0"),
            ("round", "1.5", Some(0), "2"),
            ("round", "1.5", Some(18), "1.5"),
        ] {
            let mut expr = serde_json::json!({"op":op,"input":literal(input)});
            if let Some(scale) = scale {
                expr["scale"] = serde_json::json!(scale);
            }
            assert_eq!(
                expression(expr)
                    .evaluate(&[], &mut Budget::default())
                    .unwrap(),
                Value::Text(expected.into())
            );
        }
        for value in [
            "",
            " 1",
            "1 ",
            "1e2",
            "NaN",
            "inf",
            "1.",
            ".1",
            "--1",
            "1.2.3",
            "１２",
            "0.0000000000000000001",
            "100000000000000000000000000000000000000",
        ] {
            assert!(
                expression(serde_json::json!({"op":"decimal","input":literal(value)}))
                    .evaluate(&[], &mut Budget::default())
                    .is_err()
            );
        }
        for expr in [
            serde_json::json!({"op":"divide","left":literal("1"),"right":literal("0"),"scale":2}),
            serde_json::json!({"op":"add","left":literal("99999999999999999999999999999999999999"),"right":literal("1")}),
            serde_json::json!({"op":"multiply","left":literal("0.000000000000000001"),"right":literal("0.000000000000000001")}),
        ] {
            assert!(expression(expr)
                .evaluate(&[], &mut Budget::default())
                .is_err());
        }
        let decimal =
            expression(serde_json::json!({"op":"decimal","input":{"op":"column","source":"id"}}));
        for (input, expected) in [
            (Value::Int64(i64::MIN), i64::MIN.to_string()),
            (Value::Int32(i32::MAX), i32::MAX.to_string()),
            (Value::Int16(-7), "-7".into()),
        ] {
            assert_eq!(
                decimal
                    .evaluate(&[Value::Null, Value::Null, input], &mut Budget::default())
                    .unwrap(),
                Value::Text(expected)
            );
        }
        for input in [Value::Float64(0.1), Value::Bool(true), Value::Blob(vec![1])] {
            assert!(decimal
                .evaluate(&[Value::Null, Value::Null, input], &mut Budget::default())
                .is_err());
        }
        for expr in [
            serde_json::json!({"op":"add","left":{"op":"null"},"right":literal("invalid")}),
            serde_json::json!({"op":"divide","left":literal("1"),"right":{"op":"null"},"scale":2}),
            serde_json::json!({"op":"decimal","input":{"op":"null"}}),
        ] {
            assert_eq!(
                expression(expr)
                    .evaluate(&[], &mut Budget::default())
                    .unwrap(),
                Value::Null
            );
        }
        for expr in [
            serde_json::json!({"op":"round","input":literal("1"),"scale":19}),
            serde_json::json!({"op":"divide","left":literal("1"),"right":literal("2"),"scale":19}),
        ] {
            assert!(serde_json::from_value::<Expression>(expr)
                .unwrap()
                .validate()
                .is_err());
        }
        assert!(serde_json::from_value::<Expression>(
            serde_json::json!({"op":"divide","left":literal("1"),"right":literal("2")})
        )
        .is_err());
    }

    #[test]
    fn portable_text_expressions_keep_unicode_nulls_and_native_references() {
        let row = [
            Value::Text(" caféß ".into()),
            Value::Text("A🙂界B".into()),
            Value::Int64(i64::MAX),
            Value::Blob(vec![0, 255]),
        ];
        let concat = expression(serde_json::json!({"op":"concat","inputs":[
            {"op":"upper","input":{"op":"trim","input":{"op":"column","source":"name"}}},
            {"op":"substring","input":{"op":"column","source":"tail"},"start":1,"length":2}]}));
        assert_eq!(
            concat.evaluate(&row, &mut Budget::default()).unwrap(),
            Value::Text("CAFÉSS🙂界".into())
        );
        let mut indices = BTreeSet::new();
        concat.references(&mut indices);
        assert_eq!(indices, BTreeSet::from([0, 1]));
        for (expr, expected) in [
            (
                serde_json::json!({"op":"concat","inputs":[{"op":"null"},{"op":"literal","value":"x"}]}),
                Value::Null,
            ),
            (
                serde_json::json!({"op":"coalesce","inputs":[{"op":"null"},{"op":"literal","value":""}]}),
                Value::Text("".into()),
            ),
            (
                serde_json::json!({"op":"column","source":"id"}),
                Value::Int64(i64::MAX),
            ),
            (
                serde_json::json!({"op":"text","input":{"op":"column","source":"id"}}),
                Value::Text(i64::MAX.to_string()),
            ),
            (
                serde_json::json!({"op":"column","source":"blob"}),
                Value::Blob(vec![0, 255]),
            ),
            (
                serde_json::json!({"op":"lower","input":{"op":"replace","input":{"op":"literal","value":"ÉÉ-x"},"from":"É","to":"Ö"}}),
                Value::Text("öö-x".into()),
            ),
        ] {
            assert_eq!(
                expression(expr)
                    .evaluate(&row, &mut Budget::default())
                    .unwrap(),
                expected
            );
        }
        assert!(expression(
            serde_json::json!({"op":"text","input":{"op":"column","source":"blob"}})
        )
        .evaluate(&row, &mut Budget::default())
        .is_err());
    }
    #[test]
    fn expressions_refuse_ambiguous_sources_expansion_and_work_exhaustion() {
        let column: Expression =
            serde_json::from_value(serde_json::json!({"op":"column","source":"private-source"}))
                .unwrap();
        assert!(column.resolve(&[]).is_err());
        assert!(column
            .resolve(&["private-source".into(), "private-source".into()])
            .is_err());
        let upper =
            expression(serde_json::json!({"op":"upper","input":{"op":"column","source":"name"}}));
        let row = [Value::Text("ß".repeat(MAX_CELL_BYTES / 2))];
        // Unicode case expansion remains bounded even when the input fits.
        let expand = expression(
            serde_json::json!({"op":"replace","input":{"op":"column","source":"name"},"from":"ß","to":"123"}),
        );
        assert!(expand.evaluate(&row, &mut Budget::default()).is_err());
        assert!(upper
            .evaluate(
                &row,
                &mut Budget {
                    nodes: 1,
                    bytes: usize::MAX
                }
            )
            .is_err());
        assert!(upper
            .evaluate(
                &row,
                &mut Budget {
                    nodes: 100,
                    bytes: 1
                }
            )
            .is_err());
        let mut deep = serde_json::json!({"op":"null"});
        for _ in 0..12 {
            deep = serde_json::json!({"op":"trim","input":deep});
        }
        assert!(serde_json::from_value::<Expression>(deep)
            .unwrap()
            .validate()
            .is_err());
        for bad in [
            serde_json::json!({"op":"sql","value":"private-input"}),
            serde_json::json!({"op":"literal","value":"x","sql":"private-input"}),
            serde_json::json!({"op":"null","extra":"private-input"}),
        ] {
            assert!(serde_json::from_value::<Expression>(bad).is_err());
        }
    }
}
