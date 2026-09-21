#![cfg(feature = "test-transport")]

use crate::support::*;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::Duration;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;

struct ScopedDependencyHintFixture {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: std::path::PathBuf,
    _isolation: TestTempDir,
}

fn write_dependency_declaration(project: &Path, module: &str, declarations: &str) {
    fs::create_dir_all(project.join("node_modules").join(module)).unwrap();
    fs::write(project.join(".gitignore"), "node_modules/\n").unwrap();
    fs::write(
        project.join("node_modules").join(module).join("index.d.ts"),
        declarations,
    )
    .unwrap();
}

async fn scoped_dependency_hint_fixture(
    scope_prefix: &str,
    write_sources: impl FnOnce(&Path),
) -> ScopedDependencyHintFixture {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    write_sources(&project_root);
    commit_worktree(&project_root, "dependency hint fixture");
    let harness = ProductionProjectCompositionHarnessV1::open_with_scope_prefix(
        isolation.path(),
        [project_root.clone()],
        scope_prefix,
    )
    .await
    .expect("scoped production dependency-hint harness");
    ScopedDependencyHintFixture {
        harness,
        project_root,
        _isolation: isolation,
    }
}

async fn search_payload(server: &McpServer, arguments: Value) -> Value {
    let response = handle_real_server_tool_call_raw(server, "tracedecay_search", arguments).await;
    assert!(
        response["error"].is_null(),
        "search must preserve its successful lexical result: {response}"
    );
    serde_json::from_str(extract_real_server_text(&response["result"]))
        .expect("dependency-hint search response JSON")
}

