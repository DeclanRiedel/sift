use futures::StreamExt as _;
use sha2::Digest as _;
use sift_metadata::{MetadataStore, PrincipalId};
use sift_protocol::{
    CsvImportRequest, ExportFormat, ExportRequest, TransferDirection, TransferEndpoint,
    TransferExecutionResult, TransferRecipe,
};

use crate::error::{ApiError, ApiResult};
use crate::formatter_extension::FormatterPhase;
use crate::session::SessionStore;

const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
mod columns;
pub(crate) mod source_preview;
pub(crate) use columns::validate_options as validate_column_options;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableResume {
    checkpoint_table: String,
    run_id: String,
}

pub async fn execute_recipe(
    sessions: &SessionStore,
    metadata: &MetadataStore,
    actor: PrincipalId,
    recipe: &TransferRecipe,
    request: sift_metadata::http::ExecuteTransferRecipeRequest,
) -> ApiResult<TransferExecutionResult> {
    if sessions.session_owner(request.session_id)? != Some(actor) {
        return Err(ApiError::Forbidden(
            "session belongs to another principal".into(),
        ));
    }
    let source = sessions.conn_entry(request.session_id, request.connection_id)?;
    if let crate::session::ConnectionProvenance::Managed {
        tenant_id,
        principal_id,
        ..
    } = &source.provenance
    {
        let workspace = metadata.get_workspace_for_principal(recipe.workspace_id, actor, false)?;
        if *principal_id != actor || metadata.get_room(workspace.room_id)?.tenant_id != *tenant_id {
            return Err(ApiError::BadRequest(
                "recipe workspace does not match the captured native source".into(),
            ));
        }
    }
    let scoped = sessions.bind_source(request.session_id, request.connection_id, source)?;
    execute_captured_recipe(&scoped, metadata, actor, recipe, request).await
}

