#![cfg(feature = "test-transport")]

mod graph_readiness;

use crate::common::fixture::git_run;
use crate::support::*;
use graph_readiness::{find_node_id, wait_for_current_graph};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::tracedecay::TraceDecay;
use tracedecay_domain::errors::{Result as TraceDecayResult, TraceDecayError};
use tracedecay_mcp::ToolResult;
use tracedecay_runtime_core::storage::resolve_layout_for_current_profile;

struct MountedProductionProject {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: std::path::PathBuf,
}

trait AnalysisToolHost {
    async fn call_analysis_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        server_stats: Option<Value>,
        scope_prefix: Option<&str>,
    ) -> TraceDecayResult<ToolResult>;

    async fn close_analysis_host(self)
    where
        Self: Sized;
}

async fn call_production_tool(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    tool_name: &str,
    mut arguments: Value,
) -> TraceDecayResult<ToolResult> {
    if !tracedecay_mcp::tool_defaults_to_markdown(tool_name)
        && let Some(arguments) = arguments.as_object_mut()
    {
        arguments
            .entry("format".to_owned())
            .or_insert_with(|| json!("json"));
    }
    let response = harness
        .call_tool(project_root, tool_name, arguments)
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

impl AnalysisToolHost for ProductionCompositionFixture {
    async fn call_analysis_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        _server_stats: Option<Value>,
        _scope_prefix: Option<&str>,
    ) -> TraceDecayResult<ToolResult> {
        call_production_tool(&self.harness, &self.project_root, tool_name, arguments).await
    }

    async fn close_analysis_host(self) {
        self.harness.shutdown().await;
    }
}

impl AnalysisToolHost for MountedProductionProject {
    async fn call_analysis_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        _server_stats: Option<Value>,
        _scope_prefix: Option<&str>,
    ) -> TraceDecayResult<ToolResult> {
        call_production_tool(&self.harness, &self.project_root, tool_name, arguments).await
    }

    async fn close_analysis_host(self) {
        self.harness.shutdown().await;
    }
}

impl AnalysisToolHost for TestTraceDecay {
    async fn call_analysis_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        server_stats: Option<Value>,
        scope_prefix: Option<&str>,
    ) -> TraceDecayResult<ToolResult> {
        crate::support::handle_tool_call(self, tool_name, arguments, server_stats, scope_prefix)
            .await
    }

    async fn close_analysis_host(self) {
        self.close().await;
    }
}

async fn handle_tool_call(
    host: &impl AnalysisToolHost,
    tool_name: &str,
    arguments: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
) -> TraceDecayResult<ToolResult> {
    host.call_analysis_tool(tool_name, arguments, server_stats, scope_prefix)
        .await
}

async fn close_test_graph(host: impl AnalysisToolHost) {
    host.close_analysis_host().await;
}

async fn setup_project() -> (ProductionCompositionFixture, ()) {
    (production_composition_fixture().await, ())
}

async fn setup_empty_analysis_project() -> (ProductionCompositionFixture, (), ()) {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), "").unwrap();
    })
    .await;
    (fixture, (), ())
}

fn write_integration_test_risk_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"risk_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod api;\n").unwrap();
    fs::write(
        project.join("src/api.rs"),
        "pub fn public_entry() -> String { format_greeting(\"world\") }\n\
         pub fn unused_public_api() -> String { \"unused\".to_string() }\n\
         fn format_greeting(name: &str) -> String { format!(\"Hello, {}!\", name) }\n",
    )
    .unwrap();
    fs::write(
        project.join("tests/integration_api.rs"),
        "use risk_fixture::api::public_entry;\n\
         #[test]\nfn integration_public_entry() {\n    assert_eq!(public_entry(), \"Hello, world!\");\n}\n",
    )
    .unwrap();
}

async fn setup_integration_test_risk_project() -> (ProductionCompositionFixture, ()) {
    let fixture =
        production_composition_fixture_with_sources(write_integration_test_risk_sources).await;
    (fixture, ())
}

async fn setup_test_risk_non_src_fixture() -> (ProductionCompositionFixture, ()) {
    let fixture = production_composition_fixture_with_sources(|project| {
        write_integration_test_risk_sources(project);
        fs::write(
            project.join("build.rs"),
            "fn build_script_helper(flag: &str) -> String { format!(\"cargo:warning={flag}\") }\n\
             fn main() { println!(\"{}\", build_script_helper(\"ok\")); }\n",
        )
        .unwrap();
    })
    .await;
    (fixture, ())
}

async fn setup_ts_describe_it_project() -> (ProductionCompositionFixture, ()) {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("package.json"),
            "{\"name\":\"ts-describe-it-fixture\",\"version\":\"0.1.0\"}\n",
        )
        .unwrap();
        fs::write(
            project.join("src/math.ts"),
            "export function add(a: number, b: number): number { return a + b; }\n",
        )
        .unwrap();
        fs::write(
            project.join("src/math.test.ts"),
            "import { add } from \"./math\";\n\
             describe('math', () => { it('adds two numbers', () => { const result = add(1, 2); }); });\n",
        )
        .unwrap();
    })
    .await;
    (fixture, ())
}

async fn setup_unsafe_block_fixture() -> (ProductionCompositionFixture, ()) {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"unsafe_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            project.join("src/lib.rs"),
            r#"
/// Reinterpret a total as a `usize` through a raw-pointer read. There is no
/// memory-safety reason for this to be `unsafe` — exactly the needless kind a
/// safety audit should flag.
pub fn raw_total_len(total: u64) -> usize {
    let ptr = &total as *const u64;
    unsafe { *ptr as usize }
}

/// A plainly safe function with no unsafe markers at all.
pub fn safe_add(a: u64, b: u64) -> u64 {
    a + b
}
"#,
        )
        .unwrap();
    })
    .await;
    (fixture, ())
}

