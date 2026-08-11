use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use serde_json::{json, Value};
use sift_metadata::{MemorySecretStore, MetadataStore};

const ARCHIVE_KEY_MARKER: &str =
    "archive-key-material-must-never-appear-0123456789abcdef0123456789abcdef";
const WRONG_KEY_MARKER: &str =
    "wrong-key-material-must-never-appear-0123456789abcdef0123456789abcdef";
const BEARER_MARKER: &str = "sift_fixture_bearer_token_must_never_appear";

fn server_command(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sift-server"));
    command.current_dir(cwd);
    for variable in [
        "SIFT_MODE",
        "SIFT_TRANSPORT",
        "SIFT_DEPLOYMENT",
        "SIFT_BIND",
        "SIFT_METADATA__SECRET_KEY_FILE",
    ] {
        command.env_remove(variable);
    }
    command
}

fn configured_command(cwd: &Path, metadata: &Path, runtime: &Path) -> Command {
    let mut command = server_command(cwd);
    command
        .env("SIFT_METADATA__ENABLED", "true")
        .env("SIFT_METADATA__PATH", metadata)
        .env("SIFT_METADATA__SECRET_BACKEND", "memory")
        .env("SIFT_METADATA__BOOTSTRAP_LOCAL", "true")
        .env("SIFT_RUNTIME__STATE_DIR", runtime)
        .env("SIFT_AUTH__BEARER_TOKEN", BEARER_MARKER);
    command
}

fn successful_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "command failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_redacted(&output);
    serde_json::from_slice(&output.stdout).expect("stdout is one structured JSON document")
}

fn assert_redacted(output: &Output) {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for secret in [ARCHIVE_KEY_MARKER, WRONG_KEY_MARKER, BEARER_MARKER] {
        assert!(
            !combined.contains(secret),
            "CLI disclosed fixture secret material"
        );
    }
}

fn write_private_key(path: &Path, value: &str) {
    use std::io::Write as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).unwrap();
    writeln!(file, "{value}").unwrap();
}

fn fixture(path: &str) -> Value {
    serde_json::from_str(path).unwrap()
}

#[test]
fn backup_cli_output_is_stable_structured_and_redacted() {
    let directory = tempfile::tempdir().unwrap();
    let metadata = directory.path().join("source-metadata.sqlite");
    let runtime = directory.path().join("source-runtime");
    let archive = directory.path().join("state.sift-backup");
    let key = directory.path().join("backup.key");
    let wrong_key = directory.path().join("wrong-backup.key");
    write_private_key(&key, ARCHIVE_KEY_MARKER);
    write_private_key(&wrong_key, WRONG_KEY_MARKER);

    let store = MetadataStore::open(&metadata, Arc::new(MemorySecretStore::new())).unwrap();
    store.apply_migrations(false).unwrap();
    store.bootstrap_local("fixture user").unwrap();
    drop(store);

    let create = successful_json(
        configured_command(directory.path(), &metadata, &runtime)
            .args(["backup", "create", "--output"])
            .arg(&archive)
            .arg("--key-file")
            .arg(&key)
            .output()
            .unwrap(),
    );
    let inspect = successful_json(
        server_command(directory.path())
            .args(["backup", "inspect", "--archive"])
            .arg(&archive)
            .arg("--key-file")
            .arg(&key)
            .output()
            .unwrap(),
    );
    assert_eq!(
        inspect, create,
        "create and inspect expose the same manifest"
    );

    let mut normalized_manifest = create;
    normalized_manifest["created_at"] = json!("<timestamp>");
    normalized_manifest["sift_version"] = json!("<version>");
    for payload in normalized_manifest["payloads"].as_array_mut().unwrap() {
        payload["size"] = json!(0);
        payload["sha256"] = json!("<sha256>");
    }
    assert_eq!(
        normalized_manifest,
        fixture(include_str!("fixtures/backup-manifest-v1.json")),
        "manifest v1 changed; update the fixture only with an intentional format decision"
    );

    let destination_metadata = directory.path().join("destination-metadata.sqlite");
    let destination_runtime = directory.path().join("destination-runtime");
    let mut restore = successful_json(
        configured_command(
            directory.path(),
            &destination_metadata,
            &destination_runtime,
        )
        .args(["backup", "restore", "--archive"])
        .arg(&archive)
        .arg("--key-file")
        .arg(&key)
        .output()
        .unwrap(),
    );
    assert!(
        !destination_metadata.exists(),
        "dry run mutated the destination"
    );
    restore["archive"] = json!("<archive>");
    assert_eq!(
        restore,
        fixture(include_str!("fixtures/restore-report-v1.json")),
        "restore report v1 changed without updating its lifecycle fixture"
    );

    let failure = server_command(directory.path())
        .args(["backup", "inspect", "--archive"])
        .arg(&archive)
        .arg("--key-file")
        .arg(&wrong_key)
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert_redacted(&failure);
}

#[test]
fn remote_lifecycle_output_is_stable_structured_and_redacted() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("remote-state");

    let migration = successful_json(
        server_command(directory.path())
            .args(["remote", "migrate", "--state-dir"])
            .arg(&state)
            .output()
            .unwrap(),
    );
    assert_eq!(
        migration.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["migration", "upgraded_documents"]
    );
    assert_eq!(migration["upgraded_documents"], 0);
    let report = migration["migration"].as_object().unwrap();
    assert_eq!(
        report.keys().collect::<Vec<_>>(),
        vec!["applied", "backup", "from_version", "to_version"]
    );
    assert_eq!(migration["migration"]["from_version"], 0);
    assert_eq!(migration["migration"]["to_version"], 36);
    assert_eq!(
        migration["migration"]["applied"].as_array().unwrap().len(),
        36
    );
    for descriptor in migration["migration"]["applied"].as_array().unwrap() {
        assert_eq!(
            descriptor.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["automatic", "kind", "name", "version"]
        );
    }

    let secret_key = std::fs::read_to_string(state.join("secret.key")).unwrap();
    let migration_text = serde_json::to_string(&migration).unwrap();
    assert!(!migration_text.contains(secret_key.trim()));
    assert!(!migration_text.contains(&state.display().to_string()));

    let second = successful_json(
        server_command(directory.path())
            .args(["remote", "migrate", "--state-dir"])
            .arg(&state)
            .output()
            .unwrap(),
    );
    assert_eq!(second["migration"]["from_version"], 36);
    assert_eq!(second["migration"]["to_version"], 36);
    assert_eq!(second["migration"]["applied"], json!([]));
    let second_text = serde_json::to_string(&second).unwrap();
    assert!(!second_text.contains(secret_key.trim()));
    assert!(!second_text.contains(&state.display().to_string()));

    let mut probe = successful_json(
        server_command(directory.path())
            .args(["remote", "probe", "--state-dir"])
            .arg(&state)
            .output()
            .unwrap(),
    );
    probe["server_version"] = json!("<version>");
    probe["target_os"] = json!("<os>");
    probe["target_arch"] = json!("<arch>");
    assert_eq!(
        probe,
        fixture(include_str!("fixtures/remote-probe-v1.json")),
        "remote probe schema v1 changed without updating its lifecycle fixture"
    );
    let probe_text = serde_json::to_string(&probe).unwrap();
    assert!(!probe_text.contains(secret_key.trim()));
    assert!(!probe_text.contains(&state.display().to_string()));
}
