//! Hardened bundled Git adapter.
//!
//! The command shape follows the repository-boundary pattern used by Zed:
//! structured argv, typed parsed responses, one fixed executable observation,
//! no shell, and repository configuration disabled where it could execute code.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sift_protocol::{
    VcsBranch, VcsConflictKind, VcsDiff, VcsDiffFile, VcsDiffHunk, VcsDiffLine, VcsDiffLineKind,
    VcsDiffSide, VcsFileState, VcsStageState, VcsStatus, VcsStatusEntry, VcsUpstreamStatus,
    WorkspacePath, WorkspaceRevision,
};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

use crate::config::VcsConfig;

pub const GIT_ADAPTER_ID: &str = "sift/git";
pub const GIT_ADAPTER_GENERATION: &str = "git-v1";
const MAX_GIT_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STATUS_ENTRIES: usize = 20_000;
const MAX_DIFF_FILES: usize = 2_000;
const MAX_DIFF_HUNKS: usize = 4_000;
const MAX_DIFF_LINES: usize = 200_000;

#[derive(Debug, thiserror::Error)]
pub enum GitAdapterError {
    #[error("Git integration is disabled")]
    Disabled,
    #[error("the configured Git executable is unavailable")]
    ExecutableUnavailable,
    #[error("the projection is not a Git repository")]
    NotRepository,
    #[error("Git input or output is invalid")]
    InvalidData,
    #[error("Git output exceeds the configured ceiling")]
    OutputLimit,
    #[error("Git operation timed out")]
    TimedOut,
    #[error("Git operation failed with exit code {0:?}")]
    CommandFailed(Option<i32>),
    #[error("Git network operations are disabled")]
    NetworkDisabled,
    #[error("Git credential helper is unavailable")]
    CredentialHelperUnavailable,
    #[error("Git process I/O failed")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, GitAdapterError>;

#[derive(Clone)]
pub struct GitAdapter {
    executable: PathBuf,
    executable_version: String,
    local_timeout: Duration,
    network_timeout: Duration,
    network_enabled: bool,
    askpass_executable: Option<PathBuf>,
}

#[derive(Clone)]
pub struct GitCredential {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct GitRepositoryObservation {
    pub identity: String,
    pub branch: Option<String>,
    pub head: Option<String>,
}

#[derive(Debug)]
struct GitOutput {
    stdout: Vec<u8>,
    truncated: bool,
}

#[async_trait]
pub trait VcsRepository: Send + Sync {
    fn adapter_id(&self) -> &'static str;
    fn generation(&self) -> &'static str;
    fn executable_version(&self) -> &str;
    fn network_enabled(&self) -> bool;
    async fn discover(&self, worktree: &Path) -> Result<GitRepositoryObservation>;
    async fn initialize(&self, worktree: &Path) -> Result<GitRepositoryObservation>;
    async fn status(
        &self,
        worktree: &Path,
        binding_id: sift_protocol::RepositoryBindingId,
        binding_revision: u64,
        workspace_revision: WorkspaceRevision,
    ) -> Result<VcsStatus>;
    async fn diff(
        &self,
        worktree: &Path,
        binding_id: sift_protocol::RepositoryBindingId,
        side: VcsDiffSide,
        path: Option<&WorkspacePath>,
    ) -> Result<VcsDiff>;
    async fn branches(&self, worktree: &Path) -> Result<Vec<VcsBranch>>;
    async fn stage(&self, worktree: &Path, paths: &[WorkspacePath]) -> Result<()>;
    async fn unstage(&self, worktree: &Path, paths: &[WorkspacePath]) -> Result<()>;
    async fn apply_hunk(
        &self,
        worktree: &Path,
        file: &VcsDiffFile,
        hunk: &VcsDiffHunk,
        reverse: bool,
    ) -> Result<()>;
    async fn apply_lines(
        &self,
        worktree: &Path,
        file: &VcsDiffFile,
        hunk: &VcsDiffHunk,
        line_indices: &[u32],
        stage: bool,
    ) -> Result<()>;
    async fn commit(
        &self,
        worktree: &Path,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<GitRepositoryObservation>;
    async fn amend(
        &self,
        worktree: &Path,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<GitRepositoryObservation>;
    async fn soft_reset_parent(&self, worktree: &Path) -> Result<GitRepositoryObservation>;
    async fn fetch(
        &self,
        worktree: &Path,
        remote: &str,
        credential: GitCredential,
    ) -> Result<GitRepositoryObservation>;
    async fn push(
        &self,
        worktree: &Path,
        remote: &str,
        branch: Option<&str>,
        credential: GitCredential,
    ) -> Result<GitRepositoryObservation>;
}

impl GitAdapter {
    pub async fn from_config(config: &VcsConfig) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        let executable = match &config.executable {
            Some(path) => {
                std::fs::canonicalize(path).map_err(|_| GitAdapterError::ExecutableUnavailable)?
            }
            None => find_executable("git").ok_or(GitAdapterError::ExecutableUnavailable)?,
        };
        let mut probe = Self {
            executable,
            executable_version: String::new(),
            local_timeout: Duration::from_secs(config.local_timeout_secs),
            network_timeout: Duration::from_secs(config.network_timeout_secs),
            network_enabled: config.network_enabled,
            askpass_executable: find_sibling_askpass(),
        };
        let output = probe.run(Path::new("/"), ["--version"], false, &[]).await?;
        let version = String::from_utf8(output.stdout).map_err(|_| GitAdapterError::InvalidData)?;
        probe.executable_version = version.trim().to_string();
        Ok(Some(probe))
    }

    #[cfg(test)]
    pub async fn for_tests() -> Result<Self> {
        Self::from_config(&VcsConfig {
            enabled: true,
            network_enabled: false,
            ..VcsConfig::default()
        })
        .await?
        .ok_or(GitAdapterError::Disabled)
    }

    async fn observation(&self, worktree: &Path) -> Result<GitRepositoryObservation> {
        let git_dir = self
            .run(worktree, ["rev-parse", "--absolute-git-dir"], false, &[])
            .await
            .map_err(|error| match error {
                GitAdapterError::CommandFailed(_) => GitAdapterError::NotRepository,
                other => other,
            })?;
        let git_dir = String::from_utf8(git_dir.stdout)
            .map_err(|_| GitAdapterError::InvalidData)?
            .trim()
            .to_string();
        let identity = format!("{:x}", Sha256::digest(git_dir.as_bytes()));
        let branch = self
            .run(
                worktree,
                ["symbolic-ref", "--quiet", "--short", "HEAD"],
                false,
                &[],
            )
            .await
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let head = self
            .run(worktree, ["rev-parse", "--verify", "HEAD"], false, &[])
            .await
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| is_oid(value));
        Ok(GitRepositoryObservation {
            identity,
            branch,
            head,
        })
    }

