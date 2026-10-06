//! Isolated native Claude Code/OpenCode processes with Sift-only MCP tools.
use serde_json::{json, Value};
use sift_client_sdk::Client;
use sift_protocol::{AiEventKind, AiProvider, AiRunLease, AiTurnContext};
use sift_workspace_ui::ExecutorEvent;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, Command},
};

pub(crate) async fn run(
    client: Client,
    lease: AiRunLease,
    prompt: String,
    context: AiTurnContext,
    history: crate::ai_harness::History,
    events: crate::ai_harness::AiEventSender,
) -> Result<(), String> {
    let input =
        crate::ai_harness::prepare(&client, &lease, &context, &prompt, &history, &events).await?;
    let bridge = crate::ai_mcp_bridge::Bridge::start(
        client.clone(),
        lease.clone(),
        context.clone(),
        events.clone(),
    )
    .await?;
    let selected_model = lease
        .run
        .model
        .clone()
        .or_else(|| configured_model(lease.run.provider));
    let (child, home) = launch(lease.run.provider, selected_model.as_deref(), &bridge)?;
    // Drop the child before reconciling its private authentication/home.
    let _home = home;
    let mut child = child;
    let mut stdin = child.stdin.take().ok_or("Provider input is unavailable")?;
    stdin
        .write_all(input.as_bytes())
        .await
        .map_err(|_| "Provider input failed")?;
    stdin
        .shutdown()
        .await
        .map_err(|_| "Provider input failed")?;
    drop(stdin);
    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .ok_or("Provider output is unavailable")?,
    );
    let mut publisher = crate::ai_harness::TextPublisher::default();
    let mut answer = String::new();
    let mut completed = false;
    let mut tools_seen = false;
    loop {
        let Some(frame) = crate::ai_harness::frame(&mut stdout).await? else {
            break;
        };
        let event: Value =
            serde_json::from_str(&frame).map_err(|_| "Provider sent an invalid event")?;
        match lease.run.provider {
            AiProvider::ClaudeCode => match event.get("type").and_then(Value::as_str) {
                Some("system") if event.get("subtype").and_then(Value::as_str) == Some("init") => {
                    let Some(tools) = event.get("tools").and_then(Value::as_array) else {
                        return Err("Provider tool isolation could not be verified".into());
                    };
                    if tools.iter().any(|tool| {
                        !tool
                            .as_str()
                            .is_some_and(|name| name.starts_with("mcp__sift__sift_"))
                    }) {
                        return Err("Provider exposed a native tool; Sift stopped the turn".into());
                    }
                    tools_seen = true;
                }
                Some("stream_event") => {
                    if let Some(text) = event.pointer("/event/delta/text").and_then(Value::as_str) {
                        publisher.delta(&client, &lease, &events, text).await?;
                    }
                }
                Some("assistant") => {
                    let mut text = String::new();
                    if let Some(parts) = event.pointer("/message/content").and_then(Value::as_array)
                    {
                        for part in parts {
                            if part.get("type").and_then(Value::as_str) == Some("tool_use")
                                && !part
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .is_some_and(|name| name.starts_with("mcp__sift__sift_"))
                            {
                                return Err(
                                    "Provider requested a native tool; Sift stopped the turn"
                                        .into(),
                                );
                            }
                            if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                                if text.len().saturating_add(chunk.len())
                                    > crate::ai_harness::MAX_ANSWER_BYTES
                                {
                                    return Err(
                                        "Provider reply exceeded the Sift text limit".into()
                                    );
                                }
                                text.push_str(chunk);
                            }
                        }
                    }
                    if !text.is_empty() {
                        publisher.message(&text)?;
                        answer = text;
                    }
                }
                Some("result") => {
                    if event.get("is_error").and_then(Value::as_bool) == Some(true) {
                        return Err(provider_failure("Claude Code", &event));
                    }
                    if let Some(text) = event.get("result").and_then(Value::as_str) {
                        crate::ai_harness::AnswerBudget::completed(text)?;
                        answer = text.into();
                    }
                    completed = true;
                }
                _ => {}
            },
            AiProvider::OpenCode => match event.get("type").and_then(Value::as_str) {
                Some("step_start") => completed = false,
                Some("text") => {
                    if let Some(text) = event.pointer("/part/text").and_then(Value::as_str) {
                        answer.push_str(text);
                        publisher.delta(&client, &lease, &events, text).await?;
                    }
                }
                Some("tool_use") => {
                    completed = false;
                    if !event
                        .pointer("/part/tool")
                        .and_then(Value::as_str)
                        .is_some_and(|name| name.starts_with("sift_sift_"))
                    {
                        return Err(
                            "Provider requested a native tool; Sift stopped the turn".into()
                        );
                    }
                    tools_seen = true;
                }
                Some("step_finish") => {
                    completed = opencode_step_finished(&event)?;
                }
                Some("error") => return Err(provider_failure("OpenCode", &event)),
                _ => {}
            },
            AiProvider::Codex => return Err("Use the Codex app-server adapter".into()),
        }
        crate::ai_harness::AnswerBudget::completed(&answer)?;
    }
    let status = child
        .wait()
        .await
        .map_err(|_| "Provider did not exit cleanly")?;
    if !status.success() || !completed || answer.trim().is_empty() {
        return Err(
            "Provider did not complete a reply; check the installed CLI and model access".into(),
        );
    }
    if lease.run.provider == AiProvider::ClaudeCode && !tools_seen {
        return Err("Provider tool isolation could not be verified".into());
    }
    publisher.flush(&client, &lease).await?;
    crate::ai_tools::append_text(&client, &lease, AiEventKind::MessageCompleted, &answer).await?;
    let _ = events.send(ExecutorEvent::AiMessage {
        kind: AiEventKind::MessageCompleted,
        text: answer,
    });
    Ok(())
}

