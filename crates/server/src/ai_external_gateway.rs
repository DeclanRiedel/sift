//! Remote inventory and reads. Caller supplies independently authorized pins.
use crate::ai_external_schema::ToolSchema;
use crate::ai_external_transport::{Revision, Session};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sift_protocol::{
    AiExternalSourceDefinition, AiExternalToolDefinition, AiExternalToolPolicy, AiMcpRevision,
};
use std::collections::HashSet;

fn revision(revision: AiMcpRevision) -> Revision {
    match revision {
        AiMcpRevision::Modern20260728 => Revision::Modern,
        AiMcpRevision::Legacy20251125 => Revision::Legacy,
    }
}

fn tool_definition(raw: &Value) -> Result<AiExternalToolDefinition, String> {
    let name = raw
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control))
        .ok_or("External tool name is invalid")?;
    let description = raw
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let title = raw.get("title").and_then(Value::as_str).map(str::to_owned);
    if description.len() > 2048 || title.as_ref().is_some_and(|title| title.len() > 256) {
        return Err("External tool description exceeds its limit".into());
    }
    let input_schema = raw
        .get("inputSchema")
        .ok_or("External tool has no input schema")?;
    ToolSchema::compile(input_schema, true)?;
    let output_schema = raw.get("outputSchema");
    if let Some(schema) = output_schema {
        ToolSchema::compile(schema, false)?;
    }
    let annotations = raw.get("annotations");
    if annotations.is_some_and(|value| !value.is_object()) {
        return Err("External tool annotations must be an object".into());
    }
    let bytes = serde_json::to_vec(raw).map_err(|_| "External tool cannot be encoded")?;
    Ok(AiExternalToolDefinition {
        name: name.into(),
        alias: format!("tool_{:x}", Sha256::digest(name.as_bytes()))[..37].into(),
        title,
        description: description.into(),
        input_schema: input_schema.clone(),
        output_schema: output_schema.cloned(),
        annotations: annotations.cloned(),
        schema_sha256: format!("{:x}", Sha256::digest(bytes)),
        policy: AiExternalToolPolicy::Unavailable,
    })
}

async fn inventory(session: &mut Session) -> Result<Vec<AiExternalToolDefinition>, String> {
    let mut cursor = None;
    let mut cursors = HashSet::new();
    let mut names = HashSet::new();
    let mut tools = Vec::new();
    for _ in 0..4 {
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
        let result = session.rpc("tools/list", params, &[], 256 * 1024).await?;
        let remote = result
            .get("tools")
            .and_then(Value::as_array)
            .filter(|tools| tools.len() <= 64)
            .ok_or("External inventory is invalid or exceeds its page limit")?;
        for raw in remote {
            // Invalid HTTP annotations exclude this tool as the MCP spec requires.
            let Ok(tool) = tool_definition(raw) else {
                continue;
            };
            if !names.insert(tool.name.clone()) || tools.len() >= 32 {
                return Err("External inventory has duplicate tools or exceeds its limit".into());
            }
            tools.push(tool);
        }
        match result.get("nextCursor") {
            None | Some(Value::Null) => return Ok(tools),
            Some(Value::String(next))
                if !next.is_empty() && next.len() <= 4096 && cursors.insert(next.clone()) =>
            {
                cursor = Some(next.clone());
            }
            _ => return Err("External inventory cursor is invalid or repeated".into()),
        }
    }
    Err("External inventory exceeds its page limit".into())
}

pub(crate) async fn discover(
    label: String,
    endpoint: String,
    protocol: AiMcpRevision,
    token: Option<String>,
) -> Result<AiExternalSourceDefinition, String> {
    if label.trim().is_empty() || label.len() > 120 || label.chars().any(char::is_control) {
        return Err("External source label is invalid".into());
    }
    let mut session = Session::new(&endpoint, revision(protocol), token)?;
    session.initialize().await?;
    let tools = inventory(&mut session).await?;
    if tools.is_empty() {
        return Err("External source has no supported tools to review".into());
    }
    Ok(AiExternalSourceDefinition {
        label,
        endpoint,
        protocol,
        tools,
    })
}

