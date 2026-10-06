//! Provider-independent Sift-only reads and human-reviewed staging tools.
use serde_json::{json, Value};
use sift_client_sdk::Client;
use sift_protocol::{
    AiEventKind, AiMode, AiRunLease, AiToolKind, AiTurnContext, AppendAiEventRequest,
    InvokeAiToolRequest, StageAiQueryProposalRequest,
};
use sift_workspace_ui::ExecutorEvent;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

pub(crate) async fn invoke(
    client: &Client,
    lease: &AiRunLease,
    context: &AiTurnContext,
    name: &str,
    arguments: Value,
    events: &UnboundedSender<ExecutorEvent>,
    invocation_id: Uuid,
) -> Result<String, String> {
    let sql = arguments
        .get("sql")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let _ = events.send(ExecutorEvent::AiToolActivity(format!("{name} running")));
    if name == "sift_stage_database" {
        if lease.run.mode != AiMode::Propose {
            return Err("Propose mode required".into());
        }
        let draft = serde_json::from_value(
            arguments
                .get("draft")
                .cloned()
                .ok_or("Typed draft is required")?,
        )
        .map_err(|_| "Typed database draft is invalid".to_owned())?;
        let detail = client
            .stage_ai_database_proposal(
                lease.run.id,
                &sift_protocol::StageAiDatabaseProposalRequest {
                    client_request_id: invocation_id,
                    lease_token: lease.lease_token,
                    draft,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let _ = events.send(ExecutorEvent::AiToolActivity(
            "Database changes staged for human review".into(),
        ));
        return Ok(format!("Staged database proposal {}. A person must preview and explicitly apply it on their authorized connection.",detail.proposal.id));
    }
    if name == "sift_stage_sql" {
        if lease.run.mode != AiMode::Propose {
            return Err("Propose mode required".into());
        }
        let proposed_sql = sql.ok_or("SQL draft is required")?;
        let base_revision = context
            .sql
            .as_ref()
            .and_then(|sql| sql.document_revision)
            .ok_or("Current SQL revision is unavailable")?;
        let detail = client
            .stage_ai_query_proposal(
                lease.run.id,
                &StageAiQueryProposalRequest {
                    client_request_id: invocation_id,
                    lease_token: lease.lease_token,
                    target: context.target.clone(),
                    base_revision,
                    proposed_sql,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let _ = events.send(ExecutorEvent::AiToolActivity(
            "SQL draft staged for review".into(),
        ));
        return Ok(format!(
            "Staged proposal {}. A person must review and apply it.",
            detail.proposal.id
        ));
    }
    let tool = match name {
        "sift_schema" => AiToolKind::Schema,
        "sift_catalog" => AiToolKind::Catalog,
        "sift_diagnostics" => AiToolKind::Diagnostics,
        "sift_explain" => AiToolKind::Explain,
        "sift_select" => AiToolKind::Select,
        _ => return Err("Tool is not available in Sift".into()),
    };
    let response = client
        .invoke_ai_tool(
            lease.run.id,
            &InvokeAiToolRequest {
                call_id: invocation_id,
                lease_token: lease.lease_token,
                tool,
                sql,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let _ = events.send(ExecutorEvent::AiToolActivity(format!("{name} completed")));
    serde_json::to_string(&response.result).map_err(|error| error.to_string())
}

pub(crate) async fn append_text(
    client: &Client,
    lease: &AiRunLease,
    kind: AiEventKind,
    text: &str,
) -> Result<(), String> {
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + 12 * 1024).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        client
            .append_ai_event(
                lease.run.id,
                &AppendAiEventRequest {
                    client_event_id: Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    kind,
                    content: json!({"text":&text[start..end]}),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        start = end;
    }
    Ok(())
}

pub(crate) fn database_draft_tool() -> Result<Value, String> {
    let mut schema = serde_json::to_value(schemars::schema_for!(sift_protocol::AiDatabaseDraft))
        .map_err(|_| "Cannot describe typed database drafts")?;
    let definitions = schema
        .as_object_mut()
        .ok_or("Invalid typed draft schema")?
        .remove("definitions")
        .unwrap_or_else(|| json!({}));
    schema
        .as_object_mut()
        .expect("schema object")
        .remove("$schema");
    Ok(
        json!({"type":"function","deferLoading":false,"name":"sift_stage_database","description":"Stage a bounded typed row edit set or desired schema catalog for human preview. Use sift_catalog first, preserve its database identity/provider, and supply its expected catalog revision. Never applies changes.","inputSchema":{"type":"object","properties":{"draft":schema},"required":["draft"],"additionalProperties":false,"definitions":definitions}}),
    )
}

pub(crate) fn tool(name: &str, description: &str, has_sql: bool) -> Value {
    let schema = if has_sql {
        json!({"type":"object","properties":{"sql":{"type":"string"}},"required":["sql"]})
    } else {
        json!({"type":"object","properties":{}})
    };
    json!({"type":"function","deferLoading":false,"name":name,"description":description,"inputSchema":schema})
}

pub(crate) fn tools(mode: AiMode) -> Result<Vec<Value>, String> {
    let mut tools = vec![
        tool("sift_catalog", "Read the bounded typed catalog and current revision before proposing row or schema changes", false),
        tool("sift_schema", "Read the shallow schema of the current Sift connection", false),
        tool("sift_diagnostics", "Check SQL syntax in the current Sift dialect", true),
        tool("sift_explain", "Get an estimated plan for one SELECT; never ANALYZE", true),
        tool("sift_select", "Run one bounded Sift-restricted SELECT (up to 100 rows); SELECT functions may have side effects", true),
    ];
    if mode == AiMode::Propose {
        tools.push(database_draft_tool()?);
        tools.push(tool(
            "sift_stage_sql",
            "Stage a complete replacement SQL draft for human review; does not apply it",
            true,
        ));
    }
    Ok(tools)
}