    async fn run<I, S>(
        &self,
        worktree: &Path,
        args: I,
        network: bool,
        environment: &[(OsString, OsString)],
    ) -> Result<GitOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_with_output_policy(worktree, args, network, environment, false, None)
            .await
    }

    async fn run_truncated<I, S>(
        &self,
        worktree: &Path,
        args: I,
        network: bool,
        environment: &[(OsString, OsString)],
    ) -> Result<GitOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_with_output_policy(worktree, args, network, environment, true, None)
            .await
    }

    async fn run_with_input<I, S>(
        &self,
        worktree: &Path,
        args: I,
        input: &[u8],
    ) -> Result<GitOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_with_output_policy(worktree, args, false, &[], false, Some(input))
            .await
    }

    async fn run_with_output_policy<I, S>(
        &self,
        worktree: &Path,
        args: I,
        network: bool,
        environment: &[(OsString, OsString)],
        allow_truncated_stdout: bool,
        input: Option<&[u8]>,
    ) -> Result<GitOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        if network && !self.network_enabled {
            return Err(GitAdapterError::NetworkDisabled);
        }
        let mut command = Command::new(&self.executable);
        command
            .current_dir(worktree)
            .kill_on_drop(true)
            .env_clear()
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null_device())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .arg("-c")
            .arg("core.fsmonitor=false")
            .arg("-c")
            .arg("log.showSignature=false")
            .arg("--no-optional-locks")
            .arg("--no-pager")
            .arg("-c")
            .arg(format!("core.hooksPath={}", null_device()))
            .arg("-c")
            .arg("core.sshCommand=ssh")
            .arg("-c")
            .arg("credential.helper=")
            .arg("-c")
            .arg("protocol.ext.allow=never")
            .arg("-c")
            .arg("diff.external=")
            .args(args)
            .envs(environment.iter().cloned())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if let Some(input) = input {
            let mut stdin = child.stdin.take().ok_or(GitAdapterError::InvalidData)?;
            stdin.write_all(input).await?;
            stdin.shutdown().await?;
        }
        let stdout = child.stdout.take().ok_or(GitAdapterError::InvalidData)?;
        let stderr = child.stderr.take().ok_or(GitAdapterError::InvalidData)?;
        let timeout = if network {
            self.network_timeout
        } else {
            self.local_timeout
        };
        let result = tokio::time::timeout(timeout, async {
            let (status, stdout, stderr) = tokio::try_join!(
                child.wait(),
                read_bounded(stdout, MAX_GIT_OUTPUT_BYTES),
                read_bounded(stderr, MAX_GIT_OUTPUT_BYTES),
            )?;
            Ok::<_, std::io::Error>((status, stdout, stderr))
        })
        .await
        .map_err(|_| GitAdapterError::TimedOut)??;
        if (!allow_truncated_stdout && result.1 .1) || result.2 .1 {
            return Err(GitAdapterError::OutputLimit);
        }
        if !result.0.success() {
            return Err(GitAdapterError::CommandFailed(result.0.code()));
        }
        Ok(GitOutput {
            stdout: result.1 .0,
            truncated: result.1 .1,
        })
    }

    async fn run_network<I, S>(
        &self,
        worktree: &Path,
        args: I,
        credential: GitCredential,
    ) -> Result<GitOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        #[cfg(unix)]
        {
            let askpass = self
                .askpass_executable
                .as_ref()
                .ok_or(GitAdapterError::CredentialHelperUnavailable)?;
            let directory = tempfile::Builder::new()
                .prefix("sift-git-askpass-")
                .tempdir()?;
            let socket = directory.path().join("credential.sock");
            let listener = tokio::net::UnixListener::bind(&socket)?;
            let server = tokio::spawn(serve_credential(listener, credential));
            let environment = vec![
                (
                    OsString::from("GIT_ASKPASS"),
                    askpass.as_os_str().to_owned(),
                ),
                (
                    OsString::from("SSH_ASKPASS"),
                    askpass.as_os_str().to_owned(),
                ),
                (
                    OsString::from("SSH_ASKPASS_REQUIRE"),
                    OsString::from("force"),
                ),
                (
                    OsString::from("SIFT_GIT_ASKPASS_SOCKET"),
                    socket.as_os_str().to_owned(),
                ),
            ];
            let result = self.run(worktree, args, true, &environment).await;
            server.abort();
            let _ = server.await;
            result
        }
        #[cfg(not(unix))]
        {
            let _ = (worktree, args, credential);
            Err(GitAdapterError::CredentialHelperUnavailable)
        }
    }
}