async fn execute_captured_recipe(
    sessions: &SessionStore,
    metadata: &MetadataStore,
    actor: PrincipalId,
    recipe: &TransferRecipe,
    request: sift_metadata::http::ExecuteTransferRecipeRequest,
) -> ApiResult<TransferExecutionResult> {
    let upload_digest = request
        .sql_server
        .as_ref()
        .and(request.data.as_ref())
        .map(|data| format!("{:x}", sha2::Sha256::digest(data)));
    let review_context =
        serde_json::to_string(&(recipe, actor.0, upload_digest, &request.sheet))
            .map_err(|_| ApiError::BadRequest("Cannot encode recipe review context".into()))?;
    let saved_mode = sift_core::transfer_columns::sql_server_mode(
        recipe.direction,
        &recipe.format_id,
        &recipe.options,
    )
    .map_err(|message| ApiError::BadRequest(message.into()))?;
    if saved_mode != request.sql_server.as_ref().map(|c| c.mode) {
        return Err(ApiError::BadRequest(
            "Execution column modes must match the saved recipe; use a fresh reviewed run".into(),
        ));
    }
    let durable_resume = recipe
        .options
        .get("durable_resume")
        .map(|value| serde_json::from_value::<DurableResume>(value.clone()))
        .transpose()
        .map_err(|_| ApiError::BadRequest("invalid durable_resume options".into()))?;
    if durable_resume.is_some()
        && (recipe.format_id != "csv" || recipe.direction != TransferDirection::Import)
    {
        return Err(ApiError::BadRequest(
            "durable resume is supported only for CSV imports".into(),
        ));
    }
    if sessions.session_owner(request.session_id)? != Some(actor) {
        return Err(ApiError::Forbidden(
            "session belongs to another principal".into(),
        ));
    }
    let column_selection = columns::parse(
        recipe.direction,
        &recipe.format_id,
        &recipe.format_version,
        &recipe.options,
    )?;
    let null_token = sift_core::transfer_columns::null_token(
        recipe.direction,
        &recipe.format_id,
        &recipe.format_version,
        &recipe.options,
    )
    .map_err(|error| ApiError::BadRequest(error.into()))?;
    if request.dry_run && recipe.direction == TransferDirection::Export {
        if request.sql.as_deref().map_or(true, str::is_empty) {
            return Err(ApiError::BadRequest("export SQL is required".into()));
        }
        return Ok(TransferExecutionResult::Validated {
            direction: recipe.direction,
            format_id: recipe.format_id.clone(),
            resume_from_row: request.resume_from_row,
        });
    }
    if recipe.direction == TransferDirection::Import {
        if recipe.source != TransferEndpoint::Upload || recipe.sink != TransferEndpoint::Table {
            return Err(ApiError::BadRequest(
                "this recipe is not an upload-to-table import".into(),
            ));
        }
        if recipe.format_id == "parquet" {
            let result = crate::parquet_transfer::import(
                sessions,
                request,
                column_selection,
                &review_context,
            )
            .await?;
            sessions.csv_import_completed();
            return Ok(TransferExecutionResult::Import {
                result,
                quarantine_artifact: None,
            });
        }
        let mut data = request
            .data
            .ok_or_else(|| ApiError::BadRequest("import data is required".into()))?;
        if recipe.format_id == "xlsx" {
            let sheet = request
                .sheet
                .as_deref()
                .ok_or_else(|| ApiError::BadRequest("XLSX sheet selection is required".into()))?;
            let sheet = sheet.to_owned();
            data = tokio::task::spawn_blocking(move || xlsx_to_csv(&data, &sheet))
                .await
                .map_err(|_| ApiError::Internal("XLSX conversion worker failed".into()))??;
        } else if recipe.format_id != "csv" {
            data = invoke_extension_formatter(sessions, metadata, actor, recipe, &data).await?;
        }
        let text_columns = column_selection
            .as_ref()
            .map(|selection| selection.computed_text_columns())
            .unwrap_or_default();
        let mut projection_source_digest = None;
        if let Some(selection) = column_selection {
            let bind_original = durable_resume.is_some() && selection.is_projection();
            let mapped_null_token = null_token.clone();
            let transformed = tokio::task::spawn_blocking(move || {
                let null_token = mapped_null_token;
                if null_token == "NULL" {
                    columns::rewrite(data, selection, MAX_ARTIFACT_BYTES, bind_original)
                } else {
                    columns::rewrite_with_null(
                        data,
                        selection,
                        MAX_ARTIFACT_BYTES,
                        bind_original,
                        null_token.clone(),
                    )
                }
            })
            .await
            .map_err(|_| ApiError::Internal("CSV column selection worker failed".into()))??;
            data = transformed.0;
            projection_source_digest = transformed.1;
        }
        let table = request
            .table
            .ok_or_else(|| ApiError::BadRequest("import table is required".into()))?;
        let table = match (table.catalog, table.schema) {
            (Some(catalog), Some(schema)) => format!("{catalog}.{schema}.{}", table.name),
            (Some(_), None) => {
                return Err(ApiError::BadRequest(
                    "catalog-qualified imports require a schema".into(),
                ))
            }
            (None, Some(schema)) => format!("{schema}.{}", table.name),
            (None, None) => table.name,
        };
        let import_request = CsvImportRequest {
            table,
            data,
            header: true,
            delimiter: ',',
            null_value: Some(null_token),
            create_table: request.create_table,
            conflict_policy: request.conflict_policy.unwrap_or_default(),
            dry_run: request.dry_run,
            resume_from_row: request.resume_from_row,
            type_mappings: request.type_mappings,
            sql_server: request.sql_server,
        };
        let mut authority = format!(
            "workspace:{}:recipe:{}:revision:{}:actor:{}",
            recipe.workspace_id.0, recipe.id.0, recipe.revision, actor.0
        );
        if let Some(digest) = projection_source_digest {
            authority.push_str(":projection_source:");
            authority.push_str(&digest);
        }
        let result = if let Some(resume) = durable_resume {
            crate::csv_import::import_with_checkpoint_text(
                sessions,
                request.session_id,
                request.connection_id,
                import_request,
                &resume.checkpoint_table,
                &resume.run_id,
                crate::csv_import::CheckpointContext {
                    authority: &authority,
                    text_columns: &text_columns,
                },
            )
            .await?
        } else {
            crate::csv_import::import_with_context_text(
                sessions,
                request.session_id,
                request.connection_id,
                import_request,
                &review_context,
                &text_columns,
            )
            .await?
        };
        sessions.csv_import_completed();
        let quarantine_artifact = if result.quarantined_rows.is_empty() {
            None
        } else {
            let report = serde_json::to_vec(&sift_protocol::CsvQuarantineReport {
                version: 1,
                columns: result
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                rows: result.quarantined_rows.clone(),
            })
            .map_err(|error| ApiError::Internal(error.to_string()))?;
            if report.len() > MAX_ARTIFACT_BYTES {
                return Err(ApiError::BadRequest(
                    "quarantine report exceeds 64 MiB".into(),
                ));
            }
            Some(metadata.create_workspace_artifact(
                recipe.workspace_id,
                actor,
                sift_protocol::CSV_QUARANTINE_CONTENT_TYPE,
                report,
                Some(chrono::Utc::now() + chrono::Duration::days(7)),
            )?)
        };
        return Ok(TransferExecutionResult::Import {
            result,
            quarantine_artifact,
        });
    }
    if recipe.source != TransferEndpoint::Query || recipe.sink != TransferEndpoint::Artifact {
        return Err(ApiError::BadRequest(
            "this recipe is not a query-to-artifact export".into(),
        ));
    }
    let format = bundled_format(&recipe.format_id);
    if format.is_none() {
        let sql = request
            .sql
            .ok_or_else(|| ApiError::BadRequest("export SQL is required".into()))?;
        let stream = sessions
            .export_stream(
                request.session_id,
                request.connection_id,
                ExportRequest {
                    sql,
                    params: request.params,
                    format: ExportFormat::JsonLines,
                    header: true,
                    null_display: None,
                },
            )
            .await?;
        futures::pin_mut!(stream);
        let workspace = metadata.get_workspace_for_principal(recipe.workspace_id, actor, true)?;
        let tenant_id = metadata.get_room(workspace.room_id)?.tenant_id.0;
        let registry = sessions.formatter_registry();
        let transfer_id = uuid::Uuid::new_v4().to_string();
        let mut content = Vec::new();
        let mut content_type = None;
        append_formatter_output(
            &mut content,
            &mut content_type,
            registry
                .invoke(
                    &recipe.format_id,
                    &recipe.format_version,
                    Some(tenant_id),
                    Some(workspace.room_id.0),
                    &transfer_id,
                    "export",
                    FormatterPhase::Start,
                    &recipe.options,
                    &[],
                )
                .await
                .map_err(formatter_error)?,
        )?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| ApiError::Internal(error.to_string()))?;
            for frame in chunk.chunks(256 * 1024) {
                let output = registry
                    .invoke(
                        &recipe.format_id,
                        &recipe.format_version,
                        Some(tenant_id),
                        Some(workspace.room_id.0),
                        &transfer_id,
                        "export",
                        FormatterPhase::Data,
                        &recipe.options,
                        frame,
                    )
                    .await
                    .map_err(formatter_error)?;
                append_formatter_output(&mut content, &mut content_type, output)?;
            }
        }
        append_formatter_output(
            &mut content,
            &mut content_type,
            registry
                .invoke(
                    &recipe.format_id,
                    &recipe.format_version,
                    Some(tenant_id),
                    Some(workspace.room_id.0),
                    &transfer_id,
                    "export",
                    FormatterPhase::Finish,
                    &recipe.options,
                    &[],
                )
                .await
                .map_err(formatter_error)?,
        )?;
        let artifact = metadata.create_workspace_artifact(
            recipe.workspace_id,
            actor,
            content_type
                .as_deref()
                .unwrap_or("application/octet-stream"),
            content,
            Some(chrono::Utc::now() + chrono::Duration::hours(24)),
        )?;
        return Ok(TransferExecutionResult::Artifact { artifact });
    }
    let format = format.expect("bundled format checked above");
    let content_type = crate::export::content_type(format);
    let stream = sessions
        .export_stream(
            request.session_id,
            request.connection_id,
            ExportRequest {
                sql: request
                    .sql
                    .ok_or_else(|| ApiError::BadRequest("export SQL is required".into()))?,
                params: request.params,
                format,
                header: recipe
                    .options
                    .get("header")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                null_display: recipe
                    .options
                    .get("null_display")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            },
        )
        .await?;
    futures::pin_mut!(stream);
    let mut content = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ApiError::Internal(error.to_string()))?;
        if content.len().saturating_add(chunk.len()) > MAX_ARTIFACT_BYTES {
            return Err(ApiError::BadRequest(
                "transfer artifact exceeds 64 MiB".into(),
            ));
        }
        content.extend_from_slice(&chunk);
    }
    let artifact = metadata
        .create_workspace_artifact(
            recipe.workspace_id,
            actor,
            content_type,
            content,
            Some(chrono::Utc::now() + chrono::Duration::hours(24)),
        )
        .map_err(ApiError::from)?;
    Ok(TransferExecutionResult::Artifact { artifact })
}

