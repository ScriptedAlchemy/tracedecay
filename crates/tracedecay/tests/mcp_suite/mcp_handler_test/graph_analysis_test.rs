#![cfg(feature = "test-transport")]

use crate::common::fixture::git_run;
use crate::support::*;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::project::TraceDecay;
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

async fn setup_empty_analysis_project() -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), "").unwrap();
    })
    .await
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

async fn setup_integration_test_risk_project() -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(write_integration_test_risk_sources).await
}

async fn setup_test_risk_non_src_fixture() -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(|project| {
        write_integration_test_risk_sources(project);
        fs::write(
            project.join("build.rs"),
            "fn build_script_helper(flag: &str) -> String { format!(\"cargo:warning={flag}\") }\n\
             fn main() { println!(\"{}\", build_script_helper(\"ok\")); }\n",
        )
        .unwrap();
    })
    .await
}

async fn setup_workspace_test_risk_fixture() -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("crates/demo/src")).unwrap();
        fs::create_dir_all(project.join("crates/demo/tests")).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/demo\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            project.join("crates/demo/Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            project.join("crates/demo/src/lib.rs"),
            "pub fn public_entry() -> usize { helper() }\nfn helper() -> usize { 1 }\n",
        )
        .unwrap();
        fs::write(
            project.join("crates/demo/tests/integration.rs"),
            "use demo::public_entry;\n#[test]\nfn covers_public_entry() { assert_eq!(public_entry(), 1); }\n",
        )
        .unwrap();
    })
    .await
}

