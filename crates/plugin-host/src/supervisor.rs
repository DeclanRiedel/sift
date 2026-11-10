use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use sift_extension_protocol::{
    Cancel, Credit, ExtensionId, Hello, Message, Request, Response, ResponseResult, RpcError,
    RpcLimits, Shutdown, StreamFrame, Welcome, WireId, EXTENSION_RPC_VERSION,
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
    /// Mandatory Driver range for database-provider packages; None for action-only packages.
    pub driver_rpc: Option<sift_extension_protocol::VersionRange>,
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
    #[error("extension request admission refused")]
    AdmissionRefused,
    #[error("extension request cancelled")]
    RequestCancelled,
    #[error("extension process stopped")]
    ProcessStopped,
    #[error("extension protocol violation: {0}")]
    ProtocolViolation(String),
    #[error("extension returned an error: {0:?}")]
    Remote(RpcError),
}

/// Transport evidence for one RPC, independent of native database semantics.
#[derive(Debug)]
pub struct RequestObservation {
    /// The frame writer was entered. A complete frame or native admission is not proven.
    pub frame_started: bool,
    /// A response observed before interruption (including a remote error), or a transport failure.
    pub result: Result<Response, SupervisorError>,
    /// Exact response observed after Stop/deadline during bounded cancellation cleanup.
    pub late_response: Option<Response>,
}

impl RequestObservation {
    fn refused(error: SupervisorError) -> Self {
        Self {
            frame_started: false,
            result: Err(error),
            late_response: None,
        }
    }
}

type Pending = Arc<Mutex<HashMap<WireId, oneshot::Sender<Response>>>>;
type Streams = Arc<StdMutex<HashMap<WireId, StreamSink>>>;

struct StreamSink {
    sender: tokio::sync::mpsc::UnboundedSender<ReceivedStreamFrame>,
    remaining_credit: u64,
    next_sequence: u64,
}

#[derive(Debug)]
pub struct ReceivedStreamFrame {
    pub frame: StreamFrame,
    pub encoded_bytes: usize,
}

pub struct RpcStream {
    stream_id: WireId,
    receiver: tokio::sync::mpsc::UnboundedReceiver<ReceivedStreamFrame>,
    writer: Arc<Mutex<FrameWriter<tokio::process::ChildStdin>>>,
    streams: Streams,
}

impl RpcStream {
    pub fn try_next(&mut self) -> Option<ReceivedStreamFrame> {
        self.receiver.try_recv().ok()
    }

    pub async fn next(&mut self) -> Option<ReceivedStreamFrame> {
        self.receiver.recv().await
    }

    pub async fn accept(&self, frame: &ReceivedStreamFrame) -> Result<(), SupervisorError> {
        {
            let mut streams = self.streams.lock().expect("stream map poisoned");
            if let Some(stream) = streams.get_mut(&self.stream_id) {
                stream.remaining_credit = stream
                    .remaining_credit
                    .saturating_add(frame.encoded_bytes as u64);
            }
        }
        if let Err(error) = self
            .writer
            .lock()
            .await
            .write_message(&Message::Credit(Credit {
                stream_id: self.stream_id,
                bytes: frame.encoded_bytes as u64,
            }))
            .await
        {
            let mut streams = self.streams.lock().expect("stream map poisoned");
            if let Some(stream) = streams.get_mut(&self.stream_id) {
                stream.remaining_credit = stream
                    .remaining_credit
                    .saturating_sub(frame.encoded_bytes as u64);
            }
            return Err(error.into());
        }
        Ok(())
    }
}

impl Drop for RpcStream {
    fn drop(&mut self) {
        self.streams
            .lock()
            .expect("stream map poisoned")
            .remove(&self.stream_id);
    }
}