#[async_trait]
impl VcsRepository for GitAdapter {
    fn adapter_id(&self) -> &'static str {
        GIT_ADAPTER_ID
    }

    fn generation(&self) -> &'static str {
        GIT_ADAPTER_GENERATION
    }

    fn executable_version(&self) -> &str {
        &self.executable_version
    }

    fn network_enabled(&self) -> bool {
        self.network_enabled && self.askpass_executable.is_some()
    }

    async fn discover(&self, worktree: &Path) -> Result<GitRepositoryObservation> {
        self.observation(worktree).await
    }

    async fn initialize(&self, worktree: &Path) -> Result<GitRepositoryObservation> {
        self.run(worktree, ["init", "--initial-branch=main"], false, &[])
            .await?;
        self.observation(worktree).await
    }

    async fn status(
        &self,
        worktree: &Path,
        binding_id: sift_protocol::RepositoryBindingId,
        binding_revision: u64,
        workspace_revision: WorkspaceRevision,
    ) -> Result<VcsStatus> {
        let output = self
            .run(
                worktree,
                [
                    "status",
                    "--porcelain=v2",
                    "--branch",
                    "-z",
                    "--untracked-files=all",
                ],
                false,
                &[],
            )
            .await?;
        parse_status(
            binding_id,
            binding_revision,
            workspace_revision,
            &output.stdout,
        )
    }

    async fn diff(
        &self,
        worktree: &Path,
        binding_id: sift_protocol::RepositoryBindingId,
        side: VcsDiffSide,
        path: Option<&WorkspacePath>,
    ) -> Result<VcsDiff> {
        let mut args = vec![
            OsString::from("diff"),
            OsString::from("--no-ext-diff"),
            OsString::from("--name-status"),
            OsString::from("-z"),
        ];
        match side {
            VcsDiffSide::HeadToIndex => args.push(OsString::from("--cached")),
            VcsDiffSide::IndexToWorktree => {}
            VcsDiffSide::HeadToWorktree => args.push(OsString::from("HEAD")),
        }
        if let Some(path) = path {
            validate_paths(std::slice::from_ref(path))?;
            args.push(OsString::from("--"));
            args.push(OsString::from(&path.0));
        }
        let names = self.run(worktree, args, false, &[]).await?;
        let mut stat_args = vec![
            OsString::from("diff"),
            OsString::from("--no-ext-diff"),
            OsString::from("--numstat"),
            OsString::from("-z"),
        ];
        match side {
            VcsDiffSide::HeadToIndex => stat_args.push(OsString::from("--cached")),
            VcsDiffSide::IndexToWorktree => {}
            VcsDiffSide::HeadToWorktree => stat_args.push(OsString::from("HEAD")),
        }
        if let Some(path) = path {
            stat_args.push(OsString::from("--"));
            stat_args.push(OsString::from(&path.0));
        }
        let stats = self.run(worktree, stat_args, false, &[]).await?;
        let mut diff = parse_diff(binding_id, side, &names.stdout, &stats.stdout)?;
        if let Some(path) = path {
            let binary = diff.files.first().is_some_and(|file| file.binary);
            if !binary {
                let mut patch_args = vec![
                    OsString::from("diff"),
                    OsString::from("--no-ext-diff"),
                    OsString::from("--no-textconv"),
                    OsString::from("--no-color"),
                    OsString::from("--unified=3"),
                ];
                match side {
                    VcsDiffSide::HeadToIndex => patch_args.push(OsString::from("--cached")),
                    VcsDiffSide::IndexToWorktree => {}
                    VcsDiffSide::HeadToWorktree => patch_args.push(OsString::from("HEAD")),
                }
                patch_args.push(OsString::from("--"));
                patch_args.push(OsString::from(&path.0));
                let patch = self.run_truncated(worktree, patch_args, false, &[]).await?;
                let (hunks, parsed_truncated) =
                    parse_patch(side, path, &patch.stdout, patch.truncated)?;
                if let Some(file) = diff.files.first_mut() {
                    file.hunks = hunks;
                    file.content_truncated = parsed_truncated;
                }
            }
        }
        Ok(diff)
    }

    async fn branches(&self, worktree: &Path) -> Result<Vec<VcsBranch>> {
        let output = self
            .run(
                worktree,
                [
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)%00%(HEAD)%00%(upstream:short)%00%(upstream:track,nobracket)",
                    "refs/heads",
                    "refs/remotes",
                ],
                false,
                &[],
            )
            .await?;
        parse_branches(&output.stdout)
    }

    async fn stage(&self, worktree: &Path, paths: &[WorkspacePath]) -> Result<()> {
        validate_paths(paths)?;
        let mut args = vec![
            OsString::from("add"),
            OsString::from("--all"),
            OsString::from("--"),
        ];
        args.extend(paths.iter().map(|path| OsString::from(&path.0)));
        self.run(worktree, args, false, &[]).await?;
        Ok(())
    }

    async fn unstage(&self, worktree: &Path, paths: &[WorkspacePath]) -> Result<()> {
        validate_paths(paths)?;
        let has_head = self.observation(worktree).await?.head.is_some();
        let mut args = if has_head {
            vec![OsString::from("reset"), OsString::from("--")]
        } else {
            vec![
                OsString::from("rm"),
                OsString::from("--cached"),
                OsString::from("--ignore-unmatch"),
                OsString::from("--"),
            ]
        };
        args.extend(paths.iter().map(|path| OsString::from(&path.0)));
        self.run(worktree, args, false, &[]).await?;
        Ok(())
    }

    async fn apply_hunk(
        &self,
        worktree: &Path,
        file: &VcsDiffFile,
        hunk: &VcsDiffHunk,
        reverse: bool,
    ) -> Result<()> {
        validate_paths(std::slice::from_ref(&file.path))?;
        if file.binary || hunk.truncated || hunk.lines.is_empty() {
            return Err(GitAdapterError::InvalidData);
        }
        let patch = patch_for_hunk(file, hunk);
        if patch.len() > MAX_GIT_OUTPUT_BYTES {
            return Err(GitAdapterError::OutputLimit);
        }
        let mut args = vec![
            OsString::from("apply"),
            OsString::from("--cached"),
            OsString::from("--recount"),
            OsString::from("--whitespace=nowarn"),
        ];
        if reverse {
            args.push(OsString::from("--reverse"));
        }
        args.push(OsString::from("-"));
        self.run_with_input(worktree, args, patch.as_bytes())
            .await?;
        Ok(())
    }

    async fn apply_lines(
        &self,
        worktree: &Path,
        file: &VcsDiffFile,
        hunk: &VcsDiffHunk,
        line_indices: &[u32],
        stage: bool,
    ) -> Result<()> {
        validate_paths(std::slice::from_ref(&file.path))?;
        let patch = patch_for_lines(file, hunk, line_indices, stage)?;
        if patch.len() > MAX_GIT_OUTPUT_BYTES {
            return Err(GitAdapterError::OutputLimit);
        }
        self.run_with_input(
            worktree,
            vec![
                OsString::from("apply"),
                OsString::from("--cached"),
                OsString::from("--recount"),
                OsString::from("--whitespace=nowarn"),
                OsString::from("-"),
            ],
            patch.as_bytes(),
        )
        .await?;
        Ok(())
    }

    async fn commit(
        &self,
        worktree: &Path,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<GitRepositoryObservation> {
        validate_commit_identity(message, author_name, author_email)?;
        let environment = vec![
            (
                OsString::from("GIT_AUTHOR_NAME"),
                OsString::from(author_name),
            ),
            (
                OsString::from("GIT_AUTHOR_EMAIL"),
                OsString::from(author_email),
            ),
            (
                OsString::from("GIT_COMMITTER_NAME"),
                OsString::from(author_name),
            ),
            (
                OsString::from("GIT_COMMITTER_EMAIL"),
                OsString::from(author_email),
            ),
        ];
        self.run(
            worktree,
            ["commit", "--no-gpg-sign", "--no-verify", "-m", message],
            false,
            &environment,
        )
        .await?;
        self.observation(worktree).await
    }

    async fn amend(
        &self,
        worktree: &Path,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<GitRepositoryObservation> {
        validate_commit_identity(message, author_name, author_email)?;
        let environment = vec![
            (
                OsString::from("GIT_AUTHOR_NAME"),
                OsString::from(author_name),
            ),
            (
                OsString::from("GIT_AUTHOR_EMAIL"),
                OsString::from(author_email),
            ),
            (
                OsString::from("GIT_COMMITTER_NAME"),
                OsString::from(author_name),
            ),
            (
                OsString::from("GIT_COMMITTER_EMAIL"),
                OsString::from(author_email),
            ),
        ];
        self.run(
            worktree,
            [
                "commit",
                "--amend",
                "--no-gpg-sign",
                "--no-verify",
                "-m",
                message,
            ],
            false,
            &environment,
        )
        .await?;
        self.observation(worktree).await
    }

    async fn soft_reset_parent(&self, worktree: &Path) -> Result<GitRepositoryObservation> {
        self.run(worktree, ["reset", "--soft", "HEAD^"], false, &[])
            .await?;
        self.observation(worktree).await
    }

    async fn fetch(
        &self,
        worktree: &Path,
        remote: &str,
        credential: GitCredential,
    ) -> Result<GitRepositoryObservation> {
        validate_remote(remote)?;
        self.run_network(worktree, ["fetch", "--prune", remote], credential)
            .await?;
        self.observation(worktree).await
    }

    async fn push(
        &self,
        worktree: &Path,
        remote: &str,
        branch: Option<&str>,
        credential: GitCredential,
    ) -> Result<GitRepositoryObservation> {
        validate_remote(remote)?;
        if branch.is_some_and(|branch| !valid_ref_component(branch)) {
            return Err(GitAdapterError::InvalidData);
        }
        let mut args = vec![OsString::from("push"), OsString::from(remote)];
        if let Some(branch) = branch {
            args.push(OsString::from(branch));
        }
        self.run_network(worktree, args, credential).await?;
        self.observation(worktree).await
    }
}

async fn read_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        exceeded |= read > remaining;
    }
    Ok((output, exceeded))
}

