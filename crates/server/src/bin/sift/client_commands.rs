//! Human CLI workflows use the same audited SDK routes as the desktop.
use super::{read_token, McpOptions, MAX_FILE_BYTES};
use anyhow::{bail, Context};
use serde_json::{json, Value};
use sift_client_sdk::Client;
use sift_protocol::*;
use std::{collections::BTreeMap, io::Read, path::Path, time::Duration};

pub(super) async fn run(arguments: &[String]) -> anyhow::Result<()> {
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{}", HELP);
        return Ok(());
    }
    let options = Options::parse(arguments)?;
    let token = read_token(&options.remote.token_file)?;
    let client = Client::new(options.remote.server.clone()).with_bearer_token(token);
    let result = execute(&client, &options).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

const HELP: &str = "Client commands (JSON output):
  sift tools list [--mcp-only]
  sift tools call --tool <id> --arguments-file <json> [--approval-id <id>]
  sift query --sql-file <path> [--params-file <json-array>]
  sift performance benchmark --request-file <json> --confirm-workload [--save-name <name>]
  sift performance profile --request-file <json> --confirm-workload
  sift performance runs [--cursor <uuid>]
  sift performance get --run-id <uuid>
  sift performance compare --baseline-id <uuid> --candidate-id <uuid>
All commands: --server <url> --token-file <protected-file>.
Performance library commands require --tenant-id. Query/benchmark/profile require
--tenant-id and --profile-id; they open and close a managed session automatically.
Tool commands accept --tenant-id, --room-id, --profile-id, --connection-id, --document-id.
Workload confirmation is an explicit CLI flag; JSON cannot grant confirmation.
SQL/request files are bounded to 1 MiB. Bind values are never saved with benchmarks.
Ctrl-C cancels execution by closing the owned session. Approval-required tool calls
return the approval for a human authenticated Sift client; no approval is granted here.";

struct Options {
    action: String,
    remote: McpOptions,
    values: BTreeMap<String, String>,
    confirmed: bool,
    mcp_only: bool,
}

