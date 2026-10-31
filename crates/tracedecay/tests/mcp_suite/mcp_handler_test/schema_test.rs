use crate::support::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tracedecay::mcp::McpServer;
use tracedecay_mcp::get_tool_definitions;
#[tokio::test]
async fn schema_required_arguments_match_representative_handler_parsers() {
    let tools = get_tool_definitions().expect("tool definitions");
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    wait_for_current_graph(&server).await;

    // Typed request decode.
    assert_schema_requires(&tools, "tracedecay_search", &["query"]);
    expect_real_server_missing_argument_error(
        &server,
        "tracedecay_search",
        json!({}),
        Refusal::Problem,
        "invalid arguments for tracedecay_search: missing field `query`",
    )
    .await;

    // Routed graph tools need the production query authority mounted before
    // dispatch reaches their operation parser.
    // Canonical application request parser style.
    assert_schema_requires(&tools, "tracedecay_callers", &["node_id"]);
    expect_real_server_missing_argument_error(
        &server,
        "tracedecay_callers",
        json!({}),
        Refusal::Route,
        "missing field `node_id`",
    )
    .await;

    // Multi-field edit parser style.
    assert_schema_requires(
        &tools,
        "tracedecay_insert_at",
        &["path", "anchor", "content"],
    );
    expect_real_server_missing_argument_error(
        &server,
        "tracedecay_insert_at",
        json!({ "path": "src/lib.rs" }),
        Refusal::Route,
        "missing required parameter: anchor",
    )
    .await;

    // Tools whose advertised `required` array and handler parser must agree.
    // Exact fact routes project only their operation-specific required fields;
    // the git, port, and context routes are here so that "the handler rejects a
    // call with no arguments" is checked together with "the schema told the
    // caller which arguments were missing", either half alone lets the two
    // drift apart.
    for (tool_name, required_args, refusal, expected_message) in [
        (
            "tracedecay_fact_store_add",
            &["content"][..],
            Refusal::Route,
            "missing field `content`",
        ),
        (
            "tracedecay_fact_store_search",
            &["query"][..],
            Refusal::Route,
            "missing field `query`",
        ),
        (
            "tracedecay_fact_store_probe",
            &["entity"][..],
            Refusal::Route,
            "missing field `entity`",
        ),
        (
            "tracedecay_fact_store_related",
            &["entity"][..],
            Refusal::Route,
            "missing field `entity`",
        ),
        (
            "tracedecay_fact_store_get",
            &["fact_id"][..],
            Refusal::Route,
            "missing field `fact_id`",
        ),
        (
            "tracedecay_fact_store_update",
            &["fact_id"][..],
            Refusal::Route,
            "missing field `fact_id`",
        ),
        (
            "tracedecay_fact_store_remove",
            &["fact_id"][..],
            Refusal::Route,
            "missing field `fact_id`",
        ),
        (
            "tracedecay_fact_store_supersede",
            &["fact_id", "superseded_by"][..],
            Refusal::Route,
            "missing field `fact_id`",
        ),
        // Generated schemas, decoded straight into a typed request struct, so
        // the parser error is serde's rather than a hand-written message.
        (
            "tracedecay_diff_context",
            &["files"][..],
            Refusal::Problem,
            "missing field `files`",
        ),
        (
            "tracedecay_changelog",
            &["from_ref", "to_ref"][..],
            Refusal::Problem,
            "missing field `from_ref`",
        ),
        (
            "tracedecay_port_status",
            &["source_dir", "target_dir"][..],
            Refusal::Problem,
            "missing field `source_dir`",
        ),
        (
            "tracedecay_port_order",
            &["source_dir"][..],
            Refusal::Problem,
            "missing field `source_dir`",
        ),
        (
            "tracedecay_context",
            &["task"][..],
            Refusal::Problem,
            "missing field `task`",
        ),
    ] {
        assert_schema_requires(&tools, tool_name, required_args);
        expect_real_server_missing_argument_error(
            &server,
            tool_name,
            json!({}),
            refusal,
            expected_message,
        )
        .await;
    }
    // Nested-object parser style.
    assert_schema_requires(
        &tools,
        "tracedecay_lcm_expand",
        &["provider", "session_id", "target"],
    );
    let expand = tool_schema(&tools, "tracedecay_lcm_expand");
    let target_branches = expand["properties"]["target"]["oneOf"]
        .as_array()
        .expect("closed target branches");
    assert_eq!(target_branches.len(), 4);
    assert_eq!(
        target_branches[0]["properties"]["kind"]["const"],
        "canonical_occurrence"
    );
    assert_eq!(
        target_branches[0]["required"],
        json!(["kind", "message_id"])
    );
    assert_eq!(target_branches[1]["required"], json!(["kind", "store_id"]));
    assert_eq!(target_branches[2]["required"], json!(["kind", "node_id"]));
    assert_eq!(
        target_branches[3]["required"],
        json!(["kind", "payload_ref"])
    );
    expect_real_server_missing_argument_error(
        &server,
        "tracedecay_lcm_expand",
        json!({ "provider": "cursor", "session_id": "session-1", "target": {} }),
        Refusal::Route,
        "target: missing field `kind`",
    )
    .await;
    fixture.shutdown().await;
}

