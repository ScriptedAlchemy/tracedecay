use serde_json::json;

use tracedecay::automation::backend::{
    extract_single_json_object, AgentTaskBackend, AgentTaskKind, AgentTaskRequest,
    AgentTaskResponse,
};

struct EchoBackend;

impl AgentTaskBackend for EchoBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: request.prompt.clone(),
            output_json: extract_single_json_object(&request.prompt).ok(),
            model: Some("test-model".to_string()),
            input_tokens: Some(12),
            output_tokens: Some(34),
        })
    }
}

#[test]
fn backend_contract_round_trips_structured_task_output() {
    let request = AgentTaskRequest {
        run_id: "run_001".to_string(),
        task: AgentTaskKind::MemoryCurator,
        prompt: r#"{"ops":[{"kind":"keep","id":"fact-1"}]}"#.to_string(),
        context: json!({"bank":"core"}),
    };

    let response = EchoBackend.run_task(&request).unwrap();

    assert_eq!(response.run_id, "run_001");
    assert_eq!(response.task, AgentTaskKind::MemoryCurator);
    assert_eq!(response.model.as_deref(), Some("test-model"));
    assert_eq!(response.output_json.unwrap()["ops"][0]["id"], "fact-1");
    assert_eq!(response.input_tokens, Some(12));
    assert_eq!(response.output_tokens, Some(34));
}

#[test]
fn extracts_one_plain_or_fenced_json_object() {
    assert_eq!(
        extract_single_json_object(r#" { "ok": true } "#).unwrap()["ok"],
        true
    );
    assert_eq!(
        extract_single_json_object("```json\n{\"task\":\"skill_writer\"}\n```").unwrap()["task"],
        "skill_writer"
    );
}

#[test]
fn rejects_non_object_extra_text_and_multiple_json_values() {
    for text in [
        r#"[{"ok":true}]"#,
        r#"prefix {"ok":true}"#,
        r#"{"ok":true} suffix"#,
        r#"{"ok":true} {"again":true}"#,
        "```json\n{\"ok\":true}\n```\nextra",
    ] {
        assert!(
            extract_single_json_object(text).is_err(),
            "accepted non-strict JSON output: {text}"
        );
    }
}