fn bundled_format(id: &str) -> Option<ExportFormat> {
    match id {
        "csv" => Some(ExportFormat::Csv),
        "tsv" => Some(ExportFormat::Tsv),
        "jsonl" => Some(ExportFormat::JsonLines),
        "json_array" => Some(ExportFormat::JsonArray),
        "html" => Some(ExportFormat::Html),
        "markdown" => Some(ExportFormat::Markdown),
        "xlsx" => Some(ExportFormat::Xlsx),
        "sql" => Some(ExportFormat::SqlInsert),
        "parquet" => Some(ExportFormat::Parquet),
        _ => None,
    }
}

async fn invoke_extension_formatter(
    sessions: &SessionStore,
    metadata: &MetadataStore,
    actor: PrincipalId,
    recipe: &TransferRecipe,
    data: &[u8],
) -> ApiResult<Vec<u8>> {
    let workspace = metadata.get_workspace_for_principal(recipe.workspace_id, actor, true)?;
    let tenant_id = metadata.get_room(workspace.room_id)?.tenant_id.0;
    let registry = sessions.formatter_registry();
    let transfer_id = uuid::Uuid::new_v4().to_string();
    let mut output = Vec::new();
    let mut ignored_content_type = None;
    for (phase, frame) in std::iter::once((FormatterPhase::Start, &[][..]))
        .chain(
            data.chunks(256 * 1024)
                .map(|chunk| (FormatterPhase::Data, chunk)),
        )
        .chain(std::iter::once((FormatterPhase::Finish, &[][..])))
    {
        let result = registry
            .invoke(
                &recipe.format_id,
                &recipe.format_version,
                Some(tenant_id),
                Some(workspace.room_id.0),
                &transfer_id,
                "import",
                phase,
                &recipe.options,
                frame,
            )
            .await
            .map_err(formatter_error)?;
        append_formatter_output(&mut output, &mut ignored_content_type, result)?;
    }
    Ok(output)
}