fn opencode_step_finished(event: &Value) -> Result<bool, String> {
    // OpenCode's JSON transport emits this for every model step, including
    // intermediate tool calls. Only an explicit stop completes a reply.
    match event.pointer("/part/reason").and_then(Value::as_str) {
        Some("stop") => Ok(true),
        Some("tool-calls") => Ok(false),
        _ => Err(
            "OpenCode did not report a complete reply; review model limits and CLI compatibility"
                .into(),
        ),
    }
}

fn provider_failure(provider: &str, event: &Value) -> String {
    // Classify privately; never surface raw provider errors or credential bytes.
    let mut reason = event
        .get("result")
        .and_then(Value::as_str)
        .or_else(|| event.pointer("/error/data/message").and_then(Value::as_str))
        .or_else(|| event.pointer("/error/message").and_then(Value::as_str))
        .unwrap_or("")
        .to_ascii_lowercase();
    if let Some(errors) = event.get("errors").and_then(Value::as_array) {
        for error in errors {
            if let Some(text) = error.as_str() {
                reason.push_str(&text.to_ascii_lowercase());
            }
        }
    }
    let category = if reason.contains("expired") && reason.contains("oauth") {
        if provider == "Claude Code" {
            "CLI login expired; sign in again with claude auth login"
        } else {
            "CLI login expired; sign in again with the installed provider CLI"
        }
    } else if ["read-only", "erofs", "refresh token"]
        .iter()
        .any(|needle| reason.contains(needle))
    {
        "authentication refresh could not complete in the isolated CLI"
    } else if [
        "login",
        "logged in",
        "authentication",
        "401",
        "api key",
        "oauth",
    ]
    .iter()
    .any(|needle| reason.contains(needle))
    {
        "authentication is unavailable in the isolated CLI; sign in with the CLI again"
    } else if [
        "quota",
        "credit balance",
        "hit your limit",
        "rate limit",
        "rate_limit",
        "429",
    ]
    .iter()
    .any(|needle| reason.contains(needle))
    {
        "account usage limit was reached"
    } else if ["permission", "403"]
        .iter()
        .any(|needle| reason.contains(needle))
    {
        "account permission was denied"
    } else if ["model", "404"]
        .iter()
        .any(|needle| reason.contains(needle))
    {
        "selected model is unavailable"
    } else if [
        "network",
        "fetch failed",
        "econn",
        "certificate",
        "enotfound",
    ]
    .iter()
    .any(|needle| reason.contains(needle))
    {
        "network connection failed"
    } else if ["validation", "schema", "invalid_request", "tool"]
        .iter()
        .any(|needle| reason.contains(needle))
    {
        "tool request was rejected"
    } else {
        "request failed; check CLI authentication and model access"
    };
    format!("{provider} {category}")
}

