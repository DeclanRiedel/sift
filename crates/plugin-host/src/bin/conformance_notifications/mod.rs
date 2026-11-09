use super::*;
use sift_extension_protocol::{
    ConnectionDisposition, Credit, DriverListenAck, DriverListenRequest, DriverNotificationFrame,
    DriverUnlistenAck, DriverUnlistenRequest, Request, StreamFrame,
};
use std::collections::{HashMap, VecDeque};
#[derive(Default)]
pub(super) struct Subscriptions(HashMap<WireId, Subscription>, HashMap<WireId, WireId>);
struct Subscription {
    input: DriverListenRequest,
    sequence: u64,
    credit: u64,
    events: VecDeque<serde_json::Value>,
}
fn hold(entered: &str, release: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(entered, b"entered")?;
    while !std::path::Path::new(release).exists() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}
impl Subscriptions {
    pub(super) fn request(
        &mut self,
        request: &Request,
        mode: &str,
        version: Option<u32>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if !mode.starts_with("v4") {
            return Ok(false);
        }
        match request.method.as_str() {
            "listen" => {
                if version != Some(4) {
                    return Err("v4 required".into());
                }
                let input: DriverListenRequest = serde_json::from_value(request.payload.clone())?;
                if request.stream_id != Some(input.subscription)
                    || input.connection != WireId::from_u128(100)
                    || input.initial_credit_bytes > 128 * 1024
                    || input.max_event_bytes > 64 * 1024
                {
                    return Err("invalid listener request".into());
                }
                if mode == "v4_listen_hold" {
                    hold(".sift-listen-entered", ".sift-listen-release")?;
                }
                let ack = DriverListenAck {
                    connection: input.connection,
                    subscription: input.subscription,
                    channels: input.channels.clone(),
                    disposition: if mode == "v4_bad_ack" {
                        ConnectionDisposition::Unknown
                    } else {
                        ConnectionDisposition::Reusable
                    },
                };
                write_message(&Message::Response(Response {
                    id: request.id,
                    result: ResponseResult::Stream {
                        stream_id: input.subscription,
                        payload: serde_json::to_value(ack)?,
                    },
                }))?;
                let mut events = VecDeque::new();
                let count = if mode == "v4_flood" {
                    512
                } else if matches!(mode, "v4_quiet" | "v4_stop_hold") {
                    1
                } else {
                    2
                };
                for i in 0..count {
                    let channel = if mode == "v4_wrong_channel" {
                        "unexpected".into()
                    } else {
                        input.channels[i % input.channels.len()].clone()
                    };
                    let payload = if mode == "v4_oversized" {
                        "x".repeat(input.max_event_bytes as usize)
                    } else if mode == "v4_flood" {
                        "x".repeat(4096)
                    } else if i == 0 {
                        "one".into()
                    } else {
                        "通知🦀".into()
                    };
                    let event = DriverNotificationFrame::Notification {
                        connection: if mode == "v4_wrong_handle" {
                            WireId::from_u128(999)
                        } else {
                            input.connection
                        },
                        subscription: if mode == "v4_wrong_subscription" {
                            WireId::from_u128(999)
                        } else {
                            input.subscription
                        },
                        channel,
                        payload,
                    };
                    events.push_back(if mode == "v4_malformed" {
                        serde_json::json!({"private_payload":"never_audit"})
                    } else {
                        serde_json::to_value(event)?
                    });
                }
                if mode == "v4_unexpected_stop" {
                    events.push_back(serde_json::to_value(DriverNotificationFrame::Stopped {
                        connection: input.connection,
                        subscription: input.subscription,
                        disposition: ConnectionDisposition::Reusable,
                    })?);
                }
                self.0.insert(
                    input.subscription,
                    Subscription {
                        credit: input.initial_credit_bytes,
                        input,
                        sequence: if mode == "v4_bad_sequence" { 1 } else { 0 },
                        events,
                    },
                );
                self.flush(mode)?;
                std::fs::write(".sift-listen-active", self.0.len().to_string())?;
                Ok(true)
            }
            "unlisten" => {
                let input: DriverUnlistenRequest = serde_json::from_value(request.payload.clone())?;
                if !self.0.contains_key(&input.subscription) {
                    if self.1.get(&input.subscription) != Some(&input.connection) {
                        return Err("unknown subscription".into());
                    }
                    write_message(&Message::Response(Response {
                        id: request.id,
                        result: ResponseResult::Ok {
                            payload: serde_json::to_value(DriverUnlistenAck {
                                connection: input.connection,
                                subscription: input.subscription,
                                disposition: ConnectionDisposition::Reusable,
                            })?,
                        },
                    }))?;
                    return Ok(true);
                }
                let subscription = self
                    .0
                    .get_mut(&input.subscription)
                    .expect("checked subscription");
                if subscription.input.connection != input.connection {
                    return Err("wrong base handle".into());
                }
                std::fs::write(".sift-unlisten-entered", b"entered")?;
                if mode == "v4_stop_hold" {
                    hold(".sift-unlisten-entered", ".sift-unlisten-release")?;
                }
                subscription.events.clear();
                let ack = Message::Response(Response {
                    id: request.id,
                    result: ResponseResult::Ok {
                        payload: serde_json::to_value(DriverUnlistenAck {
                            connection: input.connection,
                            subscription: if mode == "v4_bad_stop" {
                                WireId::from_u128(999)
                            } else {
                                input.subscription
                            },
                            disposition: ConnectionDisposition::Reusable,
                        })?,
                    },
                });
                if mode == "v4_ack_first" {
                    write_message(&ack)?;
                }
                if mode != "v4_missing_barrier" {
                    let event = serde_json::to_value(DriverNotificationFrame::Stopped {
                        connection: input.connection,
                        subscription: input.subscription,
                        disposition: ConnectionDisposition::Reusable,
                    })?;
                    subscription.events.push_back(event);
                    self.flush(mode)?;
                }
                if mode != "v4_ack_first" {
                    write_message(&ack)?;
                }
                // The host may need to return credit before the barrier fits.
                if self
                    .0
                    .get(&input.subscription)
                    .is_some_and(|s| s.events.is_empty())
                {
                    self.0.remove(&input.subscription);
                    std::fs::write(".sift-listen-active", self.0.len().to_string())?;
                    std::fs::write(".sift-unlisten-stopped", b"stopped")?;
                }
                Ok(true)
            }
            "close" => {
                if !self.0.is_empty() {
                    return Err("base close before subscription cleanup".into());
                }
                std::fs::write(".sift-native-close", b"closed")?;
                Ok(false)
            }
            _ => Ok(false),
        }
    }
    pub(super) fn credit(
        &mut self,
        credit: Credit,
        mode: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(subscription) = self.0.get_mut(&credit.stream_id) {
            subscription.credit = subscription.credit.saturating_add(credit.bytes);
        }
        self.flush(mode)
    }
    fn flush(&mut self, _mode: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut stopped = Vec::new();
        for (id, subscription) in &mut self.0 {
            while let Some(payload) = subscription.events.front() {
                let event = Message::Stream(StreamFrame {
                    stream_id: *id,
                    sequence: subscription.sequence,
                    payload: payload.clone(),
                });
                let bytes = serde_json::to_vec(&event)?.len() as u64 + 4;
                if bytes > subscription.credit {
                    break;
                }
                subscription.credit -= bytes;
                subscription.sequence += 1;
                let terminal =
                    payload.get("event").and_then(serde_json::Value::as_str) == Some("stopped");
                write_message(&event)?;
                subscription.events.pop_front();
                if terminal {
                    stopped.push(*id);
                    break;
                }
            }
        }
        for id in stopped {
            if let Some(subscription) = self.0.remove(&id) {
                self.1.insert(id, subscription.input.connection);
            }
            std::fs::write(".sift-listen-active", self.0.len().to_string())?;
            std::fs::write(".sift-unlisten-stopped", b"stopped")?;
        }
        Ok(())
    }
}
