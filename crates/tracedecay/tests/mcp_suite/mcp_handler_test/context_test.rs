#![cfg(feature = "test-transport")]

use crate::support::*;
use serde_json::{Value, json};
use std::fs;
use std::process::Command;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_domain::errors::{Result as TraceDecayResult, TraceDecayError};
use tracedecay_mcp::ToolResult;

struct ScopedProductionContextFixture {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: std::path::PathBuf,
    _isolation: TestTempDir,
}

async fn setup_production_project() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production context server");
    warm_code_index_search(&server, "helper").await;
    fixture
}

async fn setup_production_generated_dir_project() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(project.join("dist")).unwrap();
        fs::write(project.join("src/lib.rs"), "pub fn kept() {}\n").unwrap();
        fs::write(
            project.join("dist/generated.js"),
            "export function generatedOnly() {}\n",
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production generated-dir context server");
    warm_code_index_search(&server, "kept").await;
    fixture
}

async fn setup_scoped_production_project(scope_prefix: &str) -> ScopedProductionContextFixture {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    crate::fixture::write_indexed_fixture_sources(&project_root);
    let init = Command::new(crate::common::git_program())
        .args(["init", "-q"])
        .current_dir(&project_root)
        .status()
        .unwrap();
    assert!(init.success(), "git init must succeed");
    let add = Command::new(crate::common::git_program())
        .args(["add", "."])
        .current_dir(&project_root)
        .status()
        .unwrap();
    assert!(add.success(), "git add must succeed");
    let commit = Command::new(crate::common::git_program())
        .args([
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "scoped production context fixture",
        ])
        .current_dir(&project_root)
        .status()
        .unwrap();
    assert!(commit.success(), "git commit must succeed");
    let harness = ProductionProjectCompositionHarnessV1::open_with_scope_prefix(
        isolation.path(),
        [project_root.clone()],
        scope_prefix,
    )
    .await
    .unwrap();
    let fixture = ScopedProductionContextFixture {
        harness,
        project_root,
        _isolation: isolation,
    };
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("scoped production context server");
    warm_code_index_search(&server, "test_helper").await;
    fixture
}

