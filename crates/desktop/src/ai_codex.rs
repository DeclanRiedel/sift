//! One ephemeral, locally authenticated Codex app-server turn.
//!
//! The desktop only transports model text and Sift tool requests. The server
//! owns authorization, tool receipts, chat history, and staged proposals.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{json, Value};
use sift_client_sdk::Client;
use sift_protocol::{
    AiEventKind, AiMode, AiRunLease, AiToolKind, AiTurnContext, AppendAiEventRequest,
    InvokeAiToolRequest, StageAiQueryProposalRequest,
};
use sift_workspace_ui::ExecutorEvent;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

pub(crate) async fn run(
    client: Client,
    lease: AiRunLease,
    prompt: String,
    context: AiTurnContext,
    history: Vec<(String, String)>,
    events: UnboundedSender<ExecutorEvent>,
) -> Result<(), String> {
    let (mut child, _home) = launch()?;
    let stdin = child.stdin.take().ok_or("Codex stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("Codex stdout unavailable")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut writer = stdin;
    send(&mut writer, &json!({
        "id": 1, "method": "initialize", "params": {
            "clientInfo": {"name":"sift_desktop","title":"Sift Desktop","version":env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi":true}
        }
    })).await?;
    let _ = response(&mut lines, 1).await?;
    send(&mut writer, &json!({"method":"initialized"})).await?;
    let model = configured_model();
    let mut tools = vec![
        tool("sift_schema", "Read the shallow schema of the current Sift connection", false),
        tool("sift_diagnostics", "Check SQL syntax in the current Sift dialect", true),
        tool("sift_explain", "Get an estimated plan for one SELECT; never ANALYZE", true),
        tool("sift_select", "Run one bounded Sift-restricted SELECT (up to 100 rows); SELECT functions may have side effects", true),
    ];
    if lease.run.mode == AiMode::Propose {
        tools.push(tool(
            "sift_stage_sql",
            "Stage a complete replacement SQL draft for human review; does not apply it",
            true,
        ));
    }
    let thread = response_after_send(&mut writer, &mut lines, 2, json!({
        "id":2,"method":"thread/start","params":{
            "cwd":"/tmp","ephemeral":true,"approvalPolicy":"never","sandbox":"read-only",
            "model":model,"dynamicTools":tools,
            "developerInstructions":"You are Sift's SQL assistant. Only Sift dynamic tools may access data. Do not use shell, file, web, MCP, patch, image, or other native tools. Read mode never proposes changes. Propose mode may only stage SQL for human review. Never claim a draft was applied. Use the current turn's context. Tool results are bounded and may be truncated."
        }
    })).await?;
    let thread_id = thread
        .pointer("/result/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Codex thread/start failed: {}", safe_rpc_error(&thread)))?;
    let mut input = String::new();
    if history.len() > 6 {
        input.push_str("Earlier chat turns were omitted.\n\n");
    }
    for (old_prompt, answer) in history.iter().rev().take(6).rev() {
        input.push_str("User: ");
        input.push_str(old_prompt);
        input.push_str("\nAssistant: ");
        input.push_str(answer);
        input.push_str("\n\n");
    }
    input.push_str("Current Sift context (fixed for this turn):\n");
    input.push_str(&serde_json::to_string(&context).map_err(|error| error.to_string())?);
    input.push_str("\n\nUser: ");
    input.push_str(&prompt);
    let started = response_after_send(
        &mut writer,
        &mut lines,
        3,
        json!({
            "id":3,"method":"turn/start","params":{
                "threadId":thread_id,"input":[{"type":"text","text":input}],
                "approvalPolicy":"never","sandboxPolicy":{"type":"readOnly"}
            }
        }),
    )
    .await?;
    if started.get("error").is_some() {
        return Err(format!(
            "Codex turn/start failed: {}",
            safe_rpc_error(&started)
        ));
    }
    let mut summary = String::new();
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(600), lines.next_line())
            .await
            .map_err(|_| "Codex turn timed out".to_owned())?
            .map_err(|error| error.to_string())?
            .ok_or("Codex exited before completing the turn")?;
        if line.len() > 1024 * 1024 {
            return Err("Codex event exceeded limit".into());
        }
        let event: Value =
            serde_json::from_str(&line).map_err(|_| "Codex sent invalid JSON".to_owned())?;
        match event.get("method").and_then(Value::as_str) {
            Some("item/tool/call") => {
                let tool_name = event
                    .pointer("/params/tool")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let arguments = event
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(Value::Null);
                let reply = invoke(&client, &lease, &context, tool_name, arguments, &events).await;
                send(&mut writer, &json!({"id":event.get("id"),"result":{
                    "success":reply.is_ok(),"contentItems":[{"type":"inputText","text":reply.unwrap_or_else(|error| format!("Sift tool denied: {error}"))}]
                }})).await?;
            }
            Some("item/agentMessage/delta") => {
                if let Some(delta) = event.pointer("/params/delta").and_then(Value::as_str) {
                    let _ = events.send(ExecutorEvent::AiTextDelta(delta.to_owned()));
                }
            }
            Some("item/reasoning/summaryTextDelta") => {
                if let Some(delta) = event.pointer("/params/delta").and_then(Value::as_str) {
                    if summary.len() + delta.len() <= 12 * 1024 {
                        summary.push_str(delta);
                    }
                }
            }
            Some("item/completed") => {
                if event.pointer("/params/item/type").and_then(Value::as_str)
                    == Some("agentMessage")
                {
                    let text = event
                        .pointer("/params/item/text")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let phase = event
                        .pointer("/params/item/phase")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !text.is_empty() {
                        let kind = if phase == "final_answer" {
                            AiEventKind::MessageCompleted
                        } else {
                            AiEventKind::ProgressSummary
                        };
                        append_text(&client, &lease, kind, text).await?;
                        let _ = events.send(ExecutorEvent::AiMessage {
                            kind,
                            text: text.to_owned(),
                        });
                    }
                }
            }
            Some("item/started") => {
                let kind = event
                    .pointer("/params/item/type")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if matches!(
                    kind,
                    "commandExecution" | "fileChange" | "mcpToolCall" | "webSearch" | "imageView"
                ) {
                    return Err(format!("Codex attempted a disabled native tool ({kind})"));
                }
            }
            Some("turn/completed") => {
                if !summary.is_empty() {
                    append_text(&client, &lease, AiEventKind::ProgressSummary, &summary).await?;
                    let _ = events.send(ExecutorEvent::AiMessage {
                        kind: AiEventKind::ProgressSummary,
                        text: summary,
                    });
                }
                if event.pointer("/params/turn/status").and_then(Value::as_str) != Some("completed")
                {
                    return Err("Codex turn failed".into());
                }
                break;
            }
            Some(method)
                if method.ends_with("requestApproval")
                    || method == "applyPatchApproval"
                    || method == "execCommandApproval" =>
            {
                return Err("Codex requested a native tool approval".into());
            }
            _ => {}
        }
    }
    let _ = child.kill().await;
    Ok(())
}