impl Options {
    fn parse(arguments: &[String]) -> anyhow::Result<Self> {
        let (action, offset) = match arguments.first().map(String::as_str) {
            Some("query") => ("query".to_owned(), 1),
            Some("tools" | "performance") => {
                let command = arguments
                    .get(1)
                    .context("subcommand required; use --help")?;
                (format!("{} {command}", arguments[0]), 2)
            }
            _ => bail!("unknown client command"),
        };
        let allowed: &[&str] = match action.as_str() {
            "tools list" => &[],
            "tools call" => &["--tool", "--arguments-file", "--approval-id"],
            "query" => &["--sql-file", "--params-file"],
            "performance benchmark" => &["--request-file", "--save-name"],
            "performance profile" => &["--request-file"],
            "performance runs" => &["--cursor"],
            "performance get" => &["--run-id"],
            "performance compare" => &["--baseline-id", "--candidate-id"],
            _ => bail!("unknown client subcommand; use --help"),
        };
        let mut common = Vec::new();
        let mut values = BTreeMap::new();
        let mut confirmed = false;
        let mut mcp_only = false;
        let mut index = offset;
        while index < arguments.len() {
            let key = &arguments[index];
            if key == "--confirm-workload"
                && matches!(
                    action.as_str(),
                    "performance benchmark" | "performance profile"
                )
            {
                if confirmed {
                    bail!("duplicate --confirm-workload");
                }
                confirmed = true;
                index += 1;
                continue;
            }
            if key == "--mcp-only" && action == "tools list" {
                if mcp_only {
                    bail!("duplicate --mcp-only");
                }
                mcp_only = true;
                index += 1;
                continue;
            }
            let value = arguments
                .get(index + 1)
                .with_context(|| format!("{key} requires a value"))?;
            if value.starts_with("--") {
                bail!("{key} requires a value");
            }
            if values.insert(key.clone(), value.clone()).is_some() {
                bail!("duplicate option {key}");
            }
            let common_scope = match action.as_str() {
                "tools list" | "tools call" => true,
                "query" | "performance benchmark" | "performance profile" => {
                    matches!(
                        key.as_str(),
                        "--server" | "--token-file" | "--tenant-id" | "--profile-id"
                    ) || allowed.contains(&key.as_str())
                }
                _ => {
                    matches!(key.as_str(), "--server" | "--token-file" | "--tenant-id")
                        || allowed.contains(&key.as_str())
                }
            };
            if !common_scope {
                bail!("{key} does not apply to this command");
            }
            if matches!(
                key.as_str(),
                "--server"
                    | "--token-file"
                    | "--tenant-id"
                    | "--room-id"
                    | "--profile-id"
                    | "--connection-id"
                    | "--document-id"
            ) {
                common.extend([key.clone(), value.clone()]);
            } else if !allowed.contains(&key.as_str()) {
                bail!("unsupported option {key}");
            }
            index += 2;
        }
        let options = Self {
            action,
            remote: McpOptions::parse(&common)?,
            values,
            confirmed,
            mcp_only,
        };
        if matches!(
            options.action.as_str(),
            "performance benchmark" | "performance profile"
        ) && !confirmed
        {
            bail!("--confirm-workload is required: measured execution may load the database or invoke side-effecting functions");
        }
        Ok(options)
    }
    fn required(&self, key: &str) -> anyhow::Result<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .with_context(|| format!("{key} is required"))
    }
    fn tenant(&self) -> anyhow::Result<sift_api_types::TenantId> {
        let id = self
            .remote
            .context
            .tenant_id
            .filter(|id| *id > 0)
            .context("positive --tenant-id required")?;
        Ok(sift_api_types::TenantId(id))
    }
    fn id(&self, key: &str) -> anyhow::Result<uuid::Uuid> {
        self.required(key)?
            .parse()
            .with_context(|| format!("invalid {key}"))
    }
}

fn read_file(path: &str) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(Path::new(path)).context("cannot open input file")?;
    if !file.metadata()?.is_file() {
        bail!("input must be a regular file");
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_FILE_BYTES {
        bail!("input must contain 1 byte to 1 MiB");
    }
    Ok(bytes)
}

async fn execute(client: &Client, options: &Options) -> anyhow::Result<Value> {
    match options.action.as_str() {
        "tools list" => Ok(serde_json::to_value(
            client
                .governed_tools(&options.remote.context, options.mcp_only)
                .await?,
        )?),
        "tools call" => {
            let arguments: Value =
                serde_json::from_slice(&read_file(options.required("--arguments-file")?)?)?;
            if !arguments.is_object() {
                bail!("tool arguments must be an object");
            }
            Ok(serde_json::to_value(
                client
                    .invoke_tool(&InvokeToolRequest {
                        tool_id: options.required("--tool")?.into(),
                        arguments,
                        context: options.remote.context.clone(),
                        approval_id: options.values.get("--approval-id").cloned(),
                    })
                    .await?,
            )?)
        }
        "performance runs" => Ok(serde_json::to_value(
            client
                .benchmark_runs(
                    options.tenant()?,
                    options
                        .values
                        .get("--cursor")
                        .map(|id| id.parse())
                        .transpose()?,
                )
                .await?,
        )?),
        "performance get" => Ok(serde_json::to_value(
            client
                .saved_benchmark_run(options.tenant()?, options.id("--run-id")?)
                .await?,
        )?),
        "performance compare" => {
            let base = client
                .saved_benchmark_run(options.tenant()?, options.id("--baseline-id")?)
                .await?;
            let current = client
                .saved_benchmark_run(options.tenant()?, options.id("--candidate-id")?)
                .await?;
            Ok(serde_json::to_value(
                sift_core::performance::compare_benchmarks(&base.report, &current.report),
            )?)
        }
        _ => execute_managed(client, options).await,
    }
}