pub(crate) fn tool_schema<'a>(
    tools: &'a [tracedecay_mcp::ToolDefinition],
    name: &str,
) -> &'a Value {
    &tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("missing tool definition for {name}"))
        .input_schema
}

pub(crate) fn required_args_at<'a>(schema: &'a Value, path: &[&str]) -> Vec<&'a str> {
    let mut node = schema;
    for segment in path {
        node = &node["properties"][*segment];
    }
    node["required"]
        .as_array()
        .unwrap_or_else(|| panic!("schema path {path:?} is missing a required array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("schema path {path:?} has non-string required entry"))
        })
        .collect()
}

pub(crate) fn assert_schema_requires(
    tools: &[tracedecay_mcp::ToolDefinition],
    tool_name: &str,
    expected: &[&str],
) {
    let schema = tool_schema(tools, tool_name);
    let actual = required_args_at(schema, &[]);
    assert_eq!(
        actual, expected,
        "{tool_name} schema required arguments drifted from handler parser expectations"
    );
}

/// How a tool's parser rejection reaches the client.
#[derive(Clone, Copy)]
enum Refusal {
    /// Graph-tool owner: an `isError` result carrying an `invalid_request`
    /// problem record.
    Problem,
    /// Application-surface schema admission: a settled route refusal whose
    /// diagnostic is `application_surface_invalid_request`.
    Route,
}

async fn expect_real_server_missing_argument_error(
    server: &Arc<McpServer>,
    tool_name: &str,
    args: Value,
    refusal: Refusal,
    expected_message: &str,
) {
    let response = handle_real_server_tool_call_raw(server, tool_name, args).await;
    let message = match refusal {
        Refusal::Problem => {
            assert!(
                response.get("error").is_none(),
                "{tool_name} should refuse as a tool result: {response}"
            );
            let problem = refusal_problem(&response["result"]);
            assert_eq!(
                problem["kind"], "invalid_request",
                "{tool_name}: {response}"
            );
            assert_eq!(
                problem["code"], "application.surface.invalid_request",
                "{tool_name}: {response}"
            );
            problem["message"].as_str()
        }
        Refusal::Route => {
            let refusal = route_refusal(&response);
            assert_eq!(
                (&refusal["kind"], &refusal["diagnostic"]["code"]),
                (
                    &json!("invalid_request"),
                    &json!("application_surface_invalid_request")
                ),
                "{tool_name}: {response}"
            );
            response["result"]["structuredContent"]["problem"]["diagnostic"]["message"].as_str()
        }
    }
    .unwrap_or_else(|| panic!("{tool_name} should reject missing arguments: {response}"));
    assert!(
        message.contains(expected_message),
        "{tool_name} parser error should mention `{expected_message}`, got `{message}`"
    );
}
