//! Real imports and checkpoint identity after ordered tabular selection.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, NewRoom, PrincipalId,
    RoomKind, TenantId,
};
use sift_protocol::*;
use sift_server::{
    http::{app, AppState, AuthState},
    DriverRegistry, RoomRuntime, SessionStore, Shutdown,
};
use std::sync::Arc;
use tower::ServiceExt;

async fn post(
    router: &Router,
    path: String,
    value: serde_json::Value,
) -> Result<(StatusCode, Vec<u8>), String> {
    let response = router
        .clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap(),
        )
        .await
        .map_err(|_| "request failed")?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .map_err(|_| "response too large")?;
    Ok((status, bytes.to_vec()))
}
fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
async fn query(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    sql: String,
) -> Result<ExecuteResponse, String> {
    store
        .execute_http(
            session,
            ExecuteRequestHttp {
                connection,
                sql,
                params: vec![],
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
        )
        .await
        .map_err(|_| "fixture query failed".into())
}
async fn execute(
    router: &Router,
    id: TransferRecipeId,
    request: &sift_metadata::http::ExecuteTransferRecipeRequest,
) -> Result<(StatusCode, Vec<u8>), String> {
    post(
        router,
        format!("/v1/metadata/transfer-recipes/{}/execute", id.0),
        serde_json::to_value(request).unwrap(),
    )
    .await
}
fn imported(bytes: &[u8]) -> Result<CsvImportResponse, String> {
    match serde_json::from_slice::<TransferExecutionResult>(bytes)
        .map_err(|_| "invalid transfer result")?
    {
        TransferExecutionResult::Import { result, .. } => Ok(result),
        _ => Err("expected an import result".into()),
    }
}

fn tabular_upload(format: &str, ids: [i64; 2]) -> Vec<u8> {
    tabular_upload_with_discard(format, ids, "DROPPED-PRIVATE")
}
fn tabular_upload_with_discard(format: &str, ids: [i64; 2], discard: &str) -> Vec<u8> {
    if format == "csv" {
        return format!(
            "source_id,discard,raw_label\n{},DROPPED-PRIVATE,café\n{},DROPPED-PRIVATE,\n",
            ids[0], ids[1]
        )
        .into_bytes();
    }
    if format == "parquet" {
        use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
        use arrow_schema::{DataType, Field, Schema};
        let schema = Arc::new(Schema::new(vec![
            Field::new("source_id", DataType::Int64, false),
            Field::new("discard", DataType::Utf8, false),
            Field::new("raw_label", DataType::Utf8, false),
        ]));
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(ids.to_vec())),
            Arc::new(StringArray::from(vec![discard, discard])),
            Arc::new(StringArray::from(vec!["café", ""])),
        ];
        let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
        let mut writer = parquet::arrow::ArrowWriter::try_new(Vec::new(), schema, None).unwrap();
        writer.write(&batch).unwrap();
        return writer.into_inner().unwrap();
    }
    use std::io::Write as _;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, xml) in [
        (
            "xl/workbook.xml",
            r#"<workbook xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" r:id="r1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships><Relationship Id="r1" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet><sheetData><row><c r="A1" t="inlineStr"><is><t>source_id</t></is></c><c r="B1" t="inlineStr"><is><t>discard</t></is></c><c r="C1" t="inlineStr"><is><t>raw_label</t></is></c></row><row><c r="A2"><v>1</v></c><c r="B2" t="inlineStr"><is><t>DROPPED-PRIVATE</t></is></c><c r="C2" t="inlineStr"><is><t>café</t></is></c></row><row><c r="A3"><v>2</v></c><c r="B3" t="inlineStr"><is><t>DROPPED-PRIVATE</t></is></c><c r="C3" t="inlineStr"><is><t></t></is></c></row></sheetData></worksheet>"#,
        ),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(
                xml.replace("<v>1</v>", &format!("<v>{}</v>", ids[0]))
                    .replace("<v>2</v>", &format!("<v>{}</v>", ids[1]))
                    .as_bytes(),
            )
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

async fn projection_source(
    router: &Router,
    metadata: &MetadataStore,
    store: &SessionStore,
    session: SessionId,
    engine: Engine,
) -> ConnectionId {
    if engine == Engine::Sqlite {
        let profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile{name:"projection SQLite".into(),provider_id:engine.provider_id(),semantic_engine:Some(engine),configuration:serde_json::json!({"root_id":"projection","path":"projection.db","mode":"read_write"}),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![]}).await.unwrap();
        let (status, bytes) = post(
            router,
            format!("/v1/sessions/{session}/connections/from-profile"),
            serde_json::json!({"tenant_id":1,"profile_id":profile.id.0}),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::OK, "managed source opens");
        serde_json::from_slice::<ConnectionInfo>(&bytes).unwrap().id
    } else {
        let pg = engine == Engine::Postgres;
        let prefix = if pg { "SIFT_PG" } else { "SIFT_MSSQL" };
        let get = |key: &str, default: &str| {
            std::env::var(format!("{prefix}_{key}")).unwrap_or_else(|_| default.into())
        };
        store
            .open_connection(
                session,
                engine,
                ConnectionSpec {
                    host: get(
                        "HOST",
                        if pg {
                            "/tmp/sift-demo-pg-socket"
                        } else {
                            "127.0.0.1"
                        },
                    ),
                    port: Some(
                        get("PORT", if pg { "5433" } else { "1433" })
                            .parse()
                            .unwrap(),
                    ),
                    database: Some(get("DB", if pg { "sifttest" } else { "master" })),
                    user: get("USER", if pg { "sift" } else { "sa" }),
                    password: std::env::var(format!("{prefix}_PASSWORD")).ok(),
                    ssl_mode: Some(if pg {
                        SslMode::Disable
                    } else {
                        SslMode::Require
                    }),
                    engine_specific: if pg {
                        None
                    } else {
                        Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
                            trust_server_certificate: Some(true),
                            ..Default::default()
                        }))
                    },
                },
            )
            .await
            .expect("native source opens")
            .id
    }
}

