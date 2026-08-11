//! Pure, engine-independent row comparison.
//!
//! Source acquisition, authorization, spill, retention, and cancellation stay
//! in the server. This module accepts already bounded protocol rows and never
//! performs I/O.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use sift_protocol::{
    CellComparisonStatus, CellDiff, ColumnMetadata, ColumnTolerance, CompareColumn,
    CompareColumnPair, CompareColumnStatus, PrimitiveType, ResolvedCompareKey, Row, RowDiff,
    RowDiffKind, TypeCategory, TypeRef, UnicodeNormalization, Value,
};
use unicode_normalization::UnicodeNormalization as _;

#[derive(Debug, Clone)]
pub struct ComparisonDataset {
    pub columns: Vec<ColumnMetadata>,
    pub rows: Vec<Row>,
    pub immutable_order: bool,
}

#[derive(Debug, Clone)]
pub struct ComparisonInput {
    pub left: ComparisonDataset,
    pub right: ComparisonDataset,
    pub mappings: Vec<CompareColumnPair>,
    pub key: ResolvedCompareKey,
    pub tolerances: Vec<ColumnTolerance>,
    pub max_diff_rows: usize,
    pub max_duplicate_group: usize,
    /// Cooperative cancellation for CPU-heavy comparisons. This is an
    /// internal execution control, never part of the wire contract.
    pub cancel: Option<Arc<AtomicBool>>,
}

