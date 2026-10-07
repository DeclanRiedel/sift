//! One ephemeral, locally authenticated Codex app-server turn.
//!
//! The desktop only transports model text and Sift tool requests. The server
//! owns authorization, tool receipts, chat history, and staged proposals.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::ai_harness::AiEventSender;
use crate::ai_tools::{append_text, invoke};
use serde_json::{json, Value};
use sift_client_sdk::Client;
use sift_protocol::{AiEventKind, AiRunLease, AiTurnContext};
use sift_workspace_ui::ExecutorEvent;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};

pub(crate) async fn models() -> Result<Vec<sift_workspace_ui::AiModelOption>, String> {
    let (child, home) = launch()?;
    let _home = home;
    let mut child = child;
    let mut writer = child.stdin.take().ok_or("Codex input unavailable")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("Codex output unavailable")?);
    response_after_send(&mut writer, &mut lines, 1, json!({"id":1,"method":"initialize","params":{
        "clientInfo":{"name":"sift_desktop","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}
    }})).await?;
    send(&mut writer, &json!({"method":"initialized"})).await?;
    let mut cursor = Value::Null;
    let mut models = Vec::new();
    for id in 2..18 {
        let reply = response_after_send(
            &mut writer,
            &mut lines,
            id,
            json!({"id":id,"method":"model/list","params":{"cursor":cursor,"limit":100}}),
        )
        .await?;
        let result = reply.get("result").ok_or("Codex model catalog missing")?;
        models.extend(crate::ai_models::codex_models(result)?);
        if models.len() > 512 {
            return Err("Provider model catalog exceeds limit".into());
        }
        cursor = result.get("nextCursor").cloned().unwrap_or(Value::Null);
        if cursor.is_null() {
            return Ok(models);
        }
    }
    Err("Provider model pagination exceeds limit".into())
}