fn append_formatter_output(
    content: &mut Vec<u8>,
    content_type: &mut Option<String>,
    (chunk, returned_content_type): (Vec<u8>, Option<String>),
) -> ApiResult<()> {
    if content.len().saturating_add(chunk.len()) > MAX_ARTIFACT_BYTES {
        return Err(ApiError::BadRequest(
            "transfer formatter output exceeds 64 MiB".into(),
        ));
    }
    if let Some(returned) = returned_content_type {
        if returned.is_empty() || returned.len() > 128 {
            return Err(ApiError::BadRequest(
                "transfer formatter returned an invalid content type".into(),
            ));
        }
        if content_type
            .as_ref()
            .is_some_and(|current| current != &returned)
        {
            return Err(ApiError::BadRequest(
                "transfer formatter changed content type during execution".into(),
            ));
        }
        *content_type = Some(returned);
    }
    content.extend_from_slice(&chunk);
    Ok(())
}

fn formatter_error(error: crate::formatter_extension::FormatterError) -> ApiError {
    ApiError::BadRequest(error.to_string())
}

fn xlsx_to_csv(data: &[u8], sheet_name: &str) -> ApiResult<Vec<u8>> {
    xlsx_to_csv_limited(data, sheet_name, MAX_ARTIFACT_BYTES)
}
fn xlsx_to_csv_limited(data: &[u8], sheet_name: &str, output_limit: usize) -> ApiResult<Vec<u8>> {
    use std::io::Cursor;

    if data.len() > MAX_ARTIFACT_BYTES {
        return Err(ApiError::BadRequest("XLSX import exceeds 64 MiB".into()));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(data))
        .map_err(|_| ApiError::BadRequest("XLSX archive is invalid".into()))?;
    if archive.len() > 4096 {
        return Err(ApiError::BadRequest("XLSX has too many ZIP parts".into()));
    }
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        expanded = expanded.saturating_add(
            archive
                .by_index(index)
                .map_err(|_| ApiError::BadRequest("XLSX archive is invalid".into()))?
                .size(),
        );
        if expanded > MAX_ARTIFACT_BYTES as u64 {
            return Err(ApiError::BadRequest(
                "XLSX expanded parts exceed 64 MiB".into(),
            ));
        }
    }
    let workbook = read_zip_text(&mut archive, "xl/workbook.xml")?;
    let relationships = read_zip_text(&mut archive, "xl/_rels/workbook.xml.rels")?;
    let workbook = parse_xlsx_xml(&workbook)
        .map_err(|_| ApiError::BadRequest("XLSX workbook XML is invalid".into()))?;
    let sheet = workbook
        .descendants()
        .find(|node| {
            node.tag_name().name() == "sheet" && node.attribute("name") == Some(sheet_name)
        })
        .ok_or_else(|| ApiError::BadRequest("XLSX sheet was not found".into()))?;
    let relation_id = sheet
        .attributes()
        .find(|attribute| attribute.name() == "id")
        .map(|attribute| attribute.value())
        .ok_or_else(|| ApiError::BadRequest("XLSX sheet relationship is missing".into()))?;
    let relationships = parse_xlsx_xml(&relationships)
        .map_err(|_| ApiError::BadRequest("XLSX relationships are invalid".into()))?;
    let target = relationships
        .descendants()
        .find(|node| {
            node.tag_name().name() == "Relationship" && node.attribute("Id") == Some(relation_id)
        })
        .and_then(|node| node.attribute("Target"))
        .ok_or_else(|| ApiError::BadRequest("XLSX worksheet target is missing".into()))?;
    if target.contains("..") || target.starts_with('/') {
        return Err(ApiError::BadRequest(
            "XLSX worksheet target is unsafe".into(),
        ));
    }
    let worksheet_path = if target.starts_with("xl/") {
        target.to_string()
    } else {
        format!("xl/{target}")
    };
    let worksheet = read_zip_text(&mut archive, &worksheet_path)?;
    let shared = match read_zip_text(&mut archive, "xl/sharedStrings.xml") {
        Ok(xml) => parse_shared_strings(&xml)?,
        Err(_) => Vec::new(),
    };
    let document = parse_xlsx_xml(&worksheet)
        .map_err(|_| ApiError::BadRequest("XLSX worksheet XML is invalid".into()))?;
    let mut writer = csv::Writer::from_writer(columns::BoundedOutput::new(output_limit));
    for row in document
        .descendants()
        .filter(|node| node.tag_name().name() == "row")
    {
        let mut values = Vec::<String>::new();
        let mut row_bytes = 0usize;
        for cell in row.children().filter(|node| node.tag_name().name() == "c") {
            let index = cell
                .attribute("r")
                .map(column_index)
                .transpose()?
                .unwrap_or(values.len());
            if index >= 16384 {
                return Err(ApiError::BadRequest("XLSX row has too many columns".into()));
            }
            values.resize(index + 1, String::new());
            let raw = cell
                .descendants()
                .find(|node| node.tag_name().name() == "v" || node.tag_name().name() == "t")
                .and_then(|node| node.text())
                .unwrap_or("");
            let value = if cell.attribute("t") == Some("s") {
                raw.parse::<usize>()
                    .ok()
                    .and_then(|index| shared.get(index))
                    .map(String::as_str)
                    .ok_or_else(|| {
                        ApiError::BadRequest("XLSX shared string index is invalid".into())
                    })?
            } else {
                raw
            };
            row_bytes = row_bytes.saturating_add(value.len()).saturating_add(1);
            if row_bytes > output_limit {
                return Err(ApiError::BadRequest(
                    "XLSX row exceeds its byte limit".into(),
                ));
            }
            values[index] = value.to_owned();
        }
        writer
            .write_record(values)
            .map_err(|_| ApiError::BadRequest("XLSX row is invalid".into()))?;
    }
    writer
        .into_inner()
        .map(|output| output.into_bytes())
        .map_err(|_| {
            ApiError::BadRequest("XLSX conversion exceeds its byte limit or is invalid".into())
        })
}