fn parse_status(
    binding_id: sift_protocol::RepositoryBindingId,
    binding_revision: u64,
    workspace_revision: WorkspaceRevision,
    bytes: &[u8],
) -> Result<VcsStatus> {
    let mut branch = None;
    let mut head = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut entries = Vec::new();
    let fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut index = 0;
    while index < fields.len() {
        let field = fields[index];
        index += 1;
        if field.is_empty() {
            continue;
        }
        let line = std::str::from_utf8(field).map_err(|_| GitAdapterError::InvalidData)?;
        if let Some(value) = line.strip_prefix("# branch.head ") {
            branch = (value != "(detached)").then(|| value.to_string());
        } else if let Some(value) = line.strip_prefix("# branch.oid ") {
            head = (value != "(initial)").then(|| value.to_string());
        } else if let Some(value) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("# branch.ab ") {
            let mut counts = value.split_whitespace();
            ahead = parse_signed_count(counts.next(), '+')?;
            behind = parse_signed_count(counts.next(), '-')?;
        } else if let Some(rest) = line.strip_prefix("? ") {
            entries.push(status_entry(rest, None, "??", None)?);
        } else if line.starts_with("1 ") {
            let parts = line.splitn(9, ' ').collect::<Vec<_>>();
            if parts.len() != 9 {
                return Err(GitAdapterError::InvalidData);
            }
            entries.push(status_entry(parts[8], None, parts[1], None)?);
        } else if line.starts_with("2 ") {
            let parts = line.splitn(10, ' ').collect::<Vec<_>>();
            if parts.len() != 10 || index >= fields.len() {
                return Err(GitAdapterError::InvalidData);
            }
            let original =
                std::str::from_utf8(fields[index]).map_err(|_| GitAdapterError::InvalidData)?;
            index += 1;
            entries.push(status_entry(parts[9], Some(original), parts[1], None)?);
        } else if line.starts_with("u ") {
            let parts = line.splitn(11, ' ').collect::<Vec<_>>();
            if parts.len() != 11 {
                return Err(GitAdapterError::InvalidData);
            }
            entries.push(status_entry(
                parts[10],
                None,
                parts[1],
                Some(conflict_kind(parts[1])),
            )?);
        }
        if entries.len() > MAX_STATUS_ENTRIES {
            return Err(GitAdapterError::OutputLimit);
        }
    }
    entries.sort_by(|left, right| left.path.0.cmp(&right.path.0));
    Ok(VcsStatus {
        binding_id,
        workspace_revision,
        binding_revision,
        head_oid: head,
        branch,
        upstream: upstream.map(|value| {
            let (remote, branch) = value
                .split_once('/')
                .map_or((value.as_str(), ""), |parts| parts);
            VcsUpstreamStatus {
                remote: remote.to_string(),
                branch: branch.to_string(),
                ahead,
                behind,
            }
        }),
        entries,
        truncated: false,
        observed_at: chrono::Utc::now(),
    })
}

fn status_entry(
    path: &str,
    original: Option<&str>,
    xy: &str,
    conflict: Option<VcsConflictKind>,
) -> Result<VcsStatusEntry> {
    let mut chars = xy.chars();
    let index = chars.next().ok_or(GitAdapterError::InvalidData)?;
    let worktree = chars.next().ok_or(GitAdapterError::InvalidData)?;
    let index_state = file_state(index);
    let worktree_state = file_state(worktree);
    let stage = if conflict.is_some() {
        VcsStageState::Conflict
    } else if matches!(index_state, Some(VcsFileState::Untracked)) {
        VcsStageState::Unstaged
    } else if index_state.is_some() && worktree_state.is_some() {
        VcsStageState::PartiallyStaged
    } else if index_state.is_some() {
        VcsStageState::Staged
    } else {
        VcsStageState::Unstaged
    };
    Ok(VcsStatusEntry {
        path: checked_path(path)?,
        previous_path: original.map(checked_path).transpose()?,
        state: conflict
            .map(|_| VcsFileState::Unmerged)
            .or(worktree_state)
            .or(index_state)
            .ok_or(GitAdapterError::InvalidData)?,
        stage,
        conflict,
        pending: None,
    })
}

fn file_state(value: char) -> Option<VcsFileState> {
    match value {
        '.' | ' ' => None,
        'A' => Some(VcsFileState::Added),
        'M' => Some(VcsFileState::Modified),
        'D' => Some(VcsFileState::Deleted),
        'R' => Some(VcsFileState::Renamed),
        'C' => Some(VcsFileState::Copied),
        'T' => Some(VcsFileState::TypeChanged),
        'U' => Some(VcsFileState::Unmerged),
        '?' => Some(VcsFileState::Untracked),
        _ => Some(VcsFileState::Unmerged),
    }
}

