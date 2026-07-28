use super::*;

pub(super) async fn upsert_live_transcript_projection(
    db: &RegisteredGlobalDb,
    project_root: Option<&Path>,
    provider: &str,
    session_id: &str,
    messages: &[Value],
) -> Result<()> {
    let project = project_root.map_or_else(
        || "user".to_string(),
        |root| root.to_string_lossy().to_string(),
    );
    let storage_scope = if project_root.is_some() {
        "project"
    } else {
        "user"
    };
    let source_path = format!("live://{provider}/{session_id}");
    let mut projected = Vec::new();
    for (ordinal, message) in messages.iter().enumerate() {
        let Some(message_id) = message
            .get("id")
            .or_else(|| message.get("message_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let content = message.get("content").cloned().unwrap_or(Value::Null);
        let tool_calls = message.get("tool_calls");
        let (text, tool_names) = content_storage_text_and_tools(&content, tool_calls);
        if text.trim().is_empty() {
            continue;
        }
        let mut metadata = json!({
            "source": "lcm_preflight_live",
            "project_root": project,
            "storage_scope": storage_scope,
            "location_provenance": "host_live_route"
        });
        if let Some(roots) = message
            .get("associated_project_roots")
            .filter(|value| value.is_array())
        {
            metadata["associated_project_roots"] = roots.clone();
        }
        projected.push(SessionMessageRecord {
            provider: provider.to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role,
            timestamp: message
                .get("timestamp")
                .and_then(Value::as_f64)
                .map(|value| value as i64),
            ordinal: ordinal as i64,
            text,
            kind: Some("message".to_string()),
            model: message
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
            source_path: Some(source_path.clone()),
            source_offset: Some(ordinal as i64),
            metadata_json: Some(metadata.to_string()),
        });
    }
    if projected.is_empty() {
        return Ok(());
    }
    let title = projected
        .iter()
        .find(|message| message.role == "user")
        .map(|message| preview_title(&message.text));
    let batch = TranscriptBatch {
        session: SessionRecord {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            project_key: project.clone(),
            project_path: project,
            title,
            started_at: projected
                .iter()
                .filter_map(|message| message.timestamp)
                .min(),
            ended_at: None,
            transcript_path: Some(source_path.clone()),
            metadata_json: Some(
                json!({
                    "source": "lcm_preflight_live",
                    "storage_scope": storage_scope,
                    "location_provenance": "host_live_route"
                })
                .to_string(),
            ),
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        },
        messages: projected,
    };
    db.upsert_transcript_projection_batches(&[batch], &source_path, ParseOffset::default())
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "persist live transcript projection".to_string(),
            message: error.clone(),
        })
}