async fn query_native_receipts(
    router: &Router,
    metadata: &MetadataStore,
    store: &SessionStore,
    session: SessionId,
    engine: Engine,
    table: &str,
) -> Result<(), String> {
    for mode in ["returned", "counted", "zero", "nocount"] {
        if mode == "nocount" && engine != Engine::SqlServer {
            continue;
        }
        let connection = projection_source(router, metadata, store, session, engine).await;
        if mode == "nocount" {
            query(store, session, connection, "SET NOCOUNT ON".into()).await?;
        }
        let sql = if mode == "returned" {
            if engine == Engine::SqlServer {
                format!(
                    "UPDATE {table} SET label='native-http-returned' OUTPUT inserted.id WHERE id=1"
                )
            } else {
                format!("UPDATE {table} SET label='native-http-returned' WHERE id=1 RETURNING id")
            }
        } else {
            format!(
                "UPDATE {table} SET label='native-http-{mode}' WHERE id={}",
                if mode == "zero" { -1 } else { 1 }
            )
        };
        let correlation = format!("query-native-{engine:?}-{mode}");
        let response = router
            .clone()
            .oneshot(
                Request::post(format!("/v1/sessions/{session}/queries"))
                    .header("content-type", "application/json")
                    .header("x-correlation-id", correlation.as_str())
                    .body(Body::from(
                        serde_json::to_vec(
                            &serde_json::json!({"connection":connection,"sql":sql,"params":[]}),
                        )
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .map_err(|_| "native query request failed")?;
        require(
            response.status() == StatusCode::OK,
            "native HTTP query failed",
        )?;
        to_bytes(response.into_body(), 4096)
            .await
            .map_err(|_| "native query result could not be read")?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let ledger = metadata
                    .change_ledger(&ChangeLedgerFilter::default())
                    .map_err(|_| "native query ledger unavailable")?;
                if let Some(row) = ledger
                    .entries
                    .iter()
                    .find(|row| row.correlation_id.as_deref() == Some(correlation.as_str()))
                {
                    let uncertain =
                        engine == Engine::SqlServer && matches!(mode, "returned" | "nocount");
                    require(
                        row.outcome
                            == if uncertain {
                                ChangeLedgerOutcome::OutcomeUnknown
                            } else {
                                ChangeLedgerOutcome::Committed
                            },
                        "native query outcome lost transaction confidence",
                    )?;
                    let expected =
                        if mode == "returned" && engine != Engine::Sqlite || mode == "nocount" {
                            None
                        } else if mode == "zero" {
                            Some(0)
                        } else {
                            Some(1)
                        };
                    require(
                        row.row_count == expected,
                        "native query receipt substituted returned rows for affected counts",
                    )?;
                    require(
                        row.source_workflow == "direct_sql" && row.transaction_id.is_none(),
                        "native query source context changed",
                    )?;
                    return Ok::<_, String>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "native query receipt did not settle")??;
        store
            .close_connection(session, connection)
            .await
            .map_err(|_| "native query source cleanup failed")?;
    }
    Ok(())
}

async fn source_preview_acceptance(
    router: &Router,
    metadata: &MetadataStore,
    store: &SessionStore,
    recipe: &TransferRecipe,
    data: Vec<u8>,
) -> Result<(), String> {
    use sha2::Digest;
    let path = format!(
        "/v1/metadata/transfer-recipes/{}/preview-source",
        recipe.id.0
    );
    let mut request = PreviewTransferSourceRequest {
        expected_revision: recipe.revision,
        format_version: recipe.format_version.clone(),
        options: recipe.options.clone(),
        data,
        sheet: Some("Sheet1".into()),
    };
    let (status, body) = post(
        router,
        path.clone(),
        serde_json::to_value(&request).unwrap(),
    )
    .await?;
    require(status == StatusCode::OK, "source preview failed")?;
    let preview: TransferSourcePreview =
        serde_json::from_slice(&body).map_err(|_| "invalid source preview")?;
    require(
        preview.recipe_id == recipe.id && preview.recipe_revision == recipe.revision,
        "source preview lost recipe revision",
    )?;
    require(
        preview.source_digest == format!("{:x}", sha2::Sha256::digest(&request.data)),
        "source digest differs from upload",
    )?;
    require(
        preview
            .source_columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            == vec!["source_id", "discard", "raw_label"],
        "source headers changed",
    )?;
    require(preview.source_rows == 2, "source rows changed")?;
    if recipe.format_id == "parquet" {
        let mut large = request.clone();
        large.data = tabular_upload_with_discard("parquet", [1, 2], &"A".repeat(1100 * 1024));
        let value = serde_json::to_value(&large).unwrap();
        require(
            serde_json::to_vec(&value).unwrap().len() > 2 * 1024 * 1024,
            "large upload fixture must exceed Axum's default body limit",
        )?;
        let (status, body) = post(router, path.clone(), value).await?;
        require(
            status == StatusCode::OK && body.len() <= 1024 * 1024,
            "bounded source preview must accept uploads above the default JSON limit",
        )?;
        let large_preview: TransferSourcePreview =
            serde_json::from_slice(&body).map_err(|_| "invalid large source preview")?;
        require(
            matches!(large_preview.mapping, TransferMappedPreview::Ready { .. }),
            "dropped large field must not prevent mapped samples",
        )?;
    }

    match &preview.mapping {
        TransferMappedPreview::Ready {
            columns,
            sample_rows,
            representation,
            sample_rows_limited,
        } => {
            require(
                columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>() == vec!["label", "id"],
                "mapped schema order changed",
            )?;
            let id = if recipe.format_id == "xlsx" {
                Value::Text("1".into())
            } else {
                Value::Int64(1)
            };
            require(
                sample_rows.len() == 2
                    && sample_rows[0].values == vec![Value::Text("café".into()), id]
                    && sample_rows[1].values[0] == Value::Text("".into()),
                "mapped samples changed text/empty/typed values",
            )?;
            require(!sample_rows_limited, "small preview falsely limited")?;
            require(
                matches!(
                    (recipe.format_id.as_str(), representation),
                    (
                        "xlsx",
                        TransferSampleRepresentation::CsvTextBeforeDestinationInference
                    ) | ("parquet", TransferSampleRepresentation::TypedParquet)
                ),
                "sample representation changed",
            )?;
        }
        _ => return Err("ordinary projection unexpectedly unavailable".into()),
    }
    require(
        !String::from_utf8_lossy(&body).contains("DROPPED-PRIVATE"),
        "dropped data leaked through source preview",
    )?;
    request.options = serde_json::json!({"column_projection":[{"source":"source_id","target":"id"},{"source":"source_id","target":"copy"}]});
    let (status, body) = post(
        router,
        path.clone(),
        serde_json::to_value(&request).unwrap(),
    )
    .await?;
    require(status == StatusCode::OK, "source copy preview failed")?;
    let copied: TransferSourcePreview =
        serde_json::from_slice(&body).map_err(|_| "invalid copy preview")?;
    match copied.mapping {
        TransferMappedPreview::Ready { sample_rows, .. } => require(
            sample_rows[0].values[0] == sample_rows[0].values[1],
            "source copies changed values",
        )?,
        _ => return Err("copy unavailable".into()),
    }
    request.format_version = "3".into();
    request.options = serde_json::json!({"column_expressions":[
        {"target":"upper","expression":{"op":"upper","input":{"op":"column","source":"raw_label"}}},
        {"target":"literal","expression":{"op":"literal","value":"NULL"}},
        {"target":"nothing","expression":{"op":"null"}},
        {"target":"decimal","expression":{"op":"add","left":{"op":"decimal","input":{"op":"column","source":"source_id"}},"right":{"op":"decimal","input":{"op":"literal","value":"0.10"}}}}
    ]});
    if recipe.format_id == "xlsx" {
        request.options["null_value"] = serde_json::json!("__nil__");
    }
    let (status, body) = post(
        router,
        path.clone(),
        serde_json::to_value(&request).unwrap(),
    )
    .await?;
    require(status == StatusCode::OK, "expression source preview failed")?;
    let expressions: TransferSourcePreview =
        serde_json::from_slice(&body).map_err(|_| "invalid expression preview")?;
    match expressions.mapping {
        TransferMappedPreview::Ready { sample_rows, .. } => require(
            sample_rows[0].values
                == vec![
                    Value::Text("CAFÉ".into()),
                    Value::Text("NULL".into()),
                    Value::Null,
                    Value::Text("1.1".into()),
                ],
            "expressions/null marker/decimal samples differ from import",
        )?,
        _ => return Err("expression preview unavailable".into()),
    }
    request.options = serde_json::json!({"column_projection":[{"source":"missing","target":"id"}]});
    let (status, body) = post(
        router,
        path.clone(),
        serde_json::to_value(&request).unwrap(),
    )
    .await?;
    require(
        status == StatusCode::OK,
        "unresolvable mapping discarded source schema",
    )?;
    let unavailable: TransferSourcePreview =
        serde_json::from_slice(&body).map_err(|_| "invalid unavailable preview")?;
    require(
        unavailable.source_columns.len() == 3
            && matches!(
                unavailable.mapping,
                TransferMappedPreview::Unavailable { .. }
            ),
        "mapping failure did not preserve source schema",
    )?;
    request.expected_revision += 1;
    let (status, _) = post(
        router,
        path.clone(),
        serde_json::to_value(&request).unwrap(),
    )
    .await?;
    require(
        status.is_client_error(),
        "stale source revision was accepted",
    )?;
    request.expected_revision = recipe.revision;
    request.data = vec![1, 2, 3];
    let (status, _) = post(
        router,
        path.clone(),
        serde_json::to_value(&request).unwrap(),
    )
    .await?;
    require(status.is_client_error(), "malformed source was accepted")?;
    let outsider = metadata
        .create_principal(
            &format!("source-outsider-{}", recipe.id.0),
            "Source outsider",
            None,
        )
        .map_err(|e| e.to_string())?;
    let (_, token) = metadata
        .issue_api_token(outsider.id, None, "source preview denial", None)
        .map_err(|e| e.to_string())?;
    let denied = router
        .clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .map_err(|_| "source denial request failed")?;
    require(
        denied.status() == StatusCode::FORBIDDEN || denied.status() == StatusCode::NOT_FOUND,
        "source preview disclosed an unauthorized recipe",
    )?;
    let current = metadata
        .transfer_recipe_for_principal(recipe.id, PrincipalId(1), false)
        .map_err(|e| e.to_string())?;
    require(current == *recipe, "source review mutated the recipe")?;
    let operations = store.list_operations();
    require(operations.iter().any(|e|matches!(e.operation,Operation::PreviewTransferSource {recipe_id} if recipe_id==recipe.id) && e.status==OperationStatus::Succeeded),"source success audit missing")?;
    require(operations.iter().any(|e|matches!(e.operation,Operation::PreviewTransferSource {recipe_id} if recipe_id==recipe.id) && e.status==OperationStatus::Failed),"source refusal audit missing")?;
    let audit = serde_json::to_string(&operations).map_err(|_| "audit encoding failed")?;
    require(
        !audit.contains("DROPPED-PRIVATE") && !audit.contains("CAFÉ") && !audit.contains("__nil__"),
        "source data/options leaked into audit",
    )?;
    let update = serde_json::json!({"expected_revision":recipe.revision,"name":recipe.name,"direction":recipe.direction,"source":recipe.source,"sink":recipe.sink,"format_id":recipe.format_id,"format_version":recipe.format_version,"options":recipe.options});
    let send = || {
        Request::put(format!("/v1/metadata/transfer-recipes/{}", recipe.id.0))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&update).unwrap()))
            .unwrap()
    };
    let saved = router
        .clone()
        .oneshot(send())
        .await
        .map_err(|_| "recipe update failed")?;
    require(
        saved.status() == StatusCode::OK,
        "reviewed mapping recipe update was refused",
    )?;
    let saved = to_bytes(saved.into_body(), 1024 * 1024)
        .await
        .map_err(|_| "recipe response too large")?;
    let saved: TransferRecipe =
        serde_json::from_slice(&saved).map_err(|_| "invalid recipe update response")?;
    require(
        saved.revision == recipe.revision + 1 && saved.options == recipe.options,
        "recipe update changed unrelated options or revision",
    )?;
    let stale = router
        .clone()
        .oneshot(send())
        .await
        .map_err(|_| "stale update failed")?;
    require(
        stale.status().is_client_error(),
        "stale mapping update was accepted",
    )?;
    let updates=store.list_operations().into_iter().filter(|entry|matches!(entry.operation,Operation::TransferRecipe {recipe_id:Some(id),action:TransferRecipeAction::Update,..} if id==recipe.id)).collect::<Vec<_>>();
    require(
        updates.len() == 2
            && updates
                .iter()
                .filter(|entry| entry.status == OperationStatus::Succeeded)
                .count()
                == 1
            && updates
                .iter()
                .filter(|entry| entry.status == OperationStatus::Failed)
                .count()
                == 1,
        "recipe update terminal audits were lost or duplicated",
    )?;
    Ok(())
}

