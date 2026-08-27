use futures::StreamExt as _;
use sift_metadata::{MetadataStore, PrincipalId};
use sift_protocol::{
    CsvImportRequest, ExportFormat, ExportRequest, TransferDirection, TransferEndpoint,
    TransferExecutionResult, TransferRecipe,
};

use crate::error::{ApiError, ApiResult};
use crate::formatter_extension::FormatterPhase;
use crate::session::SessionStore;

const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

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
    if recipe.direction == TransferDirection::Import {
        if recipe.source != TransferEndpoint::Upload || recipe.sink != TransferEndpoint::Table {
            return Err(ApiError::BadRequest(
                "this recipe is not an upload-to-table import".into(),
            ));
        }
        let mut data = request
            .data
            .ok_or_else(|| ApiError::BadRequest("import data is required".into()))?;
        if recipe.format_id == "xlsx" {
            let sheet = request
                .sheet
                .as_deref()
                .ok_or_else(|| ApiError::BadRequest("XLSX sheet selection is required".into()))?;
            data = xlsx_to_csv(&data, sheet)?;
        } else if recipe.format_id != "csv" {
            data = invoke_extension_formatter(sessions, metadata, actor, recipe, &data).await?;
        }
        let table = request
            .table
            .ok_or_else(|| ApiError::BadRequest("import table is required".into()))?;
        let table = table.schema.map_or(table.name.clone(), |schema| {
            format!("{schema}.{}", table.name)
        });
        let result = crate::csv_import::import(
            sessions,
            request.session_id,
            request.connection_id,
            CsvImportRequest {
                table,
                data,
                header: true,
                delimiter: ',',
                null_value: Some("NULL".into()),
                create_table: request.create_table,
                conflict_policy: request.conflict_policy.unwrap_or_default(),
            },
        )
        .await?;
        return Ok(TransferExecutionResult::Import { result });
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
    use std::io::Cursor;

    if data.len() > MAX_ARTIFACT_BYTES {
        return Err(ApiError::BadRequest("XLSX import exceeds 64 MiB".into()));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(data))
        .map_err(|_| ApiError::BadRequest("XLSX archive is invalid".into()))?;
    let workbook = read_zip_text(&mut archive, "xl/workbook.xml")?;
    let relationships = read_zip_text(&mut archive, "xl/_rels/workbook.xml.rels")?;
    let workbook = roxmltree::Document::parse(&workbook)
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
    let relationships = roxmltree::Document::parse(&relationships)
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
    let document = roxmltree::Document::parse(&worksheet)
        .map_err(|_| ApiError::BadRequest("XLSX worksheet XML is invalid".into()))?;
    let mut writer = csv::Writer::from_writer(Vec::new());
    for row in document
        .descendants()
        .filter(|node| node.tag_name().name() == "row")
    {
        let mut values = Vec::<String>::new();
        for cell in row.children().filter(|node| node.tag_name().name() == "c") {
            let index = cell
                .attribute("r")
                .map(column_index)
                .transpose()?
                .unwrap_or(values.len());
            values.resize(index + 1, String::new());
            let raw = cell
                .descendants()
                .find(|node| node.tag_name().name() == "v" || node.tag_name().name() == "t")
                .and_then(|node| node.text())
                .unwrap_or("");
            values[index] = if cell.attribute("t") == Some("s") {
                raw.parse::<usize>()
                    .ok()
                    .and_then(|index| shared.get(index))
                    .cloned()
                    .ok_or_else(|| {
                        ApiError::BadRequest("XLSX shared string index is invalid".into())
                    })?
            } else {
                raw.to_string()
            };
        }
        writer
            .write_record(values)
            .map_err(|_| ApiError::BadRequest("XLSX row is invalid".into()))?;
    }
    writer
        .into_inner()
        .map_err(|_| ApiError::BadRequest("XLSX conversion failed".into()))
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
    file.read_to_string(&mut text)
        .map_err(|_| ApiError::BadRequest("XLSX part is not UTF-8 XML".into()))?;
    Ok(text)
}

fn parse_shared_strings(xml: &str) -> ApiResult<Vec<String>> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|_| ApiError::BadRequest("XLSX shared strings are invalid".into()))?;
    Ok(document
        .descendants()
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
    if letters == 0 {
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