pub struct SupervisedProcess {
    spec: ProcessSpec,
    limits: SupervisorLimits,
    method_family_versions: BTreeMap<String, u32>,
    writer: Arc<Mutex<FrameWriter<tokio::process::ChildStdin>>>,
    child: Arc<Mutex<Child>>,
    pending: Pending,
    streams: Streams,
    health: Arc<RwLock<GenerationHealth>>,
    diagnostics: Arc<Mutex<DiagnosticRing>>,
    request_counter: AtomicU64,
    maximum_concurrent_requests: u32,
    accepting: AtomicBool,
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
        Self::start_inner(spec, limits, None, std::future::pending()).await
    }

    /// Retain startup through child termination on Stop/deadline. Runtime callers
    /// keep their generation permit until this future returns; never drop it.
    pub async fn start_cancellable<Stop>(
        spec: ProcessSpec,
        limits: SupervisorLimits,
        deadline_unix_ms: i64,
        stopped: Stop,
    ) -> Result<Self, SupervisorError>
    where
        Stop: std::future::Future<Output = ()>,
    {
        Self::start_inner(spec, limits, Some(deadline_unix_ms), stopped).await
    }

    async fn start_inner<Stop>(
        spec: ProcessSpec,
        limits: SupervisorLimits,
        request_deadline: Option<i64>,
        stopped: Stop,
    ) -> Result<Self, SupervisorError>
    where
        Stop: std::future::Future<Output = ()>,
    {
        validate_limits(&limits)?;
        tokio::pin!(stopped);
        let now = tokio::time::Instant::now();
        let handshake_deadline = now.checked_add(limits.handshake_timeout).ok_or_else(|| {
            SupervisorError::InvalidConfiguration("handshake deadline overflow".into())
        })?;
        let request_deadline = request_deadline
            .map(deadline_duration)
            .transpose()?
            .map(|remaining| {
                now.checked_add(remaining).ok_or_else(|| {
                    SupervisorError::InvalidConfiguration("request deadline overflow".into())
                })
            })
            .transpose()?;
        let deadline =
            request_deadline.map_or(handshake_deadline, |outer| outer.min(handshake_deadline));
        let expired = || {
            if request_deadline.is_some_and(|outer| tokio::time::Instant::now() >= outer) {
                SupervisorError::RequestTimeout
            } else {
                SupervisorError::HandshakeTimeout
            }
        };
        if std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(stopped.as_mut(), cx).is_ready())
        })
        .await
        {
            return Err(SupervisorError::RequestCancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(expired());
        }
        let mut command = Command::new(&spec.executable);
        command
            .current_dir(&spec.working_directory)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        // Every error after spawn rejoins child termination before releasing
        // the caller's generation permit, including pipe/validation failures.
        let prepared = async {
            let stdin = child.stdin.take().ok_or_else(||
                SupervisorError::InvalidConfiguration("child stdin was not piped".into()))?;
            let stdout = child.stdout.take().ok_or_else(||
                SupervisorError::InvalidConfiguration("child stdout was not piped".into()))?;
            let stderr = child.stderr.take().ok_or_else(||
                SupervisorError::InvalidConfiguration("child stderr was not piped".into()))?;
            let mut reader = FrameReader::new(stdout, limits.max_frame_bytes)?;
            let hello = tokio::select! {
                biased;
                _ = &mut stopped => return Err(SupervisorError::RequestCancelled),
                received = tokio::time::timeout_at(deadline, reader.read_message()) => {
                    match received {
                        Ok(Ok(Message::Hello(hello))) if tokio::time::Instant::now() < deadline => hello,
                        Ok(Ok(Message::Hello(_))) | Err(_) => return Err(expired()),
                        Ok(Ok(_)) => return Err(SupervisorError::HelloRequired),
                        Ok(Err(error)) => return Err(error.into()),
                    }
                }
            };
            validate_hello(&spec, &hello)?;
            let method_family_versions = negotiate_families(&spec, &hello)?;
            let selected_requests = hello.max_concurrent_requests.min(limits.maximum_concurrent_requests);
            if selected_requests == 0 {
                return Err(SupervisorError::ProtocolViolation("max_concurrent_requests cannot be zero".into()));
            }
            if tokio::time::Instant::now() >= deadline { return Err(expired()); }
            let mut writer = FrameWriter::new(stdin, limits.max_frame_bytes)?;
            let welcome = Message::Welcome(Welcome {
                extension_rpc_version: EXTENSION_RPC_VERSION,
                method_family_versions: method_family_versions.clone(),
                process_generation: spec.generation,
                granted_capabilities: spec.granted_capabilities.clone(),
                limits: RpcLimits {
                    max_frame_bytes: limits.max_frame_bytes as u32,
                    max_row_bytes: (limits.max_frame_bytes / 2) as u32,
                    max_page_rows: 4096,
                    initial_stream_credit_bytes: limits.max_frame_bytes as u64 + 4,
                    control_credit_bytes: 64 * 1024,
                },
                heartbeat_interval_ms: limits.heartbeat_interval.as_millis().min(u32::MAX as u128) as u32,
                max_concurrent_requests: selected_requests,
            });
            tokio::select! {
                biased;
                _ = &mut stopped => return Err(SupervisorError::RequestCancelled),
                written = tokio::time::timeout_at(deadline, writer.write_message(&welcome)) => {
                    match written {
                        Ok(Ok(_)) if tokio::time::Instant::now() < deadline => {},
                        Ok(Ok(_)) | Err(_) => return Err(expired()),
                        Ok(Err(error)) => return Err(error.into()),
                    }
                }
            }
            Ok((reader, writer, stderr, method_family_versions, selected_requests))
        }.await;
        let (reader, writer, stderr, method_family_versions, selected_requests) = match prepared {
            Ok(parts) => parts,
            Err(reason) => {
                // Preserve the primary Stop/deadline/validation cause.
                let _ = child.kill().await;
                return Err(reason);
            }
        };

        let writer = Arc::new(Mutex::new(writer));
        let child = Arc::new(Mutex::new(child));
        let pending = Pending::default();
        let streams = Streams::default();
        let health = Arc::new(RwLock::new(GenerationHealth::Ready));
        let diagnostics = Arc::new(Mutex::new(DiagnosticRing::new(
            limits.retained_diagnostic_bytes,
        )));
        let stopped = Arc::new(AtomicBool::new(false));
        let last_heartbeat = Arc::new(Mutex::new(tokio::time::Instant::now()));
        let reader_task = spawn_reader(
            reader,
            ReaderState {
                pending: pending.clone(),
                health: health.clone(),
                stopped: stopped.clone(),
                diagnostics: diagnostics.clone(),
                child: child.clone(),
                last_heartbeat: last_heartbeat.clone(),
                streams: streams.clone(),
            },
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
            streams.clone(),
            diagnostics.clone(),
        );

        Ok(Self {
            spec,
            limits,
            method_family_versions,
            writer,
            child,
            pending,
            streams,
            health,
            diagnostics,
            request_counter: AtomicU64::new(1),
            maximum_concurrent_requests: selected_requests,
            accepting: AtomicBool::new(true),
            stopped,
            reader_task,
            stderr_task,
            heartbeat_task,
        })
    }

    pub fn method_family_version(&self, family: &str) -> Option<u32> {
        self.method_family_versions.get(family).copied()
    }
    pub fn maximum_frame_bytes(&self) -> usize {
        self.limits.max_frame_bytes
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

    pub fn begin_drain(&self) {
        self.accepting.store(false, Ordering::Release);
    }

    pub async fn active_work(&self) -> usize {
        let pending = self.pending.lock().await.len();
        let streams = self.streams.lock().expect("stream map poisoned").len();
        pending.saturating_add(streams)
    }

    pub async fn request(&self, request: Request) -> Result<Response, SupervisorError> {
        let response = self
            .request_observed(request, || async { Ok(()) })
            .await
            .result?;
        match &response.result {
            ResponseResult::Error { error } => Err(SupervisorError::Remote(error.clone())),
            _ => Ok(response),
        }
    }

    /// Run admission after writer acquisition and before entering the frame writer.
    /// The original deadline covers writer waiting and admission. The admission future
    /// must tolerate cancellation; detached work must independently fence its claim.
    /// Callers must retain this owner through cleanup if their observer disappears.
    pub async fn request_observed<F, Fut>(&self, request: Request, admit: F) -> RequestObservation
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), SupervisorError>>,
    {
        self.request_observed_cancellable(request, admit, std::future::pending())
            .await
    }

    /// Retain the issued owner through targeted cancellation and bounded cleanup.
    /// A completed Stop future closes new admission; it does not prove rollback.
    pub async fn request_observed_cancellable<F, Fut, Stop>(
        &self,
        mut request: Request,
        admit: F,
        stopped: Stop,
    ) -> RequestObservation
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), SupervisorError>>,
        Stop: std::future::Future<Output = ()>,
    {
        tokio::pin!(stopped);
        if !self.accepting.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
            return RequestObservation::refused(SupervisorError::ProcessStopped);
        }
        let remaining = match deadline_duration(request.deadline_unix_ms) {
            Ok(remaining) => remaining,
            Err(error) => return RequestObservation::refused(error),
        };
        let deadline = tokio::time::Instant::now() + remaining;
        request.id = next_wire_id(&self.request_counter, self.spec.generation);
        let request_id = request.id;
        let (sender, mut receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.len() >= self.maximum_concurrent_requests as usize {
                return RequestObservation::refused(SupervisorError::ProtocolViolation(
                    "negotiated request concurrency exceeded".into(),
                ));
            }
            pending.insert(request_id, sender);
        }
        let admission = tokio::time::timeout_at(deadline, async {
            let writer = self.writer.lock().await;
            if tokio::time::Instant::now() >= deadline {
                return Err(SupervisorError::RequestTimeout);
            }
            if !self.accepting.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
                return Err(SupervisorError::ProcessStopped);
            }
            admit().await?;
            // timeout_at may poll an immediately-ready future after expiry. Never
            // let that admit a fresh frame after slow authorization or storage work.
            if tokio::time::Instant::now() >= deadline {
                return Err(SupervisorError::RequestTimeout);
            }
            if !self.accepting.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
                return Err(SupervisorError::ProcessStopped);
            }
            Ok(writer)
        });
        let admitted = tokio::select! {
            biased;
            _ = &mut stopped => {
                self.pending.lock().await.remove(&request_id);
                return RequestObservation::refused(SupervisorError::RequestCancelled);
            }
            result = admission => result,
        };
        let mut writer = match admitted {
            Ok(Ok(writer)) => writer,
            other => {
                self.pending.lock().await.remove(&request_id);
                return RequestObservation::refused(match other {
                    Ok(Err(error)) => error,
                    Err(_) => SupervisorError::RequestTimeout,
                    Ok(Ok(_)) => unreachable!(),
                });
            }
        };
        // Cancellation before the write future is polled is still a no-frame
        // refusal. Once entered, a failed/cancelled write fences partial bytes.
        let mut frame_started = false;
        let written = tokio::select! {
            biased;
            _ = &mut stopped => Err(SupervisorError::RequestCancelled),
            result = tokio::time::timeout_at(deadline, async {
                frame_started = true;
                writer.write_message(&Message::Request(request)).await
            }) => match result {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(error)) => Err(SupervisorError::from(error)),
                Err(_) => Err(SupervisorError::RequestTimeout),
            },
        };
        drop(writer);
        if let Err(failure) = written {
            self.pending.lock().await.remove(&request_id);
            if frame_started {
                let _ = self.kill().await;
            }
            return RequestObservation {
                frame_started,
                result: Err(failure),
                late_response: None,
            };
        }
        let received = tokio::select! {
            biased;
            _ = &mut stopped => Err(SupervisorError::RequestCancelled),
            result = tokio::time::timeout_at(deadline, &mut receiver) => match result {
                Ok(value) => Ok(value),
                Err(_) => Err(SupervisorError::RequestTimeout),
            },
        };
        match received {
            // timeout_at may return a ready receiver after its deadline. Preserve
            // that reply as late evidence without issuing an unnecessary Cancel.
            Ok(Ok(response)) if tokio::time::Instant::now() >= deadline => RequestObservation {
                frame_started: true,
                result: Err(SupervisorError::RequestTimeout),
                late_response: Some(response),
            },
            Ok(Ok(response)) => RequestObservation {
                frame_started: true,
                result: Ok(response),
                late_response: None,
            },
            Ok(Err(_)) => RequestObservation {
                frame_started: true,
                result: Err(SupervisorError::ProcessStopped),
                late_response: None,
            },
            Err(reason) => {
                self.finish_interrupted_request(request_id, &mut receiver, reason)
                    .await
            }
        }
    }

    async fn finish_interrupted_request(
        &self,
        request_id: WireId,
        receiver: &mut oneshot::Receiver<Response>,
        reason: SupervisorError,
    ) -> RequestObservation {
        // An already observed late response needs no new Cancel frame.
        if let Ok(response) = receiver.try_recv() {
            return RequestObservation {
                frame_started: true,
                result: Err(reason),
                late_response: Some(response),
            };
        }
        let grace = tokio::time::Instant::now() + self.limits.cancel_grace;
        let mut cancel_frame_started = false;
        let canceled = tokio::time::timeout_at(grace, async {
            let mut writer = self.writer.lock().await;
            if tokio::time::Instant::now() >= grace {
                return Err(SupervisorError::RequestTimeout);
            }
            cancel_frame_started = true;
            writer
                .write_message(&Message::Cancel(Cancel { request_id }))
                .await
                .map_err(SupervisorError::from)
        })
        .await;
        let late_response = match receiver.try_recv() {
            Ok(response) => Some(response),
            Err(oneshot::error::TryRecvError::Empty) if matches!(canceled, Ok(Ok(_))) => {
                match tokio::time::timeout_at(grace, receiver).await {
                    Ok(Ok(response)) => Some(response),
                    _ => None,
                }
            }
            Err(_) => None,
        };
        if late_response.is_none() || (cancel_frame_started && !matches!(canceled, Ok(Ok(_)))) {
            self.pending.lock().await.remove(&request_id);
            // Cleanup failure cannot erase the observed Stop/deadline cause.
            let _ = self.kill().await;
        }
        RequestObservation {
            frame_started: true,
            result: Err(reason),
            late_response,
        }
    }

    pub async fn request_stream(
        &self,
        request: Request,
    ) -> Result<(RpcStream, serde_json::Value), SupervisorError> {
        self.request_stream_with_credit(request, self.limits.max_frame_bytes as u64 + 4)
            .await
    }

    /// A typed method family can negotiate a smaller initial stream budget.
    pub async fn request_stream_with_credit(
        &self,
        mut request: Request,
        initial_credit_bytes: u64,
    ) -> Result<(RpcStream, serde_json::Value), SupervisorError> {
        if initial_credit_bytes == 0
            || initial_credit_bytes > self.limits.max_frame_bytes as u64 + 4
        {
            return Err(SupervisorError::InvalidConfiguration(
                "invalid initial stream credit".into(),
            ));
        }
        let stream_id = request
            .stream_id
            .unwrap_or_else(|| next_wire_id(&self.request_counter, self.spec.generation));
        request.stream_id = Some(stream_id);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        {
            let mut streams = self.streams.lock().expect("stream map poisoned");
            if streams.contains_key(&stream_id) {
                return Err(SupervisorError::ProtocolViolation(
                    "duplicate stream id".into(),
                ));
            }
            streams.insert(
                stream_id,
                StreamSink {
                    sender,
                    remaining_credit: initial_credit_bytes,
                    next_sequence: 0,
                },
            );
        }
        match self.request(request).await {
            Ok(Response {
                result:
                    ResponseResult::Stream {
                        stream_id: returned,
                        payload,
                    },
                ..
            }) if returned == stream_id => Ok((
                RpcStream {
                    stream_id,
                    receiver,
                    writer: self.writer.clone(),
                    streams: self.streams.clone(),
                },
                payload,
            )),
            Ok(_) => {
                self.streams
                    .lock()
                    .expect("stream map poisoned")
                    .remove(&stream_id);
                Err(SupervisorError::ProtocolViolation(
                    "stream request returned a mismatched response".into(),
                ))
            }
            Err(error) => {
                self.streams
                    .lock()
                    .expect("stream map poisoned")
                    .remove(&stream_id);
                Err(error)
            }
        }
    }

    pub async fn shutdown(&self, reason: &str) -> Result<(), SupervisorError> {
        self.begin_drain();
        if self.stopped.swap(true, Ordering::AcqRel) {
            // Reader/heartbeat fault detection marks stopped before awaiting kill.
            // Explicit cleanup must still join native process termination.
            return self.kill().await;
        }
        let _ = tokio::time::timeout(self.limits.cancel_grace, async {
            self.writer
                .lock()
                .await
                .write_message(&Message::Shutdown(Shutdown {
                    reason: reason.to_string(),
                }))
                .await
        })
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
        self.streams.lock().expect("stream map poisoned").clear();
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

fn negotiate_families(
    spec: &ProcessSpec,
    hello: &Hello,
) -> Result<BTreeMap<String, u32>, SupervisorError> {
    let mut selected = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for family in &hello.method_families {
        if !family.versions.is_valid() || !seen.insert(&family.family) {
            return Err(SupervisorError::ProtocolViolation(
                "invalid or duplicate method-family range".into(),
            ));
        }
        let version = if family.family == "driver" {
            sift_extension_protocol::negotiate_driver_rpc(
                spec.driver_rpc
                    .unwrap_or(sift_extension_protocol::VersionRange {
                        minimum: 1,
                        maximum: sift_extension_protocol::DRIVER_RPC_LATEST_VERSION,
                    }),
                family.versions,
            )
        } else {
            family.versions.contains(1).then_some(1)
        };
        if let Some(version) = version {
            selected.insert(family.family.clone(), version);
        }
    }
    if spec.driver_rpc.is_some() && !selected.contains_key("driver") {
        return Err(SupervisorError::IncompatibleVersion);
    }
    Ok(selected)
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

struct ReaderState {
    pending: Pending,
    health: Arc<RwLock<GenerationHealth>>,
    stopped: Arc<AtomicBool>,
    diagnostics: Arc<Mutex<DiagnosticRing>>,
    child: Arc<Mutex<Child>>,
    last_heartbeat: Arc<Mutex<tokio::time::Instant>>,
    streams: Streams,
}

fn spawn_reader(
    mut reader: FrameReader<tokio::process::ChildStdout>,
    state: ReaderState,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let ReaderState {
            pending,
            health,
            stopped,
            diagnostics,
            child,
            last_heartbeat,
            streams,
        } = state;
        loop {
            match reader.read_frame().await {
                Ok(crate::ReceivedMessage {
                    message: Message::Response(response),
                    ..
                }) => {
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
                Ok(crate::ReceivedMessage {
                    message: Message::Heartbeat(_),
                    ..
                }) => {
                    *last_heartbeat.lock().await = tokio::time::Instant::now();
                }
                Ok(crate::ReceivedMessage {
                    message: Message::Log(record),
                    ..
                }) => {
                    record_diagnostic(&diagnostics, &record.message).await;
                }
                Ok(crate::ReceivedMessage {
                    message: Message::Stream(frame),
                    encoded_bytes,
                }) => {
                    let outcome = {
                        let mut streams = streams.lock().expect("stream map poisoned");
                        streams.get_mut(&frame.stream_id).map(|stream| {
                            if frame.sequence != stream.next_sequence {
                                return Err("out-of-order stream sequence");
                            }
                            if encoded_bytes as u64 > stream.remaining_credit {
                                return Err("stream byte credit exceeded");
                            }
                            stream.next_sequence += 1;
                            stream.remaining_credit -= encoded_bytes as u64;
                            stream
                                .sender
                                .send(ReceivedStreamFrame {
                                    frame,
                                    encoded_bytes,
                                })
                                .map_err(|_| "stream consumer stopped")
                        })
                    };
                    match outcome {
                        Some(Ok(())) => {}
                        Some(Err(reason)) => {
                            record_diagnostic(&diagnostics, reason).await;
                            break;
                        }
                        None => {
                            record_diagnostic(
                                &diagnostics,
                                "protocol violation: unknown stream id",
                            )
                            .await;
                            break;
                        }
                    }
                }
                Ok(crate::ReceivedMessage { message, .. }) => {
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
        {
            let mut health = health.write().await;
            if *health != GenerationHealth::Stopped {
                *health = GenerationHealth::Degraded;
            }
        }
        pending.lock().await.clear();
        streams.lock().expect("stream map poisoned").clear();
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
    streams: Streams,
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
            {
                let mut health = health.write().await;
                if *health != GenerationHealth::Stopped {
                    *health = GenerationHealth::Degraded;
                }
            }
            pending.lock().await.clear();
            streams.lock().expect("stream map poisoned").clear();
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
