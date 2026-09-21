//! `tracedecay_fact_store_search` as a caller sees it over MCP `tools/call`.
//!
//! Each test sends the production JSON-RPC request through `McpServer` and
//! asserts the payload a client reads. `tracedecay_fact_store_add` and
//! `tracedecay_fact_store_supersede` only seed the store; every assertion is
//! the search result, the typed refusal, or the markdown a client reads.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;

use crate::support::{
    TestTempDir, commit_worktree, extract_real_server_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, production_composition_fixture, test_temp_dir,
};

const TOOL: &str = "tracedecay_fact_store_search";
const HIGH: &str = "Quince ledger closes on the last Friday";
const LOW: &str = "Quince rumor from the hallway";
const MANGO: &str = "Mango routing prefers the west region";
const ACTIVE_SIRIUS: &str = "Sirius catalog stays with the calling project";
const TARGET_SIRIUS: &str = "Sirius catalog stays with the registered target";
const USER_NOTE: &str = "Operator wants violet notes in one sentence";
const OLD_KEPLER: &str = "Kepler cutoff lands on monday";
const NEW_ORION: &str = "Orion departure window moved to friday after the retro";

struct SearchProject {
    server: Arc<McpServer>,
    project_id: String,
}

async fn open_search_project() -> (crate::support::ProductionCompositionFixture, SearchProject) {
    let fixture = production_composition_fixture().await;
    let project = SearchProject::from_server(
        fixture
            .harness
            .server(&fixture.project_root)
            .expect("production fact-search MCP server"),
    )
    .await;
    (fixture, project)
}

impl SearchProject {
    async fn from_server(server: Arc<McpServer>) -> Self {
        let project_id = server
            .cg()
            .await
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("registered project id")
            .to_owned();
        Self { server, project_id }
    }
}

async fn store_fact(server: &McpServer, body: Value) -> String {
    let result = handle_real_server_tool_call(server, "tracedecay_fact_store_add", body).await;
    let payload: Value =
        serde_json::from_str(extract_real_server_text(&result)).expect("fact add JSON");
    assert_eq!(payload["outcome"], "committed", "{payload}");
    assert_eq!(payload["result"]["disposition"], "added", "{payload}");
    payload["result"]["fact"]["fact"]["fact_id"]
        .as_str()
        .expect("stored fact id")
        .to_owned()
}

async fn call_raw(server: &McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, TOOL, arguments).await
}

async fn search_payload(server: &McpServer, arguments: Value) -> Value {
    let response = call_raw(server, arguments).await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(response["error"].is_null(), "{response}");
    let result = &response["result"];
    assert_ne!(result["isError"], json!(true), "{response}");
    let envelope = parse_text(result);
    assert_eq!(
        envelope["contract"],
        json!({
            "schema_id": "schema.application.retained.fact-store-search.result",
            "schema_revision": 1
        }),
        "{envelope}"
    );
    assert_eq!(envelope["outcome"]["outcome"], "evidence", "{envelope}");
    envelope["outcome"]["value"]["payload"].clone()
}

async fn search_markdown(server: &McpServer, arguments: Value) -> String {
    let mut arguments = arguments;
    arguments
        .as_object_mut()
        .expect("markdown arguments")
        .insert("format".to_owned(), json!("markdown"));
    let response = call_raw(server, arguments).await;
    assert!(response["error"].is_null(), "{response}");
    assert_ne!(response["result"]["isError"], json!(true), "{response}");
    extract_real_server_text(&response["result"]).to_owned()
}

