use std::ops::Range;

use crate::{ConfigError, Manifest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDiagnostic {
    pub range: Range<usize>,
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestCompletion {
    pub label: String,
    pub insertion: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestHover {
    pub range: Range<usize>,
    pub path: String,
    pub value_type: &'static str,
    pub documentation: &'static str,
    pub choices: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestOutlineItem {
    pub title: String,
    pub path: String,
    pub offset: usize,
    pub depth: usize,
}

#[derive(Clone, Copy)]
struct Field {
    table: &'static str,
    key: &'static str,
    value_type: &'static str,
    documentation: &'static str,
    choices: &'static [&'static str],
}

const NONE: &[&str] = &[];
const BOOL: &[&str] = &["true", "false"];

macro_rules! field {
    ($table:literal, $key:literal, $ty:literal, $docs:literal) => {
        Field {
            table: $table,
            key: $key,
            value_type: $ty,
            documentation: $docs,
            choices: NONE,
        }
    };
    ($table:literal, $key:literal, $ty:literal, $docs:literal, $choices:expr) => {
        Field {
            table: $table,
            key: $key,
            value_type: $ty,
            documentation: $docs,
            choices: $choices,
        }
    };
}

// This is deliberately the small, stable editor schema rather than a second
// deserializer. Tests below require every accepted manifest field to be
// represented here, while serde remains the authority for parsing.
const FIELDS: &[Field] = &[
    field!(
        "",
        "kind",
        "string",
        "Manifest discriminator; must be sift-instance.",
        &["sift-instance"]
    ),
    field!(
        "",
        "format_version",
        "integer",
        "Manifest format version; currently 1.",
        &["1"]
    ),
    field!(
        "",
        "manifest_id",
        "UUID string",
        "Stable identity of this server instance."
    ),
    field!("", "name", "string", "Human-readable instance name."),
    field!(
        "compatibility",
        "sift",
        "semver requirement",
        "Compatible Sift runtime versions."
    ),
    field!(
        "server",
        "deployment",
        "enum",
        "Personal or team identity policy.",
        &["personal", "team"]
    ),
    field!(
        "server",
        "transport",
        "enum",
        "How clients reach this server.",
        &["loopback", "network", "ssh-proxy"]
    ),
    field!(
        "server",
        "mode",
        "enum",
        "Who owns the server process lifecycle.",
        &["in-process", "daemon", "container"]
    ),
    field!(
        "server",
        "bind",
        "socket or symbolic string",
        "Listen address, auto-loopback, or prompt-required."
    ),
    field!(
        "server",
        "public_base_url",
        "HTTPS origin",
        "Authoritative public OAuth origin."
    ),
    field!(
        "server.timeouts",
        "request_secs",
        "integer",
        "Synchronous request timeout in seconds."
    ),
    field!(
        "server.timeouts",
        "shutdown_drain_secs",
        "integer",
        "Graceful query-drain deadline in seconds."
    ),
    field!(
        "server.metadata",
        "secret_backend",
        "enum",
        "Destination secret storage backend.",
        &["file", "keychain", "memory"]
    ),
    field!(
        "server.metadata",
        "store_sql",
        "boolean",
        "Persist raw SQL in query history.",
        BOOL
    ),
    field!(
        "server.limits",
        "max_http_result_rows",
        "integer",
        "Maximum rows in a synchronous response."
    ),
    field!(
        "server.limits",
        "max_http_result_bytes",
        "integer",
        "Maximum approximate response bytes."
    ),
    field!(
        "server.limits",
        "max_connections",
        "integer",
        "Legacy per-tenant open-connection ceiling."
    ),
    field!(
        "server.limits",
        "max_concurrent_queries",
        "integer",
        "Legacy per-tenant concurrent-query ceiling."
    ),
    field!(
        "server.limits",
        "max_cursors_per_session",
        "integer",
        "Open cursor ceiling per session."
    ),
    field!(
        "server.limits",
        "cursor_prefetch_pages",
        "integer",
        "Cursor pages buffered ahead."
    ),
    field!(
        "server.limits",
        "cursor_spill_dir",
        "path string",
        "Optional cursor spill directory."
    ),
    field!(
        "server.limits",
        "cursor_spill_ttl_secs",
        "integer",
        "Spill-file lifetime in seconds."
    ),
    field!(
        "server.limits",
        "schema_cache_ttl_secs",
        "integer",
        "Schema cache lifetime in seconds."
    ),
    field!(
        "server.limits",
        "schema_mssql_poll_secs",
        "integer",
        "SQL Server schema polling interval."
    ),
    field!(
        "server.limits",
        "plan_capture_max_bytes",
        "integer",
        "Maximum stored plan bytes."
    ),
    field!(
        "server.limits",
        "plan_capture_max_per_tenant",
        "integer",
        "Plan captures retained per tenant."
    ),
    field!(
        "server.limits",
        "plan_capture_max_per_source",
        "integer",
        "Plan captures retained per source."
    ),
    field!(
        "server.limits",
        "plan_capture_max_age_days",
        "integer",
        "Maximum plan-capture age."
    ),
    field!(
        "server.workspaces",
        "enabled",
        "boolean",
        "Enable filesystem-backed workspaces.",
        BOOL
    ),
    field!(
        "server.workspaces.roots",
        "handle",
        "string",
        "Stable workspace root handle."
    ),
    field!(
        "server.workspaces.roots",
        "path",
        "absolute path",
        "Operator-owned workspace directory."
    ),
    field!(
        "server.workspaces.roots",
        "read_only",
        "boolean",
        "Prevent writes under this root.",
        BOOL
    ),
    field!(
        "server.vcs",
        "enabled",
        "boolean",
        "Enable bundled Git integration.",
        BOOL
    ),
    field!(
        "server.vcs",
        "network_enabled",
        "boolean",
        "Allow Git network operations.",
        BOOL
    ),
    field!(
        "server.vcs",
        "executable",
        "absolute path",
        "Optional fixed Git executable."
    ),
    field!(
        "server.vcs",
        "local_timeout_secs",
        "integer",
        "Local Git command timeout."
    ),
    field!(
        "server.vcs",
        "network_timeout_secs",
        "integer",
        "Network Git command timeout."
    ),
    field!(
        "server.vcs",
        "max_output_bytes",
        "integer",
        "Maximum Git process output."
    ),
    field!(
        "server.vcs",
        "max_file_bytes",
        "integer",
        "Maximum file size read by Git features."
    ),
    field!(
        "server.vcs",
        "max_status_entries",
        "integer",
        "Maximum worktree status entries."
    ),
    field!(
        "server.vcs",
        "max_history_page",
        "integer",
        "Maximum history page size."
    ),
    field!(
        "server.vcs",
        "max_commit_files",
        "integer",
        "Maximum files in a commit operation."
    ),
    field!(
        "server.vcs",
        "max_diff_files",
        "integer",
        "Maximum files in a diff."
    ),
    field!(
        "server.vcs",
        "max_diff_hunks",
        "integer",
        "Maximum hunks in a diff."
    ),
    field!(
        "server.vcs",
        "max_diff_lines",
        "integer",
        "Maximum lines in a diff."
    ),
    field!(
        "server.updater",
        "enabled",
        "boolean",
        "Enable signed background update checks.",
        BOOL
    ),
    field!(
        "server.updater",
        "channel",
        "string",
        "Signed release channel."
    ),
    field!(
        "server.updater",
        "manifest_url",
        "HTTPS URL",
        "Distribution-owned update manifest."
    ),
    field!(
        "server.updater",
        "signature_url",
        "HTTPS URL",
        "Detached manifest signature."
    ),
    field!(
        "server.updater",
        "max_artifact_bytes",
        "integer",
        "Update download ceiling."
    ),
    field!(
        "server.updater",
        "initial_delay_secs",
        "integer",
        "Delay before the first update check."
    ),
    field!(
        "server.updater",
        "check_interval_secs",
        "integer",
        "Update check interval."
    ),
    field!(
        "server.updater",
        "jitter_secs",
        "integer",
        "Randomized update-check delay."
    ),
    field!(
        "server.log",
        "filter",
        "string",
        "RUST_LOG-style server log filter."
    ),
    field!(
        "server.drivers",
        "mock",
        "boolean",
        "Use the mock PostgreSQL driver for demos.",
        BOOL
    ),
    field!(
        "server.drivers",
        "mock_extra",
        "boolean",
        "Register the extra synthetic driver.",
        BOOL
    ),
    field!(
        "server.extension_policy",
        "development_overrides",
        "string array",
        "Local extension development directories."
    ),
    field!(
        "server.extension_policy",
        "allow_hosted_development",
        "boolean",
        "Permit development paths in team mode.",
        BOOL
    ),
    field!(
        "server.vault",
        "max_label_bytes",
        "integer",
        "Maximum vault label bytes."
    ),
    field!(
        "server.vault",
        "max_metadata_bytes",
        "integer",
        "Maximum vault metadata bytes."
    ),
    field!(
        "server.vault",
        "max_secret_bytes",
        "integer",
        "Maximum secret bytes."
    ),
    field!(
        "server.vault",
        "max_vaults_per_tenant",
        "integer",
        "Vault ceiling per tenant."
    ),
    field!(
        "server.vault",
        "max_items_per_vault",
        "integer",
        "Item ceiling per vault."
    ),
    field!(
        "server.vault",
        "max_versions_per_item",
        "integer",
        "Retained versions per item."
    ),
    field!(
        "server.vault",
        "cleanup_batch_size",
        "integer",
        "Cleanup records processed per pass."
    ),
    field!(
        "server.vault",
        "cleanup_interval_secs",
        "integer",
        "Cleanup interval."
    ),
    field!(
        "server.vault",
        "cleanup_retry_initial_secs",
        "integer",
        "Initial cleanup retry delay."
    ),
    field!(
        "server.vault",
        "cleanup_retry_max_secs",
        "integer",
        "Maximum cleanup retry delay."
    ),
    field!(
        "server.audit",
        "operation_log_path",
        "path string",
        "Optional replayable JSONL audit path."
    ),
    field!(
        "server.rate_limits",
        "trusted_local_exempt",
        "boolean",
        "Exempt verified local clients from rate limits.",
        BOOL
    ),
    field!(
        "server.rate_limits",
        "idle_ttl_secs",
        "integer",
        "Idle rate-bucket lifetime."
    ),
    field!(
        "server.rate_limits.control",
        "refill_per_second",
        "number",
        "Control request tokens replenished per second."
    ),
    field!(
        "server.rate_limits.control",
        "burst",
        "number",
        "Control request burst capacity."
    ),
    field!(
        "server.rate_limits.control",
        "cost",
        "number",
        "Token cost per control request."
    ),
    field!(
        "server.rate_limits.interactive",
        "refill_per_second",
        "number",
        "Interactive request tokens per second."
    ),
    field!(
        "server.rate_limits.interactive",
        "burst",
        "number",
        "Interactive request burst capacity."
    ),
    field!(
        "server.rate_limits.interactive",
        "cost",
        "number",
        "Token cost per interactive request."
    ),
    field!(
        "server.rate_limits.query",
        "refill_per_second",
        "number",
        "Query tokens replenished per second."
    ),
    field!(
        "server.rate_limits.query",
        "burst",
        "number",
        "Query request burst capacity."
    ),
    field!(
        "server.rate_limits.query",
        "cost",
        "number",
        "Token cost per query."
    ),
    field!(
        "server.rate_limits.heavy_transfer",
        "refill_per_second",
        "number",
        "Heavy-transfer tokens per second."
    ),
    field!(
        "server.rate_limits.heavy_transfer",
        "burst",
        "number",
        "Heavy-transfer burst capacity."
    ),
    field!(
        "server.rate_limits.heavy_transfer",
        "cost",
        "number",
        "Token cost per heavy transfer."
    ),
    field!(
        "server.rate_limits.stream_bytes",
        "refill_per_second",
        "number",
        "Stream byte tokens per second."
    ),
    field!(
        "server.rate_limits.stream_bytes",
        "burst",
        "number",
        "Stream byte burst capacity."
    ),
    field!(
        "server.rate_limits.stream_bytes",
        "cost",
        "number",
        "Token cost per streamed unit."
    ),
    field!(
        "server.tenant_limits",
        "trusted_local_unlimited",
        "boolean",
        "Exempt trusted local tenants from resource ceilings.",
        BOOL
    ),
    field!(
        "server.tenant_limits.defaults",
        "connection_profiles",
        "optional integer",
        "Default connection-profile ceiling."
    ),
    field!(
        "server.tenant_limits.defaults",
        "sessions",
        "optional integer",
        "Default session ceiling."
    ),
    field!(
        "server.tenant_limits.defaults",
        "connections",
        "optional integer",
        "Default open-connection ceiling."
    ),
    field!(
        "server.tenant_limits.defaults",
        "concurrent_queries",
        "optional integer",
        "Default concurrent-query ceiling."
    ),
    field!(
        "server.tenant_limits.defaults",
        "cursors",
        "optional integer",
        "Default cursor ceiling."
    ),
    field!(
        "server.tenant_limits.defaults",
        "retained_result_bytes",
        "optional integer",
        "Default retained-result byte ceiling."
    ),
    field!(
        "server.tenant_limits.ceilings",
        "connection_profiles",
        "optional integer",
        "Maximum configurable connection-profile ceiling."
    ),
    field!(
        "server.tenant_limits.ceilings",
        "sessions",
        "optional integer",
        "Maximum configurable session ceiling."
    ),
    field!(
        "server.tenant_limits.ceilings",
        "connections",
        "optional integer",
        "Maximum configurable open-connection ceiling."
    ),
    field!(
        "server.tenant_limits.ceilings",
        "concurrent_queries",
        "optional integer",
        "Maximum configurable concurrent-query ceiling."
    ),
    field!(
        "server.tenant_limits.ceilings",
        "cursors",
        "optional integer",
        "Maximum configurable cursor ceiling."
    ),
    field!(
        "server.tenant_limits.ceilings",
        "retained_result_bytes",
        "optional integer",
        "Maximum configurable retained-result bytes."
    ),
    field!(
        "automation",
        "unattended_apply",
        "enum",
        "Maximum risk allowed for unattended apply.",
        &["disabled", "standard-risk"]
    ),
    field!(
        "auth.github",
        "flow",
        "enum",
        "GitHub authentication flow.",
        &["local-device", "hosted-code"]
    ),
    field!(
        "auth.github",
        "client_id",
        "string",
        "GitHub OAuth App client id."
    ),
    field!(
        "auth.github",
        "client_secret",
        "credential slot",
        "Logical OAuth secret slot; never secret bytes."
    ),
    field!(
        "auth.admission",
        "mode",
        "enum",
        "Principal admission policy.",
        &["allowlist"]
    ),
    field!(
        "identity.github_principals",
        "name",
        "string",
        "Stable logical principal name."
    ),
    field!(
        "identity.github_principals",
        "subject",
        "decimal string",
        "Immutable GitHub numeric user id."
    ),
    field!(
        "identity.github_principals",
        "login_hint",
        "string",
        "Display-only GitHub login hint."
    ),
    field!(
        "identity.github_principals",
        "instance_admin",
        "boolean",
        "Grant instance administration.",
        BOOL
    ),
    field!(
        "identity.github_principals",
        "bootstrap",
        "boolean",
        "Mark the single bootstrap administrator.",
        BOOL
    ),
    field!("tenants", "name", "string", "Stable tenant name."),
    field!(
        "tenants.memberships",
        "principal",
        "string",
        "Logical principal reference."
    ),
    field!(
        "tenants.memberships",
        "role",
        "enum",
        "Tenant authorization role.",
        &["owner", "editor", "viewer"]
    ),
    field!(
        "connections",
        "name",
        "string",
        "Stable connection profile name."
    ),
    field!("connections", "tenant", "string", "Owning tenant name."),
    field!(
        "connections",
        "provider",
        "enum",
        "Database driver provider.",
        &["postgres", "sql-server"]
    ),
    field!(
        "connections",
        "connection_string",
        "credential-free string",
        "Public endpoint, database, and username; no password."
    ),
    field!(
        "connections",
        "credential_mode",
        "enum",
        "Shared slot or per-user credentials.",
        &["shared", "per-user"]
    ),
    field!(
        "connections",
        "credential",
        "credential slot",
        "Logical shared credential reference."
    ),
    field!(
        "connections",
        "enabled",
        "boolean",
        "Format v1 requires true; remove to disable.",
        BOOL
    ),
    field!(
        "connections",
        "tags",
        "string array",
        "Search and organization tags."
    ),
    field!(
        "connections.policy",
        "allow_sql",
        "boolean",
        "Permit SQL execution.",
        BOOL
    ),
    field!(
        "connections.policy",
        "allow_schema_read",
        "boolean",
        "Permit schema inspection.",
        BOOL
    ),
    field!(
        "connections.policy",
        "allow_export",
        "boolean",
        "Permit data export.",
        BOOL
    ),
    field!(
        "connections.policy",
        "minimum_tenant_role",
        "enum",
        "Lowest tenant role allowed to use this connection.",
        &["\"member\"", "\"viewer\"", "\"admin\"", "\"owner\""]
    ),
    field!(
        "connections.policy",
        "read_only",
        "boolean",
        "Reject SQL writes while retaining query access.",
        BOOL
    ),
    field!(
        "connections.policy",
        "allowed_ops",
        "operation array",
        "Optional operation allowlist; omit for unrestricted operations."
    ),
    field!(
        "connections.policy",
        "blocked_ops",
        "operation array",
        "Operations denied even when present in allowed_ops."
    ),
    field!(
        "connections.policy",
        "allowed_schemas",
        "schema selector array",
        "Optional catalog/schema allowlist; omit for unrestricted schemas."
    ),
    field!(
        "connections.lifecycle",
        "prevent_destroy",
        "boolean",
        "Block removal even with destroy approval.",
        BOOL
    ),
    field!("extensions", "name", "string", "Stable extension name."),
    field!(
        "extensions",
        "version",
        "exact semver",
        "Pinned extension version."
    ),
    field!(
        "extensions",
        "artifact",
        "HTTPS URL",
        "Pinned extension artifact."
    ),
    field!("extensions", "sha256", "digest", "Artifact SHA-256 digest."),
    field!(
        "extensions",
        "publisher_key",
        "SHA-256 digest",
        "Fingerprint of the trusted Ed25519 publisher key."
    ),
    field!(
        "extensions",
        "publisher_public_key",
        "base64 key",
        "Public Ed25519 key used to verify the package signature."
    ),
    field!(
        "extensions",
        "grants",
        "string array",
        "Explicit extension capabilities."
    ),
];

pub fn manifest_diagnostics(source: &str) -> Vec<ManifestDiagnostic> {
    match Manifest::parse(source) {
        Ok(_) => Vec::new(),
        Err(ConfigError::ManifestToml(message)) => {
            let range = toml::from_str::<toml::Value>(source)
                .err()
                .and_then(|error| error.span())
                .unwrap_or(0..source.len().min(1));
            vec![ManifestDiagnostic {
                range,
                path: None,
                message: format!("invalid TOML: {message}"),
            }]
        }
        Err(ConfigError::Validation { path, message }) => vec![ManifestDiagnostic {
            range: find_path_range(source, &path),
            path: Some(path.clone()),
            message: format!("{path}: {message}"),
        }],
        Err(error) => vec![ManifestDiagnostic {
            range: 0..source.len().min(1),
            path: None,
            message: error.to_string(),
        }],
    }
}

pub fn manifest_completions(
    source: &str,
    cursor: usize,
) -> (Range<usize>, Vec<ManifestCompletion>) {
    let cursor = cursor.min(source.len());
    let line_start = source[..cursor].rfind('\n').map_or(0, |offset| offset + 1);
    let before = &source[line_start..cursor];
    let table = active_table(source, line_start);
    if let Some(eq) = before.find('=') {
        let key = before[..eq].trim();
        let value_start =
            line_start + eq + 1 + before[eq + 1..].len() - before[eq + 1..].trim_start().len();
        let raw_prefix = &source[value_start..cursor];
        let prefix = raw_prefix.trim_matches('"');
        let replaced = if raw_prefix.starts_with('"') {
            value_start..cursor
        } else {
            (cursor - prefix.len())..cursor
        };
        let candidates = FIELDS
            .iter()
            .find(|field| field.table == table && field.key == key)
            .into_iter()
            .flat_map(|field| {
                field
                    .choices
                    .iter()
                    .copied()
                    .map(move |choice| ManifestCompletion {
                        label: choice.into(),
                        insertion: if field.value_type == "string"
                            || field.value_type.contains("enum")
                            || field.value_type.contains("backend")
                        {
                            format!("\"{choice}\"")
                        } else {
                            choice.into()
                        },
                        detail: field.documentation.into(),
                    })
            })
            .filter(|candidate| candidate.label.starts_with(prefix))
            .collect();
        return (replaced, candidates);
    }
    let key_start = before
        .rfind(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .map_or(line_start, |offset| line_start + offset + 1);
    let prefix = &source[key_start..cursor];
    let candidates = FIELDS
        .iter()
        .filter(|field| field.table == table && field.key.starts_with(prefix))
        .map(|field| ManifestCompletion {
            label: field.key.into(),
            insertion: format!("{} = ", field.key),
            detail: format!("{} · {}", field.value_type, field.documentation),
        })
        .collect();
    (key_start..cursor, candidates)
}

pub fn manifest_hover(source: &str, offset: usize) -> Option<ManifestHover> {
    let line_start = source[..offset.min(source.len())]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    let line_end = source[offset.min(source.len())..]
        .find('\n')
        .map_or(source.len(), |at| offset.min(source.len()) + at);
    let line = &source[line_start..line_end];
    let key_end = line.find('=')?;
    let key = line[..key_end].trim();
    let key_at = line.find(key)? + line_start;
    let range = key_at..key_at + key.len();
    if !range.contains(&offset.min(source.len())) && offset != range.end {
        return None;
    }
    let table = active_table(source, line_start);
    let field = FIELDS
        .iter()
        .find(|field| field.table == table && field.key == key)?;
    Some(ManifestHover {
        range,
        path: if table.is_empty() {
            key.into()
        } else {
            format!("{table}.{key}")
        },
        value_type: field.value_type,
        documentation: field.documentation,
        choices: field.choices,
    })
}

pub fn manifest_outline(source: &str) -> Vec<ManifestOutlineItem> {
    let mut items = Vec::new();
    for (offset, line) in lines_with_offsets(source) {
        let trimmed = line.trim();
        let (array, path) = if let Some(path) = trimmed
            .strip_prefix("[[")
            .and_then(|value| value.strip_suffix("]]"))
        {
            (true, path)
        } else if let Some(path) = trimmed
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            (false, path)
        } else {
            continue;
        };
        let label = if array {
            following_name(source, offset + line.len()).map_or_else(
                || path.rsplit('.').next().unwrap_or(path).to_owned(),
                |name| format!("{}: {name}", path.rsplit('.').next().unwrap_or(path)),
            )
        } else {
            path.rsplit('.').next().unwrap_or(path).to_owned()
        };
        items.push(ManifestOutlineItem {
            title: label,
            path: path.into(),
            offset,
            depth: path.matches('.').count(),
        });
    }
    items
}

fn active_table(source: &str, before: usize) -> &str {
    source[..before]
        .lines()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("[[")
                .and_then(|value| value.strip_suffix("]]"))
                .or_else(|| {
                    trimmed
                        .strip_prefix('[')
                        .and_then(|value| value.strip_suffix(']'))
                })
        })
        .unwrap_or("")
}

fn find_path_range(source: &str, path: &str) -> Range<usize> {
    let key = path
        .rsplit('.')
        .next()
        .unwrap_or(path)
        .split('[')
        .next()
        .unwrap_or(path);
    for (offset, line) in lines_with_offsets(source) {
        let trimmed = line.trim_start();
        if trimmed
            .strip_prefix(key)
            .is_some_and(|tail| tail.trim_start().starts_with('='))
        {
            let start = offset + line.len() - trimmed.len();
            return start..start + key.len();
        }
    }
    0..source.len().min(1)
}

fn following_name(source: &str, from: usize) -> Option<String> {
    source[from..]
        .lines()
        .take_while(|line| !line.trim_start().starts_with('['))
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "name").then(|| value.trim().trim_matches('"').to_owned())
        })
}