pub(crate) async fn read(
    definition: &AiExternalSourceDefinition,
    token: Option<String>,
    alias: &str,
    arguments: Value,
) -> Result<Value, String> {
    let reviewed = definition
        .tools
        .iter()
        .find(|tool| tool.alias == alias)
        .ok_or("External tool was not registered")?;
    if reviewed.policy != AiExternalToolPolicy::Read {
        return Err("External tool is unavailable for remote invocation; use its reviewed local proposal adapter".into());
    }
    let schema = ToolSchema::compile(&reviewed.input_schema, true)?;
    let headers = schema.parameter_headers(&arguments)?;
    let mut session = Session::new(&definition.endpoint, revision(definition.protocol), token)?;
    session.initialize().await?;
    let fresh = inventory(&mut session).await?;
    if !fresh
        .iter()
        .any(|tool| tool.name == reviewed.name && tool.schema_sha256 == reviewed.schema_sha256)
    {
        return Err("External tool schema or metadata changed; review the source again".into());
    }
    let result = session
        .rpc(
            "tools/call",
            json!({"name":reviewed.name,"arguments":arguments}),
            &headers,
            8192,
        )
        .await?;
    if result
        .get("isError")
        .is_some_and(|error| error != &Value::Bool(false))
    {
        return Err("External source reported a tool error".into());
    }
    if let Some(output) = &reviewed.output_schema {
        let structured = result
            .get("structuredContent")
            .ok_or("External source omitted its reviewed structured result")?;
        ToolSchema::compile(output, false)?.validate(structured)?;
    }
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or("External tool result has no content")?;
    let text = content
        .iter()
        .map(|item| {
            if item.get("type").and_then(Value::as_str) != Some("text") {
                return Err("External source returned unsupported non-text content");
            }
            item.get("text")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or("External text content is invalid")
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(
        json!({"source":definition.label,"tool":reviewed.alias,"text":text,"structured_content":result.get("structuredContent")}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn discovery_read_and_schema_change_never_invoke_remote_writes() {
        use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
        #[derive(Clone, Default)]
        struct Fixture {
            changed: Arc<AtomicBool>,
            calls: Arc<AtomicUsize>,
        }
        async fn rpc(
            State(fixture): State<Fixture>,
            headers: HeaderMap,
            Json(request): Json<Value>,
        ) -> Json<Value> {
            let method = request["method"].as_str().unwrap();
            assert_eq!(headers.get("Mcp-Method").unwrap(), method);
            let result = if method == "tools/list" {
                json!({"resultType":"complete","tools":[{
                    "name":"lookup", "description":if fixture.changed.load(Ordering::SeqCst) { "Changed" } else { "Reviewed" },
                    "inputSchema":{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}},"required":["region"]},
                    "outputSchema":{"type":"object","properties":{"answer":{"type":"integer"}},"required":["answer"]},
                    "annotations":{"readOnlyHint":true}
                }]})
            } else {
                assert_eq!(method, "tools/call");
                assert_eq!(request["params"]["name"], "lookup");
                assert_eq!(headers.get("Mcp-Param-Region").unwrap(), "west");
                fixture.calls.fetch_add(1, Ordering::SeqCst);
                json!({"resultType":"complete","content":[{"type":"text","text":"Found"}],"structuredContent":{"answer":42}})
            };
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
        }
        let fixture = Fixture::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/mcp", post(rpc))
            .with_state(fixture.clone());
        let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        let mut source = discover(
            "Fixture".into(),
            endpoint,
            AiMcpRevision::Modern20260728,
            None,
        )
        .await
        .unwrap();
        let alias = source.tools[0].alias.clone();
        let arguments = json!({"region":"west"});
        assert!(read(&source, None, &alias, arguments.clone())
            .await
            .is_err());
        source.tools[0].policy = AiExternalToolPolicy::LocalRowDraft;
        assert!(read(&source, None, &alias, arguments.clone())
            .await
            .is_err());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        source.tools[0].policy = AiExternalToolPolicy::Read;
        let result = read(&source, None, &alias, arguments.clone())
            .await
            .unwrap();
        assert_eq!(result["structured_content"]["answer"], 42);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        fixture.changed.store(true, Ordering::SeqCst);
        assert!(read(&source, None, &alias, arguments).await.is_err());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn remote_annotations_never_approve_tools_and_metadata_is_pinned() {
        let raw = json!({"name":"lookup","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}});
        let initial = tool_definition(&raw).unwrap();
        assert_eq!(initial.policy, AiExternalToolPolicy::Unavailable);
        let mut changed = raw.clone();
        changed["annotations"]["readOnlyHint"] = json!(false);
        assert_ne!(
            initial.schema_sha256,
            tool_definition(&changed).unwrap().schema_sha256
        );
        assert_eq!(initial.alias, tool_definition(&changed).unwrap().alias);
        let mut invalid = raw;
        invalid["inputSchema"] =
            json!({"properties":{"key":{"type":"number","x-mcp-header":"Key"}}});
        assert!(tool_definition(&invalid).is_err());
    }
}
