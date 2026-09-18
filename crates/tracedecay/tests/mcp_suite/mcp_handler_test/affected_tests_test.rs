//! `tracedecay_affected_tests` through the production MCP `tools/call` path.
//!
//! The tool does not search the graph itself. A host passes the handle minted
//! by a completed feedback cycle, and the answer is that cycle's affected-test
//! projection. These assertions use the symbol ids a host can read from
//! `tracedecay_find_exact_symbol`, not values taken from the projection.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay_mcp::jsonrpc::JsonRpcResponse;
use url::Url;

use crate::support::{
    ProductionCompositionFixture, production_composition_fixture_with_sources,
    truncated_response_handle, wait_for_current_graph,
};

const LIB: &str = r#"pub fn feedback_entry(input: u32) -> u32 {
    feedback_public_replay_missing_symbol(input)
}

#[cfg(test)]
mod tests {
    fn support_helper() {
        super::feedback_entry(0);
    }

    #[test]
    fn feedback_entry_test() {
        assert_eq!(super::feedback_entry(1), 1);
    }
}

#[test]
fn root_feedback_entry_test() {
    assert_eq!(feedback_entry(2), 2);
}

#[test]
fn unrelated_test() {
    let _ = 1 + 1;
}
"#;

#[tokio::test]
async fn affected_tests_projects_the_tests_that_call_the_edited_symbol() {
    let fixture = production_composition_fixture_with_sources(|project| {
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src/lib.rs"), LIB).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let invalid = call(
        &fixture,
        "tracedecay_affected_tests",
        json!({"request_handle": " invalid", "format": "json"}),
    )
    .await;
    assert_invalid_handle(&invalid);

    let missing_field = call(&fixture, "tracedecay_affected_tests", json!({})).await;
    assert_missing_handle_field(&missing_field);

    let inline = symbol_id(&fixture, "feedback_entry_test", "::feedback_entry_test").await;
    let root = symbol_id(
        &fixture,
        "root_feedback_entry_test",
        "::root_feedback_entry_test",
    )
    .await;
    let helper = symbol_id(&fixture, "support_helper", "::support_helper").await;
    let unrelated = symbol_id(&fixture, "unrelated_test", "::unrelated_test").await;

    let document_uri = Url::from_file_path(fixture.project_root.join("src/lib.rs"))
        .expect("advisory document URI")
        .to_string();
    let published = published_cycle(&fixture, &document_uri).await;
    let cycle = &published["cycle"];
    let handle = published["read_handles"]["affected_tests_handle"]
        .as_str()
        .unwrap_or_else(|| panic!("cycle did not mint an affected-tests handle: {published}"))
        .to_owned();

    let unknown = call(
        &fixture,
        "tracedecay_affected_tests",
        json!({"request_handle": "rh_missing-affected-tests"}),
    )
    .await;
    assert_unknown_handle_markdown(&unknown);

    let response = call(
        &fixture,
        "tracedecay_affected_tests",
        json!({"request_handle": handle, "format": "json"}),
    )
    .await;
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.expect("affected-tests result");
    assert_ne!(result["isError"], true, "{result}");
    let envelope = tool_json(&fixture, &result).await;
    assert_eq!(
        envelope["contract"]["schema_id"],
        "schema.application.feedback.affected-tests.result"
    );
    assert_eq!(envelope["contract"]["schema_revision"], 1);
    assert_eq!(envelope["outcome"]["outcome"], "evidence");
    assert_eq!(
        envelope["outcome"]["value"]["execution"]["termination"],
        "completed"
    );
    let projected = &envelope["outcome"]["value"]["payload"];
    assert_eq!(projected["result_id"], cycle["result_id"]);
    assert_eq!(projected["cycle_id"], cycle["cycle_id"]);
    assert_eq!(projected["target"], cycle["impact"]["target"]);
    assert_eq!(
        projected["evidence_anchors"],
        cycle["impact"]["evidence_anchors"]
    );
    assert_eq!(projected["state"], cycle["affected_tests_state"]);

    let mut expected = vec![Value::String(inline), Value::String(root)];
    expected.sort_by(|left, right| left.as_str().unwrap().cmp(right.as_str().unwrap()));
    assert_eq!(
        projected["affected_tests"],
        Value::Array(expected),
        "affected tests must be the indexed tests that call feedback_entry, not callers or helpers: {projected}"
    );
    assert_eq!(
        projected["affected_tests"],
        cycle["impact"]["affected_tests"]
    );
    let reported = projected["affected_tests"]
        .as_array()
        .expect("affected_tests array");
    assert!(
        !reported
            .iter()
            .any(|id| id == &json!(helper) || id == &json!(unrelated)),
        "support helpers and tests that do not call feedback_entry must stay out: {reported:?}"
    );

    fixture.harness.shutdown().await;
}

fn error_str<'a>(error: &'a tracedecay_mcp::jsonrpc::JsonRpcError, field: &str) -> &'a str {
    error
        .data
        .as_ref()
        .and_then(|data| data.get(field))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{field} missing on tool error: {error:?}"))
}