fn lines_with_offsets(source: &str) -> impl Iterator<Item = (usize, &str)> {
    source.split_inclusive('\n').scan(0, |offset, line| {
        let start = *offset;
        *offset += line.len();
        Some((start, line.trim_end_matches('\n')))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_is_scoped_to_the_active_table_and_enum() {
        let source = "[server]\ndep";
        let (range, candidates) = manifest_completions(source, source.len());
        assert_eq!(&source[range], "dep");
        assert_eq!(candidates[0].label, "deployment");

        let source = "[server]\ndeployment = \"te";
        let (_, candidates) = manifest_completions(source, source.len());
        assert_eq!(candidates[0].label, "team");
        assert_eq!(candidates[0].insertion, "\"team\"");
        assert_eq!(
            &source[manifest_completions(source, source.len()).0],
            "\"te"
        );
    }

    #[test]
    fn semantic_diagnostic_points_at_the_relevant_key() {
        let source = include_str!("../../../examples/reproducible-instance/sift.toml")
            .replace("kind = \"sift-instance\"", "kind = \"wrong\"");
        let diagnostics = manifest_diagnostics(&source);
        assert_eq!(&source[diagnostics[0].range.clone()], "kind");
        assert_eq!(diagnostics[0].path.as_deref(), Some("kind"));
    }

    #[test]
    fn outline_names_repeated_resources() {
        let source = "[[connections]]\nname = \"warehouse\"\nprovider = \"postgres\"\n";
        assert_eq!(manifest_outline(source)[0].title, "connections: warehouse");
    }

    #[test]
    fn configuration_wiki_covers_every_schema_field_and_table() {
        let wiki = include_str!("../../../docs/keyboard-wiki/configuration.html");
        for field in FIELDS {
            assert!(
                wiki.contains(&format!("<code>{}</code>", field.key)),
                "configuration wiki is missing key {}.{}",
                field.table,
                field.key
            );
            if !field.table.is_empty() {
                assert!(
                    wiki.contains(field.table),
                    "configuration wiki is missing table {}",
                    field.table
                );
            }
        }
    }
}
