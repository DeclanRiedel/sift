#[path = "conformance_notifications/mod.rs"]
mod notifications;
use std::io::{Read, Write};

use sift_extension_protocol::{
    ContributionId, ExtensionId, Heartbeat, Hello, Message, MethodFamilyRange, Response,
    ResponseResult, VersionRange, WireId,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::fs::read_to_string(".sift-conformance-mode")
        .unwrap_or_else(|_| "normal".into())
        .trim()
        .to_owned();
    let contribution = ContributionId::new("acme/conformance/database_provider/fixture")?;
    write_message(&Message::Hello(Hello {
        extension_rpc: VersionRange {
            minimum: 1,
            maximum: 1,
        },
        method_families: match mode.as_str() {
            "missing_driver" => vec![],
            "duplicate_driver" => vec![driver_family(1, 2), driver_family(1, 2)],
            "invalid_driver_range" => vec![driver_family(2, 1)],
            _ if mode.starts_with("v4") => vec![driver_family(1, 4)],
            _ if mode.starts_with("v3") => vec![driver_family(1, 3)],
            _ if mode.starts_with("v2") => vec![driver_family(1, 2)],
            _ => vec![driver_family(1, 1)],
        },
        extension_id: ExtensionId::new(if mode == "wrong_identity" {
            "acme/impostor"
        } else {
            "acme/conformance"
        })?,
        extension_version: "1.0.0".into(),
        manifest_sha256: "a".repeat(64),
        process_nonce: WireId::from_u128(1),
        contributions: vec![contribution],
        max_concurrent_requests: 4,
    }))?;
    let version = match read_message()? {
        Message::Welcome(welcome) => welcome.method_family_versions.get("driver").copied(),
        _ => return Err("host did not send welcome".into()),
    };
    if mode == "ignore_input" {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
    if mode == "unknown_response" {
        write_message(&Message::Response(Response {
            id: WireId::from_u128(u128::MAX),
            result: ResponseResult::Ok {
                payload: serde_json::json!({}),
            },
        }))?;
    }
    if mode == "stderr_secret" {
        eprintln!("password=hunter2");
    }

    let mut notifications = notifications::Subscriptions::default();
    let mut heartbeat = 0;
    let mut savepoints = Vec::<String>::new();
    loop {
        match read_message()? {
            Message::Request(request) => {
                if mode != "ignore_requests"
                    && mode != "reply_on_cancel"
                    && !notifications.request(&request, &mode, version)?
                {
                    serve_request(request, &mode, version, &mut savepoints)?;
                }
            }
            Message::Cancel(cancel) => {
                if mode != "ignore_requests" {
                    write_message(&Message::Response(Response {
                        id: cancel.request_id,
                        result: if mode == "reply_on_cancel" {
                            ResponseResult::Ok {
                                payload: serde_json::json!({"late_reply":true}),
                            }
                        } else {
                            ResponseResult::Error {
                                error: sift_extension_protocol::RpcError {
                                    code: "canceled".into(),
                                    message: "request canceled".into(),
                                    retryable: false,
                                    native_code: None,
                                },
                            }
                        },
                    }))?;
                }
            }
            Message::Credit(credit) => notifications.credit(credit, &mode)?,
            Message::Shutdown(_) => return Ok(()),
            _ => return Err("unexpected host message".into()),
        }
        if mode != "ignore_heartbeats" {
            heartbeat += 1;
            write_message(&Message::Heartbeat(Heartbeat {
                sequence: heartbeat,
            }))?;
        }
    }
}

fn serve_request(
    request: sift_extension_protocol::Request,
    mode: &str,
    version: Option<u32>,
    savepoints: &mut Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    use sift_extension_protocol::{
        DriverCatalog, DriverNamespace, DriverSchemaSnapshot, DriverStreamPayload, DriverValue,
        HandleResponse, OpenResponse, PingResponse,
    };

    if mode == format!("v2_{}_uncertain", request.method)
        && matches!(request.method.as_str(), "begin" | "commit" | "rollback")
    {
        return write_message(&Message::Response(Response {
            id: request.id,
            result: if request.method == "begin" {
                ResponseResult::Ok {
                    payload: serde_json::json!({"missing_handle": true}),
                }
            } else {
                ResponseResult::Error {
                    error: sift_extension_protocol::RpcError {
                        code: "driver_internal".into(),
                        message: "uncertain terminal outcome".into(),
                        retryable: false,
                        native_code: None,
                    },
                }
            },
        }));
    }
    let payload = match request.method.as_str() {
        "open" => serde_json::to_value(OpenResponse {
            connection: WireId::from_u128(100),
        })?,
        "ping" => serde_json::to_value(PingResponse {
            server_version: "conformance-1".into(),
            current_database: "fixture".into(),
            current_user: "fixture-user".into(),
        })?,
        "schema" => {
            let request: sift_extension_protocol::SchemaRequest =
                serde_json::from_value(request.payload)?;
            serde_json::to_value(DriverSchemaSnapshot {
                catalogs: vec![DriverCatalog {
                    name: "fixture".into(),
                    namespaces: vec![DriverNamespace {
                        name: "public".into(),
                        objects: vec![],
                    }],
                }],
                fetched_at_unix_ms: 0,
                scope: request.scope,
                incomplete: false,
            })?
        }
        "explain_estimated" => {
            if !version.is_some_and(|version| version >= 3) {
                return Err("estimated capture requires v3".into());
            }
            let capture: sift_extension_protocol::DriverEstimatedPlanRequest =
                serde_json::from_value(request.payload)?;
            if capture.connection != WireId::from_u128(100)
                || capture.sql
                    != if mode == "v3_mssql" {
                        "SELECT @P1"
                    } else if mode == "v3_sqlite" {
                        "SELECT ?1"
                    } else {
                        "SELECT $1"
                    }
                || capture.params != vec![sift_extension_protocol::DriverValue::I64(7)]
            {
                return Err("exact estimated SQL/typed params changed".into());
            }
            let plan = if mode == "v3_mssql" {
                sift_extension_protocol::DriverEstimatedPlan::SqlServerXml {xml:r#"<ShowPlanXML><RelOp PhysicalOp="Constant Scan" EstimateRows="1" EstimatedTotalSubtreeCost="0.1"/></ShowPlanXML>"#.into()}
            } else if mode == "v3_sqlite" {
                sift_extension_protocol::DriverEstimatedPlan::SqliteQueryPlan {
                    rows: vec![sift_extension_protocol::DriverSqlitePlanRow {
                        id: 1,
                        parent: 0,
                        aux: 0,
                        detail: "SCAN CONSTANT ROW".into(),
                    }],
                }
            } else {
                sift_extension_protocol::DriverEstimatedPlan::PostgresJson {
                    json: if mode == "v3_actual" {
                        serde_json::json!([{"Plan":{"Node Type":"Result","Actual Rows":1}}])
                    } else if mode == "v3_bad_shape" {
                        serde_json::json!([])
                    } else {
                        serde_json::json!([{"Plan":{"Node Type":"Result","Plan Rows":1,"Total Cost":0.01}}])
                    },
                }
            };
            if mode == "v3_hold" {
                std::fs::write(".sift-plan-entered", b"entered")?;
                while !std::path::Path::new(".sift-plan-release").exists() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            serde_json::to_value(sift_extension_protocol::DriverEstimatedPlanResponse {
                connection: if mode == "v3_wrong_handle" {
                    WireId::from_u128(999)
                } else {
                    capture.connection
                },
                disposition: if mode == "v3_unknown" {
                    sift_extension_protocol::ConnectionDisposition::Unknown
                } else {
                    sift_extension_protocol::ConnectionDisposition::Reusable
                },
                plan,
            })?
        }
        "begin" => serde_json::to_value(HandleResponse {
            handle: WireId::from_u128(200),
        })?,
        "savepoint" | "rollback_to_savepoint" | "release_savepoint" => {
            if !version.is_some_and(|version| version >= 2) {
                return Err("savepoints require negotiated v2".into());
            }
            let control: sift_extension_protocol::DriverSavepointRequest =
                serde_json::from_value(request.payload)?;
            if control.transaction != WireId::from_u128(200)
                || !sift_extension_protocol::valid_savepoint_name(&control.name)
            {
                return Err("invalid savepoint request".into());
            }
            if mode == "v2_delay" || mode == "v2_hold" {
                std::fs::write(".sift-savepoint-entered", b"entered")?;
                if mode == "v2_hold" {
                    let start = std::time::Instant::now();
                    while !std::path::Path::new(".sift-savepoint-release").exists() {
                        if start.elapsed() > std::time::Duration::from_secs(10) {
                            return Err("fixture release deadline".into());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
            }
            match request.method.as_str() {
                "savepoint" => {
                    if savepoints.contains(&control.name) {
                        return Err("duplicate savepoint".into());
                    }
                    savepoints.push(control.name.clone());
                }
                method => {
                    let index = savepoints
                        .iter()
                        .position(|name| name == &control.name)
                        .ok_or("unknown savepoint")?;
                    savepoints.truncate(index + usize::from(method == "rollback_to_savepoint"));
                }
            }
            if mode == "v2_error" {
                write_message(&Message::Response(Response {
                    id: request.id,
                    result: ResponseResult::Error {
                        error: sift_extension_protocol::RpcError {
                            code: "driver_internal".into(),
                            message: "fixture failure".into(),
                            retryable: false,
                            native_code: None,
                        },
                    },
                }))?;
                return Ok(());
            }
            if mode == "v2_exit" {
                return Err("fixture disconnected".into());
            }
            if mode == "v2_timeout" {
                std::thread::sleep(std::time::Duration::from_secs(35));
            }
            if mode == "v2_malformed_ack" {
                return write_message(&Message::Response(Response {
                    id: request.id,
                    result: ResponseResult::Ok {
                        payload: serde_json::json!({"ok":true}),
                    },
                }));
            }
            if mode == "v2_bad_name" {
                return write_message(&Message::Response(Response {
                    id: request.id,
                    result: ResponseResult::Ok {
                        payload: serde_json::json!({"transaction":control.transaction,"name":"other"}),
                    },
                }));
            }
            if mode == "v2_bad_ack"
                || (mode == "v2_fail_recovery" && request.method == "rollback_to_savepoint")
            {
                serde_json::json!({"transaction": WireId::from_u128(999), "name": control.name})
            } else {
                serde_json::to_value(sift_extension_protocol::DriverSavepointAck {
                    transaction: control.transaction,
                    name: control.name,
                })?
            }
        }
        "commit" | "rollback" | "close" => {
            if request.method == "close" {
                std::fs::write(".sift-connection-closed", b"closed")?;
            }
            savepoints.clear();
            serde_json::json!({})
        }
        "cancel" => serde_json::json!({}),
        "invoke" => {
            let request: sift_extension_protocol::InvokeActionRequest =
                serde_json::from_value(request.payload)?;
            serde_json::to_value(sift_extension_protocol::InvokeActionResponse {
                result: request.arguments,
            })?
        }
        "execute" => {
            let _: sift_extension_protocol::ExecuteDriverRequest =
                serde_json::from_value(request.payload)?;
            let stream_id = request.stream_id.ok_or("execute requires stream id")?;
            write_message(&Message::Response(Response {
                id: request.id,
                result: ResponseResult::Stream {
                    stream_id,
                    payload: serde_json::to_value(sift_extension_protocol::ExecuteStart {
                        query: WireId::from_u128(500),
                    })?,
                },
            }))?;
            for (sequence, payload) in [
                DriverStreamPayload::NextResult {
                    columns: vec![sift_extension_protocol::DriverColumn {
                        name: "answer".into(),
                        type_name: "int8".into(),
                        nullable: false,
                    }],
                },
                DriverStreamPayload::Rows {
                    rows: vec![vec![DriverValue::I64(42)]],
                },
                DriverStreamPayload::Done {
                    affected_rows: None,
                    warnings: vec![],
                },
            ]
            .into_iter()
            .enumerate()
            {
                write_message(&Message::Stream(sift_extension_protocol::StreamFrame {
                    stream_id,
                    sequence: if mode == "out_of_order_stream" && sequence == 1 {
                        2
                    } else {
                        sequence as u64
                    },
                    payload: serde_json::to_value(payload)?,
                }))?;
            }
            return Ok(());
        }
        _ => {
            write_message(&Message::Response(Response {
                id: request.id,
                result: ResponseResult::Error {
                    error: sift_extension_protocol::RpcError {
                        code: "unsupported_for_engine".into(),
                        message: "unknown conformance method".into(),
                        retryable: false,
                        native_code: None,
                    },
                },
            }))?;
            return Ok(());
        }
    };
    write_message(&Message::Response(Response {
        id: request.id,
        result: ResponseResult::Ok { payload },
    }))?;
    Ok(())
}

fn read_message() -> Result<Message, Box<dyn std::error::Error>> {
    let mut length = [0_u8; 4];
    std::io::stdin().read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    let mut payload = vec![0_u8; length];
    std::io::stdin().read_exact(&mut payload)?;
    Ok(serde_json::from_slice(&payload)?)
}

fn write_message(message: &Message) -> Result<(), Box<dyn std::error::Error>> {
    let payload = serde_json::to_vec(message)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&(payload.len() as u32).to_be_bytes())?;
    stdout.write_all(&payload)?;
    stdout.flush()?;
    Ok(())
}

fn driver_family(minimum: u32, maximum: u32) -> MethodFamilyRange {
    MethodFamilyRange {
        family: "driver".into(),
        versions: VersionRange { minimum, maximum },
    }
}
