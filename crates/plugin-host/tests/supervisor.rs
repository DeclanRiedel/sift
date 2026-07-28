use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sift_extension_protocol::{ContributionId, ExtensionId, Request, RequestContext, WireId};
use sift_plugin_host::{ProcessSpec, SupervisedProcess, SupervisorLimits};

#[tokio::test]
async fn supervised_conformance_process_handshakes_and_serves_requests() {
    let temp = tempfile::tempdir().unwrap();
    let process = SupervisedProcess::start(
        ProcessSpec {
            executable: env!("CARGO_BIN_EXE_sift-conformance-provider").into(),
            working_directory: temp.path().into(),
            extension_id: ExtensionId::new("acme/conformance").unwrap(),
            extension_version: "1.0.0".into(),
            manifest_sha256: "a".repeat(64),
            expected_contributions: vec![ContributionId::new(
                "acme/conformance/database_provider/fixture",
            )
            .unwrap()],
            generation: WireId::from_u128(55),
            granted_capabilities: vec![],
        },
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
            method: "ping".into(),
            payload: serde_json::json!({"probe": true}),
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
            assert_eq!(payload, serde_json::json!({"probe": true}));
        }
        other => panic!("unexpected response: {other:?}"),
    }

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
            payload: serde_json::json!({}),
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