async fn invoke(
    client: &Client,
    lease: &AiRunLease,
    context: &AiTurnContext,
    name: &str,
    arguments: Value,
    events: &UnboundedSender<ExecutorEvent>,
) -> Result<String, String> {
    let sql = arguments
        .get("sql")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let _ = events.send(ExecutorEvent::AiToolActivity(format!("{name} running")));
    if name == "sift_stage_sql" {
        if lease.run.mode != AiMode::Propose {
            return Err("Propose mode required".into());
        }
        let proposed_sql = sql.ok_or("SQL draft is required")?;
        let base_revision = context
            .sql
            .as_ref()
            .and_then(|sql| sql.document_revision)
            .ok_or("Current SQL revision is unavailable")?;
        let detail = client
            .stage_ai_query_proposal(
                lease.run.id,
                &StageAiQueryProposalRequest {
                    client_request_id: Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    target: context.target.clone(),
                    base_revision,
                    proposed_sql,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let _ = events.send(ExecutorEvent::AiToolActivity(
            "SQL draft staged for review".into(),
        ));
        return Ok(format!(
            "Staged proposal {}. A person must review and apply it.",
            detail.proposal.id
        ));
    }
    let tool = match name {
        "sift_schema" => AiToolKind::Schema,
        "sift_diagnostics" => AiToolKind::Diagnostics,
        "sift_explain" => AiToolKind::Explain,
        "sift_select" => AiToolKind::Select,
        _ => return Err("Tool is not available in Sift".into()),
    };
    let response = client
        .invoke_ai_tool(
            lease.run.id,
            &InvokeAiToolRequest {
                call_id: Uuid::new_v4(),
                lease_token: lease.lease_token,
                tool,
                sql,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let _ = events.send(ExecutorEvent::AiToolActivity(format!("{name} completed")));
    serde_json::to_string(&response.result).map_err(|error| error.to_string())
}

async fn append_text(
    client: &Client,
    lease: &AiRunLease,
    kind: AiEventKind,
    text: &str,
) -> Result<(), String> {
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + 12 * 1024).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        client
            .append_ai_event(
                lease.run.id,
                &AppendAiEventRequest {
                    client_event_id: Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    kind,
                    content: json!({"text":&text[start..end]}),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        start = end;
    }
    Ok(())
}

fn tool(name: &str, description: &str, has_sql: bool) -> Value {
    let schema = if has_sql {
        json!({"type":"object","properties":{"sql":{"type":"string"}},"required":["sql"]})
    } else {
        json!({"type":"object","properties":{}})
    };
    json!({"type":"function","name":name,"description":description,"inputSchema":schema})
}

async fn send(stdin: &mut ChildStdin, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    stdin
        .write_all(&bytes)
        .await
        .map_err(|error| error.to_string())
}

async fn response_after_send(
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: i64,
    value: Value,
) -> Result<Value, String> {
    send(stdin, &value).await?;
    response(lines, id).await
}

async fn response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: i64,
) -> Result<Value, String> {
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), lines.next_line())
            .await
            .map_err(|_| "Codex did not respond".to_owned())?
            .map_err(|error| error.to_string())?
            .ok_or("Codex exited during startup")?;
        let value: Value =
            serde_json::from_str(&line).map_err(|_| "Codex sent invalid JSON".to_owned())?;
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            if value.get("error").is_some() {
                return Err(safe_rpc_error(&value));
            }
            return Ok(value);
        }
    }
}

