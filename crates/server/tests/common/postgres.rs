//! Disposable socket-only PostgreSQL cluster shared by native acceptance tests.
use std::io::Write;
use std::process::{Command, Output, Stdio};

pub(crate) struct Cluster(pub(crate) tempfile::TempDir);

impl Cluster {
    fn command(&self, program: &str, args: &[&str]) -> Output {
        let output = Command::new(program)
            .args(args)
            .output()
            .expect("PostgreSQL dev tools must be installed");
        assert!(
            output.status.success(),
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    pub(crate) fn psql(&self) -> Command {
        let mut command = Command::new("psql");
        command.args([
            "-X",
            "-A",
            "-t",
            "-v",
            "ON_ERROR_STOP=1",
            "-h",
            self.0.path().to_str().unwrap(),
            "-U",
            "sift",
            "-d",
            "postgres",
        ]);
        command
    }

    pub(crate) fn sql(&self, sql: &str) -> String {
        let output = self.psql().args(["-c", sql]).output().unwrap();
        assert!(
            output.status.success(),
            "fixture SQL failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    pub(crate) fn start() -> Self {
        let cluster = Self(tempfile::tempdir().unwrap());
        let data = cluster.0.path().join("data");
        cluster.command(
            "initdb",
            &[
                "-D",
                data.to_str().unwrap(),
                "-U",
                "sift",
                "--auth=trust",
                "--no-locale",
                "--encoding=UTF8",
            ],
        );
        let socket = cluster
            .0
            .path()
            .to_str()
            .unwrap()
            .replace('\\', "\\\\")
            .replace('\'', "''");
        writeln!(std::fs::OpenOptions::new().append(true).open(data.join("postgresql.conf")).unwrap(),
            "\nlisten_addresses = ''\nshared_buffers = '16MB'\nmax_connections = 10\nunix_socket_directories = '{socket}'\nlogging_collector = on\nlog_destination = 'jsonlog'\nlog_directory = 'logs'\nlog_filename = 'deadlocks.log'\nlog_timezone = 'UTC'\ndeadlock_timeout = '100ms'").unwrap();
        cluster.command(
            "pg_ctl",
            &[
                "-D",
                data.to_str().unwrap(),
                "-l",
                cluster.0.path().join("startup.log").to_str().unwrap(),
                "-w",
                "start",
            ],
        );
        cluster
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = Command::new("pg_ctl")
            .args([
                "-D",
                self.0.path().join("data").to_str().unwrap(),
                "-m",
                "immediate",
                "-w",
                "stop",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
