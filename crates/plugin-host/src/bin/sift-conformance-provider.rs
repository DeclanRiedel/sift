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
                if request.method == "execute" {
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
                        sift_extension_protocol::DriverStreamPayload::NextResult {
                            columns: vec![sift_extension_protocol::DriverColumn {
                                name: "answer".into(),
                                type_name: "int8".into(),
                                nullable: false,
                            }],
                        },
                        sift_extension_protocol::DriverStreamPayload::Rows {
                            rows: vec![vec![sift_extension_protocol::DriverValue::I64(42)]],
                        },
                        sift_extension_protocol::DriverStreamPayload::Done {
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
                } else {
                    write_message(&Message::Response(Response {
                        id: request.id,
                        result: ResponseResult::Ok {
                            payload: request.payload,
                        },
                    }))?;
                }
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