async fn setup_ts_describe_it_project() -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(|project| {
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
    .await
}

async fn setup_unsafe_block_fixture() -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(|project| {
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
/// memory-safety reason for this to be `unsafe`, exactly the needless kind a
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
    .await
}

async fn init_test_project(project: &Path) -> MountedProductionProject {
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
    mounted
}

#[tokio::test]
async fn constructors_distinguishes_explicit_update_and_missing_fields() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(
        project_root.join("src/lib.rs"),
        r#"
#[derive(Default)]
pub struct BuildOptions {
    pub name: String,
    pub retries: u8,
    pub verbose: bool,
}

pub fn explicit() -> BuildOptions {
    BuildOptions { name: String::new(), retries: 3, verbose: true }
}

pub fn updated() -> BuildOptions {
    BuildOptions { name: String::new(), ..Default::default() }
}

pub fn incomplete() -> BuildOptions {
    BuildOptions { name: String::new() }
}

pub fn recovered() -> BuildOptions {
    BuildOptions { name: String::new(), retries: }
}
"#,
    )
    .unwrap();
    let graph = init_test_project(&project_root).await;

    let result = handle_tool_call(
        &graph,
        "tracedecay_constructors",
        json!({"struct": "BuildOptions"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    let sites = payload["sites"].as_array().expect("constructor sites");
    assert_eq!(
        sites.len(),
        4,
        "all indexed literals must be returned: {payload}"
    );
    assert_eq!(payload["candidate_count"], 1);
    assert_eq!(payload["resolution_status"], "unverified");
    assert_eq!(payload["resolution_reason"], "syntax_only_simple_name");

    assert_eq!(sites[0]["fields"], json!(["name", "retries", "verbose"]));
    assert_eq!(sites[0]["update_fields"], json!([]));
    assert_eq!(sites[0]["missing_fields"], json!([]));
    assert_eq!(sites[0]["field_coverage"], "complete");

    assert_eq!(sites[1]["fields"], json!(["name"]));
    assert_eq!(sites[1]["update_fields"], json!(["retries", "verbose"]));
    assert_eq!(sites[1]["missing_fields"], json!([]));
    assert_eq!(sites[1]["field_coverage"], "complete");

    assert_eq!(sites[2]["fields"], json!(["name"]));
    assert_eq!(sites[2]["update_fields"], json!([]));
    assert_eq!(sites[2]["missing_fields"], json!(["retries", "verbose"]));
    assert_eq!(sites[2]["field_coverage"], "complete");

    assert_eq!(sites[3]["fields"], json!(["name", "retries"]));
    assert_eq!(sites[3]["update_fields"], json!([]));
    assert_eq!(sites[3]["missing_fields"], json!([]));
    assert_eq!(sites[3]["field_coverage"], "unknown");
}

#[tokio::test]
async fn constructors_marks_same_name_struct_resolution_unknown() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(
        project_root.join("src/lib.rs"),
        r#"
pub mod first {
    pub struct Options { pub one: u8 }
    pub fn build() -> Options { Options { one: 1 } }
}
pub mod second {
    pub struct Options { pub two: u8 }
    pub fn build() -> Options { Options { two: 2 } }
}
"#,
    )
    .unwrap();
    let graph = init_test_project(&project_root).await;

    let result = handle_tool_call(
        &graph,
        "tracedecay_constructors",
        json!({"struct": "Options"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    assert_eq!(payload["candidate_count"], 2);
    assert_eq!(payload["resolution_status"], "unverified");
    assert_eq!(payload["resolution_reason"], "ambiguous_simple_name");
    assert!(payload["expected_fields"].is_null());
    let sites = payload["sites"].as_array().expect("constructor sites");
    assert_eq!(
        sites.len(),
        2,
        "both syntax sites remain visible: {payload}"
    );
    assert!(sites.iter().all(|site| {
        site["field_coverage"] == "unknown"
            && site["update_fields"] == json!([])
            && site["missing_fields"] == json!([])
    }));
}

#[tokio::test]
async fn unmounted_files_ignores_comment_quotes_when_reading_config_entries() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src/app")).unwrap();
    fs::write(
        project_root.join("package.json"),
        r#"{"name":"dashboard","private":true}"#,
    )
    .unwrap();
    fs::write(
        project_root.join("rsbuild.config.ts"),
        r#"// Canonical dashboard build. build.rs embeds this build's output into the
// binary served at `/`, including every client-routed workspace.
export default defineConfig({
  source: {
    entry: { index: './src/app/main.tsx' },
    dynamicEntry: `./src/app/${page}.ts`,
  },
});
"#,
    )
    .unwrap();
    fs::write(
        project_root.join("src/app/main.tsx"),
        "import './boot';\nexport const app = 1;\n",
    )
    .unwrap();
    fs::write(
        project_root.join("src/app/boot.ts"),
        "export const boot = 1;\n",
    )
    .unwrap();
    fs::write(
        project_root.join("src/app/orphan.ts"),
        "export const orphan = 1;\n",
    )
    .unwrap();
    let graph = init_test_project(&project_root).await;

    let result = handle_tool_call(
        &graph,
        "tracedecay_unmounted_files",
        json!({"ecosystem": "typescript"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);
    let files = payload["unmounted"]
        .as_array()
        .expect("unmounted file rows")
        .iter()
        .filter_map(|row| row["file"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(files, vec!["src/app/orphan.ts"], "{payload}");
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
    let cg = production_composition_fixture().await;
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
    let cg = production_composition_fixture().await;
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
    let cg = production_composition_fixture().await;
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
    let cg = production_composition_fixture().await;
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

/// The fixture's call graph is a straight chain (`main` -> `helper` ->
/// `format_greeting`) with no symbol calling itself or looping back, so a
/// correct analysis finds no recursion. Resolving a symbol on that chain first
/// proves the call edges are present, which is what makes zero meaningful.
#[tokio::test]
async fn test_recursion() {
    let cg = production_composition_fixture().await;
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
    let cg = production_composition_fixture().await;
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
/// wild cross-type "matches", e.g. `Biquad::new` pairing with an unrelated
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

    let cg = init_test_project(project).await;

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
        "Biquad::* and Adaa::* must not cross-match, got matches: {matched:?}"
    );
    assert_eq!(
        output["matched"].as_u64(),
        Some(0),
        "matched count must be 0; output={output}"
    );
}

/// Sanity: when the same parent type name exists in both dirs, methods do
/// match, confirming the parent-aware key isn't too strict.
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

    let cg = init_test_project(project).await;

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
    let cg = production_composition_fixture().await;
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
async fn port_order_sorts_a_tied_level_before_applying_the_limit() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(
        project_root.join("src/lib.rs"),
        "pub fn zeta() {}\npub fn alpha() {}\npub fn middle() {}\n",
    )
    .unwrap();
    let cg = init_test_project(&project_root).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_port_order",
        json!({"source_dir": "src", "kinds": ["function"], "limit": 2}),
        None,
        None,
    )
    .await
    .unwrap();
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let names = output["levels"][0]["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(names, ["zeta", "alpha"]);
    assert_eq!(output["returned"], json!(2));
}

#[tokio::test]
async fn test_rename_preview_not_found() {
    let cg = setup_empty_analysis_project().await;
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

    let cg = init_test_project(project).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_commit_context",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();

    let output = commit_context_json(&result.value);
    assert_eq!(result.value.get("isError"), None);
    assert_eq!(
        output,
        json!({
            "changed_files": [],
            "symbols_by_role": {},
            "suggested_category": null,
            "recent_commits": ["init"],
            "summary": "No changes detected.",
        })
    );
}

/// Staged source and test files are what an agent sees when drafting a
/// commit: file roles, the symbols in those files, and the category that
/// follows from those roles.
#[tokio::test]
async fn commit_context_staged_source_and_test_reports_symbols() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    seed_commit(
        project,
        &[
            ("Cargo.toml", BILLING_MANIFEST),
            ("src/lib.rs", "pub fn baseline() -> i64 { 0 }\n"),
            ("tests/invoice_test.rs", "fn baseline_check() {}\n"),
        ],
        "seed context",
    );
    write_project_file(
        project,
        "src/lib.rs",
        "pub fn billed_total() -> i64 {\n    1\n}\n",
    );
    write_project_file(
        project,
        "tests/invoice_test.rs",
        "fn covers_billed_total() {}\n",
    );
    git_run(project, &["add", "src/lib.rs", "tests/invoice_test.rs"]);

    let host = init_test_project(project).await;
    let result = handle_tool_call(
        &host,
        "tracedecay_commit_context",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    close_test_graph(host).await;

    assert_eq!(result.value.get("isError"), None);
    assert_eq!(
        commit_context_json(&result.value),
        json!({
            "changed_files": [
                {"file": "src/lib.rs", "role": "source", "symbols": 1},
                {"file": "tests/invoice_test.rs", "role": "test", "symbols": 1}
            ],
            "symbols_by_role": {
                "source": [{
                    "name": "billed_total",
                    "kind": "function",
                    "file": "src/lib.rs",
                    "line": 0
                }],
                "test": [{
                    "name": "covers_billed_total",
                    "kind": "function",
                    "file": "tests/invoice_test.rs",
                    "line": 0
                }]
            },
            "suggested_category": "feature/fix (source + tests)",
            "recent_commits": ["seed context"],
            "summary": "2 file(s) changed, 2 symbol(s) affected",
        })
    );
}

/// Config and docs changes are not source work. Config files collapse to one
/// summary entry instead of one symbol per key.
#[tokio::test]
async fn commit_context_config_and_docs_report_chore() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    seed_commit(
        project,
        &[
            ("Cargo.toml", BILLING_MANIFEST),
            ("src/lib.rs", "pub fn untouched() {}\n"),
            ("billing.cfg", "timeout=1\n"),
            ("notes.txt", "committed note\n"),
        ],
        "seed context",
    );
    write_project_file(project, "billing.cfg", "timeout=9\n");
    write_project_file(project, "notes.txt", "Ship the invoice total.\n");
    git_run(project, &["add", "billing.cfg", "notes.txt"]);

    let host = init_test_project(project).await;
    let result = handle_tool_call(
        &host,
        "tracedecay_commit_context",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    close_test_graph(host).await;

    assert_eq!(result.value.get("isError"), None);
    assert_eq!(
        commit_context_json(&result.value),
        json!({
            "changed_files": [
                {"file": "billing.cfg", "role": "config", "symbols": 0},
                {"file": "notes.txt", "role": "docs", "symbols": 0}
            ],
            "symbols_by_role": {
                "config": [{
                    "file": "billing.cfg",
                    "kind": "config_summary",
                    "config_keys": 0
                }]
            },
            "suggested_category": "chore/docs/config",
            "recent_commits": ["seed context"],
            "summary": "2 file(s) changed, 1 symbol(s) affected",
        })
    );
}

/// `staged_only` is the difference between "what will this commit contain"
/// and "what is dirty". An unstaged docs edit must appear only when the
/// caller asks for every uncommitted change.
#[tokio::test]
async fn commit_context_staged_only_excludes_unstaged_file() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    seed_commit(
        project,
        &[
            ("Cargo.toml", BILLING_MANIFEST),
            ("src/lib.rs", "pub fn baseline() -> i64 { 0 }\n"),
            ("notes.txt", "committed note\n"),
        ],
        "seed context",
    );
    // gix compares the working-tree mtime, in whole seconds, with the index
    // stat. A write in the same second as `git commit` is invisible, so the
    // unstaged edit has to land in a later second.
    std::thread::sleep(Duration::from_secs(2));
    write_project_file(project, "notes.txt", "unstaged note\n");
    write_project_file(
        project,
        "src/lib.rs",
        "pub fn staged_total() -> i64 {\n    1\n}\n",
    );
    git_run(project, &["add", "src/lib.rs"]);

    let host = init_test_project(project).await;
    let staged = handle_tool_call(
        &host,
        "tracedecay_commit_context",
        json!({"format": "json", "staged_only": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let everything = handle_tool_call(
        &host,
        "tracedecay_commit_context",
        json!({"format": "json", "staged_only": false}),
        None,
        None,
    )
    .await
    .unwrap();
    close_test_graph(host).await;

    assert_eq!(staged.value.get("isError"), None);
    assert_eq!(
        commit_context_json(&staged.value),
        json!({
            "changed_files": [
                {"file": "src/lib.rs", "role": "source", "symbols": 1}
            ],
            "symbols_by_role": {
                "source": [{
                    "name": "staged_total",
                    "kind": "function",
                    "file": "src/lib.rs",
                    "line": 0
                }]
            },
            "suggested_category": "feature/fix/refactor",
            "recent_commits": ["seed context"],
            "summary": "1 file(s) changed, 1 symbol(s) affected",
        })
    );
    assert_eq!(everything.value.get("isError"), None);
    assert_eq!(
        commit_context_json(&everything.value),
        json!({
            "changed_files": [
                {"file": "notes.txt", "role": "docs", "symbols": 0},
                {"file": "src/lib.rs", "role": "source", "symbols": 1}
            ],
            "symbols_by_role": {
                "source": [{
                    "name": "staged_total",
                    "kind": "function",
                    "file": "src/lib.rs",
                    "line": 0
                }]
            },
            "suggested_category": "feature/fix/refactor",
            "recent_commits": ["seed context"],
            "summary": "2 file(s) changed, 1 symbol(s) affected",
        })
    );
}

/// A repository whose HEAD does not name a commit cannot describe a commit.
/// The tool reports that as a git status failure, not an empty success.
#[tokio::test]
async fn commit_context_unborn_head_is_git_status_error() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    seed_commit(
        project,
        &[("src/lib.rs", "pub fn baseline() {}\n")],
        "seed context",
    );
    let host = init_test_project(project).await;
    git_run(project, &["symbolic-ref", "HEAD", "refs/heads/unborn"]);

    let result = handle_tool_call(
        &host,
        "tracedecay_commit_context",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    close_test_graph(host).await;

    assert_eq!(result.value.get("isError"), Some(&json!(true)));
    assert_eq!(
        commit_context_json(&result.value),
        json!({
            "error": {
                "kind": "git",
                "operation": "status",
                "message": "cannot peel HEAD to commit: Branch 'refs/heads/unborn' does not have any commits",
            }
        })
    );
}

const BILLING_MANIFEST: &str =
    "[package]\nname = \"billing\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn seed_commit(project: &Path, files: &[(&str, &str)], message: &str) {
    fs::create_dir_all(project).unwrap();
    for (path, body) in files {
        write_project_file(project, path, body);
    }
    git_run(project, &["init"]);
    git_run(project, &["add", "."]);
    git_run(project, &["commit", "-m", message]);
}

fn write_project_file(project: &Path, path: &str, body: &str) {
    let full = project.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full, body).unwrap();
}

fn commit_context_json(value: &Value) -> Value {
    serde_json::from_str(extract_text(value)).unwrap_or_else(|error| {
        panic!(
            "tracedecay_commit_context did not return JSON: {error}\n{}",
            extract_text(value)
        )
    })
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

    let cg = init_test_project(project).await;

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

/// `details=true` must surface raw counts + interpretation per dimension,
/// so callers don't have to compose six separate tools to reproduce the
/// breakdown.
#[tokio::test]
async fn test_health_detailed_includes_raw_signals() {
    let cg = production_composition_fixture().await;
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

/// Five Rust files. The calls a reader can see are `src/ui.rs` calling
/// `panel::draw` in `src/ui/panel.rs` and `crate::core::store::load` in
/// `src/core/store.rs`. `mod` declarations are not themselves calls.
fn write_dsm_coupling_sources(project: &Path) {
    fs::create_dir_all(project.join("src/ui")).unwrap();
    fs::create_dir_all(project.join("src/core")).unwrap();
    fs::write(project.join("src/lib.rs"), "mod ui;\nmod core;\n").unwrap();
    fs::write(
        project.join("src/ui.rs"),
        "mod panel;\nuse crate::core::store::load;\n\n\
         pub fn render() -> i32 {\n    panel::draw() + load()\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/ui/panel.rs"),
        "pub fn draw() -> i32 { 1 }\n",
    )
    .unwrap();
    fs::write(project.join("src/core.rs"), "pub mod store;\n").unwrap();
    fs::write(
        project.join("src/core/store.rs"),
        "pub fn load() -> i32 { 4 }\n",
    )
    .unwrap();
}

async fn call_dsm(host: &ProductionCompositionFixture, args: Value) -> String {
    let result = handle_tool_call(host, "tracedecay_dsm", args, None, None)
        .await
        .unwrap_or_else(|error| panic!("tracedecay_dsm failed over production MCP: {error}"));
    extract_text(&result.value).to_owned()
}

fn parse_dsm_json(text: &str) -> Value {
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tracedecay_dsm did not return JSON: {error}\n{text}"))
}

fn coupling_stats() -> Value {
    json!({
        "files": 5,
        "edges": 2,
        "density": 0.1,
        "clusters": 3,
        "largest_cluster": 3
    })
}

fn coupling_clusters() -> Value {
    json!([
        {
            "directory": "src",
            "file_count": 3,
            "internal_edges": 0,
            "outgoing_edges": 2,
            "incoming_edges": 0,
            "boundary_edges": 2
        },
        {
            "directory": "src/core",
            "file_count": 1,
            "internal_edges": 0,
            "outgoing_edges": 0,
            "incoming_edges": 1,
            "boundary_edges": 1
        },
        {
            "directory": "src/ui",
            "file_count": 1,
            "internal_edges": 0,
            "outgoing_edges": 0,
            "incoming_edges": 1,
            "boundary_edges": 1
        }
    ])
}

/// Dependency pairs in the matrix, independent of the tie order among files
/// that share an edge count. The matrix sort is stable over a `HashMap`, so
/// tied rows are not a stable literal.
fn matrix_edges(matrix: &Value) -> Vec<(String, String)> {
    let files = matrix["files"]
        .as_array()
        .expect("matrix.files")
        .iter()
        .map(|file| file.as_str().expect("matrix file name").to_owned())
        .collect::<Vec<_>>();
    let rows = matrix["matrix"].as_array().expect("matrix.matrix");
    assert_eq!(rows.len(), files.len(), "matrix is not square: {matrix}");
    let mut edges = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        let cells = row.as_array().expect("matrix row");
        assert_eq!(cells.len(), files.len(), "matrix row {row_index}: {matrix}");
        for (column_index, cell) in cells.iter().enumerate() {
            let present = cell
                .as_u64()
                .unwrap_or_else(|| panic!("matrix cell is not 0 or 1: {cell} in {matrix}"));
            if row_index == column_index {
                assert_eq!(
                    present, 0,
                    "DSM matrix has a self-edge at {}",
                    files[row_index]
                );
            }
            match present {
                0 => {}
                1 => edges.push((files[row_index].clone(), files[column_index].clone())),
                other => panic!("matrix cell {other} is not 0 or 1 in {matrix}"),
            }
        }
    }
    edges.sort();
    edges
}

#[tokio::test]
async fn test_dsm_reports_authored_file_dependencies() {
    let host = production_composition_fixture_with_sources(write_dsm_coupling_sources).await;
    wait_for_current_graph(&host).await;

    let stats_markdown = call_dsm(&host, json!({})).await;
    // Density is a JSON number. The markdown renderer reads it with
    // `field_str`, which only accepts strings, so the default line is
    // `**density:** ` with an empty value. The JSON assertion below is the
    // one that checks the rounded number.
    assert_eq!(
        stats_markdown,
        "\
## Design Structure Matrix
**shape:** stats
**files:** 5
**edges:** 2
**density:** 
**clusters:** 3
**largest_cluster:** 3

### Top Clusters
- src: 3 files; 0 internal; 2 boundary (2 out, 0 in)
- src/core: 1 files; 0 internal; 1 boundary (0 out, 1 in)
- src/ui: 1 files; 0 internal; 1 boundary (0 out, 1 in)
"
    );

    let stats = parse_dsm_json(&call_dsm(&host, json!({ "format": "json" })).await);
    assert_eq!(
        stats,
        json!({
            "shape": "stats",
            "stats": coupling_stats(),
            "clusters": coupling_clusters(),
        })
    );

    let named_stats =
        parse_dsm_json(&call_dsm(&host, json!({ "format": "json", "shape": "stats" })).await);
    assert_eq!(
        named_stats,
        json!({
            "shape": "stats",
            "stats": coupling_stats(),
            "clusters": coupling_clusters(),
        })
    );

    let clusters =
        parse_dsm_json(&call_dsm(&host, json!({ "format": "json", "shape": "clusters" })).await);
    assert_eq!(
        clusters,
        json!({
            "shape": "clusters",
            "stats": coupling_stats(),
            "clusters": coupling_clusters(),
        })
    );

    // An unrecognized shape is the stats report, not an empty success.
    let unknown =
        parse_dsm_json(&call_dsm(&host, json!({ "format": "json", "shape": "layers" })).await);
    assert_eq!(
        unknown,
        json!({
            "shape": "stats",
            "stats": coupling_stats(),
            "clusters": coupling_clusters(),
        })
    );

    let matrix =
        parse_dsm_json(&call_dsm(&host, json!({ "format": "json", "shape": "matrix" })).await);
    assert_eq!(matrix["shape"], "matrix");
    assert_eq!(matrix["stats"], coupling_stats());
    assert_eq!(matrix["clusters"], coupling_clusters());
    let mut files = matrix["matrix"]["files"]
        .as_array()
        .expect("matrix files")
        .iter()
        .map(|file| file.as_str().expect("short name").to_owned())
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(
        files,
        ["core.rs", "lib.rs", "panel.rs", "store.rs", "ui.rs"]
    );
    assert_eq!(matrix["matrix"]["note"], "Top 5 files by edge count shown");
    assert_eq!(
        matrix_edges(&matrix["matrix"]),
        vec![
            ("ui.rs".to_owned(), "panel.rs".to_owned()),
            ("ui.rs".to_owned(), "store.rs".to_owned()),
        ]
    );

    let top_file = parse_dsm_json(
        &call_dsm(
            &host,
            json!({ "format": "json", "shape": "matrix", "max_files": 1 }),
        )
        .await,
    );
    assert_eq!(
        top_file["matrix"],
        json!({
            "files": ["ui.rs"],
            "matrix": [[0]],
            "note": "Top 1 files by edge count shown"
        })
    );

    let ui_only =
        parse_dsm_json(&call_dsm(&host, json!({ "format": "json", "path": "src/ui" })).await);
    assert_eq!(
        ui_only,
        json!({
            "shape": "stats",
            "stats": {
                "files": 1,
                "edges": 0,
                "density": 0.0,
                "clusters": 1,
                "largest_cluster": 1
            },
            "clusters": [{
                "directory": "src/ui",
                "file_count": 1,
                "internal_edges": 0,
                "outgoing_edges": 0,
                "incoming_edges": 0,
                "boundary_edges": 0
            }]
        })
    );

    let missing = call_dsm(&host, json!({ "path": "src/missing" })).await;
    assert_eq!(
        missing,
        "\
## Design Structure Matrix
**shape:** stats
**files:** 0
**edges:** 0
**density:** 
**clusters:** 0
**largest_cluster:** 0

### Top Clusters
_No dependency clusters found._
"
    );
}

#[tokio::test]
async fn test_test_risk() {
    let cg = production_composition_fixture().await;
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
    let cg = setup_integration_test_risk_project().await;
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
async fn test_test_risk_scopes_workspace_source_before_following_external_test_callers() {
    let cg = setup_workspace_test_risk_fixture().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_test_risk",
        json!({
            "path": "crates/demo/src/lib.rs",
            "limit": 10,
            "include_tested": true
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(parsed["summary"]["total_functions"].as_u64(), Some(2));
    assert_eq!(parsed["summary"]["tested"].as_u64(), Some(2));
    assert!(
        parsed["risks"]
            .as_array()
            .is_some_and(|risks| risks.iter().all(|risk| risk["has_test"] == true)),
        "the out-of-scope integration test should attribute both scoped functions: {text}"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_test_risk_attributes_ts_describe_it_tests() {
    let cg = setup_ts_describe_it_project().await;
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
    let cg = setup_ts_describe_it_project().await;
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
    let cg = setup_test_risk_non_src_fixture().await;
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
    let cg = init_test_project(project).await;
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
    let cg = init_test_project(project).await;

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
    let cg = init_test_project(project).await;

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
        "pub fn recurse(n: u32) -> u32 {\n    if n == 0 { 0 } else { recurse(n - 1) }\n}\n\npub fn nonrecursive() -> u32 { 42 }\n",
    )
    .unwrap();
    let cg = init_test_project(project).await;
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
        .await
        .unwrap();
    let output = extract_json(&result.value);
    assert_eq!(
        public_recursion_report(&output),
        json!({
            "cycle_count": 1,
            "cycles": [{
                "length": 1,
                "chain": [
                    {"name": "recurse", "kind": "function", "file": "src/lib.rs", "line": 1},
                    {"name": "recurse", "kind": "function", "file": "src/lib.rs", "line": 1}
                ]
            }]
        }),
        "direct recursion must be the only cycle, and `nonrecursive` must stay out: {output}"
    );
    assert_reported_cycles_close(&output);
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
        "pub fn recurse(n: u32) -> u32 {\n    if n == 0 { 0 } else { recurse(n - 1) }\n}\n\npub struct Triplet {\n    rows: Vec<usize>,\n}\n\nimpl Triplet {\n    pub fn push(&mut self, row: usize) {\n        self.rows.push(row);\n    }\n}\n",
    )
    .unwrap();
    let cg = init_test_project(project).await;
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
        .await
        .unwrap();
    let output = extract_json(&result.value);
    assert_eq!(
        public_recursion_report(&output),
        json!({
            "cycle_count": 1,
            "cycles": [{
                "length": 1,
                "chain": [
                    {"name": "recurse", "kind": "function", "file": "src/lib.rs", "line": 1},
                    {"name": "recurse", "kind": "function", "file": "src/lib.rs", "line": 1}
                ]
            }]
        }),
        "`self.rows.push` must not become a cycle while `recurse` is reported: {output}"
    );
    assert_reported_cycles_close(&output);
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
    let cg = init_test_project(project).await;
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
        .await
        .unwrap();
    let output = extract_json(&result.value);
    assert_eq!(
        public_recursion_report(&output),
        json!({
            "cycle_count": 1,
            "cycles": [{
                "length": 3,
                "chain": [
                    {"name": "a", "kind": "function", "file": "src/lib.rs", "line": 2},
                    {"name": "b", "kind": "function", "file": "src/lib.rs", "line": 3},
                    {"name": "c", "kind": "function", "file": "src/lib.rs", "line": 4},
                    {"name": "a", "kind": "function", "file": "src/lib.rs", "line": 2}
                ]
            }]
        }),
        "the only cycle is a -> b -> c -> a: {output}"
    );
    assert_reported_cycles_close(&output);
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
    let cg = init_test_project(project).await;

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
    let cg = init_test_project(project).await;

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
    fs::write(
        project.join("src/lib.rs"),
        "pub fn target() {}\npub fn caller() { target(); }\n",
    )
    .unwrap();
    let cg = init_test_project(project).await;

    let abs_path = project.join("src/lib.rs");
    let abs_str = abs_path.to_string_lossy().to_string();
    let backslash_str = "src\\lib.rs";
    let cargo_output = format!(
        "error[E0001]: synthetic error\n  --> {abs_str}:1:1\n   |\n\nerror[E0002]: backslash form\n  --> {backslash_str}:1:1\n   |\n"
    );

    let result = handle_tool_call(
        &cg,
        "tracedecay_diagnose",
        json!({"cargo_output": cargo_output, "include_callers": true}),
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
    for diagnostic in output["diagnostics"].as_array().expect("diagnostics") {
        assert_eq!(diagnostic["node"]["name"], "target");
        let callers = diagnostic["callers"].as_array().expect("callers");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["name"], "caller");
        assert_eq!(callers[0]["file"], "src/lib.rs");
        assert_eq!(callers[0]["line"], 2);
    }
}

/// The resolver's kind-compatibility filter must apply to the same-file
/// blocklist branches too. Without it, common names like
/// `new`/`default`/`clone` can still bind a `Calls` reference to a
/// non-callable same-file symbol, e.g. a const literally named
/// `default`, when it's the only same-file match for a blocklisted
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
    let cg = init_test_project(project).await;

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
    let cg = init_test_project(project).await;

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
    let cg = init_test_project(project).await;
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
    let cg = init_test_project(project).await;
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
    // No cycle entry should mix both (a,b) and (c,d) names, that would
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
    let cg = init_test_project(project).await;
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
    let cg = init_test_project(project).await;
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
    let cg = init_test_project(project).await;
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

#[tokio::test]
async fn analysis_symbol_locations_are_one_based() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(
        project_root.join("package.json"),
        "{\"name\":\"analysis-locations\",\"private\":true,\"type\":\"module\"}\n",
    )
    .unwrap();
    fs::write(
        project_root.join("src/engine.ts"),
        "export class AuditEngine {\n  private total = 0;\n  private label = \"audit\";\n  private enabled = true;\n\n  add(value: number): number {\n    this.total += value;\n    return this.total;\n  }\n\n  reset(): void {\n    this.total = 0;\n  }\n\n  describe(): string {\n    return `${this.label}:${this.total}`;\n  }\n\n  evaluate(value: number): number {\n    if (!this.enabled) {\n      return 0;\n    }\n    if (value < 0) {\n      return -1;\n    }\n    if (value === 0) {\n      return this.total;\n    }\n    if (value % 2 === 0) {\n      return this.add(value);\n    }\n    if (value > 100) {\n      return value * 2;\n    }\n    return value + 1;\n  }\n}\n\nexport function sharedScore(value: number): number {\n  return value * 3;\n}\n\nexport function firstScore(value: number): number {\n  return sharedScore(value);\n}\n\nexport function secondScore(value: number): number {\n  return sharedScore(value + 1);\n}\n\nexport function thirdScore(value: number): number {\n  return sharedScore(value + 2);\n}\n",
    )
    .unwrap();
    fs::write(
        project_root.join("src/recursion.ts"),
        "export function factorial(value: number): number {\n  if (value <= 1) {\n    return 1;\n  }\n  return value * factorial(value - 1);\n}\n",
    )
    .unwrap();
    fs::write(
        project_root.join("src/hierarchy.ts"),
        "export interface Base {}\nexport interface Middle extends Base {}\nexport interface Leaf extends Middle {}\n",
    )
    .unwrap();
    fs::write(
        project_root.join("src/dead.ts"),
        "function abandonedHelper(): number { return 7; }\nexport function entry(): number { return 1; }\n",
    )
    .unwrap();
    let graph = init_test_project(&project_root).await;

    for (tool, arguments, collection, symbol, expected_line) in [
        (
            "tracedecay_hotspots",
            json!({"limit": 100, "format": "json"}),
            "hotspots",
            "sharedScore",
            39,
        ),
        (
            "tracedecay_dead_code",
            json!({"format": "json"}),
            "symbols",
            "abandonedHelper",
            1,
        ),
        (
            "tracedecay_inheritance_depth",
            json!({"format": "json"}),
            "ranking",
            "Leaf",
            3,
        ),
    ] {
        let result = handle_tool_call(&graph, tool, arguments, None, None)
            .await
            .unwrap();
        let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
        let item = output[collection]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == symbol)
            .unwrap_or_else(|| panic!("{tool} omitted {symbol}: {output}"));
        assert_eq!(item["line"], expected_line, "{tool} returned {item}");
    }

    for (tool, collection, symbol, expected_line) in [
        ("tracedecay_complexity", "ranking", "evaluate", 19),
        ("tracedecay_god_class", "ranking", "AuditEngine", 1),
    ] {
        let result = handle_tool_call(
            &graph,
            tool,
            json!({"limit": 100, "format": "json"}),
            None,
            None,
        )
        .await
        .unwrap();
        let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
        let item = output[collection]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == symbol)
            .unwrap_or_else(|| panic!("{tool} omitted {symbol}: {output}"));
        assert_eq!(item["line"], expected_line, "{tool} returned {item}");
    }

    let result = handle_tool_call(
        &graph,
        "tracedecay_recursion",
        json!({"limit": 100, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let factorial = output["cycles"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|cycle| cycle["chain"].as_array().unwrap())
        .find(|item| item["name"] == "factorial")
        .unwrap_or_else(|| panic!("tracedecay_recursion omitted factorial: {output}"));
    assert_eq!(factorial["line"], 1, "recursion returned {factorial}");

    close_test_graph(graph).await;
}

#[tokio::test]
async fn typescript_typed_variables_reach_public_type_relation_queries() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(
        project_root.join("package.json"),
        r#"{"name":"typescript-type-relations","private":true,"type":"module"}"#,
    )
    .unwrap();
    fs::write(
        project_root.join("src/types.ts"),
        "export interface Greeter { greet(): string }\n",
    )
    .unwrap();
    fs::write(
        project_root.join("src/values.ts"),
        "import type { Greeter } from './types';\n\
         export const primary: Greeter = { greet: () => 'primary' };\n\
         export let fallback: Greeter = primary;\n",
    )
    .unwrap();
    let graph = init_test_project(&project_root).await;
    let greeter_id = find_node_id(&graph, "Greeter").await;
    let variable_ids = [
        ("primary", find_node_id(&graph, "primary").await),
        ("fallback", find_node_id(&graph, "fallback").await),
    ];
    let request = |node_id: &str| {
        json!({
            "node_id": node_id,
            "scope": {
                "generation": tracedecay_contracts::UNPINNED_LATEST_GENERATION_SENTINEL,
                "path_prefix": Value::Null,
            },
            "meta": {
                "projection": "evidence",
                "order": "source_position",
                "cursor": Value::Null,
            },
        })
    };

    for (name, node_id) in &variable_ids {
        let result = call_production_tool(
            &graph.harness,
            &graph.project_root,
            "tracedecay_code_type_definition",
            request(node_id),
        )
        .await
        .expect("public type-definition request");
        let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
        let items = output
            .pointer("/outcome/value/payload/items")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{name} type-definition items missing: {output:#}"));
        assert_eq!(
            items
                .iter()
                .map(|item| (item["name"].as_str(), item["file"].as_str()))
                .collect::<Vec<_>>(),
            [(Some("Greeter"), Some("src/types.ts"))],
            "{name} must resolve its imported annotation through the public query: {output:#}"
        );
    }

    let result = call_production_tool(
        &graph.harness,
        &graph.project_root,
        "tracedecay_code_references",
        request(&greeter_id),
    )
    .await
    .expect("public references request");
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let mut typed_variables = output
        .pointer("/outcome/value/payload/items")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("reference items missing: {output:#}"))
        .iter()
        .filter(|item| item["edge_kind"] == "typeof")
        .filter_map(|item| item.pointer("/symbol/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    typed_variables.sort_unstable();
    assert_eq!(typed_variables, ["fallback", "primary"], "{output:#}");

    close_test_graph(graph).await;
}

#[tokio::test]
async fn typescript_interface_extends_drives_hierarchy_and_depth() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(
        project_root.join("src/settings.ts"),
        r#"
interface SettingsEditable { draft: string }
interface SettingsUnderReview extends SettingsEditable { review: string }
interface GenericEditable<T> { draft: T }
interface GenericReview extends GenericEditable<string> { review: string }
namespace left { export interface Base { left: string } }
namespace right { export interface Base { right: string } }
interface ScopedReview extends right.Base { review: string }
interface Renderer<T> { render(value: T): void }
class Screen implements Renderer<string> { render(value: string) {} }
const unrelated = 1;
function helper() { return unrelated; }
"#,
    )
    .unwrap();
    let cg = init_test_project(&project_root).await;
    let parent_id = find_node_id(&cg, "SettingsEditable").await;

    let hierarchy = handle_tool_call(
        &cg,
        "tracedecay_type_hierarchy",
        json!({"node_id": parent_id, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let hierarchy: Value = serde_json::from_str(extract_text(&hierarchy.value)).unwrap();
    assert!(
        hierarchy["tree"]
            .as_str()
            .unwrap()
            .contains("extends SettingsUnderReview"),
        "interface child missing from hierarchy: {hierarchy}"
    );

    let depth = handle_tool_call(
        &cg,
        "tracedecay_inheritance_depth",
        json!({"path": "src", "limit": 10}),
        None,
        None,
    )
    .await
    .unwrap();
    let depth: Value = serde_json::from_str(extract_text(&depth.value)).unwrap();
    let ranking = depth["ranking"].as_array().unwrap();
    assert_eq!(
        ranking
            .iter()
            .find(|item| item["name"] == "SettingsUnderReview")
            .and_then(|item| item["depth"].as_u64()),
        Some(1),
        "unexpected interface depth ranking: {ranking:?}"
    );
    assert!(
        ranking
            .iter()
            .all(|item| item["name"] != "helper" && item["name"] != "unrelated"),
        "non-hierarchy symbols leaked into inheritance depth: {ranking:?}"
    );

    for (parent, relation, child) in [
        ("GenericEditable", "extends", "GenericReview"),
        ("Renderer", "implements", "Screen"),
    ] {
        let parent_id = find_node_id(&cg, parent).await;
        let hierarchy = handle_tool_call(
            &cg,
            "tracedecay_type_hierarchy",
            json!({"node_id": parent_id, "format": "json"}),
            None,
            None,
        )
        .await
        .unwrap();
        let hierarchy: Value = serde_json::from_str(extract_text(&hierarchy.value)).unwrap();
        let expected = format!("{relation} {child}");
        assert!(
            hierarchy["tree"].as_str().unwrap().contains(&expected),
            "{expected} missing from hierarchy: {hierarchy}"
        );
    }

    let exact = handle_tool_call(
        &cg,
        "tracedecay_find_exact_symbol",
        json!({"name": "Base", "limit": 20}),
        None,
        None,
    )
    .await
    .unwrap();
    let exact: Value = serde_json::from_str(extract_text(&exact.value)).unwrap();
    let matches = exact["matches"].as_array().unwrap();
    let namespace_id = |namespace: &str| {
        matches
            .iter()
            .find(|item| {
                item["qualified_name"]
                    .as_str()
                    .is_some_and(|name| name.ends_with(&format!("::{namespace}::Base")))
            })
            .and_then(|item| item["id"].as_str())
            .unwrap_or_else(|| panic!("{namespace}.Base missing from exact symbols: {exact}"))
    };
    for (namespace, contains_child) in [("left", false), ("right", true)] {
        let hierarchy = handle_tool_call(
            &cg,
            "tracedecay_type_hierarchy",
            json!({"node_id": namespace_id(namespace), "format": "json"}),
            None,
            None,
        )
        .await
        .unwrap();
        let hierarchy: Value = serde_json::from_str(extract_text(&hierarchy.value)).unwrap();
        assert_eq!(
            hierarchy["tree"]
                .as_str()
                .unwrap()
                .contains("extends ScopedReview"),
            contains_child,
            "qualified parent bound to the wrong namespace: {hierarchy}"
        );
    }
}

/// `tracedecay_circular` must emit *disjoint* SCCs, no file should appear
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
    let cg = init_test_project(project).await;
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
    let cg = init_test_project(project).await;

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
    let cg = init_test_project(project).await;
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
/// 760k tokens. Config keys should collapse to one summary per change class.
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
    git_run(project, &["branch", "base"]);
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

    let cg = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_pr_context",
        json!({"base_ref": "base", "head_ref": "HEAD"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        output["added"],
        json!([{"file": "Cargo.toml", "kind": "config_summary", "config_keys": 50}])
    );
    assert_eq!(
        output["modified"],
        json!([{"file": "Cargo.toml", "kind": "config_summary", "config_keys": 1}])
    );
}

/// Author and committer identity are fixed so the commit objects, and therefore
/// the oids `tracedecay_pr_context` returns, are literals rather than values
/// read back out of the repository under test.
fn git_with_pinned_dates(dir: &Path, args: &[&str], date: Option<&str>) {
    let mut command = std::process::Command::new(
        tracedecay_runtime_core::git::try_git_program().expect("git executable"),
    );
    command
        .args([
            "-c",
            "core.hooksPath=.git/no-hooks",
            "-c",
            "gc.auto=0",
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir);
    if let Some(date) = date {
        command
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date);
    }
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("git {args:?} should spawn: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn pr_context_json(host: &impl AnalysisToolHost, arguments: Value) -> Value {
    let result = handle_tool_call(host, "tracedecay_pr_context", arguments, None, None)
        .await
        .unwrap_or_else(|error| {
            panic!("tracedecay_pr_context should return a tool result: {error}")
        });
    serde_json::from_str(extract_text(&result.value)).expect("PR context JSON")
}

fn symbol_facts(symbols: &Value) -> Value {
    let mut facts = symbols
        .as_array()
        .unwrap_or_else(|| panic!("symbol list must be an array, got {symbols}"))
        .iter()
        .map(|symbol| {
            if symbol.get("kind").and_then(Value::as_str) == Some("config_summary") {
                json!({
                    "config_keys": symbol["config_keys"],
                    "file": symbol["file"],
                    "kind": "config_summary",
                })
            } else {
                json!({
                    "file": symbol["file"],
                    "kind": symbol["kind"],
                    "line": symbol["line"],
                    "name": symbol["name"],
                })
            }
        })
        .collect::<Vec<_>>();
    facts.sort_by_key(ToString::to_string);
    Value::Array(facts)
}

fn pr_context_view(output: &Value) -> Value {
    json!({
        "affected_tests": output["affected_tests"],
        "analysis_complete": output["analysis_coverage"]["complete"],
        "base": output["base"],
        "base_oid": output["base_oid"],
        "changes": output["changes"],
        "commits": output["commits"],
        "coverage_status": output["symbol_changes_coverage"]["status"],
        "error": output["error"],
        "files_changed": output["files_changed"],
        "head": output["head"],
        "head_oid": output["head_oid"],
        "impacted_modules": output["impacted_modules"],
        "merge_base": output["merge_base"],
        "message": output["message"],
        "next_cursor": output["next_cursor"],
        "status": output["status"],
        "symbols_added": symbol_facts(&output["added"]),
        "symbols_modified": symbol_facts(&output["modified"]),
        "symbols_removed": symbol_facts(&output["removed"]),
        "symbols_added_count": output["symbols_added"],
        "symbols_modified_count": output["symbols_modified"],
        "symbols_removed_count": output["symbols_removed"],
        "symbol_page_complete": output["symbol_page"]["complete"],
        "symbol_page_has_more": output["symbol_page"]["has_more"],
        "symbol_page_limit": output["symbol_page"]["limit"],
        "symbol_page_selection": output["symbol_page"]["selection"],
        "test_files_changed": output["test_files_changed"],
    })
}

/// `tracedecay_pr_context` is the pull-request summary a caller asks for.
/// These literals are the tool result for one pinned history: `master` at
/// `bece36f8dada44933bc1bfa4c42faaccf77dcaab` and `feature` at
/// `bd4bc112c3374fbe23cb4ae2185cbf7945cb0e82`.
#[tokio::test]
async fn pr_context_reports_the_pinned_feature_summary() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    let project = project_root.as_path();
    git_with_pinned_dates(project, &["init", "-b", "master"], None);
    fs::write(
        project.join("src/announce.rs"),
        "pub fn announce() -> &'static str {\n    greet()\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "mod announce;\n\npub fn greet() -> &'static str {\n    \"hi\"\n}\n",
    )
    .unwrap();
    git_with_pinned_dates(project, &["add", "."], None);
    git_with_pinned_dates(
        project,
        &["commit", "-m", "base"],
        Some("2020-01-02T03:04:05Z"),
    );
    git_with_pinned_dates(project, &["switch", "-c", "feature"], None);
    fs::write(
        project.join("src/lib.rs"),
        "mod announce;\n\npub fn greet() -> &'static str {\n    \"hello\"\n}\n",
    )
    .unwrap();
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("tests/greet.rs"),
        "#[test]\nfn greet_says_hello() {}\n",
    )
    .unwrap();
    git_with_pinned_dates(project, &["add", "."], None);
    git_with_pinned_dates(
        project,
        &["commit", "-m", "say hello"],
        Some("2020-01-03T03:04:05Z"),
    );

    let host = init_test_project(project).await;
    let feature = pr_context_json(
        &host,
        json!({"format": "json", "base_ref": "master", "head_ref": "feature"}),
    )
    .await;
    assert_eq!(
        feature["graph_generation"], feature["symbol_changes_coverage"]["head_generation"],
        "the served graph must be the compared head generation: {feature}"
    );
    let mut feature_summary = json!({
        "affected_tests": [],
        "analysis_complete": true,
        "base": "master",
        "base_oid": "bece36f8dada44933bc1bfa4c42faaccf77dcaab",
        "changes": [
            {"path": "src/lib.rs", "status": "modified"},
            {"path": "tests/greet.rs", "status": "added"}
        ],
        "commits": [{"hash": "bd4bc112c3374fbe23cb4ae2185cbf7945cb0e82", "subject": "say hello"}],
        "coverage_status": "complete",
        "error": null,
        "files_changed": 2,
        "head": "feature",
        "head_oid": "bd4bc112c3374fbe23cb4ae2185cbf7945cb0e82",
        "impacted_modules": [],
        "merge_base": "bece36f8dada44933bc1bfa4c42faaccf77dcaab",
        "message": null,
        "next_cursor": null,
        "status": "complete",
        "symbols_added": [
            {"file": "tests/greet.rs", "kind": "annotation_usage", "line": 0, "name": "test"},
            {"file": "tests/greet.rs", "kind": "function", "line": 1, "name": "greet_says_hello"}
        ],
        "symbols_modified": [
            {"file": "src/lib.rs", "kind": "function", "line": 2, "name": "greet"}
        ],
        "symbols_removed": [],
        "symbols_added_count": 2,
        "symbols_modified_count": 1,
        "symbols_removed_count": 0,
        "symbol_page_complete": true,
        "symbol_page_has_more": false,
        "symbol_page_limit": 200,
        "symbol_page_selection": "stable_prefix",
        "test_files_changed": ["tests/greet.rs"],
    });
    for key in ["symbols_added", "symbols_modified", "symbols_removed"] {
        feature_summary[key] = symbol_facts(&feature_summary[key]);
    }
    assert_eq!(
        pr_context_view(&feature),
        feature_summary,
        "feature summary: {feature}"
    );

    let default_base =
        pr_context_json(&host, json!({"format": "json", "head_ref": "feature"})).await;
    assert_eq!(
        pr_context_view(&default_base),
        feature_summary,
        "omitting base_ref must select the repository default branch master: {default_base}"
    );

    let same_ref = pr_context_json(
        &host,
        json!({"format": "json", "base_ref": "feature", "head_ref": "feature"}),
    )
    .await;
    assert_eq!(
        pr_context_view(&same_ref),
        json!({
            "affected_tests": [],
            "analysis_complete": true,
            "base": "feature",
            "base_oid": "bd4bc112c3374fbe23cb4ae2185cbf7945cb0e82",
            "changes": [],
            "commits": [],
            "coverage_status": "complete",
            "error": null,
            "files_changed": 0,
            "head": "feature",
            "head_oid": "bd4bc112c3374fbe23cb4ae2185cbf7945cb0e82",
            "impacted_modules": [],
            "merge_base": "bd4bc112c3374fbe23cb4ae2185cbf7945cb0e82",
            "message": null,
            "next_cursor": null,
            "status": "complete",
            "symbols_added": [],
            "symbols_modified": [],
            "symbols_removed": [],
            "symbols_added_count": 0,
            "symbols_modified_count": 0,
            "symbols_removed_count": 0,
            "symbol_page_complete": true,
            "symbol_page_has_more": false,
            "symbol_page_limit": 200,
            "symbol_page_selection": "stable_prefix",
            "test_files_changed": [],
        }),
        "identical refs must report an empty summary, not an error: {same_ref}"
    );

    let cursor = handle_tool_call(
        &host,
        "tracedecay_pr_context",
        json!({"format": "json", "base_ref": "master", "head_ref": "feature", "cursor": 1}),
        None,
        None,
    )
    .await
    .expect_err("a numeric cursor is not a continuation token");
    assert_eq!(
        cursor.to_string(),
        "config error: tracedecay_pr_context failed over production MCP: tool execution failed: config error: PR context cursor must be a string"
    );

    close_test_graph(host).await;
}

/// A short branch name whose local tip and `origin` tip have diverged is not
/// a comparison. The tool must say so, naming both explicit refs, instead of
/// silently picking one side.
#[tokio::test]
async fn pr_context_names_both_refs_when_a_branch_has_diverged() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project = project_root.as_path();
    git_with_pinned_dates(project, &["init", "-b", "main"], None);
    fs::write(project.join("base.txt"), "base\n").unwrap();
    git_with_pinned_dates(project, &["add", "."], None);
    git_with_pinned_dates(project, &["commit", "-m", "base"], None);
    git_with_pinned_dates(
        project,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
        None,
    );
    fs::write(project.join("local.txt"), "local\n").unwrap();
    git_with_pinned_dates(project, &["add", "."], None);
    git_with_pinned_dates(project, &["commit", "-m", "local advance"], None);
    git_with_pinned_dates(
        project,
        &["switch", "--detach", "refs/remotes/origin/main"],
        None,
    );
    fs::write(project.join("remote.txt"), "remote\n").unwrap();
    git_with_pinned_dates(project, &["add", "."], None);
    git_with_pinned_dates(project, &["commit", "-m", "remote advance"], None);
    git_with_pinned_dates(
        project,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
        None,
    );
    git_with_pinned_dates(project, &["switch", "main"], None);

    // The refusal is decided by the git comparison, before graph enrichment.
    // Waiting for a code-graph publication never completes for this text-only
    // history, and a caller does not wait for one before asking.
    let isolation_root = project
        .parent()
        .expect("graph-analysis project must have an isolation parent");
    let harness =
        ProductionProjectCompositionHarnessV1::open(isolation_root, [project.to_path_buf()])
            .await
            .expect("production graph-analysis composition");
    let host = MountedProductionProject {
        harness,
        project_root: project.to_path_buf(),
    };
    let output = pr_context_json(
        &host,
        json!({"format": "json", "base_ref": "main", "head_ref": "HEAD"}),
    )
    .await;
    assert_eq!(
        output,
        json!({
            "error": {
                "kind": "git",
                "operation": "diff",
                "message": "branch 'main' has diverged local and origin tips; pass 'refs/heads/main' or 'origin/main' explicitly"
            }
        })
    );
    close_test_graph(host).await;
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
    let cg = init_test_project(project).await;

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
    let cg = setup_unsafe_block_fixture().await;

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

#[tokio::test]
async fn field_sites_applies_the_qualified_field_owner() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(
        project_root.join("src/lib.rs"),
        r#"
pub struct Target { pub value: u32, pub enabled: bool }
pub struct Other { pub value: u32 }

impl Target {
    pub fn read_both(&self, other: &Other) -> u32 {
        let target_value = self.value;
        let other_value = other.value;
        target_value + other_value
    }
}

pub fn read_both(target: &Target, other: &Other) -> u32 {
    let target_value = target.value;
    let other_value = other.value;
    target_value + other_value
}
pub fn read_when_enabled(target: &Target) -> u32 {
    if target.enabled { target.value } else { 0 }
}
pub fn write_both(target: &mut Target, other: &mut Other) {
    target.value = 7;
    other.value = 9;
}
"#,
    )
    .unwrap();
    let host = init_test_project(&project_root).await;

    let result = handle_tool_call(
        &host,
        "tracedecay_field_sites",
        json!({"field": "Target::value", "limit": 20, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(output["qualifier_applied"], true, "payload: {output}");
    assert_eq!(output["read_count"], 3, "payload: {output}");
    assert_eq!(output["write_count"], 1, "payload: {output}");
    assert!(
        output["read_sites"]
            .as_array()
            .is_some_and(|sites| sites.iter().all(|site| site["snippet"]
                .as_str()
                .is_some_and(|snippet| !snippet.contains("other.value")))),
        "payload: {output}"
    );
    assert!(
        output["write_sites"][0]["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains("target.value")),
        "payload: {output}"
    );

    for shadow_source in [
        r#"
pub struct Target { pub value: u32 }
pub struct Other { pub value: u32 }

pub fn closure_then_sibling(target: &Target) -> u32 {
    let read_other = |target: Other| target.value;
    read_other(Other { value: 3 }) + target.value
}
"#,
        r#"
pub struct Target { pub value: u32 }
pub struct Other { pub value: u32 }

pub fn if_let_then_sibling(target: &Target, other: Option<Other>) -> u32 {
    let read_other = if let Some(target) = other { target.value } else { 0 };
    read_other + target.value
}
"#,
        r#"
pub struct Target { pub value: u32 }
pub struct Other { pub value: u32 }

pub fn while_let_then_sibling(target: &Target, mut other: Option<Other>) -> u32 {
    let mut read_other = 0;
    while let Some(target) = other.take() { read_other += target.value; }
    read_other + target.value
}
"#,
    ] {
        let shadow_dir = test_temp_dir();
        let shadow_root = shadow_dir.path().join("project");
        fs::create_dir_all(shadow_root.join("src")).unwrap();
        fs::write(shadow_root.join("src/lib.rs"), shadow_source).unwrap();
        let shadow_host = init_test_project(&shadow_root).await;
        let error = expect_tool_error(
            handle_tool_call(
                &shadow_host,
                "tracedecay_field_sites",
                json!({"field": "Target::value", "format": "json"}),
                None,
                None,
            )
            .await,
        );
        assert!(
            error.contains("verified-field-qualifier-unavailable"),
            "shadowed receiver must not be attributed to the parameter owner: {error}"
        );
    }
}

#[tokio::test]
async fn field_sites_ignores_field_text_in_real_rust_literals() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tracedecay-session-memory/src/monitor_ring.rs"
    ));
    let line_of = |needle: &str| {
        source
            .lines()
            .position(|line| line.contains(needle))
            .map(|index| index as u64 + 1)
            .unwrap_or_else(|| panic!("fixture lost the line containing {needle:?}"))
    };
    let literal_line = line_of("\"monitor.mmap\"");
    let write_line = line_of("self.mmap = unsafe");
    fs::write(project_root.join("src/lib.rs"), source).unwrap();
    let host = init_test_project(&project_root).await;

    let result = handle_tool_call(
        &host,
        "tracedecay_field_sites",
        json!({"field": "MmapReader::mmap", "limit": 100, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(output["qualifier_applied"], true, "payload: {output}");
    assert_eq!(output["read_count"], 9, "payload: {output}");
    assert_eq!(output["write_count"], 1, "payload: {output}");
    assert!(
        output["read_sites"]
            .as_array()
            .is_some_and(|sites| sites.iter().all(|site| site["line"] != literal_line)),
        "string literal was reported as a field site: {output}"
    );
    assert_eq!(
        output["write_sites"][0]["line"], write_line,
        "payload: {output}"
    );
}

fn field_site(line: u64, enclosing: &str, snippet: &str) -> Value {
    json!({
        "file": "src/lib.rs",
        "line": line,
        "enclosing": enclosing,
        "snippet": snippet,
    })
}

const FIELD_BEHAVIOR_SOURCE: &str = r#"pub struct Counter {
    pub n: u32,
}

pub struct Gauge {
    pub n: u32,
}

impl Counter {
    pub fn read(&self, gauge: &Gauge) -> u32 {
        let kept = self.n;
        let other = gauge.n;
        kept + other
    }
}

pub fn bump(counter: &mut Counter, gauge: &mut Gauge) -> u32 {
    let same = counter.n == 0;
    counter.n = 1;
    gauge.n += 2;
    let borrowed = &mut counter.n;
    let shifted = counter.n << 1;
    counter.n <<= 1;
    let shown = "counter.n = 9";
    // counter.n = 8;
    let _ = (same, borrowed, shifted, shown);
    counter.n
}

pub fn arrow(counter: &Counter) -> u32 {
    take!(counter.n => 1);
    counter.n
}
"#;

const FIELD_QUALIFIED_SOURCE: &str = r#"pub struct Counter {
    pub n: u32,
}

pub struct Gauge {
    pub n: u32,
}

impl Counter {
    pub fn read(&self, gauge: &Gauge) -> u32 {
        let kept = self.n;
        let other = gauge.n;
        kept + other
    }
}

pub fn bump(counter: &mut Counter, gauge: &mut Gauge) -> u32 {
    let same = counter.n == 0;
    counter.n = 1;
    gauge.n += 2;
    let borrowed = &mut counter.n;
    let shifted = counter.n << 1;
    counter.n <<= 1;
    counter.n
}
"#;

async fn call_field_sites(host: &impl AnalysisToolHost, arguments: Value) -> Value {
    let result = handle_tool_call(host, "tracedecay_field_sites", arguments, None, None)
        .await
        .expect("production MCP field-sites call");
    extract_json(&result.value)
}

#[tokio::test]
async fn field_sites_behavior_reports_literal_read_and_write_sites() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), FIELD_BEHAVIOR_SOURCE).unwrap();
    let host = init_test_project(&project_root).await;

    let read_method = "src/lib.rs::Counter::read";
    let bump = "src/lib.rs::bump";
    let arrow = "src/lib.rs::arrow";
    let reads = json!([
        field_site(11, read_method, "let kept = self.n;"),
        field_site(12, read_method, "let other = gauge.n;"),
        field_site(18, bump, "let same = counter.n == 0;"),
        field_site(22, bump, "let shifted = counter.n << 1;"),
        field_site(27, bump, "counter.n"),
        field_site(31, arrow, "take!(counter.n => 1);"),
        field_site(32, arrow, "counter.n"),
    ]);
    let writes = json!([
        field_site(19, bump, "counter.n = 1;"),
        field_site(20, bump, "gauge.n += 2;"),
        field_site(21, bump, "let borrowed = &mut counter.n;"),
        field_site(23, bump, "counter.n <<= 1;"),
    ]);

    let bare = call_field_sites(&host, json!({"field": "n", "limit": 20, "format": "json"})).await;
    assert_eq!(
        bare,
        json!({
            "field": "n",
            "qualifier": null,
            "qualifier_applied": false,
            "write_count": 4,
            "read_count": 7,
            "write_sites": writes,
            "read_sites": reads,
        }),
        "bare field must partition assignment, compound assignment, mut borrow, and shift-assign as writes, and comparison, shift, and fat-arrow uses as reads"
    );

    let writes_only = call_field_sites(
        &host,
        json!({"field": "n", "writes_only": true, "format": "json"}),
    )
    .await;
    assert_eq!(
        writes_only,
        json!({
            "field": "n",
            "qualifier": null,
            "qualifier_applied": false,
            "write_count": 4,
            "write_sites": writes,
        }),
        "writes_only must omit the read list rather than return it empty"
    );

    // The scan stops only after both kinds have reached `limit`, so reads that
    // precede the first write stay in the result.
    let limited =
        call_field_sites(&host, json!({"field": "n", "limit": 1, "format": "json"})).await;
    assert_eq!(
        limited,
        json!({
            "field": "n",
            "qualifier": null,
            "qualifier_applied": false,
            "write_count": 1,
            "read_count": 3,
            "write_sites": [
                field_site(19, bump, "counter.n = 1;"),
            ],
            "read_sites": [
                field_site(11, read_method, "let kept = self.n;"),
                field_site(12, read_method, "let other = gauge.n;"),
                field_site(18, bump, "let same = counter.n == 0;"),
            ],
        }),
    );

    let missing_field = expect_tool_error(
        handle_tool_call(
            &host,
            "tracedecay_field_sites",
            json!({"format": "json"}),
            None,
            None,
        )
        .await,
    );
    assert_eq!(
        missing_field,
        "config error: tracedecay_field_sites failed over production MCP: tool execution failed: config error: tracedecay_field_sites requires a 'field' argument"
    );

    // `take!` is parseable Rust, but its body is a token tree, so the qualifier
    // path cannot bind `counter.n` to `Counter`. The first unbound site stops
    // the qualified census.
    let unbound_macro = expect_tool_error(
        handle_tool_call(
            &host,
            "tracedecay_field_sites",
            json!({"field": "Counter::n", "format": "json"}),
            None,
            None,
        )
        .await,
    );
    assert_eq!(
        unbound_macro,
        "config error: tracedecay_field_sites failed over production MCP: tool project route failed: reason_code=verified-field-qualifier-unavailable retryable=false: the indexed graph cannot bind field receiver '<unresolved>' at src/lib.rs:31 to exactly one qualified owner"
    );
    close_test_graph(host).await;

    let qualified_dir = test_temp_dir();
    let qualified_root = qualified_dir.path().join("project");
    fs::create_dir_all(qualified_root.join("src")).unwrap();
    fs::write(qualified_root.join("src/lib.rs"), FIELD_QUALIFIED_SOURCE).unwrap();
    let qualified_host = init_test_project(&qualified_root).await;
    let qualified = call_field_sites(
        &qualified_host,
        json!({"field": "Counter::n", "format": "json"}),
    )
    .await;
    assert_eq!(
        qualified,
        json!({
            "field": "Counter::n",
            "qualifier": "Counter",
            "qualifier_applied": true,
            "write_count": 3,
            "read_count": 4,
            "write_sites": [
                field_site(19, bump, "counter.n = 1;"),
                field_site(21, bump, "let borrowed = &mut counter.n;"),
                field_site(23, bump, "counter.n <<= 1;"),
            ],
            "read_sites": [
                field_site(11, read_method, "let kept = self.n;"),
                field_site(18, bump, "let same = counter.n == 0;"),
                field_site(22, bump, "let shifted = counter.n << 1;"),
                field_site(24, bump, "counter.n"),
            ],
        }),
        "Counter::n must drop Gauge sites"
    );

    let missing = call_field_sites(
        &qualified_host,
        json!({"field": "Missing::n", "format": "json"}),
    )
    .await;
    assert_eq!(
        missing,
        json!({
            "field": "Missing::n",
            "qualifier": "Missing",
            "qualifier_applied": true,
            "write_count": 0,
            "read_count": 0,
            "write_sites": [],
            "read_sites": [],
        }),
        "an unknown qualifier is an empty census, not every same-named field"
    );
    close_test_graph(qualified_host).await;
}

#[tokio::test]
async fn field_sites_behavior_refuses_unbound_qualified_receiver() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(
        project_root.join("src/lib.rs"),
        r#"pub struct Counter {
    pub n: u32,
}
pub struct Gauge {
    pub n: u32,
}

pub fn closure_then_sibling(counter: &Counter) -> u32 {
    let read_gauge = |counter: Gauge| counter.n;
    read_gauge(Gauge { n: 3 }) + counter.n
}
"#,
    )
    .unwrap();
    let host = init_test_project(&project_root).await;

    let error = expect_tool_error(
        handle_tool_call(
            &host,
            "tracedecay_field_sites",
            json!({"field": "Counter::n", "format": "json"}),
            None,
            None,
        )
        .await,
    );
    assert_eq!(
        error,
        "config error: tracedecay_field_sites failed over production MCP: tool project route failed: reason_code=verified-field-qualifier-unavailable retryable=false: the indexed graph cannot bind field receiver '<unresolved>' at src/lib.rs:9 to exactly one qualified owner"
    );

    close_test_graph(host).await;
}

async fn wait_for_current_graph(host: &impl AnalysisToolHost) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = handle_tool_call(
                host,
                "tracedecay_status",
                json!({
                    "format": "json",
                    "include_branch_diagnostics": false,
                    "include_storage_health": false,
                    "include_session_ingest": false,
                    "include_staleness": false,
                }),
                None,
                None,
            )
            .await
            .expect("typed project status while awaiting the current graph");
            let status: Value = serde_json::from_str(extract_text(&status.value))
                .expect("typed project status JSON");
            let freshness = &status["code_index_freshness"];
            let serving = &freshness["worktree"]["code_graph_serving"];
            match (
                freshness["status"].as_str(),
                serving["state"].as_str(),
                serving["reason"].as_str(),
                freshness["worktree"]["staleness_state"].as_str(),
            ) {
                (Some("current"), Some("ready"), _, _) => break,
                (Some("warming"), _, _, _)
                | (Some("stale"), Some("ready"), _, Some("verifying"))
                | (_, Some("pending"), _, _)
                | (_, Some("unavailable"), Some("generation_unavailable"), _) => {
                    tokio::task::yield_now().await;
                }
                (_, Some("refused"), _, _) | (_, _, Some("activation_disabled"), _) => {
                    panic!("graph readiness was refused: {status}");
                }
                actual => panic!("graph readiness became {actual:?}: {status}"),
            }
        }
    })
    .await
    .expect("graph did not become current within the publication budget");
}

async fn find_node_id(host: &impl AnalysisToolHost, name: &str) -> String {
    wait_for_current_graph(host).await;
    let result = handle_tool_call(
        host,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20}),
        None,
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("production exact-symbol read failed: {error}"));
    let payload: Value =
        serde_json::from_str(extract_text(&result.value)).expect("exact-symbol JSON");
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches
                .iter()
                .find(|result| result["name"].as_str() == Some(name))
        })
        .and_then(|result| result["id"].as_str())
        .unwrap_or_else(|| panic!("node '{name}' not found in production generation: {payload}"))
        .to_owned()
}

// `tracedecay_diff_context` as an agent host observes it: one production
// MCP `tools/call`, then the JSON text the caller reads.
//
// The fixture is one crate. `tier_b` calls `tier_c`, and `tier_a` calls
// `tier_b`. `#[test]` on `checks_tier_c` is itself a modified symbol
// (`annotation_usage` named `test`). Lines are the extractor's 0-based
// tree-sitter rows.

const LIB_RS: &str = "mod tier_a;\nmod tier_b;\nmod tier_c;\n";
const TIER_A_RS: &str = "use crate::tier_b::tier_b;\n\npub fn tier_a() -> u8 {\n    tier_b()\n}\n";
const TIER_B_RS: &str = "use crate::tier_c::tier_c;\n\npub fn tier_b() -> u8 {\n    tier_c()\n}\n";
const TIER_C_RS: &str = "\
pub fn tier_c() -> u8 {\n\
    1\n\
}\n\
\n\
#[test]\n\
fn checks_tier_c() {\n\
    let _ = tier_c();\n\
}\n";

fn diff_symbol_facts(symbols: &Value) -> Vec<Value> {
    let Some(symbols) = symbols.as_array() else {
        panic!("diff_context symbol list is not an array: {symbols}");
    };
    let mut facts = symbols
        .iter()
        .map(|symbol| {
            json!({
                "name": symbol["name"],
                "kind": symbol["kind"],
                "file": symbol["file"],
                "line": symbol["line"],
            })
        })
        .collect::<Vec<_>>();
    facts.sort_by(|left, right| {
        (
            left["file"].as_str(),
            left["name"].as_str(),
            left["line"].as_u64(),
        )
            .cmp(&(
                right["file"].as_str(),
                right["name"].as_str(),
                right["line"].as_u64(),
            ))
    });
    facts
}

fn write_call_chain(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    fs::write(project.join("src/tier_a.rs"), TIER_A_RS).unwrap();
    fs::write(project.join("src/tier_b.rs"), TIER_B_RS).unwrap();
    fs::write(project.join("src/tier_c.rs"), TIER_C_RS).unwrap();
}

#[tokio::test]
async fn diff_context_reports_changed_symbols_callers_and_refuses_invalid_input() {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    write_call_chain(&project);
    let host = init_test_project(&project).await;

    let changed = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": ["src/tier_c.rs"], "depth": 1, "format": "json"}),
        None,
        None,
    )
    .await
    .expect("depth-1 diff_context");
    let changed = extract_json(&changed.value);
    assert_eq!(changed["changed_files"], json!(["src/tier_c.rs"]));
    // `tier_a` calls `tier_b`, so a depth-1 walk stops with callers still
    // unexplored. The tool must say so instead of pretending the radius is
    // complete.
    assert_eq!(changed["impact_complete"], json!(false), "{changed}");
    assert_eq!(
        diff_symbol_facts(&changed["modified_symbols"]),
        vec![
            json!({"name": "checks_tier_c", "kind": "function", "file": "src/tier_c.rs", "line": 5}),
            json!({"name": "test", "kind": "annotation_usage", "file": "src/tier_c.rs", "line": 4}),
            json!({"name": "tier_c", "kind": "function", "file": "src/tier_c.rs", "line": 0}),
        ],
        "modified symbols: {changed}"
    );
    assert_eq!(changed["impacted_symbols_count"], json!(1));
    assert_eq!(
        diff_symbol_facts(&changed["impacted_symbols"]),
        vec![json!({"name": "tier_b", "kind": "function", "file": "src/tier_b.rs", "line": 2}),],
        "direct callers of tier_c: {changed}"
    );
    assert_eq!(
        changed["affected_tests"],
        json!(["src/tier_c.rs"]),
        "{changed}"
    );

    let wider = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": ["src/tier_c.rs"], "depth": 2, "format": "json"}),
        None,
        None,
    )
    .await
    .expect("depth-2 diff_context");
    let wider = extract_json(&wider.value);
    assert_eq!(wider["impact_complete"], json!(true), "{wider}");
    assert_eq!(wider["impacted_symbols_count"], json!(2));
    assert_eq!(
        diff_symbol_facts(&wider["impacted_symbols"]),
        vec![
            json!({"name": "tier_a", "kind": "function", "file": "src/tier_a.rs", "line": 2}),
            json!({"name": "tier_b", "kind": "function", "file": "src/tier_b.rs", "line": 2}),
        ],
        "depth 2 also reaches tier_a, which only calls tier_b: {wider}"
    );

    let duplicated = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({
            "files": ["src/tier_c.rs", "src/tier_c.rs"],
            "depth": 1,
            "format": "json"
        }),
        None,
        None,
    )
    .await
    .expect("duplicate-path diff_context");
    let duplicated = extract_json(&duplicated.value);
    assert_eq!(duplicated["changed_files"], json!(["src/tier_c.rs"]));
    assert_eq!(
        diff_symbol_facts(&duplicated["modified_symbols"]),
        vec![
            json!({"name": "checks_tier_c", "kind": "function", "file": "src/tier_c.rs", "line": 5}),
            json!({"name": "test", "kind": "annotation_usage", "file": "src/tier_c.rs", "line": 4}),
            json!({"name": "tier_c", "kind": "function", "file": "src/tier_c.rs", "line": 0}),
        ]
    );
    assert_eq!(
        diff_symbol_facts(&duplicated["impacted_symbols"]),
        vec![json!({"name": "tier_b", "kind": "function", "file": "src/tier_b.rs", "line": 2}),]
    );

    // A path this generation never published carries no symbols, so the
    // affected-test walk has no seeds and answers empty and complete rather
    // than turning "no such file here" into an invalid request.
    let absent = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": ["src/not_in_repo.rs"], "format": "json"}),
        None,
        None,
    )
    .await
    .expect("unpublished-path diff_context");
    assert_eq!(
        extract_json(&absent.value),
        json!({
            "changed_files": ["src/not_in_repo.rs"],
            "modified_symbols": [],
            "impacted_symbols_count": 0,
            "impacted_symbols": [],
            "impact_complete": true,
            "affected_tests": []
        })
    );

    let empty_files = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": [], "format": "json"}),
        None,
        None,
    )
    .await
    .expect("empty-files diff_context");
    assert_eq!(
        extract_json(&empty_files.value),
        json!({
            "changed_files": [],
            "modified_symbols": [],
            "impacted_symbols_count": 0,
            "impacted_symbols": [],
            "impact_complete": true,
            "affected_tests": []
        })
    );

    let missing_files = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"format": "json"}),
        None,
        None,
    )
    .await;
    assert_eq!(
        missing_files
            .expect_err("missing files must be refused")
            .to_string(),
        "config error: tracedecay_diff_context failed over production MCP: missing required parameter: files (array of strings)"
    );

    let files_not_array = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": "src/tier_c.rs", "format": "json"}),
        None,
        None,
    )
    .await;
    assert_eq!(
        files_not_array
            .expect_err("a string files argument must be refused")
            .to_string(),
        "config error: tracedecay_diff_context failed over production MCP: missing required parameter: files (array of strings)"
    );

    let not_object = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!(["src/tier_c.rs"]),
        None,
        None,
    )
    .await;
    assert_eq!(
        not_object
            .expect_err("non-object arguments must be refused")
            .to_string(),
        "config error: tracedecay_diff_context failed over production MCP: tool execution failed: config error: invalid arguments: tracedecay_diff_context expects a JSON object"
    );

    let zero_depth = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": ["src/tier_c.rs"], "depth": 0, "format": "json"}),
        None,
        None,
    )
    .await;
    assert_eq!(
        zero_depth.expect_err("depth 0 must be refused").to_string(),
        "config error: tracedecay_diff_context failed over production MCP: tool project route failed: reason_code=code-graph-invalid-request retryable=false: the code-graph read request is invalid: code graph impact depth must be positive"
    );

    close_test_graph(host).await;
}

