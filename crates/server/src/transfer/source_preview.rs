//! Revision-bound source review. No database access or retained upload.
use super::*;
use sift_protocol::{
    PreviewTransferSourceRequest, Row, TransferMappedPreview, TransferSampleRepresentation,
    TransferSourceColumn, TransferSourcePreview, Value,
};

pub(crate) const MAX_WIRE_BYTES: usize = 4 * 64 * 1024 * 1024 + 1024 * 1024 + 64 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;
const MAX_SCHEMA: usize = 256 * 1024;
const SAMPLE_ROWS: usize = 20;
static WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
pub(crate) struct Mapped {
    pub columns: Vec<TransferSourceColumn>,
    pub rows: Vec<Row>,
    pub representation: TransferSampleRepresentation,
}
pub(crate) struct Contents {
    pub columns: Vec<TransferSourceColumn>,
    pub rows: u64,
    pub mapped: ApiResult<Mapped>,
}
pub(crate) fn bound<T: serde::Serialize>(value: &T, limit: usize) -> ApiResult<()> {
    let mut output = super::columns::BoundedOutput::new(limit);
    serde_json::to_writer(&mut output, value)
        .map_err(|_| ApiError::BadRequest("Source preview exceeds its byte budget".into()))
}
pub(crate) fn check_schema(columns: &[TransferSourceColumn]) -> ApiResult<()> {
    if columns.is_empty()
        || columns.len() > 2000
        || columns
            .iter()
            .any(|column| column.name.contains('\0') || column.name.len() > 4096)
    {
        return Err(ApiError::BadRequest(
            "Source preview requires 1–2000 bounded column names".into(),
        ));
    }
    bound(&columns, MAX_SCHEMA)
}
fn unavailable() -> TransferMappedPreview {
    TransferMappedPreview::Unavailable {reason:"Candidate samples are unavailable under mapping/preview limits; revise selected columns, expressions or null rules".into()}
}
pub(crate) fn reserve_worker() -> ApiResult<std::sync::Arc<tokio::sync::SemaphorePermit<'static>>> {
    WORKERS.try_acquire().map(std::sync::Arc::new).map_err(|_| {
        ApiError::BadRequest(
            "Source preview worker limit reached; retry after the current previews finish".into(),
        )
    })
}
// Admission precedes body buffering; detached decoders retain the same lease.
pub(crate) async fn admit_request(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let permit = match reserve_worker() {
        Ok(permit) => permit,
        Err(error) => return error.into_response(),
    };
    request.extensions_mut().insert(permit.clone());
    let response =
        tokio::time::timeout(std::time::Duration::from_secs(30), next.run(request)).await;
    drop(permit);
    match response {
        Ok(response) => response,
        Err(_) => ApiError::BadRequest("Source preview request timed out".into()).into_response(),
    }
}
pub(crate) async fn preview(
    recipe: TransferRecipe,
    request: PreviewTransferSourceRequest,
    permit: std::sync::Arc<tokio::sync::SemaphorePermit<'static>>,
) -> ApiResult<TransferSourcePreview> {
    if recipe.revision != request.expected_revision {
        return Err(ApiError::BadRequest(
            "Transfer recipe changed; refresh source review".into(),
        ));
    }
    if recipe.direction != TransferDirection::Import
        || recipe.source != TransferEndpoint::Upload
        || recipe.sink != TransferEndpoint::Table
        || !matches!(recipe.format_id.as_str(), "xlsx" | "parquet")
    {
        return Err(ApiError::BadRequest(
            "Source preview requires a bundled XLSX or Parquet upload import".into(),
        ));
    }
    if request.data.is_empty() || request.data.len() > MAX_ARTIFACT_BYTES {
        return Err(ApiError::BadRequest(
            "Source upload must be nonempty and at most 64 MiB".into(),
        ));
    }
    run_worker(
        permit,
        move || decode(recipe, request),
        std::time::Duration::from_secs(30),
    )
    .await
}
async fn run_worker<F, T>(
    permit: std::sync::Arc<tokio::sync::SemaphorePermit<'static>>,
    work: F,
    timeout: std::time::Duration,
) -> ApiResult<T>
where
    F: FnOnce() -> ApiResult<T> + Send + 'static,
    T: Send + 'static,
{
    let guard = crate::shutdown::current_query_guard();
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _guard = guard;
        work()
    });
    tokio::time::timeout(timeout, worker)
        .await
        .map_err(|_| {
            ApiError::BadRequest(
                "Source preview timed out; its bounded worker is still settling".into(),
            )
        })?
        .map_err(|_| ApiError::Internal("Source preview worker failed".into()))?
}
fn decode(
    recipe: TransferRecipe,
    request: PreviewTransferSourceRequest,
) -> ApiResult<TransferSourcePreview> {
    let selection = super::columns::parse(
        recipe.direction,
        &recipe.format_id,
        &request.format_version,
        &request.options,
    )?;
    let digest = format!("{:x}", sha2::Sha256::digest(&request.data));
    let contents = if recipe.format_id == "parquet" {
        crate::parquet_transfer::preview_source(request.data, selection)?
    } else {
        let sheet = request.sheet.as_deref().ok_or_else(|| {
            ApiError::BadRequest("XLSX source preview requires an explicit sheet".into())
        })?;
        let data = super::xlsx_to_csv(&request.data, sheet)?;
        let null = sift_core::transfer_columns::null_token(
            recipe.direction,
            &recipe.format_id,
            &request.format_version,
            &request.options,
        )
        .map_err(|error| ApiError::BadRequest(error.into()))?;
        xlsx(data, selection, null)?
    };
    let mapping = match contents.mapped {
        Ok(mapped)
            if mapped.columns.len() <= 256
                && !mapped.rows.iter().flat_map(|row| &row.values).any(|v| {
                    matches!(v,Value::Float32(n) if !n.is_finite())
                        || matches!(v,Value::Float64(n) if !n.is_finite())
                }) =>
        {
            TransferMappedPreview::Ready {
                columns: mapped.columns,
                sample_rows: mapped.rows,
                representation: mapped.representation,
                sample_rows_limited: contents.rows > SAMPLE_ROWS as u64,
            }
        }
        _ => unavailable(),
    };
    let mut response = TransferSourcePreview {
        recipe_id: recipe.id,
        recipe_revision: recipe.revision,
        format_id: recipe.format_id,
        format_version: request.format_version,
        source_digest: digest,
        source_columns: contents.columns,
        source_rows: contents.rows,
        mapping,
    };
    while bound(&response, MAX_RESPONSE).is_err() {
        match &mut response.mapping {
            TransferMappedPreview::Ready {
                sample_rows,
                sample_rows_limited,
                ..
            } if !sample_rows.is_empty() => {
                sample_rows.pop();
                *sample_rows_limited = true;
            }
            TransferMappedPreview::Ready { .. } => response.mapping = unavailable(),
            TransferMappedPreview::Unavailable { .. } => {
                return Err(ApiError::BadRequest(
                    "Source schema exceeds preview response budget".into(),
                ))
            }
        }
    }
    Ok(response)
}
fn xlsx(
    data: Vec<u8>,
    selection: Option<super::columns::Selection>,
    null: String,
) -> ApiResult<Contents> {
    let mut reader = csv::Reader::from_reader(data.as_slice());
    let headers = reader
        .headers()
        .map_err(|_| ApiError::BadRequest("Converted XLSX headers are invalid".into()))?
        .iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut occurrences = std::collections::HashMap::new();
    for header in &headers {
        *occurrences.entry(header.as_str()).or_insert(0usize) += 1;
    }
    let columns = headers
        .iter()
        .map(|name| TransferSourceColumn {
            name: name.clone(),
            source_type: "text before destination inference".into(),
            nullable: true,
            selectable: occurrences[name.as_str()] == 1,
        })
        .collect::<Vec<_>>();
    check_schema(&columns)?;
    let mut rows = 0;
    for record in reader.records() {
        let record = record
            .map_err(|_| ApiError::BadRequest("Converted XLSX row shape is invalid".into()))?;
        if record.len() != headers.len() {
            return Err(ApiError::BadRequest(
                "Converted XLSX row width changed".into(),
            ));
        }
        rows += 1;
    }
    drop(reader);
    let text = selection
        .as_ref()
        .map(|selection| selection.computed_text_columns())
        .unwrap_or_default();
    let mapped = (|| {
        let data = match selection {
            Some(selection) => {
                super::columns::rewrite_with_null(
                    data,
                    selection,
                    MAX_ARTIFACT_BYTES,
                    false,
                    null.clone(),
                )?
                .0
            }
            None => data,
        };
        let parsed = crate::csv_import::prepare_input(&data, true, ',', Some(&null), &text)?;
        let columns = parsed
            .columns
            .into_iter()
            .map(|column| TransferSourceColumn {
                name: column.name,
                source_type: format!("CSV inferred {:?}", column.inferred_type),
                nullable: column.nullable,
                selectable: true,
            })
            .collect();
        let rows = parsed
            .records
            .into_iter()
            .take(SAMPLE_ROWS)
            .map(|row| Row {
                values: row
                    .into_iter()
                    .map(|value| value.map_or(Value::Null, Value::Text))
                    .collect(),
            })
            .collect();
        Ok(Mapped {
            columns,
            rows,
            representation: TransferSampleRepresentation::CsvTextBeforeDestinationInference,
        })
    })();
    Ok(Contents {
        columns,
        rows,
        mapped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{ColumnMetadata, PrimitiveType, TransferRecipeId, TypeRef, WorkspaceId};
    fn recipe() -> TransferRecipe {
        TransferRecipe {
            id: TransferRecipeId(1),
            workspace_id: WorkspaceId(1),
            name: "preview".into(),
            direction: TransferDirection::Import,
            source: TransferEndpoint::Upload,
            sink: TransferEndpoint::Table,
            format_id: "parquet".into(),
            format_version: "3".into(),
            options: serde_json::json!({}),
            revision: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }
    fn data(kind: PrimitiveType, values: Vec<Value>) -> Vec<u8> {
        let mut encoder = crate::parquet_transfer::Encoder::new(&[ColumnMetadata::new(
            "value",
            TypeRef::Primitive(kind),
        )])
        .unwrap();
        encoder
            .write(
                &values
                    .into_iter()
                    .map(|v| Row { values: vec![v] })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        encoder.finish().unwrap().to_vec()
    }
    fn request(data: Vec<u8>) -> PreviewTransferSourceRequest {
        PreviewTransferSourceRequest {
            expected_revision: 1,
            format_version: "3".into(),
            options: serde_json::json!({}),
            data,
            sheet: None,
        }
    }
    #[test]
    fn source_samples_bound_rows_bytes_and_nonfinite_values_without_fabricating_nulls() {
        let preview = decode(
            recipe(),
            request(data(
                PrimitiveType::Text,
                (0..30).map(|_| Value::Text("NULL".into())).collect(),
            )),
        )
        .unwrap();
        assert_eq!(preview.source_rows, 30);
        match preview.mapping {
            TransferMappedPreview::Ready {
                sample_rows,
                sample_rows_limited,
                ..
            } => {
                assert_eq!(sample_rows.len(), 20);
                assert!(sample_rows_limited);
                assert_eq!(sample_rows[0].values, vec![Value::Text("NULL".into())]);
            }
            _ => panic!("expected samples"),
        }
        let large = decode(
            recipe(),
            request(data(
                PrimitiveType::Blob,
                vec![Value::Blob(vec![9; MAX_RESPONSE + 1])],
            )),
        )
        .unwrap();
        bound(&large, MAX_RESPONSE).unwrap();
        match large.mapping {
            TransferMappedPreview::Ready {
                sample_rows,
                sample_rows_limited,
                ..
            } => {
                assert!(sample_rows.is_empty());
                assert!(sample_rows_limited);
            }
            _ => panic!("expected bounded empty sample"),
        }
        let nonfinite = decode(
            recipe(),
            request(data(PrimitiveType::Float64, vec![Value::Float64(f64::NAN)])),
        )
        .unwrap();
        assert!(matches!(
            nonfinite.mapping,
            TransferMappedPreview::Unavailable { .. }
        ));
        assert_eq!(nonfinite.source_columns[0].source_type, "float64");
        let chosen = sift_core::transfer_columns::Selection::Project(vec![
            sift_core::transfer_columns::Projection {
                source: "valid".into(),
                target: "kept".into(),
            },
        ]);
        let recover = xlsx(
            b"dup,dup,valid\nPRIVATE-A,PRIVATE-B,ok\n".to_vec(),
            Some(chosen),
            "NULL".into(),
        )
        .unwrap();
        assert!(
            !recover.columns[0].selectable
                && !recover.columns[1].selectable
                && recover.columns[2].selectable
        );
        assert_eq!(
            recover.mapped.unwrap().rows[0].values,
            vec![Value::Text("ok".into())]
        );
        let chosen = sift_core::transfer_columns::Selection::Project(vec![
            sift_core::transfer_columns::Projection {
                source: "".into(),
                target: "renamed".into(),
            },
        ]);
        let blank = xlsx(
            b",other\nselected,dropped\n".to_vec(),
            Some(chosen),
            "NULL".into(),
        )
        .unwrap();
        assert!(blank.columns[0].name.is_empty());
        assert_eq!(
            blank.mapped.unwrap().rows[0].values,
            vec![Value::Text("selected".into())]
        );
        let mut malformed = request(vec![1, 2, 3]);
        malformed.sheet = Some("PRIVATE-SHEET".into());
        malformed.options = serde_json::json!({"private":"PRIVATE-OPTIONS"});
        let debug = format!("{malformed:?}");
        assert!(!debug.contains("PRIVATE-"));
        assert!(decode(recipe(), malformed).is_err());
    }
    #[tokio::test]
    async fn source_worker_timeout_retains_admission_and_shutdown_until_actual_completion() {
        let shutdown = crate::Shutdown::default();
        let guard = std::sync::Arc::new(shutdown.track_query());
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let permit = reserve_worker().unwrap();
        let observed = tokio::spawn(crate::shutdown::scope_query(
            Some(guard),
            run_worker(
                permit,
                move || {
                    signal.notify_one();
                    wait.recv().unwrap();
                    Ok(())
                },
                std::time::Duration::from_millis(1),
            ),
        ));
        started.notified().await;
        let second = reserve_worker().unwrap();
        assert!(reserve_worker().is_err());
        assert!(observed.await.unwrap().is_err());
        assert_eq!(shutdown.in_flight(), 1);
        assert!(
            reserve_worker().is_err(),
            "timed-out blocking work still owns its slot"
        );
        release.send(()).unwrap();
        assert_eq!(
            shutdown
                .await_drain(std::time::Duration::from_secs(1))
                .await,
            0
        );
        let freed = reserve_worker().unwrap();
        drop(freed);
        drop(second);
    }
}
