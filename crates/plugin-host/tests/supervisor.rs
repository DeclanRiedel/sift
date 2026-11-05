use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sift_extension_protocol::{ContributionId, ExtensionId, Request, RequestContext, WireId};
use sift_plugin_host::{
    GenerationHealth, ProcessSpec, SupervisedProcess, SupervisorError, SupervisorLimits,
};

fn process_spec(working_directory: &std::path::Path) -> ProcessSpec {
    ProcessSpec {
        executable: env!("CARGO_BIN_EXE_sift-conformance-provider").into(),
        working_directory: working_directory.into(),
        extension_id: ExtensionId::new("acme/conformance").unwrap(),
        extension_version: "1.0.0".into(),
        manifest_sha256: "a".repeat(64),
        expected_contributions: vec![ContributionId::new(
            "acme/conformance/database_provider/fixture",
        )
        .unwrap()],
        driver_rpc: Some(sift_extension_protocol::VersionRange {
            minimum: 1,
            maximum: 1,
        }),
        generation: WireId::from_u128(55),
        granted_capabilities: vec![],
    }
}

fn deadline_after(duration: Duration) -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + duration.as_millis() as i64
}

fn request(method: &str, deadline: i64) -> Request {
    Request {
        id: WireId::from_u128(0),
        contribution_id: ContributionId::new("acme/conformance/database_provider/fixture").unwrap(),
        method: method.into(),
        payload: serde_json::json!({}),
        correlation_id: WireId::from_u128(99),
        deadline_unix_ms: deadline,
        context: None,
        stream_id: None,
    }
}

async fn wait_for_health(process: &SupervisedProcess, expected: GenerationHealth) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if process.health().await == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn supervised_conformance_process_handshakes_and_serves_requests() {
    let temp = tempfile::tempdir().unwrap();
    let process = SupervisedProcess::start(
        process_spec(temp.path()),
        SupervisorLimits {
            heartbeat_interval: Duration::from_secs(30),
            ..SupervisorLimits::default()
        },
    )
    .await
    .unwrap();

    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 5_000;
    let response = process
        .request(Request {
            id: WireId::from_u128(0),
            contribution_id: ContributionId::new("acme/conformance/database_provider/fixture")
                .unwrap(),
            method: "open".into(),
            payload: serde_json::to_value(sift_extension_protocol::OpenRequest {
                configuration: serde_json::json!({}),
                credentials: vec![],
            })
            .unwrap(),
            correlation_id: WireId::from_u128(99),
            deadline_unix_ms: deadline,
            context: Some(RequestContext {
                tenant_id: Some(1),
                room_id: None,
            }),
            stream_id: None,
        })
        .await
        .unwrap();
    match response.result {
        sift_extension_protocol::ResponseResult::Ok { payload } => {
            let opened: sift_extension_protocol::OpenResponse =
                serde_json::from_value(payload).unwrap();
            assert_eq!(opened.connection, WireId::from_u128(100));
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let response = process
        .request(Request {
            id: WireId::from_u128(0),
            contribution_id: ContributionId::new("acme/conformance/database_provider/fixture")
                .unwrap(),
            method: "invoke".into(),
            payload: serde_json::to_value(sift_extension_protocol::InvokeActionRequest {
                action: sift_extension_protocol::SegmentId::new("echo").unwrap(),
                target_kind: sift_extension_protocol::SegmentId::new("fixture").unwrap(),
                target_id: None,
                arguments: serde_json::json!({"safe": true}),
            })
            .unwrap(),
            correlation_id: WireId::from_u128(101),
            deadline_unix_ms: deadline,
            context: None,
            stream_id: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        response.result,
        sift_extension_protocol::ResponseResult::Ok { payload }
            if payload == serde_json::json!({"result": {"safe": true}})
    ));

    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 5_000;
    let (mut stream, start) = process
        .request_stream(Request {
            id: WireId::from_u128(0),
            contribution_id: ContributionId::new("acme/conformance/database_provider/fixture")
                .unwrap(),
            method: "execute".into(),
            payload: serde_json::to_value(sift_extension_protocol::ExecuteDriverRequest {
                connection: WireId::from_u128(100),
                sql: "select 42".into(),
                params: vec![],
            })
            .unwrap(),
            correlation_id: WireId::from_u128(100),
            deadline_unix_ms: deadline,
            context: None,
            stream_id: None,
        })
        .await
        .unwrap();
    let start: sift_extension_protocol::ExecuteStart = serde_json::from_value(start).unwrap();
    assert_eq!(start.query, WireId::from_u128(500));
    let mut payloads = Vec::new();
    for _ in 0..3 {
        let frame = stream.next().await.unwrap();
        payloads.push(
            serde_json::from_value::<sift_extension_protocol::DriverStreamPayload>(
                frame.frame.payload.clone(),
            )
            .unwrap(),
        );
        stream.accept(&frame).await.unwrap();
    }
    assert!(matches!(
        payloads.last(),
        Some(sift_extension_protocol::DriverStreamPayload::Done { .. })
    ));
    process.shutdown("test complete").await.unwrap();
}

#[tokio::test]
async fn identity_mismatch_is_rejected_during_handshake() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(".sift-conformance-mode"), "wrong_identity").unwrap();
    let error = match SupervisedProcess::start(
        process_spec(temp.path()),
        SupervisorLimits::default(),
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("identity mismatch must reject the process"),
    };
    assert!(matches!(error, SupervisorError::IdentityMismatch));
}