// Literal `tracedecay_gini` results for a fixture whose metric values are
// fixed by source shape, not by reading the coefficient back out of the tool.
//
// The handler rounds `2*Σ(i*x_i)/(n*Σx) - (n+1)/n` (1-indexed `i` on values
// sorted ascending) to four decimals. One file, or a missing path, is the
// empty-or-singleton case and the coefficient is exactly 0.

fn write_gini_distribution_sources(project: &Path) {
    std::fs::create_dir_all(project.join("src/spans")).unwrap();
    // Body is one block and no branch: complexity 1, line span 1.
    std::fs::write(
        project.join("src/spans/short.rs"),
        "pub fn short() -> i32 { 1 }\n",
    )
    .unwrap();
    // Same complexity 1, line span 3 (declaration, body, closing brace).
    std::fs::write(
        project.join("src/spans/tall.rs"),
        "pub fn tall() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    // Tiny has one field, Big has three. `plain` is a single block (complexity
    // 1). `branched` is if + else (2 branches) inside a body block, so the
    // inner blocks reach nesting 2 and the symbol score is 4.
    std::fs::write(
        project.join("src/kinds.rs"),
        "\
pub struct Tiny {\n    \
    pub only: i32,\n\
}\n\
\n\
pub struct Big {\n    \
    pub a: i32,\n    \
    pub b: i32,\n    \
    pub c: i32,\n\
}\n\
\n\
pub fn plain() -> i32 { 1 }\n\
\n\
pub fn branched(n: i32) -> i32 {\n    \
    if n > 0 {\n        \
        n\n    \
    } else {\n        \
        0\n    \
    }\n\
}\n",
    )
    .unwrap();
}

async fn gini_json(host: &impl AnalysisToolHost, args: Value) -> Value {
    let result = handle_tool_call(host, "tracedecay_gini", args, None, None)
        .await
        .expect("tracedecay_gini over production MCP");
    extract_json(&result.value)
}

/// Equal values do not define an outlier rank. Sort by name so the assertion
/// stays on the reported rows rather than `HashMap` iteration order.
fn outliers_sorted_by_name(mut payload: Value) -> Value {
    if let Some(outliers) = payload.get_mut("outliers").and_then(Value::as_array_mut) {
        outliers.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    }
    payload
}

#[tokio::test]
async fn gini_reports_literal_coefficients_for_known_distributions() {
    let host = production_composition_fixture_with_sources(write_gini_distribution_sources).await;

    let lines = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "lines",
            "scope": "file",
            "path": "src/spans",
        }),
    )
    .await;
    assert_eq!(
        lines,
        json!({
            "gini": 0.25,
            "interpretation": "moderate inequality",
            "total_items": 2,
            "metric": "lines",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/spans/tall.rs", "value": 3.0, "pct_of_max": 100.0},
                {"name": "src/spans/short.rs", "value": 1.0, "pct_of_max": 33.0},
            ],
        }),
        "line spans 1 and 3 must produce Gini 0.25: {lines}"
    );

    let truncated = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "lines",
            "scope": "file",
            "path": "src/spans",
            "limit": 1,
        }),
    )
    .await;
    assert_eq!(
        truncated,
        json!({
            "gini": 0.25,
            "interpretation": "moderate inequality",
            "total_items": 2,
            "metric": "lines",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/spans/tall.rs", "value": 3.0, "pct_of_max": 100.0},
            ],
        }),
        "limit truncates the ranking and keeps the census: {truncated}"
    );

    let one_file = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "lines",
            "path": "src/spans/tall.rs",
        }),
    )
    .await;
    assert_eq!(
        one_file,
        json!({
            "gini": 0.0,
            "interpretation": "low inequality (healthy)",
            "total_items": 1,
            "metric": "lines",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/spans/tall.rs", "value": 3.0, "pct_of_max": 100.0},
            ],
        }),
        "a single file is perfect equality: {one_file}"
    );

    let missing = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "lines",
            "path": "src/nowhere",
        }),
    )
    .await;
    assert_eq!(
        missing,
        json!({
            "gini": 0.0,
            "interpretation": "low inequality (healthy)",
            "total_items": 0,
            "metric": "lines",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [],
        }),
        "a path with no symbols is an empty census, not the unfiltered one: {missing}"
    );

    // Defaults are complexity + file. Both span functions score 1, so this
    // coefficient is 0. The lines call above is 0.25 for the same path.
    let defaults = gini_json(
        &host,
        json!({
            "format": "json",
            "path": "src/spans",
        }),
    )
    .await;
    assert_eq!(
        outliers_sorted_by_name(defaults.clone()),
        json!({
            "gini": 0.0,
            "interpretation": "low inequality (healthy)",
            "total_items": 2,
            "metric": "complexity",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/spans/short.rs", "value": 1.0, "pct_of_max": 100.0},
                {"name": "src/spans/tall.rs", "value": 1.0, "pct_of_max": 100.0},
            ],
        }),
        "default metric is complexity, not lines: {defaults}"
    );

    let members = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "members",
            "path": "src/kinds.rs",
        }),
    )
    .await;
    assert_eq!(
        members,
        json!({
            "gini": 0.25,
            "interpretation": "moderate inequality",
            "total_items": 2,
            "metric": "members",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "Big", "value": 3.0, "pct_of_max": 100.0},
                {"name": "Tiny", "value": 1.0, "pct_of_max": 33.0},
            ],
        }),
        "struct member counts 1 and 3 must produce Gini 0.25: {members}"
    );

    let symbols = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "complexity",
            "scope": "symbol",
            "path": "src/kinds.rs",
        }),
    )
    .await;
    assert_eq!(
        symbols,
        json!({
            "gini": 0.3,
            "interpretation": "moderate inequality",
            "total_items": 2,
            "metric": "complexity",
            "scope": "symbol",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/kinds.rs:branched", "value": 4.0, "pct_of_max": 100.0},
                {"name": "src/kinds.rs:plain", "value": 1.0, "pct_of_max": 25.0},
            ],
        }),
        "symbol scores 1 and 4 must produce Gini 0.3: {symbols}"
    );

    close_test_graph(host).await;
}

