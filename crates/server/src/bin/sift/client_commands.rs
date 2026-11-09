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
    if options.action == "performance export" {
        let saved = client
            .saved_benchmark_run(options.tenant()?, options.id("--run-id")?)
            .await?;
        let output = match options.export_format()? {
            "csv" => sift_core::performance::artifact::samples_csv(&saved.report),
            "text" => sift_core::performance::artifact::readable_report(&saved.report),
            _ => serde_json::to_string_pretty(&sift_core::performance::artifact::export(
                &saved.report,
                options.redact_sql,
            ))?,
        };
        println!("{output}");
        return Ok(());
    }
    let result = execute(&client, &options).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

const HELP: &str = "Client commands (JSON, except selected export format):
  sift tools list [--mcp-only]
  sift tools call --tool <id> --arguments-file <json> [--approval-id <id>]
  sift query --sql-file <path> [--params-file <json-array>]
  sift performance benchmark --request-file <json> --confirm-workload [--save-name <name>]
  sift performance pair --request-file <json> --confirm-workload [--save-name <name>]
  sift performance suite --request-file <json> --confirm-workload [--save-name <prefix>] [--baselines-file <json>]
  sift performance profile --request-file <json> --confirm-workload
  sift performance export --run-id <uuid> [--format json|csv|text] [--redact-sql]
  sift performance import --artifact-file <json> --name <new-private-name>
  sift performance runs [--cursor <uuid>]
  sift performance get --run-id <uuid>
  sift performance compare --baseline-id <uuid> --candidate-id <uuid> [--max-regression-ms <ms>] [--max-regression-percent <percent>] [--plans]
