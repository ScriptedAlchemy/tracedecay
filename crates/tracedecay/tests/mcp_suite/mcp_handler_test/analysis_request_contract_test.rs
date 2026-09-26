//! Analysis reports decode their arguments against the typed request over the
//! production MCP `tools/call` path.
//!
//! `src/a.rs::one` calls `src/b.rs::two`, the only cross-file relation, and
//! `two` holds the only `unwrap()` site. An argument outside a report's typed
//! request is refused instead of being defaulted or ignored.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_json, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

fn write_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).expect("src dir");
    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").expect("lib.rs");
    fs::write(
        project.join("src/a.rs"),
        "use crate::b::two;\n\npub fn one() -> u32 {\n    two()\n}\n",
    )
    .expect("a.rs");
    fs::write(
        project.join("src/b.rs"),
        "pub fn two() -> u32 {\n    Some(2).unwrap()\n}\n",
    )
    .expect("b.rs");
}

async fn report(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool_name}: {:?}",
        response.error
    );
    extract_json(&response.result.expect("tools/call result"))
}

async fn refusal(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> String {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(
        response.result.is_none(),
        "{tool_name}: {:?}",
        response.result
    );
    response
        .error
        .unwrap_or_else(|| panic!("{tool_name} must refuse the request"))
        .message
}

#[tokio::test]
async fn analysis_reports_refuse_arguments_outside_their_typed_request() {
    let fixture = production_composition_fixture_with_sources(write_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production analysis server");
    wait_for_current_graph(&server).await;
    drop(server);

    assert_eq!(
        report(
            &fixture,
            "tracedecay_coupling",
            json!({"direction": "fan_in", "limit": 1, "format": "json"}),
        )
        .await,
        json!({
            "direction": "fan_in",
            "result_count": 1,
            "ranking": [{"file": "src/b.rs", "coupled_files": 1}],
        })
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_coupling",
            json!({"direction": "fan_in", "limit": "1"}),
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_coupling: invalid type: string \"1\", expected u32"
    );

    assert_eq!(
        report(
            &fixture,
            "tracedecay_unsafe_patterns",
            json!({"kinds": ["unwrap"], "format": "json"}),
        )
        .await,
        json!({
            "match_count": 1,
            "by_kind": {"unwrap": 1},
            "matches": [{
                "kind": "unwrap",
                "file": "src/b.rs",
                "line": 2,
                "snippet": "Some(2).unwrap()",
                "enclosing": "src/b.rs::two",
                "in_test": false,
            }],
        })
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_unsafe_patterns",
            json!({"kinds": ["unwraps"]}),
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_unsafe_patterns: unknown variant `unwraps`, expected one of `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, `unsafe_block`"
    );

    assert_eq!(
        report(
            &fixture,
            "tracedecay_distribution",
            json!({"path": "src/b.rs", "summary": true, "format": "json"}),
        )
        .await,
        json!({
            "path_filter": "src/b.rs",
            "mode": "summary",
            "total_kinds": 1,
            "distribution": [{"kind": "function", "count": 1}],
        })
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_distribution",
            json!({"path": "src/b.rs", "summary": "yes"}),
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_distribution: invalid type: string \"yes\", expected a boolean"
    );

    fixture.harness.shutdown().await;
}