async fn call_production_tool(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> TraceDecayResult<ToolResult> {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .map_err(|error| TraceDecayError::Config {
            message: format!("{tool_name} production project server is unavailable: {error}"),
        })?;
    let mut value = handle_real_server_tool_call(&server, tool_name, arguments).await;
    if let Some(text) = value["content"][0]["text"].as_str()
        && let Ok(envelope) = serde_json::from_str::<Value>(text)
        && let Some(payload) = envelope.pointer("/outcome/value/payload")
    {
        value["content"][0]["text"] = Value::String(payload.to_string());
    }
    Ok(ToolResult::new(value, Vec::new()))
}

async fn call_direct_production_tool(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> TraceDecayResult<ToolResult> {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await?;
    if let Some(error) = response.error {
        return Err(TraceDecayError::Config {
            message: format!("{tool_name} failed over production MCP: {}", error.message),
        });
    }
    let value = response.result.ok_or_else(|| TraceDecayError::Config {
        message: format!("{tool_name} returned no production MCP result"),
    })?;
    Ok(ToolResult::new(value, Vec::new()))
}

async fn call_production_fact_tool(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    mut arguments: Value,
) -> Value {
    arguments
        .as_object_mut()
        .expect("canonical fact-store request object")
        .insert("format".to_owned(), json!("json"));
    let result = call_production_tool(fixture, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} failed: {error}"));
    assert_ne!(
        result.value["isError"], true,
        "{tool_name}: {:?}",
        result.value
    );
    let response: Value = serde_json::from_str(extract_text(&result.value))
        .unwrap_or_else(|error| panic!("{tool_name} returned invalid JSON: {error}"));
    response
}

fn available_fact(projection: &Value) -> &Value {
    assert_eq!(projection["kind"], "available");
    projection
        .get("fact")
        .expect("available projection must contain a fact")
}

fn committed_fact_id(payload: &Value) -> &str {
    assert_eq!(payload["outcome"], "committed");
    assert_eq!(payload["result"]["disposition"], "added");
    available_fact(&payload["result"]["fact"])["fact_id"]
        .as_str()
        .expect("committed add must return a canonical fact id")
}

#[tokio::test]
async fn test_context_appends_index_coverage_hint_for_skipped_generated_dirs() {
    let fixture = setup_production_generated_dir_project().await;

    let result = call_production_tool(
        &fixture,
        "tracedecay_context",
        json!({"task": "generatedOnly", "max_nodes": 5, "format": "json"}),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert!(payload["code_generation"].as_str().is_some());
    assert_eq!(payload["code"], json!([]), "{payload}");
    assert_eq!(payload["symbols"], json!([]), "{payload}");
    assert_eq!(payload["coverage"]["exact"], "complete");
    assert_eq!(payload["coverage"]["lexical"], "complete");
    assert_eq!(payload["coverage"]["graph"], "complete");
    assert_eq!(payload["coverage"]["recall"], "partial");
    assert_eq!(payload["coverage"]["semantic"]["status"], "unavailable");
    assert!(
        payload.get("index_coverage_hint").is_none(),
        "verified retrieval coverage must not fabricate legacy skipped-directory advice: {payload}"
    );
    fixture.harness.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2. tracedecay_context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_context() {
    let fixture = setup_production_project().await;
    let result = call_direct_production_tool(
        &fixture,
        "tracedecay_context",
        json!({"task": "understand the helper function"}),
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn context_includes_matching_memory_facts() {
    let fixture = setup_production_project().await;
    let added = call_production_fact_tool(
        &fixture,
        "tracedecay_fact_store_add",
        json!({
            "content": "Helper function reviews should check durable memory before broad file search.",
            "category": "decision",
            "entities": ["helper function"],
            "tags": ["context", "memory"],
            "trust": 0.91,
            "source_label": "mcp-context-test"
        }),
    )
    .await;
    let fact_id = committed_fact_id(&added).to_owned();
    let before_context = call_production_fact_tool(
        &fixture,
        "tracedecay_fact_store_get",
        json!({"fact_id": fact_id.clone()}),
    )
    .await;
    let before_context = available_fact(&before_context["fact"]);
    let before_retrieval_count = before_context["telemetry"]["retrieval_count"]
        .as_u64()
        .expect("canonical get must project retrieval telemetry");
    let before_access_count = before_context["telemetry"]["access_count"]
        .as_u64()
        .expect("canonical get must project access telemetry");

    let markdown_result = call_production_tool(
        &fixture,
        "tracedecay_context",
        json!({
            "task": "helper function durable memory review",
            "format": "markdown"
        }),
    )
    .await
    .unwrap();
    let markdown = extract_text(&markdown_result.value);
    assert!(
        markdown.contains("### Memory Matches"),
        "context markdown should render matching durable memory: {markdown}"
    );
    assert!(markdown.contains(&format!("fact_id={fact_id}")));
    assert!(markdown.contains("Helper function reviews should check durable memory"));

    let json_result = call_production_tool(
        &fixture,
        "tracedecay_context",
        json!({"task": "helper function durable memory review", "format": "json"}),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&json_result.value)).unwrap();
    assert!(
        payload.get("context_memory_analytics").is_none(),
        "internal context analytics must not be serialized in direct tool payloads"
    );
    assert!(payload["memory_matches"].as_array().is_some_and(|matches| {
        matches
            .iter()
            .any(|hit| hit["fact"]["fact_id"].as_str() == Some(fact_id.as_str()))
    }));
    assert_eq!(payload["memory_graph_coverage"]["kind"], "complete");
    assert!(payload.get("memory_matches_error").is_none());

    let after_context = call_production_fact_tool(
        &fixture,
        "tracedecay_fact_store_get",
        json!({"fact_id": fact_id}),
    )
    .await;
    let after_context = available_fact(&after_context["fact"]);
    let after_retrieval_count = after_context["telemetry"]["retrieval_count"]
        .as_u64()
        .expect("canonical get must project retrieval telemetry");
    let after_access_count = after_context["telemetry"]["access_count"]
        .as_u64()
        .expect("canonical get must project access telemetry");
    assert_eq!(
        after_retrieval_count, before_retrieval_count,
        "context memory enrichment should not count as an explicit memory retrieval"
    );
    assert_eq!(
        after_access_count, before_access_count,
        "context memory enrichment should not count as an explicit memory recall"
    );
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn context_memory_controls_filter_disable_and_preserve_markdown() {
    let fixture = setup_production_project().await;
    let long_content = format!("Long memory control fact {}", "x".repeat(320));
    let long_added = call_production_fact_tool(
        &fixture,
        "tracedecay_fact_store_add",
        json!({
            "content": long_content,
            "category": "decision",
            "entities": ["long memory control"],
            "tags": ["context-memory-controls"],
            "trust": 0.92,
            "source_label": "mcp-context-test"
        }),
    )
    .await;
    let _long_fact_id = committed_fact_id(&long_added);
    let low_trust_added = call_production_fact_tool(
        &fixture,
        "tracedecay_fact_store_add",
        json!({
            "content": "Low trust memory control fact should stay filtered.",
            "category": "decision",
            "entities": ["low trust memory control"],
            "tags": ["context-memory-controls"],
            "trust": 0.8,
            "source_label": "mcp-context-test"
        }),
    )
    .await;
    let low_trust_fact_id = committed_fact_id(&low_trust_added).to_owned();

    let disabled = call_production_tool(
        &fixture,
        "tracedecay_context",
        json!({
            "task": "long memory control fact",
            "format": "json",
            "include_memory": false
        }),
    )
    .await
    .unwrap();
    let disabled_payload: Value = serde_json::from_str(extract_text(&disabled.value)).unwrap();
    assert_eq!(
        disabled_payload["memory_matches"].as_array().map(Vec::len),
        Some(0)
    );

    let admitted = call_production_tool(
        &fixture,
        "tracedecay_context",
        json!({
            "task": "low trust memory control fact",
            "format": "json",
            "memory_min_trust": 0.5
        }),
    )
    .await
    .unwrap();
    let admitted_payload: Value = serde_json::from_str(extract_text(&admitted.value)).unwrap();
    assert!(
        admitted_payload["memory_matches"]
            .as_array()
            .is_some_and(|matches| matches
                .iter()
                .any(|hit| hit["fact"]["fact_id"].as_str() == Some(low_trust_fact_id.as_str())))
    );

    let filtered = call_production_tool(
        &fixture,
        "tracedecay_context",
        json!({
            "task": "low trust memory control fact",
            "format": "json",
            "memory_min_trust": 0.9
        }),
    )
    .await
    .unwrap();
    let filtered_payload: Value = serde_json::from_str(extract_text(&filtered.value)).unwrap();
    assert!(
        !filtered_payload["memory_matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["fact"]["fact_id"].as_str() == Some(low_trust_fact_id.as_str()))
    );

    let markdown = call_production_tool(
        &fixture,
        "tracedecay_context",
        json!({"task": "long memory control fact", "memory_limit": 1}),
    )
    .await
    .unwrap();
    let text = extract_text(&markdown.value);
    assert!(text.contains("Long memory control fact"));
    assert!(text.contains(&"x".repeat(300)));
    assert!(!text.contains("..."));
    fixture.harness.shutdown().await;
}

// The `tracedecay_context` missing-required-argument case lives in
// `schema_test::schema_required_arguments_match_representative_handler_parsers`,
// which pairs it with the schema `required` array the handler parser is
// supposed to mirror instead of only asserting that *some* error came back.

#[tokio::test]
async fn test_context_scope_prefix_filters() {
    let fixture = setup_scoped_production_project("tests").await;
    let response = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_context",
            json!({"task": "test helper", "format": "json"}),
        )
        .await
        .unwrap();
    assert!(
        response.error.is_none(),
        "scoped context failed: {response:?}"
    );
    let result = response.result.expect("scoped context MCP result");
    let payload: Value = serde_json::from_str(extract_text(&result)).unwrap();
    let search_matches = payload["search_matches"]
        .as_array()
        .expect("context search matches");
    assert!(
        search_matches
            .iter()
            .any(|search_match| search_match["name"] == "test_helper"),
        "context should retain the matching primary-search evidence inside the configured scope: {payload}"
    );
    assert!(
        search_matches
            .iter()
            .all(|search_match| search_match["file"]
                .as_str()
                .is_some_and(|path| path.starts_with("tests/"))),
        "context primary-search evidence must honor the production handshake scope: {payload}"
    );
    fixture.harness.shutdown().await;
}
