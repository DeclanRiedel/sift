//! Pinned, request-bounded MCP HTTP transport. Authorization lives in the gateway.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures::StreamExt as _;
use reqwest::{header::HeaderValue, Client, Url};
use serde_json::{json, Value};
use std::time::Duration;

const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Revision {
    Modern,
    Legacy,
}

impl Revision {
    pub(crate) fn version(self) -> &'static str {
        match self {
            Self::Modern => "2026-07-28",
            Self::Legacy => "2025-11-25",
        }
    }
}

pub(crate) fn endpoint(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "MCP endpoint must be an absolute HTTP URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.port() == Some(0)
    {
        return Err(
            "MCP endpoint cannot contain credentials, query parameters or a fragment".into(),
        );
    }
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback());
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(
            "MCP endpoints require HTTPS; HTTP is limited to literal loopback addresses".into(),
        );
    }
    Ok(url)
}

pub(crate) fn encoded_header(value: &str) -> String {
    if value.trim() != value
        || !value
            .bytes()
            .all(|byte| matches!(byte, b'\t' | 0x20..=0x7e))
        || (value.starts_with("=?base64?") && value.ends_with("?="))
    {
        format!("=?base64?{}?=", STANDARD.encode(value.as_bytes()))
    } else {
        value.into()
    }
}

/// No cookies, proxies, redirect following, ambient authentication or native state.
/// This type deliberately has no Debug implementation.
pub(crate) struct Session {
    http: Client,
    url: Url,
    revision: Revision,
    authorization: Option<HeaderValue>,
    token: Option<String>,
    session: Option<HeaderValue>,
}

impl Session {
    pub(crate) fn new(
        raw_url: &str,
        revision: Revision,
        token: Option<String>,
    ) -> Result<Self, String> {
        let url = endpoint(raw_url)?;
        let authorization = token
            .as_ref()
            .map(|token| {
                if !(16..=8192).contains(&token.len())
                    || token.bytes().any(|byte| {
                        !byte.is_ascii_alphanumeric()
                            && !matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
                    })
                {
                    return Err("MCP bearer credential is invalid".to_owned());
                }
                let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| "MCP bearer credential is invalid".to_owned())?;
                value.set_sensitive(true);
                Ok(value)
            })
            .transpose()?;
        let http = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| "MCP transport is unavailable".to_owned())?;
        Ok(Self {
            http,
            url,
            revision,
            authorization,
            token,
            session: None,
        })
    }

    fn request(&self, method: reqwest::Method) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .request(method, self.url.clone())
            .header("MCP-Protocol-Version", self.revision.version());
        if let Some(value) = &self.authorization {
            request = request.header(reqwest::header::AUTHORIZATION, value.clone());
        }
        if let Some(value) = &self.session {
            request = request.header("Mcp-Session-Id", value.clone());
        }
        request
    }

    pub(crate) async fn initialize(&mut self) -> Result<(), String> {
        if self.revision == Revision::Modern {
            return Ok(());
        }
        let initialized = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion":self.revision.version(), "capabilities":{},
                    "clientInfo":{"name":"sift","version":env!("CARGO_PKG_VERSION")}
                }),
                &[],
                MAX_BYTES,
            )
            .await?;
        if initialized.get("protocolVersion").and_then(Value::as_str)
            != Some(self.revision.version())
        {
            return Err("MCP protocol changed; review the pinned source version".into());
        }
        let response = self
            .request(reqwest::Method::POST)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .json(&json!({"jsonrpc":"2.0", "method":"notifications/initialized", "params":{}}))
            .send()
            .await
            .map_err(|_| "MCP initialization notification failed".to_owned())?;
        if response.status() != reqwest::StatusCode::ACCEPTED {
            return Err("MCP initialization notification was rejected".into());
        }
        Ok(())
    }

    pub(crate) async fn rpc(
        &mut self,
        method: &str,
        mut params: Value,
        parameter_headers: &[(String, String)],
        max_bytes: usize,
    ) -> Result<Value, String> {
        if !(1..=MAX_BYTES).contains(&max_bytes) {
            return Err("MCP response limit is invalid".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let object = params
            .as_object_mut()
            .ok_or("MCP request parameters must be an object")?;
        let mut request = self.request(reqwest::Method::POST).header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        );
        if self.revision == Revision::Modern {
            object.insert("_meta".into(), json!({
                "io.modelcontextprotocol/protocolVersion":self.revision.version(),
                "io.modelcontextprotocol/clientInfo":{"name":"sift", "version":env!("CARGO_PKG_VERSION")},
                "io.modelcontextprotocol/clientCapabilities":{}, "progressToken":id
            }));
            request = request.header("Mcp-Method", method);
            if let Some(name) = object.get("name").and_then(Value::as_str) {
                request = request.header("Mcp-Name", encoded_header(name));
            }
            for (name, value) in parameter_headers {
                let name =
                    reqwest::header::HeaderName::from_bytes(format!("Mcp-Param-{name}").as_bytes())
                        .map_err(|_| "MCP parameter header is invalid".to_owned())?;
                let mut value = HeaderValue::from_str(&encoded_header(value))
                    .map_err(|_| "MCP parameter header is invalid".to_owned())?;
                value.set_sensitive(true);
                request = request.header(name, value);
            }
        }
        let body =
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .map_err(|_| "MCP request cannot be encoded".to_owned())?;
        if body.len() > MAX_BYTES {
            return Err("MCP request exceeds its byte limit".into());
        }
        let response = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| "MCP endpoint did not respond".to_owned())?;
        if response.status().is_redirection() {
            return Err("MCP redirects are not allowed".into());
        }
        if self.revision == Revision::Legacy && method == "initialize" {
            if let Some(session) = response.headers().get("Mcp-Session-Id") {
                if session.is_empty()
                    || session.as_bytes().len() > 128
                    || !session
                        .as_bytes()
                        .iter()
                        .all(|byte| matches!(byte, 0x21..=0x7e))
                {
                    return Err("MCP session identity is invalid".into());
                }
                let mut session = session.clone();
                session.set_sensitive(true);
                self.session = Some(session);
            }
        }
        let status = response.status();
        let value = read_response(response, &id, max_bytes).await?;
        let result = value.get("result").ok_or_else(|| {
            if value.pointer("/error/code").and_then(Value::as_i64) == Some(-32022) {
                "MCP protocol changed; review the pinned source version".to_owned()
            } else {
                "MCP request was rejected by the source".to_owned()
            }
        })?;
        if !status.is_success() {
            return Err("MCP request was rejected by the source".into());
        }
        if self.revision == Revision::Modern
            && result.get("resultType").and_then(Value::as_str) != Some("complete")
        {
            return Err(
                "MCP source requested unsupported client input or returned an incomplete result"
                    .into(),
            );
        }
        if let Some(token) = &self.token {
            if contains_credential(result, token) {
                return Err(
                    "MCP source returned authentication material; response withheld".into(),
                );
            }
        }
        Ok(result.clone())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let http = self.http.clone();
        let url = self.url.clone();
        let authorization = self.authorization.clone();
        let version = self.revision.version();
        runtime.spawn(async move {
            let mut request = http
                .delete(url)
                .header("Mcp-Session-Id", session)
                .header("MCP-Protocol-Version", version);
            if let Some(value) = authorization {
                request = request.header(reqwest::header::AUTHORIZATION, value);
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), request.send()).await;
        });
    }
}

