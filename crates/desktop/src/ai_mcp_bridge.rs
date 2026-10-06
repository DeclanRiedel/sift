//! Ephemeral, authenticated Sift-only MCP transport for isolated provider CLIs.
use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use sift_client_sdk::Client;
use sift_protocol::{AiRunLease, AiTurnContext};
use sift_workspace_ui::ExecutorEvent;
use tokio::sync::{mpsc::UnboundedSender, Semaphore};
use uuid::Uuid;

struct BridgeState {
    capability: String,
    client: Client,
    lease: AiRunLease,
    context: AiTurnContext,
    events: UnboundedSender<ExecutorEvent>,
    tools: Vec<Value>,
    calls: Semaphore,
    receipts: tokio::sync::Mutex<std::collections::HashMap<String, (Value, Uuid)>>,
    max_calls: usize,
    initialized: std::sync::atomic::AtomicBool,
    stopping: tokio::sync::watch::Sender<bool>,
}

pub(crate) struct Bridge {
    pub(crate) url: String,
    pub(crate) capability: String,
    task: tokio::task::JoinHandle<()>,
    stopping: tokio::sync::watch::Sender<bool>,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stopping.send_replace(true);
        self.task.abort();
    }
}
impl Bridge {
    pub(crate) async fn start(
        client: Client,
        lease: AiRunLease,
        context: AiTurnContext,
        events: UnboundedSender<ExecutorEvent>,
    ) -> Result<Self, String> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "Cannot start the local Sift tool connection")?;
        let url = format!(
            "http://{}/mcp",
            listener
                .local_addr()
                .map_err(|_| "Cannot inspect the local Sift tool connection")?
        );
        let capability = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let policy = client
            .ai_policy()
            .await
            .map_err(|_| "Cannot load Sift tool limits")?;
        let tools = crate::ai_tools::tools(lease.run.mode)?
            .into_iter()
            .map(|mut tool| {
                tool.as_object_mut().expect("tool object").remove("type");
                tool.as_object_mut()
                    .expect("tool object")
                    .remove("deferLoading");
                let read_only = tool["name"].as_str() != Some("sift_select")
                    && !tool["name"]
                        .as_str()
                        .unwrap_or("")
                        .starts_with("sift_stage_");
                tool["annotations"] = json!({"readOnlyHint":read_only,"openWorldHint":false});
                tool
            })
            .collect();
        let (stopping, _) = tokio::sync::watch::channel(false);
        let state = Arc::new(BridgeState {
            capability: capability.clone(),
            client,
            lease,
            context,
            events,
            tools,
            calls: Semaphore::new(4),
            receipts: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            max_calls: usize::try_from(policy.max_tool_calls_per_run)
                .unwrap_or(20)
                .min(128),
            initialized: std::sync::atomic::AtomicBool::new(false),
            stopping: stopping.clone(),
        });
        let router = Router::new()
            .route("/mcp", post(handle).get(no_event_stream))
            .layer(DefaultBodyLimit::max(1100 * 1024))
            .with_state(state);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            url,
            capability,
            task,
            stopping,
        })
    }
}

fn admitted(headers: &HeaderMap, state: &BridgeState) -> bool {
    // CLI clients are not browsers. Any Origin is rejected to close rebinding.
    !headers.contains_key("origin")
        && headers
            .get("authorization")
            .and_then(|header| header.to_str().ok())
            .and_then(|header| header.strip_prefix("Bearer "))
            .is_some_and(|capability| capability == state.capability)
        && !*state.stopping.borrow()
}
async fn no_event_stream(State(state): State<Arc<BridgeState>>, headers: HeaderMap) -> Response {
    if !admitted(&headers, &state) {
        return StatusCode::FORBIDDEN.into_response();
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}
fn error(id: Value, code: i32, message: &str) -> Response {
    Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})).into_response()
}
async fn handle(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if !admitted(&headers, &state) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|version| !matches!(version, "2025-03-26" | "2025-06-18" | "2025-11-25"))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || request.is_array() {
        return error(id, -32600, "Invalid bounded MCP request");
    }
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    if method == "notifications/initialized" {
        if request.get("id").is_some() {
            return error(id, -32600, "Notifications cannot have request IDs");
        }
        return StatusCode::ACCEPTED.into_response();
    }
    if request.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    if !(id.is_string() || id.is_number()) {
        return error(id, -32600, "Invalid MCP request identity");
    }
    let result = match method {
        "initialize" => {
            let requested = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = match requested {
                "2025-03-26" | "2025-06-18" | "2025-11-25" => requested,
                _ => "2025-11-25",
            };
            state
                .initialized
                .store(true, std::sync::atomic::Ordering::Release);
            json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"sift_turn","version":env!("CARGO_PKG_VERSION")},"instructions":"Only governed Sift reads and human-reviewed staging are available. No apply, filesystem, shell or arbitrary MCP access."})
        }
        "ping" => json!({}),
        _ if !state.initialized.load(std::sync::atomic::Ordering::Acquire) => {
            return error(id, -32002, "Initialize the Sift tool connection first")
        }
        "tools/list" => json!({"tools":state.tools}),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !state
                .tools
                .iter()
                .any(|tool| tool["name"].as_str() == Some(name))
            {
                return error(id, -32602, "Tool is not available in this Sift turn");
            }
            let Some(arguments) = request
                .pointer("/params/arguments")
                .filter(|value| value.is_object())
                .cloned()
            else {
                return error(id, -32602, "Tool arguments must be an object");
            };
            let Ok(_permit) = state.calls.try_acquire() else {
                return error(id, -32000, "Sift tool concurrency limit reached");
            };
            let invocation_id = {
                let mut receipts = state.receipts.lock().await;
                let key = id.to_string();
                let input = json!({"name":name,"arguments":arguments});
                if let Some((prior, invocation)) = receipts.get(&key) {
                    if prior != &input {
                        return error(id, -32602, "MCP request identity changed");
                    }
                    *invocation
                } else {
                    if receipts.len() >= state.max_calls {
                        return error(id, -32000, "Sift turn tool budget reached");
                    }
                    let invocation = Uuid::new_v4();
                    receipts.insert(key, (input, invocation));
                    invocation
                }
            };
            let mut stopping = state.stopping.subscribe();
            if *stopping.borrow() {
                return error(id, -32000, "Sift turn stopped");
            }
            let outcome = tokio::select! {
                _=stopping.changed()=>return error(id,-32000,"Sift turn stopped"),
                result=crate::ai_tools::invoke(&state.client,&state.lease,&state.context,name,arguments,&state.events,invocation_id)=>result,
            };
            match outcome {
                Ok(text) => json!({"content":[{"type":"text","text":text}],"isError":false}),
                Err(_) => {
                    json!({"content":[{"type":"text","text":"Sift tool failed or authorization changed. Review the current context and permissions."}],"isError":true})
                }
            }
        }
        _ => return error(id, -32601, "MCP operation is not available in Sift"),
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}
