use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use sift_extension_protocol::{
    Cancel, ExtensionId, Hello, Message, Request, Response, RpcError, RpcLimits, Shutdown, Welcome,
    WireId, EXTENSION_RPC_VERSION,
};
use thiserror::Error;
use tokio::{
    process::{Child, Command},
    sync::{oneshot, Mutex, RwLock},
    task::JoinHandle,
};

use crate::{FrameError, FrameReader, FrameWriter, HARD_MAX_FRAME_BYTES};

const MAX_DIAGNOSTIC_LINE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct SupervisorLimits {
    pub handshake_timeout: Duration,
    pub cancel_grace: Duration,
    pub max_frame_bytes: usize,
    pub heartbeat_interval: Duration,
    pub missed_heartbeats: u32,
    pub retained_diagnostic_bytes: usize,
    pub maximum_concurrent_requests: u32,
}

impl Default for SupervisorLimits {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            cancel_grace: Duration::from_secs(2),
            max_frame_bytes: 8 * 1024 * 1024,
            heartbeat_interval: Duration::from_secs(5),
            missed_heartbeats: 3,
            retained_diagnostic_bytes: 1024 * 1024,
            maximum_concurrent_requests: 64,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub executable: PathBuf,
    pub working_directory: PathBuf,
    pub extension_id: ExtensionId,
    pub extension_version: String,
    pub manifest_sha256: String,
    pub expected_contributions: Vec<sift_extension_protocol::ContributionId>,
    pub generation: WireId,
    pub granted_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationHealth {
    Starting,
    Ready,
    Degraded,
    Quarantined,
    Stopped,
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("invalid supervisor configuration: {0}")]
    InvalidConfiguration(String),
    #[error("extension process I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("extension handshake timed out")]
    HandshakeTimeout,
    #[error("extension sent a non-hello first frame")]
    HelloRequired,
    #[error("extension handshake identity differs from its manifest")]
    IdentityMismatch,
    #[error("extension has no compatible RPC version")]
    IncompatibleVersion,
    #[error("extension request timed out")]
    RequestTimeout,
    #[error("extension process stopped")]
    ProcessStopped,
    #[error("extension protocol violation: {0}")]
    ProtocolViolation(String),
    #[error("extension returned an error: {0:?}")]
    Remote(RpcError),
}

type Pending = Arc<Mutex<HashMap<WireId, oneshot::Sender<Response>>>>;

pub struct SupervisedProcess {
    spec: ProcessSpec,
    limits: SupervisorLimits,
    writer: Arc<Mutex<FrameWriter<tokio::process::ChildStdin>>>,
    child: Arc<Mutex<Child>>,
    pending: Pending,
    health: Arc<RwLock<GenerationHealth>>,
    diagnostics: Arc<Mutex<DiagnosticRing>>,
    request_counter: AtomicU64,
    maximum_concurrent_requests: u32,
    stopped: Arc<AtomicBool>,
    reader_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
    heartbeat_task: JoinHandle<()>,
}