fn conflict_kind(xy: &str) -> VcsConflictKind {
    match xy {
        "AA" => VcsConflictKind::BothAdded,
        "DD" => VcsConflictKind::BothDeleted,
        "UU" => VcsConflictKind::BothModified,
        "AU" => VcsConflictKind::AddedByUs,
        "UA" => VcsConflictKind::AddedByThem,
        "DU" => VcsConflictKind::DeletedByUs,
        "UD" => VcsConflictKind::DeletedByThem,
        _ => VcsConflictKind::Unknown,
    }
}

fn parse_diff(
    binding_id: sift_protocol::RepositoryBindingId,
    side: VcsDiffSide,
    names: &[u8],
    stats: &[u8],
) -> Result<VcsDiff> {
    let mut counts = BTreeMap::<String, (u32, u32, bool)>::new();
    for field in stats
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
    {
        let value = std::str::from_utf8(field).map_err(|_| GitAdapterError::InvalidData)?;
        let mut parts = value.splitn(3, '\t');
        let additions = parts.next().ok_or(GitAdapterError::InvalidData)?;
        let deletions = parts.next().ok_or(GitAdapterError::InvalidData)?;
        let path = parts.next().ok_or(GitAdapterError::InvalidData)?;
        let binary = additions == "-" || deletions == "-";
        counts.insert(
            path.to_string(),
            (
                if binary {
                    0
                } else {
                    additions
                        .parse()
                        .map_err(|_| GitAdapterError::InvalidData)?
                },
                if binary {
                    0
                } else {
                    deletions
                        .parse()
                        .map_err(|_| GitAdapterError::InvalidData)?
                },
                binary,
            ),
        );
    }
    let fields = names
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let mut index = 0usize;
    let mut files = Vec::new();
    while index < fields.len() {
        let status =
            std::str::from_utf8(fields[index]).map_err(|_| GitAdapterError::InvalidData)?;
        index += 1;
        let state = file_state(status.chars().next().ok_or(GitAdapterError::InvalidData)?)
            .ok_or(GitAdapterError::InvalidData)?;
        let renamed = matches!(state, VcsFileState::Renamed | VcsFileState::Copied);
        let first = fields.get(index).ok_or(GitAdapterError::InvalidData)?;
        index += 1;
        let first = std::str::from_utf8(first).map_err(|_| GitAdapterError::InvalidData)?;
        let (previous_path, path) = if renamed {
            let next = fields.get(index).ok_or(GitAdapterError::InvalidData)?;
            index += 1;
            (
                Some(checked_path(first)?),
                std::str::from_utf8(next).map_err(|_| GitAdapterError::InvalidData)?,
            )
        } else {
            (None, first)
        };
        let (additions, deletions, binary) = counts.get(path).copied().unwrap_or((0, 0, false));
        files.push(VcsDiffFile {
            path: checked_path(path)?,
            previous_path,
            state,
            old_digest: None,
            new_digest: None,
            binary,
            additions,
            deletions,
            hunks: Vec::new(),
            content_truncated: false,
        });
        if files.len() > MAX_DIFF_FILES {
            return Err(GitAdapterError::OutputLimit);
        }
    }
    files.sort_by(|left, right| left.path.0.cmp(&right.path.0));
    Ok(VcsDiff {
        binding_id,
        side,
        files,
        truncated: false,
    })
}

fn parse_patch(
    side: VcsDiffSide,
    path: &WorkspacePath,
    bytes: &[u8],
    output_truncated: bool,
) -> Result<(Vec<VcsDiffHunk>, bool)> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return Ok((Vec::new(), true)),
    };
    let mut hunks = Vec::new();
    let mut current: Option<VcsDiffHunk> = None;
    let mut old_line = 0u32;
    let mut new_line = 0u32;
    let mut line_count = 0usize;
    let mut truncated = output_truncated;

    for raw_line in text.lines() {
        if raw_line.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(finalize_hunk(side, path, hunk));
            }
            if hunks.len() >= MAX_DIFF_HUNKS {
                truncated = true;
                break;
            }
            let (old_start, old_lines, new_start, new_lines, header) = parse_hunk_header(raw_line)?;
            old_line = old_start;
            new_line = new_start;
            current = Some(VcsDiffHunk {
                id: String::new(),
                old_start,
                old_lines,
                new_start,
                new_lines,
                header,
                lines: Vec::new(),
                truncated: false,
            });
            continue;
        }
        let Some(hunk) = current.as_mut() else {
            continue;
        };
        if line_count >= MAX_DIFF_LINES {
            hunk.truncated = true;
            truncated = true;
            break;
        }
        let Some(marker) = raw_line.as_bytes().first().copied() else {
            return Err(GitAdapterError::InvalidData);
        };
        let (kind, old, new) = match marker {
            b' ' => {
                let coordinates = (Some(old_line), Some(new_line));
                old_line = old_line.saturating_add(1);
                new_line = new_line.saturating_add(1);
                (VcsDiffLineKind::Context, coordinates.0, coordinates.1)
            }
            b'+' => {
                let line = new_line;
                new_line = new_line.saturating_add(1);
                (VcsDiffLineKind::Addition, None, Some(line))
            }
            b'-' => {
                let line = old_line;
                old_line = old_line.saturating_add(1);
                (VcsDiffLineKind::Deletion, Some(line), None)
            }
            b'\\' => (VcsDiffLineKind::NoNewline, None, None),
            _ => return Err(GitAdapterError::InvalidData),
        };
        let content = raw_line.get(1..).ok_or(GitAdapterError::InvalidData)?;
        hunk.lines.push(VcsDiffLine {
            kind,
            old_line: old,
            new_line: new,
            text: content.to_owned(),
        });
        line_count += 1;
    }
    if let Some(mut hunk) = current {
        if output_truncated {
            hunk.truncated = true;
        }
        hunks.push(finalize_hunk(side, path, hunk));
    }
    Ok((hunks, truncated))
}

fn patch_for_hunk(file: &VcsDiffFile, hunk: &VcsDiffHunk) -> String {
    let old_path = if file.state == VcsFileState::Added {
        "/dev/null".to_owned()
    } else {
        format!("a/{}", file.previous_path.as_ref().unwrap_or(&file.path).0)
    };
    let new_path = if file.state == VcsFileState::Deleted {
        "/dev/null".to_owned()
    } else {
        format!("b/{}", file.path.0)
    };
    let mut patch = format!(
        "--- {old_path}\n+++ {new_path}\n@@ -{},{} +{},{} @@ {}\n",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines, hunk.header
    );
    for line in &hunk.lines {
        let marker = match line.kind {
            VcsDiffLineKind::Context => ' ',
            VcsDiffLineKind::Addition => '+',
            VcsDiffLineKind::Deletion => '-',
            VcsDiffLineKind::NoNewline => '\\',
        };
        patch.push(marker);
        patch.push_str(&line.text);
        patch.push('\n');
    }
    patch
}