async fn acceptance(engine: Engine) {
    let root = tempfile::tempdir().unwrap();
    let registry = match engine {
        Engine::Postgres => DriverRegistry::builder()
            .register(sift_driver_postgres::PgDriver::new())
            .build(),
        Engine::SqlServer => DriverRegistry::builder()
            .register(sift_driver_sqlserver::MssqlDriver::new())
            .build(),
        Engine::Sqlite => {
            rusqlite::Connection::open(root.path().join("projection.db")).unwrap();
            DriverRegistry::builder()
                .register(sift_driver_sqlite::SqliteDriver::with_files(
                    sift_driver_sqlite::FilePolicy {
                        config: SqliteDriverConfig {
                            roots: std::collections::BTreeMap::from([(
                                "projection".into(),
                                SqliteRootConfig {
                                    path: root.path().display().to_string(),
                                    allowed_tenants: vec![1],
                                    read_only: false,
                                },
                            )]),
                            max_connections: 4,
                        },
                        protected: vec![],
                    },
                ))
                .build()
        }
    };
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("projection owner").unwrap();
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "projection".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    let workspace = metadata
        .create_workspace(room.id, PrincipalId(1), "projection")
        .unwrap();
    let store = SessionStore::new(registry);
    store.set_audit_store(metadata.clone());
    let session = store
        .open_session_with_owner(
            OpenSessionRequest {
                tag: Some("projection-acceptance".into()),
                tenant_id: Some(1),
            },
            Some(PrincipalId(1)),
            Some(TenantId(1)),
            false,
        )
        .unwrap()
        .id;
    let router = app(AppState {
        sessions: store.clone(),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: Some(metadata.clone()),
        shutdown: Shutdown::default(),
    });
    let connection = projection_source(&router, &metadata, &store, session, engine).await;
    let schema = match engine {
        Engine::Postgres => "public",
        Engine::SqlServer => "dbo",
        Engine::Sqlite => "main",
    };
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let target = format!("proj_{suffix}");
    let resumed = format!("proj_resume_{suffix}");
    let checkpoint = format!("proj_ck_{suffix}");
    let xlsx_target = format!("proj_xlsx_{suffix}");
    let parquet_target = format!("proj_parquet_{suffix}");
    let counted_target = format!("proj_counted_{suffix}");
    let atomic_target = format!("proj_atomic_{suffix}");
    let table = |name: &str| ObjectPath {
        catalog: None,
        schema: Some(schema.into()),
        name: name.into(),
        kind: Some(ObjectKind::Table),
        routine_args: None,
    };
    let projection = serde_json::json!([{"source":"raw_label","target":"label"},{"source":"source_id","target":"id"}]);
    let recipe_json = |options: serde_json::Value, version: &str| serde_json::json!({"name":"ordered projection","direction":"import","source":{"kind":"upload"},"sink":{"kind":"table"},"format_id":"csv","format_version":version,"options":options});
    let recipe_path = format!(
        "/v1/metadata/workspaces/{}/transfer-recipes",
        workspace.id.0
    );
    let outcome: Result<(), String> = async {
        let label_type = if engine == Engine::SqlServer {
            "nvarchar(256)"
        } else {
            "text"
        };
        for name in [&target, &resumed] {
            query(&store,session,connection,format!("CREATE TABLE {schema}.{name} (id bigint NOT NULL PRIMARY KEY,label {label_type} NULL)")).await?;
        }
        if engine==Engine::SqlServer {
            query(&store,session,connection,"SET NOCOUNT ON".into()).await?;
            let route=format!("/v1/sessions/{session}/connections/{connection}/import/csv");
            let (status,bytes)=post(&router,route.clone(),serde_json::json!({"table":format!("{schema}.{counted_target}"),"data":b"id,label\n1,one\n2,two\n".to_vec(),"create_table":true})).await?;
            require(status==StatusCode::OK,"NOCOUNT import failed")?;
            let counted:CsvImportResponse=serde_json::from_slice(&bytes).map_err(|_|"invalid NOCOUNT import response")?;
            require(counted.rows_inserted==2,"NOCOUNT import lost observed row counts")?;
            query(&store,session,connection,"SET ROWCOUNT 1".into()).await?;
            let (status,_)=post(&router,route.clone(),serde_json::json!({"table":format!("{schema}.{counted_target}"),"data":b"id,label\n3,three\n4,four\n".to_vec()})).await?;
            require(!status.is_success(),"ROWCOUNT-limited import claimed full success")?;
            query(&store,session,connection,"SET ROWCOUNT 0".into()).await?;
            let rows=query(&store,session,connection,format!("SELECT CAST(COUNT(*) AS bigint) FROM {schema}.{counted_target}")).await?;
            require(rows.rows[0].values==vec![Value::Int64(2)],"ROWCOUNT-limited import left partial rows")?;

            let mut data=String::from("id,label\n");for id in 1..=1000 {data.push_str(&format!("{id},ok\n"));}data.push_str("1001,NULL\n");
            let (status,_)=post(&router,route,serde_json::json!({"table":format!("{schema}.{atomic_target}"),"data":data.into_bytes(),"create_table":true})).await?;
            require(!status.is_success(),"late invalid SQL Server row was accepted")?;
            let rows=query(&store,session,connection,format!("SELECT CAST(COUNT(*) AS bigint) FROM sys.tables WHERE name='{atomic_target}'")).await?;
            require(rows.rows[0].values==vec![Value::Int64(0)],"failed SQL Server CSV left its created table")?;
            let affected=format!("{schema}.{atomic_target}");
            let changes=metadata.change_ledger(&ChangeLedgerFilter::default()).map_err(|_|"CSV ledger unavailable")?.entries.into_iter().filter(|entry|entry.affected_object.as_deref()==Some(affected.as_str())).collect::<Vec<_>>();
            require(changes.len()==2 && changes.iter().all(|entry|entry.outcome==ChangeLedgerOutcome::RolledBack && entry.transaction_id.is_some()),"SQL Server CSV rollback observations were not retained")?;
            query(&store,session,connection,"SET NOCOUNT OFF".into()).await?;
        }
        for options in [
            serde_json::json!({"column_projection":[]}),
            serde_json::json!({"column_projection":[{"source":"x","target":"a","sql":"unsafe"}]}),
            serde_json::json!({"column_projection":[{"source":"x","target":"a"}],"column_mappings":{}}),
        ] {
            let (status, _) = post(&router, recipe_path.clone(), recipe_json(options, "2")).await?;
            require(
                status == StatusCode::BAD_REQUEST,
                "invalid projection saved through HTTP",
            )?;
        }
        let options = serde_json::json!({"column_projection":projection});
        let (status, _) = post(
            &router,
            recipe_path.clone(),
            recipe_json(options.clone(), "1"),
        )
        .await?;
        require(
            status == StatusCode::BAD_REQUEST,
            "projection must require CSV version 2",
        )?;
        let (status, bytes) = post(&router, recipe_path.clone(), recipe_json(options, "2")).await?;
        require(
            status == StatusCode::OK,
            "projection recipe could not be created",
        )?;
        let recipe: TransferRecipe = serde_json::from_slice(&bytes).map_err(|_| "invalid recipe")?;
        let (status, _) = post(
            &router,
            format!("/v1/metadata/transfer-recipes/{}/validate", recipe.id.0),
            serde_json::json!({}),
        )
        .await?;
        require(
            status == StatusCode::OK,
            "saved projection did not validate",
        )?;
        let data="source_id,discard,raw_label\n1,DROPPED-PRIVATE-A,\"a,b\n\"\"café\"\"\"\n2,DROPPED-PRIVATE-B,NULL\n3,DROPPED-PRIVATE-C,\n".as_bytes().to_vec();
        let mut request = sift_metadata::http::ExecuteTransferRecipeRequest {
            session_id: session,
            connection_id: connection,
            sql: None,
            params: vec![],
            data: Some(data.clone()),
            table: Some(table(&target)),
            sheet: None,
            create_table: false,
            conflict_policy: None,
            dry_run: true,
            resume_from_row: 0,
            type_mappings: Default::default(),
                    sql_server: None,
        };
        let (status, bytes) = execute(&router, recipe.id, &request).await?;
        require(status == StatusCode::OK, "projection dry run failed")?;
        let preview = imported(&bytes)?;
        require(
            preview.dry_run && preview.rows_inserted == 0 && preview.rows_validated == 3,
            "dry-run counters changed",
        )?;
        require(
            preview
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>()
                == vec!["label", "id"],
            "preview did not use projected column order",
        )?;
        let rows = query(
            &store,
            session,
            connection,
            format!("SELECT id FROM {schema}.{target}"),
        )
        .await?;
        require(rows.rows.is_empty(), "dry run wrote rows")?;
        request.dry_run = false;
        let (status, bytes) = execute(&router, recipe.id, &request).await?;
        require(
            status == StatusCode::OK && imported(&bytes)?.rows_inserted == 3,
            "projected import failed",
        )?;
        let rows = query(
            &store,
            session,
            connection,
            format!("SELECT id,label FROM {schema}.{target} ORDER BY id"),
        )
        .await?;
        require(rows.rows.len() == 3, "target row count changed")?;
        require(
            rows.rows[0].values == vec![Value::Int64(1), Value::Text("a,b\n\"café\"".into())]
                && rows.rows[1].values[1].is_null()
                && rows.rows[2].values[1] == Value::Text("".into()),
            "quoted Unicode, NULL or empty values changed",
        )?;
        request.conflict_policy = Some(CsvConflictPolicy::Quarantine);
        request.data=Some(b"source_id,discard,raw_label\n1,DROPPED-QUARANTINE-PRIVATE,rejected\n4,DROPPED-OTHER-PRIVATE,kept\n".to_vec());
        let (status, bytes) = execute(&router, recipe.id, &request).await?;
        require(status == StatusCode::OK, "projected quarantine failed")?;
        let transfer: TransferExecutionResult =
            serde_json::from_slice(&bytes).map_err(|_| "invalid quarantine result")?;
        let artifact = match transfer {
            TransferExecutionResult::Import {
                result,
                quarantine_artifact: Some(artifact),
            } => {
                require(
                    result.rows_inserted == 1 && result.quarantined_rows.len() == 1,
                    "quarantine counters changed",
                )?;
                artifact
            }
            _ => return Err("quarantine artifact missing".into()),
        };
        let artifact = metadata
            .workspace_artifact_for_principal(artifact.id, PrincipalId(1))
            .map_err(|_| "quarantine artifact unavailable")?;
        require(
            !String::from_utf8_lossy(&artifact.content).contains("DROPPED-")
                && !String::from_utf8_lossy(&bytes).contains("DROPPED-"),
            "dropped source data entered quarantine",
        )?;
        let report: CsvQuarantineReport =
            serde_json::from_slice(&artifact.content).map_err(|_| "invalid quarantine artifact")?;
        require(
            report.columns == vec!["label", "id"],
            "quarantine columns did not follow projection",
        )?;
        let resume_options = serde_json::json!({"column_projection":projection,"durable_resume":{"checkpoint_table":format!("{schema}.{checkpoint}"),"run_id":uuid::Uuid::new_v4().to_string()}});
        let (status, bytes) = post(&router, recipe_path.clone(), {
            let mut recipe = recipe_json(resume_options, "2");
            recipe["name"] = serde_json::json!("durable ordered projection");
            recipe
        })
        .await?;
        require(
            status == StatusCode::OK,
            "durable projection recipe was rejected",
        )?;
        let resume: TransferRecipe =
            serde_json::from_slice(&bytes).map_err(|_| "invalid resume recipe")?;
        request.data = Some(data.clone());
        request.table = Some(table(&resumed));
        request.conflict_policy = Some(CsvConflictPolicy::Abort);
        let (status, bytes) = execute(&router, resume.id, &request).await?;
        require(
            status == StatusCode::OK && imported(&bytes)?.rows_inserted == 3,
            "durable projected import failed",
        )?;
        let ledger = metadata.change_ledger(&ChangeLedgerFilter::default()).map_err(|_| "checkpoint ledger unavailable")?.entries;
        require(ledger.iter().any(|entry| entry.workspace_id == Some(resume.workspace_id)
            && entry.source_workflow == "transfer_recipe_import"
            && entry.operation == ChangeLedgerOperation::DdlApply
            && entry.affected_object.as_deref() == Some(format!("{schema}.{checkpoint}").as_str())
            && entry.outcome == ChangeLedgerOutcome::Committed), "checkpoint DDL lost its actual target or workspace")?;
        let (status, bytes) = execute(&router, resume.id, &request).await?;
        require(
            status == StatusCode::OK && imported(&bytes)?.rows_inserted == 0,
            "durable projection replay duplicated rows",
        )?;
        request.data = Some(
            String::from_utf8(data)
                .unwrap()
                .replace("DROPPED-PRIVATE-A", "DROPPED-CHANGED")
                .into_bytes(),
        );
        let (status, bytes) = execute(&router, resume.id, &request).await?;
        require(
            status == StatusCode::CONFLICT,
            "changed dropped source field did not invalidate resume",
        )?;
        require(
            !String::from_utf8_lossy(&bytes).contains("DROPPED-CHANGED"),
            "source refusal exposed dropped data",
        )?;
        let rows = query(
            &store,
            session,
            connection,
            format!("SELECT id FROM {schema}.{resumed}"),
        )
        .await?;
        require(
            rows.rows.len() == 3,
            "changed-source refusal altered target rows",
        )?;
        for (format, destination) in [("xlsx", &xlsx_target), ("parquet", &parquet_target)] {
            if engine==Engine::SqlServer && format=="parquet" {query(&store,session,connection,"SET NOCOUNT ON".into()).await?;}
            query(&store,session,connection,format!("CREATE TABLE {schema}.{destination} (id bigint NOT NULL PRIMARY KEY,label {label_type} NULL)")).await?;
            let mut input = recipe_json(serde_json::json!({"column_projection":projection}), "2");
            input["name"] = serde_json::json!(format!("{format} projection"));
            input["format_id"] = serde_json::json!(format);
            input["options"]["sheet"] = serde_json::json!("Sheet1");
            let (status, bytes) = post(&router,recipe_path.clone(),input).await?;
            require(status == StatusCode::OK, &format!("{format} v2 recipe save failed: {status}"))?;
            let recipe: TransferRecipe = serde_json::from_slice(&bytes).map_err(|_| "invalid tabular recipe")?;
            source_preview_acceptance(&router,&metadata,&store,&recipe,tabular_upload(format,[1,2])).await?;
            request.data = Some(tabular_upload(format, [1, 2]));
            request.table = Some(table(destination));
            request.sheet = Some("Sheet1".into());
            request.dry_run = true;
            let (status, bytes) = execute(&router, recipe.id, &request).await?;
            require(status == StatusCode::OK, "tabular projection dry run failed")?;
            let preview = imported(&bytes)?;
            require(preview.rows_validated == 2 && preview.rows_inserted == 0 && preview.columns.iter().map(|c|c.name.as_str()).collect::<Vec<_>>() == vec!["label", "id"], "tabular projection preview changed")?;
            require(query(&store,session,connection,format!("SELECT id FROM {schema}.{destination}")).await?.rows.is_empty(), "tabular preview wrote rows")?;
            request.dry_run = false;
            let (status, bytes) = execute(&router, recipe.id, &request).await?;
            require(status == StatusCode::OK && imported(&bytes)?.rows_inserted == 2, "tabular projection apply failed")?;
            let ledger = metadata.change_ledger(&ChangeLedgerFilter::default()).map_err(|_| "tabular ledger unavailable")?.entries;
            require(ledger.iter().any(|entry| entry.workspace_id == Some(recipe.workspace_id)
                && entry.source_workflow == "transfer_recipe_import"
                && entry.operation == ChangeLedgerOperation::CsvImport
                && entry.affected_object.as_deref() == Some(format!("{schema}.{destination}").as_str())
                && entry.row_count == Some(2)
                && entry.transaction_id.is_some()
                && entry.outcome == ChangeLedgerOutcome::Committed), "tabular native commit lost observed count, transaction or workspace")?;
            request.data = Some(tabular_upload(format, [3, 1]));
            let (status, _) = execute(&router, recipe.id, &request).await?;
            require(!status.is_success(), &format!("{format} duplicate tabular import accepted"))?;
            let rows = query(&store,session,connection,format!("SELECT id,label FROM {schema}.{destination} ORDER BY id")).await?;
            require(rows.rows.len() == 2 && rows.rows[0].values == vec![Value::Int64(1),Value::Text("café".into())] && rows.rows[1].values == vec![Value::Int64(2),Value::Text("".into())], "tabular values or rollback changed")?;
            if engine == Engine::Sqlite {
                let mut alias = recipe_json(serde_json::json!({"column_projection":[{"source":"source_id","target":"id"},{"source":"source_id","target":"ID"}]}),"2");
                alias["name"] = serde_json::json!(format!("{format} alias refusal"));
                alias["format_id"] = serde_json::json!(format);
                let (status, bytes) = post(&router,recipe_path.clone(),alias).await?;
                require(status == StatusCode::OK,"alias fixture recipe failed")?;
                let recipe: TransferRecipe = serde_json::from_slice(&bytes).map_err(|_| "invalid alias recipe")?;
                request.dry_run = true;
                let (status, _) = execute(&router,recipe.id,&request).await?;
                require(status == StatusCode::BAD_REQUEST,"SQLite target alias accepted")?;
            }
            if engine==Engine::SqlServer && format=="parquet" {query(&store,session,connection,"SET NOCOUNT OFF".into()).await?;}
        }
        let export_connection=projection_source(&router,&metadata,&store,session,engine).await;
        let sql = if engine == Engine::SqlServer {
            format!("UPDATE {schema}.{parquet_target} SET label='exported-native' OUTPUT inserted.id WHERE id=1")
        } else {
            format!("UPDATE {schema}.{parquet_target} SET label='exported-native' WHERE id=1 RETURNING id")
        };
        let (status, _) = post(&router,format!("/v1/sessions/{session}/connections/{export_connection}/export"),
            serde_json::json!({"sql":sql,"params":[],"format":"csv","header":false})).await?;
        require(status == StatusCode::OK,"native write export failed")?;
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            loop {
                let rows=metadata.change_ledger(&ChangeLedgerFilter::default()).unwrap().entries;
                if let Some(entry)=rows.iter().find(|entry|entry.source_workflow=="direct_export") {
                    require(entry.outcome==(if engine==Engine::SqlServer {ChangeLedgerOutcome::OutcomeUnknown} else {ChangeLedgerOutcome::Committed}) && entry.row_count==(if engine==Engine::Sqlite {Some(1)} else {None})
                        && entry.transaction_id.is_none(),&format!("native export effect: {:?}, {:?}, {:?}",entry.outcome,entry.row_count,entry.operation))?;
                    return Ok::<_,String>(());
                }
                tokio::task::yield_now().await;
            }
        }).await.map_err(|_|"native export receipt did not settle")??;
        require(query(&store,session,export_connection,format!("SELECT id FROM {schema}.{parquet_target} WHERE label='exported-native'")).await?.rows.len()==1,
            "native export receipt does not match the database write")?;
        if engine != Engine::Sqlite {
            store.close_connection(session,export_connection).await.map_err(|_| "native output source cleanup failed")?;
            let export_connection=projection_source(&router,&metadata,&store,session,engine).await;
            let (status, _) = post(&router,format!("/v1/sessions/{session}/connections/{export_connection}/export"),
                serde_json::json!({"sql":format!("UPDATE {schema}.{parquet_target} SET label='exported-counted' WHERE id=1"),"params":[],"format":"csv","header":false})).await?;
            require(status==StatusCode::OK,"plain native write export failed")?;
            tokio::time::timeout(std::time::Duration::from_secs(5),async {
                loop {
                    let rows=metadata.change_ledger(&ChangeLedgerFilter::default()).unwrap().entries;
                    if rows.iter().any(|entry| entry.source_workflow=="direct_export" && entry.row_count==Some(1)
                        && entry.outcome==ChangeLedgerOutcome::Committed) {return;}
                    tokio::task::yield_now().await;
                }
            }).await.map_err(|_|"plain native DML lost its affected count")?;
            let (status, _) = post(&router,format!("/v1/sessions/{session}/connections/{export_connection}/export"),
                serde_json::json!({"sql":format!("UPDATE {schema}.{parquet_target} SET label='unchanged' WHERE id=-1"),"params":[],"format":"csv","header":false})).await?;
            require(status==StatusCode::OK,"zero-row native export failed")?;
            tokio::time::timeout(std::time::Duration::from_secs(5),async {
                loop {
                    let rows=metadata.change_ledger(&ChangeLedgerFilter::default()).unwrap().entries;
                    if rows.iter().any(|entry|entry.source_workflow=="direct_export" && entry.row_count==Some(0)
                        && entry.outcome==ChangeLedgerOutcome::Committed) {return;}
                    tokio::task::yield_now().await;
                }
            }).await.map_err(|_|"explicit native zero was not preserved")?;
            require(query(&store,session,connection,format!("SELECT id FROM {schema}.{parquet_target} WHERE label='exported-counted'")).await?.rows.len()==1,
                "counted export receipt did not match the committed database write")?;
            if engine == Engine::SqlServer {
            let before=metadata.change_ledger(&ChangeLedgerFilter::default()).map_err(|_|"native ledger unavailable")?.entries.iter().map(|row|row.id).max().unwrap();
            query(&store,session,export_connection,"SET NOCOUNT ON".into()).await?;
            let (status, _) = post(&router,format!("/v1/sessions/{session}/connections/{export_connection}/export"),
                serde_json::json!({"sql":format!("UPDATE {schema}.{parquet_target} SET label='exported-nocount' WHERE id=1"),"params":[],"format":"csv","header":false})).await?;
            require(status==StatusCode::OK,"NOCOUNT native export failed")?;
            tokio::time::timeout(std::time::Duration::from_secs(5),async {
                loop {
                    let rows=metadata.change_ledger(&ChangeLedgerFilter::default()).unwrap().entries;
                    if let Some(entry)=rows.iter().find(|entry|entry.id>before && entry.source_workflow=="direct_export") {
                        return require(entry.row_count.is_none() && entry.outcome==ChangeLedgerOutcome::OutcomeUnknown,
                            "NOCOUNT invented a zero count or a commit after raw SET");
                    }
                    tokio::task::yield_now().await;
                }
            }).await.map_err(|_|"NOCOUNT native receipt did not settle")??;
            require(query(&store,session,connection,format!("SELECT id FROM {schema}.{parquet_target} WHERE label='exported-nocount'")).await?.rows.len()==1,
                "NOCOUNT receipt did not match the actual database write")?;
            }
            store.close_connection(session,export_connection).await.map_err(|_| "counted export source cleanup failed")?;
        } else { store.close_connection(session,export_connection).await.map_err(|_| "native export source cleanup failed")?; }
        query_native_receipts(&router,&metadata,&store,session,engine,&format!("{schema}.{parquet_target}")).await?;
        text_expressions(&router, &store, session, connection, schema, &suffix, workspace.id).await?;
        if engine == Engine::SqlServer { generated_modes(&router, &store, session, connection, &suffix, workspace.id).await?; }
        let audit = serde_json::to_string(&store.list_operations())
            .map_err(|_| "audit serialization failed")?;
        require(!audit.contains("DROPPED-"), "source row data entered audit")?;
        Ok(())
    }
    .await;
    let mut cleanup_ok = true;
    for name in [
        &checkpoint,
        &target,
        &resumed,
        &xlsx_target,
        &parquet_target,
        &counted_target,
        &atomic_target,
    ] {
        if query(
            &store,
            session,
            connection,
            format!("DROP TABLE IF EXISTS {schema}.{name}"),
        )
        .await
        .is_err()
        {
            cleanup_ok = false;
        }
    }
    let _ = store.close_connection(session, connection).await;
    assert!(outcome.is_ok(), "{}", outcome.unwrap_err());
    assert!(cleanup_ok, "projection fixture cleanup failed");
}
#[tokio::test]
async fn sqlite_projection_and_resume_preserve_selected_data() {
    acceptance(Engine::Sqlite).await;
}
#[cfg(feature = "live-pg")]
#[tokio::test]
async fn postgres_projection_and_resume_preserve_selected_data() {
    acceptance(Engine::Postgres).await;
}
#[cfg(feature = "live-mssql")]
#[tokio::test]
async fn sqlserver_projection_and_resume_preserve_selected_data() {
    acceptance(Engine::SqlServer).await;
}

