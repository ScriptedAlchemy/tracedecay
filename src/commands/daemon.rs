pub(crate) async fn daemon_tool_json(
    project_path: Option<&std::path::Path>,
    tool_name: &str,
    arguments: serde_json::Value,
) -> tracedecay::errors::Result<serde_json::Value> {
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        project_path.map(std::path::Path::to_path_buf),
        None,
        false,
        false,
    )?;
    let result = tracedecay::daemon::call_default_tool(&handshake, tool_name, arguments).await?;
    let blocks = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no content blocks"),
        })?;
    let payload = parse_daemon_tool_json_content(tool_name, blocks)?;
    let Some(handle) = truncated_response_handle(&payload, tool_name)? else {
        return Ok(payload);
    };
    let retrieved = tracedecay::daemon::call_default_tool(
        &handshake,
        "tracedecay_retrieve",
        serde_json::json!({ "handle": handle, "format": "json" }),
    )
    .await?;
    let retrieved_blocks = retrieved
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon retrieval for {tool_name} returned no content blocks"),
        })?;
    let retrieved = parse_daemon_tool_json_content("tracedecay_retrieve", retrieved_blocks)?;
    parse_retrieved_tool_json(&retrieved, tool_name)
}

fn parse_daemon_tool_json_content(
    tool_name: &str,
    blocks: &[serde_json::Value],
) -> tracedecay::errors::Result<serde_json::Value> {
    tracedecay::daemon::tool_json_payload(&serde_json::json!({ "content": blocks }), tool_name)
}

fn truncated_response_handle<'a>(
    payload: &'a serde_json::Value,
    tool_name: &str,
) -> tracedecay::errors::Result<Option<&'a str>> {
    if payload
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Ok(None);
    }
    payload
        .get("handle")
        .and_then(serde_json::Value::as_str)
        .map(Some)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "daemon tool {tool_name} returned truncated JSON without a retrieval handle"
            ),
        })
}

fn parse_retrieved_tool_json(
    retrieved: &serde_json::Value,
    tool_name: &str,
) -> tracedecay::errors::Result<serde_json::Value> {
    let content = retrieved
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon retrieval for {tool_name} omitted response content"),
        })?;
    serde_json::from_str(content).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_daemon_tool_json_content, parse_retrieved_tool_json, truncated_response_handle,
    };
    use serde_json::json;

    #[test]
    fn accepts_exactly_one_json_payload() {
        let blocks = vec![json!({"text": "status"}), json!({"text": "{\"ok\":true}"})];

        assert_eq!(
            parse_daemon_tool_json_content("test", &blocks).unwrap(),
            json!({"ok": true})
        );
    }

    #[test]
    fn rejects_multiple_json_payloads() {
        let blocks = vec![json!({"text": "{\"first\":1}"}), json!({"text": "[2]"})];

        let error = parse_daemon_tool_json_content("test", &blocks).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("daemon tool test returned multiple JSON payloads")
        );
    }

    #[test]
    fn rejects_missing_json_payload() {
        let blocks = vec![json!({"text": "status"}), json!({"type": "image"})];

        let error = parse_daemon_tool_json_content("test", &blocks).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("daemon tool test returned no JSON payload")
        );
    }

    #[test]
    fn truncated_response_requires_a_retrieval_handle() {
        let envelope = json!({
            "truncated": true,
            "handle": "rh_status",
            "preview": "{}",
        });

        assert_eq!(
            truncated_response_handle(&envelope, "tracedecay_status").unwrap(),
            Some("rh_status")
        );
        assert!(
            truncated_response_handle(
                &json!({"truncated": true, "preview": "{}"}),
                "tracedecay_status"
            )
            .unwrap_err()
            .to_string()
            .contains("without a retrieval handle")
        );
    }

    #[test]
    fn retrieved_content_restores_the_original_json_payload() {
        let retrieved = json!({
            "content": "{\"node_count\":42,\"edge_count\":7}"
        });

        assert_eq!(
            parse_retrieved_tool_json(&retrieved, "tracedecay_status").unwrap(),
            json!({"node_count": 42, "edge_count": 7})
        );
    }
}