fn patch_for_lines(
    file: &VcsDiffFile,
    hunk: &VcsDiffHunk,
    line_indices: &[u32],
    stage: bool,
) -> Result<String> {
    if file.binary
        || file.state != VcsFileState::Modified
        || hunk.truncated
        || hunk.lines.is_empty()
        || line_indices.is_empty()
        || line_indices.len() > hunk.lines.len()
    {
        return Err(GitAdapterError::InvalidData);
    }
    let selected = line_indices.iter().copied().collect::<BTreeSet<_>>();
    if selected.len() != line_indices.len()
        || selected.iter().any(|index| {
            hunk.lines.get(*index as usize).map_or(true, |line| {
                !matches!(
                    line.kind,
                    VcsDiffLineKind::Addition | VcsDiffLineKind::Deletion
                )
            })
        })
    {
        return Err(GitAdapterError::InvalidData);
    }

    let mut lines = Vec::with_capacity(hunk.lines.len());
    for (index, line) in hunk.lines.iter().enumerate() {
        let chosen = selected.contains(&(index as u32));
        let kind = if stage {
            match (line.kind, chosen) {
                (VcsDiffLineKind::Context, _) => Some(VcsDiffLineKind::Context),
                (VcsDiffLineKind::Addition, true) => Some(VcsDiffLineKind::Addition),
                (VcsDiffLineKind::Addition, false) => None,
                (VcsDiffLineKind::Deletion, true) => Some(VcsDiffLineKind::Deletion),
                (VcsDiffLineKind::Deletion, false) => Some(VcsDiffLineKind::Context),
                (VcsDiffLineKind::NoNewline, _) => None,
            }
        } else {
            match (line.kind, chosen) {
                (VcsDiffLineKind::Context, _) => Some(VcsDiffLineKind::Context),
                (VcsDiffLineKind::Addition, true) => Some(VcsDiffLineKind::Deletion),
                (VcsDiffLineKind::Addition, false) => Some(VcsDiffLineKind::Context),
                (VcsDiffLineKind::Deletion, true) => Some(VcsDiffLineKind::Addition),
                (VcsDiffLineKind::Deletion, false) => None,
                (VcsDiffLineKind::NoNewline, _) => None,
            }
        };
        if let Some(kind) = kind {
            lines.push((kind, &line.text));
        }
    }
    let old_lines = lines
        .iter()
        .filter(|(kind, _)| *kind != VcsDiffLineKind::Addition)
        .count();
    let new_lines = lines
        .iter()
        .filter(|(kind, _)| *kind != VcsDiffLineKind::Deletion)
        .count();
    let start = if stage {
        hunk.old_start
    } else {
        hunk.new_start
    };
    let mut patch = format!(
        "--- a/{0}\n+++ b/{0}\n@@ -{1},{2} +{1},{3} @@ {4}\n",
        file.path.0, start, old_lines, new_lines, hunk.header
    );
    for (kind, text) in lines {
        patch.push(match kind {
            VcsDiffLineKind::Context => ' ',
            VcsDiffLineKind::Addition => '+',
            VcsDiffLineKind::Deletion => '-',
            VcsDiffLineKind::NoNewline => unreachable!("no-newline markers are omitted"),
        });
        patch.push_str(text);
        patch.push('\n');
    }
    Ok(patch)
}

fn parse_hunk_header(line: &str) -> Result<(u32, u32, u32, u32, String)> {
    let mut fields = line.split_whitespace();
    if fields.next() != Some("@@") {
        return Err(GitAdapterError::InvalidData);
    }
    let (old_start, old_lines) = parse_hunk_range(
        fields
            .next()
            .and_then(|value| value.strip_prefix('-'))
            .ok_or(GitAdapterError::InvalidData)?,
    )?;
    let (new_start, new_lines) = parse_hunk_range(
        fields
            .next()
            .and_then(|value| value.strip_prefix('+'))
            .ok_or(GitAdapterError::InvalidData)?,
    )?;
    if fields.next() != Some("@@") {
        return Err(GitAdapterError::InvalidData);
    }
    let header = line
        .split_once("@@")
        .and_then(|(_, rest)| rest.split_once("@@"))
        .map_or("", |(_, header)| header)
        .trim()
        .to_owned();
    Ok((old_start, old_lines, new_start, new_lines, header))
}

fn parse_hunk_range(value: &str) -> Result<(u32, u32)> {
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    Ok((
        start.parse().map_err(|_| GitAdapterError::InvalidData)?,
        count.parse().map_err(|_| GitAdapterError::InvalidData)?,
    ))
}

fn finalize_hunk(side: VcsDiffSide, path: &WorkspacePath, mut hunk: VcsDiffHunk) -> VcsDiffHunk {
    let mut digest = Sha256::new();
    digest.update(format!("{side:?}\0{}\0", path.0));
    digest.update(hunk.old_start.to_le_bytes());
    digest.update(hunk.new_start.to_le_bytes());
    digest.update(hunk.header.as_bytes());
    for line in &hunk.lines {
        digest.update([line.kind as u8]);
        digest.update(line.text.as_bytes());
        digest.update([0]);
    }
    hunk.id = format!("{:x}", digest.finalize());
    hunk
}

fn parse_branches(bytes: &[u8]) -> Result<Vec<VcsBranch>> {
    let text = std::str::from_utf8(bytes).map_err(|_| GitAdapterError::InvalidData)?;
    let mut branches = Vec::new();
    for line in text.lines() {
        let fields = line.split('\0').collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(GitAdapterError::InvalidData);
        }
        let remote = fields[0].starts_with("refs/remotes/");
        let name = fields[0]
            .strip_prefix("refs/heads/")
            .or_else(|| fields[0].strip_prefix("refs/remotes/"))
            .ok_or(GitAdapterError::InvalidData)?;
        let (ahead, behind) = parse_track(fields[4])?;
        branches.push(VcsBranch {
            name: name.to_string(),
            head: is_oid(fields[1]).then(|| fields[1].to_string()),
            current: fields[2] == "*",
            remote,
            upstream: (!fields[3].is_empty()).then(|| fields[3].to_string()),
            ahead,
            behind,
        });
    }
    branches.sort_by(|left, right| (left.remote, &left.name).cmp(&(right.remote, &right.name)));
    Ok(branches)
}