async fn generated_modes(
    router: &Router,
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    suffix: &str,
    workspace: WorkspaceId,
) -> Result<(), String> {
    let table = format!("dbo.generated_{suffix}");
    query(store,session,connection,format!("CREATE TABLE {table} (id int IDENTITY(1,1) PRIMARY KEY, label nvarchar(256) NOT NULL CHECK(label <> 'BAD'), calculated AS (id+1))")).await?;
    let result = async {
        query(store,session,connection,format!("SET ROWCOUNT 1; SET IDENTITY_INSERT {table} ON")).await?;
        let path = format!("/v1/sessions/{session}/connections/{connection}/import/csv");
        let mut request = serde_json::json!({"table":table,"data":"ID,label,calculated\n900,café,123\n901,second,124\n".as_bytes().to_vec(),"header":true,"delimiter":",","null_value":"NULL","create_table":false,"conflict_policy":"abort","dry_run":true,"resume_from_row":0,"type_mappings":{},"sql_server":{"run_id":uuid::Uuid::new_v4(),"mode":{"identity":"generate","computed":"omit"}}});
        let (status, bytes) = post(router,path.clone(),request.clone()).await?;
        if status != StatusCode::OK { return Err(format!("generated review failed: {status}: {}",String::from_utf8_lossy(&bytes))); }
        let review: CsvImportResponse = serde_json::from_slice(&bytes).map_err(|_| "review decoding failed")?;
        let review = review.sql_server_review.ok_or("missing native review")?;
        require(review.imported_columns == ["label"] && review.omitted_columns == ["id","calculated"],"native generated binding mismatch")?;
        request["dry_run"] = serde_json::json!(false);
        request["sql_server"]["run_id"] = serde_json::json!(uuid::Uuid::new_v4());
        request["sql_server"]["confirmation"] = serde_json::json!({"precondition":review.precondition,"confirmed":true});
        let mut changed=request.clone();changed["data"]=serde_json::json!(b"ID,label,calculated\n900,changed,123\n".to_vec());
        require(post(router,path.clone(),changed).await?.0==StatusCode::BAD_REQUEST,"changed input reused old review")?;
        query(store,session,connection,format!("ALTER TABLE {table} ADD review_marker int NULL")).await?;
        require(post(router,path.clone(),request.clone()).await?.0==StatusCode::BAD_REQUEST,"changed schema reused old review")?;
        request["dry_run"]=serde_json::json!(true);
        let (status,bytes)=post(router,path.clone(),request.clone()).await?;
        require(status==StatusCode::OK,"schema refresh review failed")?;
        let fresh:CsvImportResponse=serde_json::from_slice(&bytes).map_err(|_| "schema review decoding failed")?;
        request["sql_server"]["confirmation"]["precondition"]=serde_json::json!(fresh.sql_server_review.ok_or("missing schema review")?.precondition);
        request["dry_run"]=serde_json::json!(false);
        let (status, bytes) = post(router,path.clone(),request.clone()).await?;
        if status != StatusCode::OK { return Err(format!("generated write failed: {status}: {}",String::from_utf8_lossy(&bytes))); }
        let inserted: CsvImportResponse = serde_json::from_slice(&bytes).map_err(|_| "write decoding failed")?;
        require(inserted.rows_inserted == 2,"generated input count mismatch")?;
        let limited=query(store,session,connection,format!("SELECT id FROM {table}")).await?;
        require(limited.rows.len()==1,"disposable import changed editor ROWCOUNT")?;
        query(store,session,connection,"SET ROWCOUNT 0".into()).await?;
        query(store,session,connection,format!("INSERT INTO {table}(id,label) VALUES (700,'editor-state'); DELETE FROM {table} WHERE id=700; SET IDENTITY_INSERT {table} OFF")).await?;
        let rows = query(store,session,connection,format!("SELECT id,calculated FROM {table} ORDER BY id")).await?;
        require(rows.rows.len()==2 && rows.rows[0].values == [Value::Int32(1),Value::Int32(2)],"identity was not generated natively")?;
        request["sql_server"]["mode"]["identity"] = serde_json::json!("preserve");
        let (status, _) = post(router,path.clone(),request.clone()).await?;
        require(status==StatusCode::BAD_REQUEST,"changed modes accepted stale review")?;
        request["dry_run"]=serde_json::json!(true);
        let (status, bytes) = post(router,path.clone(),request.clone()).await?;
        require(status==StatusCode::OK,"preserve review failed")?;
        let review: CsvImportResponse=serde_json::from_slice(&bytes).map_err(|_| "preserve decoding failed")?;
        request["dry_run"]=serde_json::json!(false);
        request["sql_server"]["confirmation"]["precondition"]=serde_json::json!(review.sql_server_review.ok_or("missing preserve review")?.precondition);
        let (status, _) = post(router,path.clone(),request.clone()).await?;
        require(status==StatusCode::OK,"preserving explicit identities failed")?;
        let rows=query(store,session,connection,format!("SELECT id FROM {table} WHERE id>=900 ORDER BY id")).await?;
        require(rows.rows.len()==2 && rows.rows[0].values==[Value::Int32(900)],"explicit identity values changed")?;
        // A later batch constraint error rolls back earlier writes. Values stay out of error bodies.
        request["sql_server"]["mode"]["identity"]=serde_json::json!("generate");
        let mut data=String::from("id,label,calculated\n");
        for i in 0..130 {data.push_str(&format!("{i},{},0\n",if i==129 {"BAD"}else{"PRIVATE-IMPORT-VALUE"}));}
        request["data"]=serde_json::json!(data.into_bytes());request["dry_run"]=serde_json::json!(true);
        let (status,bytes)=post(router,path.clone(),request.clone()).await?;
        require(status==StatusCode::OK,"failure fixture review failed")?;
        let review: CsvImportResponse=serde_json::from_slice(&bytes).map_err(|_| "failure fixture decoding failed")?;
        request["dry_run"]=serde_json::json!(false);request["sql_server"]["confirmation"]["precondition"]=serde_json::json!(review.sql_server_review.ok_or("missing failure review")?.precondition);
        let (status,bytes)=post(router,path,request).await?;
        require(!status.is_success(),"late constraint violation succeeded")?;
        require(!String::from_utf8_lossy(&bytes).contains("PRIVATE-IMPORT-VALUE"),"row value leaked into response")?;
        let rows=query(store,session,connection,format!("SELECT COUNT(*) FROM {table}")).await?;
        require(rows.rows[0].values==[Value::Int32(4)],"late batch failure left partial rows")?;
        for format in ["csv","xlsx","parquet"] {
            let (status,bytes)=post(router,format!("/v1/metadata/workspaces/{}/transfer-recipes",workspace.0),serde_json::json!({"name":format!("generated {format} {suffix}"),"direction":"import","source":{"kind":"upload"},"sink":{"kind":"table"},"format_id":format,"format_version":"2","options":{"column_projection":[{"source":"source_id","target":"id"},{"source":"raw_label","target":"label"}],"sql_server_mode":{"identity":"generate","computed":"omit"}}})).await?;
            require(status==StatusCode::OK,"generated recipe save failed")?;
            let recipe:TransferRecipe=serde_json::from_slice(&bytes).map_err(|_| "generated recipe decoding failed")?;
            let mut request=sift_api_types::ExecuteTransferRecipeRequest {session_id:session,connection_id:connection,sql:None,params:vec![],data:Some(tabular_upload(format,[1000,1001])),table:Some(ObjectPath {catalog:None,schema:Some("dbo".into()),name:format!("generated_{suffix}"),kind:Some(ObjectKind::Table),routine_args:None}),sheet:Some("Sheet1".into()),create_table:false,conflict_policy:Some(CsvConflictPolicy::Abort),dry_run:true,resume_from_row:0,type_mappings:Default::default(),sql_server:Some(SqlServerImportRequest {run_id:uuid::Uuid::new_v4(),mode:SqlServerColumnMode {identity:SqlServerIdentityMode::Generate,computed:SqlServerComputedMode::Omit},confirmation:None})};
            let (status,bytes)=execute(router,recipe.id,&request).await?;
            if status!=StatusCode::OK {return Err(format!("{format} generated recipe preview failed: {status}: {}",String::from_utf8_lossy(&bytes)));}
            let preview=imported(&bytes)?;
            let token=preview.sql_server_review.ok_or("generated recipe review missing")?.precondition;
            request.dry_run=false;
            let control=request.sql_server.as_mut().unwrap();control.run_id=uuid::Uuid::new_v4();control.confirmation=Some(SqlServerImportConfirmation {precondition:token,confirmed:true});
            if format=="csv" {
                let mut changed=request.clone();
                for byte in changed.data.as_mut().unwrap() {if *byte==b'D' {*byte=b'X';}}
                require(execute(router,recipe.id,&changed).await?.0==StatusCode::BAD_REQUEST,"changed unselected upload data reused native review")?;
            }
            let (status,bytes)=execute(router,recipe.id,&request).await?;
            if status!=StatusCode::OK {return Err(format!("{format} generated recipe apply failed: {status}: {}",String::from_utf8_lossy(&bytes)));}
            require(imported(&bytes)?.rows_inserted==2,"generated recipe row count mismatch")?;
        }

        cancelled_generated_import(router,store,session,connection,&table).await?;
        Ok(())
    }.await;
    let cleanup = query(store, session, connection, format!("DROP TABLE {table}")).await;
    result?;
    require(cleanup.is_ok(), "generated fixture cleanup failed")
}