#[tokio::test]
async fn gini_empty_index_reports_perfect_equality() {
    let host = setup_empty_analysis_project().await;
    let payload = gini_json(&host, json!({"format": "json"})).await;
    assert_eq!(
        payload,
        json!({
            "gini": 0.0,
            "interpretation": "low inequality (healthy)",
            "total_items": 0,
            "metric": "complexity",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [],
        }),
        "an empty index is not a missing field: {payload}"
    );
    close_test_graph(host).await;
}

// `tracedecay_hotspots` through production MCP `tools/call`.
//
// Occurrence ids are minted per project, so two equal totals may swap order
// across runs. The host-visible ranking of distinct degrees, the line and
// degree of each named symbol, the default page, the clamped page, and the
// zero-limit rejection are stable and asserted literally.

const CHAIN_SOURCE: &str = "\
export function quiet(): number {\n\
  return 0;\n\
}\n\
\n\
export function leaf(): number {\n\
  return 1;\n\
}\n\
\n\
export function mid(): number {\n\
  return leaf();\n\
}\n\
\n\
export function hub(): number {\n\
  return mid();\n\
}\n\
";

fn write_package(project: &Path, name: &str) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("package.json"),
        format!("{{\"name\":\"{name}\",\"private\":true,\"type\":\"module\"}}\n"),
    )
    .unwrap();
}

