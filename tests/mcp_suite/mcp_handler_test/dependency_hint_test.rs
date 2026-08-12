#![cfg(feature = "test-transport")]

mod lazy_cutover;

use crate::support::*;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;

struct ScopedDependencyHintFixture {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: std::path::PathBuf,
    _isolation: TestTempDir,
}

fn write_dependency_declaration(project: &Path, declarations: &str) {
    fs::create_dir_all(project.join("node_modules/pkg")).unwrap();
    fs::write(project.join(".gitignore"), "node_modules/\n").unwrap();
    fs::write(project.join("node_modules/pkg/index.d.ts"), declarations).unwrap();
}

fn initialize_git_repository(project: &Path) {
    let init = Command::new(crate::common::git_program())
        .args(["init", "-q"])
        .current_dir(project)
        .status()
        .expect("git init dependency-hint fixture");
    assert!(init.success(), "git init must succeed");
    let add = Command::new(crate::common::git_program())
        .args(["add", "."])
        .current_dir(project)
        .status()
        .expect("git add dependency-hint fixture");
    assert!(add.success(), "git add must succeed");
    let commit = Command::new(crate::common::git_program())
        .args([
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "dependency hint fixture",
        ])
        .current_dir(project)
        .status()
        .expect("git commit dependency-hint fixture");
    assert!(commit.success(), "git commit must succeed");
}

async fn scoped_dependency_hint_fixture(
    scope_prefix: &str,
    write_sources: impl FnOnce(&Path),
) -> ScopedDependencyHintFixture {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    write_sources(&project_root);
    initialize_git_repository(&project_root);
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
    for _ in 0..60 {
        let payload = search_payload(server, arguments.clone()).await;
        if payload["reason"].as_str() != Some("authority_unavailable") {
            return payload;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("code-index search authority did not activate within the polling budget");
}

fn hint_candidates(payload: &Value) -> &[Value] {
    payload["external_import_hint"]["candidates"]
        .as_array()
        .expect("parser-backed external import candidates")
}

#[tokio::test]
async fn test_search_reports_parser_backed_external_import_hint_without_mutating_generation() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        write_dependency_declaration(project, "export interface SparseWidget { value: string }\n");
        fs::write(
            project.join("src/app.ts"),
            r#"import type { SparseWidget as ExternalSparseWidget } from "pkg";
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

    let sparse = search_payload(&server, json!({"query": "SparseWidget", "limit": 5})).await;
    assert!(
        sparse["results"].as_array().is_some_and(|results| {
            results
                .iter()
                .any(|result| result["display"]["name"] == "SparseWidgetHelper")
        }),
        "the successful sparse lexical result must be preserved: {sparse}"
    );
    assert_eq!(
        hint_candidates(&sparse),
        &[json!({
            "module": "pkg",
            "symbol": "SparseWidget",
            "import_file": "src/app.ts",
            "line": 1,
        })]
    );
    assert_eq!(
        sparse["external_import_hint"]["resolution_status"],
        "unverified"
    );
    assert_eq!(
        sparse["external_import_hint"]["evidence"],
        "parser_external_module_specifier"
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
async fn test_search_external_import_hint_does_not_claim_project_alias_resolution() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/app.ts"),
            r#"import type { AliasWidget } from "@/types";
export function AliasWidgetHelper() { return 1; }
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
        wait_for_search_payload(&server, json!({"query": "AliasWidget", "limit": 5})).await;
    assert_eq!(
        hint_candidates(&payload),
        &[json!({
            "module": "@/types",
            "symbol": "AliasWidget",
            "import_file": "src/app.ts",
            "line": 1,
        })]
    );
    assert_eq!(
        payload["external_import_hint"]["evidence"],
        "parser_external_module_specifier"
    );
    assert_eq!(
        payload["external_import_hint"]["resolution_status"],
        "unverified"
    );
    let message = payload["external_import_hint"]["message"]
        .as_str()
        .expect("truthful external import message");
    assert!(message.contains("not verified"));
    assert!(!message.contains("ignored dependency"));
    assert!(!message.contains("node_modules"));

    let markdown = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_search",
        json!({"query": "AliasWidget", "limit": 5, "format": "markdown"}),
    )
    .await;
    assert!(
        markdown["error"].is_null(),
        "alias advisory markdown: {markdown}"
    );
    let markdown = extract_real_server_text(&markdown["result"]);
    assert!(markdown.contains("is imported from external-module specifier"));
    assert!(!markdown.contains("exports `AliasWidget`"));
    assert!(!markdown.contains("ignored dependency"));
    assert!(!markdown.contains("node_modules"));
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn test_search_external_import_hint_respects_scope_before_limit() {
    let fixture = scoped_dependency_hint_fixture("src", |project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(project.join("outside")).unwrap();
        write_dependency_declaration(
            project,
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
        "a full result page must skip the parser-backed advisory read: {payload}"
    );
    fixture.harness.shutdown().await;
}