pub(crate) fn configured_model(provider: AiProvider) -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let paths = match provider {
        AiProvider::ClaudeCode => vec![home.join(".claude/settings.json")],
        AiProvider::OpenCode => {
            let directory = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("opencode");
            vec![
                directory.join("opencode.json"),
                directory.join("opencode.jsonc"),
            ]
        }
        AiProvider::Codex => return None,
    };
    let configured = paths.into_iter().find_map(|path| {
        use std::io::Read;
        let file = std::fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() > 64 * 1024 {
            return None;
        }
        model_preference(&bytes)
    });
    configured.or_else(|| {
        if provider != AiProvider::OpenCode {
            return None;
        }
        let path = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"))
            .join("opencode/model.json");
        recent_model(&bounded_file(&path, 64 * 1024)?)
    })
}
fn recent_model(bytes: &[u8]) -> Option<String> {
    let state: Value = serde_json::from_slice(bytes).ok()?;
    let recent = state.get("recent")?.as_array()?.first()?;
    let provider = recent.get("providerID")?.as_str()?;
    let model = recent.get("modelID")?.as_str()?;
    let selected = format!("{provider}/{model}");
    (!provider.is_empty()
        && !model.is_empty()
        && selected.len() <= 128
        && !selected.chars().any(char::is_control))
    .then_some(selected)
}
fn bounded_file(path: &Path, limit: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

struct ProviderHome {
    temp: tempfile::TempDir,
    source_auth: PathBuf,
    relative_auth: String,
    original_auth: Vec<u8>,
}
impl Drop for ProviderHome {
    fn drop(&mut self) {
        // The installed CLI may rotate OAuth credentials in its private HOME.
        // Preserve a complete refresh only while its original host credentials
        // are still current. Never persist settings or any model-produced data.
        let Some(updated) = bounded_file(&self.temp.path().join(&self.relative_auth), 128 * 1024)
        else {
            return;
        };
        if updated == self.original_auth {
            return;
        }
        let Ok(original) = serde_json::from_slice::<Value>(&self.original_auth) else {
            return;
        };
        let Ok(next) = serde_json::from_slice::<Value>(&updated) else {
            return;
        };
        if !valid_auth_refresh(&original, &next) {
            return;
        }
        let Some(parent) = self.source_auth.parent() else {
            return;
        };
        let Ok(mut replacement) = tempfile::NamedTempFile::new_in(parent) else {
            return;
        };
        use std::io::Write;
        if replacement.write_all(&updated).is_err() || replacement.as_file().sync_all().is_err() {
            return;
        }
        if bounded_file(&self.source_auth, 128 * 1024).as_deref() != Some(&self.original_auth) {
            return;
        }
        let _ = replacement.persist(&self.source_auth);
    }
}
fn valid_auth_refresh(original: &Value, next: &Value) -> bool {
    let (Some(original), Some(next)) = (original.as_object(), next.as_object()) else {
        return false;
    };
    if original.len() != next.len() {
        return false;
    }
    let mut changed = false;
    for (key, prior) in original {
        let Some(current) = next.get(key) else {
            return false;
        };
        if prior == current {
            continue;
        }
        let (Some(prior), Some(current)) = (prior.as_object(), current.as_object()) else {
            return false;
        };
        // Admit only the provider's existing OAuth object and known rotation
        // fields. New accounts, API keys, URLs and unrelated fields are rejected.
        if key != "claudeAiOauth" && prior.get("type").and_then(Value::as_str) != Some("oauth") {
            return false;
        }
        if prior.len() != current.len() {
            return false;
        }
        for (field, value) in prior {
            let Some(new) = current.get(field) else {
                return false;
            };
            if value != new
                && !matches!(
                    field.as_str(),
                    "accessToken"
                        | "refreshToken"
                        | "expiresAt"
                        | "refreshTokenExpiresAt"
                        | "access"
                        | "refresh"
                        | "expires"
                )
            {
                return false;
            }
        }
        let access = current
            .get("accessToken")
            .or_else(|| current.get("access"))
            .and_then(Value::as_str);
        let refresh = current
            .get("refreshToken")
            .or_else(|| current.get("refresh"))
            .and_then(Value::as_str);
        let expiry = current
            .get("expiresAt")
            .or_else(|| current.get("expires"))
            .and_then(Value::as_u64);
        let old_expiry = prior
            .get("expiresAt")
            .or_else(|| prior.get("expires"))
            .and_then(Value::as_u64);
        if access.is_none_or(|token| token.is_empty())
            || refresh.is_none_or(|token| token.is_empty())
            || expiry.zip(old_expiry).is_none_or(|(new, old)| new <= old)
        {
            return false;
        }
        changed = true;
    }
    changed
}
fn model_preference(bytes: &[u8]) -> Option<String> {
    // Read only the model field from JSON/JSONC. No host configuration is copied.
    let mut plain = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut string = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if string {
            plain.push(byte);
            index += 1;
            if byte == b'\\' && index < bytes.len() {
                plain.push(bytes[index]);
                index += 1;
            } else if byte == b'"' {
                string = false;
            }
        } else if byte == b'"' {
            string = true;
            plain.push(byte);
            index += 1;
        } else if bytes.get(index..index + 2) == Some(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            index += 2;
            while index + 1 < bytes.len() && bytes.get(index..index + 2) != Some(b"*/") {
                index += 1;
            }
            if index + 1 >= bytes.len() {
                return None;
            }
            index += 2;
            plain.push(b' ');
        } else {
            plain.push(byte);
            index += 1;
        }
    }
    // JSONC preferences can have trailing commas; preserve commas inside strings.
    let mut clean = Vec::with_capacity(plain.len());
    let mut index = 0;
    let mut string = false;
    while index < plain.len() {
        let byte = plain[index];
        if string {
            clean.push(byte);
            index += 1;
            if byte == b'\\' && index < plain.len() {
                clean.push(plain[index]);
                index += 1;
            } else if byte == b'"' {
                string = false;
            }
        } else {
            if byte == b'"' {
                string = true;
            }
            let trailing = byte == b','
                && plain[index + 1..]
                    .iter()
                    .find(|byte| !byte.is_ascii_whitespace())
                    .is_some_and(|next| matches!(next, b'}' | b']'));
            if !trailing {
                clean.push(byte);
            }
            index += 1;
        }
    }
    let value: Value = serde_json::from_slice(&clean).ok()?;
    value
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| {
            !model.is_empty() && model.len() <= 128 && !model.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

#[cfg(target_os = "linux")]
fn launch(
    provider: AiProvider,
    model: Option<&str>,
    bridge: &crate::ai_mcp_bridge::Bridge,
) -> Result<(Child, ProviderHome), String> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("Provider home is unavailable")?;
    let name = match provider {
        AiProvider::ClaudeCode => "claude",
        AiProvider::OpenCode => "opencode",
        AiProvider::Codex => return Err("Use the Codex adapter".into()),
    };
    let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("Installed {name} CLI was not found"))?;
    let executable =
        std::fs::canonicalize(executable).map_err(|_| "Cannot inspect the installed provider")?;
    let mut magic = [0; 4];
    std::fs::File::open(&executable)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map_err(|_| "Cannot inspect the installed provider")?;
    if magic != [127, b'E', b'L', b'F'] {
        return Err(format!(
            "Sift's isolated {name} adapter currently requires the native Linux CLI"
        ));
    }
    let temp = tempfile::tempdir().map_err(|_| "Cannot create an isolated provider home")?;
    let (source_auth, relative_auth) = match provider {
        AiProvider::ClaudeCode => (
            home.join(".claude/.credentials.json"),
            ".claude/.credentials.json",
        ),
        AiProvider::OpenCode => (
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"))
                .join("opencode/auth.json"),
            ".local/share/opencode/auth.json",
        ),
        _ => unreachable!(),
    };
    if !source_auth.is_file() {
        return Err(format!("Sign in with the installed {name} CLI first"));
    }
    let auth = temp.path().join(relative_auth);
    std::fs::create_dir_all(
        auth.parent()
            .ok_or("Provider auth directory is unavailable")?,
    )
    .map_err(|_| "Cannot prepare provider authentication")?;
    let original_auth =
        bounded_file(&source_auth, 128 * 1024).ok_or("Cannot prepare provider authentication")?;
    std::fs::write(&auth, &original_auth).map_err(|_| "Cannot prepare provider authentication")?;
    std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "Cannot protect provider authentication")?;
    if provider == AiProvider::ClaudeCode {
        // Only OAuth identity cache is needed; never copy user settings/hooks.
        if let Some(bytes) = bounded_file(&home.join(".claude.json"), 128 * 1024) {
            if bytes.len() <= 128 * 1024 {
                if let Ok(source) = serde_json::from_slice::<Value>(&bytes) {
                    let mut account = serde_json::Map::new();
                    for key in [
                        "accountUuid",
                        "emailAddress",
                        "organizationUuid",
                        "billingType",
                        "hasExtraUsageEnabled",
                        "organizationRole",
                        "workspaceRole",
                    ] {
                        if let Some(value) = source
                            .get("oauthAccount")
                            .and_then(|account| account.get(key))
                        {
                            account.insert(key.into(), value.clone());
                        }
                    }
                    if !account.is_empty() {
                        let metadata = json!({"oauthAccount":account,"hasCompletedOnboarding":source.get("hasCompletedOnboarding").and_then(Value::as_bool).unwrap_or(false)});
                        let path = temp.path().join(".claude.json");
                        std::fs::write(
                            &path,
                            serde_json::to_vec(&metadata)
                                .map_err(|_| "Cannot prepare provider sign-in metadata")?,
                        )
                        .map_err(|_| "Cannot prepare provider sign-in metadata")?;
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                            .map_err(|_| "Cannot protect provider sign-in metadata")?;
                    }
                }
            }
        }
    }
    let config = match provider {
        AiProvider::ClaudeCode => {
            json!({"mcpServers":{"sift":{"type":"http","alwaysLoad":true,"url":bridge.url,"headers":{"Authorization":format!("Bearer {}",bridge.capability)}}}})
        }
        AiProvider::OpenCode => {
            json!({"permission":{"*":"deny","sift_*":"allow"},"tools":{"*":false,"sift_*":true},"agent":{"sift":{"mode":"primary","prompt":SYSTEM_PROMPT,"tools":{"*":false,"sift_*":true},"permission":{"*":"deny","sift_*":"allow"}}},"mcp":{"sift":{"type":"remote","url":bridge.url,"headers":{"Authorization":format!("Bearer {}",bridge.capability)},"enabled":true,"oauth":false}}})
        }
        _ => unreachable!(),
    };
    let config_path = temp.path().join("sift-provider.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).map_err(|_| "Cannot encode provider settings")?,
    )
    .map_err(|_| "Cannot prepare provider settings")?;
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "Cannot protect provider settings")?;
    let bwrap = if Path::new("/usr/bin/bwrap").is_file() {
        PathBuf::from("/usr/bin/bwrap")
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("bwrap"))
            .find(|path| path.is_file())
            .ok_or("Bubblewrap is required for isolated AI chat")?
    };
    let mut command = Command::new(bwrap);
    command
        .env_clear()
        .env("LANG", "C.UTF-8")
        .env("PATH", "/sift-cli:/usr/bin:/bin");
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
        "/sift-cli",
    ]);
    if Path::new("/nix").exists() {
        command.args(["--ro-bind", "/nix", "/nix"]);
    }
    command
        .arg("--bind")
        .arg(temp.path())
        .arg("/sift-home")
        .arg("--ro-bind")
        .arg(&config_path)
        .arg("/sift-home/sift-provider.json")
        .arg("--ro-bind")
        .arg(&executable)
        .arg(format!("/sift-cli/{name}"));
    command.args([
        "--chdir",
        "/tmp",
        "--setenv",
        "HOME",
        "/sift-home",
        "--setenv",
        "XDG_CONFIG_HOME",
        "/sift-home/.config",
        "--setenv",
        "XDG_DATA_HOME",
        "/sift-home/.local/share",
        "--setenv",
        "XDG_CACHE_HOME",
        "/sift-home/.cache",
        "--setenv",
        "PATH",
        "/sift-cli:/usr/bin:/bin",
    ]);
    if provider == AiProvider::OpenCode {
        command.args([
            "--setenv",
            "OPENCODE_CONFIG",
            "/sift-home/sift-provider.json",
        ]);
    }
    if provider == AiProvider::ClaudeCode {
        command.args(["--setenv", "ENABLE_TOOL_SEARCH", "false"]);
    }
    command.arg(format!("/sift-cli/{name}"));
    match provider {
        AiProvider::ClaudeCode => {
            command.args([
                "--print",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--restricted",
                "--tools",
                "",
                "--strict-mcp-config",
                "--mcp-config",
                "/sift-home/sift-provider.json",
                "--setting-sources",
                "",
                "--settings",
                "{\"disableAllHooks\":true}",
                "--disable-slash-commands",
                "--no-session-persistence",
                "--permission-mode",
                "dontAsk",
                "--allowedTools",
                "mcp__sift__sift_*",
                "--system-prompt",
                SYSTEM_PROMPT,
            ]);
        }
        AiProvider::OpenCode => {
            command.args([
                "run", "--pure", "--format", "json", "--agent", "sift", "--dir", "/tmp",
            ]);
        }
        _ => unreachable!(),
    }
    if let Some(model) = model {
        command.arg(format!("--model={model}"));
    }
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Isolated provider launch failed")?;
    Ok((
        child,
        ProviderHome {
            temp,
            source_auth,
            relative_auth: relative_auth.into(),
            original_auth,
        },
    ))
}
#[cfg(not(target_os = "linux"))]
fn launch(
    _: AiProvider,
    _: Option<&str>,
    _: &crate::ai_mcp_bridge::Bridge,
) -> Result<(Child, ProviderHome), String> {
    Err("Isolated AI providers are currently supported on Linux only".into())
}
const SYSTEM_PROMPT:&str="You are Sift's SQL assistant. Only Sift MCP tools may access data. Native shell, file, web, task, coding and arbitrary MCP tools are unavailable. Read mode never proposes changes. In Propose mode stage SQL or typed row/schema drafts for human review; use sift_catalog first and original values for updates/deletes. Only a human may apply a draft. Never claim staged work was applied. Current turn context is authoritative; bounded results may be truncated.";

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[tokio::test]
    async fn native_token_fragments_are_batched_without_exhausting_event_receipts() {
        let (client, lease, _context, server) = fixture(AiProvider::ClaudeCode).await;
        let (events, mut ui) = tokio::sync::mpsc::unbounded_channel();
        let events = crate::ai_harness::AiEventSender::from(events);
        let mut publisher = crate::ai_harness::TextPublisher::default();
        for _ in 0..5000 {
            publisher
                .delta(&client, &lease, &events, "🌍")
                .await
                .unwrap();
        }
        publisher.delta(&client, &lease, &events, "").await.unwrap();
        publisher.flush(&client, &lease).await.unwrap();
        let saved = client.ai_events(lease.run.id, 0).await.unwrap();
        let deltas = saved
            .into_iter()
            .filter(|event| event.kind == AiEventKind::MessageDelta)
            .collect::<Vec<_>>();
        assert!(deltas.len() < 30);
        let text = deltas
            .into_iter()
            .map(|event| event.content.unwrap()["text"].as_str().unwrap().to_owned())
            .collect::<String>();
        assert_eq!(text, "🌍".repeat(5000));
        let mut count = 0;
        while let Ok(event) = ui.try_recv() {
            assert!(matches!(event, ExecutorEvent::AiTextDelta(ref text) if text == "🌍"));
            count += 1;
        }
        assert_eq!(count, 5000);
        // Worst-case JSON escaping still fits one durable event.
        crate::ai_tools::append_text(
            &client,
            &lease,
            AiEventKind::ProgressSummary,
            &"\u{0001}".repeat(16 * 1024),
        )
        .await
        .unwrap();
        server.abort();
    }

    #[test]
    fn opencode_intermediate_steps_cannot_complete_a_turn() {
        use super::*;
        assert!(!opencode_step_finished(&json!({"part":{"reason":"tool-calls"}})).unwrap());
        assert!(opencode_step_finished(&json!({"part":{"reason":"stop"}})).unwrap());
        for reason in ["unknown", "length", "content-filter", "error"] {
            assert!(opencode_step_finished(&json!({"part":{"reason":reason}})).is_err());
        }
        assert!(opencode_step_finished(&json!({"part":{}})).is_err());
    }

    use super::*;
    use serde_json::Value;
    use sift_metadata::{
        CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId,
        TenantId,
    };
    use sift_protocol::*;
    use std::sync::Arc;

    #[test]
    fn provider_errors_are_sanitized_and_point_to_the_matching_cli() {
        let error = json!({"result":"OAuth session expired: DO_NOT_EXPOSE_CREDENTIAL"});
        let claude = provider_failure("Claude Code", &error);
        assert!(claude.contains("claude auth login"));
        let opencode = provider_failure("OpenCode", &error);
        assert!(!opencode.contains("claude"));
        assert!(opencode.contains("installed provider CLI"));
        assert!(!claude.contains("DO_NOT_EXPOSE_CREDENTIAL"));
        assert!(!opencode.contains("DO_NOT_EXPOSE_CREDENTIAL"));
    }
    #[test]
    fn model_preference_reads_jsonc_without_loading_plugins_or_mcp_settings() {
        let config=br#"{/* preference */ "model":"openai/gpt-example", "plugin":["https://untrusted.invalid/plugin",], // ignored
            "mcp":{"unexpected":{"url":"https://untrusted.invalid/mcp"}}, }"#;
        assert_eq!(
            model_preference(config).as_deref(),
            Some("openai/gpt-example")
        );
        assert_eq!(
            model_preference(br#"{"model":"provider/name//literal"}"#).as_deref(),
            Some("provider/name//literal")
        );
        assert!(model_preference(br#"{"model":"bad\nmodel"}"#).is_none());
        assert!(model_preference(br#"{/* unterminated"#).is_none());
    }

    async fn fixture(
        provider: AiProvider,
    ) -> (
        Client,
        AiRunLease,
        AiTurnContext,
        tokio::task::JoinHandle<()>,
    ) {
        let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        metadata.bootstrap_local("Isolated provider probe").unwrap();
        let profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {name:"Synthetic provider probe".into(),provider_id:Engine::Postgres.provider_id(),semantic_engine:Some(Engine::Postgres),configuration:json!({"host":"mock.invalid","port":5432,"database":"probe","user":"probe","ssl_mode":"disable"}),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![]}).await.unwrap();
        let driver = sift_driver_api::mock::MockDriver::builder()
            .engine(Engine::Postgres)
            .build();
        let mut auth = sift_server::http::AuthState::default();
        auth.ai.enabled = true;
        let router = sift_server::http::app(sift_server::http::AppState {
            sessions: sift_server::SessionStore::new(
                sift_server::DriverRegistry::builder()
                    .register(driver)
                    .build(),
            ),
            rooms: sift_server::room_runtime::RoomRuntime::default(),
            shutdown: Default::default(),
            auth,
            metadata: Some(metadata.clone()),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = Client::new(format!("http://{addr}"));
        let session = client.open_session(None).await.unwrap().id;
        let connection = client
            .open_connection_from_profile(
                session,
                sift_api_types::OpenConnectionFromProfileRequest {
                    tenant_id: 1,
                    profile_id: profile.id.0,
                },
            )
            .await
            .unwrap()
            .id;
        let chat = metadata
            .create_ai_chat(
                TenantId(1),
                None,
                PrincipalId(1),
                AiVisibility::Private,
                "Provider isolation probe".into(),
            )
            .await
            .unwrap();
        let context = AiTurnContext {
            inclusion: Default::default(),
            workspace: None,
            attachments: Vec::new(),
            target: ToolContext {
                tenant_id: Some(1),
                room_id: None,
                profile_id: Some(profile.id.0),
                connection_id: Some(format!("{}:{}", session.0, connection.0)),
                document_id: None,
            },
            editor_item_id: None,
            database: Some("probe".into()),
            dialect: Some("postgres".into()),
            environment_label: None,
            sql: None,
            current_error: None,
            staged_change_count: 0,
            publication_id: None,
        };
        let prompt="Call the available Sift diagnostics tool with SQL SELECT 1, then reply OK. No other tools.".to_owned();
        let lease = client
            .start_ai_turn(
                chat.id,
                &StartAiTurnRequest {
                    attachment_previews: Vec::new(),
                    client_request_id: uuid::Uuid::new_v4(),
                    desktop_id: uuid::Uuid::new_v4(),
                    prompt: prompt.clone(),
                    provider,
                    model: None,
                    mode: AiMode::Read,
                    context: context.clone(),
                },
            )
            .await
            .unwrap();
        (client, lease, context, server)
    }
    async fn probe(provider: AiProvider) {
        let (client, lease, context, server) = fixture(provider).await;
        let prompt = "Call the available Sift diagnostics tool with SQL SELECT 1, then reply OK. No other tools.".to_owned();
        let (events, _) = tokio::sync::mpsc::unbounded_channel();
        let events = crate::ai_harness::AiEventSender::from(events);
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            run(
                client.clone(),
                lease.clone(),
                prompt,
                context,
                Default::default(),
                events,
            ),
        )
        .await
        .expect("provider probe timeout")
        .expect("isolated provider roundtrip");
        let events = client.ai_events(lease.run.id, 0).await.unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == AiEventKind::ToolCompleted),
            "provider must complete the governed Sift tool"
        );
        assert!(events
            .iter()
            .any(|event| event.kind == AiEventKind::MessageCompleted
                && event
                    .content
                    .as_ref()
                    .and_then(|content| content.get("text"))
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains("OK"))));
        server.abort();
    }
    #[test]
    fn credential_refresh_preserves_only_oauth_rotation_and_current_source() {
        let original = json!({"claudeAiOauth":{"accessToken":"old","refreshToken":"refresh-old","expiresAt":10,"scopes":["inference"]},"trustedDeviceToken":"device"});
        let next = json!({"claudeAiOauth":{"accessToken":"new","refreshToken":"refresh-new","expiresAt":20,"scopes":["inference"]},"trustedDeviceToken":"device"});
        assert!(valid_auth_refresh(&original, &next));
        let mut bad = next.clone();
        bad["claudeAiOauth"]["scopes"] = json!(["changed"]);
        assert!(!valid_auth_refresh(&original, &bad));
        bad = next.clone();
        bad["claudeAiOauth"]["expiresAt"] = json!(5);
        assert!(!valid_auth_refresh(&original, &bad));
        bad = next.clone();
        bad["newAccount"] = json!("unexpected");
        assert!(!valid_auth_refresh(&original, &bad));
        for concurrent_signin in [false, true] {
            let source = tempfile::tempdir().unwrap();
            let path = source.path().join("credentials.json");
            let original_auth = serde_json::to_vec(&original).unwrap();
            std::fs::write(&path, &original_auth).unwrap();
            let temp = tempfile::tempdir().unwrap();
            std::fs::write(
                temp.path().join("auth.json"),
                serde_json::to_vec(&next).unwrap(),
            )
            .unwrap();
            let home = ProviderHome {
                temp,
                source_auth: path.clone(),
                relative_auth: "auth.json".into(),
                original_auth,
            };
            if concurrent_signin {
                std::fs::write(&path, b"new-native-signin").unwrap();
            }
            drop(home);
            let actual = std::fs::read(&path).unwrap();
            if concurrent_signin {
                assert_eq!(actual, b"new-native-signin");
            } else {
                assert_eq!(serde_json::from_slice::<Value>(&actual).unwrap(), next);
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        assert_eq!(
            recent_model(br#"{"recent":[{"providerID":"provider","modelID":"selected"}]}"#)
                .as_deref(),
            Some("provider/selected")
        );
        assert!(
            recent_model(br#"{"recent":[{"providerID":"provider","modelID":"bad\nvalue"}]}"#)
                .is_none()
        );
    }
    #[tokio::test]
    async fn bridge_enforces_capability_protocol_tools_and_immutable_receipts() {
        let (client, lease, context, server) = fixture(AiProvider::ClaudeCode).await;
        let (events, _) = tokio::sync::mpsc::unbounded_channel();
        let events = crate::ai_harness::AiEventSender::from(events);
        let bridge =
            crate::ai_mcp_bridge::Bridge::start(client.clone(), lease.clone(), context, events)
                .await
                .unwrap();
        let http = reqwest::Client::new();
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
        assert_eq!(
            http.post(&bridge.url)
                .json(&request)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        assert_eq!(
            http.post(&bridge.url)
                .bearer_auth(&bridge.capability)
                .header("Origin", "https://untrusted.invalid")
                .json(&request)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        assert_eq!(
            http.post(&bridge.url)
                .bearer_auth(&bridge.capability)
                .header("MCP-Protocol-Version", "unsupported")
                .json(&request)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::BAD_REQUEST
        );
        let call = |request: Value| {
            http.post(&bridge.url)
                .bearer_auth(&bridge.capability)
                .json(&request)
        };
        let result: Value = call(request.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["error"]["code"], -32002);
        let init = json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"unknown"}});
        let result: Value = call(init).send().await.unwrap().json().await.unwrap();
        assert_eq!(result["result"]["protocolVersion"], "2025-11-25");
        let result: Value = call(request).send().await.unwrap().json().await.unwrap();
        let tools = result["result"]["tools"].as_array().unwrap();
        assert!(tools
            .iter()
            .all(|tool| tool["name"].as_str().unwrap().starts_with("sift_")));
        assert!(tools
            .iter()
            .all(|tool| !tool["name"].as_str().unwrap().contains("stage")));
        for name in [
            "bash",
            "sift_apply_edits",
            "sift_stage_sql",
            "sift_stage_database",
        ] {
            let result:Value=call(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":{}}})).send().await.unwrap().json().await.unwrap();
            assert_eq!(result["error"]["code"], -32602);
        }
        let request = json!({"jsonrpc":"2.0","id":"immutable","method":"tools/call","params":{"name":"sift_diagnostics","arguments":{"sql":"SELECT 1"}}});
        let result: Value = call(request.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["result"]["isError"], false);
        let mut changed = request.clone();
        changed["params"]["arguments"]["sql"] = json!("SELECT 2");
        let result: Value = call(changed).send().await.unwrap().json().await.unwrap();
        assert_eq!(result["error"]["code"], -32602);
        // A duplicate read identity is never dispatched twice by Sift.
        let result: Value = call(request).send().await.unwrap().json().await.unwrap();
        assert_eq!(result["result"]["isError"], true);
        let events = client.ai_events(lease.run.id, 0).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == AiEventKind::ToolCompleted)
                .count(),
            1
        );
        let url = bridge.url.clone();
        let capability = bridge.capability.clone();
        drop(bridge);
        // Keep-alive connections may outlive the listener; the capability is
        // revoked immediately even on an already accepted connection.
        if let Ok(response) = http
            .post(url)
            .bearer_auth(capability)
            .json(&json!({"jsonrpc":"2.0","id":99,"method":"tools/list"}))
            .send()
            .await
        {
            assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        }
        server.abort();
    }
    #[tokio::test]
    #[ignore = "requires installed signed-in native Claude Code and Bubblewrap"]
    async fn claude_isolated_sift_tool_roundtrip() {
        probe(AiProvider::ClaudeCode).await;
    }
    #[tokio::test]
    #[ignore = "requires installed signed-in native OpenCode and Bubblewrap"]
    async fn opencode_isolated_sift_tool_roundtrip() {
        probe(AiProvider::OpenCode).await;
    }
}