fn write_chain_project(project: &Path) {
    write_package(project, "hotspots-chain");
    fs::write(project.join("src/calls.ts"), CHAIN_SOURCE).unwrap();
}

/// `hub` plus 101 callers. Returns the source length the savings footer
/// measures for `src/fanout.ts`.
fn write_fanout_project(project: &Path) -> usize {
    write_package(project, "hotspots-fanout");
    let mut source = String::from("export function hub(): number { return 1; }\n");
    for index in 0..101 {
        source.push_str(&format!(
            "export function caller{index}(): number {{ return hub(); }}\n"
        ));
    }
    let bytes = source.len();
    fs::write(project.join("src/fanout.ts"), source).unwrap();
    bytes
}

async fn call_hotspots(host: &MountedProductionProject, arguments: Value) -> Value {
    let result = handle_tool_call(host, "tracedecay_hotspots", arguments, None, None)
        .await
        .unwrap_or_else(|error| panic!("tracedecay_hotspots failed over production MCP: {error}"));
    result.value
}

fn content(result: &Value) -> &[Value] {
    result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("hotspots content missing: {result}"))
}

fn body_text(result: &Value) -> &str {
    let item = &content(result)[0];
    assert_eq!(item["type"], "text", "{result}");
    item["text"]
        .as_str()
        .unwrap_or_else(|| panic!("hotspots text missing: {result}"))
}

