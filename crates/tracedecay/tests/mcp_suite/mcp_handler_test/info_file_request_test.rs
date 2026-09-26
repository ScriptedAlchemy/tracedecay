//! The file inspections (`tracedecay_files`, `tracedecay_config`) decode their
//! arguments against a typed request over the production MCP `tools/call`
//! path: an argument outside the request contract is refused instead of being
//! silently defaulted or ignored.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

fn write_probe_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.4.2\"\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn probe() -> i32 {\n    1\n}\n",
    )
    .unwrap();
}

async fn call_json(
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
        response.error.is_none(),
        "{tool_name} returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result"));
    extract_text(&result).to_owned()
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
        "{tool_name} must refuse the request, answered {:?}",
        response.result
    );
    response
        .error
        .unwrap_or_else(|| panic!("{tool_name} must refuse the request"))
        .message
}

#[tokio::test]
async fn file_inspections_refuse_arguments_outside_their_typed_request() {
    let fixture = production_composition_fixture_with_sources(write_probe_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production file-inspection server");
    wait_for_current_graph(&server).await;

    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_files",
            json!({"path": "src", "layout": "flat", "format": "json"}),
        )
        .await,
        r#"{"count":1,"files":[{"bytes":32,"path":"src/lib.rs","symbols":1}],"layout":"flat"}"#
    );
    assert_eq!(
        refusal(&fixture, "tracedecay_files", json!({"layout": "tree"})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_files: unknown variant `tree`, expected `flat` or `grouped`"
    );
    assert_eq!(
        refusal(&fixture, "tracedecay_files", json!({"patern": "*.rs"})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_files: unknown field `patern`, expected one of `path`, `pattern`, `layout`"
    );

    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_config",
            json!({"key": "package.version", "path": "Cargo.toml", "format": "json"}),
        )
        .await,
        r#"{"match_count":1,"matches":[{"file":"Cargo.toml","key":"package.version","line":3,"value":"0.4.2"}]}"#
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_config",
            json!({"key": 1, "path": "Cargo.toml"})
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_config: invalid type: integer `1`, expected a string"
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_config",
            json!({"key": "package.version", "paths": "Cargo.toml"})
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_config: unknown field `paths`, expected one of `key`, `path`, `glob`"
    );

    fixture.harness.shutdown().await;
}