async fn execute_managed(client: &Client, options: &Options) -> anyhow::Result<Value> {
    let tenant = options.tenant()?;
    let profile = options
        .remote
        .context
        .profile_id
        .filter(|id| *id > 0)
        .context("positive --profile-id required")?;
    // Validate/read inputs before opening any database connection.
    let mut input = if options.action == "query" {
        let sql = String::from_utf8(read_file(options.required("--sql-file")?)?)?;
        if sql.trim().is_empty() {
            bail!("SQL must not be blank");
        }
        let params: Vec<sift_protocol::Value> = options
            .values
            .get("--params-file")
            .map(|path| -> anyhow::Result<_> { Ok(serde_json::from_slice(&read_file(path)?)?) })
            .transpose()?
            .unwrap_or_default();
        json!({"sql":sql,"params":params})
    } else {
        serde_json::from_slice(&read_file(options.required("--request-file")?)?)?
    };
    if options.action != "query" {
        let object = input
            .as_object_mut()
            .context("request must be a JSON object")?;
        object.insert("workload_confirmed".into(), json!(options.confirmed));
        object
            .entry("run_id")
            .or_insert_with(|| json!(uuid::Uuid::new_v4()));
        if options.action == "performance profile" {
            object.insert("connection".into(), json!(0));
        }
    }
    let benchmark = if options.action == "performance benchmark" {
        let mut request: BenchmarkRequest = serde_json::from_value(input.clone())?;
        request.workload_confirmed = options.confirmed;
        Some(request)
    } else {
        None
    };
    let profile_request = if options.action == "performance profile" {
        let mut request: ProfileRequest = serde_json::from_value(input.clone())?;
        request.workload_confirmed = options.confirmed;
        Some(request)
    } else {
        None
    };
    let session = client
        .open_session_for_tenant(None, Some(tenant.0))
        .await?
        .id;
    let result = async {
        let connection = client
            .open_connection_from_profile(
                session,
                sift_api_types::OpenConnectionFromProfileRequest {
                    tenant_id: tenant.0,
                    profile_id: profile,
                },
            )
            .await?
            .id;
        if let Some(request) = benchmark {
            let report = client.benchmark(session, connection, request).await?;
            if let Some(name) = options.values.get("--save-name") {
                return Ok(serde_json::to_value(
                    client
                        .save_benchmark_run(
                            tenant,
                            &SaveBenchmarkRunRequest {
                                name: name.clone(),
                                report,
                            },
                        )
                        .await?,
                )?);
            }
            Ok(serde_json::to_value(report)?)
        } else if let Some(mut request) = profile_request {
            request.connection = connection;
            Ok(serde_json::to_value(
                client.profile(session, connection, request).await?,
            )?)
        } else {
            let params = serde_json::from_value(input["params"].clone())?;
            Ok(serde_json::to_value(
                client
                    .execute_with_params(
                        session,
                        connection,
                        input["sql"].as_str().context("SQL required")?,
                        params,
                    )
                    .await?,
            )?)
        }
    };
    let outcome: anyhow::Result<Value> = tokio::select! {
        result=result=>result,
        signal=tokio::signal::ctrl_c()=> match signal { Ok(())=>Err(anyhow::anyhow!("execution interrupted")),Err(error)=>Err(error.into()) },
    };
    let cleanup = tokio::time::timeout(Duration::from_secs(5), client.close_session(session)).await;
    match outcome {
        Err(error) => Err(error),
        Ok(value) => {
            cleanup.context("session cleanup timed out")??;
            Ok(value)
        }
    }
}