fn parse_body(result: &Value) -> Value {
    let text = body_text(result);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("hotspots JSON did not parse: {error}\n{text}"))
}

fn assert_savings_footer(result: &Value, source_bytes: usize) {
    let items = content(result);
    assert_eq!(items.len(), 2, "{result}");
    assert_eq!(items[1]["type"], "text", "{result}");
    let footer = items[1]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("hotspots footer missing: {result}"));
    assert_eq!(
        footer,
        format!(
            "\ntracedecay_metrics: before={} after={}",
            source_bytes / 4,
            body_text(result).len() / 4
        )
    );
}

fn assert_symbol_id(id: &str) {
    let prefix = "symbol.v1.sha256:";
    let Some(hex) = id.strip_prefix(prefix) else {
        panic!("hotspot id {id} is not a sealed symbol occurrence");
    };
    assert_eq!(hex.len(), 64, "{id}");
    assert!(
        hex.chars().all(|character| character.is_ascii_hexdigit()),
        "{id}"
    );
}

fn assert_exact_hotspot(
    row: &Value,
    name: &str,
    file: &str,
    line: u64,
    incoming: u64,
    outgoing: u64,
    total: u64,
) {
    let id = row["id"]
        .as_str()
        .unwrap_or_else(|| panic!("hotspot id missing: {row}"));
    assert_symbol_id(id);
    assert_eq!(
        row,
        &json!({
            "id": id,
            "name": name,
            "kind": "function",
            "file": file,
            "line": line,
            "incoming": incoming,
            "outgoing": outgoing,
            "total": total,
        }),
        "{row}"
    );
}