async fn cancelled_generated_import(
    router: &Router,
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    table: &str,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "fixture listener failed")?;
    let address = listener
        .local_addr()
        .map_err(|_| "fixture address failed")?;
    let app = router.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let client = sift_client_sdk::Client::new(format!("http://{address}"));
    let result=async {
        let mut request:CsvImportRequest=serde_json::from_value(serde_json::json!({"table":table,"data":b"id,label\n9,cancelled\n".to_vec(),"header":true,"delimiter":",","create_table":false,"conflict_policy":"abort","dry_run":true,"sql_server":{"run_id":uuid::Uuid::new_v4(),"mode":{"identity":"generate","computed":"omit"}}})).map_err(|_| "cancel request encoding failed")?;
        let preview=client.import_csv(session,connection,request.clone()).await.map_err(|_| "SDK generated preview failed")?;
        request.dry_run=false;
        let control=request.sql_server.as_mut().unwrap();
        control.run_id=uuid::Uuid::new_v4();
        control.confirmation=Some(SqlServerImportConfirmation {precondition:preview.sql_server_review.ok_or("SDK review missing")?.precondition,confirmed:true});
        let run=control.run_id;
        require(!client.cancel_import(session,connection,uuid::Uuid::new_v4()).await.map_err(|_| "inactive cancellation failed")?.cancel_requested,"inactive cancellation claimed an active request")?;
        let transaction=store.begin_transaction(session,BeginTransactionRequest {connection,mode:TxMode::default()}).await.map_err(|_| "lock transaction failed")?;
        let locked=store.execute_http(session,ExecuteRequestHttp {connection,sql:format!("UPDATE {table} WITH (TABLOCKX) SET label=label"),params:vec![],tx:Some(TxHandleRef {tx_id:transaction.tx_id,connection,mode:transaction.mode}),room_id:None,connection_profile_id:None,transform:None,source:None}).await;
        if locked.is_err() {let _=store.rollback_transaction(session,EndTransactionRequest {connection,tx_id:transaction.tx_id}).await;return Err("native lock fixture failed".into());}
        let worker=client.clone();
        let task=tokio::spawn(async move {worker.import_csv(session,connection,request).await});
        let observed=tokio::time::timeout(std::time::Duration::from_secs(10),async {
            loop {
                if let Some(progress)=client.import_progress(session,connection,run).await.map_err(|_| "SDK progress failed")? {
                    if progress.stage==ImportStage::Inserting {
                        require(progress.rows_committed==0,"attempted rows claimed commit")?;
                        break Ok::<_,String>(());
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }).await;
        let cancel=client.cancel_import(session,connection,run).await;
        let finished=tokio::time::timeout(std::time::Duration::from_secs(10),task).await;
        let rollback=store.rollback_transaction(session,EndTransactionRequest {connection,tx_id:transaction.tx_id}).await;
        require(rollback.is_ok(),"source lock rollback failed")?;
        observed.map_err(|_| "native progress never reached blocked insertion")??;
        require(cancel.map_err(|_| "SDK cancellation failed")?.cancel_requested,"SDK cancellation did not target original run")?;
        let outcome=finished.map_err(|_| "cancelled import did not finish promptly")?.map_err(|_| "cancelled task failed")?;
        require(outcome.is_err(),"cancelled native import reported success")?;
        let rows=query(store,session,connection,format!("SELECT COUNT(*) FROM {table} WHERE label='cancelled'")).await?;
        require(rows.rows[0].values==[Value::Int32(0)],"cancelled import left rows")?;
        Ok(())
    }.await;
    server.abort();
    result
}

async fn text_expressions(
    router: &Router,
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    schema: &str,
    suffix: &str,
    workspace: WorkspaceId,
) -> Result<(), String> {
    let engine = store
        .conn_entry(session, connection)
        .map_err(|_| "source unavailable")?
        .driver
        .engine();
    let text_type = if engine == Engine::SqlServer {
        "nvarchar(256)"
    } else {
        "text"
    };
    let names = [
        format!("expr_csv_{suffix}"),
        format!("expr_xlsx_{suffix}"),
        format!("expr_parquet_{suffix}"),
        format!("expr_resume_{suffix}"),
        format!("expr_ck_{suffix}"),
    ];
    let target = |name: &str| ObjectPath {
        catalog: None,
        schema: Some(schema.into()),
        name: name.into(),
        kind: Some(ObjectKind::Table),
        routine_args: None,
    };
    let expressions = serde_json::json!([
        {"target":"label","expression":{"op":"concat","inputs":[{"op":"upper","input":{"op":"trim","input":{"op":"column","source":"raw_label"}}},{"op":"literal","value":"!"}]}},
        {"target":"id","expression":{"op":"column","source":"source_id"}},
        {"target":"constant_text","expression":{"op":"literal","value":"NULL"}},
        {"target":"null_text","expression":{"op":"null"}},
        {"target":"empty_text","expression":{"op":"literal","value":""}},
        {"target":"numeric_text","expression":{"op":"literal","value":"001"}},
        {"target":"amount","expression":{"op":"divide","left":{"op":"add","left":{"op":"decimal","input":{"op":"column","source":"source_id"}},"right":{"op":"literal","value":"0.25"}},"right":{"op":"literal","value":"1"},"scale":2}}
    ]);
    let csv = "source_id,discard,raw_label\n1,DROPPED-PRIVATE, caféß \n2,DROPPED-PRIVATE,[nil]\n3,DROPPED-PRIVATE,\n".as_bytes().to_vec();
    let outcome: Result<(),String> = async {
        for (index, format) in ["csv", "xlsx", "parquet"].into_iter().enumerate() {
            let name = &names[index];
            if format != "parquet" {
                query(store,session,connection,format!("CREATE TABLE {schema}.{name} (id bigint NOT NULL PRIMARY KEY,label {text_type} NULL,constant_text {text_type} NULL,null_text {text_type} NULL,empty_text {text_type} NULL,numeric_text {text_type} NULL,amount decimal(38,18) NULL)")).await?;
            }
            let mut options=serde_json::json!({"column_expressions":expressions});
            if format != "parquet" { options["null_value"] = serde_json::json!("[nil]"); }
            let body=serde_json::json!({"name":format!("{format} text expressions"),"direction":"import","source":{"kind":"upload"},"sink":{"kind":"table"},"format_id":format,"format_version":"3","options":options});
            let (status,bytes)=post(router,format!("/v1/metadata/workspaces/{}/transfer-recipes",workspace.0),body.clone()).await?;
            require(status==StatusCode::OK,"expression recipe save failed")?;
            let recipe:TransferRecipe=serde_json::from_slice(&bytes).map_err(|_| "expression recipe decode failed")?;
            let mut request=sift_metadata::http::ExecuteTransferRecipeRequest {
                session_id:session,connection_id:connection,sql:None,params:vec![],
                data:Some(if format=="csv" { csv.clone() } else { tabular_upload(format,[1,2]) }),
                table:Some(target(name)),sheet:(format=="xlsx").then(|| "Sheet1".into()),
                create_table:format=="parquet",conflict_policy:None,dry_run:true,resume_from_row:0,
                type_mappings:Default::default(),sql_server:None,
            };
            let expected=if format=="csv" {3} else {2};
            let (status,bytes)=execute(router,recipe.id,&request).await?;
            require(status==StatusCode::OK,"expression dry run failed")?;
            let preview=imported(&bytes)?;
            require(preview.rows_validated==expected && preview.rows_inserted==0
                && preview.columns.iter().map(|c|c.name.as_str()).collect::<Vec<_>>()==["label","id","constant_text","null_text","empty_text","numeric_text","amount"],"expression preview shape changed")?;
            require(preview.columns[0].inferred_type==InferredCsvType::Text && preview.columns[1].inferred_type==InferredCsvType::Int64 && preview.columns[5].inferred_type==InferredCsvType::Text && preview.columns[6].inferred_type==InferredCsvType::Text,"expression type inference changed")?;
            if format!="parquet" {
                require(query(store,session,connection,format!("SELECT id FROM {schema}.{name}")).await?.rows.is_empty(),"expression preview wrote rows")?;
            }
            request.dry_run=false;
            let (status,bytes)=execute(router,recipe.id,&request).await?;
            if status!=StatusCode::OK { return Err(format!("{format} expression import failed: {status}: {}",String::from_utf8_lossy(&bytes))); }
            require(imported(&bytes)?.rows_inserted==expected,"expression import count changed")?;
            let rows=query(store,session,connection,format!("SELECT id,label,constant_text,null_text,empty_text,numeric_text FROM {schema}.{name} ORDER BY id")).await?;
            require(rows.rows.len()==expected as usize,"expression native row count changed")?;
            for (row_index,row) in rows.rows.iter().enumerate() {
                let label=if format=="csv" { match row_index {0=>Value::Text("CAFÉSS!".into()),1=>Value::Null,_=>Value::Text("!".into())} }
                    else if row_index==0 {Value::Text("CAFÉ!".into())} else {Value::Text("!".into())};
                require(row.values==vec![Value::Int64(row_index as i64+1),label,Value::Text("NULL".into()),Value::Null,Value::Text("".into()),Value::Text("001".into())],"expression Unicode/null/empty/native values changed")?;
            }
            let numeric=query(store,session,connection,format!("SELECT id FROM {schema}.{name} WHERE CAST(amount AS decimal(38,18)) = id + CAST(0.25 AS decimal(3,2))")).await?;
            require(numeric.rows.len()==expected as usize,"native decimal destination values changed")?;
            let mut invalid=body.clone();invalid["name"]=serde_json::json!(format!("{format} invalid expression source"));
            invalid["options"]["column_expressions"][0]["expression"]=serde_json::json!({"op":"column","source":"unknown"});
            let (status,bytes)=post(router,format!("/v1/metadata/workspaces/{}/transfer-recipes",workspace.0),invalid).await?;
            require(status==StatusCode::OK,"unknown-source fixture was not saved")?;
            let invalid:TransferRecipe=serde_json::from_slice(&bytes).map_err(|_| "invalid expression fixture response")?;
            let mut bad_request=request.clone();bad_request.create_table=false;
            let (status,bytes)=execute(router,invalid.id,&bad_request).await?;
            require(status==StatusCode::BAD_REQUEST && String::from_utf8_lossy(&bytes).contains("unknown source"),"unknown expression source was not refused before import")?;
            require(query(store,session,connection,format!("SELECT id FROM {schema}.{name}")).await?.rows.len()==expected as usize,"expression refusal altered rows")?;
            let mut arithmetic=body.clone();arithmetic["name"]=serde_json::json!(format!("{format} invalid decimal source"));
            arithmetic["options"]["column_expressions"][6]["expression"]=serde_json::json!({"op":"decimal","input":{"op":"column","source":"raw_label"}});
            let (status,bytes)=post(router,format!("/v1/metadata/workspaces/{}/transfer-recipes",workspace.0),arithmetic).await?;
            require(status==StatusCode::OK,"invalid decimal fixture was not saved")?;
            let arithmetic:TransferRecipe=serde_json::from_slice(&bytes).map_err(|_| "invalid decimal fixture response")?;
            let (status,bytes)=execute(router,arithmetic.id,&bad_request).await?;
            require(status==StatusCode::BAD_REQUEST && String::from_utf8_lossy(&bytes).contains("decimal expression")
                && !String::from_utf8_lossy(&bytes).contains("café"),"invalid decimal operand refusal leaked data or was not returned")?;
            require(query(store,session,connection,format!("SELECT id FROM {schema}.{name}")).await?.rows.len()==expected as usize,"invalid decimal operand altered rows")?;
            if format=="csv" {
                request.conflict_policy=Some(CsvConflictPolicy::Quarantine);
                request.data=Some(b"source_id,discard,raw_label\n1,DROPPED-QUARANTINE,rejected\n4,DROPPED-PRIVATE, kept \n".to_vec());
                let (status,bytes)=execute(router,recipe.id,&request).await?;
                require(status==StatusCode::OK,"expression quarantine failed")?;
                let result:TransferExecutionResult=serde_json::from_slice(&bytes).map_err(|_| "expression quarantine response invalid")?;
                let artifact=match result {TransferExecutionResult::Import {result,quarantine_artifact:Some(artifact)}=>{
                    require(result.rows_inserted==1 && result.quarantined_rows.len()==1,"expression quarantine count changed")?;artifact
                },_=>return Err("expression quarantine artifact missing".into())};
                let response=router.clone().oneshot(Request::get(format!("/v1/metadata/artifacts/{}",artifact.id.0))
                    .body(Body::empty()).unwrap()).await.map_err(|_| "artifact request failed")?;
                require(response.status()==StatusCode::OK,"expression quarantine artifact unavailable")?;
                let artifact=to_bytes(response.into_body(),1024*1024).await.map_err(|_| "artifact body failed")?;
                require(!String::from_utf8_lossy(&artifact).contains("DROPPED-"),"expression quarantine exposed dropped values")?;
                let report:CsvQuarantineReport=serde_json::from_slice(&artifact).map_err(|_| "expression quarantine artifact invalid")?;
                require(report.rows[0].values[0].as_deref()==Some("REJECTED!") && report.rows[0].values[2].as_deref()==Some("NULL")
                    && report.rows[0].values[3].is_none() && report.rows[0].values[4].as_deref()==Some("") && report.rows[0].values[5].as_deref()==Some("001") && report.rows[0].values[6].as_deref()==Some("1.25"),"quarantine lost computed/null/empty values")?;
                query(store,session,connection,format!("CREATE TABLE {schema}.{} (id bigint NOT NULL PRIMARY KEY,label {text_type} NULL,constant_text {text_type} NULL,null_text {text_type} NULL,empty_text {text_type} NULL,numeric_text {text_type} NULL,amount decimal(38,18) NULL)",names[3])).await?;
                let mut resume=body;resume["name"]=serde_json::json!("resumable text expressions");
                resume["options"]["durable_resume"]=serde_json::json!({"checkpoint_table":format!("{schema}.{}",names[4]),"run_id":uuid::Uuid::new_v4()});
                let (status,bytes)=post(router,format!("/v1/metadata/workspaces/{}/transfer-recipes",workspace.0),resume).await?;
                require(status==StatusCode::OK,"expression resume recipe rejected")?;
                let resume:TransferRecipe=serde_json::from_slice(&bytes).map_err(|_| "expression resume response invalid")?;
                request.table=Some(target(&names[3]));request.conflict_policy=Some(CsvConflictPolicy::Abort);request.data=Some(csv.clone());
                let (status,bytes)=execute(router,resume.id,&request).await?;
                require(status==StatusCode::OK && imported(&bytes)?.rows_inserted==3,"expression resume import failed")?;
                let (status,bytes)=execute(router,resume.id,&request).await?;
                require(status==StatusCode::OK && imported(&bytes)?.rows_inserted==0,"expression resume replay duplicated rows")?;
                let resumed=query(store,session,connection,format!("SELECT numeric_text FROM {schema}.{} WHERE amount = id + CAST(0.25 AS decimal(3,2)) ORDER BY id",names[3])).await?;
                require(resumed.rows.len()==3 && resumed.rows.iter().all(|row| row.values==vec![Value::Text("001".into())]),"resumed expressions lost leading zeros")?;
                request.data=Some(String::from_utf8(csv.clone()).unwrap().replace("DROPPED-PRIVATE","DROPPED-CHANGED").into_bytes());
                require(execute(router,resume.id,&request).await?.0==StatusCode::CONFLICT,"changed dropped expression source did not invalidate resume")?;
                require(query(store,session,connection,format!("SELECT id FROM {schema}.{}",names[3])).await?.rows.len()==3,"expression resume refusal changed rows")?;
            }
        }
        Ok(())
    }.await;
    let mut cleanup_ok = true;
    for name in &names {
        if query(
            store,
            session,
            connection,
            format!("DROP TABLE IF EXISTS {schema}.{name}"),
        )
        .await
        .is_err()
        {
            cleanup_ok = false;
        }
    }
    require(cleanup_ok, "expression cleanup failed")?;
    outcome
}
