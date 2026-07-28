use std::io::{Read, Write};

use sift_extension_protocol::{
    ContributionId, ExtensionId, Heartbeat, Hello, Message, MethodFamilyRange, Response,
    ResponseResult, VersionRange, WireId,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let contribution = ContributionId::new("acme/conformance/database_provider/fixture")?;
    write_message(&Message::Hello(Hello {
        extension_rpc: VersionRange {
            minimum: 1,
            maximum: 1,
        },
        method_families: vec![MethodFamilyRange {
            family: "driver".into(),
            versions: VersionRange {
                minimum: 1,
                maximum: 1,
            },
        }],
        extension_id: ExtensionId::new("acme/conformance")?,
        extension_version: "1.0.0".into(),
        manifest_sha256: "a".repeat(64),
        process_nonce: WireId::from_u128(1),
        contributions: vec![contribution],
        max_concurrent_requests: 4,
    }))?;
    match read_message()? {
        Message::Welcome(_) => {}
        _ => return Err("host did not send welcome".into()),
    }

    let mut heartbeat = 0;
    loop {
        match read_message()? {
            Message::Request(request) => {
                serve_request(request)?;
            }
            Message::Cancel(cancel) => {
                write_message(&Message::Response(Response {
                    id: cancel.request_id,
                    result: ResponseResult::Error {
                        error: sift_extension_protocol::RpcError {
                            code: "canceled".into(),
                            message: "request canceled".into(),
                            retryable: false,
                            native_code: None,
                        },
                    },
                }))?;
            }
            Message::Credit(_) => {}
            Message::Shutdown(_) => return Ok(()),
            _ => return Err("unexpected host message".into()),
        }
        heartbeat += 1;
        write_message(&Message::Heartbeat(Heartbeat {
            sequence: heartbeat,
        }))?;
    }
}

fn serve_request(
    request: sift_extension_protocol::Request,
) -> Result<(), Box<dyn std::error::Error>> {
    use sift_extension_protocol::{
        DriverCatalog, DriverNamespace, DriverSchemaSnapshot, DriverStreamPayload, DriverValue,
        HandleResponse, OpenResponse, PingResponse,
    };

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
        "begin" => serde_json::to_value(HandleResponse {
            handle: WireId::from_u128(200),
        })?,
        "commit" | "rollback" | "cancel" | "close" => serde_json::json!({}),
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
                    sequence: sequence as u64,
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