impl SupervisedProcess {
    pub async fn start(
        spec: ProcessSpec,
        limits: SupervisorLimits,
    ) -> Result<Self, SupervisorError> {
        validate_limits(&limits)?;
        let mut command = Command::new(&spec.executable);
        command
            .current_dir(&spec.working_directory)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| {
            SupervisorError::InvalidConfiguration("child stdin was not piped".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            SupervisorError::InvalidConfiguration("child stdout was not piped".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            SupervisorError::InvalidConfiguration("child stderr was not piped".into())
        })?;
        let mut reader = FrameReader::new(stdout, limits.max_frame_bytes)?;
        let hello =
            match tokio::time::timeout(limits.handshake_timeout, reader.read_message()).await {
                Ok(Ok(Message::Hello(hello))) => hello,
                Ok(Ok(_)) => {
                    let _ = child.kill().await;
                    return Err(SupervisorError::HelloRequired);
                }
                Ok(Err(error)) => {
                    let _ = child.kill().await;
                    return Err(error.into());
                }
                Err(_) => {
                    let _ = child.kill().await;
                    return Err(SupervisorError::HandshakeTimeout);
                }
            };
        validate_hello(&spec, &hello)?;

        let selected_requests = hello
            .max_concurrent_requests
            .min(limits.maximum_concurrent_requests);
        if selected_requests == 0 {
            let _ = child.kill().await;
            return Err(SupervisorError::ProtocolViolation(
                "max_concurrent_requests cannot be zero".into(),
            ));
        }
        let mut writer = FrameWriter::new(stdin, limits.max_frame_bytes)?;
        writer
            .write_message(&Message::Welcome(Welcome {
                extension_rpc_version: EXTENSION_RPC_VERSION,
                method_family_versions: hello
                    .method_families
                    .iter()
                    .filter(|family| family.versions.contains(1))
                    .map(|family| (family.family.clone(), 1))
                    .collect(),
                process_generation: spec.generation,
                granted_capabilities: spec.granted_capabilities.clone(),
                limits: RpcLimits {
                    max_frame_bytes: limits.max_frame_bytes as u32,
                    max_row_bytes: (limits.max_frame_bytes / 2) as u32,
                    max_page_rows: 4096,
                    initial_stream_credit_bytes: limits.max_frame_bytes as u64 + 4,
                    control_credit_bytes: 64 * 1024,
                },
                heartbeat_interval_ms: limits.heartbeat_interval.as_millis().min(u32::MAX as u128)
                    as u32,
                max_concurrent_requests: selected_requests,
            }))
            .await?;

        let writer = Arc::new(Mutex::new(writer));
        let child = Arc::new(Mutex::new(child));
        let pending = Pending::default();
        let health = Arc::new(RwLock::new(GenerationHealth::Ready));
        let diagnostics = Arc::new(Mutex::new(DiagnosticRing::new(
            limits.retained_diagnostic_bytes,
        )));
        let stopped = Arc::new(AtomicBool::new(false));
        let last_heartbeat = Arc::new(Mutex::new(tokio::time::Instant::now()));
        let reader_task = spawn_reader(
            reader,
            pending.clone(),
            health.clone(),
            stopped.clone(),
            diagnostics.clone(),
            child.clone(),
            last_heartbeat.clone(),
        );
        let stderr_task = spawn_stderr(stderr, diagnostics.clone(), stopped.clone());
        let heartbeat_task = spawn_heartbeat_monitor(
            limits.heartbeat_interval,
            limits.missed_heartbeats,
            last_heartbeat,
            health.clone(),
            stopped.clone(),
            child.clone(),
            pending.clone(),
            diagnostics.clone(),
        );

        Ok(Self {
            spec,
            limits,
            writer,
            child,
            pending,
            health,
            diagnostics,
            request_counter: AtomicU64::new(1),
            maximum_concurrent_requests: selected_requests,
            stopped,
            reader_task,
            stderr_task,
            heartbeat_task,
        })
    }

    pub fn generation(&self) -> WireId {
        self.spec.generation
    }

    pub async fn health(&self) -> GenerationHealth {
        *self.health.read().await
    }

    pub async fn diagnostics(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .await
            .lines
            .iter()
            .cloned()
            .collect()
    }

    pub async fn request(&self, mut request: Request) -> Result<Response, SupervisorError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(SupervisorError::ProcessStopped);
        }
        request.id = next_wire_id(&self.request_counter, self.spec.generation);
        let request_id = request.id;
        let timeout = deadline_duration(request.deadline_unix_ms)?;
        let (sender, mut receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.len() >= self.maximum_concurrent_requests as usize {
                return Err(SupervisorError::ProtocolViolation(
                    "negotiated request concurrency exceeded".into(),
                ));
            }
            pending.insert(request_id, sender);
        }
        if let Err(error) = self
            .writer
            .lock()
            .await
            .write_message(&Message::Request(request))
            .await
        {
            self.pending.lock().await.remove(&request_id);
            return Err(error.into());
        }

