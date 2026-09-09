//! Read-only destination preflight for the explicit PostgreSQL restore target.
use super::{execute, Spec};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

#[derive(Debug, Deserialize, Serialize)]
pub struct TargetValidation {
    pub database: String,
    pub user: String,
    pub server_version_num: u32,
    pub source_major_version: u32,
    #[serde(default)]
    pub dump_tool_major_version: u32,
    pub empty: bool,
    pub writable: bool,
    pub can_create: bool,
}

const TARGET_SQL: &str = "SELECT json_build_object('database',current_database(),'user',current_user,'server_version_num',current_setting('server_version_num')::int,'source_major_version',0,'writable',NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off','can_create',has_database_privilege(current_database(),'CREATE'),'empty',NOT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname NOT IN ('public','information_schema') AND nspname NOT LIKE 'pg\\_%' ESCAPE '\\') AND NOT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT IN ('information_schema') AND n.nspname NOT LIKE 'pg\\_%' ESCAPE '\\') AND NOT EXISTS (SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname NOT IN ('information_schema') AND n.nspname NOT LIKE 'pg\\_%' ESCAPE '\\') AND NOT EXISTS (SELECT 1 FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname NOT IN ('information_schema') AND n.nspname NOT LIKE 'pg\\_%' ESCAPE '\\') AND NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname <> 'plpgsql') AND NOT EXISTS (SELECT 1 FROM pg_largeobject_metadata))::text";

fn archive_major(list: &str, prefix: &str) -> anyhow::Result<u32> {
    let version = list
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix(';')?
                .trim()
                .strip_prefix(prefix.trim_start_matches(';').trim_start())
        })
        .context("archive does not identify its source PostgreSQL version")?;
    let major: u32 = version
        .split('.')
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .parse()
        .context("unrecognized archive PostgreSQL version")?;
    ensure!(
        major >= 10,
        "restore preflight supports PostgreSQL 10 and later archives"
    );
    Ok(major)
}

fn validate(report: &TargetValidation, spec: &Spec) -> anyhow::Result<()> {
    ensure!(
        report.database == spec.database && report.user == spec.user,
        "connected PostgreSQL target identity differs from specification"
    );
    ensure!(
        report.server_version_num / 10000
            >= report
                .source_major_version
                .max(report.dump_tool_major_version),
        "destination PostgreSQL major version is older than the archive source or dump tool"
    );
    ensure!(
        report.empty,
        "restore destination must be empty (no user schemas, objects, extensions or large objects)"
    );
    ensure!(
        report.writable && report.can_create,
        "restore destination must be writable with database CREATE privilege"
    );
    Ok(())
}

pub async fn check(spec: &Spec, archive: &Path) -> anyhow::Result<TargetValidation> {
    let mut listing = tempfile::tempfile()?;
    let mut command = spec.command("pg_restore");
    command
        .arg("--list")
        .arg(archive)
        .env("LC_ALL", "C")
        .stdout(listing.try_clone()?);
    execute(&mut command, Duration::from_secs(spec.timeout_seconds)).await?;
    ensure!(
        listing.metadata()?.len() <= 1024 * 1024,
        "archive table of contents exceeds preflight's 1 MiB limit"
    );
    listing.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    listing.read_to_string(&mut text)?;
    let source_major_version = archive_major(&text, "; Dumped from database version: ")?;
    let dump_tool_major_version = archive_major(&text, "; Dumped by pg_dump version: ")?;
    let mut output = tempfile::tempfile()?;
    let mut command = spec.command("psql");
    command
        .args([
            "-X",
            "--no-password",
            "-A",
            "-t",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            TARGET_SQL,
        ])
        .stdout(output.try_clone()?);
    execute(
        &mut command,
        Duration::from_secs(spec.timeout_seconds.min(60)),
    )
    .await?;
    ensure!(
        output.metadata()?.len() <= 16 * 1024,
        "PostgreSQL target response exceeds preflight limit"
    );
    output.seek(SeekFrom::Start(0))?;
    let mut report: TargetValidation =
        serde_json::from_reader(output).context("invalid PostgreSQL target response")?;
    report.source_major_version = source_major_version;
    report.dump_tool_major_version = dump_tool_major_version;
    validate(&report, spec)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_archive_source_major_without_confusing_tool_version() {
        let prefix = "; Dumped from database version: ";
        assert_eq!(
            archive_major(
                ";     Dumped from database version: 17.4\n; Dumped by pg_dump version: 18.2",
                prefix
            )
            .unwrap(),
            17
        );
        assert!(archive_major("; Dumped from database version: 18beta1", prefix).is_err());
        assert!(archive_major("; Dumped by pg_dump version: 18.2", prefix).is_err());
        assert!(archive_major("; Dumped from database version: 9.6", prefix).is_err());
    }

    #[test]
    fn rejects_wrong_old_nonempty_and_unwritable_targets() {
        let spec = Spec {
            tools_directory: "/tools".into(),
            host: "localhost".into(),
            port: 5432,
            database: "destination".into(),
            user: "owner".into(),
            password_file: None,
            ssl_mode: "disable".into(),
            timeout_seconds: 60,
        };
        let mut report = TargetValidation {
            database: "destination".into(),
            user: "owner".into(),
            server_version_num: 180001,
            source_major_version: 17,
            dump_tool_major_version: 18,
            empty: true,
            writable: true,
            can_create: true,
        };
        validate(&report, &spec).unwrap();
        report.database = "wrong".into();
        assert!(validate(&report, &spec).is_err());
        report.database = spec.database.clone();
        report.server_version_num = 170005;
        assert!(validate(&report, &spec).is_err());
        report.server_version_num = 180001;
        report.empty = false;
        assert!(validate(&report, &spec).is_err());
        report.empty = true;
        report.writable = false;
        assert!(validate(&report, &spec).is_err());
        report.writable = true;
        report.can_create = false;
        assert!(validate(&report, &spec).is_err());
    }
}