fn read_zip_text<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    path: &str,
) -> ApiResult<String> {
    use std::io::Read as _;
    let mut file = archive
        .by_name(path)
        .map_err(|_| ApiError::BadRequest("XLSX part is missing".into()))?;
    if file.size() > MAX_ARTIFACT_BYTES as u64 {
        return Err(ApiError::BadRequest("XLSX part is too large".into()));
    }
    let mut text = String::new();
    file.by_ref()
        .take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| ApiError::BadRequest("XLSX part is not UTF-8 XML".into()))?;
    if text.len() > MAX_ARTIFACT_BYTES {
        return Err(ApiError::BadRequest("XLSX part exceeds 64 MiB".into()));
    }
    Ok(text)
}

fn parse_xlsx_xml(xml: &str) -> Result<roxmltree::Document<'_>, roxmltree::Error> {
    roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            nodes_limit: 1_000_000,
            ..Default::default()
        },
    )
}

fn parse_shared_strings(xml: &str) -> ApiResult<Vec<String>> {
    let document = parse_xlsx_xml(xml)
        .map_err(|_| ApiError::BadRequest("XLSX shared strings are invalid".into()))?;
    Ok(document
        .root_element()
        .children()
        .filter(|node| node.tag_name().name() == "si")
        .map(|item| {
            item.descendants()
                .filter(|node| node.tag_name().name() == "t")
                .filter_map(|node| node.text())
                .collect::<String>()
        })
        .collect())
}