fn contains_credential(value: &Value, token: &str) -> bool {
    match value {
        Value::String(text) => {
            text.contains(token) || text.contains(&STANDARD.encode(token.as_bytes()))
        }
        Value::Array(values) => values.iter().any(|value| contains_credential(value, token)),
        Value::Object(values) => values.iter().any(|(key, value)| {
            key.contains(token)
                || key.contains(&STANDARD.encode(token.as_bytes()))
                || contains_credential(value, token)
        }),
        _ => false,
    }
}

fn envelope(value: Value, id: &str) -> Result<Option<Value>, String> {
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("MCP response envelope is invalid".into());
    }
    if value.get("id").is_none() {
        if matches!(
            value.get("method").and_then(Value::as_str),
            Some("notifications/progress" | "notifications/message")
        ) {
            return Ok(None);
        }
        return Err("MCP source sent an unsupported server message".into());
    }
    if value.get("method").is_some() {
        return Err("MCP server requests are not supported".into());
    }
    if value.get("id").and_then(Value::as_str) != Some(id)
        || value.get("result").is_some() == value.get("error").is_some()
    {
        return Err("MCP response identity or result is invalid".into());
    }
    Ok(Some(value))
}

async fn read_response(
    response: reqwest::Response,
    id: &str,
    max_bytes: usize,
) -> Result<Value, String> {
    if response
        .content_length()
        .is_some_and(|bytes| bytes > max_bytes as u64)
    {
        return Err("MCP response exceeds its byte limit".into());
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let sse = content_type == "text/event-stream";
    if !sse && content_type != "application/json" {
        return Err("MCP response type is unsupported".into());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut line = Vec::new();
    let mut event = Vec::new();
    let mut received = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "MCP response stream ended unexpectedly".to_owned())?;
        received = received.saturating_add(chunk.len());
        if received > max_bytes {
            return Err("MCP response exceeds its byte limit".into());
        }
        if !sse {
            bytes.extend_from_slice(&chunk);
            continue;
        }
        for byte in chunk {
            if byte != b'\n' {
                line.push(byte);
                continue;
            }
            let text = std::str::from_utf8(&line)
                .map_err(|_| "MCP event is invalid UTF-8".to_owned())?
                .trim_end_matches('\r');
            if text.is_empty() {
                if !event.is_empty() {
                    let value: Value = serde_json::from_slice(&event)
                        .map_err(|_| "MCP event is invalid JSON".to_owned())?;
                    event.clear();
                    if let Some(value) = envelope(value, id)? {
                        return Ok(value);
                    }
                }
            } else if let Some(data) = text.strip_prefix("data:") {
                if !event.is_empty() {
                    event.push(b'\n');
                }
                event.extend_from_slice(data.strip_prefix(' ').unwrap_or(data).as_bytes());
            }
            line.clear();
        }
    }
    if sse {
        return Err("MCP stream ended without a final response".into());
    }
    let value =
        serde_json::from_slice(&bytes).map_err(|_| "MCP response is invalid JSON".to_owned())?;
    envelope(value, id)?.ok_or_else(|| "MCP JSON response was not a final response".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};

    const TOKEN: &str = "fixture-bearer-credential-12345";

    #[derive(Clone, Default)]
    struct Fixture {
        methods: Arc<Mutex<Vec<String>>>,
        pending: Arc<tokio::sync::Notify>,
        deleted: Arc<tokio::sync::Notify>,
    }

    async fn serve(
        State(fixture): State<Fixture>,
        headers: HeaderMap,
        Json(request): Json<Value>,
    ) -> Response {
        assert_eq!(
            headers.get("authorization").unwrap(),
            format!("Bearer {TOKEN}").as_str()
        );
        let method = request["method"].as_str().unwrap();
        fixture.methods.lock().unwrap().push(method.into());
        let modern = headers.get("MCP-Protocol-Version").unwrap() == "2026-07-28";
        if modern {
            assert_eq!(headers.get("Mcp-Method").unwrap(), method);
            assert_eq!(
                request
                    .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
                    .unwrap(),
                "2026-07-28"
            );
            assert_eq!(
                request
                    .pointer("/params/_meta/io.modelcontextprotocol~1clientCapabilities")
                    .unwrap(),
                &json!({})
            );
            assert!(headers.get("Mcp-Session-Id").is_none());
        }
        if method == "notifications/initialized" {
            return StatusCode::ACCEPTED.into_response();
        }
        if method == "initialize" {
            assert!(!modern);
            assert_eq!(
                request.pointer("/params/protocolVersion").unwrap(),
                "2025-11-25"
            );
            let mut response = Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{
                "protocolVersion":"2025-11-25", "capabilities":{"tools":{}}
            }}))
            .into_response();
            response
                .headers_mut()
                .insert("Mcp-Session-Id", "fixture-session".parse().unwrap());
            return response;
        }
        if !modern {
            assert_eq!(headers.get("Mcp-Session-Id").unwrap(), "fixture-session");
        }
        if method == "tools/list" {
            let mut result = json!({"tools":[{"name":"read", "inputSchema":{"type":"object"}}]});
            if modern {
                result["resultType"] = "complete".into();
            }
            return Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
                .into_response();
        }
        assert_eq!(method, "tools/call");
        let scenario = request
            .pointer("/params/arguments/scenario")
            .and_then(Value::as_str)
            .unwrap_or("json");
        if scenario == "redirect" {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [("location", "/must-not-follow")],
            )
                .into_response();
        }
        if scenario == "pending" {
            fixture.pending.notify_one();
            let stream =
                futures::stream::once(async { Ok::<_, std::convert::Infallible>(": pending\n\n") })
                    .chain(futures::stream::pending());
            return (
                [("content-type", "text/event-stream")],
                Body::from_stream(stream),
            )
                .into_response();
        }
        let mut result = json!({"content":[{"type":"text","text":"Reviewed result"}]});
        if modern {
            result["resultType"] = "complete".into();
        }
        if scenario == "credential" {
            result["content"][0]["text"] = json!(TOKEN);
        }
        if scenario == "encoded-credential" {
            result["content"][0]["text"] = json!(STANDARD.encode(TOKEN));
        }
        if scenario == "input" {
            result = json!({"resultType":"input_required", "inputRequests":{"root":{"method":"roots/list"}}});
        }
        let id = if scenario == "wrong-id" {
            json!("another-request")
        } else {
            request["id"].clone()
        };
        let response = json!({"jsonrpc":"2.0","id":id,"result":result});
        if scenario == "sse" {
            assert_eq!(
                headers.get("Mcp-Name").unwrap(),
                encoded_header("read 世界").as_str()
            );
            assert_eq!(
                headers.get("Mcp-Param-Tenant").unwrap(),
                encoded_header(" padded \n世界").as_str()
            );
            return ([("content-type", "text/event-stream")], format!(": keepalive\n\nevent: message\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{{}}}}\n\ndata: {response}\n\n")).into_response();
        }
        if scenario == "server-request" {
            return ([("content-type", "text/event-stream")], format!("data: {}\n\n", json!({"jsonrpc":"2.0","id":request["id"],"method":"sampling/createMessage","params":{}}))).into_response();
        }
        if scenario == "oversized" {
            return (
                [("content-type", "text/event-stream")],
                Body::from_stream(futures::stream::once(async {
                    Ok::<_, std::convert::Infallible>("x".repeat(8192))
                })),
            )
                .into_response();
        }
        Json(response).into_response()
    }

    async fn remove(State(fixture): State<Fixture>, headers: HeaderMap) -> StatusCode {
        assert_eq!(headers.get("Mcp-Session-Id").unwrap(), "fixture-session");
        fixture.methods.lock().unwrap().push("DELETE".into());
        fixture.deleted.notify_one();
        StatusCode::NO_CONTENT
    }

    async fn fixture() -> (String, Fixture, tokio_util::task::AbortOnDropHandle<()>) {
        let fixture = Fixture::default();
        let router = Router::new()
            .route("/registered", post(serve).delete(remove))
            .with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        (format!("http://{address}/registered"), fixture, server)
    }

    #[test]
    fn endpoint_and_header_rules_do_not_admit_ambient_auth_or_unsafe_headers() {
        for url in [
            "file:///tmp/socket",
            "http://localhost/mcp",
            "http://192.0.2.1/mcp",
            "https://user:secret@example.invalid/mcp",
            "https://example.invalid/mcp?token=value",
            "https://example.invalid/mcp#auth",
        ] {
            assert!(endpoint(url).is_err());
        }
        assert!(endpoint("http://[::1]:8123/registered").is_ok());
        for value in ["世界", " padded ", "line1\nline2", "=?base64?literal?="] {
            let encoded = encoded_header(value);
            let bytes = STANDARD
                .decode(
                    encoded
                        .strip_prefix("=?base64?")
                        .unwrap()
                        .strip_suffix("?=")
                        .unwrap(),
                )
                .unwrap();
            assert_eq!(String::from_utf8(bytes).unwrap(), value);
        }
        assert_eq!(encoded_header("us-west1"), "us-west1");
    }

    #[tokio::test]
    async fn modern_requests_support_bounded_json_sse_and_reject_unreviewed_interactions() {
        let (url, fixture, _server) = fixture().await;
        let mut session = Session::new(&url, Revision::Modern, Some(TOKEN.into())).unwrap();
        session.initialize().await.unwrap();
        let list = session
            .rpc("tools/list", json!({}), &[], MAX_BYTES)
            .await
            .unwrap();
        assert_eq!(list["tools"][0]["name"], "read");
        for scenario in ["json", "sse"] {
            let headers = if scenario == "sse" {
                vec![("Tenant".into(), " padded \n世界".into())]
            } else {
                Vec::new()
            };
            let result = session.rpc("tools/call", json!({"name":if scenario == "sse" {"read 世界"} else {"read"}, "arguments":{"scenario":scenario}}), &headers, MAX_BYTES).await.unwrap();
            assert_eq!(result["content"][0]["text"], "Reviewed result");
        }
        for scenario in [
            "wrong-id",
            "server-request",
            "input",
            "credential",
            "encoded-credential",
            "redirect",
            "oversized",
        ] {
            assert!(
                session
                    .rpc(
                        "tools/call",
                        json!({"name":"read","arguments":{"scenario":scenario}}),
                        &[],
                        if scenario == "oversized" {
                            64
                        } else {
                            MAX_BYTES
                        }
                    )
                    .await
                    .is_err(),
                "{scenario}"
            );
        }
        drop(session);
        assert!(!fixture
            .methods
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "initialize" || method == "DELETE"));
    }

    #[tokio::test]
    async fn legacy_session_is_pinned_and_cleaned_up_when_a_pending_read_is_canceled() {
        let (url, fixture, _server) = fixture().await;
        let mut session = Session::new(&url, Revision::Legacy, Some(TOKEN.into())).unwrap();
        session.initialize().await.unwrap();
        session
            .rpc("tools/list", json!({}), &[], MAX_BYTES)
            .await
            .unwrap();
        let read = tokio::spawn(async move {
            session
                .rpc(
                    "tools/call",
                    json!({"name":"read","arguments":{"scenario":"pending"}}),
                    &[],
                    MAX_BYTES,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), fixture.pending.notified())
            .await
            .unwrap();
        read.abort();
        let _ = read.await;
        tokio::time::timeout(Duration::from_secs(2), fixture.deleted.notified())
            .await
            .unwrap();
        assert_eq!(
            *fixture.methods.lock().unwrap(),
            [
                "initialize",
                "notifications/initialized",
                "tools/list",
                "tools/call",
                "DELETE"
            ]
        );
    }
}