fn hotspots(payload: &Value) -> &[Value] {
    let rows = payload["hotspots"]
        .as_array()
        .unwrap_or_else(|| panic!("hotspots array missing: {payload}"));
    assert_eq!(
        payload["hotspot_count"].as_u64(),
        Some(u64::try_from(rows.len()).expect("hotspot count fits")),
        "{payload}"
    );
    rows
}

fn assert_chain_ranking(payload: &Value) {
    let rows = hotspots(payload);
    assert_eq!(rows.len(), 4, "{payload}");
    assert_exact_hotspot(&rows[0], "mid", "src/calls.ts", 9, 1, 1, 2);
    assert_exact_hotspot(&rows[3], "quiet", "src/calls.ts", 1, 0, 0, 0);
    let mut tied = [rows[1].clone(), rows[2].clone()];
    tied.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or("")
            .cmp(right["name"].as_str().unwrap_or(""))
    });
    assert_exact_hotspot(&tied[0], "hub", "src/calls.ts", 13, 0, 1, 1);
    assert_exact_hotspot(&tied[1], "leaf", "src/calls.ts", 5, 1, 0, 1);
    assert!(
        rows.windows(2)
            .all(|pair| pair[0]["total"].as_u64() >= pair[1]["total"].as_u64()),
        "chain ranking is not highest degree first: {payload}"
    );
}

fn assert_fanout_page(payload: &Value, expected_count: usize) {
    let rows = hotspots(payload);
    assert_eq!(rows.len(), expected_count, "{payload}");
    assert_exact_hotspot(&rows[0], "hub", "src/fanout.ts", 1, 101, 0, 101);
    let mut seen = Vec::new();
    for row in rows.iter().skip(1) {
        let name = row["name"]
            .as_str()
            .unwrap_or_else(|| panic!("caller name missing: {row}"));
        let index: u64 = name
            .strip_prefix("caller")
            .unwrap_or_else(|| panic!("non-caller in the fan-out page: {row}"))
            .parse()
            .unwrap_or_else(|_| panic!("caller index missing: {row}"));
        assert!(index < 101, "caller outside the fixture: {row}");
        assert_exact_hotspot(row, name, "src/fanout.ts", index + 2, 0, 1, 1);
        seen.push(index);
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), expected_count - 1, "{payload}");
}