fn parse_text(result: &Value) -> Value {
    let text = extract_real_server_text(result);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

fn contents(payload: &Value) -> Vec<String> {
    payload["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("hits is not an array: {payload}"))
        .iter()
        .map(|hit| {
            hit["fact"]["content"]
                .as_str()
                .unwrap_or_else(|| panic!("hit content missing: {hit}"))
                .to_owned()
        })
        .collect()
}

fn stable_hit(hit: &Value) -> Value {
    let fact = &hit["fact"];
    json!({
        "content": fact["content"],
        "category": fact["category"],
        "tags": fact["tags"],
        "entities": fact["entities"],
        "source_label": fact["source_label"],
        "trust_score_millionths": fact["trust_score_millionths"],
        "source_kind": fact["source"]["kind"],
        "metadata": fact["metadata"],
        "retrieval_count": fact["telemetry"]["retrieval_count"],
        "access_count": fact["telemetry"]["access_count"],
        "helpful_count": fact["telemetry"]["helpful_count"],
        "unhelpful_count": fact["telemetry"]["unhelpful_count"],
        "last_feedback_at": fact["telemetry"]["last_feedback_at"],
        "scores": hit["scores"],
        "why": hit["why"],
    })
}

fn assert_project_page(payload: &Value, project_id: &str, roots: u64, relations: u64) {
    assert_eq!(
        payload["owner"],
        json!({"kind": "project", "project_id": project_id}),
        "{payload}"
    );
    assert_eq!(
        payload["graph_coverage"],
        json!({
            "kind": "complete",
            "root_count": roots,
            "relation_count": relations,
            "expanded_fact_count": 0
        }),
        "{payload}"
    );
}

fn assert_empty_miss(payload: &Value, project_id: &str) {
    assert_eq!(payload["hits"], json!([]), "{payload}");
    assert_eq!(payload["next_after"], Value::Null, "{payload}");
    assert_eq!(
        payload["retrieval_telemetry"],
        json!({"kind": "not_applicable"}),
        "{payload}"
    );
    assert_eq!(
        payload["owner"],
        json!({"kind": "project", "project_id": project_id}),
        "{payload}"
    );
    assert_eq!(
        payload["graph_coverage"],
        json!({
            "kind": "complete",
            "root_count": 0,
            "relation_count": 0,
            "expanded_fact_count": 0
        }),
        "{payload}"
    );
}

const CLI_FALLBACK: &str = "This tool is also available from the shell: `tracedecay tool fact_store_search ...` (`tracedecay tool fact_store_search --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.";

fn decode_failure(detail: &str) -> String {
    format!(
        "tool execution failed: config error: invalid retained application request for {TOOL}: {detail}"
    )
}

fn assert_schema_rejection(response: &Value, message: &str) {
    assert_eq!(response["jsonrpc"], "2.0", "{response}");
    assert_eq!(response["id"], 1, "{response}");
    assert!(
        response.get("result").is_none() || response["result"].is_null(),
        "{response}"
    );
    assert_eq!(
        response["error"],
        json!({
            "code": -32603,
            "message": message,
            "data": {
                "cli_fallback": CLI_FALLBACK,
                "tool": TOOL
            }
        }),
        "{response}"
    );
}

fn stable_problem(problem: &Value) -> Value {
    let mut problem = problem.clone();
    let object = problem.as_object_mut().expect("problem is not an object");
    let request_id = object
        .get("request_id")
        .and_then(Value::as_str)
        .expect("problem request id")
        .to_owned();
    let trace_id = object
        .get("trace_id")
        .and_then(Value::as_str)
        .expect("problem trace id")
        .to_owned();
    assert_eq!(request_id, trace_id);
    object.insert("request_id".to_owned(), json!("request.stable"));
    object.insert("trace_id".to_owned(), json!("request.stable"));
    problem
}

fn invalid_request_problem() -> Value {
    json!({
        "revision": 1,
        "kind": "invalid_request",
        "code": "application.retained.invalid-request",
        "message": "The retained operation request is invalid.",
        "diagnostic": {
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid."
        },
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": "pre_admission",
        "retryable": false,
        "retry": "never",
        "retry_scope": null,
        "retry_after_millis": null,
        "cancellation_stage": null,
        "unavailable_classification": null,
        "execution_failure_classification": null,
        "request_id": "request.stable",
        "trace_id": "request.stable",
        "details": [],
        "legal_actions": ["correct_request"],
        "coverage": null
    })
}

fn assert_problem(response: &Value, expected: Value) {
    assert_eq!(response["jsonrpc"], "2.0", "{response}");
    assert_eq!(response["id"], 1, "{response}");
    assert!(response["error"].is_null(), "{response}");
    let result = &response["result"];
    assert_eq!(result["isError"], true, "{response}");
    assert_eq!(stable_problem(&result["problem"]), expected, "{response}");
    let envelope = parse_text(result);
    assert_eq!(
        envelope["contract"]["schema_id"], "schema.application.retained.fact-store-search.result",
        "{envelope}"
    );
    assert_eq!(envelope["contract"]["schema_revision"], 1, "{envelope}");
    assert_eq!(
        envelope["request_id"], envelope["problem"]["request_id"],
        "{envelope}"
    );
    assert_eq!(stable_problem(&envelope["problem"]), expected, "{envelope}");
}

fn initialize_fact_project(root: &Path) {
    fs::create_dir_all(root).expect("fact project root");
    crate::fixture::write_indexed_fixture_sources(root);
    commit_worktree(root, "fact search fixture");
}

struct CrossProject {
    harness: ProductionProjectCompositionHarnessV1,
    active: SearchProject,
    target: SearchProject,
    _isolation: TestTempDir,
}

async fn open_cross_project() -> CrossProject {
    let isolation = test_temp_dir();
    let active_root = isolation.path().join("active");
    let target_root = isolation.path().join("target");
    initialize_fact_project(&active_root);
    initialize_fact_project(&target_root);
    let harness = Box::pin(ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        vec![active_root.clone(), target_root.clone()],
    ))
    .await
    .expect("cross-project fact search harness");
    let active = SearchProject::from_server(
        harness
            .server(&active_root)
            .expect("active fact search server"),
    )
    .await;
    let target = SearchProject::from_server(
        harness
            .server(&target_root)
            .expect("target fact search server"),
    )
    .await;
    CrossProject {
        harness,
        active,
        target,
        _isolation: isolation,
    }
}

#[tokio::test]
async fn fact_store_search_returns_the_stored_fact_and_pages_the_rest() {
    let (fixture, project) = open_search_project().await;
    let server = &project.server;
    let high_id = store_fact(
        server,
        json!({
            "content": HIGH,
            "category": "decision",
            "tags": ["billing"],
            "entities": ["Desk"],
            "trust": 1.0,
            "source_label": "billing-note"
        }),
    )
    .await;
    store_fact(
        server,
        json!({
            "content": LOW,
            "category": "decision",
            "tags": ["rumor"],
            "entities": ["Hallway"],
            "trust": 0.25,
            "source_label": "hallway-note"
        }),
    )
    .await;
    store_fact(
        server,
        json!({
            "content": MANGO,
            "category": "project",
            "trust": 1.0,
            "source_label": "routing-note"
        }),
    )
    .await;

    let exact = search_payload(
        server,
        json!({"query": "ledger", "category": "decision", "limit": 5}),
    )
    .await;
    assert_eq!(contents(&exact), vec![HIGH.to_owned()], "{exact}");
    assert_eq!(exact["hits"][0]["fact"]["fact_id"], high_id, "{exact}");
    assert_eq!(exact["next_after"], Value::Null, "{exact}");
    assert_eq!(
        exact["retrieval_telemetry"],
        json!({"kind": "recorded", "fact_count": 1}),
        "{exact}"
    );
    assert_project_page(&exact, &project.project_id, 1, 2);
    assert_eq!(
        stable_hit(&exact["hits"][0]),
        json!({
            "content": HIGH,
            "category": "decision",
            "tags": ["billing"],
            "entities": ["Desk"],
            "source_label": "billing-note",
            "trust_score_millionths": 1_000_000,
            "source_kind": "application",
            "metadata": {},
            "retrieval_count": 1,
            "access_count": 1,
            "helpful_count": 0,
            "unhelpful_count": 0,
            "last_feedback_at": null,
            "scores": {
                "fts_score_millionths": 1_000_000,
                "holographic_score_millionths": 621_363,
                "jaccard_score_millionths": 111_111,
                "score_millionths": 619_742,
                "trust_score_millionths": 1_000_000
            },
            "why": "fts=1.000, coverage=1.000, jaccard=0.111, holographic=0.621, trust=1.000, temporal_decay=1.000, retrieval_count=0"
        }),
        "{exact}"
    );

    let above_floor =
        search_payload(server, json!({"query": "Quince", "category": "decision"})).await;
    assert_eq!(
        contents(&above_floor),
        vec![HIGH.to_owned()],
        "{above_floor}"
    );

    let first_page = search_payload(
        server,
        json!({
            "query": "Quince",
            "category": "decision",
            "min_trust": 0.0,
            "limit": 1
        }),
    )
    .await;
    assert_eq!(contents(&first_page), vec![HIGH.to_owned()], "{first_page}");
    assert_eq!(
        first_page["next_after"]["fact_id"], first_page["hits"][0]["fact"]["fact_id"],
        "{first_page}"
    );
    assert_eq!(
        first_page["retrieval_telemetry"],
        json!({"kind": "recorded", "fact_count": 1}),
        "{first_page}"
    );
    let second_page = search_payload(
        server,
        json!({
            "query": "Quince",
            "category": "decision",
            "min_trust": 0.0,
            "limit": 1,
            "after": first_page["next_after"]
        }),
    )
    .await;
    assert_eq!(
        contents(&second_page),
        vec![LOW.to_owned()],
        "{second_page}"
    );
    assert_eq!(second_page["next_after"], Value::Null, "{second_page}");
    assert_eq!(
        second_page["hits"][0]["fact"]["trust_score_millionths"], 250_000,
        "{second_page}"
    );
    assert_eq!(
        second_page["hits"][0]["fact"]["source_label"], "hallway-note",
        "{second_page}"
    );

    let project_only = search_payload(
        server,
        json!({"query": "Mango routing", "category": "project", "limit": 5}),
    )
    .await;
    assert_eq!(
        contents(&project_only),
        vec![MANGO.to_owned()],
        "{project_only}"
    );
    assert_eq!(
        project_only["hits"][0]["fact"]["category"], "project",
        "{project_only}"
    );

    let miss = search_payload(server, json!({"query": "zzzznotoken", "limit": 5})).await;
    assert_empty_miss(&miss, &project.project_id);

    let markdown = search_markdown(
        server,
        json!({"query": "ledger", "category": "decision", "limit": 5}),
    )
    .await;
    assert!(
        markdown.starts_with("## fact\\_store\\_search\n"),
        "{markdown}"
    );
    assert!(markdown.contains("\n- Status: `success`"), "{markdown}");
    assert!(markdown.contains(HIGH), "{markdown}");
    assert!(!markdown.contains(LOW), "{markdown}");
    assert!(!markdown.contains(MANGO), "{markdown}");

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn fact_store_search_rejects_blank_limit_and_unknown_fields() {
    let (fixture, project) = open_search_project().await;
    let server = &project.server;

    for (arguments, detail) in [
        (json!({}), decode_failure("missing field `query`")),
        (
            json!({"limit": 1}),
            decode_failure("missing field `query`"),
        ),
        (
            json!({"query": null}),
            decode_failure("query: invalid type: null, expected a string"),
        ),
        (
            json!({"query": 12}),
            decode_failure("query: invalid type: integer `12`, expected a string"),
        ),
        (
            json!({"query": "ledger", "action": "search"}),
            decode_failure("unknown field `action`"),
        ),
        (
            json!({"query": "ledger", "format": "yaml"}),
            "tool execution failed: config error: application surface request does not match its reviewed schema: `format` must be markdown or json"
                .to_owned(),
        ),
        (
            json!({"query": "ledger", "category": "pitfall"}),
            decode_failure(
                "unknown variant `pitfall`, expected one of `general`, `user_pref`, `project`, `tool`, `decision`, `code_area`",
            ),
        ),
        (
            json!({"query": "ledger", "memory_scope": "global"}),
            decode_failure("unknown variant `global`, expected `project` or `user`"),
        ),
    ] {
        let response = call_raw(server, arguments).await;
        assert_schema_rejection(&response, &detail);
    }

    for arguments in [
        json!({"query": "   "}),
        json!({"query": ""}),
        json!({"query": "ledger", "limit": 0}),
        json!({"query": "ledger", "limit": 201}),
        json!({"query": "ledger", "min_trust": 1.5}),
        json!({"query": "ledger", "min_trust": -0.1}),
    ] {
        let response = call_raw(server, arguments).await;
        assert_problem(&response, invalid_request_problem());
    }

    let missing = call_raw(
        server,
        json!({
            "query": "ledger",
            "project_selector": {"project_id": "project.missing"}
        }),
    )
    .await;
    assert_eq!(missing["jsonrpc"], "2.0", "{missing}");
    assert_eq!(missing["id"], 1, "{missing}");
    assert!(
        missing.get("result").is_none() || missing["result"].is_null(),
        "{missing}"
    );
    assert_eq!(
        missing["error"],
        json!({
            "code": -32602,
            "message": "tool project route failed: reason_code=project_route_not_found retryable=false: registered project not found for project_selector.project_id=project.missing; run tracedecay_project_search",
            "data": {
                "detail": "registered project not found for project_selector.project_id=project.missing; run tracedecay_project_search",
                "reason_code": "project_route_not_found",
                "retryable": false,
                "tool": TOOL
            }
        }),
        "{missing}"
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn fact_store_search_keeps_scope_and_drops_superseded_facts() {
    let fixture = open_cross_project().await;
    let active = &fixture.active.server;
    let target = &fixture.target.server;

    store_fact(
        active,
        json!({
            "content": ACTIVE_SIRIUS,
            "category": "project",
            "trust": 1.0
        }),
    )
    .await;
    store_fact(
        target,
        json!({
            "content": TARGET_SIRIUS,
            "category": "project",
            "trust": 1.0
        }),
    )
    .await;
    store_fact(
        active,
        json!({
            "content": USER_NOTE,
            "category": "user_pref",
            "memory_scope": "user",
            "trust": 1.0
        }),
    )
    .await;

    let active_page = search_payload(active, json!({"query": "Sirius catalog", "limit": 5})).await;
    assert_eq!(
        contents(&active_page),
        vec![ACTIVE_SIRIUS.to_owned()],
        "{active_page}"
    );
    assert_eq!(
        active_page["owner"],
        json!({"kind": "project", "project_id": fixture.active.project_id}),
        "{active_page}"
    );

    let selected = search_payload(
        active,
        json!({
            "query": "Sirius catalog",
            "limit": 5,
            "project_selector": {"project_id": fixture.target.project_id}
        }),
    )
    .await;
    assert_eq!(
        contents(&selected),
        vec![TARGET_SIRIUS.to_owned()],
        "{selected}"
    );
    assert_eq!(
        selected["owner"],
        json!({"kind": "project", "project_id": fixture.target.project_id}),
        "{selected}"
    );

    let user_page = search_payload(
        active,
        json!({"query": "violet notes", "memory_scope": "user", "limit": 5}),
    )
    .await;
    assert_eq!(
        contents(&user_page),
        vec![USER_NOTE.to_owned()],
        "{user_page}"
    );
    assert_eq!(
        user_page["owner"],
        json!({"kind": "profile"}),
        "{user_page}"
    );
    assert_eq!(
        user_page["hits"][0]["fact"]["category"], "user_pref",
        "{user_page}"
    );
    let project_miss = search_payload(active, json!({"query": "violet notes", "limit": 5})).await;
    assert_empty_miss(&project_miss, &fixture.active.project_id);

    let old_id = store_fact(
        active,
        json!({"content": OLD_KEPLER, "category": "decision", "trust": 1.0}),
    )
    .await;
    let new_id = store_fact(
        active,
        json!({"content": NEW_ORION, "category": "decision", "trust": 1.0}),
    )
    .await;
    let before = search_payload(active, json!({"query": "Kepler cutoff", "limit": 5})).await;
    assert_eq!(contents(&before), vec![OLD_KEPLER.to_owned()], "{before}");

    let superseded = handle_real_server_tool_call(
        active,
        "tracedecay_fact_store_supersede",
        json!({"fact_id": old_id, "superseded_by": new_id}),
    )
    .await;
    let superseded: Value =
        serde_json::from_str(extract_real_server_text(&superseded)).expect("supersede JSON");
    assert_eq!(superseded["outcome"], "superseded", "{superseded}");

    let retired = search_payload(active, json!({"query": "Kepler cutoff", "limit": 5})).await;
    assert_empty_miss(&retired, &fixture.active.project_id);
    let successor = search_payload(active, json!({"query": "Orion departure", "limit": 5})).await;
    assert_eq!(
        contents(&successor),
        vec![NEW_ORION.to_owned()],
        "{successor}"
    );
    assert_eq!(
        successor["hits"][0]["fact"]["fact_id"], new_id,
        "{successor}"
    );

    fixture.harness.shutdown().await;
}
