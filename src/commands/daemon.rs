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
    parse_daemon_tool_json_content(tool_name, blocks)
}

fn parse_daemon_tool_json_content(
    tool_name: &str,
    blocks: &[serde_json::Value],
) -> tracedecay::errors::Result<serde_json::Value> {
    tracedecay::daemon::tool_json_payload(&serde_json::json!({ "content": blocks }), tool_name)
}

#[cfg(test)]
mod tests {
    use super::parse_daemon_tool_json_content;
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
}