fn assert_clamped_truncation(payload: &Value) {
    assert_eq!(payload["truncated"], true, "{payload}");
    assert_eq!(payload["retrieve_tool"], "tracedecay_retrieve", "{payload}");
    assert_eq!(payload["retrieve_ttl_seconds"], 86_400, "{payload}");
    let preview_chars = payload["preview_chars"]
        .as_u64()
        .unwrap_or_else(|| panic!("preview_chars missing: {payload}"));
    assert_eq!(preview_chars, 11_928, "{payload}");
    let original_chars = payload["original_chars"]
        .as_u64()
        .unwrap_or_else(|| panic!("original_chars missing: {payload}"));
    assert!(
        original_chars > preview_chars,
        "clamped body must not fit in the preview: {payload}"
    );
    let preview = payload["preview"]
        .as_str()
        .unwrap_or_else(|| panic!("preview missing: {payload}"));
    assert_eq!(preview.chars().count() as u64, preview_chars, "{preview}");
    let marker = r#"{"hotspot_count":100,"hotspots":["#;
    let array = preview
        .strip_prefix(marker)
        .unwrap_or_else(|| panic!("clamped preview did not start with 100 rows: {preview}"));
    assert!(
        array.starts_with('{'),
        "clamped preview omitted the hub object: {preview}"
    );
    let mut depth = 0_i32;
    let mut end = None;
    for (index, byte) in array.bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.unwrap_or_else(|| panic!("clamped hub object was cut off: {preview}"));
    let first: Value = serde_json::from_str(&array[..=end]).unwrap_or_else(|error| {
        panic!(
            "clamped hub object did not parse: {error}\n{}",
            &array[..=end]
        )
    });
    assert_exact_hotspot(&first, "hub", "src/fanout.ts", 1, 101, 0, 101);

    let handle = payload["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("truncation handle missing: {payload}"));
    assert!(
        handle.starts_with("rh_") && handle.len() > "rh_".len(),
        "{payload}"
    );
    let expires = payload["retrieve_expires_at"]
        .as_i64()
        .unwrap_or_else(|| panic!("retrieve expiry missing: {payload}"));
    let instruction = payload["retrieve_instruction"]
        .as_str()
        .unwrap_or_else(|| panic!("retrieve instruction missing: {payload}"));
    assert!(instruction.contains(handle), "{instruction}");
    assert!(instruction.contains(&expires.to_string()), "{instruction}");
    assert!(
        instruction.contains(&preview_chars.to_string()),
        "{instruction}"
    );
    assert!(
        instruction.contains(&original_chars.to_string()),
        "{instruction}"
    );
    assert!(instruction.contains("tracedecay_retrieve"), "{instruction}");
}

#[tokio::test]
async fn hotspots_ranks_symbols_by_edge_degree_and_clamps_limit() {
    let chain_dir = test_temp_dir();
    let chain_root = chain_dir.path().join("project");
    write_chain_project(&chain_root);
    let chain = init_test_project(&chain_root).await;

    let chain_default = call_hotspots(&chain, json!({"format": "json"})).await;
    let chain_limit_one = call_hotspots(&chain, json!({"format": "json", "limit": 1})).await;
    let chain_markdown = call_hotspots(&chain, json!({"format": "markdown", "limit": 1})).await;
    let chain_rejected = chain
        .harness
        .call_tool(
            &chain.project_root,
            "tracedecay_hotspots",
            json!({"limit": 0, "format": "json"}),
        )
        .await
        .expect("zero limit still reaches the MCP server");
    close_test_graph(chain).await;

    let chain_default_payload = parse_body(&chain_default);
    assert_chain_ranking(&chain_default_payload);
    assert_savings_footer(&chain_default, CHAIN_SOURCE.len());

    let chain_one_payload = parse_body(&chain_limit_one);
    let one = hotspots(&chain_one_payload);
    assert_eq!(one.len(), 1, "{chain_one_payload}");
    assert_exact_hotspot(&one[0], "mid", "src/calls.ts", 9, 1, 1, 2);
    assert_savings_footer(&chain_limit_one, CHAIN_SOURCE.len());

    let mid_id = one[0]["id"].as_str().expect("mid occurrence id").to_owned();
    assert_eq!(
        body_text(&chain_markdown),
        format!(
            "**hotspot_count:** 1\n\n## hotspots\n- **mid**\n  **kind:** function\n  **file:** src/calls.ts\n  **line:** 9\n  **id:** `{mid_id}`\n  **incoming:** 1\n  **outgoing:** 1\n  **total:** 2\n"
        )
    );
    assert_savings_footer(&chain_markdown, CHAIN_SOURCE.len());

    let rejected = chain_rejected.error.expect("zero limit is a tool error");
    assert_eq!(rejected.code, -32603);
    assert_eq!(
        rejected.message,
        "tool execution failed: config error: invalid parameter: tracedecay_hotspots requires limit to be at least 1"
    );
    assert_eq!(
        rejected.data,
        Some(json!({
            "tool": "tracedecay_hotspots",
            "cli_fallback": "This tool is also available from the shell: `tracedecay tool hotspots ...` (`tracedecay tool hotspots --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
        }))
    );

    let fanout_dir = test_temp_dir();
    let fanout_root = fanout_dir.path().join("project");
    let fanout_bytes = write_fanout_project(&fanout_root);
    let fanout = init_test_project(&fanout_root).await;
    let fanout_default = call_hotspots(&fanout, json!({"format": "json"})).await;
    let fanout_capped = call_hotspots(&fanout, json!({"format": "json", "limit": 250})).await;
    let fanout_one = call_hotspots(&fanout, json!({"format": "json", "limit": 1})).await;
    close_test_graph(fanout).await;

    let fanout_default_payload = parse_body(&fanout_default);
    assert_fanout_page(&fanout_default_payload, 10);
    assert_savings_footer(&fanout_default, fanout_bytes);

    let fanout_one_payload = parse_body(&fanout_one);
    let fanout_top = hotspots(&fanout_one_payload);
    assert_eq!(fanout_top.len(), 1, "{fanout_one_payload}");
    assert_exact_hotspot(&fanout_top[0], "hub", "src/fanout.ts", 1, 101, 0, 101);
    assert_savings_footer(&fanout_one, fanout_bytes);

    assert_clamped_truncation(&parse_body(&fanout_capped));
    assert_savings_footer(&fanout_capped, fanout_bytes);
}

// Literal `tracedecay_recursion` results from production MCP `tools/call`.
//
// `length` is the number of call edges in the cycle. The chain repeats its
// start symbol so the path is closed. Occurrence ids include the temp
// project path, so which node the search starts on changes between runs.
// Comparisons rotate each chain to its smallest `(name, file, line)` and
// then pin names, kinds, files, and lines. Ids are still required to close
// the cycle.

const DIRECT_SOURCE: &str = "\
pub fn recurse(n: u32) -> u32 {
    if n == 0 { 0 } else { recurse(n - 1) }
}

pub fn leaf() -> u32 { 1 }
";

const MUTUAL_SOURCE: &str = "\
pub fn ping() { pong(); }
pub fn pong() { ping(); }
";

const NOISE_SOURCE: &str = "\
pub struct Triplet {
    rows: Vec<usize>,
}

impl Triplet {
    pub fn push(&mut self, row: usize) {
        self.rows.push(row);
    }
}
";

fn direct_cycle() -> Value {
    json!({
        "length": 1,
        "chain": [
            {"name": "recurse", "kind": "function", "file": "src/direct.rs", "line": 1},
            {"name": "recurse", "kind": "function", "file": "src/direct.rs", "line": 1}
        ]
    })
}

fn mutual_cycle() -> Value {
    json!({
        "length": 2,
        "chain": [
            {"name": "ping", "kind": "function", "file": "src/mutual.rs", "line": 1},
            {"name": "pong", "kind": "function", "file": "src/mutual.rs", "line": 2},
            {"name": "ping", "kind": "function", "file": "src/mutual.rs", "line": 1}
        ]
    })
}

fn full_report() -> Value {
    json!({
        "cycle_count": 2,
        "cycles": [direct_cycle(), mutual_cycle()]
    })
}

fn public_recursion_report(payload: &Value) -> Value {
    let keys = sorted_keys(payload, "recursion payload");
    assert_eq!(
        keys,
        ["cycle_count", "cycles"],
        "recursion payload keys drifted: {payload}"
    );
    let cycles = payload["cycles"]
        .as_array()
        .unwrap_or_else(|| panic!("cycles must be an array: {payload}"));
    let cycles = cycles
        .iter()
        .map(|cycle| {
            let cycle_keys = sorted_keys(cycle, "cycle");
            assert_eq!(
                cycle_keys,
                ["chain", "length"],
                "cycle keys drifted: {cycle}"
            );
            let chain = cycle["chain"]
                .as_array()
                .unwrap_or_else(|| panic!("chain must be an array: {cycle}"));
            json!({
                "length": cycle["length"],
                "chain": canonical_public_chain(chain),
            })
        })
        .collect::<Vec<_>>();
    let mut cycles = cycles;
    cycles.sort_by_key(cycle_order_key);
    json!({
        "cycle_count": payload["cycle_count"],
        "cycles": cycles,
    })
}

fn cycle_order_key(cycle: &Value) -> (i64, String) {
    (
        cycle["length"].as_i64().unwrap_or(i64::MAX),
        cycle["chain"].to_string(),
    )
}

fn canonical_public_chain(chain: &[Value]) -> Vec<Value> {
    let public = chain.iter().map(public_chain_node).collect::<Vec<_>>();
    assert!(
        public.len() >= 2,
        "a cycle chain must repeat its start: {public:?}"
    );
    assert_eq!(
        public.first(),
        public.last(),
        "a cycle chain must close on the same symbol: {public:?}"
    );
    let body = &public[..public.len() - 1];
    let start = body
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| public_node_order(left).cmp(&public_node_order(right)))
        .map(|(index, _)| index)
        .expect("a cycle body is non-empty");
    let mut rotated = body[start..]
        .iter()
        .chain(&body[..start])
        .cloned()
        .collect::<Vec<_>>();
    rotated.push(rotated[0].clone());
    rotated
}

fn public_node_order(node: &Value) -> (String, String, i64) {
    (
        node["name"].as_str().unwrap_or_default().to_owned(),
        node["file"].as_str().unwrap_or_default().to_owned(),
        node["line"].as_i64().unwrap_or(i64::MAX),
    )
}

fn sorted_keys<'a>(value: &'a Value, label: &str) -> Vec<&'a str> {
    let mut keys = value
        .as_object()
        .unwrap_or_else(|| panic!("{label} must be an object: {value}"))
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}

fn public_chain_node(node: &Value) -> Value {
    let keys = sorted_keys(node, "chain node");
    assert_eq!(
        keys,
        ["file", "id", "kind", "line", "name"],
        "chain node keys drifted: {node}"
    );
    assert!(
        node["id"].as_str().is_some_and(|id| !id.is_empty()),
        "chain node id must be a non-empty string: {node}"
    );
    json!({
        "name": node["name"],
        "kind": node["kind"],
        "file": node["file"],
        "line": node["line"],
    })
}

fn assert_reported_cycles_close(payload: &Value) {
    let cycles = payload["cycles"]
        .as_array()
        .unwrap_or_else(|| panic!("cycles must be an array: {payload}"));
    for cycle in cycles {
        let chain = cycle["chain"]
            .as_array()
            .unwrap_or_else(|| panic!("chain must be an array: {cycle}"));
        let start = chain
            .first()
            .and_then(|node| node["id"].as_str())
            .unwrap_or_else(|| panic!("cycle is missing its start id: {cycle}"));
        let end = chain
            .last()
            .and_then(|node| node["id"].as_str())
            .unwrap_or_else(|| panic!("cycle is missing its closing id: {cycle}"));
        assert_eq!(
            start, end,
            "a reported cycle must return to its start symbol: {cycle}"
        );
    }
}

async fn call_recursion(graph: &impl AnalysisToolHost, arguments: Value) -> Value {
    let result = handle_tool_call(graph, "tracedecay_recursion", arguments, None, None)
        .await
        .unwrap_or_else(|error| panic!("tracedecay_recursion failed: {error}"));
    extract_json(&result.value)
}

#[tokio::test]
async fn recursion_reports_literal_cycles_and_refuses_non_positive_limit() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs_write_fixture(&project_root);
    let graph = init_test_project(&project_root).await;

    let payload = call_recursion(&graph, json!({"format": "json", "limit": 10})).await;
    assert_eq!(
        public_recursion_report(&payload),
        full_report(),
        "default-sized recursion report: {payload}"
    );
    assert_reported_cycles_close(&payload);

    let scoped = call_recursion(&graph, json!({"format": "json", "path": "src/direct.rs"})).await;
    assert_eq!(
        public_recursion_report(&scoped),
        json!({"cycle_count": 1, "cycles": [direct_cycle()]}),
        "path filter must keep only the direct cycle: {scoped}"
    );

    let mutual = call_recursion(
        &graph,
        json!({"format": "json", "path": "src/mutual.rs", "limit": 10}),
    )
    .await;
    assert_eq!(
        public_recursion_report(&mutual),
        json!({"cycle_count": 1, "cycles": [mutual_cycle()]}),
        "path filter must keep only the mutual cycle: {mutual}"
    );

    let noise = call_recursion(&graph, json!({"format": "json", "path": "src/noise.rs"})).await;
    assert_eq!(
        public_recursion_report(&noise),
        json!({"cycle_count": 0, "cycles": []}),
        "receiver `.push` must not be a cycle when the same graph has real cycles: {noise}"
    );

    let limited = call_recursion(&graph, json!({"format": "json", "limit": 1})).await;
    assert_eq!(
        public_recursion_report(&limited),
        json!({"cycle_count": 1, "cycles": [direct_cycle()]}),
        "limit 1 keeps the shortest cycle: {limited}"
    );

    let error = expect_tool_error(
        handle_tool_call(
            &graph,
            "tracedecay_recursion",
            json!({"format": "json", "limit": 0}),
            None,
            None,
        )
        .await,
    );
    assert_eq!(
        error,
        "config error: tracedecay_recursion failed over production MCP: tool execution failed: config error: invalid parameter: tracedecay_recursion requires limit to be at least 1"
    );
    close_test_graph(graph).await;
}

fn fs_write_fixture(project_root: &Path) {
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub mod direct;\npub mod mutual;\npub mod noise;\n",
    )
    .unwrap();
    std::fs::write(project_root.join("src/direct.rs"), DIRECT_SOURCE).unwrap();
    std::fs::write(project_root.join("src/mutual.rs"), MUTUAL_SOURCE).unwrap();
    std::fs::write(project_root.join("src/noise.rs"), NOISE_SOURCE).unwrap();
}
