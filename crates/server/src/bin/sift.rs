use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde_json::{json, Value};
use sift_client_sdk::Client;
use sift_protocol::{InvokeToolRequest, InvokeToolResponse, ToolContext};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_MCP_MESSAGE_BYTES: usize = 1024 * 1024;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) != Some("mcp") {
        bail!("usage: sift mcp --server <url> --token-file <path> [context options]");
    }
    let options = McpOptions::parse(&arguments[1..])?;
    let token = read_token(&options.token_file)?;
    let client = Client::new(options.server).with_bearer_token(token);
    serve_mcp(client, options.context).await
}

struct McpOptions {
    server: String,
    token_file: PathBuf,
    context: ToolContext,
}

impl McpOptions {
    fn parse(arguments: &[String]) -> anyhow::Result<Self> {
        let mut server = None;
        let mut token_file = None;
        let mut context = ToolContext {
            tenant_id: None,
            room_id: None,
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        let mut index = 0;
        while index < arguments.len() {
            let option = arguments[index].as_str();
            let value = arguments
                .get(index + 1)
                .with_context(|| format!("{option} requires a value"))?;
            match option {
                "--server" => server = Some(value.clone()),
                "--token-file" => token_file = Some(PathBuf::from(value)),
                "--tenant-id" => context.tenant_id = Some(value.parse().context("invalid tenant")?),
                "--room-id" => context.room_id = Some(value.parse().context("invalid room")?),
                "--profile-id" => {
                    context.profile_id = Some(value.parse().context("invalid profile")?)
                }
                "--connection-id" => context.connection_id = Some(value.clone()),
                "--document-id" => context.document_id = Some(value.clone()),
                unknown => bail!("unknown mcp option `{unknown}`"),
            }
            index += 2;
        }
        let server = server.context("--server is required")?;
        if !(server.starts_with("http://") || server.starts_with("https://")) {
            bail!("--server must be an explicit http:// or https:// URL");
        }
        Ok(Self {
            server,
            token_file: token_file.context("--token-file is required")?,
            context,
        })
    }
}

fn read_token(path: &Path) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("reading token-file metadata: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("token file is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("token file must not be accessible by group or other users");
        }
    }
    let token = std::fs::read_to_string(path)
        .with_context(|| format!("reading token file: {}", path.display()))?;
    let token = token.trim().to_owned();
    if token.is_empty() || token.contains(['\r', '\n']) {
        bail!("token file must contain exactly one non-empty token");
    }
    Ok(token)
}

async fn serve_mcp(client: Client, context: ToolContext) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut input = BufReader::new(stdin);
    let mut output = tokio::io::stdout();
    let mut initialized = false;
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let read = input
            .read_until(b'\n', &mut buffer)
            .await
            .context("reading MCP request")?;
        if read == 0 {
            return Ok(());
        }
        if buffer.len() > MAX_MCP_MESSAGE_BYTES {
            write_response(
                &mut output,
                &rpc_error(Value::Null, -32600, "MCP request exceeds the byte limit"),
            )
            .await?;
            continue;
        }
        while matches!(buffer.last(), Some(b'\n' | b'\r')) {
            buffer.pop();
        }
        let request: Value = match serde_json::from_slice(&buffer) {
            Ok(request) => request,
            Err(_) => {
                write_response(&mut output, &rpc_error(Value::Null, -32700, "Parse error")).await?;
                continue;
            }
        };
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            write_response(
                &mut output,
                &rpc_error(request_id(&request), -32600, "Invalid Request"),
            )
            .await?;
            continue;
        };
        let id = request_id(&request);
        if request.get("id").is_none() {
            if method == "notifications/initialized" {
                initialized = true;
            }
            continue;
        }
        let response = match method {
            "initialize" => {
                initialized = true;
                rpc_result(
                    id,
                    json!({
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {
                            "name": "sift",
                            "version": sift_server::VERSION
                        }
                    }),
                )
            }
            "ping" if initialized => rpc_result(id, json!({})),
            "tools/list" if initialized => match client.governed_tools(&context, true).await {
                Ok(tools) => rpc_result(
                    id,
                    json!({
                        "tools": tools.into_iter().map(|tool| json!({
                            "name": tool.id,
                            "title": tool.title,
                            "description": tool.description,
                            "inputSchema": tool.input_schema,
                            "outputSchema": tool.output_schema,
                            "execution": {"taskSupport": "forbidden"}
                        })).collect::<Vec<_>>()
                    }),
                ),
                Err(_) => rpc_error(id, -32603, "Unable to list authorized Sift tools"),
            },
            "tools/call" if initialized => {
                call_tool(&client, &context, id, request.get("params")).await
            }
            _ if !initialized => rpc_error(id, -32002, "Server is not initialized"),
            _ => rpc_error(id, -32601, "Method not found"),
        };
        write_response(&mut output, &response).await?;
    }
}

async fn call_tool(
    client: &Client,
    context: &ToolContext,
    id: Value,
    params: Option<&Value>,
) -> Value {
    let Some(params) = params.and_then(Value::as_object) else {
        return rpc_error(id, -32602, "Invalid tools/call parameters");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "Tool name is required");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return rpc_error(id, -32602, "Tool arguments must be an object");
    }
    let approval_id = params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("sift/approvalId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    match client
        .invoke_tool(&InvokeToolRequest {
            tool_id: name.into(),
            arguments,
            context: context.clone(),
            approval_id,
        })
        .await
    {
        Ok(InvokeToolResponse::Completed { result }) => rpc_result(
            id,
            json!({
                "content": [{"type": "text", "text": result.to_string()}],
                "structuredContent": result,
                "isError": false
            }),
        ),
        Ok(InvokeToolResponse::ApprovalRequired { approval }) => rpc_result(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": "Approval is required through the authenticated Sift client."
                }],
                "structuredContent": {
                    "status": "approval_required",
                    "approval": approval
                },
                "isError": true
            }),
        ),
        Err(_) => rpc_result(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": "Sift rejected or failed the governed tool operation."
                }],
                "isError": true
            }),
        ),
    }
}

fn request_id(request: &Value) -> Value {
    request.get("id").cloned().unwrap_or(Value::Null)
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

async fn write_response(output: &mut tokio::io::Stdout, response: &Value) -> anyhow::Result<()> {
    let mut encoded = serde_json::to_vec(response)?;
    encoded.push(b'\n');
    output.write_all(&encoded).await?;
    output.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_explicit_server_and_token_file() {
        assert!(McpOptions::parse(&[
            "--server".into(),
            "http://127.0.0.1:3000".into(),
            "--token-file".into(),
            "/tmp/token".into(),
        ])
        .is_ok());
        assert!(McpOptions::parse(&["--server".into(), "localhost".into()]).is_err());
    }
}