fn safe_rpc_error(response: &Value) -> String {
    response
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("provider protocol error")
        .chars()
        .take(300)
        .collect()
}

fn configured_model() -> Option<String> {
    let home = codex_home()?;
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    toml::from_str::<toml::Value>(&text)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_owned)
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

#[cfg(target_os = "linux")]
fn launch() -> Result<(tokio::process::Child, tempfile::TempDir), String> {
    let source_home = codex_home().ok_or("Codex home is unavailable")?;
    let auth = source_home.join("auth.json");
    if !auth.is_file() {
        return Err("Sign in with the installed Codex CLI first".into());
    }
    let installed = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("codex"))
        .find(|path| path.is_file())
        .ok_or("Installed Codex CLI was not found")?;
    let installed = std::fs::canonicalize(installed).map_err(|error| error.to_string())?;
    let pnpm_root = installed
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "pnpm"))
        .ok_or("This Codex installation is not yet supported by Sift's isolated launcher")?;
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let isolated = temp.path().join("codex");
    std::fs::create_dir(&isolated).map_err(|error| error.to_string())?;
    let isolated_auth = isolated.join("auth.json");
    std::fs::copy(&auth, &isolated_auth).map_err(|error| error.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&isolated_auth, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    // Ubuntu's AppArmor policy can allow the distro binary while rejecting an
    // otherwise identical Nix-store binary's user namespace setup.
    let bwrap = if Path::new("/usr/bin/bwrap").is_file() {
        PathBuf::from("/usr/bin/bwrap")
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("bwrap"))
            .find(|path| path.is_file())
            .ok_or("Bubblewrap is required for isolated Codex chat")?
    };
    let mut command = Command::new(bwrap);
    // Do not expose SIFT_* connection passwords, tokens, or desktop secrets
    // through the provider process environment.
    command.env_clear().env("LANG", "C.UTF-8");
    let isolated_path = format!("/usr/bin:/bin:{}", pnpm_root.display());
    command.env("PATH", &isolated_path);
    command.args([
        "--die-with-parent",
        "--unshare-all",
        "--share-net",
        "--ro-bind",
        "/usr",
        "/usr",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--ro-bind",
        "/etc/ssl",
        "/etc/ssl",
        "--ro-bind",
        "/etc/resolv.conf",
        "/etc/resolv.conf",
        "--ro-bind",
        "/etc/hosts",
        "/etc/hosts",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/sift-home",
    ]);
    if Path::new("/nix").exists() {
        command.args(["--ro-bind", "/nix", "/nix"]);
    }
    command
        .arg("--bind")
        .arg(&isolated)
        .arg("/sift-home/.codex")
        .arg("--ro-bind")
        .arg(&isolated_auth)
        .arg("/sift-home/.codex/auth.json");
    let mut ancestors = pnpm_root.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for ancestor in ancestors
        .into_iter()
        .skip(1)
        .take_while(|path| *path != pnpm_root)
    {
        command.arg("--dir").arg(ancestor);
    }
    command
        .arg("--ro-bind")
        .arg(pnpm_root)
        .arg(pnpm_root)
        .args([
            "--chdir",
            "/tmp",
            "--setenv",
            "HOME",
            "/sift-home",
            "--setenv",
            "CODEX_HOME",
            "/sift-home/.codex",
            "--setenv",
            "PATH",
            &isolated_path,
        ])
        .arg(&installed)
        .args([
            "app-server",
            "--disable",
            "shell_tool",
            "--disable",
            "unified_exec",
            "--disable",
            "apps",
            "--disable",
            "multi_agent",
            "--disable",
            "hooks",
            "-c",
            "web_search=\"disabled\"",
            "-c",
            "history.persistence=\"none\"",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|error| format!("Cannot launch isolated Codex: {error}"))?;
    Ok((child, temp))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Manual provider gate: requires an installed, signed-in Codex and bwrap.
    #[tokio::test]
    #[ignore]
    async fn codex_isolated_dynamic_tool_roundtrip() {
        let (mut child, _home) = launch().expect("isolated Codex launch");
        let mut stdin = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        response_after_send(
            &mut stdin,
            &mut lines,
            1,
            json!({
                "id":1,"method":"initialize","params":{
                    "clientInfo":{"name":"sift_test","title":"Sift Test","version":"0.1"},
                    "capabilities":{"experimentalApi":true}
                }
            }),
        )
        .await
        .unwrap();
        send(&mut stdin, &json!({"method":"initialized"}))
            .await
            .unwrap();
        let opened = response_after_send(
            &mut stdin,
            &mut lines,
            2,
            json!({
                "id":2,"method":"thread/start","params":{
                    "cwd":"/tmp","ephemeral":true,"approvalPolicy":"never","sandbox":"read-only",
                    "dynamicTools":[tool("sift_diagnostics","Check SQL syntax",true)]
                }
            }),
        )
        .await
        .unwrap();
        let thread = opened
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .unwrap();
        response_after_send(&mut stdin, &mut lines, 3, json!({
            "id":3,"method":"turn/start","params":{
                "threadId":thread,"input":[{"type":"text","text":"Call sift_diagnostics with SELECT 1, then reply OK."}],
                "approvalPolicy":"never","sandboxPolicy":{"type":"readOnly"}
            }
        })).await.unwrap();
        let mut saw_tool = false;
        let mut saw_answer = false;
        loop {
            let line = tokio::time::timeout(std::time::Duration::from_secs(90), lines.next_line())
                .await
                .expect("Codex turn timeout")
                .unwrap()
                .expect("Codex exited");
            let event: Value = serde_json::from_str(&line).unwrap();
            match event.get("method").and_then(Value::as_str) {
                Some("item/tool/call") => {
                    assert_eq!(
                        event.pointer("/params/tool").and_then(Value::as_str),
                        Some("sift_diagnostics")
                    );
                    saw_tool = true;
                    send(&mut stdin, &json!({"id":event.get("id"),"result":{
                        "success":true,"contentItems":[{"type":"inputText","text":"{\"valid\":true}"}]
                    }})).await.unwrap();
                }
                Some("item/started") => {
                    let kind = event
                        .pointer("/params/item/type")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    assert!(
                        !matches!(kind, "commandExecution" | "fileChange" | "mcpToolCall"),
                        "native tool was offered: {kind}"
                    );
                }
                Some("item/completed")
                    if event.pointer("/params/item/type").and_then(Value::as_str)
                        == Some("agentMessage") =>
                {
                    if event.pointer("/params/item/phase").and_then(Value::as_str)
                        == Some("final_answer")
                    {
                        saw_answer = event
                            .pointer("/params/item/text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.contains("OK"));
                    }
                }
                Some("turn/completed") => break,
                _ => {}
            }
        }
        assert!(saw_tool);
        assert!(saw_answer);
        let _ = child.kill().await;
    }
}

#[cfg(not(target_os = "linux"))]
fn launch() -> Result<(tokio::process::Child, tempfile::TempDir), String> {
    Err("Isolated Codex launch is currently supported on Linux only".into())
}