fn assert_invalid_handle(response: &JsonRpcResponse) {
    let error = response
        .error
        .as_ref()
        .expect("an untrimmed handle is a JSON-RPC error, not an empty success");
    assert_eq!(error.code, -32602);
    assert_eq!(error_str(error, "tool"), "tracedecay_affected_tests");
    assert_eq!(
        error_str(error, "reason_code"),
        "application_surface_invalid_request"
    );
    assert_eq!(error_str(error, "kind"), "invalid_request");
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("retryable"))
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        error_str(error, "detail"),
        "application surface request handle is invalid"
    );
    assert_eq!(
        error.message,
        "tool project route failed: reason_code=application_surface_invalid_request retryable=false: application surface request handle is invalid"
    );
}

fn assert_missing_handle_field(response: &JsonRpcResponse) {
    let error = response
        .error
        .as_ref()
        .expect("omitting request_handle is a JSON-RPC error");
    assert_eq!(error.code, -32602);
    assert_eq!(error_str(error, "tool"), "tracedecay_affected_tests");
    assert_eq!(
        error_str(error, "reason_code"),
        "application_surface_invalid_request"
    );
    assert_eq!(error_str(error, "kind"), "invalid_request");
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("retryable"))
            .and_then(Value::as_bool),
        Some(false)
    );
    let detail = error
        .data
        .as_ref()
        .and_then(|data| data.get("detail"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing schema detail: {error:?}"));
    assert!(
        detail.contains("missing field `request_handle`"),
        "the refusal must name the missing handle: {detail}"
    );
}

fn assert_unknown_handle_markdown(response: &JsonRpcResponse) {
    assert!(
        response.error.is_none(),
        "an unknown handle is a typed tool problem, not a transport error: {:?}",
        response.error
    );
    let result = response
        .result
        .as_ref()
        .expect("unknown-handle tool result");
    assert_eq!(result["isError"], true, "{result}");
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("unknown-handle markdown: {result}"));
    assert!(
        text.contains("## affected\\_tests"),
        "default view must name the tool: {text}"
    );
    assert!(
        text.contains("- Problem: `not_found_or_not_authorized`"),
        "{text}"
    );
    assert!(
        text.contains("- Message: The requested resource was not found or is not authorized"),
        "{text}"
    );
    assert!(text.contains("- Retryable: `false`"), "{text}");
    assert!(text.contains("- Retry: `never`"), "{text}");
}

async fn published_cycle(fixture: &ProductionCompositionFixture, document_uri: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let response = call(
            fixture,
            "tracedecay_feedback_advisory_cycle",
            json!({"document_uri": document_uri, "format": "json"}),
        )
        .await;
        if response.error.is_none()
            && response
                .result
                .as_ref()
                .is_some_and(|result| result["isError"] != true)
        {
            let result = response.result.expect("advisory result");
            let envelope = tool_json(fixture, &result).await;
            let payload = envelope
                .pointer("/outcome/value/payload")
                .cloned()
                .unwrap_or_else(|| panic!("advisory cycle returned no payload: {envelope}"));
            assert_eq!(envelope["outcome"]["outcome"], "evidence");
            return payload;
        }
        let retryable = advisory_retryable(&response);
        assert!(
            retryable && Instant::now() < deadline,
            "advisory cycle did not publish a readable cycle: {response:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn advisory_retryable(response: &JsonRpcResponse) -> bool {
    if let Some(error) = &response.error {
        return error
            .data
            .as_ref()
            .is_some_and(|data| data["retryable"] == true);
    }
    let Some(result) = &response.result else {
        return false;
    };
    let Ok(body) =
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap_or(""))
    else {
        return false;
    };
    body["problem"]["code"] == "feedback.advisory-cycle.unavailable"
        && body["problem"]["retryable"] == true
}

async fn symbol_id(
    fixture: &ProductionCompositionFixture,
    name: &str,
    qualified_suffix: &str,
) -> String {
    let response = call(
        fixture,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20, "format": "json"}),
    )
    .await;
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.expect("exact-symbol result");
    assert_ne!(result["isError"], true, "{result}");
    let payload = tool_json(fixture, &result).await;
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches.iter().find(|item| {
                item["name"] == name
                    && item["qualified_name"]
                        .as_str()
                        .is_some_and(|qualified| qualified.ends_with(qualified_suffix))
            })
        })
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("indexed symbol {name} ({qualified_suffix}) missing: {payload}"))
        .to_owned()
}

async fn call(
    fixture: &ProductionCompositionFixture,
    name: &str,
    arguments: Value,
) -> JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{name} MCP call failed: {error}"))
}

async fn tool_json(fixture: &ProductionCompositionFixture, result: &Value) -> Value {
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool text missing: {result}"));
    let text = if let Some(handle) = truncated_response_handle(text) {
        let retrieved = call(
            fixture,
            "tracedecay_retrieve",
            json!({"handle": handle, "format": "json"}),
        )
        .await;
        assert!(retrieved.error.is_none(), "{:?}", retrieved.error);
        let retrieved = retrieved.result.expect("retrieve result");
        let record: Value = serde_json::from_str(
            retrieved["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("retrieve text missing: {retrieved}")),
        )
        .unwrap_or_else(|error| panic!("retrieve JSON: {error}; {retrieved}"));
        record["content"]
            .as_str()
            .unwrap_or_else(|| panic!("retrieve did not restore the tool text: {record}"))
            .to_owned()
    } else {
        text.to_owned()
    };
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("tool JSON: {error}; {text}"))
}