Regression limits report observed median crossings with explicit uncertainty; they do not change exit status.
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
    redact_sql: bool,
    plans: bool,
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
            "performance benchmark" | "performance pair" => &["--request-file", "--save-name"],
            "performance suite" => &["--request-file", "--save-name", "--baselines-file"],
            "performance profile" => &["--request-file"],
            "performance export" => &["--run-id", "--format"],
            "performance import" => &["--artifact-file", "--name"],
            "performance runs" => &["--cursor"],
            "performance get" => &["--run-id"],
            "performance compare" => &[
                "--baseline-id",
                "--candidate-id",
                "--max-regression-ms",
                "--max-regression-percent",
            ],
            _ => bail!("unknown client subcommand; use --help"),
        };
        let mut common = Vec::new();
        let mut values = BTreeMap::new();
        let mut confirmed = false;
        let mut mcp_only = false;
        let mut redact_sql = false;
        let mut plans = false;
        let mut index = offset;
        while index < arguments.len() {
            let key = &arguments[index];
            if key == "--confirm-workload"
                && matches!(
                    action.as_str(),
                    "performance benchmark"
                        | "performance pair"
                        | "performance suite"
                        | "performance profile"
                )
            {
                if confirmed {
                    bail!("duplicate --confirm-workload");
                }
                confirmed = true;
                index += 1;
                continue;
            }
            if key == "--plans" && action == "performance compare" {
                if plans {
                    bail!("duplicate --plans");
                }
                plans = true;
                index += 1;
                continue;
            }
            if key == "--redact-sql" && action == "performance export" {
                if redact_sql {
                    bail!("duplicate --redact-sql");
                }
                redact_sql = true;
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
                "query"
                | "performance benchmark"
                | "performance pair"
                | "performance suite"
                | "performance profile" => {
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
            redact_sql,
            plans,
        };
        if matches!(
            options.action.as_str(),
            "performance benchmark"
                | "performance pair"
                | "performance suite"
                | "performance profile"
        ) && !confirmed
        {
            bail!("--confirm-workload is required: measured execution may load the database or invoke side-effecting functions");
        }
        options
            .regression_limits()?
            .validate()
            .map_err(anyhow::Error::msg)?;
        Ok(options)
    }
    fn export_format(&self) -> anyhow::Result<&str> {
        let format = self.values.get("--format").map_or("json", String::as_str);
        if !matches!(format, "json" | "csv" | "text") {
            bail!("--format must be json, csv or text");
        }
        Ok(format)
    }
    fn regression_limits(
        &self,
    ) -> anyhow::Result<sift_core::performance::regression::RegressionLimits> {
        let number = |key: &str| -> anyhow::Result<Option<f64>> {
            self.values
                .get(key)
                .map(|value| {
                    value
                        .parse::<f64>()
                        .with_context(|| format!("invalid {key}"))
                })
                .transpose()
        };
        Ok(sift_core::performance::regression::RegressionLimits {
            milliseconds: number("--max-regression-ms")?,
            percent: number("--max-regression-percent")?,
        })
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
        "performance import" => {
            let artifact: BenchmarkArtifact =
                serde_json::from_slice(&read_file(options.required("--artifact-file")?)?)?;
            let report = sift_core::performance::artifact::import(artifact, uuid::Uuid::new_v4())
                .map_err(anyhow::Error::msg)?;
            Ok(serde_json::to_value(
                client
                    .save_benchmark_run(
                        options.tenant()?,
                        &SaveBenchmarkRunRequest {
                            profile: None,
                            source_context: None,
                            name: options.required("--name")?.into(),
                            report,
                        },
                    )
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
            let comparison =
                sift_core::performance::compare_benchmarks(&base.report, &current.report);
            if options.plans {
                let plans = sift_core::performance::plan_comparison::compare(&base, &current)
                    .map_err(anyhow::Error::msg)?;
                let limits = options.regression_limits()?;
                let regression = if limits.supplied() {
                    Some(
                        sift_core::performance::regression::evaluate(&comparison, limits)
                            .map_err(anyhow::Error::msg)?,
                    )
                } else {
                    None
                };
                return Ok(
                    json!({"comparison":comparison,"plans":plans,"regression":regression,"notice":"Private plan identifiers/attributes included by explicit --plans; heuristic correspondences, no workload execution or controlled regression verdict."}),
                );
            }
            let limits = options.regression_limits()?;
            if limits.supplied() {
                let regression = sift_core::performance::regression::evaluate(&comparison, limits)
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"comparison": comparison, "regression": regression}))
            } else {
                Ok(serde_json::to_value(comparison)?)
            }
        }
        _ => execute_managed(client, options).await,
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteBaseline {
    query_id: String,
    case_id: String,
    run_id: uuid::Uuid,
}
async fn suite_baselines(
    client: &Client,
    options: &Options,
    request: &BenchmarkSuiteRequest,
) -> anyhow::Result<BTreeMap<(String, String), SavedBenchmarkRun>> {
    let Some(path) = options.values.get("--baselines-file") else {
        return Ok(BTreeMap::new());
    };
    let entries: Vec<SuiteBaseline> = serde_json::from_slice(&read_file(path)?)?;
    let expected: std::collections::BTreeSet<_> = request
        .queries
        .iter()
        .flat_map(|query| {
            query
                .cases
                .iter()
                .map(|case| (query.id.clone(), case.id.clone()))
        })
        .collect();
    let mut ids = BTreeMap::new();
    for entry in entries {
        let key = (entry.query_id, entry.case_id);
        if entry.run_id.is_nil()
            || !expected.contains(&key)
            || ids.insert(key, entry.run_id).is_some()
        {
            bail!(
                "Baseline mappings require unique known query/case IDs and non-nil saved run IDs"
            );
        }
    }
    if ids.len() != expected.len() {
        bail!("Baseline mappings must cover every suite case exactly once");
    }
    let mut baselines = BTreeMap::new();
    for (key, id) in ids {
        let saved = client.saved_benchmark_run(options.tenant()?, id).await?;
        if let Some(context) = &saved.report.suite_context {
            if context.query_id != key.0 || context.case_id != key.1 {
                bail!("Saved baseline suite identity differs from its mapping");
            }
        }
        baselines.insert(key, saved);
    }
    Ok(baselines)
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
    let paired = if options.action == "performance pair" {
        let request: BenchmarkPairRequest = serde_json::from_value(input.clone())?;
        request.limits.validate().map_err(anyhow::Error::msg)?;
        if let Some(check) = request.verify_results {
            check.validate().map_err(anyhow::Error::msg)?;
        }
        if request.run_id.is_nil() {
            bail!("paired run identity cannot be nil");
        }
        Some(request)
    } else {
        None
    };
    let suite = if options.action == "performance suite" {
        let request: BenchmarkSuiteRequest = serde_json::from_value(input.clone())?;
        sift_core::performance_suite::validate(&request).map_err(anyhow::Error::msg)?;
        if let Some(name) = options.values.get("--save-name") {
            if name.trim().is_empty() || name.len() > 60 {
                bail!("suite save prefix must be 1–60 bytes");
            }
        }
        Some(request)
    } else {
        None
    };
    let baselines = if let Some(request) = &suite {
        suite_baselines(client, options, request).await?
    } else {
        BTreeMap::new()
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
        if let Some(request) = suite {
            let report = client.benchmark_suite(session, connection, request).await?;
            let mut comparisons = Vec::new();
            let mut saved = Vec::new();
            for case in &report.cases {
                let context = case
                    .suite_context
                    .as_ref()
                    .context("Suite case context is missing")?;
                let key = (context.query_id.clone(), context.case_id.clone());
                if let Some(baseline) = baselines.get(&key) {
                    comparisons.push(json!({"query_id":key.0,"case_id":key.1,"baseline_id":baseline.id,
                        "comparison":sift_core::performance::compare_benchmarks(&baseline.report,case)}));
                }
                if let Some(prefix) = options.values.get("--save-name") {
                    let result = client
                        .save_benchmark_run(
                            tenant,
                            &SaveBenchmarkRunRequest {
                                profile: None,
                                source_context: None,
                                name: format!(
                                    "{prefix} {} / {}",
                                    context.query_id, context.case_id
                                ),
                                report: case.clone(),
                            },
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Saving suite case {}/{} failed; already saved mappings: {}",
                                context.query_id,
                                context.case_id,
                                serde_json::to_string(&saved).unwrap_or_default()
                            )
                        })?;
                    saved.push(json!({"query_id":key.0,"case_id":key.1,"run_id":result.id}));
                }
            }
            return Ok(json!({"suite":report,"saved":saved,"comparisons":comparisons}));
        }
        if let Some(request) = paired {
            let report = client.benchmark_pair(session, connection, request).await?;
            if let Some(name) = options.values.get("--save-name") {
                let baseline = client
                    .save_benchmark_run(
                        tenant,
                        &SaveBenchmarkRunRequest {
                            profile: None,
                            source_context: None,
                            name: format!("{name} A"),
                            report: report.baseline,
                        },
                    )
                    .await?;
                let candidate = client
                    .save_benchmark_run(
                        tenant,
                        &SaveBenchmarkRunRequest {
                            profile: None,
                            source_context: None,
                            name: format!("{name} B"),
                            report: report.candidate,
                        },
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Population A was saved as {}; saving population B failed",
                            baseline.id
                        )
                    })?;
                return Ok(json!({"baseline":baseline,"candidate":candidate}));
            }
            return Ok(serde_json::to_value(report)?);
        }
        if let Some(request) = benchmark {
            let report = client.benchmark(session, connection, request).await?;
            if let Some(name) = options.values.get("--save-name") {
                return Ok(serde_json::to_value(
                    client
                        .save_benchmark_run(
                            tenant,
                            &SaveBenchmarkRunRequest {
                                profile: None,
                                source_context: None,
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
            let page = client.benchmark_runs(tenant, None).await?;
            let items = page.items.iter().map(|item| json!({"id":item.id,"saved_at":item.saved_at,"name":item.name,"engine":item.engine,"payload_available":item.payload_available,"completed":item.completed,"median_ns":item.median_ns})).collect::<Vec<_>>();
            json!({"scope":"initiator_and_tenant","notice":"User-saved measurement snapshots, no profile provenance or server attestation.","page":{"items":items,"next_cursor":page.next_cursor}})
        }
        "sift_benchmark_run" => {
            let saved = client.saved_benchmark_run(tenant, id("run_id")?).await?;
            let mut value = json!({"id":saved.id,"saved_at":saved.saved_at,"name":saved.name,"engine":saved.report.engine,
                "statistics":sift_core::performance::benchmark_statistics(&saved.report),"completed":saved.report.completed,
                "scope":"initiator_and_tenant","timing":"server_side_client_elapsed_ns","notice":"User-saved snapshot; no profile provenance, SQL/binds/samples or workload execution."});
            if let Some(pair) = &saved.report.pair_context {
                value["pair_context"] = json!({"run_id":pair.run_id,"population":pair.population,"order":pair.order,"pair_completed":pair.pair_completed,"verification":pair.verification,"attempts":pair.actual_order.len()});
            }
            if let Ok(Some(summary)) = sift_core::performance::load::report_summary(&saved.report) {
                let context = saved.report.suite_context.as_ref().unwrap();
                let load = context.load.as_ref().unwrap();
                value["load_context"] = json!({"config":load.config,"suite_run_id":context.run_id,"case_index":context.case_index,"suite_completed":context.suite_completed,"attempts":load.traces.len(),"measured_window":load.measured_window});
                value["load_statistics"] = json!(summary);
            }

            value
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
    fn comparison_limits_are_validated_before_network_work() {
        let mut arguments = args(&["performance", "compare"], "http://127.0.0.1:3000");
        arguments.truncate(arguments.len() - 2); // No profile scope on saved reads.
        for value in ["NaN", "inf", "-1", "invalid"] {
            let mut invalid = arguments.clone();
            invalid.extend(["--max-regression-ms".into(), value.into()]);
            assert!(Options::parse(&invalid).is_err());
        }
        arguments.extend([
            "--max-regression-ms".into(),
            "0".into(),
            "--max-regression-percent".into(),
            "5".into(),
        ]);
        let options = Options::parse(&arguments).unwrap();
        assert_eq!(options.regression_limits().unwrap().percent, Some(5.0));
    }

    #[test]
    fn execution_requires_cli_confirmation_and_bounded_inputs() {
        let base = args(
            &["performance", "benchmark", "--request-file", "request.json"],
            "http://127.0.0.1:3000",
        );
        assert!(Options::parse(&base).is_err());
        let mut pair = base.clone();
        pair[1] = "pair".into();
        assert!(Options::parse(&pair).is_err());
        pair.push("--confirm-workload".into());
        assert!(Options::parse(&pair).unwrap().confirmed);
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
    #[cfg(unix)]
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
        std::fs::write(&request,r#"{"sql":"SELECT 7","warmups":0,"iterations":8,"query_timeout_ms":1000,"total_budget_ms":5000}"#).unwrap();
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
        assert!(saved.source_context.is_none());
        let mut copied = saved.report.clone();
        copied.run_id = uuid::Uuid::new_v4();
        let saved = client
            .save_benchmark_run(
                sift_api_types::TenantId(1),
                &SaveBenchmarkRunRequest {
                    profile: Some(serde_json::from_value(json!({"captured_at":chrono::Utc::now(),"sql":copied.sql,"parameter_count":0,"response":{"run_id":uuid::Uuid::new_v4(),"plan":{"engine":"sqlite","analyzed":false,"raw":"private-mcp-plan-sentinel","root":{"op":"SCAN"}},"server_elapsed_ns":100,"rows_returned":1}})).unwrap()),
                    name: "Context-bearing measurement".into(),
                    report: copied,
                    source_context: Some(sift_protocol::BenchmarkSourceContext {
                        captured_at: chrono::Utc::now(),
                        instance_id: "private-mcp-context-sentinel".into(),
                        workspace_id: sift_protocol::WorkspaceId(1),
                        workspace_revision: sift_protocol::WorkspaceRevision(2),
                        path: sift_protocol::WorkspacePath("queries/private-path.sql".into()),
                        git: None,
                    }),
                },
            )
            .await
            .unwrap();
        let artifact = root.path().join("measurement.json");
        let exported = sift_core::performance::artifact::export(&saved.report, true);
        assert!(exported.report.sql.is_empty());
        assert!(!serde_json::to_string(&exported)
            .unwrap()
            .contains("private-mcp-plan-sentinel"));
        assert!(!serde_json::to_string(&exported)
            .unwrap()
            .contains("private-mcp-context-sentinel"));
        assert_eq!(saved.report.sql, "SELECT 7");
        let mut malformed = exported.clone();
        malformed.artifact_version = 2;
        assert!(sift_core::performance::artifact::import(malformed, uuid::Uuid::new_v4()).is_err());
        let mut malformed = exported.clone();
        malformed.report.sql = "SELECT secret".into();
        assert!(sift_core::performance::artifact::import(malformed, uuid::Uuid::new_v4()).is_err());
        let mut full_warnings = exported.clone();
        full_warnings.report.warnings = vec!["existing warning".into(); 128];
        assert!(
            sift_core::performance::artifact::import(full_warnings, uuid::Uuid::new_v4()).is_err()
        );
        assert!(
            sift_core::performance::artifact::import(exported.clone(), exported.report.run_id)
                .is_err()
        );
        let mut raw = serde_json::to_value(&exported).unwrap();
        raw["owner_id"] = json!(1);
        assert!(serde_json::from_value::<BenchmarkArtifact>(raw).is_err());
        std::fs::write(&artifact, serde_json::to_vec(&exported).unwrap()).unwrap();
        let mut import_args = args(
            &[
                "performance",
                "import",
                "--artifact-file",
                artifact.to_str().unwrap(),
                "--name",
                "Imported measurement",
            ],
            &url,
        );
        import_args.truncate(import_args.len() - 2);
        let imported: SavedBenchmarkRun = serde_json::from_value(
            execute(&client, &Options::parse(&import_args).unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_ne!(imported.id, saved.id);
        assert_ne!(imported.report.run_id, saved.report.run_id);
        assert!(imported
            .report
            .warnings
            .iter()
            .any(|warning| warning.contains(&saved.report.run_id.to_string())
                && warning.contains("not a new execution")));
        assert_eq!(imported.name, "Imported measurement");
        assert!(imported.report.sql.is_empty());
        assert_eq!(imported.report.samples.len(), saved.report.samples.len());
        let mut report = saved.report.clone();
        report.samples[0].client_elapsed_ns = None;
        report.samples[0].elapsed_ns = 0;
        report.samples[0].database_execution_ns = None;
        report.samples[0].first_row_ns = None;
        report.samples[0].full_consumption_ns = None;
        report.samples[0].rows = None;
        let csv = sift_core::performance::artifact::samples_csv(&report);
        assert!(csv.lines().nth(1).unwrap().ends_with(",success,0,,,,"));
        assert!(!csv.contains("SELECT"));
        report.median_ns = Some(99999999.0);
        let text = sift_core::performance::artifact::readable_report(&report);
        assert!(!text.contains("99999999"));
        assert!(!text.contains("SELECT"));
        assert!(text.contains("Measured successful: 8"));

        client
            .update_benchmark_run_annotations(
                sift_api_types::TenantId(1),
                saved.id,
                &sift_protocol::UpdateBenchmarkRunAnnotationsRequest {
                    expected_revision: 0,
                    notes: "private-mcp-note-sentinel".into(),
                    tags: vec!["private-mcp-tag-sentinel".into()],
                },
            )
            .await
            .unwrap();
        let context = ToolContext {
            tenant_id: Some(1),
            room_id: None,
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        let listed = saved_performance_tool(&client, &context, "sift_benchmark_runs", &json!({}))
            .await
            .unwrap();
        let listed_json = serde_json::to_string(&listed).unwrap();
        assert!(
            !listed_json.contains("private-mcp-note-sentinel")
                && !listed_json.contains("private-mcp-tag-sentinel")
                && !listed_json.contains("private-mcp-context-sentinel")
                && !listed_json.contains("private-mcp-plan-sentinel")
        );
        assert!(listed["page"]["items"][0].get("tags").is_none());
        let measured = saved_performance_tool(
            &client,
            &context,
            "sift_benchmark_run",
            &json!({"run_id":saved.id}),
        )
        .await
        .unwrap();
        assert_eq!(measured["statistics"]["successful"], 8);
        assert!(measured.get("sql").is_none());
        assert!(!measured.to_string().contains("private-mcp-plan-sentinel"));
        assert!(!measured
            .to_string()
            .contains("private-mcp-context-sentinel"));
        let compared = saved_performance_tool(
            &client,
            &context,
            "sift_benchmark_compare",
            &json!({"baseline_id":saved.id,"candidate_id":saved.id}),
        )
        .await
        .unwrap();
        assert_eq!(compared["comparison"]["delta_percent"], 0.0);
        let model = &compared["comparison"]["median_uncertainty"];
        assert_eq!(model["baseline"]["samples"], 8);
        assert_eq!(model["joint_confidence_level"], 0.95);
        assert!(
            model["delta_ns"]["lower"].as_f64().unwrap() <= 0.0
                && model["delta_ns"]["upper"].as_f64().unwrap() >= 0.0
        );
        assert!(model["assumptions"]
            .as_str()
            .unwrap()
            .contains("not verified"));
        assert!(model.get("samples").is_none() && !model.to_string().contains("SELECT 7"));
        let saved_id = saved.id.to_string();
        let mut compare_args = args(
            &[
                "performance",
                "compare",
                "--baseline-id",
                &saved_id,
                "--candidate-id",
                &saved_id,
                "--max-regression-ms",
                "0",
                "--max-regression-percent",
                "0",
            ],
            &url,
        );
        compare_args.truncate(compare_args.len() - 2);
        let threshold_report = execute(&client, &Options::parse(&compare_args).unwrap())
            .await
            .unwrap();
        assert_eq!(threshold_report["regression"]["absolute"], "within_limit");
        assert_eq!(threshold_report["regression"]["relative"], "within_limit");
        assert_eq!(threshold_report["comparison"]["delta_ns"], 0.0);
        assert!(threshold_report.get("plans").is_none());
        compare_args.push("--plans".into());
        let with_plans = execute(&client, &Options::parse(&compare_args).unwrap())
            .await
            .unwrap();
        assert_eq!(with_plans["plans"]["unchanged"], 1);
        assert_eq!(with_plans["plans"]["rows"][0]["a"]["op"], "SCAN");
        assert!(with_plans["notice"].as_str().unwrap().contains("Private"));
        assert!(!with_plans.to_string().contains("private-mcp-plan-sentinel"));
        compare_args.push("--plans".into());
        assert!(Options::parse(&compare_args).is_err());

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
        std::fs::write(&request, r#"{"baseline":{"sql":"SELECT 1","params":[]},"candidate":{"sql":"SELECT 2 UNION ALL SELECT 3","params":[]},"limits":{"warmups":1,"iterations":2,"query_timeout_ms":1000,"total_budget_ms":5000,"delay_ms":0},"order":{"kind":"randomized_v1","seed":0}}"#).unwrap();
        let options = Options::parse(&args(
            &[
                "performance",
                "pair",
                "--request-file",
                request.to_str().unwrap(),
                "--confirm-workload",
                "--save-name",
                "CLI pair",
            ],
            &url,
        ))
        .unwrap();
        let paired = execute(&client, &options).await.unwrap();
        let a: SavedBenchmarkRun = serde_json::from_value(paired["baseline"].clone()).unwrap();
        let b: SavedBenchmarkRun = serde_json::from_value(paired["candidate"].clone()).unwrap();
        assert!(a.report.completed && b.report.completed);
        assert_eq!(a.name, "CLI pair A");
        assert_eq!(b.name, "CLI pair B");
        assert_eq!(a.report.samples[1].rows, Some(1));
        assert_eq!(b.report.samples[1].rows, Some(2));
        assert_eq!(
            a.report.pair_context.as_ref().unwrap().run_id,
            b.report.pair_context.as_ref().unwrap().run_id
        );
        let measurement = saved_performance_tool(
            &client,
            &context,
            "sift_benchmark_run",
            &json!({"run_id":b.id}),
        )
        .await
        .unwrap();
        assert_eq!(measurement["pair_context"]["pair_completed"], true);
        assert_eq!(measurement["pair_context"]["order"]["seed"], 0);
        assert!(measurement["pair_context"].get("actual_order").is_none());
        let mut verified: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&request).unwrap()).unwrap();
        verified["verify_results"] =
            json!({"order":"unordered","max_rows":10000,"max_bytes":4194304});
        std::fs::write(&request, serde_json::to_vec(&verified).unwrap()).unwrap();
        let result = execute(&client, &options).await.unwrap();
        let verified_b: SavedBenchmarkRun =
            serde_json::from_value(result["candidate"].clone()).unwrap();
        assert!(!verified_b.report.completed && verified_b.report.samples.is_empty());
        let measurement = saved_performance_tool(
            &client,
            &context,
            "sift_benchmark_run",
            &json!({"run_id":verified_b.id}),
        )
        .await
        .unwrap();
        assert_eq!(
            measurement["pair_context"]["verification"]["outcome"],
            "different"
        );
        assert!(measurement["pair_context"]["verification"]
            .get("fingerprint")
            .is_none());
        // Serialize real wire values rather than depending on Value's tag spelling.
        let suite_request = BenchmarkSuiteRequest {
            order: sift_protocol::BenchmarkSuiteOrder::Serial,
            run_id: uuid::Uuid::new_v4(),
            workload_confirmed: false,
            limits: BenchmarkLimits {
                warmups: 0,
                iterations: 1,
                query_timeout_ms: 1000,
                total_budget_ms: 5000,
                delay_ms: 0,
            },
            queries: vec![BenchmarkSuiteQuery {
                id: "query".into(),
                sql: "SELECT ?1".into(),
                cases: vec![
                    BenchmarkParameterCase {
                        limits: None,
                        id: "one".into(),
                        params: vec![sift_protocol::Value::Text("PRIVATE-CLI-SUITE".into())],
                    },
                    BenchmarkParameterCase {
                        limits: None,
                        id: "two".into(),
                        params: vec![sift_protocol::Value::Int64(2)],
                    },
                ],
            }],
        };
        let mut suite_input = serde_json::to_value(&suite_request).unwrap();
        suite_input.as_object_mut().unwrap().remove("run_id");
        std::fs::write(&request, serde_json::to_vec(&suite_input).unwrap()).unwrap();
        let suite_args = args(
            &[
                "performance",
                "suite",
                "--request-file",
                request.to_str().unwrap(),
                "--confirm-workload",
                "--save-name",
                "CLI suite",
            ],
            &url,
        );
        let options = Options::parse(&suite_args).unwrap();
        let measured = execute(&client, &options).await.unwrap();
        assert_eq!(measured["suite"]["completed"], true);
        assert_eq!(measured["saved"].as_array().unwrap().len(), 2);
        assert!(!measured.to_string().contains("PRIVATE-CLI-SUITE"));
        let baseline_path = root.path().join("baselines.json");
        std::fs::write(
            &baseline_path,
            serde_json::to_vec(&measured["saved"]).unwrap(),
        )
        .unwrap();
        let mut comparison_args = suite_args.clone();
        comparison_args.extend([
            "--baselines-file".into(),
            baseline_path.display().to_string(),
        ]);
        let options = Options::parse(&comparison_args).unwrap();
        let compared = execute(&client, &options).await.unwrap();
        assert_eq!(compared["comparisons"].as_array().unwrap().len(), 2);
        assert!(compared["comparisons"]
            .as_array()
            .unwrap()
            .iter()
            .all(|case| case["comparison"]["delta_ns"].is_number()));
        let mut invalid = measured["saved"].clone();
        invalid[0]["case_id"] = json!("missing");
        std::fs::write(&baseline_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(execute(&client, &options).await.is_err());
        // A complete mapping cannot relabel an existing suite baseline.
        let mut swapped = measured["saved"].clone();
        let first = swapped[0]["run_id"].clone();
        swapped[0]["run_id"] = swapped[1]["run_id"].clone();
        swapped[1]["run_id"] = first;
        std::fs::write(&baseline_path, serde_json::to_vec(&swapped).unwrap()).unwrap();
        assert!(execute(&client, &options).await.is_err());
        assert!(
            client.list_sessions().await.unwrap().is_empty(),
            "CLI must close all owned sessions"
        );
        server.abort();
    }
}