async fn wait_for_search_payload(server: &McpServer, arguments: Value) -> Value {
    let mut last = Value::Null;
    for _ in 0..60 {
        let payload = search_payload(server, arguments.clone()).await;
        if !matches!(
            payload["reason"].as_str(),
            Some(
                "authority_unavailable"
                    | "generation_unavailable"
                    | "generation_unverified"
                    | "search_capacity_unavailable"
            )
        ) {
            return payload;
        }
        last = payload;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("code-index search authority did not activate within the polling budget: {last}");
}

async fn wait_for_named_search_payload(
    server: &McpServer,
    arguments: Value,
    expected_name: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..60 {
        let payload = search_payload(server, arguments.clone()).await;
        if payload["results"].as_array().is_some_and(|results| {
            results
                .iter()
                .any(|result| result["display"]["name"] == expected_name)
        }) {
            return payload;
        }
        last = payload;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("named code-index result did not activate within the polling budget: {last}");
}

fn hint_candidates(payload: &Value) -> &[Value] {
    payload["external_import_hint"]["candidates"]
        .as_array()
        .expect("parser-backed external import candidates")
}

#[tokio::test]
async fn test_search_reports_unresolved_external_import_hint_without_mutating_generation() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        write_dependency_declaration(
            project,
            "SparseWidgetHelperDependency",
            "export interface SparseWidget { value: string }\n",
        );
        fs::write(
            project.join("src/app.ts"),
            r#"import type { SparseWidget as ExternalSparseWidget } from "SparseWidgetHelperDependency";
export function SparseWidgetHelper() { return 1; }
export function GenerationAnchor() { return 2; }
"#,
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let before =
        wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;

    let sparse = wait_for_named_search_payload(
        &server,
        json!({"query": "SparseWidgetHelper", "limit": 5}),
        "SparseWidgetHelper",
    )
    .await;
    assert!(
        sparse["results"].as_array().is_some_and(|results| {
            results.iter().any(|result| {
                result["display"]["name"] == "SparseWidgetHelper"
                    && result["display"]["path"] == "src/app.ts"
            })
        }),
        "the successful sparse lexical symbol result must be preserved: {sparse}"
    );
    assert_eq!(
        hint_candidates(&sparse),
        &[json!({
            "module": "SparseWidgetHelperDependency",
            "symbol": "SparseWidget",
            "import_file": "src/app.ts",
            "line": 1,
        })]
    );
    assert_eq!(
        sparse["external_import_hint"]["suggested_action"],
        "verify_external_import_before_lazy_indexing"
    );
    assert_eq!(sparse["code_generation"], before["code_generation"]);

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;
        assert_eq!(after["code_generation"], before["code_generation"]);

        let exact = handle_real_server_tool_call_raw(
            &server,
            "tracedecay_find_exact_symbol",
            json!({"name": "SparseWidget", "limit": 5}),
        )
        .await;
        assert!(
            exact["error"].is_null(),
            "the read-only hint must not disturb exact reads: {exact}"
        );
        let exact: Value = serde_json::from_str(extract_real_server_text(&exact["result"]))
            .expect("post-hint exact-symbol response JSON");
        assert_eq!(
            exact["count"], 0,
            "automatic hinting must not index the ignored dependency: {exact}"
        );
    }
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn test_search_external_import_hint_respects_scope_before_limit() {
    let fixture = scoped_dependency_hint_fixture("src", |project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(project.join("outside")).unwrap();
        write_dependency_declaration(
            project,
            "pkg",
            "export interface ScopedDependency { value: string }\n",
        );
        for index in 0..8 {
            fs::write(
                project.join(format!("outside/{index:02}.ts")),
                format!(
                    "import type {{ ScopedDependency }} from \"pkg\";\nexport const outside{index} = {index};\n"
                ),
            )
            .unwrap();
        }
        fs::write(
            project.join("src/inside.ts"),
            r#"import type { ScopedDependency } from "pkg";
export function GenerationAnchor() { return 1; }
"#,
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("scoped production project server");
    wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;

    let payload = search_payload(&server, json!({"query": "ScopedDependency", "limit": 1})).await;
    assert_eq!(payload["scope_prefix"], "src");
    assert_eq!(
        hint_candidates(&payload),
        &[json!({
            "module": "pkg",
            "symbol": "ScopedDependency",
            "import_file": "src/inside.ts",
            "line": 1,
        })]
    );
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn test_search_skips_external_import_hint_when_results_fill_limit() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        write_dependency_declaration(
            project,
            "pkg",
            "export interface IndexedAnchor { value: string }\n",
        );
        fs::write(
            project.join("src/app.ts"),
            r#"import type { IndexedAnchor as DependencyAnchor } from "pkg";
export function IndexedAnchor() { return 1; }
"#,
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let payload =
        wait_for_search_payload(&server, json!({"query": "IndexedAnchor", "limit": 1})).await;
    assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["results"][0]["display"]["name"], "IndexedAnchor");
    assert!(
        payload["external_import_hint"].is_null(),
        "a full result page must skip the verified advisory read: {payload}"
    );
    fixture.harness.shutdown().await;
}

const GENERATION_ADVANCED_REASON: &str =
    "application.symbol-graph.ignored-dependency-generation-advanced";

enum DependencyImportStyle {
    Named,
    Default,
}

async fn lazy_dependency_fixture(
    symbol: &str,
    import_style: DependencyImportStyle,
) -> ProductionCompositionFixture {
    let (declaration, import) = match import_style {
        DependencyImportStyle::Named => (
            format!("export interface {symbol} {{ value: string }}\n"),
            format!("import type {{ {symbol} }} from \"pkg\";\n"),
        ),
        DependencyImportStyle::Default => (
            format!("export default interface {symbol} {{ value: string }}\n"),
            format!("import type {symbol} from \"pkg\";\n"),
        ),
    };
    production_composition_fixture_with_sources(move |project| {
        fs::create_dir_all(project.join("src")).unwrap();
        write_dependency_declaration(project, "pkg", &declaration);
        fs::write(
            project.join("src/app.ts"),
            "export function GenerationAnchor() { return 1; }\n",
        )
        .unwrap();
        fs::write(project.join("src/dependency-types.ts"), import).unwrap();
    })
    .await
}

fn assert_generation_advanced_retry(response: &Value) {
    assert!(
        response["result"].is_null(),
        "the generation-advancing call must not return a same-call symbol payload: {response}"
    );
    assert_eq!(
        response["error"]["data"]["reason_code"].as_str(),
        Some(GENERATION_ADVANCED_REASON),
        "lazy admission must expose the canonical usecase retry reason: {response}"
    );
    assert_eq!(response["error"]["data"]["retryable"], true);
}

fn code_generation(payload: &Value) -> &str {
    payload["code_generation"]
        .as_str()
        .unwrap_or_else(|| panic!("search response code generation: {payload}"))
}

async fn exact_symbol_payload(server: &McpServer, arguments: Value) -> Value {
    let response =
        handle_real_server_tool_call_raw(server, "tracedecay_find_exact_symbol", arguments).await;
    assert!(
        response["error"].is_null(),
        "exact-symbol read must succeed: {response}"
    );
    serde_json::from_str(extract_real_server_text(&response["result"]))
        .expect("exact-symbol response JSON")
}

#[tokio::test]
async fn exact_symbol_explicit_lazy_admission_advances_generation_then_retry_finds_symbol_once() {
    let fixture =
        lazy_dependency_fixture("ExactOnlyDependency", DependencyImportStyle::Named).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let before =
        wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;

    let zero =
        exact_symbol_payload(&server, json!({"name": "ExactOnlyDependency", "limit": 5})).await;
    assert_eq!(
        zero["count"], 0,
        "the ignored dependency must be absent before explicit admission: {zero}"
    );
    let after_zero =
        wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;
    assert_eq!(code_generation(&after_zero), code_generation(&before));

    let arguments = json!({
        "name": "ExactOnlyDependency",
        "limit": 5,
        "lazy_index_ignored_dependencies": true
    });
    let first = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_find_exact_symbol",
        arguments.clone(),
    )
    .await;
    assert_generation_advanced_retry(&first);

    let after =
        wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;
    assert_ne!(
        code_generation(&after),
        code_generation(&before),
        "the scheduler must publish a new serving generation before requesting a retry"
    );

    let payload = exact_symbol_payload(&server, arguments).await;
    assert_eq!(payload["count"], 1, "retry must find one symbol: {payload}");
    let matches = payload["matches"]
        .as_array()
        .expect("exact-symbol retry matches");
    assert_eq!(matches.len(), 1, "retry must find one symbol: {payload}");
    assert_eq!(
        matches[0]["name"], "ExactOnlyDependency",
        "retry must return the requested dependency symbol: {payload}"
    );
    assert_eq!(
        matches[0]["file"], "node_modules/pkg/index.d.ts",
        "retry must return the ignored dependency declaration: {payload}"
    );
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn search_explicit_lazy_admission_advances_generation_then_retry_finds_dependency_chunk_once()
{
    let fixture =
        lazy_dependency_fixture("SearchOnlyDependency", DependencyImportStyle::Default).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let before =
        wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;

    let zero = search_payload(&server, json!({"query": "default", "limit": 5})).await;
    assert_eq!(
        zero["results"],
        json!([]),
        "the ignored dependency must be absent before explicit admission: {zero}"
    );
    let after_zero =
        wait_for_search_payload(&server, json!({"query": "GenerationAnchor", "limit": 1})).await;
    assert_eq!(code_generation(&after_zero), code_generation(&before));

    let arguments = json!({
        "query": "default",
        "limit": 5,
        "lazy_index_ignored_dependencies": true
    });
    let first = search_payload(&server, arguments.clone()).await;
    assert_eq!(
        first["results"],
        json!([]),
        "generation-advancing admission must preserve the successful lexical result: {first}"
    );
    assert_eq!(
        code_generation(&first),
        code_generation(&before),
        "the current response remains bound to the generation that produced its lexical result"
    );

    let retry = wait_for_search_payload(&server, arguments.clone()).await;
    assert_ne!(
        code_generation(&retry),
        code_generation(&before),
        "the retry must bind the scheduler-published generation"
    );
    let results = retry["results"].as_array().expect("search retry results");
    assert_eq!(
        results.len(),
        1,
        "retry must find one dependency chunk: {retry}"
    );
    let result = &results[0];
    let anchor = result["candidate"]["anchor_id"]
        .as_str()
        .expect("search retry candidate anchor");
    assert!(
        anchor.starts_with("code-chunk:"),
        "the default keyword must bind the admitted dependency chunk: {retry}"
    );
    let chunk_id = anchor
        .strip_prefix("code-chunk:")
        .expect("search retry chunk anchor");
    let expected_source = format!("code-chunk:{}:{chunk_id}", code_generation(&retry));
    assert!(
        result["candidate"]["occurrences"]
            .as_array()
            .is_some_and(|occurrences| occurrences.iter().any(|occurrence| {
                occurrence["source_occurrence_id"].as_str() == Some(expected_source.as_str())
                    && occurrence["file_occurrence_id"]
                        .as_str()
                        .is_some_and(|file| !file.is_empty())
            })),
        "the returned chunk must retain its exact generation, chunk, and file occurrence: {retry}"
    );
    let stable = wait_for_search_payload(&server, arguments).await;
    assert_eq!(
        code_generation(&stable),
        code_generation(&retry),
        "a positive retry must not schedule another generation: {stable}"
    );
    assert_eq!(
        stable["results"].as_array().map(Vec::len),
        Some(1),
        "a positive retry must continue returning exactly one result: {stable}"
    );
    fixture.harness.shutdown().await;
}