#[tokio::test]
async fn unknown_response_degrades_and_stops_only_the_extension() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join(".sift-conformance-mode"),
        "unknown_response",
    )
    .unwrap();
    let process = SupervisedProcess::start(
        process_spec(temp.path()),
        SupervisorLimits {
            heartbeat_interval: Duration::from_secs(30),
            ..SupervisorLimits::default()
        },
    )
    .await
    .unwrap();
    wait_for_health(&process, GenerationHealth::Degraded).await;
    assert!(process
        .diagnostics()
        .await
        .iter()
        .any(|line| line.contains("unknown request")));
}

#[tokio::test]
async fn missed_heartbeat_degrades_and_kills_the_extension() {
    let temp = tempfile::tempdir().unwrap();
    let process = SupervisedProcess::start(
        process_spec(temp.path()),
        SupervisorLimits {
            heartbeat_interval: Duration::from_millis(20),
            missed_heartbeats: 1,
            ..SupervisorLimits::default()
        },
    )
    .await
    .unwrap();
    wait_for_health(&process, GenerationHealth::Degraded).await;
    assert!(process
        .diagnostics()
        .await
        .iter()
        .any(|line| line.contains("heartbeat deadline")));
}

#[tokio::test]
async fn ignored_cancellation_is_bounded_and_kills_the_extension() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join(".sift-conformance-mode"),
        "ignore_requests",
    )
    .unwrap();
    let process = SupervisedProcess::start(
        process_spec(temp.path()),
        SupervisorLimits {
            cancel_grace: Duration::from_millis(20),
            heartbeat_interval: Duration::from_secs(30),
            ..SupervisorLimits::default()
        },
    )
    .await
    .unwrap();
    let error = process
        .request(request("ignore", deadline_after(Duration::from_millis(40))))
        .await
        .unwrap_err();
    assert!(matches!(error, SupervisorError::RequestTimeout));
    assert_eq!(process.health().await, GenerationHealth::Stopped);
}

#[tokio::test]
async fn secret_shaped_stderr_is_redacted() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(".sift-conformance-mode"), "stderr_secret").unwrap();
    let process = SupervisedProcess::start(
        process_spec(temp.path()),
        SupervisorLimits {
            heartbeat_interval: Duration::from_secs(30),
            ..SupervisorLimits::default()
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let diagnostics = process.diagnostics().await;
    assert!(diagnostics
        .iter()
        .any(|line| line == "[redacted secret-shaped diagnostic]"));
    assert!(!diagnostics.iter().any(|line| line.contains("hunter2")));
    process.shutdown("test complete").await.unwrap();
}

#[tokio::test]
async fn out_of_order_stream_is_contained_as_a_protocol_violation() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join(".sift-conformance-mode"),
        "out_of_order_stream",
    )
    .unwrap();
    let process = SupervisedProcess::start(
        process_spec(temp.path()),
        SupervisorLimits {
            heartbeat_interval: Duration::from_secs(30),
            ..SupervisorLimits::default()
        },
    )
    .await
    .unwrap();
    let mut execute = request("execute", deadline_after(Duration::from_secs(1)));
    execute.payload = serde_json::to_value(sift_extension_protocol::ExecuteDriverRequest {
        connection: WireId::from_u128(100),
        sql: "select 42".into(),
        params: vec![],
    })
    .unwrap();
    let (mut stream, _) = process.request_stream(execute).await.unwrap();
    let first = stream.next().await.unwrap();
    let _ = stream.accept(&first).await;
    assert!(stream.next().await.is_none());
    wait_for_health(&process, GenerationHealth::Degraded).await;
    assert!(process
        .diagnostics()
        .await
        .iter()
        .any(|line| line.contains("out-of-order stream sequence")));
}