fn column_index(reference: &str) -> ApiResult<usize> {
    let mut value = 0usize;
    let mut letters = 0usize;
    for byte in reference.bytes().take_while(u8::is_ascii_alphabetic) {
        value = value
            .checked_mul(26)
            .and_then(|value| value.checked_add(usize::from(byte.to_ascii_uppercase() - b'A') + 1))
            .ok_or_else(|| ApiError::BadRequest("XLSX cell reference is invalid".into()))?;
        letters += 1;
    }
    if letters == 0 || value > 16384 {
        Err(ApiError::BadRequest(
            "XLSX cell reference is invalid".into(),
        ))
    } else {
        Ok(value - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_recipe_mapping_changes_only_headers_and_rejects_ambiguous_targets() {
        let source = b"source,amount\n\"a,b\",12\n";
        let map = |options| {
            let selection = columns::parse(
                TransferDirection::Import,
                "csv",
                "1",
                &serde_json::json!({"column_mappings":options}),
            )?
            .unwrap();
            columns::rewrite(source.to_vec(), selection, MAX_ARTIFACT_BYTES, false)
                .map(|result| result.0)
        };
        let mapped = map(serde_json::json!({"source": "target"})).unwrap();
        let mut reader = csv::Reader::from_reader(mapped.as_slice());
        assert_eq!(
            reader.headers().unwrap(),
            &csv::StringRecord::from(vec!["target", "amount"])
        );
        assert_eq!(
            reader.records().next().unwrap().unwrap(),
            csv::StringRecord::from(vec!["a,b", "12"])
        );
        assert!(map(serde_json::json!({"source": "amount"})).is_err());
        assert!(map(serde_json::json!({"missing": "target"})).is_err());
    }

    #[test]
    fn xlsx_import_requires_named_sheet_and_preserves_text_cells() {
        let sheet = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
            "<sheetData><row><c r=\"A1\" t=\"inlineStr\"><is><t>name</t></is></c>",
            "<c r=\"B1\" t=\"inlineStr\"><is><t>=SUM(1,1)</t></is></c></row></sheetData>",
            "</worksheet>"
        );
        let workbook = crate::export::build_xlsx(sheet).unwrap();
        let csv = xlsx_to_csv(&workbook, "Results").unwrap();
        assert_eq!(String::from_utf8(csv).unwrap(), "name,\"=SUM(1,1)\"\n");
        assert!(xlsx_to_csv(&workbook, "Missing").is_err());
    }
}

#[cfg(test)]
mod xlsx_projection_limits {
    use super::*;
    use std::io::Write as _;
    #[test]
    fn shared_string_expansion_and_cell_coordinates_are_bounded() {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let text = "é,NULL\n".repeat(40);
        let worksheet=format!("<worksheet><sheetData><row><c r=\"A1\"><is><t>source</t></is></c></row>{}</sheetData></worksheet>",(2..7).map(|row|format!("<row><c r=\"A{row}\" t=\"s\"><v>0</v></c></row>")).collect::<String>());
        for (name,data) in [("xl/workbook.xml","<workbook><sheets><sheet name=\"Data\" id=\"r\"/></sheets></workbook>".into()),("xl/_rels/workbook.xml.rels","<Relationships><Relationship Id=\"r\" Target=\"worksheets/sheet1.xml\"/></Relationships>".into()),("xl/worksheets/sheet1.xml",worksheet),("xl/sharedStrings.xml",format!("<sst><si><t>{text}</t></si></sst>"))] {
            zip.start_file(name,zip::write::SimpleFileOptions::default()).unwrap();zip.write_all(data.as_bytes()).unwrap();
        }
        let data = zip.finish().unwrap().into_inner();
        assert!(xlsx_to_csv_limited(&data, "Data", 1024).is_err());
        let csv = xlsx_to_csv_limited(&data, "Data", 4096).unwrap();
        let rows = csv::Reader::from_reader(csv.as_slice())
            .records()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(&rows[0][0], text);
        assert_eq!(column_index("XFD1").unwrap(), 16383);
        assert!(column_index("XFE1").is_err());
        assert!(column_index("ZZZZZZ1").is_err());
    }
}
