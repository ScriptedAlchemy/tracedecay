//! `index.exclude.v1` and `index.include.v1` as an operator changes them:
//! `tracedecay_configuration_set` / `_unset` through a production MCP
//! `tools/call`, a daemon restart (both keys are restart-applied), then the
//! `tracedecay_files` census of what the code index now holds.
//!
//! `vendor/**` is in the shipped exclude defaults, so `vendor/kept/lib.rs` is
//! out of the index until `index.include.v1` re-admits it.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_first_json_content, extract_real_server_text,
    handle_real_server_tool_call, production_composition_fixture_with_sources, refusal_problem,
    wait_for_readiness,
};

const EXCLUDE_KEY: &str = "index.exclude.v1";
const INCLUDE_KEY: &str = "index.include.v1";

const LIB_RS: &str = "pub fn kept() {}\n";
const GEN_RS: &str = "pub fn generated_only() {}\n";
const VENDORED_RS: &str = "pub fn vendored_kept() {}\n";

fn lib() -> Value {
    json!({"path": "src/lib.rs", "symbols": 1, "bytes": 17})
}

fn generated() -> Value {
    json!({"path": "generated-fixtures/gen.rs", "symbols": 1, "bytes": 27})
}

fn vendored() -> Value {
    json!({"path": "vendor/kept/lib.rs", "symbols": 1, "bytes": 26})
}

fn census(files: Vec<Value>) -> Value {
    json!({"count": files.len(), "layout": "grouped", "files": files, "freshness": {"state": "fresh"}})
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("fixture directory");
    fs::write(path, contents).expect("fixture file");
}

/// Wait for the restarted owner's current generation, then read the census.
async fn indexed(fixture: &ProductionCompositionFixture) -> (Value, String) {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    let status = wait_for_readiness(&server, "ready", Duration::from_secs(60)).await;
    let generation = status["code_index_freshness"]["worktree"]["latest_generation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("ready status names no generation: {status}"))
        .to_owned();
    let response = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_files",
            json!({"format": "json"}),
        )
        .await
        .expect("production MCP answers tracedecay_files");
    assert!(response.error.is_none(), "{:?}", response.error);
    let listing = extract_first_json_content(response.result.as_ref().expect("files result"));
    (listing, generation)
}

async fn mutate(fixture: &ProductionCompositionFixture, tool: &str, arguments: Value) {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    let result = handle_real_server_tool_call(&server, tool, arguments).await;
    let envelope: Value = serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"));
    assert_eq!(result["isError"], Value::Null, "{envelope}");
    assert_eq!(
        envelope["outcome"]["outcome"],
        json!("effect"),
        "{envelope}"
    );
}

async fn set(fixture: &ProductionCompositionFixture, key: &str, patterns: &[&str], step: &str) {
    let layer = project_layer(fixture).await;
    let expected_revision = revision(fixture).await;
    mutate(
        fixture,
        "tracedecay_configuration_set",
        json!({
            "layer": layer,
            "key": key,
            "value": {"kind": "string_list", "value": patterns},
            "expected_revision": expected_revision,
            "idempotency_key": format!("configuration.idempotency.index-paths-{step}"),
        }),
    )
    .await;
}

async fn unset(fixture: &ProductionCompositionFixture, key: &str, step: &str) {
    let layer = project_layer(fixture).await;
    let expected_revision = revision(fixture).await;
    mutate(
        fixture,
        "tracedecay_configuration_unset",
        json!({
            "layer": layer,
            "key": key,
            "expected_revision": expected_revision,
            "idempotency_key": format!("configuration.idempotency.index-paths-{step}"),
        }),
    )
    .await;
}

async fn project_layer(fixture: &ProductionCompositionFixture) -> Value {
    let project_id = fixture
        .harness
        .project_id(&fixture.project_root)
        .await
        .expect("registered fixture project");
    json!({"kind": "project", "project_id": project_id})
}

async fn revision(fixture: &ProductionCompositionFixture) -> String {
    fixture
        .harness
        .configuration_revision(&fixture.project_root)
        .await
        .expect("configuration revision")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn index_path_settings_republish_exactly_the_matching_files() {
    let mut fixture = production_composition_fixture_with_sources(|root| {
        write(root, "src/lib.rs", LIB_RS);
        write(root, "generated-fixtures/gen.rs", GEN_RS);
        write(root, "vendor/kept/lib.rs", VENDORED_RS);
    })
    .await;

    let (initial, initial_generation) = indexed(&fixture).await;
    assert_eq!(
        initial,
        census(vec![generated(), lib()]),
        "shipped defaults"
    );

    fixture = fixture.reopen().await;
    let (unchanged, unchanged_generation) = indexed(&fixture).await;
    assert_eq!(unchanged, census(vec![generated(), lib()]));
    assert_eq!(
        unchanged_generation, initial_generation,
        "a restart under unchanged settings must not republish"
    );

    set(
        &fixture,
        EXCLUDE_KEY,
        &["vendor/**", "generated-fixtures/**"],
        "exclude",
    )
    .await;
    fixture = fixture.reopen().await;
    let (excluded, excluded_generation) = indexed(&fixture).await;
    assert_eq!(excluded, census(vec![lib()]), "generated-fixtures excluded");
    assert_ne!(excluded_generation, initial_generation);

    set(&fixture, INCLUDE_KEY, &["vendor/kept/**"], "include").await;
    fixture = fixture.reopen().await;
    let (included, _) = indexed(&fixture).await;
    assert_eq!(
        included,
        census(vec![lib(), vendored()]),
        "include re-admits vendor/kept while the exclude list still drops generated-fixtures"
    );

    unset(&fixture, EXCLUDE_KEY, "unset-exclude").await;
    unset(&fixture, INCLUDE_KEY, "unset-include").await;
    fixture = fixture.reopen().await;
    let (restored, _) = indexed(&fixture).await;
    assert_eq!(
        restored,
        census(vec![generated(), lib()]),
        "unset restores the shipped defaults"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_index_pattern_is_refused_before_it_is_committed() {
    let fixture = production_composition_fixture_with_sources(|root| {
        write(root, "src/lib.rs", LIB_RS);
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    let before = revision(&fixture).await;
    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_configuration_set",
        json!({
            "layer": project_layer(&fixture).await,
            "key": EXCLUDE_KEY,
            "value": {"kind": "string_list", "value": ["src/[abc"]},
            "expected_revision": before,
            "idempotency_key": "configuration.idempotency.index-paths-malformed",
        }),
    )
    .await;
    // `index.exclude.v1` is a sensitive setting, so the refusal names its
    // class and never echoes the submitted value.
    let problem = refusal_problem(&result);
    assert_eq!(problem["kind"], "invalid_request", "{result}");
    assert_eq!(problem["code"], "configuration.invalid_request", "{result}");
    assert_eq!(
        revision(&fixture).await,
        before,
        "a refused pattern must not write a revision"
    );
    fixture.shutdown().await;
}