        match tokio::time::timeout(timeout, &mut receiver).await {
            Ok(Ok(response)) => match &response.result {
                sift_extension_protocol::ResponseResult::Error { error } => {
                    Err(SupervisorError::Remote(error.clone()))
                }
                _ => Ok(response),
            },
            Ok(Err(_)) => Err(SupervisorError::ProcessStopped),
            Err(_) => {
                let _ = self
                    .writer
                    .lock()
                    .await
                    .write_message(&Message::Cancel(Cancel { request_id }))
                    .await;
                if tokio::time::timeout(self.limits.cancel_grace, &mut receiver)
                    .await
                    .is_err()
                {
                    self.pending.lock().await.remove(&request_id);
                    self.kill().await?;
                }
                Err(SupervisorError::RequestTimeout)
            }
        }
    }

    pub async fn shutdown(&self, reason: &str) -> Result<(), SupervisorError> {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let _ = self
            .writer
            .lock()
            .await
            .write_message(&Message::Shutdown(Shutdown {
                reason: reason.to_string(),
            }))
            .await;
        self.kill().await
    }

    async fn kill(&self) -> Result<(), SupervisorError> {
        self.stopped.store(true, Ordering::Release);
        let mut child = self.child.lock().await;
        match child.kill().await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(error) => return Err(error.into()),
        }
        *self.health.write().await = GenerationHealth::Stopped;
        self.pending.lock().await.clear();
        Ok(())
    }
}

impl Drop for SupervisedProcess {
    fn drop(&mut self) {
        self.reader_task.abort();
        self.stderr_task.abort();
        self.heartbeat_task.abort();
    }
}

fn validate_limits(limits: &SupervisorLimits) -> Result<(), SupervisorError> {
    if limits.max_frame_bytes == 0 || limits.max_frame_bytes > HARD_MAX_FRAME_BYTES {
        return Err(SupervisorError::InvalidConfiguration(
            "max frame size is outside the hard bounds".into(),
        ));
    }
    if limits.handshake_timeout.is_zero()
        || limits.cancel_grace.is_zero()
        || limits.heartbeat_interval.is_zero()
        || limits.missed_heartbeats == 0
        || limits.retained_diagnostic_bytes == 0
        || limits.maximum_concurrent_requests == 0
    {
        return Err(SupervisorError::InvalidConfiguration(
            "supervisor limits must be non-zero".into(),
        ));
    }
    Ok(())
}

fn validate_hello(spec: &ProcessSpec, hello: &Hello) -> Result<(), SupervisorError> {
    if !hello.extension_rpc.contains(EXTENSION_RPC_VERSION) {
        return Err(SupervisorError::IncompatibleVersion);
    }
    let mut expected = spec.expected_contributions.clone();
    let mut actual = hello.contributions.clone();
    expected.sort();
    actual.sort();
    if hello.extension_id != spec.extension_id
        || hello.extension_version != spec.extension_version
        || hello.manifest_sha256 != spec.manifest_sha256
        || expected != actual
    {
        return Err(SupervisorError::IdentityMismatch);
    }
    Ok(())
}

