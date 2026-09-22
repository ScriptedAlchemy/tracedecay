//! `tracedecay_multi_root_scope_set_read` as an MCP client sees it.
//!
//! Hosts call this through `tools/call` on the production composition. An
//! absent scope set is concealed as not-found-or-not-authorized; a saved set
//! is returned unchanged. Compare-and-swap is only the fixture that creates
//! the record the read is asked about.

use std::sync::Arc;

use serde_json::{Value, json};

use super::support::{jsonrpc_request, response_with_id, run_client_connection_with_messages};

const READ_TOOL: &str = "tracedecay_multi_root_scope_set_read";
const SAVED_SCOPE_SET_ID: &str = "scope-set.read-proof";
const OTHER_ABSENT_SCOPE_SET_ID: &str = "scope-set.read-proof.other";

fn tool_call(id: i64, name: &str, arguments: Value) -> String {
    jsonrpc_request(
        json!(id),
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
}

fn application_text(response: &Value) -> Value {
    assert!(
        response["error"].is_null(),
        "tools/call must stay a JSON-RPC success: {response}"
    );
    assert_eq!(
        response["result"]["content"][0]["type"], "text",
        "tool content must be text: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    serde_json::from_str(text).unwrap_or_else(|error| panic!("tool text is JSON ({error}): {text}"))
}

fn assert_concealed_absence(response: &Value) {
    assert_eq!(
        response["result"]["isError"], true,
        "an absent scope set must be a semantic tool error, not an empty success: {response}"
    );
    let application = application_text(response);
    assert_eq!(
        application["binding_id"],
        "binding.http.multi_root.scope_set_read.v1"
    );
    assert_eq!(
        application["application"]["contract"]["schema_id"],
        "schema.tracedecay.multi-root.scope-set-read-result.v1"
    );
    assert_eq!(application["application"]["contract"]["schema_revision"], 1);
    assert!(
        application["application"].get("outcome").is_none(),
        "concealment must not return an evidence outcome: {application}"
    );
    let problem = &application["application"]["problem"];
    assert_eq!(problem["kind"], "not_found_or_not_authorized");
    assert_eq!(problem["code"], "not_found_or_not_authorized");
    assert_eq!(
        problem["message"],
        "The requested resource was not found or is not authorized"
    );
    assert_eq!(problem["diagnostic"], Value::Null);
    assert_eq!(problem["legal_actions"], json!([]));
    assert_eq!(problem["retry"], "never");
    assert_eq!(problem["retryable"], false);
    assert_eq!(problem["owning_layer"], "runtime");
    assert_eq!(problem["terminality"], "pre_admission");
    assert_eq!(problem["revision"], 1);
    assert_eq!(
        application["application"]["request_id"], problem["request_id"],
        "the problem must name the same request the client was given"
    );
    assert_eq!(problem["request_id"], problem["trace_id"]);
}

fn assert_invalid_request(response: &Value) {
    assert_eq!(
        response["result"]["isError"], true,
        "an invalid read must be a semantic tool error: {response}"
    );
    let application = application_text(response);
    assert_eq!(
        application["binding_id"],
        "binding.http.multi_root.scope_set_read.v1"
    );
    assert_eq!(
        application["application"]["contract"]["schema_id"],
        "schema.tracedecay.multi-root.scope-set-read-result.v1"
    );
    assert_eq!(application["application"]["contract"]["schema_revision"], 1);
    let problem = &application["application"]["problem"];
    assert_eq!(problem["kind"], "invalid_request");
    assert_eq!(problem["code"], "multi_root.invalid_request");
    assert_eq!(
        problem["message"],
        "The multi-root application request is invalid"
    );
    assert_eq!(
        problem["diagnostic"],
        json!({
            "code": "multi_root.invalid_request",
            "message": "The multi-root application request is invalid"
        })
    );
    assert_eq!(problem["legal_actions"], json!(["correct_request"]));
    assert_eq!(problem["retry"], "never");
    assert_eq!(problem["retryable"], false);
    assert_eq!(problem["owning_layer"], "runtime");
    assert_eq!(problem["terminality"], "pre_admission");
    assert_eq!(
        application["application"]["request_id"],
        problem["request_id"],
    );
}

#[tokio::test]
async fn tracedecay_multi_root_scope_set_read_reports_the_saved_set_and_conceals_absence() {
    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let project_id = fixture
        .harness
        .project_id(&fixture.project_root)
        .await
        .expect("registered project id");
    let root = fixture
        .project_root
        .canonicalize()
        .expect("canonical registered root");
    let root_text = root.to_string_lossy().into_owned();

    let responses = run_client_connection_with_messages(
        Arc::clone(&server),
        vec![
            tool_call(
                1,
                READ_TOOL,
                json!({
                    "scope_set_id": SAVED_SCOPE_SET_ID,
                    "unexpected_field": true
                }),
            ),
            tool_call(2, READ_TOOL, json!({})),
            tool_call(3, READ_TOOL, json!({ "scope_set_id": SAVED_SCOPE_SET_ID })),
            tool_call(
                4,
                "tracedecay_multi_root_scope_set_compare_and_swap",
                json!({
                    "scope_set_id": SAVED_SCOPE_SET_ID,
                    "roots": [{
                        "project_id": project_id,
                        "root": root_text
                    }]
                }),
            ),
            tool_call(5, READ_TOOL, json!({ "scope_set_id": SAVED_SCOPE_SET_ID })),
            tool_call(
                6,
                READ_TOOL,
                json!({ "scope_set_id": OTHER_ABSENT_SCOPE_SET_ID }),
            ),
        ],
    )
    .await;

    assert_invalid_request(&response_with_id(&responses, json!(1)));
    assert_invalid_request(&response_with_id(&responses, json!(2)));
    assert_concealed_absence(&response_with_id(&responses, json!(3)));

    let saved = response_with_id(&responses, json!(4));
    assert!(
        saved["error"].is_null() && saved["result"].get("isError").is_none(),
        "scope-set fixture save failed: {saved}"
    );

    let read = response_with_id(&responses, json!(5));
    assert!(
        read["error"].is_null(),
        "saved-set read must stay a JSON-RPC success: {read}"
    );
    assert!(
        read["result"].get("isError").is_none(),
        "a saved scope set must not be reported as a tool error: {read}"
    );
    let application = application_text(&read);
    assert_eq!(
        application["binding_id"],
        "binding.http.multi_root.scope_set_read.v1"
    );
    assert_eq!(
        application["application"]["contract"]["schema_id"],
        "schema.tracedecay.multi-root.scope-set-read-result.v1"
    );
    assert_eq!(application["application"]["contract"]["schema_revision"], 1);
    assert_eq!(application["application"]["outcome"]["outcome"], "evidence");
    assert_eq!(
        application["application"]["outcome"]["value"]["execution"]["termination"],
        "completed"
    );
    let payload = &application["application"]["outcome"]["value"]["payload"];
    assert_eq!(payload["scope_set_id"], SAVED_SCOPE_SET_ID);
    assert_eq!(payload["revision"], 1);
    assert_eq!(payload["roots"][0]["scope"]["project_id"], project_id);
    assert_eq!(payload["roots"][0]["locator"]["project_id"], project_id);
    assert_eq!(payload["roots"][0]["locator"]["canonical_root"], root_text);
    assert_eq!(payload["roots"].as_array().map(Vec::len), Some(1));

    assert_concealed_absence(&response_with_id(&responses, json!(6)));

    fixture.harness.shutdown().await;
}