fn parse_track(value: &str) -> Result<(u32, u32)> {
    let mut ahead = 0;
    let mut behind = 0;
    for part in value.split(',').map(str::trim) {
        if let Some(value) = part.strip_prefix("ahead ") {
            ahead = value.parse().map_err(|_| GitAdapterError::InvalidData)?;
        } else if let Some(value) = part.strip_prefix("behind ") {
            behind = value.parse().map_err(|_| GitAdapterError::InvalidData)?;
        } else if !part.is_empty() && part != "gone" {
            return Err(GitAdapterError::InvalidData);
        }
    }
    Ok((ahead, behind))
}

fn parse_signed_count(value: Option<&str>, prefix: char) -> Result<u32> {
    value
        .and_then(|value| value.strip_prefix(prefix))
        .ok_or(GitAdapterError::InvalidData)?
        .parse()
        .map_err(|_| GitAdapterError::InvalidData)
}

fn checked_path(value: &str) -> Result<WorkspacePath> {
    WorkspacePath::new(value.to_string()).map_err(|_| GitAdapterError::InvalidData)
}

fn validate_paths(paths: &[WorkspacePath]) -> Result<()> {
    if paths.is_empty() || paths.len() > 10_000 || paths.iter().any(|path| !path.is_valid()) {
        Err(GitAdapterError::InvalidData)
    } else {
        Ok(())
    }
}

fn validate_commit_identity(message: &str, name: &str, email: &str) -> Result<()> {
    if message.is_empty()
        || message.len() > 64 * 1024
        || message.contains('\0')
        || name.is_empty()
        || name.len() > 256
        || name.contains(['\0', '\n', '\r'])
        || email.is_empty()
        || email.len() > 320
        || email.contains(['\0', '\n', '\r'])
    {
        Err(GitAdapterError::InvalidData)
    } else {
        Ok(())
    }
}

fn validate_remote(remote: &str) -> Result<()> {
    if valid_ref_component(remote) {
        Ok(())
    } else {
        Err(GitAdapterError::InvalidData)
    }
}

fn valid_ref_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.starts_with('-')
        && !value.contains(['\0', '\n', '\r', ' '])
}

fn is_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| std::fs::canonicalize(candidate).ok())
}

fn find_sibling_askpass() -> Option<PathBuf> {
    let directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidate = directory.join(if cfg!(windows) {
        "sift-git-askpass.exe"
    } else {
        "sift-git-askpass"
    });
    candidate.is_file().then_some(candidate)
}

fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