fn spawn_reader(
    mut reader: FrameReader<tokio::process::ChildStdout>,
    pending: Pending,
    health: Arc<RwLock<GenerationHealth>>,
    stopped: Arc<AtomicBool>,
    diagnostics: Arc<Mutex<DiagnosticRing>>,
    child: Arc<Mutex<Child>>,
    last_heartbeat: Arc<Mutex<tokio::time::Instant>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match reader.read_message().await {
                Ok(Message::Response(response)) => {
                    if let Some(sender) = pending.lock().await.remove(&response.id) {
                        let _ = sender.send(response);
                    } else {
                        record_diagnostic(
                            &diagnostics,
                            "protocol violation: response for unknown request",
                        )
                        .await;
                        break;
                    }
                }
                Ok(Message::Heartbeat(_)) => {
                    *last_heartbeat.lock().await = tokio::time::Instant::now();
                }
                Ok(Message::Log(record)) => {
                    record_diagnostic(&diagnostics, &record.message).await;
                }
                Ok(message) => {
                    record_diagnostic(
                        &diagnostics,
                        &format!("protocol violation: unexpected {message:?}"),
                    )
                    .await;
                    break;
                }
                Err(error) => {
                    record_diagnostic(&diagnostics, &format!("RPC reader stopped: {error}")).await;
                    break;
                }
            }
        }
        stopped.store(true, Ordering::Release);
        *health.write().await = GenerationHealth::Degraded;
        pending.lock().await.clear();
        let _ = child.lock().await.kill().await;
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_heartbeat_monitor(
    interval: Duration,
    missed: u32,
    last_heartbeat: Arc<Mutex<tokio::time::Instant>>,
    health: Arc<RwLock<GenerationHealth>>,
    stopped: Arc<AtomicBool>,
    child: Arc<Mutex<Child>>,
    pending: Pending,
    diagnostics: Arc<Mutex<DiagnosticRing>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let maximum_gap = interval.saturating_mul(missed);
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if stopped.load(Ordering::Acquire) {
                return;
            }
            if last_heartbeat.lock().await.elapsed() <= maximum_gap {
                continue;
            }
            record_diagnostic(&diagnostics, "extension missed its heartbeat deadline").await;
            stopped.store(true, Ordering::Release);
            *health.write().await = GenerationHealth::Degraded;
            pending.lock().await.clear();
            let _ = child.lock().await.kill().await;
            return;
        }
    })
}

fn spawn_stderr(
    stderr: tokio::process::ChildStderr,
    diagnostics: Arc<Mutex<DiagnosticRing>>,
    stopped: Arc<AtomicBool>,
) -> JoinHandle<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while !stopped.load(Ordering::Acquire) {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let bounded = if line.len() > MAX_DIAGNOSTIC_LINE_BYTES {
                        &line[..floor_char_boundary(&line, MAX_DIAGNOSTIC_LINE_BYTES)]
                    } else {
                        &line
                    };
                    record_diagnostic(&diagnostics, bounded).await;
                }
                _ => break,
            }
        }
    })
}

async fn record_diagnostic(ring: &Arc<Mutex<DiagnosticRing>>, message: &str) {
    ring.lock().await.push(redact_diagnostic(message));
}

fn redact_diagnostic(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if ["password", "bearer ", "token=", "secret", "credential"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        "[redacted secret-shaped diagnostic]".into()
    } else {
        message.to_string()
    }
}

fn floor_char_boundary(value: &str, maximum: usize) -> usize {
    let mut index = maximum.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

struct DiagnosticRing {
    lines: VecDeque<String>,
    bytes: usize,
    maximum: usize,
}

impl DiagnosticRing {
    fn new(maximum: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            maximum,
        }
    }

    fn push(&mut self, line: String) {
        self.bytes = self.bytes.saturating_add(line.len());
        self.lines.push_back(line);
        while self.bytes > self.maximum {
            let Some(removed) = self.lines.pop_front() else {
                self.bytes = 0;
                break;
            };
            self.bytes = self.bytes.saturating_sub(removed.len());
        }
    }
}

fn next_wire_id(counter: &AtomicU64, generation: WireId) -> WireId {
    let sequence = counter.fetch_add(1, Ordering::Relaxed);
    WireId::from_u128(generation.as_u128() ^ sequence as u128)
}

fn chrono_like_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn deadline_duration(deadline_unix_ms: i64) -> Result<Duration, SupervisorError> {
    let remaining = deadline_unix_ms.saturating_sub(chrono_like_unix_ms());
    if remaining <= 0 {
        return Err(SupervisorError::RequestTimeout);
    }
    Ok(Duration::from_millis(remaining as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_ring_is_byte_bounded_and_redacts_secrets() {
        let mut ring = DiagnosticRing::new(8);
        ring.push("1234".into());
        ring.push("5678".into());
        ring.push("90".into());
        assert_eq!(
            ring.lines.into_iter().collect::<Vec<_>>(),
            vec!["5678", "90"]
        );
        assert_eq!(
            redact_diagnostic("password=hunter2"),
            "[redacted secret-shaped diagnostic]"
        );
    }

    #[test]
    fn utf8_truncation_stays_on_a_character_boundary() {
        assert_eq!(floor_char_boundary("aéz", 2), 1);
    }
}