#[derive(Debug, Clone)]
pub struct ComparisonOutput {
    pub columns: Vec<CompareColumn>,
    pub rows: Vec<RowDiff>,
    pub equal_rows: u64,
    pub changed_rows: u64,
    pub added_rows: u64,
    pub removed_rows: u64,
    pub incomparable_rows: u64,
    pub duplicate_key_groups: u64,
    pub truncated: bool,
    pub digest: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ComparisonError {
    #[error("column mapping references missing {side} column {column:?}")]
    MissingColumn { side: &'static str, column: String },
    #[error("column {column:?} is mapped more than once on the {side}")]
    DuplicateMapping { side: &'static str, column: String },
    #[error("comparison key must contain at least one column")]
    EmptyKey,
    #[error("row-ordinal keys require two immutable retained results")]
    UnsafeOrdinalKey,
    #[error("comparison key column {column:?} is not a compatible mapped column")]
    InvalidKeyColumn { column: String },
    #[error("tolerance references unmapped column {column:?}")]
    InvalidToleranceColumn { column: String },
    #[error("tolerance for column {column:?} is invalid")]
    InvalidTolerance { column: String },
    #[error("{side} row {row} has {actual} values but its schema has {expected} columns")]
    MalformedRow {
        side: &'static str,
        row: usize,
        actual: usize,
        expected: usize,
    },
    #[error("duplicate key group exceeds the limit of {limit}")]
    DuplicateGroupTooLarge { limit: usize },
    #[error("serializing canonical comparison data failed: {0}")]
    Serialization(String),
    #[error("comparison was canceled")]
    Canceled,
}

#[derive(Clone, Copy)]
struct MappedIndex {
    left: usize,
    right: usize,
}

pub fn compare(input: ComparisonInput) -> Result<ComparisonOutput, ComparisonError> {
    check_canceled(&input)?;
    validate_rows("left", &input.left)?;
    validate_rows("right", &input.right)?;
    let (columns, mapped) =
        resolve_columns(&input.left.columns, &input.right.columns, &input.mappings)?;
    let key_indices = resolve_key(&input, &columns, &mapped)?;
    let tolerances = resolve_tolerances(&input.tolerances, &columns, mapped.len())?;

    let mut left_groups = group_rows(&input.left.rows, &key_indices, true)?;
    let mut right_groups = group_rows(&input.right.rows, &key_indices, false)?;
    let all_keys: BTreeSet<String> = left_groups
        .keys()
        .chain(right_groups.keys())
        .cloned()
        .collect();

    let mut output = ComparisonOutput {
        columns,
        rows: Vec::new(),
        equal_rows: 0,
        changed_rows: 0,
        added_rows: 0,
        removed_rows: 0,
        incomparable_rows: 0,
        duplicate_key_groups: 0,
        truncated: false,
        digest: String::new(),
    };

    for encoded_key in all_keys {
        check_canceled(&input)?;
        let mut left = left_groups.remove(&encoded_key).unwrap_or_default();
        let mut right = right_groups.remove(&encoded_key).unwrap_or_default();
        if left.len().max(right.len()) > input.max_duplicate_group {
            return Err(ComparisonError::DuplicateGroupTooLarge {
                limit: input.max_duplicate_group,
            });
        }
        let duplicate = left.len() > 1 || right.len() > 1;
        if duplicate {
            output.duplicate_key_groups += 1;
        }
        sort_group(&mut left, &mapped, true)?;
        sort_group(&mut right, &mapped, false)?;
        let key = left
            .first()
            .map(|entry| entry.key.clone())
            .or_else(|| right.first().map(|entry| entry.key.clone()))
            .unwrap_or_default();

        // Exact row digests are paired first. This makes duplicate-key
        // matching deterministic and avoids reporting changes for identical
        // multiset members merely because provider row order differed.
        let mut li = 0;
        let mut ri = 0;
        let mut left_remaining = Vec::new();
        let mut right_remaining = Vec::new();
        while li < left.len() && ri < right.len() {
            match left[li].digest.cmp(&right[ri].digest) {
                std::cmp::Ordering::Equal => {
                    output.equal_rows += 1;
                    li += 1;
                    ri += 1;
                }
                std::cmp::Ordering::Less => {
                    left_remaining.push(left[li].clone());
                    li += 1;
                }
                std::cmp::Ordering::Greater => {
                    right_remaining.push(right[ri].clone());
                    ri += 1;
                }
            }
        }
        left_remaining.extend_from_slice(&left[li..]);
        right_remaining.extend_from_slice(&right[ri..]);

        let paired = left_remaining.len().min(right_remaining.len());
        for occurrence in 0..paired {
            let cells = compare_row(
                left_remaining[occurrence].row,
                right_remaining[occurrence].row,
                &input.left.columns,
                &input.right.columns,
                &mapped,
                &tolerances,
            );
            if cells.iter().all(|cell| {
                matches!(
                    cell.status,
                    CellComparisonStatus::Equal | CellComparisonStatus::TolerantEqual
                )
            }) {
                output.equal_rows += 1;
                continue;
            }
            let incomparable = cells.iter().any(|cell| {
                matches!(
                    cell.status,
                    CellComparisonStatus::Incomparable | CellComparisonStatus::ConversionFailed
                )
            });
            let kind = if incomparable {
                output.incomparable_rows += 1;
                RowDiffKind::Incomparable
            } else {
                output.changed_rows += 1;
                RowDiffKind::Changed
            };
            retain_diff(
                &mut output,
                input.max_diff_rows,
                RowDiff {
                    key: key.clone(),
                    occurrence: occurrence as u32,
                    kind,
                    duplicate_key: duplicate,
                    cells,
                },
            );
        }
        for (offset, entry) in left_remaining.into_iter().skip(paired).enumerate() {
            output.removed_rows += 1;
            let cells = one_sided_cells(
                entry.row,
                true,
                &input.left.columns,
                &input.right.columns,
                &mapped,
            );
            retain_diff(
                &mut output,
                input.max_diff_rows,
                RowDiff {
                    key: key.clone(),
                    occurrence: (paired + offset) as u32,
                    kind: RowDiffKind::Removed,
                    duplicate_key: duplicate,
                    cells,
                },
            );
        }
        for (offset, entry) in right_remaining.into_iter().skip(paired).enumerate() {
            output.added_rows += 1;
            let cells = one_sided_cells(
                entry.row,
                false,
                &input.left.columns,
                &input.right.columns,
                &mapped,
            );
            retain_diff(
                &mut output,
                input.max_diff_rows,
                RowDiff {
                    key: key.clone(),
                    occurrence: (paired + offset) as u32,
                    kind: RowDiffKind::Added,
                    duplicate_key: duplicate,
                    cells,
                },
            );
        }
    }

    let digest_input = serde_json::to_vec(&(
        &output.columns,
        &input.key,
        &input.tolerances,
        output.equal_rows,
        output.changed_rows,
        output.added_rows,
        output.removed_rows,
        output.incomparable_rows,
        output.duplicate_key_groups,
        output.truncated,
        &output.rows,
    ))
    .map_err(|error| ComparisonError::Serialization(error.to_string()))?;
    output.digest = format!("cmpfp:{:x}", Sha256::digest(digest_input));
    Ok(output)
}

fn check_canceled(input: &ComparisonInput) -> Result<(), ComparisonError> {
    if input
        .cancel
        .as_ref()
        .is_some_and(|cancel| cancel.load(Ordering::Acquire))
    {
        Err(ComparisonError::Canceled)
    } else {
        Ok(())
    }
}

fn validate_rows(side: &'static str, dataset: &ComparisonDataset) -> Result<(), ComparisonError> {
    for (index, row) in dataset.rows.iter().enumerate() {
        if row.values.len() != dataset.columns.len() {
            return Err(ComparisonError::MalformedRow {
                side,
                row: index,
                actual: row.values.len(),
                expected: dataset.columns.len(),
            });
        }
    }
    Ok(())
}

fn resolve_columns(
    left: &[ColumnMetadata],
    right: &[ColumnMetadata],
    explicit: &[CompareColumnPair],
) -> Result<(Vec<CompareColumn>, Vec<MappedIndex>), ComparisonError> {
    let left_by_name: HashMap<&str, usize> = left
        .iter()
        .enumerate()
        .map(|(index, column)| (column.name.as_str(), index))
        .collect();
    let right_by_name: HashMap<&str, usize> = right
        .iter()
        .enumerate()
        .map(|(index, column)| (column.name.as_str(), index))
        .collect();
    let mut used_left = HashSet::new();
    let mut used_right = HashSet::new();
    let mut pairs = Vec::new();
    for mapping in explicit {
        let left_index = *left_by_name.get(mapping.left.as_str()).ok_or_else(|| {
            ComparisonError::MissingColumn {
                side: "left",
                column: mapping.left.clone(),
            }
        })?;
        let right_index = *right_by_name.get(mapping.right.as_str()).ok_or_else(|| {
            ComparisonError::MissingColumn {
                side: "right",
                column: mapping.right.clone(),
            }
        })?;
        if !used_left.insert(left_index) {
            return Err(ComparisonError::DuplicateMapping {
                side: "left",
                column: mapping.left.clone(),
            });
        }
        if !used_right.insert(right_index) {
            return Err(ComparisonError::DuplicateMapping {
                side: "right",
                column: mapping.right.clone(),
            });
        }
        pairs.push(MappedIndex {
            left: left_index,
            right: right_index,
        });
    }
    for (left_index, left_column) in left.iter().enumerate() {
        if used_left.contains(&left_index) {
            continue;
        }
        if let Some(&right_index) = right_by_name.get(left_column.name.as_str()) {
            if used_right.insert(right_index) {
                used_left.insert(left_index);
                pairs.push(MappedIndex {
                    left: left_index,
                    right: right_index,
                });
            }
        }
    }
    pairs.sort_by_key(|pair| pair.left);

    let mut columns = Vec::new();
    for pair in &pairs {
        columns.push(CompareColumn {
            left: Some(left[pair.left].clone()),
            right: Some(right[pair.right].clone()),
            status: if compatible_types(&left[pair.left].type_ref, &right[pair.right].type_ref) {
                CompareColumnStatus::Mapped
            } else {
                CompareColumnStatus::Incompatible
            },
        });
    }
    for (index, column) in left.iter().enumerate() {
        if !used_left.contains(&index) {
            columns.push(CompareColumn {
                left: Some(column.clone()),
                right: None,
                status: CompareColumnStatus::MissingRight,
            });
        }
    }
    for (index, column) in right.iter().enumerate() {
        if !used_right.contains(&index) {
            columns.push(CompareColumn {
                left: None,
                right: Some(column.clone()),
                status: CompareColumnStatus::MissingLeft,
            });
        }
    }
    Ok((columns, pairs))
}

fn resolve_key(
    input: &ComparisonInput,
    columns: &[CompareColumn],
    mapped: &[MappedIndex],
) -> Result<Vec<MappedIndex>, ComparisonError> {
    if input.key.row_ordinal {
        if !input.left.immutable_order || !input.right.immutable_order {
            return Err(ComparisonError::UnsafeOrdinalKey);
        }
        return Ok(Vec::new());
    }
    if input.key.columns.is_empty() {
        return Err(ComparisonError::EmptyKey);
    }
    let mut result = Vec::new();
    for key in &input.key.columns {
        let Some(index) = mapped.iter().copied().find(|pair| {
            input.left.columns[pair.left].name == key.left
                && input.right.columns[pair.right].name == key.right
        }) else {
            return Err(ComparisonError::InvalidKeyColumn {
                column: format!("{} -> {}", key.left, key.right),
            });
        };
        let compatible = columns.iter().any(|column| {
            column.status == CompareColumnStatus::Mapped
                && column
                    .left
                    .as_ref()
                    .is_some_and(|left| left.name == key.left)
                && column
                    .right
                    .as_ref()
                    .is_some_and(|right| right.name == key.right)
        });
        if !compatible {
            return Err(ComparisonError::InvalidKeyColumn {
                column: format!("{} -> {}", key.left, key.right),
            });
        }
        result.push(index);
    }
    Ok(result)
}

fn resolve_tolerances<'a>(
    requested: &'a [ColumnTolerance],
    columns: &[CompareColumn],
    mapped_count: usize,
) -> Result<HashMap<usize, &'a ColumnTolerance>, ComparisonError> {
    let mut result = HashMap::new();
    for tolerance in requested {
        if tolerance
            .numeric_absolute
            .is_some_and(|value| !value.is_finite() || value < 0.0)
            || tolerance
                .numeric_relative
                .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err(ComparisonError::InvalidTolerance {
                column: tolerance.column.clone(),
            });
        }
        let Some(mapped_index) = columns[..mapped_count].iter().position(|column| {
            column.status == CompareColumnStatus::Mapped
                && column
                    .left
                    .as_ref()
                    .is_some_and(|left| left.name == tolerance.column)
        }) else {
            return Err(ComparisonError::InvalidToleranceColumn {
                column: tolerance.column.clone(),
            });
        };
        if result.insert(mapped_index, tolerance).is_some() {
            return Err(ComparisonError::InvalidTolerance {
                column: tolerance.column.clone(),
            });
        }
    }
    Ok(result)
}