pub(crate) async fn run(
    client: Client,
    lease: AiRunLease,
    prompt: String,
    context: AiTurnContext,
    history: crate::ai_harness::History,
    events: AiEventSender,
) -> Result<(), String> {
    let input =
        crate::ai_harness::prepare(&client, &lease, &context, &prompt, &history, &events).await?;
    let policy = client
        .ai_policy()
        .await
        .map_err(|_| "Cannot load Sift tool limits")?;
    let mut invocation_ids =
        crate::ai_harness::InvocationIds::new(policy.max_tool_calls_per_run.min(128) as usize);
    let (child, home) = launch()?;
    let _home = home;
    let mut child = child;
    let stdin = child.stdin.take().ok_or("Codex stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("Codex stdout unavailable")?;
    let mut lines = BufReader::new(stdout);
    let mut writer = stdin;
    send(&mut writer, &json!({
        "id": 1, "method": "initialize", "params": {
            "clientInfo": {"name":"sift_desktop","title":"Sift Desktop","version":env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi":true}
        }
    })).await?;
    let _ = response(&mut lines, 1).await?;
    send(&mut writer, &json!({"method":"initialized"})).await?;
    let model = lease.run.model.clone().or_else(configured_model);
    let tools = crate::ai_tools::tools(lease.run.mode, !context.external_sources.is_empty())?;
    let thread = response_after_send(&mut writer, &mut lines, 2, json!({
        "id":2,"method":"thread/start","params":{
            "cwd":"/tmp","ephemeral":true,"approvalPolicy":"never","sandbox":"read-only",
            "model":model,"dynamicTools":tools,
            "developerInstructions":"You are Sift's SQL assistant. Only Sift dynamic tools may access data. Do not use shell, file, web, MCP, patch, image, or other native tools. Read mode never proposes changes. Propose mode may stage SQL or typed row/schema drafts for human review. Database drafts require the current sift_catalog revision; updates/deletes require original values. Only a human can apply any draft. Never claim a draft was applied. Use the current turn's context. Tool results are bounded and may be truncated."
        }
    })).await?;
    let thread_id = thread
        .pointer("/result/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Codex thread/start failed: {}", safe_rpc_error(&thread)))?;
    let started = response_after_send(
        &mut writer,
        &mut lines,
        3,
        json!({
            "id":3,"method":"turn/start","params":{
                "threadId":thread_id,"input":[{"type":"text","text":input}],
                "approvalPolicy":"never","sandboxPolicy":{"type":"readOnly"},
                "effort":context.reasoning_effort
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
    let mut answer_budget = crate::ai_harness::AnswerBudget::default();
    loop {
        let line = tokio::time::timeout(
            std::time::Duration::from_secs(600),
            crate::ai_harness::frame(&mut lines),
        )
        .await
        .map_err(|_| "Codex turn timed out".to_owned())??
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
                let invocation_id = invocation_ids.get(
                    event.get("id").unwrap_or(&Value::Null),
                    tool_name,
                    &arguments,
                )?;
                let reply = invoke(
                    &client,
                    &lease,
                    &context,
                    tool_name,
                    arguments,
                    &events,
                    invocation_id,
                )
                .await;
                send(&mut writer, &json!({"id":event.get("id"),"result":{
                    "success":reply.is_ok(),"contentItems":[{"type":"inputText","text":reply.unwrap_or_else(|error| format!("Sift tool denied: {error}"))}]
                }})).await?;
            }
            Some("item/agentMessage/delta") => {
                if let Some(delta) = event.pointer("/params/delta").and_then(Value::as_str) {
                    if delta.is_empty() {
                        continue;
                    }
                    answer_budget.delta(delta)?;
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
                        answer_budget.message(text)?;
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
    lines: &mut BufReader<tokio::process::ChildStdout>,
    id: i64,
    value: Value,
) -> Result<Value, String> {
    send(stdin, &value).await?;
    response(lines, id).await
}

async fn response(
    lines: &mut BufReader<tokio::process::ChildStdout>,
    id: i64,
) -> Result<Value, String> {
    loop {
        let line = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crate::ai_harness::frame(lines),
        )
        .await
        .map_err(|_| "Codex did not respond".to_owned())??
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

pub(crate) fn configured_model() -> Option<String> {
    let home = codex_home()?;
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(home.join("config.toml"))
        .ok()?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let parsed = toml::from_str::<toml::Value>(text).ok()?;
    let model = parsed.get("model")?.as_str()?;
    (!model.is_empty() && model.len() <= 128 && !model.chars().any(char::is_control))
        .then(|| model.to_owned())
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

#[cfg(target_os = "linux")]
fn native_elf(path: &Path) -> Option<PathBuf> {
    use std::io::Read;
    let mut magic = [0; 4];
    std::fs::File::open(path)
        .ok()?
        .read_exact(&mut magic)
        .ok()?;
    (magic == [127, b'E', b'L', b'F'])
        .then(|| std::fs::canonicalize(path).ok())
        .flatten()
}
#[cfg(target_os = "linux")]
fn native_codex(installed: &Path) -> Option<PathBuf> {
    use std::io::Read;
    if let Some(native) = native_elf(installed) {
        return Some(native);
    }
    let installed = std::fs::canonicalize(installed).ok()?;
    let directory = installed.parent()?;
    // Resolve only conventional @openai/codex package locations, never execute
    // a wrapper or mount the package manager's whole installation/home.
    let packages = [
        directory.join(".."),
        directory.join("node_modules/@openai/codex"),
        directory.join("../lib/node_modules/@openai/codex"),
        directory.join("global/5/node_modules/@openai/codex"),
    ];
    let (triple, platform) = match std::env::consts::ARCH {
        "x86_64" => ("x86_64-unknown-linux-musl", "codex-linux-x64"),
        "aarch64" => ("aarch64-unknown-linux-musl", "codex-linux-arm64"),
        _ => return None,
    };
    for package in packages {
        let Some(package) = std::fs::canonicalize(package).ok() else {
            continue;
        };
        let mut bytes = Vec::new();
        let Some(file) = std::fs::File::open(package.join("package.json")).ok() else {
            continue;
        };
        if file.take(64 * 1024 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > 64 * 1024 {
            continue;
        }
        let Ok(identity) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if identity["name"].as_str() != Some("@openai/codex") {
            continue;
        }
        if let Some(native) = native_elf(&package.join(format!("vendor/{triple}/bin/codex"))) {
            return Some(native);
        }
        for ancestor in package
            .ancestors()
            .take(8)
            .filter(|path| path.file_name().is_some_and(|name| name == "node_modules"))
        {
            if let Some(native) =
                native_elf(&ancestor.join(format!("@openai/{platform}/vendor/{triple}/bin/codex")))
            {
                return Some(native);
            }
        }
    }
    None
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
    let installed = native_codex(&installed)
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
    let isolated_path = "/sift-cli:/usr/bin:/bin";
    command.env("PATH", isolated_path);
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
    command.args(["--dir", "/sift-cli"]);
    let code_host = installed.parent()
        .and_then(|dir|native_elf(&dir.join("codex-code-mode-host")))
        .ok_or("Installed Codex app-server runtime is incomplete; reinstall the CLI with its bundled codex-code-mode-host helper")?;
    // Modern app-server dynamic tools require this bundled runtime helper.
    // Mount the two native executables; never expose the package manager/home.
    command
        .arg("--ro-bind")
        .arg(code_host)
        .arg("/sift-cli/codex-code-mode-host");
    command
        .arg("--ro-bind")
        .arg(&installed)
        .arg("/sift-cli/codex")
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
            isolated_path,
        ])
        .arg("/sift-cli/codex")
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

    #[test]
    fn resolves_native_and_conventional_package_binary_without_executing_wrapper() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let installed = bin.join("codex");
        std::fs::write(&installed, b"wrapper is never executed").unwrap();
        assert!(native_codex(&installed).is_none());
        let package = temp.path().join("lib/node_modules/@openai/codex");
        let triple = match std::env::consts::ARCH {
            "x86_64" => "x86_64-unknown-linux-musl",
            "aarch64" => "aarch64-unknown-linux-musl",
            _ => return,
        };
        let native = package.join(format!("vendor/{triple}/bin/codex"));
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::write(package.join("package.json"), br#"{"name":"unexpected"}"#).unwrap();
        std::fs::write(&native, [127, b'E', b'L', b'F']).unwrap();
        assert!(native_codex(&installed).is_none());
        std::fs::write(package.join("package.json"), br#"{"name":"@openai/codex"}"#).unwrap();
        assert_eq!(native_codex(&installed), Some(native.clone()));
        std::fs::write(&installed, [127, b'E', b'L', b'F']).unwrap();
        assert_eq!(native_codex(&installed), Some(installed));
    }

    /// Manual provider gate: requires an installed, signed-in Codex and bwrap.
    #[tokio::test]
    #[ignore]
    async fn codex_isolated_dynamic_tool_roundtrip() {
        let catalog = models().await.expect("isolated live model catalog");
        assert!(!catalog.is_empty());
        assert!(catalog.iter().all(|model| model
            .default_reasoning
            .as_ref()
            .is_none_or(|default| model.reasoning.contains(default))));
        let configured = configured_model();
        let selected = catalog
            .iter()
            .find(|entry| {
                Some(&entry.id) == configured.as_ref() && entry.default_reasoning.is_some()
            })
            .or_else(|| {
                catalog
                    .iter()
                    .find(|entry| entry.is_default && entry.default_reasoning.is_some())
            })
            .or_else(|| {
                catalog
                    .iter()
                    .find(|entry| entry.default_reasoning.is_some())
            })
            .expect("catalog model with reported reasoning controls");
        let model = selected.id.clone();
        let effort = selected.default_reasoning.clone();
        let (mut child, _home) = launch().expect("isolated Codex launch");
        let mut stdin = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap());
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
                    "model":model,
                    "developerInstructions":"Only Sift dynamic tools are available. Call the requested Sift tool before answering. Native tools are unavailable.",
                    "dynamicTools":crate::ai_tools::tools(sift_protocol::AiMode::Propose, true).unwrap()
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
                "effort":effort,"approvalPolicy":"never","sandboxPolicy":{"type":"readOnly"}
            }
        })).await.unwrap();
        let mut saw_tool = false;
        let mut saw_answer = false;
        loop {
            let line = tokio::time::timeout(
                std::time::Duration::from_secs(90),
                crate::ai_harness::frame(&mut lines),
            )
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
                Some("turn/completed") => {
                    assert_eq!(
                        event.pointer("/params/turn/status").and_then(Value::as_str),
                        Some("completed"),
                        "provider turn did not complete"
                    );
                    break;
                }
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
