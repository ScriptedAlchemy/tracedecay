#![cfg(feature = "test-transport")]

use crate::support::*;
use serde_json::{Value, json};
use std::fs;
use std::process::Command;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::errors::{Result as TraceDecayResult, TraceDecayError};
use tracedecay::mcp::ToolResult;

struct ScopedProductionContextFixture {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: std::path::PathBuf,
    _isolation: TestTempDir,
}

async fn setup_production_project() -> ProductionCompositionFixture {
    production_composition_fixture().await
}

async fn setup_production_generated_dir_project() -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(project.join("dist")).unwrap();
        fs::write(project.join("src/lib.rs"), "pub fn kept() {}\n").unwrap();
        fs::write(
            project.join("dist/generated.js"),
            "export function generatedOnly() {}\n",
        )
        .unwrap();
    })
    .await
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
    ScopedProductionContextFixture {
        harness,
        project_root,
        _isolation: isolation,
    }
}

async fn call_production_tool(
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
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| {
            panic!("{tool_name} omitted the retained application payload: {response}")
        })
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
        json!({"task": "generatedOnly", "max_nodes": 5}),
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("### Index Coverage Hint"),
        "context miss should include coverage hint, got: {text}"
    );
    assert!(
        text.contains("tracedecay sync --include-folder dist"),
        "hint should include opt-in command, got: {text}"
    );
    fixture.harness.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2. tracedecay_context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_context() {
    let fixture = setup_production_project().await;
    let result = call_production_tool(
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
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production context MCP server");
    let cg = server.cg().await;
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

    let markdown_result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "helper function durable memory review"}),
        None,
        None,
    )
    .await
    .unwrap();
    let markdown = extract_text(&markdown_result.value);
    assert!(markdown.contains("### Memory Matches"));
    assert!(markdown.contains(&format!("fact_id={fact_id}")));
    assert!(markdown.contains("Helper function reviews should check durable memory"));

    let json_result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "helper function durable memory review", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&json_result.value)).unwrap();
    assert!(
        payload.get("context_memory_analytics").is_none(),
        "internal context analytics must not be serialized in direct tool payloads"
    );
    assert!(
        json_result.internal_analytics().is_some(),
        "direct tool results should carry context analytics on the internal side channel"
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

// ---------------------------------------------------------------------------
// Extra: missing required params for other handlers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_context_missing_task() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_context", json!({}), None, None).await;
    assert!(result.is_err(), "context without task should error");
}

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
    let symbols = payload["symbols"].as_array().expect("context symbols");
    assert!(
        !symbols.is_empty(),
        "context should retain matching symbols inside the configured scope: {payload}"
    );
    assert!(
        symbols.iter().all(|symbol| symbol["file"]
            .as_str()
            .is_some_and(|path| path.starts_with("tests/"))),
        "context entry points must honor the production handshake scope: {payload}"
    );
    fixture.harness.shutdown().await;
}