#[tokio::test]
async fn generation_drain_closes_admission_without_affecting_the_host() {
    let temp = tempfile::tempdir().unwrap();
    let process = SupervisedProcess::start(
        process_spec(temp.path()),
        SupervisorLimits {
            heartbeat_interval: Duration::from_secs(30),
            ..SupervisorLimits::default()
        },
    )
    .await
    .unwrap();
    process.begin_drain();
    let error = process
        .request(request("ping", deadline_after(Duration::from_secs(1))))
        .await
        .unwrap_err();
    assert!(matches!(error, SupervisorError::ProcessStopped));
    assert_eq!(process.health().await, GenerationHealth::Ready);
    process.shutdown("drain complete").await.unwrap();
}

#[tokio::test]
async fn driver_negotiation_intersects_manifest_hello_and_host() {
    use sift_extension_protocol::VersionRange;
    for (mode, minimum, maximum, expected) in [
        ("normal", 1, 2, Some(1)),
        ("v2", 1, 2, Some(2)),
        ("v2", 1, 1, Some(1)),
        ("v2", 2, 2, Some(2)),
        ("normal", 2, 2, None),
        ("missing_driver", 1, 2, None),
        ("duplicate_driver", 1, 2, None),
        ("invalid_driver_range", 1, 2, None),
    ] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".sift-conformance-mode"), mode).unwrap();
        let mut spec = process_spec(temp.path());
        spec.driver_rpc = Some(VersionRange { minimum, maximum });
        let result = SupervisedProcess::start(spec, SupervisorLimits::default()).await;
        match expected {
            Some(version) => {
                let process = result.unwrap();
                assert_eq!(
                    process.method_family_version("driver"),
                    Some(version),
                    "{mode}"
                );
                if version == 2 {
                    // The fixture checks Welcome and models native nesting. A
                    // wrong negotiated version or stack operation exits it.
                    for (method, name) in [
                        ("savepoint", "outer"),
                        ("savepoint", "inner"),
                        ("rollback_to_savepoint", "outer"),
                        ("savepoint", "inner"),
                        ("release_savepoint", "inner"),
                        ("release_savepoint", "outer"),
                    ] {
                        let mut call = request(method, deadline_after(Duration::from_secs(5)));
                        call.payload =
                            serde_json::to_value(sift_extension_protocol::DriverSavepointRequest {
                                transaction: WireId::from_u128(200),
                                name: name.into(),
                            })
                            .unwrap();
                        let response = process.request(call).await.unwrap();
                        let sift_extension_protocol::ResponseResult::Ok { payload } =
                            response.result
                        else {
                            panic!("expected savepoint ack")
                        };
                        let ack: sift_extension_protocol::DriverSavepointAck =
                            serde_json::from_value(payload).unwrap();
                        assert_eq!(ack.transaction, WireId::from_u128(200));
                        assert_eq!(ack.name, name);
                    }
                }
                process.shutdown("test complete").await.unwrap();
            }
            None => assert!(
                matches!(
                    result,
                    Err(SupervisorError::IncompatibleVersion
                        | SupervisorError::ProtocolViolation(_))
                ),
                "{mode}"
            ),
        }
    }
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(".sift-conformance-mode"), "missing_driver").unwrap();
    let mut spec = process_spec(temp.path());
    spec.driver_rpc = None;
    let process = SupervisedProcess::start(spec, SupervisorLimits::default())
        .await
        .unwrap();
    assert_eq!(process.method_family_version("driver"), None);
    process.shutdown("action-only package").await.unwrap();
}

#[tokio::test]
async fn blocked_provider_input_cannot_extend_request_or_shutdown_deadlines() {
    for shutdown in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".sift-conformance-mode"), "ignore_input").unwrap();
        let process = SupervisedProcess::start(
            process_spec(temp.path()),
            SupervisorLimits {
                heartbeat_interval: Duration::from_secs(60),
                cancel_grace: Duration::from_millis(100),
                ..SupervisorLimits::default()
            },
        )
        .await
        .unwrap();
        let mut work = request(
            "ping",
            deadline_after(if shutdown {
                Duration::from_secs(10)
            } else {
                Duration::from_millis(100)
            }),
        );
        work.payload = serde_json::json!({"padding": "x".repeat(1024 * 1024)});
        tokio::time::timeout(Duration::from_secs(2), async {
            if shutdown {
                let pending = process.request(work);
                let stop = async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    process.shutdown("test blocked writer").await.unwrap();
                };
                let (result, _) = tokio::join!(pending, stop);
                assert!(result.is_err());
            } else {
                assert!(matches!(
                    process.request(work).await,
                    Err(SupervisorError::RequestTimeout)
                ));
            }
        })
        .await
        .expect("blocked stdin must not defeat deadlines");
        assert_eq!(process.active_work().await, 0);
        assert_eq!(process.health().await, GenerationHealth::Stopped);
        assert!(matches!(
            process
                .request(request("ping", deadline_after(Duration::from_secs(1))))
                .await,
            Err(SupervisorError::ProcessStopped)
        ));
    }
}
