//! Exact bounded row/column selection, preserving source ordinals and duplicate names.
use std::collections::HashSet;

use crate::error::{ApiError, ApiResult};
use sift_protocol::{ColumnMetadata, Row};

pub(crate) fn validate_selection(rows: &[u64], columns: &[u32]) -> ApiResult<()> {
    if rows.is_empty()
        || rows.len() > 100
        || columns.is_empty()
        || columns.len() > 64
        || rows.iter().any(|row| *row >= 50_000)
        || rows.iter().collect::<HashSet<_>>().len() != rows.len()
        || columns.iter().collect::<HashSet<_>>().len() != columns.len()
    {
        return Err(ApiError::BadRequest("Select 1–100 distinct source rows and 1–64 distinct columns within the first 50,000 rows".into()));
    }
    Ok(())
}
pub(crate) fn select_columns(
    columns: &[ColumnMetadata],
    indices: &[u32],
) -> ApiResult<Vec<ColumnMetadata>> {
    indices
        .iter()
        .map(|index| {
            columns.get(*index as usize).cloned().ok_or_else(|| {
                ApiError::BadRequest(
                    "Selected column is absent from the original result schema".into(),
                )
            })
        })
        .collect()
}
pub(crate) fn select_row(row: &Row, indices: &[u32]) -> ApiResult<Row> {
    Ok(Row::new(
        indices
            .iter()
            .map(|index| {
                row.values.get(*index as usize).cloned().ok_or_else(|| {
                    ApiError::BadRequest("Source row does not match its result schema".into())
                })
            })
            .collect::<ApiResult<_>>()?,
    ))
}
pub(crate) fn select_rows(rows: &[Row], ordinals: &[u64], indices: &[u32]) -> ApiResult<Vec<Row>> {
    ordinals.iter().map(|ordinal| rows.get(*ordinal as usize)
        .ok_or_else(||ApiError::BadRequest("Selected row was not retained or lies outside the AI excerpt. Select retained rows or a narrower projection; Sift will not rerun the query.".into()))
        .and_then(|row|select_row(row,indices))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::Value;

    #[test]
    fn selection_preserves_display_order_and_never_fills_missing_rows() {
        let rows = vec![
            Row::new(vec![Value::Int64(1), Value::Text("first".into())]),
            Row::new(vec![Value::Int64(2), Value::Text("second".into())]),
        ];
        assert_eq!(
            select_rows(&rows, &[1, 0], &[1, 0]).unwrap()[0].values,
            vec![Value::Text("second".into()), Value::Int64(2)]
        );
        assert!(select_rows(&rows, &[2], &[0]).is_err());
        assert!(select_rows(&rows, &[0], &[2]).is_err());
        for (ordinals, columns) in [
            (vec![], vec![0]),
            (vec![0, 0], vec![0]),
            (vec![0], vec![0, 0]),
            (vec![50_000], vec![0]),
            ((0..101).collect(), vec![0]),
            (vec![0], (0..65).collect()),
        ] {
            assert!(validate_selection(&ordinals, &columns).is_err());
        }
    }
}