#[derive(Clone)]
struct GroupEntry<'a> {
    row: &'a Row,
    key: Vec<Value>,
    digest: String,
}

fn group_rows<'a>(
    rows: &'a [Row],
    key: &[MappedIndex],
    left: bool,
) -> Result<BTreeMap<String, Vec<GroupEntry<'a>>>, ComparisonError> {
    let mut groups: BTreeMap<String, Vec<GroupEntry<'a>>> = BTreeMap::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let values = if key.is_empty() {
            vec![Value::Int64(ordinal as i64)]
        } else {
            key.iter()
                .map(|pair| row.values[if left { pair.left } else { pair.right }].clone())
                .collect()
        };
        let canonical: Vec<String> = values.iter().map(canonical_key_value).collect();
        let encoded = serde_json::to_string(&canonical)
            .map_err(|error| ComparisonError::Serialization(error.to_string()))?;
        groups.entry(encoded).or_default().push(GroupEntry {
            row,
            key: values,
            digest: String::new(),
        });
    }
    Ok(groups)
}

fn sort_group(
    group: &mut [GroupEntry<'_>],
    mapped: &[MappedIndex],
    left: bool,
) -> Result<(), ComparisonError> {
    for entry in group.iter_mut() {
        let values: Vec<&Value> = mapped
            .iter()
            .map(|pair| &entry.row.values[if left { pair.left } else { pair.right }])
            .collect();
        let bytes = serde_json::to_vec(&values)
            .map_err(|error| ComparisonError::Serialization(error.to_string()))?;
        entry.digest = format!("{:x}", Sha256::digest(bytes));
    }
    group.sort_by(|a, b| a.digest.cmp(&b.digest));
    Ok(())
}

fn canonical_key_value(value: &Value) -> String {
    match value {
        Value::Null | Value::TypedNull { .. } => "null".into(),
        Value::Int16(value) => format!("number:{value}"),
        Value::Int32(value) => format!("number:{value}"),
        Value::Int64(value) => format!("number:{value}"),
        Value::Decimal(value) => normalize_decimal(value)
            .map(|value| format!("number:{value}"))
            .unwrap_or_else(|| format!("invalid_decimal:{value}")),
        Value::Float32(value) if value.is_finite() => format!("number:{}", f64::from(*value)),
        Value::Float64(value) if value.is_finite() => format!("number:{value}"),
        Value::Float32(value) => format!("float32:{:08x}", value.to_bits()),
        Value::Float64(value) => format!("float64:{:016x}", value.to_bits()),
        other => serde_json::to_string(other)
            .unwrap_or_else(|_| format!("type:{}", other.type_category())),
    }
}

fn compare_row(
    left: &Row,
    right: &Row,
    left_columns: &[ColumnMetadata],
    right_columns: &[ColumnMetadata],
    mapped: &[MappedIndex],
    tolerances: &HashMap<usize, &ColumnTolerance>,
) -> Vec<CellDiff> {
    mapped
        .iter()
        .enumerate()
        .map(|(mapping_index, pair)| {
            let left_value = &left.values[pair.left];
            let right_value = &right.values[pair.right];
            let status = compare_value(
                left_value,
                right_value,
                tolerances.get(&mapping_index).copied(),
            );
            CellDiff {
                column: CompareColumnPair {
                    left: left_columns[pair.left].name.clone(),
                    right: right_columns[pair.right].name.clone(),
                },
                status,
                left: Some(left_value.clone()),
                right: Some(right_value.clone()),
            }
        })
        .collect()
}

fn one_sided_cells(
    row: &Row,
    is_left: bool,
    left_columns: &[ColumnMetadata],
    right_columns: &[ColumnMetadata],
    mapped: &[MappedIndex],
) -> Vec<CellDiff> {
    mapped
        .iter()
        .map(|pair| CellDiff {
            column: CompareColumnPair {
                left: left_columns[pair.left].name.clone(),
                right: right_columns[pair.right].name.clone(),
            },
            status: CellComparisonStatus::Unequal,
            left: is_left.then(|| row.values[pair.left].clone()),
            right: (!is_left).then(|| row.values[pair.right].clone()),
        })
        .collect()
}

fn retain_diff(output: &mut ComparisonOutput, limit: usize, row: RowDiff) {
    if output.rows.len() < limit {
        output.rows.push(row);
    } else {
        output.truncated = true;
    }
}

fn compare_value(
    left: &Value,
    right: &Value,
    tolerance: Option<&ColumnTolerance>,
) -> CellComparisonStatus {
    if left.is_null() || right.is_null() {
        return if left.is_null() && right.is_null() {
            CellComparisonStatus::Equal
        } else {
            CellComparisonStatus::Unequal
        };
    }
    if exact_value_equal(left, right) {
        return CellComparisonStatus::Equal;
    }
    if matches!(left, Value::Native { .. }) || matches!(right, Value::Native { .. }) {
        return CellComparisonStatus::Incomparable;
    }
    let Some(tolerance) = tolerance else {
        return if same_value_family(left, right) {
            CellComparisonStatus::Unequal
        } else {
            CellComparisonStatus::Incomparable
        };
    };
    if is_numeric(left) && is_numeric(right) {
        let (Some(left), Some(right)) = (numeric_f64(left), numeric_f64(right)) else {
            return CellComparisonStatus::ConversionFailed;
        };
        if left.is_nan() || right.is_nan() {
            return CellComparisonStatus::Unequal;
        }
        let difference = (left - right).abs();
        let absolute = tolerance
            .numeric_absolute
            .is_some_and(|maximum| difference <= maximum);
        let relative = tolerance
            .numeric_relative
            .is_some_and(|maximum| difference <= maximum * left.abs().max(right.abs()));
        return if absolute || relative {
            CellComparisonStatus::TolerantEqual
        } else {
            CellComparisonStatus::Unequal
        };
    }
    match (left, right) {
        (Value::Timestamp(left), Value::Timestamp(right)) => tolerance
            .timestamp_microseconds
            .map(|maximum| {
                left.signed_duration_since(*right)
                    .num_microseconds()
                    .map(i64::unsigned_abs)
                    .is_some_and(|value| value <= maximum)
            })
            .map_or(CellComparisonStatus::Unequal, |equal| {
                if equal {
                    CellComparisonStatus::TolerantEqual
                } else {
                    CellComparisonStatus::Unequal
                }
            }),
        (Value::TimestampTz(left), Value::TimestampTz(right)) => tolerance
            .timestamp_microseconds
            .map(|maximum| {
                left.signed_duration_since(*right)
                    .num_microseconds()
                    .map(i64::unsigned_abs)
                    .is_some_and(|value| value <= maximum)
            })
            .map_or(CellComparisonStatus::Unequal, |equal| {
                if equal {
                    CellComparisonStatus::TolerantEqual
                } else {
                    CellComparisonStatus::Unequal
                }
            }),
        (Value::Text(left), Value::Text(right)) => {
            let normalize = |value: &str| {
                let mut value = match tolerance.unicode_normalization {
                    Some(UnicodeNormalization::Nfc) => value.nfc().collect(),
                    Some(UnicodeNormalization::Nfkc) => value.nfkc().collect(),
                    None => value.to_owned(),
                };
                if tolerance.trim_outer_whitespace {
                    value = value.trim().to_owned();
                }
                if tolerance.case_fold {
                    value = value.to_lowercase();
                }
                value
            };
            if normalize(left) == normalize(right) {
                CellComparisonStatus::TolerantEqual
            } else {
                CellComparisonStatus::Unequal
            }
        }
        (Value::Blob(left), Value::Blob(right)) if tolerance.binary_digest => {
            if Sha256::digest(left) == Sha256::digest(right) {
                CellComparisonStatus::TolerantEqual
            } else {
                CellComparisonStatus::Unequal
            }
        }
        _ if same_value_family(left, right) => CellComparisonStatus::Unequal,
        _ => CellComparisonStatus::Incomparable,
    }
}

fn exact_value_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Int16(left), Value::Int16(right)) => left == right,
        (Value::Int16(left), Value::Int32(right)) => i32::from(*left) == *right,
        (Value::Int16(left), Value::Int64(right)) => i64::from(*left) == *right,
        (Value::Int32(left), Value::Int16(right)) => *left == i32::from(*right),
        (Value::Int32(left), Value::Int32(right)) => left == right,
        (Value::Int32(left), Value::Int64(right)) => i64::from(*left) == *right,
        (Value::Int64(left), Value::Int16(right)) => *left == i64::from(*right),
        (Value::Int64(left), Value::Int32(right)) => *left == i64::from(*right),
        (Value::Int64(left), Value::Int64(right)) => left == right,
        (Value::Decimal(left), Value::Decimal(right)) => {
            matches!((normalize_decimal(left), normalize_decimal(right)), (Some(left), Some(right)) if left == right)
        }
        (Value::Decimal(decimal), integer) | (integer, Value::Decimal(decimal))
            if integer_value(integer).is_some() =>
        {
            normalize_decimal(decimal) == integer_value(integer).map(|value| value.to_string())
        }
        _ => left == right,
    }
}