async fn init_test_project(project: &Path) -> (MountedProductionProject, ()) {
    if !project.join(".git").is_dir() {
        git_run(project, &["init", "--quiet"]);
        git_run(project, &["add", "."]);
        git_run(
            project,
            &[
                "-c",
                "user.name=TraceDecay Tests",
                "-c",
                "user.email=tests@tracedecay.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
    }
    let isolation_root = project
        .parent()
        .expect("graph-analysis project must have an isolation parent");
    let harness =
        ProductionProjectCompositionHarnessV1::open(isolation_root, [project.to_path_buf()])
            .await
            .expect("production graph-analysis composition");
    let mounted = MountedProductionProject {
        harness,
        project_root: project.to_path_buf(),
    };
    wait_for_current_graph(&mounted).await;
    (mounted, ())
}

#[tokio::test]
async fn test_branch_list_reports_live_vs_serving_drift_state() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    let _env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = project.join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let _global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
    git_run(project, &["init"]);
    git_run(project, &["config", "user.email", "test@test.com"]);
    git_run(project, &["config", "user.name", "Test"]);
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", "initial"]);
    git_run(project, &["branch", "-M", "main"]);

    let _initialized = TestTraceDecay::new(TraceDecay::init(project).await.unwrap());
    let tracedecay_dir = resolve_layout_for_current_profile(project)
        .unwrap()
        .data_root;
    tracedecay_runtime_core::branch_meta::save_branch_meta(
        &tracedecay_dir,
        &tracedecay_runtime_core::branch_meta::BranchMeta::new("main"),
    )
    .unwrap();

    let cg = TestTraceDecay::new(TraceDecay::open(project).await.unwrap());
    git_run(project, &["checkout", "-b", "feature"]);

    // Branch drift diagnostics moved off `tracedecay_branch_list` (now the
    // paginated branch-ref snapshot read) to the active-project context's
    // `branch` block.
    let result = handle_tool_call(&cg, "tracedecay_active_project", json!({}), None, None)
        .await
        .unwrap();
    let report: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let branch = &report["branch"];
    assert_eq!(branch["current_branch"], json!("feature"));
    assert_eq!(branch["open_active_branch"], json!("main"));
    assert_eq!(branch["serving_branch"], json!("main"));
    assert_eq!(branch["branch_drifted"], json!(true));
    assert_eq!(branch["branch_resolution"], json!("stale_serving_branch"));
}

/// The shared fixture plants exactly four functions, and every one of them is
/// excluded from the default dead-code census for a *different* reason:
///
/// | symbol            | why it is not dead                                  |
/// |-------------------|-----------------------------------------------------|
/// | `main`            | entry-point name exclusion                          |
/// | `test_helper`     | `test`-prefixed and `#[test]`-annotated             |
/// | `helper`          | `pub`, and `include_public` defaults to false       |
/// | `format_greeting` | private, but `helper` calls it (incoming edge)      |
///
/// So the correct answer is an empty dead-code set. Resolving `format_greeting`
/// first is the anti-vacuity gate: it waits for the graph to become current and
/// panics unless that private, *called* symbol is in the census, which makes the
/// zero below a real negative result rather than an unpopulated index.
#[tokio::test]
async fn test_dead_code() {
    let (cg, _dir) = setup_project().await;
    let _populated = find_node_id(&cg, "format_greeting").await;

    let result = handle_tool_call(&cg, "tracedecay_dead_code", json!({}), None, None)
        .await
        .unwrap();
    let payload = extract_json(&result.value);
    let symbols = payload["symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("dead_code must return a symbols array: {payload}"));

    assert_eq!(
        payload["dead_code_count"].as_u64(),
        Some(0),
        "no fixture symbol qualifies as dead code: {payload}"
    );
    assert!(
        symbols.is_empty(),
        "dead_code_count and symbols must agree: {payload}"
    );
}

/// `src/utils.rs` holds `helper` and `format_greeting`, and `main` (in
/// `src/main.rs`) calls `helper`. A correct semantic diff therefore reports both
/// of the file's symbols as modified and `main` as impacted downstream.
#[tokio::test]
async fn test_diff_context() {
    let (cg, _dir) = setup_project().await;
    wait_for_current_graph(&cg).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_diff_context",
        json!({"files": ["src/utils.rs"]}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);

    assert_eq!(
        payload["changed_files"],
        json!(["src/utils.rs"]),
        "changed_files must echo the requested paths: {payload}"
    );

    let modified: Vec<&str> = payload["modified_symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("diff_context must return modified_symbols: {payload}"))
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    for expected in ["helper", "format_greeting"] {
        assert!(
            modified.contains(&expected),
            "every symbol defined in src/utils.rs must be reported modified, \
             missing `{expected}` in {modified:?}: {payload}"
        );
    }

    let impacted = payload["impacted_symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("diff_context must return impacted_symbols: {payload}"));
    assert_eq!(
        payload["impacted_symbols_count"].as_u64(),
        Some(impacted.len() as u64),
        "impacted_symbols_count must match the returned list: {payload}"
    );
    let impacted_names: Vec<&str> = impacted
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert!(
        impacted_names.contains(&"main"),
        "`main` calls `helper`, so it must appear downstream of a utils.rs change, \
         got {impacted_names:?}: {payload}"
    );
}

/// The fixture's file dependencies are strictly acyclic (`main.rs` imports
/// `utils.rs`; nothing imports back), so a correct analysis reports no cycles.
/// Resolving a symbol first proves the graph is populated, so the zero below is
/// a real "no cycles here" rather than "nothing was analysed".
#[tokio::test]
async fn test_circular() {
    let (cg, _dir) = setup_project().await;
    let _populated = find_node_id(&cg, "helper").await;

    let result = handle_tool_call(&cg, "tracedecay_circular", json!({}), None, None)
        .await
        .unwrap();
    let payload = extract_json(&result.value);

    assert_eq!(
        payload["cycle_count"].as_u64(),
        Some(0),
        "the acyclic fixture must not report dependency cycles: {payload}"
    );
    assert_eq!(payload["reported_cycle_count"].as_u64(), Some(0));
    assert_eq!(payload["omitted_cycle_count"].as_u64(), Some(0));
    assert!(
        payload["cycles"].as_array().is_some_and(Vec::is_empty),
        "cycle_count and cycles must agree: {payload}"
    );
}

#[tokio::test]
async fn test_rename_preview() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rename_preview",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);

    assert_eq!(payload["symbol"], json!("helper"), "payload: {payload}");
    assert_eq!(
        payload["node"]["name"],
        json!("helper"),
        "payload: {payload}"
    );
    assert_eq!(
        payload["node"]["file"],
        json!("src/utils.rs"),
        "payload: {payload}"
    );
    assert_eq!(
        payload["read_only"],
        json!(true),
        "rename_preview must never claim to have written: {payload}"
    );

    // `main` calls `helper`, so renaming it has at least that one real
    // reference to update.
    let references = payload["references"]
        .as_array()
        .unwrap_or_else(|| panic!("rename_preview must return references: {payload}"));
    assert_eq!(
        payload["reference_count"].as_u64(),
        Some(references.len() as u64),
        "reference_count must match the returned references: {payload}"
    );
    let referrers: Vec<&str> = references
        .iter()
        .filter_map(|reference| reference["from_name"].as_str())
        .collect();
    assert!(
        referrers.contains(&"main"),
        "`main` calls `helper`, so it must appear as a rename reference, \
         got {referrers:?}: {payload}"
    );
}

/// Both `use crate::utils::helper;` statements the fixture plants (in
/// `src/main.rs` and `tests/test_utils.rs`) are followed by a real `helper()`
/// call, so nothing is unused. `scanned_files` is the anti-vacuity signal: a
/// zero finding only means something if the scan actually inspected files.
#[tokio::test]
async fn test_unused_imports() {
    let (cg, _dir) = setup_project().await;
    wait_for_current_graph(&cg).await;
    let result = handle_tool_call(&cg, "tracedecay_unused_imports", json!({}), None, None)
        .await
        .unwrap();
    let payload = extract_json(&result.value);

    assert!(
        payload["scanned_files"].as_u64().is_some_and(|n| n > 0),
        "a zero finding is only meaningful if files were scanned: {payload}"
    );
    assert_eq!(
        payload["complete"],
        json!(true),
        "the fixture is far under the scan budget: {payload}"
    );
    assert_eq!(
        payload["unused_import_count"].as_u64(),
        Some(0),
        "every fixture import is used, so none may be flagged: {payload}"
    );
    assert!(
        payload["imports"].as_array().is_some_and(Vec::is_empty),
        "unused_import_count and imports must agree: {payload}"
    );
}

/// The fixture's call graph is a straight chain (`main` -> `helper` ->
/// `format_greeting`) with no symbol calling itself or looping back, so a
/// correct analysis finds no recursion. Resolving a symbol on that chain first
/// proves the call edges are present, which is what makes zero meaningful.
#[tokio::test]
async fn test_recursion() {
    let (cg, _dir) = setup_project().await;
    let _populated = find_node_id(&cg, "format_greeting").await;

    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
        .await
        .unwrap();
    let payload = extract_json(&result.value);

    assert_eq!(
        payload["cycle_count"].as_u64(),
        Some(0),
        "the non-recursive fixture must not report call cycles: {payload}"
    );
    assert!(
        payload["cycles"].as_array().is_some_and(Vec::is_empty),
        "cycle_count and cycles must agree: {payload}"
    );
}

#[tokio::test]
async fn test_changelog_no_git() {
    let (cg, _env, _dir) = setup_empty_project().await;
    // Fixture enrollment pins a repository identity, which initializes an
    // empty git repository with an unborn HEAD and no commits. The tree diff
    // must surface a structured git error naming the unresolvable ref in the
    // tool payload rather than success-looking prose (a project that is not a
    // repository at all is covered by the git shell's own open refusal test).
    let result = handle_tool_call(
        &cg,
        "tracedecay_changelog",
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["error"]["kind"].as_str(), Some("git"));
    assert_eq!(output["error"]["operation"].as_str(), Some("diff"));
    assert!(
        output["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot resolve 'HEAD~1'"),
        "the unborn-HEAD refusal must name the unresolvable ref: {output}"
    );
}

#[tokio::test]
async fn run_affected_tests_requires_manifest_scoped_changed_paths() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_run_affected_tests",
        json!({"timeout_secs": 1}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["error"]["kind"].as_str(), Some("invalid_request"));
    assert_eq!(output["error"]["operation"].as_str(), Some("changed_paths"));
    assert!(
        output["note"].is_null(),
        "missing scope input must not be reported as a no-change note: {output}"
    );
}

#[tokio::test]
async fn pr_context_no_git_returns_structured_git_error() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_pr_context",
        json!({"base_ref": "HEAD~1", "head_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["error"]["kind"].as_str(), Some("git"));
    assert_eq!(output["error"]["operation"].as_str(), Some("diff"));
}

#[tokio::test]
async fn test_port_status() {
    let (cg, _dir) = setup_project().await;
    wait_for_current_graph(&cg).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_status",
        json!({"source_dir": "src", "target_dir": "tests"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);

    assert_eq!(payload["source_dir"], json!("src"), "payload: {payload}");
    assert_eq!(payload["target_dir"], json!("tests"), "payload: {payload}");

    // `src/` holds `main`, `helper`, and `format_greeting`; `tests/` holds only
    // `test_helper`. No source name has a counterpart in the target, so nothing
    // is matched and coverage is exactly zero.
    let source_count = payload["source_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("port_status must report source_count: {payload}"));
    assert!(
        source_count >= 3,
        "src/ defines at least main, helper, and format_greeting: {payload}"
    );
    assert_eq!(
        payload["matched"].as_u64(),
        Some(0),
        "`test_helper` is not a counterpart of any src symbol: {payload}"
    );
    assert_eq!(
        payload["unmatched"].as_u64(),
        Some(source_count),
        "matched + unmatched must account for every source symbol: {payload}"
    );
    assert_eq!(
        payload["coverage_percent"].as_f64(),
        Some(0.0),
        "zero matches must render as zero percent coverage: {payload}"
    );

    let target_only: Vec<&str> = payload["target_only_symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("port_status must report target_only_symbols: {payload}"))
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert!(
        target_only.contains(&"test_helper"),
        "`test_helper` exists only in the target dir, got {target_only:?}: {payload}"
    );

    close_test_graph(cg).await;
}

/// `port_status` must not match symbols purely on (name, kind_compat_group).
/// Common method names like `new`, `process`, `fmt`, or `reset` produced
/// wild cross-type "matches" — e.g. `Biquad::new` pairing with an unrelated
/// `Adaa::new`. The match key must also include the parent type so siblings
/// of distinct owners stay unmatched.
#[tokio::test]
async fn port_status_does_not_match_methods_of_different_parents() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src_a")).unwrap();
    fs::create_dir_all(project.join("src_b")).unwrap();

    fs::write(
        project.join("src_a/biquad.rs"),
        "pub struct Biquad;\n\
         impl Biquad {\n    pub fn new() -> Self { Self }\n    pub fn process(&self) {}\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src_b/adaa.rs"),
        "pub struct Adaa;\n\
         impl Adaa {\n    pub fn new() -> Self { Self }\n    pub fn process(&self) {}\n}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_port_status",
        json!({
            "source_dir": "src_a",
            "target_dir": "src_b",
            "kinds": ["method"],
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).expect("response must be JSON");
    let matched: Vec<&Value> = output["matched_symbols"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    // None of the source methods should match because the parent types differ.
    assert!(
        matched.is_empty(),
        "Biquad::* and Adaa::* must not cross-match — got matches: {matched:?}"
    );
    assert_eq!(
        output["matched"].as_u64(),
        Some(0),
        "matched count must be 0; output={output}"
    );
}

/// Sanity: when the same parent type name exists in both dirs, methods do
/// match — confirming the parent-aware key isn't too strict.
#[tokio::test]
async fn port_status_matches_methods_with_same_parent_type() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src_a")).unwrap();
    fs::create_dir_all(project.join("src_b")).unwrap();

    fs::write(
        project.join("src_a/biquad.rs"),
        "pub struct Biquad;\n\
         impl Biquad { pub fn process(&self) {} }\n",
    )
    .unwrap();
    fs::write(
        project.join("src_b/biquad_port.rs"),
        "pub struct Biquad;\n\
         impl Biquad { pub fn process(&self) {} }\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_port_status",
        json!({
            "source_dir": "src_a",
            "target_dir": "src_b",
            "kinds": ["method"],
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).expect("response must be JSON");
    assert_eq!(
        output["matched"].as_u64(),
        Some(1),
        "Biquad::process should match Biquad::process; output={output}"
    );
}

#[tokio::test]
async fn test_port_order() {
    let (cg, _dir) = setup_project().await;
    wait_for_current_graph(&cg).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_order",
        json!({"source_dir": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);

    assert_eq!(payload["source_dir"], json!("src"), "payload: {payload}");
    let total_symbols = payload["total_symbols"]
        .as_u64()
        .unwrap_or_else(|| panic!("port_order must report total_symbols: {payload}"));
    assert!(
        total_symbols >= 3,
        "src/ defines at least main, helper, and format_greeting: {payload}"
    );

    let levels = payload["levels"]
        .as_array()
        .unwrap_or_else(|| panic!("port_order must report levels: {payload}"));
    assert!(!levels.is_empty(), "payload: {payload}");

    // Map every ordered symbol to the level it landed in.
    let mut level_of = std::collections::HashMap::<&str, u64>::new();
    let mut emitted = 0usize;
    for level in levels {
        let index = level["level"]
            .as_u64()
            .unwrap_or_else(|| panic!("each level must carry its index: {payload}"));
        for symbol in level["symbols"]
            .as_array()
            .unwrap_or_else(|| panic!("each level must carry symbols: {payload}"))
        {
            emitted += 1;
            if let Some(name) = symbol["name"].as_str() {
                level_of.insert(name, index);
            }
        }
    }
    assert_eq!(
        payload["returned"].as_u64(),
        Some(emitted as u64),
        "`returned` must count the symbols actually laid out: {payload}"
    );

    // The fixture's dependency chain is main -> helper -> format_greeting, and
    // port order is leaves first, so the chain must come back strictly
    // reversed. This is the assertion that a topological sort can actually
    // fail: a broken layering collapses all three into one level.
    let level_for = |name: &str| -> u64 {
        *level_of
            .get(name)
            .unwrap_or_else(|| panic!("`{name}` missing from the port order: {payload}"))
    };
    assert!(
        level_for("format_greeting") < level_for("helper"),
        "`helper` depends on `format_greeting`, so the leaf must be ported first: {payload}"
    );
    assert!(
        level_for("helper") < level_for("main"),
        "`main` depends on `helper`, so `helper` must be ported first: {payload}"
    );
}

#[tokio::test]
async fn test_rename_preview_not_found() {
    let (cg, _env, _dir) = setup_empty_analysis_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rename_preview",
        json!({"node_id": "nonexistent_id_12345"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("Node not found"),
        "rename_preview with bad id should report 'Node not found', got: {}",
        text,
    );
}

// The missing-required-argument cases for `tracedecay_diff_context`,
// `tracedecay_changelog`, `tracedecay_port_status`, and `tracedecay_port_order`
// live in `schema_test::schema_required_arguments_match_representative_handler_parsers`,
// which pairs each one with the schema `required` array the handler parser is
// supposed to mirror instead of only asserting that *some* error came back.

#[tokio::test]
async fn commit_context_clean_worktree_returns_json() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    git_run(project, &["init"]);
    git_run(project, &["config", "user.email", "t@t"]);
    git_run(project, &["config", "user.name", "t"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join(".gitignore"), ".tracedecay/\nhome/\n").unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn clean() {}\n").unwrap();
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", "init"]);

    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_commit_context",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["summary"].as_str(), Some("No changes detected."));
    assert_eq!(output["changed_files"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        output["symbols_by_role"]
            .as_object()
            .map(serde_json::Map::len),
        Some(0)
    );
    assert!(output["recent_commits"].as_array().is_some());
}

#[tokio::test]
async fn test_changelog_with_real_git() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();

    git_run(project, &["init"]);
    git_run(project, &["config", "user.email", "test@test.com"]);
    git_run(project, &["config", "user.name", "Test"]);

    fs::write(project.join("src/lib.rs"), "pub fn original() {}\n").unwrap();
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", "initial"]);

    fs::write(
        project.join("src/lib.rs"),
        "pub fn original() {}\npub fn added() {}\n",
    )
    .unwrap();
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", "add function"]);

    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_changelog",
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    assert!(
        !text.contains("git diff failed"),
        "changelog in git repo should not fail, got: {}",
        text,
    );

    // The second commit touches exactly one file, so the tree diff between
    // HEAD~1 and HEAD is precisely `src/lib.rs`.
    let payload = extract_json(&result.value);
    assert_eq!(payload["from_ref"], json!("HEAD~1"), "payload: {payload}");
    assert_eq!(payload["to_ref"], json!("HEAD"), "payload: {payload}");
    assert_eq!(
        payload["changed_file_count"].as_u64(),
        Some(1),
        "the second commit changed exactly one file: {payload}"
    );
    assert_eq!(
        payload["changed_files"],
        json!(["src/lib.rs"]),
        "src/lib.rs is the only file in the diff: {payload}"
    );
}

#[tokio::test]
async fn test_dead_code_custom_kinds() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dead_code",
        json!({"kinds": ["struct"]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("dead_code_count"),
        "should have dead_code_count key"
    );
    let parsed: Value = serde_json::from_str(text).unwrap_or(json!({}));
    if let Some(items) = parsed["dead_code"].as_array() {
        for item in items {
            assert_eq!(
                item["kind"].as_str().unwrap_or(""),
                "struct",
                "dead code items should be structs when kinds=['struct']"
            );
        }
    }
}

#[tokio::test]
async fn test_gini() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_gini",
        json!({ "metric": "lines" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("gini").is_some(),
        "gini field should exist, got: {}",
        text
    );
    assert!(
        parsed.get("interpretation").is_some(),
        "interpretation field should exist"
    );
}

#[tokio::test]
async fn test_gini_default_metric() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_gini", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("gini").is_some(),
        "gini field should exist with default args, got: {}",
        text
    );
}

#[tokio::test]
async fn test_dependency_depth() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dependency_depth",
        json!({ "limit": 5 }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("max_depth").is_some(),
        "max_depth field should exist, got: {}",
        text
    );
    assert!(
        parsed.get("ideal_depth").is_some(),
        "ideal_depth field should exist"
    );
}

#[tokio::test]
async fn test_health_summary() {
    let (cg, _env, _dir) = setup_empty_analysis_project().await;
    let result = handle_tool_call(&cg, "tracedecay_health", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("quality_signal").is_some(),
        "quality_signal field should exist, got: {}",
        text
    );
    assert!(
        parsed.get("files_analyzed").is_some(),
        "files_analyzed field should exist"
    );
}

#[tokio::test]
async fn test_health_detailed() {
    let (cg, _env, _dir) = setup_empty_analysis_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_health",
        json!({ "details": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("quality_signal").is_some(),
        "quality_signal should exist, got: {}",
        text
    );
    let dims = parsed.get("dimensions").expect("dimensions should exist");
    assert!(dims.get("acyclicity").is_some(), "acyclicity score missing");
    assert!(dims.get("depth").is_some(), "depth score missing");
    assert!(dims.get("equality").is_some(), "equality score missing");
    assert!(dims.get("redundancy").is_some(), "redundancy score missing");
    assert!(dims.get("modularity").is_some(), "modularity score missing");
}

/// `tracedecay_redundancy` must surface AST-isomorphic duplicate pairs and
/// rank them by composite similarity. Plant two structurally identical
/// functions in a fixture and assert the pair surfaces in the top hit with
/// the `definite` severity bucket.
#[tokio::test]
async fn test_redundancy_finds_planted_duplicate() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();

    // Two functions: identical structure, renamed identifiers.
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn compute_a(value: i32) -> i32 {
    let mut acc = 0;
    for i in 0..value {
        if i % 2 == 0 {
            acc += i;
        } else {
            acc -= i;
        }
    }
    acc
}

pub fn compute_b(input: i32) -> i32 {
    let mut total = 0;
    for j in 0..input {
        if j % 2 == 0 {
            total += j;
        } else {
            total -= j;
        }
    }
    total
}

pub fn unrelated(x: i32) -> i32 {
    x * 2
}
"#,
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    wait_for_current_graph(&cg).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_redundancy",
        json!({ "min_lines": 5, "similarity_threshold": 0.5 }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();

    let pair_count = parsed["pair_count"].as_u64().unwrap_or(0);
    assert!(
        pair_count >= 1,
        "expected at least 1 duplicate pair, got: {text}"
    );

    let pairs = parsed["pairs"].as_array().expect("pairs array");
    let top = &pairs[0];
    assert!(
        top["ranking_score"].as_f64().unwrap_or(0.0) > 0.0,
        "top pair should expose ranking_score; full output: {text}"
    );
    assert!(
        top["signals"]["body_vector_cosine"].as_f64().is_some(),
        "top pair should expose body_vector_cosine; full output: {text}"
    );
    let kind = top["overlap_kind"].as_str().unwrap_or("");
    assert_eq!(
        kind, "ast_isomorphic",
        "top pair should be AST-isomorphic; full output: {text}"
    );
    let severity = top["severity"].as_str().unwrap_or("");
    assert_eq!(
        severity, "definite",
        "AST-identical pair should be 'definite'"
    );
    let names: Vec<&str> = vec![
        top["a"]["name"].as_str().unwrap_or(""),
        top["b"]["name"].as_str().unwrap_or(""),
    ];
    assert!(
        names.contains(&"compute_a") && names.contains(&"compute_b"),
        "expected compute_a/compute_b in pair, got {names:?}"
    );
    let groups = parsed["groups"].as_array().expect("groups array");
    assert!(
        groups
            .iter()
            .any(|group| group["size"].as_u64().unwrap_or(0) >= 2),
        "expected at least one duplicate group, got: {text}"
    );

    // Calling again against the same sealed generation is deterministic.
    let result2 = handle_tool_call(
        &cg,
        "tracedecay_redundancy",
        json!({ "min_lines": 5, "similarity_threshold": 0.5 }),
        None,
        None,
    )
    .await
    .unwrap();
    let parsed2: serde_json::Value = serde_json::from_str(extract_text(&result2.value)).unwrap();
    assert_eq!(parsed2["pair_count"], parsed["pair_count"]);
}

/// `details=true` must surface raw counts + interpretation per dimension,
/// not just the scalar score, so callers don't have to compose six
/// separate tools to reproduce the breakdown.
#[tokio::test]
async fn test_health_detailed_includes_raw_signals() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_health",
        json!({ "details": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let dims = parsed.get("dimensions").expect("dimensions should exist");

    for dim in [
        "acyclicity",
        "depth",
        "equality",
        "redundancy",
        "modularity",
        "coverage_discipline",
    ] {
        let d = dims.get(dim).unwrap_or_else(|| panic!("missing {dim}"));
        assert!(
            d.get("score").is_some(),
            "{dim}: 'score' field missing in details view"
        );
        assert!(
            d.get("source").is_some(),
            "{dim}: 'source' formula attribution missing"
        );
    }

    // Specific raw signals that the issue called out as missing today.
    assert!(dims["equality"].get("gini").is_some());
    assert!(dims["equality"].get("interpretation").is_some());
    assert!(dims["acyclicity"].get("edges_in_cycles").is_some());
    assert!(dims["depth"].get("max_chain").is_some());
    assert!(dims["depth"].get("ideal_chain").is_some());
    assert!(dims["modularity"].get("interpretation").is_some());
    assert!(dims["redundancy"].get("dead_count").is_some());
}

#[tokio::test]
async fn test_dsm_stats() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dsm",
        json!({ "shape": "stats" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.starts_with("## Design Structure Matrix"));
    assert!(
        text.contains("**files:**"),
        "files field should exist, got: {}",
        text
    );
    assert!(
        text.contains("**density:**"),
        "density field should exist, got: {}",
        text
    );
    assert!(
        text.contains("### Top Clusters"),
        "default DSM markdown should include top clusters, got: {}",
        text
    );
}

#[tokio::test]
async fn test_dsm_json_returns_stats_shape() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dsm",
        json!({ "format": "json" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed["stats"].get("files").is_some(),
        "files field should exist, got: {}",
        text
    );
    assert!(
        parsed["stats"].get("density").is_some(),
        "density field should exist"
    );
    assert_eq!(parsed["shape"], "stats");
}

#[tokio::test]
async fn test_dsm_clusters() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dsm",
        json!({ "shape": "clusters" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("### Top Clusters"),
        "clusters section should exist, got: {}",
        text
    );
}

#[tokio::test]
async fn test_test_risk() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_risk",
        json!({ "limit": 10 }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let summary = parsed.get("summary").expect("summary should exist");
    assert!(
        summary
            .get("total_functions")
            .and_then(|v| v.as_u64())
            .is_some_and(|v| v > 0),
        "total_functions should be > 0, got: {}",
        text
    );
    assert_eq!(
        summary["attribution"]["depth"].as_u64(),
        Some(3),
        "test-risk summary should advertise the calibrated attribution depth"
    );
    assert!(
        summary["buckets"]["attributed"].as_u64().is_some(),
        "summary should include calibrated attribution buckets, got: {}",
        text
    );
    assert_eq!(
        summary["confidence"].as_str(),
        Some("static_lower_bound"),
        "summary should label the calibrated coverage signal honestly"
    );
    assert!(parsed.get("risks").is_some(), "risks array should exist");
}

#[tokio::test]
async fn test_test_risk_distinguishes_direct_and_closure_attribution() {
    let (cg, _dir) = setup_integration_test_risk_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_risk",
        json!({ "limit": 10, "include_tested": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let summary = &parsed["summary"];

    assert_eq!(summary["total_functions"].as_u64(), Some(3));
    assert_eq!(summary["tested"].as_u64(), Some(2));
    assert_eq!(summary["coverage_pct"].as_f64(), Some(67.0));
    assert_eq!(
        summary["attribution"]["direct_unit_attributed"].as_u64(),
        Some(1)
    );
    assert_eq!(
        summary["attribution"]["closure_attributed"].as_u64(),
        Some(1)
    );
    assert_eq!(summary["buckets"]["attributed"].as_u64(), Some(2));
    assert_eq!(summary["buckets"]["orphan_entry"].as_u64(), Some(1));
    assert_eq!(summary["confidence"].as_str(), Some("static_lower_bound"));

    let risks = parsed["risks"]
        .as_array()
        .expect("risks should be an array");
    let public_entry = risks
        .iter()
        .find(|item| item["name"].as_str() == Some("public_entry"))
        .expect("public_entry should appear in risk output");
    let format_greeting = risks
        .iter()
        .find(|item| item["name"].as_str() == Some("format_greeting"))
        .expect("format_greeting should appear in risk output");
    let unused_public_api = risks
        .iter()
        .find(|item| item["name"].as_str() == Some("unused_public_api"))
        .expect("unused_public_api should appear in risk output");

    assert_eq!(public_entry["has_test"].as_bool(), Some(true));
    assert_eq!(
        public_entry["attribution_method"].as_str(),
        Some("direct_unit")
    );
    assert_eq!(public_entry["attribution_depth"].as_u64(), Some(1));

    assert_eq!(format_greeting["has_test"].as_bool(), Some(true));
    assert_eq!(
        format_greeting["attribution_method"].as_str(),
        Some("closure")
    );
    assert_eq!(format_greeting["attribution_depth"].as_u64(), Some(2));

    assert_eq!(unused_public_api["has_test"].as_bool(), Some(false));
    assert_eq!(
        unused_public_api["attribution_method"].as_str(),
        Some("none")
    );
    assert!(
        summary["confidence_note"]
            .as_str()
            .is_some_and(|note| note.contains("closure")),
        "confidence note should explain the conservative closure signal, got: {}",
        text
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_test_risk_attributes_ts_describe_it_tests() {
    let (cg, _dir) = setup_ts_describe_it_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_risk",
        json!({ "limit": 10, "include_tested": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let summary = &parsed["summary"];

    // Only `add` is a source function (the it-callback lives in a .test.ts file
    // and is excluded from the denominator). It is directly unit-attributed via
    // the describe/it callback.
    assert_eq!(
        summary["total_functions"].as_u64(),
        Some(1),
        "only add() should count as a source function, got: {text}"
    );
    assert_eq!(
        summary["attribution"]["direct_unit_attributed"].as_u64(),
        Some(1),
        "add() should be direct-unit attributed via the it() callback, got: {text}"
    );

    let risks = parsed["risks"].as_array().expect("risks array");
    let add = risks
        .iter()
        .find(|item| item["name"].as_str() == Some("add"))
        .expect("add should appear in risk output");
    assert_eq!(add["has_test"].as_bool(), Some(true));
    assert_eq!(add["attribution_method"].as_str(), Some("direct_unit"));
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_test_map_lists_ts_it_title_as_covering_test() {
    let (cg, _dir) = setup_ts_describe_it_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_map",
        json!({ "file": "src/math.ts" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();

    let coverage = parsed["coverage"].as_array().expect("coverage array");
    let add_cov = coverage
        .iter()
        .find(|c| c["source_name"].as_str() == Some("add"))
        .expect("add should be covered, got: {text}");
    let tests = add_cov["tests"].as_array().expect("tests array");
    assert!(
        tests
            .iter()
            .any(|t| t["test_name"].as_str() == Some("adds two numbers")),
        "test_map should list the it title as the covering test, got: {text}"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_test_risk_excludes_non_src_functions_from_denominator_and_risks() {
    let (cg, _dir) = setup_test_risk_non_src_fixture().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_risk",
        json!({ "limit": 10, "include_tested": true }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let summary = &parsed["summary"];

    assert_eq!(summary["total_functions"].as_u64(), Some(3));
    assert_eq!(summary["buckets"]["attributed"].as_u64(), Some(2));
    assert_eq!(summary["buckets"]["orphan_entry"].as_u64(), Some(1));
    assert_eq!(summary["buckets"]["excluded"].as_u64(), Some(2));
    assert_eq!(
        summary["top_risk_untested"].as_str(),
        Some("unused_public_api")
    );

    let risks = parsed["risks"]
        .as_array()
        .expect("risks should be an array");
    assert!(
        risks
            .iter()
            .all(|item| item["file"].as_str() != Some("build.rs")),
        "non-src build script functions should be excluded from risk rows, got: {}",
        text
    );
    assert!(
        risks
            .iter()
            .all(|item| item["name"].as_str() != Some("build_script_helper")),
        "build script helper should not be ranked as source risk, got: {}",
        text
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_todos_finds_markers() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/main.rs"),
        r#"
fn main() {
    // TODO: refactor this
    let x = 1;
    // FIXME: handle the error case
    let y = 2;
    println!("{} {}", x, y);
}

fn helper() {
    // not a marker: rendered todoist
    let _ = 0;
}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    wait_for_current_graph(&cg).await;

    let result = handle_tool_call(&cg, "tracedecay_todos", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let count = output["match_count"].as_u64().unwrap();
    assert_eq!(count, 2, "should find exactly TODO and FIXME, got: {text}");
    let kinds: Vec<&str> = output["markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"TODO"));
    assert!(kinds.contains(&"FIXME"));
    let enclosing: Vec<&str> = output["markers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["enclosing"].as_str())
        .collect();
    assert!(
        enclosing.iter().any(|e| e.contains("main")),
        "TODO inside main should report main as enclosing, got: {enclosing:?}"
    );
}

#[tokio::test]
async fn test_todos_filters_by_kind() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/main.rs"),
        r#"
fn main() {
    // TODO: a
    // FIXME: b
    // HACK: c
    let _ = 0;
}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_todos",
        json!({"kinds": ["FIXME"]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["match_count"].as_u64().unwrap(), 1);
    assert_eq!(output["markers"][0]["kind"].as_str().unwrap(), "FIXME");
}

#[tokio::test]
async fn test_todos_empty_when_clean() {
    let (cg, _env, _dir) = setup_empty_analysis_project().await;
    let result = handle_tool_call(&cg, "tracedecay_todos", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["match_count"].as_u64().unwrap(), 0);
    close_test_graph(cg).await;
}

/// `tracedecay_diff_context.impacted_symbols` must not list the same
/// downstream node more than once. The same id appeared 6+ times
/// consecutively when several modified symbols all reached the same dependent.
#[tokio::test]
async fn diff_context_dedupes_impacted_symbols() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Two functions in `mod.rs` both call `shared` in `dep.rs`. Without dedup,
    // `shared` appears twice in `impacted_symbols`.
    fs::write(
        project.join("src/lib.rs"),
        r#"
mod dep;
pub fn first() { dep::shared(); }
pub fn second() { dep::shared(); }
"#,
    )
    .unwrap();
    fs::write(project.join("src/dep.rs"), "pub fn shared() {}\n").unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_diff_context",
        json!({"files": ["src/lib.rs"], "depth": 3}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let impacted = output["impacted_symbols"].as_array().unwrap();
    let mut ids: Vec<&str> = impacted.iter().filter_map(|v| v["id"].as_str()).collect();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    let after = ids.len();
    assert_eq!(
        before, after,
        "impacted_symbols must not contain duplicates by id; got {before} entries, {after} unique"
    );
}

/// `tracedecay_recursion` must preserve genuine direct recursion while
/// filtering length-1 self-edge artifacts.
#[tokio::test]
async fn recursion_keeps_direct_recursion() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn recurse(n: u32) -> u32 {
    if n == 0 { 0 } else { recurse(n - 1) }
}

pub fn nonrecursive() -> u32 { 42 }
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    let has_recurse = cycles.iter().any(|cycle| {
        cycle["chain"].as_array().is_some_and(|chain| {
            chain
                .iter()
                .filter_map(|n| n["name"].as_str())
                .filter(|name| *name == "recurse")
                .count()
                >= 2
        })
    });
    assert!(
        has_recurse,
        "direct self-recursive function should be reported; got {cycles:?}"
    );
}

#[tokio::test]
async fn recursion_filters_self_edge_artifacts() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub struct Triplet {
    rows: Vec<usize>,
}

impl Triplet {
    pub fn push(&mut self, row: usize) {
        self.rows.push(row);
    }
}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    let mentions_push = cycles.iter().any(|cycle| {
        cycle["chain"]
            .as_array()
            .is_some_and(|chain| chain.iter().any(|n| n["name"].as_str() == Some("push")))
    });
    assert!(
        !mentions_push,
        "`self.rows.push(...)` should not be reported as recursive; got {cycles:?}"
    );
}

#[tokio::test]
async fn recursion_reports_real_cycle_path() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn a() { b(); }
pub fn b() { c(); }
pub fn c() { a(); }
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    let chain = cycles
        .iter()
        .find_map(|cycle| {
            let chain = cycle["chain"].as_array()?;
            let names: Vec<&str> = chain.iter().filter_map(|n| n["name"].as_str()).collect();
            (names.len() == 4).then_some(names)
        })
        .expect("expected a three-node cycle path");
    let valid_edges = [("a", "b"), ("b", "c"), ("c", "a")];
    for pair in chain.windows(2) {
        assert!(
            valid_edges.contains(&(pair[0], pair[1])),
            "chain must follow real call edges; got {chain:?}"
        );
    }
}

/// `tracedecay_changelog`'s response must not list directories under
/// `files_not_indexed`. A small git repo with a real commit history that
/// touches both a real file and a synthesised directory path must have the
/// directory filtered out.
#[tokio::test]
async fn changelog_filters_directory_paths() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    git_run(project, &["init"]);
    git_run(project, &["config", "user.email", "t@t"]);
    git_run(project, &["config", "user.name", "t"]);
    fs::create_dir_all(project.join("src/sub")).unwrap();
    fs::write(project.join("src/sub/keep.rs"), "pub fn k() {}\n").unwrap();
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", "init"]);
    fs::write(
        project.join("src/sub/keep.rs"),
        "pub fn k() { let _ = 1; }\n",
    )
    .unwrap();
    fs::write(project.join("src/sub/added.rs"), "pub fn a() {}\n").unwrap();
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", "two"]);
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_changelog",
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let changed: Vec<&str> = output["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for entry in &changed {
        let p = project.join(entry);
        assert!(
            !p.is_dir(),
            "changed_files must not include directories; got {entry:?}"
        );
    }
}

/// `tracedecay_unused_imports` must flag unused imports. Testing
/// `incoming.is_empty()` on every Use node never fires: Use nodes always
/// have at least one incoming Contains edge from their containing
/// module/file, so that condition returned 0 on every real codebase.
#[tokio::test]
async fn unused_imports_detects_truly_unused() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
use std::collections::HashMap;
use std::collections::HashSet;
mod inner;

pub fn used_one() -> HashMap<u32, u32> { HashMap::new() }
"#,
    )
    .unwrap();
    fs::write(project.join("src/inner.rs"), "pub fn inner_fn() {}\n").unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(&cg, "tracedecay_unused_imports", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let imports = output["imports"].as_array().unwrap();
    let names: Vec<&str> = imports.iter().filter_map(|u| u["name"].as_str()).collect();
    // `HashSet` is imported but never used in the file body.
    assert!(
        names.iter().any(|n| n.contains("HashSet")),
        "HashSet should be reported as unused; got names={names:?}"
    );
}

/// An import named only in a nearby comment (like the audit fixture's own
/// `// Planted unused import: BTreeMap …`) must not be read as "used" by the
/// text scan. The masked scan must flag it in both markdown and JSON with
/// file:line, and must not flag a genuinely-used import.
#[tokio::test]
async fn unused_imports_reports_in_markdown_and_json() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    // `HashMap` is used; `BTreeMap` is unused but named in the comment above it.
    fs::write(
        project.join("src/lib.rs"),
        "use std::collections::HashMap;\n\
         // Planted unused import: BTreeMap is referenced nowhere in real code.\n\
         use std::collections::BTreeMap;\n\
         \n\
         pub fn used_one() -> HashMap<u32, u32> { HashMap::new() }\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    // Markdown (runtime default; request explicitly so the test helper does not
    // force-inject `format=json`).
    let md = handle_tool_call(
        &cg,
        "tracedecay_unused_imports",
        json!({"format": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&md.value);
    assert!(text.contains("## Unused Imports"), "got: {text}");
    // `line` is the node's 0-based start line (source line 3 → 2), matching the
    // rest of the graph API.
    assert!(
        text.contains("**BTreeMap unused in src/lib.rs:2**"),
        "markdown must report the unused import with file:line: {text}"
    );
    assert!(
        !text.contains("HashMap unused"),
        "used import must not be flagged: {text}"
    );

    // JSON output must carry the same structured finding.
    let js = handle_tool_call(
        &cg,
        "tracedecay_unused_imports",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&js.value)).unwrap();
    assert_eq!(payload["unused_import_count"], 1, "payload: {payload}");
    let imports = payload["imports"].as_array().unwrap();
    assert_eq!(imports.len(), 1, "payload: {payload}");
    let m = &imports[0];
    assert_eq!(m["unused"], "BTreeMap");
    assert_eq!(m["file"], "src/lib.rs");
    assert_eq!(m["line"], 2);
    // The used import must never appear.
    assert!(
        imports
            .iter()
            .all(|u| u["unused"].as_str() != Some("HashMap")),
        "used HashMap must not be flagged: {payload}"
    );
}

/// Rust's format macros implicitly capture identifiers named inside the format
/// string. Those captures are real references even though they are lexically
/// inside a string literal, so masking string noise must preserve them.
#[tokio::test]
async fn unused_imports_keeps_implicit_format_capture() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "use std::f64::consts::PI;\n\
         pub fn print_pi() { println!(\"{PI}\"); }\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_unused_imports",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let imports = payload["imports"].as_array().unwrap();
    assert!(
        imports
            .iter()
            .all(|item| item["unused"].as_str() != Some("PI")),
        "PI is used by the implicit format capture and must not be flagged: {payload}"
    );
}

/// `tracedecay_dead_code` must support `include_public` so agents can audit
/// pub items with no callers in the indexed scope. SQL that hard-codes
/// `visibility != 'public'` reports 0 dead symbols on a mostly-`pub` codebase.
#[tokio::test]
async fn dead_code_with_include_public_finds_pub_unreferenced() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn called() {}
pub fn never_called_anywhere() {}
pub fn caller() { called(); }
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let default_result = handle_tool_call(&cg, "tracedecay_dead_code", json!({}), None, None)
        .await
        .unwrap();
    let default_text = extract_text(&default_result.value);
    let default_output: Value = serde_json::from_str(default_text).unwrap();
    assert_eq!(
        default_output["dead_code_count"].as_u64().unwrap_or(99),
        0,
        "default dead_code (no include_public) must still skip pub items"
    );

    let with_pub = handle_tool_call(
        &cg,
        "tracedecay_dead_code",
        json!({"include_public": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let with_pub_text = extract_text(&with_pub.value);
    let with_pub_output: Value = serde_json::from_str(with_pub_text).unwrap();
    let symbols: Vec<&str> = with_pub_output["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(
        symbols.contains(&"never_called_anywhere"),
        "with include_public, the pub unreferenced fn should appear; got {symbols:?}"
    );
}

/// `build_file_adjacency` must count only `uses` and `calls` for file-level
/// dependency depth. `implements` and `extends` edges are heavily
/// resolver-fuzzy-bound to nonsense targets in unrelated files.
#[tokio::test]
async fn dependency_depth_excludes_implements_and_extends() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    // file_a derives Debug — extractor emits derives_macro and the
    // resolver historically pollutes implements edges across files.
    fs::write(
        project.join("src/lib.rs"),
        r#"
mod a;
mod b;
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/a.rs"),
        r#"
#[derive(Debug, Clone)]
pub struct A;
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        r#"
pub trait T {}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_dependency_depth",
        json!({"limit": 100}),
        None,
        None,
    )
    .await
    .unwrap();
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let chains = output["chains"]
        .as_array()
        .expect("dependency-depth response should contain chains");
    assert!(
        chains.iter().all(|entry| {
            let chain = entry["chain"]
                .as_array()
                .expect("dependency-depth chain should be an array");
            !chain.windows(2).any(|pair| {
                matches!(
                    (pair[0].as_str(), pair[1].as_str()),
                    (Some("src/a.rs"), Some("src/b.rs")) | (Some("src/b.rs"), Some("src/a.rs"))
                )
            })
        }),
        "derive/trait metadata must not create a dependency between leaf files: {output}"
    );
}

/// `tracedecay_diagnose` must normalize span paths before looking them up
/// in the graph. cargo emits absolute and (on Windows) backslash-separated
/// paths; the graph stores project-relative, forward-slash paths. Without
/// normalization a diagnostic with span `/abs/path/to/project/src/lib.rs:42:1`
/// or `src\lib.rs:42:1` resolves to `node: null` even though the file is
/// indexed.
#[tokio::test]
async fn diagnose_normalizes_absolute_and_backslash_paths() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn target() {}\n").unwrap();
    let (cg, _env) = init_test_project(project).await;

    let abs_path = project.join("src/lib.rs");
    let abs_str = abs_path.to_string_lossy().to_string();
    let backslash_str = "src\\lib.rs";
    let cargo_output = format!(
        "error[E0001]: synthetic error\n  --> {abs_str}:1:1\n   |\n\nerror[E0002]: backslash form\n  --> {backslash_str}:1:1\n   |\n"
    );

    let result = handle_tool_call(
        &cg,
        "tracedecay_diagnose",
        json!({"cargo_output": cargo_output, "include_callers": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let mapped = output["mapped_to_node"].as_u64().unwrap_or(0);
    assert_eq!(
        mapped, 2,
        "both diagnostics should map to nodes after path normalization; got mapped={mapped} full={output:#}"
    );
}

/// `tracedecay_diagnose` builds a request-scoped redundancy view from the
/// admitted generation and surfaces AST-isomorphic functions under
/// `near_duplicates`.
#[tokio::test]
async fn diagnose_surfaces_generation_pinned_near_duplicates() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub fn compute_a(value: i32) -> i32 {
    let mut acc = 0;
    for i in 0..value {
        if i % 2 == 0 {
            acc += i;
        } else {
            acc -= i;
        }
    }
    acc
}

pub fn compute_b(input: i32) -> i32 {
    let mut total = 0;
    for j in 0..input {
        if j % 2 == 0 {
            total += j;
        } else {
            total -= j;
        }
    }
    total
}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    // The fixture is stable: compute_a begins on line 2.
    let diag_line = 2;
    let cargo_output =
        format!("error[E0001]: synthetic error\n  --> src/lib.rs:{diag_line}:5\n   |\n");

    let result = handle_tool_call(
        &cg,
        "tracedecay_diagnose",
        json!({"cargo_output": cargo_output, "include_callers": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();

    let diagnostics = output["diagnostics"].as_array().expect("diagnostics");
    let diag = diagnostics
        .iter()
        .find(|d| d["node"]["name"].as_str() == Some("compute_a"))
        .unwrap_or_else(|| panic!("diagnostic did not map to compute_a: {output:#}"));
    let dupes = diag["near_duplicates"]
        .as_array()
        .unwrap_or_else(|| panic!("near_duplicates missing: {output:#}"));
    assert!(
        dupes
            .iter()
            .any(|d| d["name"].as_str() == Some("compute_b")),
        "expected compute_b in near_duplicates, got: {output:#}"
    );
    let top = &dupes[0];
    assert_eq!(top["name"].as_str(), Some("compute_b"));
    assert_eq!(top["overlap_kind"].as_str(), Some("ast_isomorphic"));
    assert_eq!(top["severity"].as_str(), Some("definite"));
    assert!(top["ranking_score"].as_f64().unwrap_or(0.0) > 0.0);
}

/// The resolver's kind-compatibility filter must apply to the same-file
/// blocklist branches too. Without it, common names like
/// `new`/`default`/`clone` can still bind a `Calls` reference to a
/// non-callable same-file symbol — e.g. a const literally named
/// `default` — when it's the only same-file match for a blocklisted
/// name.
#[tokio::test]
async fn resolver_blocklist_branch_respects_kind_filter() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Use a struct named after a blocklisted identifier ("new") plus a
    // call site that the parser treats as a call_expression. The same-file
    // blocklist branch must not bind the Calls ref to this struct just
    // because no other "new" lives in the file.
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub struct new;

pub fn caller() {
    let _ = new();
    helper();
}

pub fn helper() {}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let caller_id = find_node_id(&cg, "caller").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callees",
        json!({"node_id": caller_id, "max_depth": 1, "resolve_dispatch": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let items: Value = serde_json::from_str(text).unwrap();
    let arr = items.as_array().unwrap();
    for entry in arr {
        let kind = entry["kind"].as_str().unwrap_or("");
        let name = entry["name"].as_str().unwrap_or("");
        let callable = matches!(
            kind,
            "function" | "method" | "struct_method" | "constructor" | "macro" | "arrow_function"
        );
        assert!(
            callable,
            "caller's callees must be callable kinds; got name={name} kind={kind} full={arr:#?}"
        );
    }
}

/// When an `impl Trait for X` reference cannot resolve to a real trait node
/// (e.g. `Default` lives in std and isn't indexed), the resolver must not
/// fuzzy-bind it to an unrelated node kind. A parser `Token` enum whose
/// `Default` variant became the target of 150 stray `implements` edges from
/// manual `impl Default for X` blocks poisoned `tracedecay_rank --edge-kind
/// implements`. Implements/Extends/derives references must only resolve to
/// trait-shaped targets.
#[tokio::test]
async fn implements_refs_dont_resolve_to_enum_variants() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub enum Token { Default, Plus }

pub trait Renderable {}

pub struct A;
impl Default for A { fn default() -> Self { A } }
impl Renderable for A {}

pub struct B;
impl Default for B { fn default() -> Self { B } }
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
        json!({"edge_kind": "implements", "direction": "incoming", "limit": 100}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let ranking = output["ranking"].as_array().unwrap();
    let enum_variant = ranking
        .iter()
        .find(|entry| entry["kind"] == "enum_variant" && entry["name"] == "Default")
        .expect("the poisoned Default enum variant remains a typed graph identity");
    assert_eq!(enum_variant["count"].as_u64(), Some(0));
    let trait_target = ranking
        .iter()
        .find(|entry| entry["kind"] == "trait" && entry["name"] == "Renderable")
        .expect("ordinary trait target remains ranked");
    assert!(
        trait_target["count"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "ordinary Implements edge must survive compatible target filtering"
    );
}

/// `tracedecay_circular` must report one entry per strongly-connected
/// component, not every walk through the cycle. Counting DFS paths through
/// the same SCC reported 73 "cycles" that were one genuine component.
#[tokio::test]
async fn circular_reports_one_entry_per_scc_not_per_walk() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Three-file cycle: a uses b, b uses c, c uses a. Multiple DFS walks
    // through this triangle must not report 3+ "cycles"
    // (a→b→c→a, b→c→a→b, c→a→b→c).
    fs::write(project.join("src/lib.rs"), "mod a; mod b; mod c;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "use crate::b::b_fn;\npub fn a_fn() { b_fn(); }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "use crate::c::c_fn;\npub fn b_fn() { c_fn(); }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/c.rs"),
        "use crate::a::a_fn;\npub fn c_fn() { a_fn(); }\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(&cg, "tracedecay_circular", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycle_count = output["cycle_count"].as_u64().unwrap();
    assert_eq!(
        cycle_count, 1,
        "three-file SCC must report exactly one cycle entry, got {cycle_count}"
    );
    let cycle = &output["cycles"][0];
    assert_eq!(
        cycle["member_count"].as_u64(),
        Some(3),
        "the cycle should account for all three files in the SCC; got {cycle:?}"
    );
    assert_eq!(
        cycle["members"].as_array().map(Vec::len),
        Some(3),
        "three members fit the default member bound; got {cycle:?}"
    );
    assert_eq!(cycle["omitted_member_count"].as_u64(), Some(0));
}

/// `tracedecay_port_order`'s `cycles` output must expose the SCCs forming
/// each cycle separately, instead of collapsing all unsorted nodes into a
/// single mega-blob. Collapsing them packed 200+ unrelated symbols into one
/// entry with no way to know what to break first.
#[tokio::test]
async fn port_order_reports_separate_scc_groups() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Two disjoint mutually-recursive pairs: (a, b) and (c, d). Each pair
    // must appear as its own cycle group, not one lumped "Mutual
    // dependency" entry.
    fs::write(project.join("src/lib.rs"), "pub mod m;\n").unwrap();
    fs::write(
        project.join("src/m.rs"),
        r#"
pub fn a() { b(); }
pub fn b() { a(); }
pub fn c() { d(); }
pub fn d() { c(); }
pub fn leaf() {}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_order",
        json!({"source_dir": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    assert!(
        cycles.len() >= 2,
        "expected at least 2 disjoint cycle groups; got {} entries: {cycles:?}",
        cycles.len()
    );
    // No cycle entry should mix both (a,b) and (c,d) names — that would
    // mean the fix didn't actually separate them. (Each symbol is now an
    // object: {name, kind, file, line, in_cycle_out_degree, ...}.)
    for c in cycles {
        let names: Vec<&str> = c["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s["name"].as_str().or_else(|| s.as_str()))
            .collect();
        let has_ab = names.iter().any(|n| *n == "a" || *n == "b");
        let has_cd = names.iter().any(|n| *n == "c" || *n == "d");
        assert!(
            !(has_ab && has_cd),
            "one cycle entry contains both SCCs (a/b mixed with c/d): {names:?}"
        );
    }
}

/// `tracedecay_port_order` must expose intra-cycle ordering signals so an
/// agent can pick a starting point inside a 200-symbol SCC instead of
/// staring at an undifferentiated blob. Each cycle entry must carry
/// per-symbol in-cycle degree data, a file-level member-count breakdown,
/// and explicit `entry_point` / `break_point_candidate` suggestions.
#[tokio::test]
async fn port_order_provides_intra_cycle_ordering() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    // a → b → c → a, plus a "hub" h that all three call into and that
    // calls a back. h is the central node (highest in-cycle in-degree).
    fs::write(project.join("src/lib.rs"), "pub mod m;\n").unwrap();
    fs::write(
        project.join("src/m.rs"),
        r#"
pub fn a() { b(); h(); }
pub fn b() { c(); h(); }
pub fn c() { a(); h(); }
pub fn h() { a(); }
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_order",
        json!({"source_dir": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    assert!(!cycles.is_empty(), "expected at least one cycle");
    let cycle = &cycles[0];
    assert!(
        cycle["files"].as_array().is_some(),
        "cycle must carry a `files` breakdown"
    );
    let files_arr = cycle["files"].as_array().unwrap();
    for f in files_arr {
        assert!(
            f.is_object() && f["members_in_cycle"].as_u64().is_some(),
            "files entries must be objects with `members_in_cycle`, got {f}"
        );
    }
    let symbols = cycle["symbols"].as_array().unwrap();
    for s in symbols {
        assert!(
            s["in_cycle_out_degree"].as_u64().is_some(),
            "each symbol must report in_cycle_out_degree; got {s}"
        );
        assert!(
            s["in_cycle_in_degree"].as_u64().is_some(),
            "each symbol must report in_cycle_in_degree; got {s}"
        );
    }
    assert!(
        cycle["entry_point"].is_object(),
        "cycle must surface a suggested entry_point; got {cycle}"
    );
    assert!(
        cycle["break_point_candidate"].is_object(),
        "cycle must surface a break_point_candidate; got {cycle}"
    );
    // The break point should be `h` (most internal callers).
    assert_eq!(
        cycle["break_point_candidate"]["name"].as_str(),
        Some("h"),
        "break_point_candidate should be the hub function `h`; got {cycle}"
    );
}

/// Self-edges from fuzzy resolution (`self.rows.push(...)` inside a method
/// named `push`) must not make singleton symbols appear as cycles.
#[tokio::test]
async fn port_order_ignores_self_edges() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod m;\n").unwrap();
    fs::write(
        project.join("src/m.rs"),
        r#"
pub struct Triplet {
    rows: Vec<usize>,
}

impl Triplet {
    pub fn push(&mut self, row: usize) {
        self.rows.push(row);
    }
}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_order",
        json!({"source_dir": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    assert!(
        cycles.is_empty(),
        "self-edge-only methods should stay out of port_order cycles: {cycles:?}"
    );
}

/// `tracedecay_inheritance_depth` must surface Rust supertrait chains
/// (`trait T: U`) as `Extends` edges.
#[tokio::test]
async fn inheritance_depth_walks_rust_supertraits() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub trait Base {}
pub trait Middle: Base {}
pub trait Leaf: Middle {}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(&cg, "tracedecay_inheritance_depth", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let ranking = output["ranking"].as_array().unwrap();
    let names: Vec<&str> = ranking.iter().filter_map(|r| r["name"].as_str()).collect();
    assert!(
        names.contains(&"Leaf"),
        "expected Leaf trait in inheritance_depth ranking; got {names:?}"
    );
    let leaf = ranking
        .iter()
        .find(|r| r["name"].as_str() == Some("Leaf"))
        .unwrap();
    let depth = leaf["depth"].as_u64().unwrap();
    assert!(depth >= 2, "Leaf depth should be >= 2 hops, got {depth}");
}

/// `tracedecay_circular` must emit *disjoint* SCCs — no file should appear
/// in more than one cycle entry. Cycles "sharing long tails" mean the SCC
/// condensation step is broken. This stress test wires up many disjoint
/// cycles plus DAG-style tails between them and asserts no file leaks into
/// a second cycle entry.
#[tokio::test]
async fn circular_emits_disjoint_sccs_under_load() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    let mut lib_rs = String::new();
    // Build 5 disjoint 3-file cycles with shared DAG tails between them.
    // Cycle k = (a_k -> b_k -> c_k -> a_k); plus a one-way edge from c_k
    // to a_{k+1} that introduces a non-cyclic "shared tail" between the
    // SCCs. Tarjan must still emit each cycle as its own SCC.
    for k in 0..5 {
        let _ = write!(lib_rs, "pub mod a{k};\npub mod b{k};\npub mod c{k};\n");
    }
    fs::write(project.join("src/lib.rs"), lib_rs).unwrap();
    for k in 0..5 {
        let next = (k + 1) % 5;
        fs::write(
            project.join(format!("src/a{k}.rs")),
            format!("use crate::b{k}::b_fn;\npub fn a_fn() {{ b_fn(); }}\n"),
        )
        .unwrap();
        fs::write(
            project.join(format!("src/b{k}.rs")),
            format!("use crate::c{k}::c_fn;\npub fn b_fn() {{ c_fn(); }}\n"),
        )
        .unwrap();
        fs::write(
            project.join(format!("src/c{k}.rs")),
            format!(
                "use crate::a{k}::a_fn;\nuse crate::a{next}::a_fn as next_a;\npub fn c_fn() {{ a_fn(); next_a(); }}\n"
            ),
        )
        .unwrap();
    }
    let (cg, _env) = init_test_project(project).await;
    // Disjointness is only observable when every member is listed, so raise the
    // member bound above this fixture's 15-file component.
    let result = handle_tool_call(
        &cg,
        "tracedecay_circular",
        json!({"member_limit": 200}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let cycles = output["cycles"].as_array().unwrap();
    // All cycles forming one giant SCC since c_k → a_{k+1} chains them.
    // The critical invariant is *disjointness*: no file appears twice.
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    for cycle in cycles {
        assert_eq!(
            cycle["omitted_member_count"].as_u64(),
            Some(0),
            "the raised member bound must list every member; got {cycle:?}"
        );
        let files = cycle["members"].as_array().unwrap();
        for f in files {
            let s = f.as_str().unwrap().to_string();
            assert!(
                seen.insert(s.clone()),
                "file {s} appears in more than one cycle entry; SCCs must be disjoint"
            );
        }
    }
}

/// `tracedecay_diff_context`'s `modified_symbols` must dedup by node id,
/// even when callers pass the same path multiple times in `files`. A file
/// node listed 7× in a row is the caller duplicating the same path
/// upstream.
#[tokio::test]
async fn diff_context_dedupes_modified_symbols_on_duplicate_input() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub struct S; pub fn one() {} pub fn two() {}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_diff_context",
        json!({"files": ["src/lib.rs", "src/lib.rs", "src/lib.rs"], "depth": 1}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let modified = output["modified_symbols"].as_array().unwrap();
    let mut ids: Vec<&str> = modified.iter().filter_map(|v| v["id"].as_str()).collect();
    let before = ids.len();
    ids.sort();
    ids.dedup();
    let after = ids.len();
    assert_eq!(
        before, after,
        "modified_symbols must not contain duplicate ids even when input has the same file 3×; got {before} entries, {after} unique"
    );
}

/// When a whole subtree is removed in a diff, `tracedecay_changelog` must
/// not report the deleted directory under `files_not_indexed`. An `is_dir()`
/// filter misses this because the path is gone from disk by the time it is
/// checked. gix's `entry_mode` flag skips tree entries before they enter
/// the change list.
#[tokio::test]
async fn changelog_filters_deleted_directory_entries() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    git_run(project, &["init"]);
    git_run(project, &["config", "user.email", "t@t"]);
    git_run(project, &["config", "user.name", "t"]);
    fs::create_dir_all(project.join("crates/sub")).unwrap();
    fs::write(project.join("crates/sub/keep.rs"), "pub fn k() {}\n").unwrap();
    fs::write(project.join("main.rs"), "fn main() {}\n").unwrap();
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", "init"]);
    // Remove the whole subtree so gix's tree-diff yields a directory-mode
    // deletion entry.
    fs::remove_dir_all(project.join("crates")).unwrap();
    git_run(project, &["add", "-A"]);
    git_run(project, &["commit", "-m", "drop crates"]);
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_changelog",
        json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let changed: Vec<String> = output["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let problematic: Vec<&String> = changed.iter().filter(|p| !p.ends_with(".rs")).collect();
    assert!(
        problematic.is_empty(),
        "changed_files should be file paths only (no directories like 'crates' or 'crates/sub'); got problematic={problematic:?} full={changed:?}"
    );
}

/// `tracedecay_pr_context` must not explode Cargo.toml (or any
/// .toml/.yaml/.json config file) into one symbol per `[name]`,
/// `[version]`, `[dependencies]` key. A Cargo.toml change with ~30
/// dependency lines produced ~70 entries that pushed the response past
/// 760k tokens. Config files should collapse to a single summary symbol.
#[tokio::test]
async fn pr_context_collapses_cargo_toml_keys() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    git_run(project, &["init"]);
    git_run(project, &["config", "user.email", "t@t"]);
    git_run(project, &["config", "user.name", "t"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", "init"]);
    // Second commit: bloat Cargo.toml with many deps.
    let mut bloated = String::from(
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    for i in 0..50 {
        let _ = writeln!(bloated, "dep{i} = \"0.1.{i}\"");
    }
    fs::write(project.join("Cargo.toml"), &bloated).unwrap();
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", "deps"]);

    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_pr_context",
        json!({"base_ref": "HEAD~1", "head_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let added = output["added"].as_array().unwrap();
    let modified = output["modified"].as_array().unwrap();
    let count_cargo = |arr: &[Value]| -> usize {
        arr.iter()
            .filter(|v| v["file"].as_str() == Some("Cargo.toml"))
            .count()
    };
    let cargo_total = count_cargo(added) + count_cargo(modified);
    assert!(
        cargo_total <= 1,
        "Cargo.toml should collapse to at most one summary symbol; got {cargo_total} entries. added={added:?}, modified={modified:?}"
    );
    // And the surviving entry must be a config summary, not a regular key.
    let summary = modified
        .iter()
        .find(|v| v["file"].as_str() == Some("Cargo.toml"));
    assert!(
        summary.is_some(),
        "expected one config_summary entry for Cargo.toml in modified; got {modified:?}"
    );
    assert_eq!(
        summary.unwrap()["kind"].as_str(),
        Some("config_summary"),
        "Cargo.toml entry should be kind=config_summary"
    );
}

/// `tracedecay_unused_imports` must flag genuinely unused identifiers
/// inside grouped `use foo::{A, B}` imports. Without per-identifier
/// splitting, the heuristic never flags anything from a grouped import
/// (`use std::collections::{HashMap, HashSet, BTreeMap};`).
#[tokio::test]
async fn unused_imports_handles_grouped_use() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
use std::collections::{HashMap, HashSet};

pub fn used() -> HashMap<u32, u32> { HashMap::new() }
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    let result = handle_tool_call(&cg, "tracedecay_unused_imports", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let imports = output["imports"].as_array().unwrap();
    let payloads: Vec<String> = imports
        .iter()
        .map(|u| {
            format!(
                "{}::{}",
                u["name"].as_str().unwrap_or(""),
                u["unused"].as_str().unwrap_or("")
            )
        })
        .collect();
    let mentions_hashset = imports.iter().any(|u| {
        u["unused"].as_str().is_some_and(|s| s.contains("HashSet"))
            || u["name"].as_str().is_some_and(|n| n.contains("HashSet"))
    });
    assert!(
        mentions_hashset,
        "HashSet from grouped use should be reported as unused; got {payloads:?}"
    );
    // Critically, the *used* identifier HashMap must NOT be reported. If the
    // handler treats the whole grouped use as one opaque identifier it'll
    // either flag both or neither — both modes are wrong.
    let any_falsely_flags_hashmap = imports
        .iter()
        .any(|u| u["unused"].as_str().is_some_and(|s| s == "HashMap"));
    assert!(
        !any_falsely_flags_hashmap,
        "HashMap is used (HashMap::new()) and must not appear in `unused`; got {payloads:?}"
    );
}

/// `tracedecay_dead_code` must not treat non-reference edges like
/// `annotates` or `derives_macro` as "this function is alive" evidence. A
/// private helper with no callers but an `#[inline]` (or any other
/// attribute) has an incoming `annotates` edge from the synthesised
/// annotation_usage node, which `NOT EXISTS (target = id AND kind !=
/// 'contains')` accepted as a live reference.
#[tokio::test]
async fn dead_code_flags_unreferenced_fn_with_attribute() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
fn caller() {
    used_helper();
}

#[inline]
fn used_helper() {}

#[inline]
fn dead_helper_with_attr() {}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(&cg, "tracedecay_dead_code", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let symbols = output["symbols"].as_array().unwrap();
    let names: Vec<&str> = symbols.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(
        names.contains(&"dead_helper_with_attr"),
        "private fn with #[inline] and no callers should be dead; got {names:?}"
    );
    assert!(
        !names.contains(&"used_helper"),
        "used_helper has a real caller and must NOT appear; got {names:?}"
    );
}

/// `tracedecay_unsafe_patterns` detected the unsafe block (the JSON payload
/// was correct) but the Markdown renderer dropped every finding and printed
/// "No diagnostics.", so agents saw nothing. The default (Markdown) response
/// must surface the site.
#[tokio::test]
async fn unsafe_patterns_reports_unsafe_block_in_markdown_and_json() {
    let (cg, _project) = setup_unsafe_block_fixture().await;

    // Markdown is the runtime default; request it explicitly so the test helper
    // (which force-injects `format=json` for tools outside its allowlist) does
    // not override it.
    let md = handle_tool_call(
        &cg,
        "tracedecay_unsafe_patterns",
        json!({"format": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&md.value);
    assert!(
        !text.contains("No diagnostics"),
        "renderer regression: markdown swallowed the finding: {text}"
    );
    assert!(text.contains("## Risky Patterns"), "got: {text}");
    assert!(
        text.contains("UNSAFE_BLOCK at src/lib.rs:7"),
        "markdown must report the unsafe block with file:line: {text}"
    );
    assert!(
        text.contains("raw_total_len"),
        "markdown must name the enclosing symbol: {text}"
    );

    // JSON output must carry the same structured match.
    let js = handle_tool_call(
        &cg,
        "tracedecay_unsafe_patterns",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&js.value)).unwrap();
    assert_eq!(payload["match_count"], 1, "payload: {payload}");
    assert_eq!(payload["by_kind"]["unsafe_block"], 1, "payload: {payload}");
    let m = &payload["matches"][0];
    assert_eq!(m["kind"], "unsafe_block");
    assert_eq!(m["file"], "src/lib.rs");
    assert_eq!(m["line"], 7);
    assert!(
        m["enclosing"]
            .as_str()
            .unwrap_or_default()
            .ends_with("raw_total_len"),
        "payload: {payload}"
    );

    // The safe function must NOT be flagged.
    assert!(
        !text.contains("safe_add"),
        "safe code should produce no findings: {text}"
    );
}