#[cfg(unix)]
async fn serve_credential(listener: tokio::net::UnixListener, mut credential: GitCredential) {
    while let Ok((mut stream, _)) = listener.accept().await {
        let mut request = [0u8; 1];
        if stream.read_exact(&mut request).await.is_err() {
            continue;
        }
        let answer = if request[0] == b'U' {
            credential.username.as_bytes()
        } else {
            credential.password.as_bytes()
        };
        if answer.len() <= 16 * 1024 {
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, answer).await;
        }
    }
    credential.username.clear();
    credential.password.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_status_diff_stage_and_commit_are_typed() {
        let adapter = GitAdapter::for_tests().await.unwrap();
        let repository = tempfile::tempdir().unwrap();
        adapter.initialize(repository.path()).await.unwrap();
        std::fs::write(repository.path().join("query.sql"), "select 1;\n").unwrap();
        let status = adapter
            .status(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                1,
                WorkspaceRevision(1),
            )
            .await
            .unwrap();
        assert_eq!(status.entries[0].state, VcsFileState::Untracked);
        assert_eq!(status.entries[0].stage, VcsStageState::Unstaged);
        adapter
            .stage(repository.path(), &[WorkspacePath("query.sql".into())])
            .await
            .unwrap();
        adapter
            .unstage(repository.path(), &[WorkspacePath("query.sql".into())])
            .await
            .unwrap();
        adapter
            .stage(repository.path(), &[WorkspacePath("query.sql".into())])
            .await
            .unwrap();
        adapter
            .commit(
                repository.path(),
                "initial",
                "Sift Test",
                "sift@example.invalid",
            )
            .await
            .unwrap();
        std::fs::write(repository.path().join("query.sql"), "select 2;\n").unwrap();
        let diff = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::IndexToWorktree,
                None,
            )
            .await
            .unwrap();
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].additions, 1);
        assert_eq!(diff.files[0].deletions, 1);
        assert!(diff.files[0].hunks.is_empty());

        let path = WorkspacePath("query.sql".into());
        let file_diff = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::IndexToWorktree,
                Some(&path),
            )
            .await
            .unwrap();
        assert_eq!(file_diff.files[0].hunks.len(), 1);
        assert!(file_diff.files[0].hunks[0]
            .lines
            .iter()
            .any(|line| line.kind == VcsDiffLineKind::Deletion && line.text == "select 1;"));
        assert!(file_diff.files[0].hunks[0]
            .lines
            .iter()
            .any(|line| line.kind == VcsDiffLineKind::Addition && line.text == "select 2;"));
        assert_eq!(file_diff.files[0].hunks[0].id.len(), 64);
    }

    #[tokio::test]
    async fn one_typed_hunk_can_be_staged_and_unstaged_without_touching_the_other() {
        let adapter = GitAdapter::for_tests().await.unwrap();
        let repository = tempfile::tempdir().unwrap();
        adapter.initialize(repository.path()).await.unwrap();
        let path = WorkspacePath("query.sql".into());
        let original = (1..=14)
            .map(|line| format!("select {line};"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(repository.path().join(&path.0), &original).unwrap();
        adapter
            .stage(repository.path(), std::slice::from_ref(&path))
            .await
            .unwrap();
        adapter
            .commit(
                repository.path(),
                "initial",
                "Sift Test",
                "sift@example.invalid",
            )
            .await
            .unwrap();
        let changed = original
            .replace("select 2;", "select 200;")
            .replace("select 13;", "select 1300;");
        std::fs::write(repository.path().join(&path.0), changed).unwrap();

        let worktree = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::IndexToWorktree,
                Some(&path),
            )
            .await
            .unwrap();
        assert_eq!(worktree.files[0].hunks.len(), 2);
        adapter
            .apply_hunk(
                repository.path(),
                &worktree.files[0],
                &worktree.files[0].hunks[0],
                false,
            )
            .await
            .unwrap();

        let staged = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::HeadToIndex,
                Some(&path),
            )
            .await
            .unwrap();
        assert_eq!(staged.files[0].hunks.len(), 1);
        let remaining = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::IndexToWorktree,
                Some(&path),
            )
            .await
            .unwrap();
        assert_eq!(remaining.files[0].hunks.len(), 1);

        adapter
            .apply_hunk(
                repository.path(),
                &staged.files[0],
                &staged.files[0].hunks[0],
                true,
            )
            .await
            .unwrap();
        let staged_after = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::HeadToIndex,
                Some(&path),
            )
            .await
            .unwrap();
        assert!(staged_after.files.is_empty());
    }

    #[tokio::test]
    async fn selected_typed_lines_can_be_staged_without_the_rest_of_the_hunk() {
        let adapter = GitAdapter::for_tests().await.unwrap();
        let repository = tempfile::tempdir().unwrap();
        adapter.initialize(repository.path()).await.unwrap();
        let path = WorkspacePath("query.sql".into());
        let original = (1..=8)
            .map(|line| format!("select {line};"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(repository.path().join(&path.0), &original).unwrap();
        adapter
            .stage(repository.path(), std::slice::from_ref(&path))
            .await
            .unwrap();
        adapter
            .commit(
                repository.path(),
                "initial",
                "Sift Test",
                "sift@example.invalid",
            )
            .await
            .unwrap();
        let changed = original
            .replace("select 3;", "select 300;")
            .replace("select 5;", "select 500;");
        std::fs::write(repository.path().join(&path.0), changed).unwrap();

        let worktree = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::IndexToWorktree,
                Some(&path),
            )
            .await
            .unwrap();
        let file = &worktree.files[0];
        assert_eq!(file.hunks.len(), 1);
        let hunk = &file.hunks[0];
        let selected = hunk
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| matches!(line.text.as_str(), "select 3;" | "select 300;"))
            .map(|(index, _)| index as u32)
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 2);
        adapter
            .apply_lines(repository.path(), file, hunk, &selected, true)
            .await
            .unwrap();

        let staged = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::HeadToIndex,
                Some(&path),
            )
            .await
            .unwrap();
        let staged_text = staged.files[0].hunks[0]
            .lines
            .iter()
            .filter(|line| line.kind != VcsDiffLineKind::Context)
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        assert!(staged_text.contains(&"select 300;"));
        assert!(!staged_text.contains(&"select 500;"));
        let remaining = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::IndexToWorktree,
                Some(&path),
            )
            .await
            .unwrap();
        let remaining_text = remaining.files[0].hunks[0]
            .lines
            .iter()
            .filter(|line| line.kind != VcsDiffLineKind::Context)
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        assert!(remaining_text.contains(&"select 500;"));
        assert!(!remaining_text.contains(&"select 300;"));

        let staged_file = &staged.files[0];
        let staged_hunk = &staged_file.hunks[0];
        let staged_selection = staged_hunk
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| matches!(line.text.as_str(), "select 3;" | "select 300;"))
            .map(|(index, _)| index as u32)
            .collect::<Vec<_>>();
        adapter
            .apply_lines(
                repository.path(),
                staged_file,
                staged_hunk,
                &staged_selection,
                false,
            )
            .await
            .unwrap();
        let staged_after = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::HeadToIndex,
                Some(&path),
            )
            .await
            .unwrap();
        assert!(staged_after.files.is_empty());
    }

    #[tokio::test]
    async fn amend_and_soft_uncommit_keep_guarded_changes_in_the_index() {
        let adapter = GitAdapter::for_tests().await.unwrap();
        let repository = tempfile::tempdir().unwrap();
        adapter.initialize(repository.path()).await.unwrap();
        let path = WorkspacePath("query.sql".into());
        std::fs::write(repository.path().join(&path.0), "select 1;\n").unwrap();
        adapter
            .stage(repository.path(), std::slice::from_ref(&path))
            .await
            .unwrap();
        let initial = adapter
            .commit(
                repository.path(),
                "initial",
                "Sift Test",
                "sift@example.invalid",
            )
            .await
            .unwrap()
            .head
            .unwrap();

        std::fs::write(repository.path().join(&path.0), "select 2;\n").unwrap();
        adapter
            .stage(repository.path(), std::slice::from_ref(&path))
            .await
            .unwrap();
        let second = adapter
            .commit(
                repository.path(),
                "second",
                "Sift Test",
                "sift@example.invalid",
            )
            .await
            .unwrap()
            .head
            .unwrap();
        std::fs::write(repository.path().join(&path.0), "select 3;\n").unwrap();
        adapter
            .stage(repository.path(), std::slice::from_ref(&path))
            .await
            .unwrap();
        let amended = adapter
            .amend(
                repository.path(),
                "second amended",
                "Sift Test",
                "sift@example.invalid",
            )
            .await
            .unwrap()
            .head
            .unwrap();
        assert_ne!(amended, second);

        let reset = adapter.soft_reset_parent(repository.path()).await.unwrap();
        assert_eq!(reset.head.as_deref(), Some(initial.as_str()));
        let staged = adapter
            .diff(
                repository.path(),
                sift_protocol::RepositoryBindingId(1),
                VcsDiffSide::HeadToIndex,
                Some(&path),
            )
            .await
            .unwrap();
        assert!(staged.files[0].hunks[0]
            .lines
            .iter()
            .any(|line| line.kind == VcsDiffLineKind::Addition && line.text == "select 3;"));
    }

    #[test]
    fn typed_patch_parser_preserves_coordinates_and_marks_truncation() {
        let path = WorkspacePath("queries/report.sql".into());
        let patch = b"diff --git a/queries/report.sql b/queries/report.sql\n--- a/queries/report.sql\n+++ b/queries/report.sql\n@@ -2,2 +2,3 @@ report\n keep\n-old\n+new\n+extra\n";
        let (hunks, truncated) =
            parse_patch(VcsDiffSide::IndexToWorktree, &path, patch, true).unwrap();

        assert!(truncated);
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].truncated);
        assert_eq!(hunks[0].header, "report");
        assert_eq!(hunks[0].lines[0].old_line, Some(2));
        assert_eq!(hunks[0].lines[0].new_line, Some(2));
        assert_eq!(hunks[0].lines[1].old_line, Some(3));
        assert_eq!(hunks[0].lines[2].new_line, Some(3));
        assert_eq!(hunks[0].lines[3].new_line, Some(4));
    }

    #[test]
    fn malicious_paths_and_identity_values_fail_before_git() {
        assert!(validate_paths(&[WorkspacePath("../escape".into())]).is_err());
        assert!(validate_remote("--upload-pack=evil").is_err());
        assert!(validate_commit_identity("ok", "name\nmalicious", "a@b").is_err());
    }
}