fn normalize_decimal(value: &str) -> Option<String> {
    let value = value.trim();
    let (negative, unsigned) = value
        .strip_prefix('-')
        .map_or((false, value), |rest| (true, rest));
    let unsigned = unsigned.strip_prefix('+').unwrap_or(unsigned);
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if integer.is_empty() && fraction.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let integer = integer.trim_start_matches('0');
    let fraction = fraction.trim_end_matches('0');
    let integer = if integer.is_empty() { "0" } else { integer };
    let sign = if negative && (integer != "0" || !fraction.is_empty()) {
        "-"
    } else {
        ""
    };
    Some(if fraction.is_empty() {
        format!("{sign}{integer}")
    } else {
        format!("{sign}{integer}.{fraction}")
    })
}

fn integer_value(value: &Value) -> Option<i64> {
    match value {
        Value::Int16(value) => Some(i64::from(*value)),
        Value::Int32(value) => Some(i64::from(*value)),
        Value::Int64(value) => Some(*value),
        _ => None,
    }
}

fn is_numeric(value: &Value) -> bool {
    matches!(
        value,
        Value::Int16(_)
            | Value::Int32(_)
            | Value::Int64(_)
            | Value::Float32(_)
            | Value::Float64(_)
            | Value::Decimal(_)
    )
}