pub(super) fn performance_tools() -> Vec<Value> {
    [
        ("sift_benchmark_runs",vec![],"List owned private tenant benchmark snapshots; user-saved, no profile attestation."),
        ("sift_benchmark_run",vec!["run_id"],"Read owned private tenant benchmark measurements; no workload executed."),
        ("sift_benchmark_compare",vec!["baseline_id","candidate_id"],"Compare owned private tenant benchmark snapshots; descriptive client elapsed only, no workload executed."),
    ].into_iter().map(|(name,fields,description)| {
        let properties=fields.iter().map(|field|((*field).to_owned(),json!({"type":"string","format":"uuid"}))).collect::<serde_json::Map<_,_>>();
        json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":fields,"additionalProperties":false},"annotations":{"readOnlyHint":true}})
    }).collect()
}

pub(super) async fn saved_performance_tool(
    client: &Client,
    context: &ToolContext,
    name: &str,
    arguments: &Value,
) -> anyhow::Result<Value> {
    if context.room_id.is_some() {
        bail!("benchmark snapshots have no room-public publication contract");
    }
    let tenant = sift_api_types::TenantId(
        context
            .tenant_id
            .filter(|id| *id > 0)
            .context("tenant required")?,
    );
    let fields: &[&str] = match name {
        "sift_benchmark_runs" => &[],
        "sift_benchmark_run" => &["run_id"],
        "sift_benchmark_compare" => &["baseline_id", "candidate_id"],
        _ => bail!("unknown performance tool"),
    };
    let args = arguments
        .as_object()
        .context("arguments must be an object")?;
    if args.len() != fields.len() || args.keys().any(|key| !fields.contains(&key.as_str())) {
        bail!("invalid performance arguments");
    }
    let id = |key: &str| -> anyhow::Result<uuid::Uuid> {
        Ok(args
            .get(key)
            .and_then(Value::as_str)
            .context("UUID required")?
            .parse()?)
    };
    let value = match name {
        "sift_benchmark_runs" => {
            json!({"scope":"initiator_and_tenant","notice":"User-saved measurement snapshots, no profile provenance or server attestation.","page":client.benchmark_runs(tenant,None).await?})
        }
        "sift_benchmark_run" => {
            let saved = client.saved_benchmark_run(tenant, id("run_id")?).await?;
            json!({"id":saved.id,"saved_at":saved.saved_at,"name":saved.name,"engine":saved.report.engine,
                "statistics":sift_core::performance::benchmark_statistics(&saved.report),"completed":saved.report.completed,
                "scope":"initiator_and_tenant","timing":"server_side_client_elapsed_ns","notice":"User-saved snapshot; no profile provenance, SQL/binds/samples or workload execution."})
        }
        _ => {
            let base = client
                .saved_benchmark_run(tenant, id("baseline_id")?)
                .await?;
            let current = client
                .saved_benchmark_run(tenant, id("candidate_id")?)
                .await?;
            json!({"scope":"initiator_and_tenant","comparison":sift_core::performance::compare_benchmarks(&base.report,&current.report)})
        }
    };
    if serde_json::to_vec(&value)?.len() > 64 * 1024 {
        bail!("performance result exceeds tool limit");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(action: &[&str], url: &str) -> Vec<String> {
        action
            .iter()
            .map(|value| (*value).to_owned())
            .chain([
                "--server".into(),
                url.into(),
                "--token-file".into(),
                "unused".into(),
                "--tenant-id".into(),
                "1".into(),
                "--profile-id".into(),
                "1".into(),
            ])
            .collect()
    }
    #[test]
    fn execution_requires_cli_confirmation_and_bounded_inputs() {
        let base = args(
            &["performance", "benchmark", "--request-file", "request.json"],
            "http://127.0.0.1:3000",
        );
        assert!(Options::parse(&base).is_err());
        let mut confirmed = base.clone();
        confirmed.push("--confirm-workload".into());
        assert!(Options::parse(&confirmed).unwrap().confirmed);
        confirmed.extend(["--server".into(), "http://127.0.0.1:4000".into()]);
        assert!(Options::parse(&confirmed).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversized.json");
        std::fs::write(&path, vec![b'x'; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(read_file(path.to_str().unwrap()).is_err());
    }
    #[tokio::test]
    async fn managed_execution_uses_sdk_confirmation_and_always_closes_session() {
        use axum::{
            routing::{delete, post},
            Json, Router,
        };
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let opened = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let open = opened.clone();
        let close = closed.clone();
        let router = Router::new()
            .route(
                "/v1/sessions",
                post(move || {
                    let open = open.clone();
                    async move {
                        open.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"id":7,"created_at":"2026-10-07T00:00:00Z","connections":[]}))
                    }
                }),
            )
            .route(
                "/v1/sessions/7/connections/from-profile",
                post(|| async { Json(json!({"id":9,"provider_id":"sqlite","display_name":"fixture","created_at":"2026-10-07T00:00:00Z"})) }),
            )
            .route(
                "/v1/sessions/7/queries",
                post(|Json(request): Json<Value>| async move {
                    assert_eq!(request["connection"], 9);
                    assert_eq!(request["sql"], "SELECT 1");
                    (axum::http::StatusCode::FORBIDDEN, "denied")
                }),
            )
            .route(
                "/v1/sessions/7/connections/9/benchmark",
                post(|Json(request): Json<Value>| async move {
                    assert_eq!(request["workload_confirmed"], true);
                    assert!(request["run_id"].as_str().is_some());
                    (axum::http::StatusCode::FORBIDDEN, "denied")
                }),
            )
            .route(
                "/v1/sessions/7",
                delete(move || {
                    let close = close.clone();
                    async move {
                        close.fetch_add(1, Ordering::SeqCst);
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            );
        let router = router
            .fallback_service(sift_server::http::app(sift_server::http::AppState {
                sessions: sift_server::SessionStore::new(sift_server::DriverRegistry::new()),
                rooms: Default::default(),
                shutdown: Default::default(),
                auth: sift_server::http::AuthState::default(),
                metadata: None,
            }))
            .layer(axum::middleware::from_fn(
                |request: axum::extract::Request, next: axum::middleware::Next| async move {
                    let mut response = next.run(request).await;
                    response.headers_mut().insert(
                        "x-sift-protocol-version",
                        sift_protocol::PROTOCOL_VERSION_NUMBER
                            .to_string()
                            .parse()
                            .unwrap(),
                    );
                    response
                },
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = Client::new(&url);
        let dir = tempfile::tempdir().unwrap();
        let sql = dir.path().join("query.sql");
        std::fs::write(&sql, "SELECT 1").unwrap();
        let options =
            Options::parse(&args(&["query", "--sql-file", sql.to_str().unwrap()], &url)).unwrap();
        assert!(execute(&client, &options).await.is_err());
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        let request = dir.path().join("request.json");
        std::fs::write(&request,r#"{"sql":"SELECT 1","warmups":0,"iterations":1,"query_timeout_ms":100,"total_budget_ms":1000,"workload_confirmed":false}"#).unwrap();
        let options = Options::parse(&args(
            &[
                "performance",
                "benchmark",
                "--request-file",
                request.to_str().unwrap(),
                "--confirm-workload",
            ],
            &url,
        ))
        .unwrap();
        assert!(execute(&client, &options).await.is_err());
        assert_eq!(opened.load(Ordering::SeqCst), 2);
        assert_eq!(closed.load(Ordering::SeqCst), 2);
        let context = ToolContext {
            tenant_id: Some(1),
            room_id: Some(1),
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        assert!(
            saved_performance_tool(&client, &context, "sift_benchmark_runs", &json!({}))
                .await
                .is_err()
        );
        server.abort();
    }
    #[tokio::test]
    async fn sqlite_cli_queries_measures_saves_compares_and_profiles() {
        use sift_driver_sqlite::{FilePolicy, SqliteDriver};
        use sift_metadata::{
            CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId,
            TenantId,
        };
        use sift_server::{
            http::{app, AppState, AuthState},
            DriverRegistry, SessionStore,
        };
        use std::sync::Arc;
        let root = tempfile::tempdir().unwrap();
        rusqlite::Connection::open(root.path().join("cli.db")).unwrap();
        let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        metadata.bootstrap_local("CLI acceptance").unwrap();
        metadata
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "CLI fixture".into(),
                    provider_id: Engine::Sqlite.provider_id(),
                    semantic_engine: Some(Engine::Sqlite),
                    configuration: json!({"root_id":"fixture","path":"cli.db","mode":"read_write"}),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        let driver = SqliteDriver::with_files(FilePolicy {
            config: SqliteDriverConfig {
                roots: BTreeMap::from([(
                    "fixture".into(),
                    SqliteRootConfig {
                        path: root.path().display().to_string(),
                        allowed_tenants: vec![1],
                        read_only: false,
                    },
                )]),
                max_connections: 4,
            },
            protected: vec![],
        });
        let router = app(AppState {
            sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
            rooms: Default::default(),
            shutdown: Default::default(),
            auth: AuthState::default(),
            metadata: Some(metadata),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = Client::new(&url);
        let sql = root.path().join("query.sql");
        std::fs::write(&sql, "SELECT 7").unwrap();
        let options =
            Options::parse(&args(&["query", "--sql-file", sql.to_str().unwrap()], &url)).unwrap();
        let output: ExecuteResponse =
            serde_json::from_value(execute(&client, &options).await.unwrap()).unwrap();
        assert_eq!(output.rows[0].values, vec![sift_protocol::Value::Int64(7)]);
        let request = root.path().join("request.json");
        std::fs::write(&request,r#"{"sql":"SELECT 7","warmups":0,"iterations":2,"query_timeout_ms":1000,"total_budget_ms":5000}"#).unwrap();
        let options = Options::parse(&args(
            &[
                "performance",
                "benchmark",
                "--request-file",
                request.to_str().unwrap(),
                "--confirm-workload",
                "--save-name",
                "CLI measurement",
            ],
            &url,
        ))
        .unwrap();
        let saved: SavedBenchmarkRun =
            serde_json::from_value(execute(&client, &options).await.unwrap()).unwrap();
        assert!(saved.report.completed);
        let context = ToolContext {
            tenant_id: Some(1),
            room_id: None,
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        let measured = saved_performance_tool(
            &client,
            &context,
            "sift_benchmark_run",
            &json!({"run_id":saved.id}),
        )
        .await
        .unwrap();
        assert_eq!(measured["statistics"]["successful"], 2);
        assert!(measured.get("sql").is_none());
        let compared = saved_performance_tool(
            &client,
            &context,
            "sift_benchmark_compare",
            &json!({"baseline_id":saved.id,"candidate_id":saved.id}),
        )
        .await
        .unwrap();
        assert_eq!(compared["comparison"]["delta_percent"], 0.0);
        assert!(saved_performance_tool(
            &client,
            &context,
            "sift_benchmark_run",
            &json!({"run_id":saved.id,"sql":"SELECT 7"})
        )
        .await
        .is_err());
        std::fs::write(&request, r#"{"sql":"SELECT 7","timeout_ms":1000}"#).unwrap();
        let options = Options::parse(&args(
            &[
                "performance",
                "profile",
                "--request-file",
                request.to_str().unwrap(),
                "--confirm-workload",
            ],
            &url,
        ))
        .unwrap();
        let profiled: ProfileResponse =
            serde_json::from_value(execute(&client, &options).await.unwrap()).unwrap();
        assert_eq!(profiled.rows_returned, Some(1));
        assert!(
            client.list_sessions().await.unwrap().is_empty(),
            "CLI must close all owned sessions"
        );
        server.abort();
    }
}