fn numeric_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int16(value) => Some(f64::from(*value)),
        Value::Int32(value) => Some(f64::from(*value)),
        Value::Int64(value) => Some(*value as f64),
        Value::Float32(value) => Some(f64::from(*value)),
        Value::Float64(value) => Some(*value),
        Value::Decimal(value) => value.parse().ok(),
        _ => None,
    }
}

fn same_value_family(left: &Value, right: &Value) -> bool {
    (is_numeric(left) && is_numeric(right)) || left.type_category() == right.type_category()
}

fn compatible_types(left: &TypeRef, right: &TypeRef) -> bool {
    match (type_family(left), type_family(right)) {
        (
            TypeFamily::Native(left_provider, left_name),
            TypeFamily::Native(right_provider, right_name),
        ) => left_provider == right_provider && left_name.eq_ignore_ascii_case(right_name),
        (left, right) => left == right,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TypeFamily<'a> {
    Numeric,
    Text,
    Binary,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
    Boolean,
    Uuid,
    Json,
    Native(&'a sift_protocol::ProviderId, &'a str),
    Other,
}

fn type_family(value: &TypeRef) -> TypeFamily<'_> {
    match value {
        TypeRef::Primitive(primitive) => match primitive {
            PrimitiveType::Int16
            | PrimitiveType::Int32
            | PrimitiveType::Int64
            | PrimitiveType::Float32
            | PrimitiveType::Float64
            | PrimitiveType::Decimal => TypeFamily::Numeric,
            PrimitiveType::Text => TypeFamily::Text,
            PrimitiveType::Blob => TypeFamily::Binary,
            PrimitiveType::Date => TypeFamily::Date,
            PrimitiveType::Time => TypeFamily::Time,
            PrimitiveType::Timestamp => TypeFamily::Timestamp,
            PrimitiveType::TimestampTz => TypeFamily::TimestampTz,
            PrimitiveType::Interval => TypeFamily::Interval,
            PrimitiveType::Bool => TypeFamily::Boolean,
            PrimitiveType::Uuid => TypeFamily::Uuid,
            PrimitiveType::Json | PrimitiveType::Jsonb => TypeFamily::Json,
        },
        TypeRef::Native {
            provider_id,
            name,
            category,
        } => match category {
            TypeCategory::Numeric => TypeFamily::Numeric,
            TypeCategory::Text => TypeFamily::Text,
            TypeCategory::Binary => TypeFamily::Binary,
            TypeCategory::Temporal => TypeFamily::Other,
            TypeCategory::Boolean => TypeFamily::Boolean,
            TypeCategory::Uuid => TypeFamily::Uuid,
            TypeCategory::Json => TypeFamily::Json,
            _ => TypeFamily::Native(provider_id, name),
        },
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
    use sift_protocol::{Nullability, PrimitiveType};

    use super::*;

    fn column(name: &str, primitive: PrimitiveType) -> ColumnMetadata {
        let mut column = ColumnMetadata::new(name, TypeRef::Primitive(primitive));
        column.nullable = Nullability::NotNullable;
        column
    }

    fn input(left: Vec<Row>, right: Vec<Row>) -> ComparisonInput {
        let columns = vec![
            column("id", PrimitiveType::Int64),
            column("value", PrimitiveType::Text),
        ];
        ComparisonInput {
            left: ComparisonDataset {
                columns: columns.clone(),
                rows: left,
                immutable_order: true,
            },
            right: ComparisonDataset {
                columns,
                rows: right,
                immutable_order: true,
            },
            mappings: Vec::new(),
            key: ResolvedCompareKey {
                columns: vec![CompareColumnPair {
                    left: "id".into(),
                    right: "id".into(),
                }],
                inferred_constraint: None,
                row_ordinal: false,
            },
            tolerances: Vec::new(),
            max_diff_rows: 100,
            max_duplicate_group: 10,
            cancel: None,
        }
    }

    #[test]
    fn duplicate_groups_match_identical_rows_before_stable_pairing() {
        let left = vec![
            Row::new(vec![Value::Int64(1), Value::Text("a".into())]),
            Row::new(vec![Value::Int64(1), Value::Text("b".into())]),
        ];
        let right = vec![
            Row::new(vec![Value::Int64(1), Value::Text("c".into())]),
            Row::new(vec![Value::Int64(1), Value::Text("a".into())]),
        ];
        let output = compare(input(left, right)).unwrap();
        assert_eq!(output.duplicate_key_groups, 1);
        assert_eq!(output.equal_rows, 1);
        assert_eq!(output.changed_rows, 1);
        assert_eq!(output.rows.len(), 1);
    }

    #[test]
    fn comparison_honors_cooperative_cancellation() {
        let mut request = input(Vec::new(), Vec::new());
        request.cancel = Some(Arc::new(AtomicBool::new(true)));
        assert!(matches!(compare(request), Err(ComparisonError::Canceled)));
    }

    #[test]
    fn text_tolerance_marks_tolerant_equality() {
        let left = vec![Row::new(vec![
            Value::Int64(1),
            Value::Text(" Café ".into()),
        ])];
        let right = vec![Row::new(vec![
            Value::Int64(1),
            Value::Text("cafe\u{301}".into()),
        ])];
        let mut request = input(left, right);
        request.tolerances.push(ColumnTolerance {
            column: "value".into(),
            unicode_normalization: Some(UnicodeNormalization::Nfc),
            case_fold: true,
            trim_outer_whitespace: true,
            ..ColumnTolerance::default()
        });
        let output = compare(request).unwrap();
        assert_eq!(output.equal_rows, 1);
        assert!(output.rows.is_empty());
    }

    #[test]
    fn ordinal_key_rejects_live_source() {
        let mut request = input(Vec::new(), Vec::new());
        request.key = ResolvedCompareKey {
            columns: Vec::new(),
            inferred_constraint: None,
            row_ordinal: true,
        };
        request.left.immutable_order = false;
        assert_eq!(
            compare(request).unwrap_err(),
            ComparisonError::UnsafeOrdinalKey
        );
    }

    #[test]
    fn exact_value_matrix_covers_protocol_families() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 10).unwrap();
        let time = NaiveTime::from_hms_micro_opt(12, 30, 1, 42).unwrap();
        let timestamp = date.and_time(time);
        let values = vec![
            Value::Null,
            Value::TypedNull {
                type_name: "int8".into(),
            },
            Value::Bool(true),
            Value::Int16(7),
            Value::Int32(7),
            Value::Int64(7),
            Value::Float32(1.25),
            Value::Float64(1.25),
            Value::Decimal("7.500".into()),
            Value::Text("hello".into()),
            Value::Blob(vec![0, 1, 2]),
            Value::Date(date),
            Value::Time(time),
            Value::Timestamp(timestamp),
            Value::TimestampTz(Utc.from_utc_datetime(&timestamp)),
            Value::Interval(chrono::Duration::seconds(90)),
            Value::Uuid(uuid::Uuid::nil()),
            Value::Json(serde_json::json!({"a": [1, true]})),
        ];
        for value in values {
            assert_eq!(
                compare_value(&value, &value, None),
                CellComparisonStatus::Equal,
                "value family {}",
                value.type_category()
            );
        }
        assert_eq!(
            compare_value(&Value::Int16(7), &Value::Int64(7), None),
            CellComparisonStatus::Equal
        );
        assert_eq!(
            compare_value(
                &Value::Decimal("007.5000".into()),
                &Value::Decimal("7.5".into()),
                None
            ),
            CellComparisonStatus::Equal
        );
        assert_eq!(
            compare_value(&Value::Decimal("7.0".into()), &Value::Int32(7), None),
            CellComparisonStatus::Equal
        );
        assert_eq!(
            compare_value(&Value::Null, &Value::Int32(0), None),
            CellComparisonStatus::Unequal
        );
    }

    #[test]
    fn tolerance_matrix_distinguishes_equal_conversion_and_incomparable() {
        let numeric = ColumnTolerance {
            column: "value".into(),
            numeric_absolute: Some(0.1),
            numeric_relative: Some(0.01),
            ..ColumnTolerance::default()
        };
        assert_eq!(
            compare_value(
                &Value::Float64(10.0),
                &Value::Decimal("10.05".into()),
                Some(&numeric)
            ),
            CellComparisonStatus::TolerantEqual
        );
        assert_eq!(
            compare_value(
                &Value::Decimal("invalid".into()),
                &Value::Int32(10),
                Some(&numeric)
            ),
            CellComparisonStatus::ConversionFailed
        );
        let timestamp = NaiveDate::from_ymd_opt(2026, 8, 10)
            .unwrap()
            .and_hms_micro_opt(1, 2, 3, 0)
            .unwrap();
        let temporal = ColumnTolerance {
            column: "value".into(),
            timestamp_microseconds: Some(10),
            ..ColumnTolerance::default()
        };
        assert_eq!(
            compare_value(
                &Value::Timestamp(timestamp),
                &Value::Timestamp(timestamp + chrono::Duration::microseconds(9)),
                Some(&temporal)
            ),
            CellComparisonStatus::TolerantEqual
        );
        assert_eq!(
            compare_value(
                &Value::Timestamp(timestamp),
                &Value::TimestampTz(Utc.from_utc_datetime(&timestamp)),
                Some(&temporal)
            ),
            CellComparisonStatus::Incomparable
        );
        let binary = ColumnTolerance {
            column: "value".into(),
            binary_digest: true,
            ..ColumnTolerance::default()
        };
        assert_eq!(
            compare_value(&Value::Blob(vec![1]), &Value::Blob(vec![2]), Some(&binary)),
            CellComparisonStatus::Unequal
        );
        let native = |provider: &str, display: &str| Value::Native {
            provider_id: sift_protocol::ProviderId::new(provider).unwrap(),
            type_name: "geography".into(),
            display_text: display.into(),
        };
        assert_eq!(
            compare_value(&native("test/one", "a"), &native("test/two", "a"), None),
            CellComparisonStatus::Incomparable
        );
    }

    #[test]
    fn diff_retention_truncates_without_losing_summary_counts() {
        let left = (0..4)
            .map(|id| Row::new(vec![Value::Int64(id), Value::Text("left".into())]))
            .collect();
        let right = (0..4)
            .map(|id| Row::new(vec![Value::Int64(id), Value::Text("right".into())]))
            .collect();
        let mut request = input(left, right);
        request.max_diff_rows = 2;
        let output = compare(request).unwrap();
        assert_eq!(output.changed_rows, 4);
        assert_eq!(output.rows.len(), 2);
        assert!(output.truncated);
    }

    #[test]
    fn duplicate_group_cap_fails_instead_of_overwriting_members() {
        let rows = vec![
            Row::new(vec![Value::Int64(1), Value::Text("a".into())]),
            Row::new(vec![Value::Int64(1), Value::Text("b".into())]),
        ];
        let mut request = input(rows.clone(), rows);
        request.max_duplicate_group = 1;
        assert_eq!(
            compare(request).unwrap_err(),
            ComparisonError::DuplicateGroupTooLarge { limit: 1 }
        );
    }

    #[test]
    fn composite_null_keys_form_stable_duplicate_multisets() {
        let mut nullable_id = column("tenant_id", PrimitiveType::Int64);
        nullable_id.nullable = Nullability::Nullable;
        let columns = vec![
            nullable_id,
            column("external_id", PrimitiveType::Text),
            column("value", PrimitiveType::Text),
        ];
        let rows = |second: &str| {
            vec![
                Row::new(vec![
                    Value::Null,
                    Value::Text("key".into()),
                    Value::Text("same".into()),
                ]),
                Row::new(vec![
                    Value::Null,
                    Value::Text("key".into()),
                    Value::Text(second.into()),
                ]),
            ]
        };
        let output = compare(ComparisonInput {
            left: ComparisonDataset {
                columns: columns.clone(),
                rows: rows("left"),
                immutable_order: true,
            },
            right: ComparisonDataset {
                columns,
                rows: rows("right"),
                immutable_order: true,
            },
            mappings: Vec::new(),
            key: ResolvedCompareKey {
                columns: vec![
                    CompareColumnPair {
                        left: "tenant_id".into(),
                        right: "tenant_id".into(),
                    },
                    CompareColumnPair {
                        left: "external_id".into(),
                        right: "external_id".into(),
                    },
                ],
                inferred_constraint: None,
                row_ordinal: false,
            },
            tolerances: Vec::new(),
            max_diff_rows: 10,
            max_duplicate_group: 10,
            cancel: None,
        })
        .unwrap();
        assert_eq!(output.duplicate_key_groups, 1);
        assert_eq!(output.equal_rows, 1);
        assert_eq!(output.changed_rows, 1);
        assert_eq!(
            output.rows[0].key,
            vec![Value::Null, Value::Text("key".into())]
        );
        assert!(output.rows[0].duplicate_key);
    }
}
