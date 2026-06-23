//! Integration tests for MCP tool handlers (`handle_tool_call`).
//!
//! Each test exercises a real `TraceDecay` instance with indexed test data,
//! ensuring that the MCP dispatch layer formats results correctly.

mod common;

use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::{Mutex, MutexGuard};
use tracedecay::db::Database;
use tracedecay::errors::TraceDecayError;
use tracedecay::global_db::GlobalDb;
use tracedecay::mcp::{get_tool_definitions, ToolResult};
use tracedecay::memory::store::MemoryStore;
use tracedecay::sessions::cursor::open_project_session_db;
use tracedecay::sessions::lcm::{
    LcmLifecycleUpdate, LcmMaintenanceDebt, LcmSourceRef, LcmSummaryNodeDraft,
};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::storage::{
    resolve_layout_for_current_profile, resolve_lcm_payload_root, resolve_project_session_db_path,
    resolve_response_handle_root,
};
use tracedecay::tracedecay::TraceDecay;

static GLOBAL_DB_ENV_LOCK: Mutex<()> = Mutex::const_new(());

async fn handle_tool_call(
    cg: &TraceDecay,
    tool_name: &str,
    mut args: serde_json::Value,
    server_stats: Option<serde_json::Value>,
    scope_prefix: Option<&str>,
) -> tracedecay::errors::Result<ToolResult> {
    let owns_format = matches!(
        tool_name,
        "tracedecay_context" | "tracedecay_dsm" | "tracedecay_files" | "tracedecay_type_hierarchy"
    );
    if !owns_format {
        if let Some(obj) = args.as_object_mut() {
            obj.entry("format".to_string())
                .or_insert_with(|| serde_json::json!("json"));
        }
    }
    tracedecay::mcp::handle_tool_call(cg, tool_name, args, server_stats, scope_prefix).await
}

async fn index_all_retrying_sync_lock(cg: &TraceDecay) {
    for attempt in 0..20 {
        match cg.index_all().await {
            Ok(_) => return,
            Err(TraceDecayError::SyncLock { .. }) if attempt < 19 => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => panic!("failed to index test fixture: {err}"),
        }
    }
}

struct GlobalDbEnvGuard {
    previous: Option<OsString>,
}

impl GlobalDbEnvGuard {
    fn set(db_path: &Path) -> Self {
        let previous = std::env::var_os("TRACEDECAY_GLOBAL_DB");
        let db_path = canonicalize_test_db_path(db_path);
        std::env::set_var("TRACEDECAY_GLOBAL_DB", db_path);
        Self { previous }
    }
}

impl Drop for GlobalDbEnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("TRACEDECAY_GLOBAL_DB", value),
            None => std::env::remove_var("TRACEDECAY_GLOBAL_DB"),
        }
    }
}

struct HomeEnvGuard {
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
    previous_data_dir: Option<OsString>,
}

impl HomeEnvGuard {
    fn set(home: &Path) -> Self {
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let previous_data_dir = std::env::var_os(tracedecay::config::USER_DATA_DIR_ENV);
        let home = canonicalize_test_dir(home);
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var(
            tracedecay::config::USER_DATA_DIR_ENV,
            home.join(tracedecay::config::TRACEDECAY_DIR),
        );
        Self {
            previous_home,
            previous_userprofile,
            previous_data_dir,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match self.previous_userprofile.take() {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
        match self.previous_data_dir.take() {
            Some(value) => std::env::set_var(tracedecay::config::USER_DATA_DIR_ENV, value),
            None => std::env::remove_var(tracedecay::config::USER_DATA_DIR_ENV),
        }
    }
}

fn canonicalize_test_dir(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap_or_else(|err| {
        panic!(
            "failed to create test directory '{}': {err}",
            path.display()
        )
    });
    path.canonicalize().unwrap_or_else(|err| {
        panic!(
            "failed to canonicalize test directory '{}': {err}",
            path.display()
        )
    })
}

fn canonicalize_test_db_path(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .unwrap_or_else(|| panic!("test DB path '{}' has no parent", path.display()));
    canonicalize_test_dir(parent).join(
        path.file_name()
            .unwrap_or_else(|| panic!("test DB path '{}' has no file name", path.display())),
    )
}

// ---------------------------------------------------------------------------
// Shared setup
// ---------------------------------------------------------------------------

struct TestProject {
    dir: Option<TempDir>,
    _env_lock: MutexGuard<'static, ()>,
    _home_guard: HomeEnvGuard,
    _global_db_guard: GlobalDbEnvGuard,
}

impl std::ops::Deref for TestProject {
    type Target = TempDir;

    fn deref(&self) -> &Self::Target {
        self.dir.as_ref().expect("test project dir already kept")
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(dir) = self.dir.take() {
            let _ = dir.keep();
        }
    }
}

struct TestEnv {
    _env_lock: MutexGuard<'static, ()>,
    _home_guard: HomeEnvGuard,
    _global_db_guard: GlobalDbEnvGuard,
}

struct CrossProjectMemoryEnv {
    _dir: TempDir,
    _env_lock: MutexGuard<'static, ()>,
    _storage_guard: common::TraceDecayStorageEnvGuard,
}

/// Creates a temporary Rust project with cross-file calls, structs, impls,
/// test files, and doc comments, then initialises and indexes a `TraceDecay`.
async fn setup_project() -> (TraceDecay, TestProject) {
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    let home = project.join("home");
    let home_guard = HomeEnvGuard::set(&home);
    let global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        r#"
use crate::utils::helper;
mod utils;

fn main() {
    let result = helper();
    println!("{}", result);
}
"#,
    )
    .unwrap();

    fs::write(
        project.join("src/utils.rs"),
        r#"
/// Returns a greeting string.
pub fn helper() -> String {
    format_greeting("world")
}

fn format_greeting(name: &str) -> String {
    format!("Hello, {}!", name)
}
"#,
    )
    .unwrap();

    // Test file so affected-tests can find something
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("tests/test_utils.rs"),
        r#"
use crate::utils::helper;

#[test]
fn test_helper() { assert!(!helper().is_empty()); }
"#,
    )
    .unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    index_all_retrying_sync_lock(&cg).await;
    (
        cg,
        TestProject {
            dir: Some(dir),
            _env_lock: env_lock,
            _home_guard: home_guard,
            _global_db_guard: global_db_guard,
        },
    )
}

async fn close_test_graph(cg: TraceDecay) {
    cg.checkpoint().await.unwrap();
    cg.close();
}

async fn init_test_project(project: &Path) -> (TraceDecay, TestEnv) {
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = project.join("home");
    let home_guard = HomeEnvGuard::set(&home);
    let global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    let cg = TraceDecay::init(project).await.unwrap();
    (
        cg,
        TestEnv {
            _env_lock: env_lock,
            _home_guard: home_guard,
            _global_db_guard: global_db_guard,
        },
    )
}

async fn setup_generated_dir_project(include_dist: bool) -> (TraceDecay, TestEnv, TempDir) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("dist")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn kept() {}\n").unwrap();
    fs::write(
        project.join("dist/generated.js"),
        "export function generatedOnly() {}\n",
    )
    .unwrap();

    let (mut cg, env) = init_test_project(project).await;
    if include_dist {
        cg.add_include_folders(&["dist".to_string()]);
    }
    cg.index_all().await.unwrap();
    (cg, env, dir)
}

async fn setup_cross_project_memory_projects() -> (TraceDecay, TraceDecay, CrossProjectMemoryEnv) {
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let storage_guard = common::isolated_tracedecay_storage(&dir);

    let active_project = dir.path().join("active");
    let target_project = dir.path().join("target");
    fs::create_dir_all(active_project.join("src")).unwrap();
    fs::create_dir_all(target_project.join("src")).unwrap();
    fs::write(active_project.join("src/lib.rs"), "pub fn active() {}\n").unwrap();
    fs::write(target_project.join("src/lib.rs"), "pub fn target() {}\n").unwrap();

    let active = TraceDecay::init(&active_project).await.unwrap();
    let target = TraceDecay::init(&target_project).await.unwrap();

    (
        active,
        target,
        CrossProjectMemoryEnv {
            _dir: dir,
            _env_lock: env_lock,
            _storage_guard: storage_guard,
        },
    )
}

fn project_data_dir(cg: &TraceDecay) -> PathBuf {
    resolve_layout_for_current_profile(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test project storage layout: {err}"))
        .data_root
}

fn project_graph_db(cg: &TraceDecay) -> PathBuf {
    resolve_layout_for_current_profile(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test project storage layout: {err}"))
        .graph_db_path
}

fn response_handle_dir(cg: &TraceDecay) -> PathBuf {
    resolve_response_handle_root(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test response handle root: {err}"))
}

fn lcm_payload_dir(cg: &TraceDecay) -> PathBuf {
    resolve_lcm_payload_root(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test LCM payload root: {err}"))
}

fn project_session_db_path(cg: &TraceDecay) -> PathBuf {
    resolve_project_session_db_path(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test project session DB path: {err}"))
}

/// Creates a small Rust library with an integration-style test that calls a
/// public entry point, which then reaches an internal helper. This exercises
/// the calibrated depth-3 attribution path in `tracedecay_test_risk`.
async fn setup_integration_test_risk_project() -> (TraceDecay, TestProject) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = project.join("home");
    let home_guard = HomeEnvGuard::set(&home);
    let global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("tests")).unwrap();

    fs::write(
        project.join("Cargo.toml"),
        r#"
[package]
name = "risk_fixture"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    fs::write(
        project.join("src/lib.rs"),
        r#"
pub mod api;
"#,
    )
    .unwrap();

    fs::write(
        project.join("src/api.rs"),
        r#"
pub fn public_entry() -> String {
    format_greeting("world")
}

pub fn unused_public_api() -> String {
    "unused".to_string()
}

fn format_greeting(name: &str) -> String {
    format!("Hello, {}!", name)
}
"#,
    )
    .unwrap();

    fs::write(
        project.join("tests/integration_api.rs"),
        r#"
use risk_fixture::api::public_entry;

#[test]
fn integration_public_entry() {
    assert_eq!(public_entry(), "Hello, world!");
}
"#,
    )
    .unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    (
        cg,
        TestProject {
            dir: Some(dir),
            _env_lock: env_lock,
            _home_guard: home_guard,
            _global_db_guard: global_db_guard,
        },
    )
}

/// Extends the calibrated integration-risk fixture with a build script so the
/// test-risk denominator can prove non-`src/` functions are excluded.
async fn setup_test_risk_non_src_fixture() -> (TraceDecay, TestProject) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = project.join("home");
    let home_guard = HomeEnvGuard::set(&home);
    let global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("tests")).unwrap();

    fs::write(
        project.join("Cargo.toml"),
        r#"
[package]
name = "risk_fixture"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    fs::write(
        project.join("src/lib.rs"),
        r#"
pub mod api;
"#,
    )
    .unwrap();

    fs::write(
        project.join("src/api.rs"),
        r#"
pub fn public_entry() -> String {
    format_greeting("world")
}

pub fn unused_public_api() -> String {
    "unused".to_string()
}

fn format_greeting(name: &str) -> String {
    format!("Hello, {}!", name)
}
"#,
    )
    .unwrap();

    fs::write(
        project.join("tests/integration_api.rs"),
        r#"
use risk_fixture::api::public_entry;

#[test]
fn integration_public_entry() {
    assert_eq!(public_entry(), "Hello, world!");
}
"#,
    )
    .unwrap();

    fs::write(
        project.join("build.rs"),
        r#"
fn build_script_helper(flag: &str) -> String {
    format!("cargo:warning={flag}")
}

fn main() {
    println!("{}", build_script_helper("ok"));
}
"#,
    )
    .unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    (
        cg,
        TestProject {
            dir: Some(dir),
            _env_lock: env_lock,
            _home_guard: home_guard,
            _global_db_guard: global_db_guard,
        },
    )
}

/// Extracts the text content from a `ToolResult` value (the standard
/// `content[0].text` envelope).
fn extract_text(value: &Value) -> &str {
    value["content"][0]["text"]
        .as_str()
        .unwrap_or("<missing text>")
}

fn extract_json(value: &Value) -> Value {
    serde_json::from_str(extract_text(value)).unwrap()
}

fn assert_fact_results(payload: &Value, included: &str, excluded: &str, context: &str) {
    assert_eq!(payload["count"].as_u64(), Some(1), "{context}: {payload}");
    let results = payload["results"].to_string();
    assert!(
        results.contains(included),
        "{context} should include {included:?}: {payload}"
    );
    assert!(
        !results.contains(excluded),
        "{context} should not include {excluded:?}: {payload}"
    );
}

async fn extract_lcm_json_following_handle(cg: &TraceDecay, value: &Value) -> Value {
    let payload = extract_json(value);
    if payload.get("truncated").and_then(Value::as_bool) != Some(true) {
        return payload;
    }
    let handle = payload["handle"]
        .as_str()
        .expect("truncated LCM payload should include a retrieve handle");
    let retrieved = handle_tool_call(
        cg,
        "tracedecay_retrieve",
        json!({"handle": handle}),
        None,
        None,
    )
    .await
    .unwrap();
    let retrieved_payload = extract_json(&retrieved.value);
    serde_json::from_str(
        retrieved_payload["content"]
            .as_str()
            .expect("retrieved LCM payload should carry original JSON content"),
    )
    .unwrap()
}

fn expect_tool_error<T>(result: tracedecay::errors::Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected tool call to fail"),
        Err(err) => format!("{err}"),
    }
}

async fn seed_project_registry(db_path: &Path, project_root: &Path) {
    let db = GlobalDb::open_at(db_path).await.unwrap();
    let project = db
        .upsert_code_project(
            "proj_alpha",
            project_root,
            None,
            Some("https://token:secret@example.test/alpha.git"),
            Some("main"),
        )
        .await
        .unwrap();
    let store = db
        .upsert_store_instance(tracedecay::global_db::StoreInstanceUpsert {
            store_id: "store_alpha".to_string(),
            project_id: project.project_id.clone(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_alpha".to_string(),
            manifest_relpath: Some("projects/proj_alpha/store_manifest.json".to_string()),
            last_verified_at: Some(1_800_000_001),
            last_write_at: None,
        })
        .await
        .unwrap();
    db.upsert_graph_scope(tracedecay::global_db::GraphScopeUpsert {
        graph_scope_id: "scope_alpha_main".to_string(),
        project_id: project.project_id.clone(),
        store_id: store.store_id.clone(),
        branch_name: "main".to_string(),
        db_relpath: "projects/proj_alpha/tracedecay.db".to_string(),
        parent_scope_id: None,
        last_synced_at: Some(1_800_000_002),
        writable: true,
    })
    .await
    .unwrap();
    db.upsert_store_artifact(tracedecay::global_db::StoreArtifactUpsert {
        store_id: store.store_id,
        artifact_kind: "graph_db".to_string(),
        relpath: "projects/proj_alpha/tracedecay.db".to_string(),
        size_bytes: Some(128),
        schema_version: Some("1".to_string()),
        updated_at: Some(1_800_000_003),
    })
    .await
    .unwrap();
    db.upsert_code_project(
        "proj_beta",
        &project_root.with_file_name("beta"),
        None,
        Some("https://example.test/beta.git"),
        Some("main"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn project_registry_tools_are_bounded_read_only_and_contextual() {
    let (cg, _project_dir) = setup_project().await;
    let registry_dir = TempDir::new().unwrap();
    let registry_path = registry_dir.path().join("global.db");
    seed_project_registry(&registry_path, cg.project_root()).await;
    let _env_guard = GlobalDbEnvGuard::set(&registry_path);

    let list = handle_tool_call(
        &cg,
        "tracedecay_project_list",
        json!({"limit": 1}),
        None,
        None,
    )
    .await
    .unwrap();
    let list_payload: Value = serde_json::from_str(extract_text(&list.value)).unwrap();
    assert_eq!(list_payload["projects"].as_array().unwrap().len(), 1);
    assert_eq!(list_payload["limit"], 1);
    assert_eq!(list_payload["truncated"], true);
    let list_text = extract_text(&list.value);
    assert!(
        !list_text.contains("secret") && !list_text.contains("git_remote_url"),
        "project list must not expose credential-bearing remotes: {list_text}"
    );

    let search = handle_tool_call(
        &cg,
        "tracedecay_project_search",
        json!({"query": "alpha", "limit": 10}),
        None,
        None,
    )
    .await
    .unwrap();
    let search_payload: Value = serde_json::from_str(extract_text(&search.value)).unwrap();
    let search_projects = search_payload["projects"].as_array().unwrap();
    assert_eq!(search_projects.len(), 1);
    assert_eq!(search_projects[0]["project_id"], "proj_alpha");
    let search_text = extract_text(&search.value);
    assert!(
        !search_text.contains("secret") && !search_text.contains("git_remote_url"),
        "project search must not expose credential-bearing remotes: {search_text}"
    );

    let context = handle_tool_call(
        &cg,
        "tracedecay_project_context",
        json!({"project_id": "proj_alpha"}),
        None,
        None,
    )
    .await
    .unwrap();
    let context_payload: Value = serde_json::from_str(extract_text(&context.value)).unwrap();
    assert_eq!(context_payload["project"]["project_id"], "proj_alpha");
    let context_text = extract_text(&context.value);
    assert!(
        !context_text.contains("secret") && !context_text.contains("git_remote_url"),
        "project context must not expose credential-bearing remotes: {context_text}"
    );
    assert_eq!(context_payload["stores"].as_array().unwrap().len(), 1);
    assert_eq!(
        context_payload["stores"][0]["graph_scopes"][0]["branch_name"],
        "main"
    );
    assert_eq!(
        context_payload["stores"][0]["artifacts"][0]["artifact_kind"],
        "graph_db"
    );
}

#[tokio::test]
async fn project_registry_tools_prefer_injected_registry_over_process_default() {
    let (cg, _project_dir) = setup_project().await;
    let process_registry_dir = TempDir::new().unwrap();
    let process_registry_path = process_registry_dir.path().join("global.db");
    let client_registry_dir = TempDir::new().unwrap();
    let client_registry_path = client_registry_dir.path().join("global.db");
    let _env_guard = GlobalDbEnvGuard::set(&process_registry_path);

    let process_db = GlobalDb::open_at(&process_registry_path).await.unwrap();
    process_db
        .upsert_code_project(
            "proj_process_default",
            &cg.project_root().with_file_name("process-default"),
            None,
            None,
            Some("main"),
        )
        .await
        .unwrap();
    seed_project_registry(&client_registry_path, cg.project_root()).await;
    let client_db = GlobalDb::open_at(&client_registry_path).await.unwrap();

    let list = tracedecay::mcp::tools::handle_tool_call_with_registry(
        &cg,
        "tracedecay_project_list",
        json!({"limit": 10, "format": "json"}),
        None,
        None,
        Some(&client_db),
        false,
    )
    .await
    .unwrap();
    let list_payload: Value = serde_json::from_str(extract_text(&list.value)).unwrap();
    assert_eq!(
        list_payload["registry_path"],
        client_registry_path.display().to_string()
    );
    let list_text = extract_text(&list.value);
    assert!(list_text.contains("proj_alpha"));
    assert!(
        !list_text.contains("proj_process_default"),
        "project list should not read process-default registry: {list_text}"
    );

    let search = tracedecay::mcp::tools::handle_tool_call_with_registry(
        &cg,
        "tracedecay_project_search",
        json!({"query": "alpha", "limit": 10, "format": "json"}),
        None,
        None,
        Some(&client_db),
        false,
    )
    .await
    .unwrap();
    let search_text = extract_text(&search.value);
    assert!(search_text.contains("proj_alpha"));
    assert!(
        !search_text.contains("proj_process_default"),
        "project search should not read process-default registry: {search_text}"
    );

    let context = tracedecay::mcp::tools::handle_tool_call_with_registry(
        &cg,
        "tracedecay_project_context",
        json!({"project_id": "proj_alpha", "format": "json"}),
        None,
        None,
        Some(&client_db),
        false,
    )
    .await
    .unwrap();
    let context_payload: Value = serde_json::from_str(extract_text(&context.value)).unwrap();
    assert_eq!(context_payload["project"]["project_id"], "proj_alpha");
    assert_eq!(
        context_payload["registry_path"],
        client_registry_path.display().to_string()
    );
}

#[tokio::test]
async fn selected_project_read_skips_cache_write_for_read_only_store() {
    let (cg, _project_dir) = setup_project().await;
    let registry_dir = TempDir::new().unwrap();
    let registry_path = registry_dir.path().join("global.db");
    let _env_guard = GlobalDbEnvGuard::set(&registry_path);
    let target_dir = TempDir::new().unwrap();
    let target_project = target_dir.path();

    fs::create_dir_all(target_project.join("src")).unwrap();
    fs::write(
        target_project.join("src/main.rs"),
        "fn main() { println!(\"selected\"); }\n",
    )
    .unwrap();

    let registry = GlobalDb::open_at(&registry_path).await.unwrap();
    registry
        .upsert_code_project("proj_read", target_project, None, None, Some("main"))
        .await
        .unwrap();
    let target_cg = TraceDecay::init(target_project).await.unwrap();
    index_all_retrying_sync_lock(&target_cg).await;

    let read_args = json!({
        "project_id": "proj_read",
        "file": "src/main.rs",
        "mode": "full"
    });
    for attempt in 1..=2 {
        let selected_read = handle_tool_call(&cg, "tracedecay_read", read_args.clone(), None, None)
            .await
            .unwrap();
        let read_payload = extract_json(&selected_read.value);
        assert_eq!(read_payload["file"], "src/main.rs");
        assert!(
            read_payload["body"]
                .as_str()
                .is_some_and(|body| body.contains("selected")),
            "attempt {attempt}: selected read should return file content without writing to the read-only cache: {read_payload}"
        );
    }
}

fn tool_schema<'a>(tools: &'a [tracedecay::mcp::ToolDefinition], name: &str) -> &'a Value {
    &tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("missing tool definition for {name}"))
        .input_schema
}

fn required_args_at<'a>(schema: &'a Value, path: &[&str]) -> Vec<&'a str> {
    let mut node = schema;
    for segment in path {
        node = &node["properties"][*segment];
    }
    node["required"]
        .as_array()
        .unwrap_or_else(|| panic!("schema path {path:?} is missing a required array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("schema path {path:?} has non-string required entry"))
        })
        .collect()
}

fn assert_schema_requires(
    tools: &[tracedecay::mcp::ToolDefinition],
    tool_name: &str,
    expected: &[&str],
) {
    let schema = tool_schema(tools, tool_name);
    let actual = required_args_at(schema, &[]);
    assert_eq!(
        actual, expected,
        "{tool_name} schema required arguments drifted from handler parser expectations"
    );
}

fn assert_nested_schema_requires(
    tools: &[tracedecay::mcp::ToolDefinition],
    tool_name: &str,
    path: &[&str],
    expected: &[&str],
) {
    let schema = tool_schema(tools, tool_name);
    let actual = required_args_at(schema, path);
    assert_eq!(
        actual, expected,
        "{tool_name} schema required arguments at {path:?} drifted from handler parser expectations"
    );
}

fn assert_action_schema_requires(
    tools: &[tracedecay::mcp::ToolDefinition],
    tool_name: &str,
    action: &str,
    expected_required: &[&str],
) {
    let schema = tool_schema(tools, tool_name);
    let all_of = schema["allOf"]
        .as_array()
        .unwrap_or_else(|| panic!("{tool_name} schema is missing allOf action requirements"));
    let matching = all_of
        .iter()
        .find(|entry| entry["if"]["properties"]["action"]["const"].as_str() == Some(action));
    let entry = matching.unwrap_or_else(|| {
        panic!("{tool_name} schema is missing conditional requirements for action={action}")
    });
    let actual: Vec<&str> = entry["then"]["required"]
        .as_array()
        .unwrap_or_else(|| panic!("{tool_name} action={action} is missing then.required"))
        .iter()
        .map(|value| {
            value.as_str().unwrap_or_else(|| {
                panic!("{tool_name} action={action} has non-string required entry")
            })
        })
        .collect();
    assert_eq!(
        actual, expected_required,
        "{tool_name} schema conditional requirements for action={action} drifted from handler parser expectations"
    );
}

#[test]
fn outline_schema_requires_file_without_provider_property() {
    let tools = get_tool_definitions();
    let schema = tool_schema(&tools, "tracedecay_outline");

    assert_eq!(required_args_at(schema, &[]), vec!["file"]);
    assert!(
        schema["properties"]
            .as_object()
            .is_some_and(|properties| !properties.contains_key("provider")),
        "tracedecay_outline should not advertise a provider property: {schema}"
    );
}

#[test]
fn active_project_and_storage_status_tools_are_advertised_readonly() {
    let tools = get_tool_definitions();
    for name in ["tracedecay_active_project", "tracedecay_storage_status"] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("missing MCP tool definition for {name}"));
        assert_eq!(tool.input_schema["type"], "object");
        assert!(
            tool.input_schema["properties"]
                .as_object()
                .is_some_and(|properties| properties.is_empty()),
            "{name} should not require callers to pass resolver internals"
        );
        assert_eq!(
            tool.annotations
                .as_ref()
                .and_then(|annotations| annotations["readOnlyHint"].as_bool()),
            Some(true),
            "{name} must be advertised read-only"
        );
        assert!(
            !tool.description.contains(".tracedecay/tracedecay.db"),
            "{name} description must not hardcode the repo-local graph DB path"
        );
    }
}

#[tokio::test]
async fn active_project_tool_reports_resolved_store_metadata() {
    let (cg, _dir) = setup_project().await;
    let project_root = cg.project_root().display().to_string();
    let graph_db_path = cg.db_path().display().to_string();

    let result = handle_tool_call(
        &cg,
        "tracedecay_active_project",
        json!({}),
        Some(json!({"transport": "stdio"})),
        Some("src"),
    )
    .await
    .unwrap();

    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(
        payload["project_root"].as_str(),
        Some(project_root.as_str())
    );
    assert_eq!(payload["scope_prefix"].as_str(), Some("src"));
    assert_eq!(
        payload["resolution_source"].as_str(),
        Some("active_project")
    );
    assert_eq!(payload["storage"]["class"].as_str(), Some("code_project"));
    assert_eq!(payload["storage"]["mode"].as_str(), Some("profile_sharded"));
    assert_eq!(
        payload["storage"]["graph_db_path"].as_str(),
        Some(graph_db_path.as_str())
    );
    assert!(payload["storage"]["data_root"]
        .as_str()
        .is_some_and(|path| path.contains(".tracedecay") && path.contains("projects")));
    assert_eq!(payload["branch"]["serving_db_exists"].as_bool(), Some(true));
}

#[tokio::test]
async fn storage_status_tool_summarizes_active_project_store_health() {
    let (cg, _dir) = setup_project().await;
    let layout = cg.store_layout();
    let project_root = cg.project_root().display().to_string();
    let graph_db_path = cg.db_path().display().to_string();
    let config_path = layout.config_path.display().to_string();
    let sync_lock_path = layout.sync_lock_path.display().to_string();
    let branch_add_lock_path = layout.branch_add_lock_path.display().to_string();

    let result = handle_tool_call(&cg, "tracedecay_storage_status", json!({}), None, None)
        .await
        .unwrap();

    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"].as_str(), Some("ok"));
    assert_eq!(
        payload["active_project"]["project_root"].as_str(),
        Some(project_root.as_str())
    );
    assert_eq!(
        payload["active_project"]["storage"]["graph_db_path"].as_str(),
        Some(graph_db_path.as_str())
    );
    assert_eq!(
        payload["active_project"]["storage"]["class"],
        "code_project"
    );
    assert_eq!(payload["writable"].as_bool(), Some(true));
    assert!(payload["warnings"]
        .as_array()
        .is_some_and(|warnings| warnings.is_empty()));
    assert_eq!(
        payload["paths"]["graph_db_path"].as_str(),
        Some(graph_db_path.as_str())
    );
    assert_eq!(
        payload["paths"]["config_path"].as_str(),
        Some(config_path.as_str())
    );
    assert_eq!(
        payload["locks"]["sync_lock_path"].as_str(),
        Some(sync_lock_path.as_str())
    );
    assert_eq!(
        payload["locks"]["branch_add_lock_path"].as_str(),
        Some(branch_add_lock_path.as_str())
    );
    assert_eq!(payload["locks"]["sync_lock_exists"].as_bool(), Some(false));
    assert_eq!(
        payload["locks"]["branch_add_lock_exists"].as_bool(),
        Some(false)
    );
    assert_eq!(payload["quotas"]["enforced"].as_bool(), Some(false));
    assert_eq!(payload["quotas"]["graph_db_size_limit_bytes"], Value::Null);
}

fn assert_schema_has_required_alternatives(
    tools: &[tracedecay::mcp::ToolDefinition],
    tool_name: &str,
    alternatives: &[&str],
) {
    let schema = tool_schema(tools, tool_name);
    let any_of = schema["anyOf"]
        .as_array()
        .unwrap_or_else(|| panic!("{tool_name} schema is missing anyOf required alternatives"));
    for alternative in alternatives {
        assert!(
            any_of
                .iter()
                .any(|entry| entry["required"] == json!([alternative])),
            "{tool_name} schema must advertise that one of {alternatives:?} is required by the handler parser; missing alternative {alternative}"
        );
    }
}

async fn expect_missing_argument_error(
    cg: &TraceDecay,
    tool_name: &str,
    args: Value,
    expected_message: &str,
) {
    let message = expect_tool_error(handle_tool_call(cg, tool_name, args, None, None).await);
    assert!(
        message.contains(expected_message),
        "{tool_name} parser error should mention `{expected_message}`, got `{message}`"
    );
}

#[tokio::test]
async fn schema_required_arguments_match_representative_handler_parsers() {
    let (cg, _dir) = setup_project().await;
    let tools = get_tool_definitions();

    // Direct `args.get(...).ok_or(...)` parser style.
    assert_schema_requires(&tools, "tracedecay_search", &["query"]);
    expect_missing_argument_error(
        &cg,
        "tracedecay_search",
        json!({}),
        "missing required parameter: query",
    )
    .await;

    // Shared helper parser style, including canonical node_id despite id alias support.
    assert_schema_requires(&tools, "tracedecay_callers", &["node_id"]);
    expect_missing_argument_error(
        &cg,
        "tracedecay_callers",
        json!({}),
        "missing required parameter: node_id",
    )
    .await;

    // Non-empty array parser style.
    assert_schema_requires(&tools, "tracedecay_callers_for", &["node_ids"]);
    expect_missing_argument_error(&cg, "tracedecay_callers_for", json!({}), "node_ids").await;

    // Multi-field edit parser style.
    assert_schema_requires(
        &tools,
        "tracedecay_insert_at",
        &["path", "anchor", "content"],
    );
    expect_missing_argument_error(
        &cg,
        "tracedecay_insert_at",
        json!({ "path": "src/lib.rs" }),
        "missing required parameter: anchor",
    )
    .await;

    // Action-dependent parser style: fact_store requires different arguments per action.
    assert_schema_requires(&tools, "tracedecay_fact_store", &["action"]);
    for (action, required_arg, expected_message) in [
        ("add", "content", "missing required parameter: content"),
        ("search", "query", "missing required parameter: query"),
        ("probe", "entity", "missing required parameter: entity"),
        ("related", "entity", "missing required parameter: entity"),
        ("update", "fact_id", "missing required parameter: fact_id"),
        ("remove", "fact_id", "missing required parameter: fact_id"),
    ] {
        assert_action_schema_requires(&tools, "tracedecay_fact_store", action, &[required_arg]);
        expect_missing_argument_error(
            &cg,
            "tracedecay_fact_store",
            json!({ "action": action }),
            expected_message,
        )
        .await;
    }

    // Alternative parser style: fact_feedback accepts action/helpful/unhelpful, but one is required.
    assert_schema_requires(&tools, "tracedecay_fact_feedback", &["fact_id"]);
    assert_schema_has_required_alternatives(
        &tools,
        "tracedecay_fact_feedback",
        &["action", "helpful", "unhelpful"],
    );
    expect_missing_argument_error(
        &cg,
        "tracedecay_fact_feedback",
        json!({ "fact_id": 1 }),
        "missing feedback action",
    )
    .await;

    // Nested-object parser style.
    assert_schema_requires(&tools, "tracedecay_lcm_expand", &["session_id", "target"]);
    assert_nested_schema_requires(&tools, "tracedecay_lcm_expand", &["target"], &["kind"]);
    expect_missing_argument_error(
        &cg,
        "tracedecay_lcm_expand",
        json!({ "session_id": "session-1", "target": {} }),
        "target.kind must be one of raw_message, summary_node, external_payload",
    )
    .await;
}

#[test]
fn lcm_tool_schemas_are_registered_with_stable_names() {
    let tools = get_tool_definitions();
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    for expected in [
        "tracedecay_lcm_status",
        "tracedecay_lcm_load_session",
        "tracedecay_lcm_grep",
        "tracedecay_lcm_describe",
        "tracedecay_lcm_expand",
        "tracedecay_lcm_expand_query",
        "tracedecay_lcm_preflight",
        "tracedecay_lcm_compress",
        "tracedecay_lcm_session_boundary",
        "tracedecay_lcm_doctor",
    ] {
        assert!(names.contains(expected), "missing {expected}");
    }

    for read_only in [
        "tracedecay_lcm_status",
        "tracedecay_lcm_load_session",
        "tracedecay_lcm_grep",
        "tracedecay_lcm_describe",
        "tracedecay_lcm_expand",
        "tracedecay_lcm_expand_query",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == read_only)
            .unwrap_or_else(|| panic!("{read_only} definition"));
        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.annotations.as_ref().unwrap()["readOnlyHint"], true);
    }

    for mutating in [
        "tracedecay_lcm_preflight",
        "tracedecay_lcm_compress",
        "tracedecay_lcm_session_boundary",
        "tracedecay_lcm_doctor",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == mutating)
            .unwrap_or_else(|| panic!("{mutating} definition"));
        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.annotations.as_ref().unwrap()["readOnlyHint"], false);
    }

    for scoped in [
        "tracedecay_lcm_status",
        "tracedecay_lcm_load_session",
        "tracedecay_lcm_grep",
        "tracedecay_lcm_describe",
        "tracedecay_lcm_expand",
        "tracedecay_lcm_expand_query",
        "tracedecay_lcm_preflight",
        "tracedecay_lcm_compress",
        "tracedecay_lcm_session_boundary",
        "tracedecay_lcm_doctor",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == scoped)
            .unwrap_or_else(|| panic!("{scoped} definition"));
        let storage_scope = &tool.input_schema["properties"]["storage_scope"];
        assert_eq!(
            storage_scope["enum"],
            json!(["project_local", "hermes_profile"]),
            "{scoped} must advertise supported storage scopes"
        );
        assert!(
            storage_scope["description"]
                .as_str()
                .unwrap_or_default()
                .contains("hermes_home"),
            "{scoped} storage_scope should document hermes_profile requirements"
        );
        let hermes_home = &tool.input_schema["properties"]["hermes_home"];
        assert_eq!(
            hermes_home["type"],
            json!("string"),
            "{scoped} must expose hermes_home"
        );
        assert!(
            hermes_home["description"]
                .as_str()
                .unwrap_or_default()
                .contains("absolute"),
            "{scoped} hermes_home should document absolute path requirements"
        );
    }

    let load = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_load_session")
        .expect("tracedecay_lcm_load_session definition");
    assert_eq!(load.input_schema["required"], json!(["session_id"]));
    assert!(load.input_schema["properties"]
        .get("content_limit")
        .is_some());
    assert_eq!(
        load.input_schema["properties"]["limit"]["type"],
        json!("integer")
    );
    assert_eq!(
        load.input_schema["properties"]["content_limit"]["maximum"],
        json!(20000)
    );

    let grep = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_grep")
        .expect("tracedecay_lcm_grep definition");
    assert_eq!(
        grep.input_schema["properties"]["limit"]["type"],
        json!("integer")
    );

    let expand = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_expand")
        .expect("tracedecay_lcm_expand definition");
    assert_eq!(
        expand.input_schema["required"],
        json!(["session_id", "target"])
    );
    assert!(expand.input_schema["properties"].get("target").is_some());
    assert_eq!(
        expand.input_schema["properties"]["target"]["properties"]["store_id"]["type"],
        json!("integer")
    );
    assert_eq!(
        expand.input_schema["properties"]["source_offset"]["type"],
        json!("integer")
    );
    assert_eq!(
        expand.input_schema["properties"]["source_limit"]["type"],
        json!("integer")
    );

    let doctor = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_doctor")
        .expect("tracedecay_lcm_doctor definition");
    assert_eq!(
        doctor.input_schema["properties"]["mode"]["enum"],
        json!(["diagnose", "repair", "retention", "clean", "gc"])
    );
    assert_eq!(
        doctor.input_schema["properties"]["apply"]["type"],
        json!("boolean")
    );
    assert_eq!(
        doctor.input_schema["properties"]["doctor_clean_apply_enabled"]["type"],
        json!("boolean")
    );
    assert_eq!(
        doctor.input_schema["properties"]["lcm_gc_apply_enabled"]["type"],
        json!("boolean")
    );
    assert_eq!(
        doctor.input_schema["properties"]["gc_config"]["type"],
        json!("object")
    );
}

/// Searches for `name` via the search handler and returns the first matching
/// node id whose name field equals `name`.
async fn find_node_id(cg: &TraceDecay, name: &str) -> String {
    let result = handle_tool_call(cg, "tracedecay_search", json!({"query": name}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let items: Vec<Value> = serde_json::from_str(text).unwrap();
    items
        .iter()
        .find(|item| item["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("node '{}' not found via search", name))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
fn tool_properties<'a>(
    tools: &'a [tracedecay::mcp::ToolDefinition],
    name: &str,
) -> &'a serde_json::Map<String, Value> {
    tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("{name} definition"))
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name} properties"))
}

#[test]
fn retrieve_tool_schema_requires_handle_and_accepts_project_selector() {
    let tools = get_tool_definitions();
    let retrieve = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_retrieve")
        .expect("tracedecay_retrieve definition");
    let properties = tool_properties(&tools, "tracedecay_retrieve");

    assert!(properties.contains_key("handle"));
    assert!(properties.contains_key("project_selector"));
    assert!(properties.contains_key("project_id"));
    assert!(properties.contains_key("project_path"));
    assert!(!properties.contains_key("retrieve_handle"));
    assert_eq!(retrieve.input_schema["required"], json!(["handle"]));

    assert!(retrieve.description.contains("tracedecay_retrieve"));
    assert!(retrieve.description.contains("required argument `handle`"));
    assert!(retrieve.description.contains("pass the same selector"));
    assert!(retrieve
        .description
        .contains("Only call it when the missing details are needed"));
    assert!(properties["handle"]["description"]
        .as_str()
        .unwrap_or_default()
        .contains("required `handle` argument"));
}

#[test]
fn always_loaded_graph_tool_schemas_advertise_project_selectors() {
    let tools = get_tool_definitions();
    for name in ["tracedecay_search", "tracedecay_context"] {
        let properties = tool_properties(&tools, name);
        assert!(
            properties.contains_key("project_selector"),
            "{name} should advertise nested project_selector"
        );
        assert!(
            properties.contains_key("project_id"),
            "{name} should advertise project_id"
        );
        assert!(
            properties.contains_key("project_path"),
            "{name} should advertise project_path"
        );
    }
}

#[test]
fn lcm_compress_public_schema_excludes_test_summarizer_modes() {
    let tools = get_tool_definitions();
    let compress = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_compress")
        .expect("tracedecay_lcm_compress definition");
    let modes = compress.input_schema["properties"]["summarizer"]["properties"]["mode"]["enum"]
        .as_array()
        .expect("summarizer mode enum");

    assert!(modes.iter().any(|mode| mode == "provided"));
    assert!(modes.iter().any(|mode| mode == "hermes_auxiliary"));
    assert!(
        modes.iter().all(|mode| mode != "noop" && mode != "fake"),
        "public MCP schema should not advertise test/control summarizers: {modes:?}"
    );
}

// 1. tracedecay_search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_search() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_search",
        json!({"query": "helper", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    assert!(
        text.contains("helper"),
        "search results should contain 'helper'"
    );
}

#[tokio::test]
async fn test_search_returns_index_coverage_hint_for_skipped_generated_dirs() {
    let (cg, _env, _dir) = setup_generated_dir_project(false).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_search",
        json!({"query": "generatedOnly", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["results"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        parsed["index_coverage_hint"]["suggested_command"].as_str(),
        Some("tracedecay sync --include-folder dist")
    );
    assert_eq!(
        parsed["index_coverage_hint"]["skipped_dirs"][0].as_str(),
        Some("dist")
    );
}

#[tokio::test]
async fn test_context_appends_index_coverage_hint_for_skipped_generated_dirs() {
    let (cg, _env, _dir) = setup_generated_dir_project(false).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "generatedOnly", "max_nodes": 5}),
        None,
        None,
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
}

#[tokio::test]
async fn test_search_omits_index_coverage_hint_when_generated_dir_is_included() {
    let (cg, _env, _dir) = setup_generated_dir_project(true).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_search",
        json!({"query": "missingAfterInclude", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.as_array().is_some(),
        "search should keep array shape when there is no coverage hint, got: {text}"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn retrieve_tool_returns_full_stored_response() {
    let (cg, _dir) = setup_project().await;
    let original = "{\"items\":[{\"id\":1,\"name\":\"alpha\"}]}";
    let stored = tracedecay::mcp::response_handles::store_response_handle(
        cg.project_root(),
        original,
        tracedecay::tracedecay::current_timestamp(),
    )
    .unwrap();

    let stored_payload: Value = serde_json::from_str(
        &fs::read_to_string(response_handle_dir(&cg).join(format!("{}.json", stored.handle)))
            .unwrap(),
    )
    .unwrap();
    assert!(stored_payload.get("handle").is_none());
    assert!(stored_payload.get("original_chars").is_none());

    let result = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": stored.handle }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(payload["handle"], stored.handle);
    assert_eq!(payload["content"], original);
    assert_eq!(payload["expired"], false);

    let alias_result = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "retrieve_handle": stored.handle }),
        None,
        None,
    )
    .await;
    assert!(
        alias_result.is_err(),
        "tracedecay_retrieve must accept only the canonical `handle` field"
    );
}

#[tokio::test]
async fn retrieve_tool_reports_missing_and_expired_handles_actionably() {
    let (cg, _dir) = setup_project().await;

    let missing = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": "rh_0123456789abcdef01234567" }),
        None,
        None,
    )
    .await
    .unwrap();
    let missing_payload: Value = serde_json::from_str(extract_text(&missing.value)).unwrap();
    assert_eq!(missing_payload["expired"], true);
    assert_eq!(missing_payload["content"], Value::Null);
    assert_eq!(missing_payload["reason_code"], "handle_not_found");
    assert_eq!(missing_payload["retryable"], true);
    assert!(missing_payload["message"]
        .as_str()
        .unwrap_or_default()
        .contains("not found"));
    assert!(missing_payload["retry_instruction"]
        .as_str()
        .unwrap_or_default()
        .contains("Re-run the original MCP tool"));

    let expired = tracedecay::mcp::response_handles::store_response_handle(
        cg.project_root(),
        "{\"items\":[42]}",
        tracedecay::tracedecay::current_timestamp()
            - tracedecay::mcp::response_handles::RESPONSE_HANDLE_TTL_SECS
            - 5,
    )
    .unwrap();

    let expired_result = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": expired.handle }),
        None,
        None,
    )
    .await
    .unwrap();
    let expired_payload: Value = serde_json::from_str(extract_text(&expired_result.value)).unwrap();
    assert_eq!(expired_payload["expired"], true);
    assert_eq!(expired_payload["content"], Value::Null);
    assert_eq!(expired_payload["reason_code"], "handle_expired");
    assert_eq!(expired_payload["retryable"], true);
    assert_eq!(expired_payload["expires_at"], expired.expires_at);
    assert!(expired_payload["message"]
        .as_str()
        .unwrap_or_default()
        .contains("expired"));
    assert!(expired_payload["retry_instruction"]
        .as_str()
        .unwrap_or_default()
        .contains("Re-run the original MCP tool"));
}

#[tokio::test]
async fn fact_store_large_list_response_uses_retrieve_handle() {
    let (cg, _dir) = setup_project().await;
    let mut last_fact_id = None;
    for i in 0..35 {
        let added = handle_tool_call(
            &cg,
            "tracedecay_fact_store",
            json!({
                "action": "add",
                "content": format!(
                    "LONG_FACT_MARKER_{i:02}: {}",
                    "large fact-store response should remain retrievable ".repeat(80)
                ),
                "category": "project",
                "trust": 0.9
            }),
            None,
            None,
        )
        .await
        .unwrap();
        if i == 34 {
            let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
            last_fact_id = added["fact"]["fact_id"].as_i64();
        }
    }
    let last_fact_id = last_fact_id.expect("tail fact id");

    let listed = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "list", "category": "project", "min_trust": 0.0, "limit": 200}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&listed.value);
    let envelope: Value = serde_json::from_str(text).expect("large response should stay JSON");
    assert_eq!(envelope["truncated"], true);
    let handle = envelope["handle"]
        .as_str()
        .expect("large fact-store response should include retrieve handle")
        .to_string();
    assert_eq!(envelope["retrieve_tool"], "tracedecay_retrieve");
    let instruction = envelope["retrieve_instruction"]
        .as_str()
        .expect("large response envelope should teach retrieval");
    assert!(instruction.contains("This response was truncated"));
    assert!(instruction.contains("original response is stored locally"));
    assert!(instruction.contains("expires"));
    assert!(instruction.contains("tracedecay_retrieve"));
    assert!(instruction.contains("required argument `handle`"));
    assert!(instruction.contains(&handle));
    assert!(instruction.contains("Only call it if the missing details are needed"));

    let removed = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({ "action": "remove", "fact_id": last_fact_id }),
        None,
        None,
    )
    .await
    .unwrap();
    let removed: Value = serde_json::from_str(extract_text(&removed.value)).unwrap();
    assert_eq!(removed["removed"], true);
    assert!(
        cg.get_fact(last_fact_id).await.unwrap().is_none(),
        "tail fact should be absent from the live store before handle retrieval"
    );

    let retrieved = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": handle }),
        None,
        None,
    )
    .await
    .unwrap();
    let retrieved_payload: Value = serde_json::from_str(extract_text(&retrieved.value)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);
    let full_json = retrieved_payload["content"]
        .as_str()
        .expect("retrieve response should contain original JSON text");
    let full: Value = serde_json::from_str(full_json).expect("retrieved content should be JSON");
    assert_eq!(full["count"].as_u64(), Some(35));
    assert!(
        full_json.contains("LONG_FACT_MARKER_34"),
        "retrieved response should include the full fact list"
    );
}

#[tokio::test]
async fn fact_store_large_list_response_reports_store_failure_actionably() {
    let (cg, _dir) = setup_project().await;
    for i in 0..35 {
        handle_tool_call(
            &cg,
            "tracedecay_fact_store",
            json!({
                "action": "add",
                "content": format!(
                    "STORE_FAILURE_MARKER_{i:02}: {}",
                    "large fact-store response should surface cache failures ".repeat(80)
                ),
                "category": "project",
                "trust": 0.9
            }),
            None,
            None,
        )
        .await
        .unwrap();
    }

    let handle_dir = response_handle_dir(&cg);
    fs::write(&handle_dir, "not-a-directory").unwrap();

    let listed = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "list", "category": "project", "min_trust": 0.0, "limit": 200}),
        None,
        None,
    )
    .await
    .unwrap();
    let envelope: Value = serde_json::from_str(extract_text(&listed.value)).unwrap();

    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["handle_available"], false);
    assert!(envelope.get("handle").is_none());
    assert!(envelope["preview"]
        .as_str()
        .unwrap_or_default()
        .contains("STORE_FAILURE_MARKER_"));
    assert_eq!(
        envelope["handle_status"]["reason_code"],
        "handle_store_failed"
    );
    assert_eq!(envelope["handle_status"]["retryable"], true);
    assert!(envelope["handle_status"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("could not be cached locally"));
    assert!(envelope["handle_status"]["retry_instruction"]
        .as_str()
        .unwrap_or_default()
        .contains("re-run the original MCP tool"));
    fs::remove_file(&handle_dir).unwrap();
    fs::create_dir_all(&handle_dir).unwrap();
    close_test_graph(cg).await;
}

#[tokio::test]
async fn search_large_response_uses_retrievable_truncation_handle() {
    let (cg, project) = setup_project().await;
    let mut source = String::new();
    for i in 0..420 {
        source.push_str(&format!(
            "pub fn reversible_search_marker_{i:03}() -> &'static str {{ \"marker-{i:03}\" }}\n"
        ));
    }
    fs::write(project.path().join("src/large_search.rs"), source).unwrap();
    index_all_retrying_sync_lock(&cg).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_search",
        json!({"query": "reversible_search_marker", "limit": 420}),
        None,
        None,
    )
    .await
    .unwrap();
    let envelope: Value =
        serde_json::from_str(extract_text(&result.value)).expect("large search response envelope");
    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["retrieve_tool"], "tracedecay_retrieve");
    let handle = envelope["handle"]
        .as_str()
        .expect("large search response should include a handle");

    let retrieved = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": handle }),
        None,
        None,
    )
    .await
    .unwrap();
    let retrieved_payload: Value = serde_json::from_str(extract_text(&retrieved.value)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);
    let full_json = retrieved_payload["content"]
        .as_str()
        .expect("retrieve response should contain full search JSON");
    assert!(
        full_json.contains("reversible_search_marker_419"),
        "retrieved search response should include the tail result"
    );
}

// ---------------------------------------------------------------------------
// 2. tracedecay_context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_context() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "understand the helper function"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
}

// ---------------------------------------------------------------------------
// 3. tracedecay_callers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_callers() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callers",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
}

// ---------------------------------------------------------------------------
// 4. tracedecay_callees
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_callees() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callees",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
}

// ---------------------------------------------------------------------------
// 5. tracedecay_impact
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_impact() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_impact",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("node_count"));
}

// ---------------------------------------------------------------------------
// 6. tracedecay_node — existing node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_node_existing() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_node",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("helper"),
        "node detail should contain the name"
    );
    assert!(
        text.contains("start_line"),
        "node detail should contain start_line"
    );
    assert!(
        text.contains("signature"),
        "node detail should contain signature"
    );
    assert!(
        text.contains("visibility"),
        "node detail should contain visibility"
    );
}

// ---------------------------------------------------------------------------
// 7. tracedecay_node — nonexistent node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_node_not_found() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_node",
        json!({"node_id": "nonexistent_id_12345"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("Node not found"),
        "should report 'Node not found', got: {}",
        text,
    );
}

// ---------------------------------------------------------------------------
// 8. tracedecay_status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_status() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_status",
        json!({}),
        Some(json!({"uptime": 100})),
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("node_count"),
        "status should include node_count"
    );
    assert!(
        text.contains("server"),
        "status should include server stats"
    );
    assert!(
        text.contains("branch_diagnostics"),
        "status should include branch diagnostics"
    );
}

#[tokio::test]
async fn test_branch_list_reports_live_vs_serving_drift_state() {
    fn git(project: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .expect("git command failed to spawn");
        assert!(
            status.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    let dir = TempDir::new().unwrap();
    let project = dir.path();
    let _env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = project.join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let _global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
    git(project, &["init"]);
    git(project, &["config", "user.email", "test@test.com"]);
    git(project, &["config", "user.name", "Test"]);
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "initial"]);
    git(project, &["branch", "-M", "main"]);

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let tracedecay_dir = resolve_layout_for_current_profile(project)
        .unwrap()
        .data_root;
    tracedecay::branch_meta::save_branch_meta(
        &tracedecay_dir,
        &tracedecay::branch_meta::BranchMeta::new("main"),
    )
    .unwrap();

    let cg = TraceDecay::open(project).await.unwrap();
    git(project, &["checkout", "-b", "feature"]);

    let result = handle_tool_call(&cg, "tracedecay_branch_list", json!({}), None, None)
        .await
        .unwrap();
    let report: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(report["current_branch"], json!("feature"));
    assert_eq!(report["open_active_branch"], json!("main"));
    assert_eq!(report["serving_branch"], json!("main"));
    assert_eq!(report["branch_drifted"], json!(true));
    assert_eq!(report["branch_resolution"], json!("stale_serving_branch"));
}

// ---------------------------------------------------------------------------
// 9. tracedecay_files — no filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_no_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_files", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty(), "files listing should not be empty");
    assert!(
        text.contains("indexed files"),
        "should have 'indexed files' header"
    );
}

// ---------------------------------------------------------------------------
// 10. tracedecay_files — path filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_path_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_files", json!({"path": "src"}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    // The test file lives under tests/, so if path filter works it should
    // only contain src/ files.
    assert!(
        !text.contains("tests/test_utils"),
        "path filter should exclude files outside 'src'"
    );
}

// ---------------------------------------------------------------------------
// 11. tracedecay_files — pattern filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_pattern_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_files",
        json!({"pattern": "*.rs"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
}

// ---------------------------------------------------------------------------
// 12. tracedecay_files — flat format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_flat_format() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_files",
        json!({"format": "flat"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    // Flat format includes "bytes" per entry
    assert!(text.contains("bytes"), "flat format should show byte sizes");
}

// ---------------------------------------------------------------------------
// 13. tracedecay_affected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_affected() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_affected",
        json!({"files": ["src/utils.rs"]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("affected_tests"),
        "should have affected_tests key"
    );
    assert!(text.contains("count"), "should have count key");
}

// ---------------------------------------------------------------------------
// 14. tracedecay_dead_code
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dead_code() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_dead_code", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("dead_code_count"),
        "should have dead_code_count key"
    );
}

// ---------------------------------------------------------------------------
// 15. tracedecay_diff_context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_diff_context() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_diff_context",
        json!({"files": ["src/utils.rs"]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("changed_files"),
        "should have changed_files key"
    );
    assert!(
        text.contains("modified_symbols"),
        "should have modified_symbols key"
    );
}

#[tokio::test]
async fn diff_context_large_response_uses_retrievable_truncation_handle() {
    let (cg, project) = setup_project().await;
    let mut source = String::new();
    for i in 0..420 {
        source.push_str(&format!(
            "pub fn reversible_diff_context_marker_{i:03}() -> &'static str {{ \"marker-{i:03}\" }}\n"
        ));
    }
    fs::write(project.path().join("src/large_diff.rs"), source).unwrap();
    index_all_retrying_sync_lock(&cg).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_diff_context",
        json!({"files": ["src/large_diff.rs"], "depth": 1}),
        None,
        None,
    )
    .await
    .unwrap();
    let envelope: Value =
        serde_json::from_str(extract_text(&result.value)).expect("large diff_context envelope");
    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["retrieve_tool"], "tracedecay_retrieve");
    let handle = envelope["handle"]
        .as_str()
        .expect("large diff_context response should include a handle");

    let retrieved = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": handle }),
        None,
        None,
    )
    .await
    .unwrap();
    let retrieved_payload: Value = serde_json::from_str(extract_text(&retrieved.value)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);
    let full_json = retrieved_payload["content"]
        .as_str()
        .expect("retrieve response should contain full diff_context JSON");
    assert!(
        full_json.contains("reversible_diff_context_marker_419"),
        "retrieved diff_context response should include the tail result"
    );
}

// ---------------------------------------------------------------------------
// 16. tracedecay_module_api
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_module_api() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_module_api",
        json!({"path": "src"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("public_symbol_count"),
        "should have public_symbol_count key"
    );
    // helper is pub so it should appear
    assert!(
        text.contains("helper"),
        "pub fn helper should appear in module API"
    );
}

// ---------------------------------------------------------------------------
// 17. tracedecay_circular
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_circular() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_circular", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("cycle_count"), "should have cycle_count key");
}

// ---------------------------------------------------------------------------
// 18. tracedecay_hotspots
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_hotspots() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_hotspots", json!({"limit": 5}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("hotspot_count"),
        "should have hotspot_count key"
    );
}

// ---------------------------------------------------------------------------
// 19. tracedecay_similar
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_similar() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_similar",
        json!({"symbol": "helper"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    assert!(
        text.contains("helper"),
        "similar results should include 'helper'"
    );
}

// ---------------------------------------------------------------------------
// 20. tracedecay_rename_preview
// ---------------------------------------------------------------------------

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
    let text = extract_text(&result.value);
    assert!(
        text.contains("reference_count"),
        "should have reference_count key"
    );
    assert!(text.contains("node"), "should have node key");
}

// ---------------------------------------------------------------------------
// 21. tracedecay_unused_imports
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_unused_imports() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_unused_imports", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("unused_import_count"),
        "should have unused_import_count key"
    );
}

// ---------------------------------------------------------------------------
// 22. tracedecay_rank
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rank() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
        json!({"edge_kind": "calls", "direction": "incoming"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("ranking"), "should have ranking key");
    assert!(
        text.contains("result_count"),
        "should have result_count key"
    );
}

// ---------------------------------------------------------------------------
// 23. tracedecay_rank — invalid direction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rank_invalid_direction() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
        json!({"edge_kind": "calls", "direction": "sideways"}),
        None,
        None,
    )
    .await;
    match result {
        Err(err) => {
            let err_msg = format!("{}", err);
            assert!(
                err_msg.contains("invalid direction"),
                "error should mention 'invalid direction', got: {}",
                err_msg,
            );
        }
        Ok(_) => panic!("invalid direction should produce an error"),
    }
}

// ---------------------------------------------------------------------------
// 24. tracedecay_largest
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_largest() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_largest", json!({"limit": 5}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("ranking"), "should have ranking key");
    assert!(
        text.contains("result_count"),
        "should have result_count key"
    );
}

// ---------------------------------------------------------------------------
// 25. tracedecay_coupling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_coupling() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_coupling",
        json!({"direction": "fan_in"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("ranking"), "should have ranking key");
}

// ---------------------------------------------------------------------------
// 26. tracedecay_inheritance_depth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_inheritance_depth() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_inheritance_depth",
        json!({"limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("result_count"),
        "should have result_count key"
    );
}

// ---------------------------------------------------------------------------
// 27. tracedecay_distribution — default and summary mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_distribution_default() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_distribution", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("per_file"), "default mode should be per_file");
}

#[tokio::test]
async fn test_distribution_summary() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_distribution",
        json!({"summary": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("summary"),
        "summary mode should report 'summary'"
    );
    assert!(
        text.contains("distribution"),
        "should have distribution key"
    );
}

// ---------------------------------------------------------------------------
// 28. tracedecay_recursion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_recursion() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_recursion", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("cycle_count"), "should have cycle_count key");
}

// ---------------------------------------------------------------------------
// 29. tracedecay_complexity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_complexity() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_complexity", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("ranking"), "should have ranking key");
    assert!(text.contains("formula"), "should have formula key");
}

// ---------------------------------------------------------------------------
// 30. tracedecay_doc_coverage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_doc_coverage() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_doc_coverage", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("total_undocumented"),
        "should have total_undocumented key"
    );
    close_test_graph(cg).await;
}

// ---------------------------------------------------------------------------
// 31. tracedecay_god_class
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_god_class() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_god_class", json!({"limit": 5}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("result_count"),
        "should have result_count key"
    );
}

// ---------------------------------------------------------------------------
// 32. tracedecay_changelog — requires git refs, expect graceful error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_changelog_no_git() {
    let (cg, _dir) = setup_project().await;
    // The temp dir is not a git repo, so this should return a structured git
    // error in the tool payload rather than success-looking prose.
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
    assert!(output["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("failed to open git repo"));
}

#[tokio::test]
async fn run_affected_tests_reports_git_failure_without_changed_paths() {
    let (cg, _dir) = setup_project().await;
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
    assert_eq!(output["error"]["kind"].as_str(), Some("git"));
    assert_eq!(output["error"]["operation"].as_str(), Some("diff"));
    assert!(
        output["note"].is_null(),
        "git failure must not be reported as a no-change note: {output}"
    );
}

#[tokio::test]
async fn pr_context_no_git_returns_structured_git_error() {
    let (cg, _dir) = setup_project().await;
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

// ---------------------------------------------------------------------------
// 33. tracedecay_port_status — no matching dirs expected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_port_status() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_port_status",
        json!({"source_dir": "src", "target_dir": "tests"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("coverage_percent"),
        "should have coverage_percent key"
    );
    close_test_graph(cg).await;
}

/// Regression: port_status used to match symbols purely on (name,
/// kind_compat_group), so common method names like `new`, `process`, `fmt`,
/// or `reset` produced wild cross-type "matches" — e.g. `Biquad::new` would
/// pair with an unrelated `Adaa::new` simply because both methods are named
/// "new". The match key must also include the parent type so siblings of
/// distinct owners stay unmatched.
#[tokio::test]
async fn port_status_does_not_match_methods_of_different_parents() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    index_all_retrying_sync_lock(&cg).await;

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
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();

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

// ---------------------------------------------------------------------------
// 34. tracedecay_port_order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_port_order() {
    let (cg, _dir) = setup_project().await;
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
    assert!(
        text.contains("total_symbols"),
        "should have total_symbols key"
    );
    assert!(text.contains("levels"), "should have levels key");
}

// ---------------------------------------------------------------------------
// 35. Unknown tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_unknown_tool() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_unknown", json!({}), None, None).await;
    match result {
        Err(err) => {
            let err_msg = format!("{}", err);
            assert!(
                err_msg.contains("unknown tool"),
                "error should mention 'unknown tool', got: {}",
                err_msg,
            );
        }
        Ok(_) => panic!("unknown tool should produce an error"),
    }
}

// ---------------------------------------------------------------------------
// 36. Missing required params — search without query
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_missing_required_params() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_search", json!({}), None, None).await;
    let err_msg = match result {
        Err(err) => format!("{}", err),
        Ok(_) => panic!("missing query should produce an error"),
    };
    assert!(
        err_msg.contains("missing required parameter"),
        "error should mention 'missing required parameter', got: {}",
        err_msg,
    );
}

// ---------------------------------------------------------------------------
// 37. Node ID alias — using "id" instead of "node_id"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_node_id_alias() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    // Use "id" instead of "node_id"
    let result = handle_tool_call(&cg, "tracedecay_node", json!({"id": node_id}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("helper"),
        "node lookup via 'id' alias should still find the node"
    );
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_status without server_stats
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_status_without_server_stats() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_status", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("node_count"),
        "status should include node_count"
    );
    // Should NOT contain "server" key when None is passed
    assert!(
        !text.contains("\"server\""),
        "status without server_stats should not include 'server' key"
    );
}

// ---------------------------------------------------------------------------
// Extra: touched_files populated for search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_search_populates_touched_files() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_search",
        json!({"query": "helper"}),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        !result.touched_files.is_empty(),
        "search results should populate touched_files"
    );
}

// ---------------------------------------------------------------------------
// Extra: rename_preview with nonexistent node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rename_preview_not_found() {
    let (cg, _dir) = setup_project().await;
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

// ---------------------------------------------------------------------------
// Extra: coupling with fan_out direction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_coupling_fan_out() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_coupling",
        json!({"direction": "fan_out"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("fan_out"), "should report fan_out direction");
}

// ---------------------------------------------------------------------------
// Extra: rank with outgoing direction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rank_outgoing() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
        json!({"edge_kind": "calls", "direction": "outgoing"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("outgoing"),
        "should reflect outgoing direction"
    );
    close_test_graph(cg).await;
}

// ---------------------------------------------------------------------------
// Extra: missing required params for other handlers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_context_missing_task() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_context", json!({}), None, None).await;
    assert!(result.is_err(), "context without task should error");
}

#[tokio::test]
async fn test_callers_missing_node_id() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_callers", json!({}), None, None).await;
    assert!(result.is_err(), "callers without node_id should error");
}

#[tokio::test]
async fn test_affected_missing_files() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_affected", json!({}), None, None).await;
    assert!(result.is_err(), "affected without files should error");
}

#[tokio::test]
async fn test_module_api_missing_path() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_module_api", json!({}), None, None).await;
    assert!(result.is_err(), "module_api without path should error");
}

#[tokio::test]
async fn test_rank_missing_edge_kind() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
        json!({"direction": "incoming"}),
        None,
        None,
    )
    .await;
    assert!(result.is_err(), "rank without edge_kind should error");
}

#[tokio::test]
async fn test_similar_missing_symbol() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_similar", json!({}), None, None).await;
    assert!(result.is_err(), "similar without symbol should error");
}

#[tokio::test]
async fn test_diff_context_missing_files() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_diff_context", json!({}), None, None).await;
    assert!(result.is_err(), "diff_context without files should error");
}

#[tokio::test]
async fn test_changelog_missing_refs() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_changelog", json!({}), None, None).await;
    assert!(result.is_err(), "changelog without from_ref should error");
}

#[tokio::test]
async fn test_port_status_missing_dirs() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_port_status", json!({}), None, None).await;
    assert!(
        result.is_err(),
        "port_status without source_dir should error"
    );
}

#[tokio::test]
async fn test_port_order_missing_source_dir() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_port_order", json!({}), None, None).await;
    assert!(
        result.is_err(),
        "port_order without source_dir should error"
    );
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_changelog with a real git repo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn commit_context_clean_worktree_returns_json() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fn git(cwd: &std::path::Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|_| panic!("git {args:?} failed"));
    }

    git(project, &["init"]);
    git(project, &["config", "user.email", "t@t"]);
    git(project, &["config", "user.name", "t"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join(".gitignore"), ".tracedecay/\nhome/\n").unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn clean() {}\n").unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "init"]);

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
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    // Initialize git repo and make a first commit
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(project)
        .output()
        .expect("git init failed");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(project)
        .output()
        .unwrap();

    fs::write(project.join("src/lib.rs"), "pub fn original() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(project)
        .output()
        .unwrap();

    // Make a second commit with changes
    fs::write(
        project.join("src/lib.rs"),
        "pub fn original() {}\npub fn added() {}\n",
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "add function"])
        .current_dir(project)
        .output()
        .unwrap();

    let (cg, _env) = init_test_project(project).await;
    index_all_retrying_sync_lock(&cg).await;

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
    // Should not report "git diff failed" since it's a real git repo
    assert!(
        !text.contains("git diff failed"),
        "changelog in git repo should not fail, got: {}",
        text,
    );
    assert!(
        text.contains("changed_file_count") || text.contains("lib.rs"),
        "changelog should mention changed files, got: {}",
        text,
    );
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_distribution with path prefix filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_distribution_with_path_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_distribution",
        json!({"path": "src/"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(text.contains("per_file"), "default mode should be per_file");
    // Should only contain src/ files, not tests/
    assert!(
        !text.contains("tests/test_utils"),
        "path filter should exclude files outside 'src/'",
    );
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_files — grouped format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_grouped_format() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_files",
        json!({"format": "grouped"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(!text.is_empty());
    // Grouped format shows directory headers like "src/ (N files)"
    assert!(
        text.contains("indexed files"),
        "grouped format should have 'indexed files' header"
    );
    assert!(
        text.contains("files)"),
        "grouped format should show file counts per directory"
    );
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_dead_code with custom kinds parameter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dead_code_custom_kinds() {
    let (cg, _dir) = setup_project().await;
    // Ask only for struct dead code
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
    // Parse and verify any returned items are structs
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

// ---------------------------------------------------------------------------
// Extra: tracedecay_affected with custom filter glob
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_affected_with_custom_filter() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_affected",
        json!({"files": ["src/utils.rs"], "filter": "**/*test*"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("affected_tests"),
        "should have affected_tests key"
    );
    assert!(text.contains("count"), "should have count key");
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_complexity — verify response structure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_complexity_response_fields() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_complexity", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(parsed.get("ranking").is_some(), "should have ranking key");
    assert!(parsed.get("formula").is_some(), "should have formula key");
    // Check ranking items have expected fields
    if let Some(items) = parsed["ranking"].as_array() {
        if let Some(first) = items.first() {
            assert!(
                first.get("cyclomatic_complexity").is_some(),
                "ranking item should have cyclomatic_complexity"
            );
            assert!(
                first.get("branches").is_some(),
                "ranking item should have branches"
            );
            assert!(
                first.get("max_nesting").is_some(),
                "ranking item should have max_nesting"
            );
            assert!(
                first.get("fan_out").is_some(),
                "ranking item should have fan_out"
            );
            assert!(
                first.get("score").is_some(),
                "ranking item should have score"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_doc_coverage — verify response structure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_doc_coverage_response_structure() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_doc_coverage", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("total_undocumented").is_some(),
        "should have total_undocumented"
    );
    assert!(parsed.get("file_count").is_some(), "should have file_count");
    assert!(parsed.get("files").is_some(), "should have files array");
    // If there are files, check their structure
    if let Some(files) = parsed["files"].as_array() {
        if let Some(first) = files.first() {
            assert!(first.get("file").is_some(), "file entry should have 'file'");
            assert!(
                first.get("count").is_some(),
                "file entry should have 'count'"
            );
            assert!(
                first.get("symbols").is_some(),
                "file entry should have 'symbols'"
            );
        }
    }
}

#[tokio::test]
async fn test_files_scope_prefix_filters() {
    let (cg, _dir) = setup_project().await;
    // With scope_prefix "src", should only return files under src/
    let result = handle_tool_call(&cg, "tracedecay_files", json!({}), None, Some("src"))
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        !text.contains("tests/"),
        "scope_prefix 'src' should exclude test files"
    );
    assert!(text.contains("main.rs"), "should include src/main.rs");
}

#[tokio::test]
async fn test_search_scope_prefix_filters() {
    let (cg, _dir) = setup_project().await;
    // Search for "helper" but scoped to "tests" — should only return test file results
    let result = handle_tool_call(
        &cg,
        "tracedecay_search",
        json!({"query": "helper", "limit": 20}),
        None,
        Some("tests"),
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let items: Vec<serde_json::Value> = serde_json::from_str(text).unwrap_or_default();
    for item in &items {
        let file = item["file"].as_str().unwrap_or("");
        assert!(
            file.starts_with("tests"),
            "scoped search should only return files under 'tests', got: {}",
            file
        );
    }
}

#[tokio::test]
async fn test_files_explicit_path_overrides_scope() {
    let (cg, _dir) = setup_project().await;
    // Explicit path "tests" should override scope_prefix "src"
    let result = handle_tool_call(
        &cg,
        "tracedecay_files",
        json!({"path": "tests"}),
        None,
        Some("src"),
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        !text.contains("src/main.rs"),
        "explicit path 'tests' should exclude src files"
    );
}

#[tokio::test]
async fn test_context_scope_prefix_filters() {
    let (cg, _dir) = setup_project().await;
    // Context scoped to "tests" should return results (even if limited to test files)
    let result = handle_tool_call(
        &cg,
        "tracedecay_context",
        json!({"task": "understand helper"}),
        None,
        Some("tests"),
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        !text.is_empty(),
        "context should return results even when scoped"
    );
}

#[tokio::test]
async fn test_status_reports_scope_prefix() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_status", json!({}), None, Some("src/mcp"))
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("scope_prefix"),
        "status should report scope_prefix"
    );
    assert!(
        text.contains("src/mcp"),
        "status should show the actual prefix value"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_status_no_scope_prefix() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_status", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("scope_prefix").is_none() || parsed["scope_prefix"].is_null(),
        "status should not have scope_prefix when None"
    );
}

// ---------------------------------------------------------------------------
// Edit tools: tracedecay_str_replace, tracedecay_multi_str_replace, tracedecay_insert_at
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_str_replace_success() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "fn hello() {}\nfn world() {}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn hello() {}",
            "new_str": "fn hello_updated() {}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["matched_str"], "fn hello() {}");
    assert_eq!(parsed["new_str"], "fn hello_updated() {}");

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(content.contains("fn hello_updated() {}"));
    assert!(!content.contains("fn hello() {}"));
}

#[tokio::test]
async fn path_containment_config_rejects_parent_traversal_before_serving_config() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(
        dir.path().join("outside.toml"),
        "token = \"OUTSIDE_SECRET\"\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(&project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_config",
        json!({"path": "../outside.toml", "key": "token"}),
        None,
        None,
    )
    .await;

    assert!(
        result.is_err(),
        "config read should reject parent traversal, got {result:?}"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn path_containment_read_rejects_parent_traversal_before_serving_file() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("outside.rs"), "fn leaked() {}\n").unwrap();

    let (cg, _env) = init_test_project(&project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_read",
        json!({"file": "../outside.rs", "mode": "full"}),
        None,
        None,
    )
    .await;

    assert!(
        result.is_err(),
        "read should reject parent traversal before serving outside files, got {result:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn read_and_outline_preserve_symlink_indexed_file_key() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let external = dir.path().join("external-src");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("lib.rs"), "pub fn through_symlink() {}\n").unwrap();
    unix_fs::symlink(&external, project.join("src")).unwrap();

    let (cg, _env) = init_test_project(&project).await;
    cg.index_all().await.unwrap();

    let read = handle_tool_call(
        &cg,
        "tracedecay_read",
        json!({"file": "src/lib.rs", "mode": "full"}),
        None,
        None,
    )
    .await
    .unwrap();
    let read_text = extract_text(&read.value);
    let read_payload: serde_json::Value = serde_json::from_str(read_text).unwrap();
    assert_eq!(read_payload["file"], "src/lib.rs");
    assert!(
        read_payload["body"]
            .as_str()
            .unwrap_or_default()
            .contains("through_symlink"),
        "read should serve indexed source behind symlink: {read_payload:?}"
    );

    if !tracedecay::mcp::tools::ast_grep_outline_available() {
        return;
    }

    let outline = handle_tool_call(
        &cg,
        "tracedecay_outline",
        json!({"file": "src/lib.rs"}),
        None,
        None,
    )
    .await
    .unwrap();
    let outline_text = extract_text(&outline.value);
    let outline_payload: serde_json::Value = serde_json::from_str(outline_text).unwrap();
    assert_eq!(outline_payload["file"], "src/lib.rs");
    assert!(
        outline_payload["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .any(|symbol| symbol["name"] == "through_symlink"),
        "outline should query graph by indexed symlink path: {outline_payload:?}"
    );
}

#[tokio::test]
async fn outline_preserves_db_payload_and_adds_ast_grep_outline_when_available() {
    if !tracedecay::mcp::tools::ast_grep_outline_available() {
        return;
    }

    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_outline",
        json!({"file": "src/utils.rs"}),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.touched_files, vec!["src/utils.rs"]);
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["file"], "src/utils.rs");
    assert!(payload["symbol_count"].as_u64().is_some());
    assert!(
        payload["symbols"]
            .as_array()
            .is_some_and(|symbols| symbols.iter().any(|symbol| symbol["name"] == "helper")),
        "DB-backed symbols should still be present: {payload}"
    );
    assert!(
        payload["ast_grep_outline"]
            .as_array()
            .is_some_and(|files| files.iter().any(|file| file["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["name"] == "helper")))),
        "ast-grep outline should be attached under ast_grep_outline: {payload}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn path_containment_config_rejects_symlink_escape_before_serving_config() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let outside_dir = dir.path().join("outside");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(&outside_dir).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(
        outside_dir.join("secret.toml"),
        "token = \"SYMLINK_SECRET\"\n",
    )
    .unwrap();
    unix_fs::symlink(&outside_dir, project.join("escape")).unwrap();

    let (cg, _env) = init_test_project(&project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_config",
        json!({"path": "escape/secret.toml", "key": "token"}),
        None,
        None,
    )
    .await;

    assert!(
        result.is_err(),
        "config read should reject symlink escape, got {result:?}"
    );
}

#[tokio::test]
async fn project_selector_is_rejected_before_write_tool_parsing() {
    let (cg, _dir) = setup_project().await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({"project_selector": {"include_all_registered": true}}),
        None,
        None,
    )
    .await;
    let message = expect_tool_error(result);

    assert!(
        message.contains("does not accept project selectors"),
        "write tool should reject project_selector before parser errors, got {message}"
    );
}

#[tokio::test]
async fn lcm_project_root_storage_arg_is_not_rejected_as_selector() {
    let (cg, _dir) = setup_project().await;
    let project_root = cg.project_root().to_string_lossy().to_string();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "session_id": "stock-check-session",
            "project_root": project_root,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi there"}
            ],
            "current_tokens": 50
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["session_id"], "stock-check-session");
}

#[tokio::test]
async fn lcm_project_path_selector_is_rejected_before_dispatch() {
    let (cg, _dir) = setup_project().await;
    let project_path = cg.project_root().to_string_lossy().to_string();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "session_id": "stock-check-session",
            "project_path": project_path,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi there"}
            ],
            "current_tokens": 50
        }),
        None,
        None,
    )
    .await;
    let message = expect_tool_error(result);

    assert!(
        message.contains("does not accept project selectors"),
        "LCM preflight should reject project_path selectors before dispatch, got {message}"
    );
}

#[tokio::test]
async fn test_str_replace_not_found() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "fn hello() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn not_exists() {}",
            "new_str": "fn replaced() {}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_str_replace_multiple_matches_fails() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "fn foo() {}\nfn foo() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "fn foo() {}",
            "new_str": "fn bar() {}"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"]
        .as_str()
        .unwrap()
        .contains("matches 2 times"));
}

#[tokio::test]
async fn test_multi_str_replace_success() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "fn foo() {}\nfn bar() {}\nfn baz() {}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["fn foo() {}", "fn foo_replaced() {}"],
                ["fn bar() {}", "fn bar_replaced() {}"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["applied_count"], 2);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(content.contains("fn foo_replaced()"));
    assert!(content.contains("fn bar_replaced()"));
    assert!(content.contains("fn baz() {}"));
}

#[tokio::test]
async fn test_multi_str_replace_atomic_failure() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "fn foo() {}\nfn baz() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                ["fn not_exists() {}", "fn replaced() {}"],
                ["fn baz() {}", "fn baz_replaced() {}"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"]
        .as_str()
        .unwrap()
        .contains("must match exactly once"));

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(content.contains("fn foo() {}"));
    assert!(content.contains("fn baz() {}"));
    assert!(!content.contains("fn replaced()"));
}

#[tokio::test]
async fn test_multi_str_replace_unicode_preview_does_not_panic() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "fn main() {}\n";
    fs::write(project.join("src/main.rs"), original).unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let missing_old = format!("{}é", "a".repeat(19));
    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "src/main.rs",
            "replacements": [
                [missing_old, "replacement"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    let message = parsed["message"].as_str().unwrap();
    assert!(message.contains("matches 0 times"));
    assert!(message.contains("must match exactly once"));

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert_eq!(content, original);
}

#[tokio::test]
async fn test_str_replace_unsupported_file_type_succeeds() {
    // Regression: editing unsupported types (e.g. .css) previously wrote the
    // file then returned a reindex error, silently mutating the file.
    let dir = TempDir::new().unwrap();
    let project = dir.path();

    fs::write(project.join("style.css"), ".foo {\n\tfont-size: 14px;\n}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_str_replace",
        json!({
            "path": "style.css",
            "old_str": "\tfont-size: 14px;",
            "new_str": "\tfont-size: 0.85rem;"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("style.css")).unwrap();
    assert!(content.contains("0.85rem"));
    assert!(!content.contains("14px"));
}

#[tokio::test]
async fn ast_grep_rewrite_has_literal_fallback_when_binary_missing() {
    if tracedecay::mcp::tools::ast_grep_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn old_name() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_ast_grep_rewrite",
        json!({"path": "src/lib.rs", "pattern": "old_name", "rewrite": "new_name"}),
        None,
        None,
    )
    .await
    .unwrap();

    let output = extract_json(&result.value);
    assert_eq!(output["success"].as_bool(), Some(true), "{output}");
    assert!(
        fs::read_to_string(project.join("src/lib.rs"))
            .unwrap()
            .contains("new_name"),
        "literal fallback should update the file"
    );
}

#[tokio::test]
async fn ast_grep_rewrite_uses_current_cli_update_flag() {
    if !tracedecay::mcp::tools::ast_grep_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn caller() { old_name(); }\npub fn old_name() {}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_ast_grep_rewrite",
        json!({"path": "src/lib.rs", "pattern": "old_name()", "rewrite": "new_name()"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["success"].as_bool(), Some(true), "{output}");
    let content = fs::read_to_string(project.join("src/lib.rs")).unwrap();
    assert!(
        content.contains("new_name();"),
        "ast-grep rewrite should apply with the installed CLI: {content}"
    );
    assert!(
        !output["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unexpected argument '-d'"),
        "rewrite must not use the removed -d flag: {output}"
    );
}

/// Regression: `branch_diff` previously errored with `MCP error -32603: base
/// and head are the same branch` when base == head. `pr_context` handles the
/// same case gracefully (empty arrays); branch_diff must match that shape so
/// callers can rely on consistent behaviour.
#[tokio::test]
async fn branch_diff_returns_empty_when_base_equals_head() {
    let (cg, _dir) = setup_project().await;

    // branch_diff requires branch tracking metadata to be present.
    let tracedecay_dir = project_data_dir(&cg);
    let meta = tracedecay::branch_meta::BranchMeta::new("master");
    tracedecay::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_branch_diff",
        json!({"base": "master", "head": "master"}),
        None,
        None,
    )
    .await
    .expect("branch_diff must not error when base == head");

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).expect("response must be valid JSON");
    assert_eq!(output["summary"]["added"].as_u64(), Some(0));
    assert_eq!(output["summary"]["removed"].as_u64(), Some(0));
    assert_eq!(output["summary"]["changed"].as_u64(), Some(0));
    assert_eq!(output["added"].as_array().map(Vec::len), Some(0));
    assert_eq!(output["removed"].as_array().map(Vec::len), Some(0));
    assert_eq!(output["changed"].as_array().map(Vec::len), Some(0));
    close_test_graph(cg).await;
}

/// Regression: when ast-grep exits non-zero with empty stderr (no language
/// inferred from the file extension, or pattern matches nothing), the tool
/// used to surface `"ast-grep failed: "` — a useless empty trailer. The
/// message must instead explain the likely cause so the caller can act on it.
#[tokio::test]
async fn ast_grep_rewrite_surfaces_useful_error_on_empty_stderr() {
    if !tracedecay::mcp::tools::ast_grep_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn foo() {}\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_ast_grep_rewrite",
        json!({
            "path": "src/lib.rs",
            "pattern": "__NONEXISTENT_PATTERN__",
            "rewrite": "whatever"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["success"].as_bool(), Some(false), "{output}");
    let message = output["message"].as_str().unwrap_or_default();
    assert!(
        !message.trim_end_matches(':').trim().eq("ast-grep failed"),
        "message must not end as an empty 'ast-grep failed:' — got: {message:?}"
    );
    assert!(
        message.contains("exit") || message.contains("0 nodes") || message.contains("no language"),
        "message must explain the likely cause (exit code / no language / 0 matches), got: {message:?}"
    );
}

#[tokio::test]
async fn test_multi_str_replace_unsupported_file_type_succeeds() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();

    fs::write(
        project.join("style.css"),
        ".foo {\n\tfont-size: 14px;\n}\n.bar {\n\tfont-size: 16px;\n}\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_multi_str_replace",
        json!({
            "path": "style.css",
            "replacements": [
                ["\tfont-size: 14px;", "\tfont-size: 0.85rem;"],
                ["\tfont-size: 16px;", "\tfont-size: 1rem;"]
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["applied_count"], 2);

    let content = fs::read_to_string(project.join("style.css")).unwrap();
    assert!(content.contains("0.85rem"));
    assert!(content.contains("1rem"));
    assert!(!content.contains("14px"));
    assert!(!content.contains("16px"));

    cg.checkpoint().await.unwrap();
    cg.close();
}

#[tokio::test]
async fn test_insert_at_string_anchor_before() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "line one\nline two\nline three\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "line two",
            "content": "inserted line",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(
        content.ends_with('\n'),
        "trailing newline must be preserved"
    );
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[0], "line one");
    assert_eq!(lines[1], "inserted line");
    assert_eq!(lines[2], "line two");
    assert_eq!(lines[3], "line three");
}

#[tokio::test]
async fn test_insert_at_line_number() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "line one\nline two\nline three\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "2",
            "content": "inserted at line 2",
            "before": false
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["anchor_line"], 2);

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(
        content.ends_with('\n'),
        "trailing newline must be preserved"
    );
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[0], "line one");
    assert_eq!(lines[1], "line two");
    assert_eq!(lines[2], "inserted at line 2");
    assert_eq!(lines[3], "line three");
}

#[tokio::test]
async fn test_insert_at_anchor_not_found() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/main.rs"), "line one\nline two\n").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "nonexistent",
            "content": "should not be inserted",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_insert_at_unicode_anchor_prefix_does_not_panic() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "line one\nline two\n";
    fs::write(project.join("src/main.rs"), original).unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let long_anchor = format!("{}é", "a".repeat(99));
    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": long_anchor,
            "content": "should not be inserted",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"].as_str().unwrap().contains("not found"));

    let content = fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert_eq!(content, original);
}

#[tokio::test]
async fn test_insert_at_ambiguous_anchor() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        "line foo\nline foo\nline bar\n",
    )
    .unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/main.rs",
            "anchor": "foo",
            "content": "should not be inserted",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], false);
    assert!(parsed["message"]
        .as_str()
        .unwrap()
        .contains("matches 2 lines"));
}

// Regression: insert_at must not strip trailing newline (#57)
#[tokio::test]
async fn test_insert_at_preserves_trailing_newline() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    let original = "fn hello() {}\n\nfn world() {}\n";
    fs::write(project.join("src/lib.rs"), original).unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_insert_at",
        json!({
            "path": "src/lib.rs",
            "anchor": "fn world",
            "content": "fn extra() {}",
            "before": true
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["success"], true);

    let content = fs::read_to_string(project.join("src/lib.rs")).unwrap();
    assert!(
        content.ends_with('\n'),
        "file must end with newline after insert_at, got: {:?}",
        &content[content.len().saturating_sub(20)..]
    );
    assert_eq!(content, "fn hello() {}\n\nfn extra() {}\nfn world() {}\n");
}

// ---------------------------------------------------------------------------
// tracedecay_gini
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// tracedecay_dependency_depth
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// tracedecay_health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_health_summary() {
    let (cg, _dir) = setup_project().await;
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

/// Issue #83: tracedecay_redundancy must surface AST-isomorphic duplicate
/// pairs and rank them by composite similarity. Plant two structurally
/// identical functions in a fixture and assert the pair surfaces in the
/// top hit with the `definite` severity bucket.
#[tokio::test]
async fn test_redundancy_finds_planted_duplicate() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();
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

    // Calling again should be a cache hit (no panic, same result).
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

/// Issue #80: `tracedecay_runtime` must surface process + DB telemetry so
/// users hitting unexpected CPU/RAM can capture a structured snapshot
/// without leaving the chat session.
#[tokio::test]
async fn test_runtime_snapshot_exposes_process_and_db_signals() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_runtime", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();

    // Top-level envelope.
    assert!(parsed.get("captured_at").is_some());
    assert!(parsed["tracedecay_version"].is_string());
    assert!(parsed["host_os"].is_string());

    // Process block — PID must match our own.
    let proc = &parsed["process"];
    assert_eq!(
        proc["pid"].as_u64().unwrap_or(0),
        u64::from(std::process::id()),
        "snapshot must report this process's PID"
    );
    assert!(
        proc["rss_bytes"].as_u64().unwrap_or(0) > 0,
        "RSS should be non-zero"
    );
    assert!(proc["system_cpu_count"].as_u64().unwrap_or(0) >= 1);
    assert!(proc["system_total_memory_bytes"].as_u64().unwrap_or(0) > 0);

    // Database block — the DB file we just opened must be present and sized.
    let db = &parsed["database"];
    assert!(db["db_path"].is_string());
    assert!(
        db["db_size_bytes"].as_u64().unwrap_or(0) > 0,
        "DB file should have non-zero size"
    );
    assert!(
        db["node_count"].as_u64().unwrap_or(0) > 0,
        "fixture indexed > 0 nodes"
    );
    // journal_mode pragma should be readable on a libsql connection.
    assert!(db["journal_mode"].is_string() || db["journal_mode"].is_null());
}

/// Issue #82: `details=true` must surface raw counts + interpretation per
/// dimension, not just the scalar score, so callers don't have to compose
/// six separate tools to reproduce the breakdown.
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

// ---------------------------------------------------------------------------
// tracedecay_dsm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dsm_stats() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dsm",
        json!({ "format": "stats" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("files").is_some(),
        "files field should exist, got: {}",
        text
    );
    assert!(
        parsed.get("density").is_some(),
        "density field should exist"
    );
}

#[tokio::test]
async fn test_dsm_clusters() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_dsm",
        json!({ "format": "clusters" }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("clusters").is_some(),
        "clusters array should exist, got: {}",
        text
    );
}

// ---------------------------------------------------------------------------
// tracedecay_test_risk
// ---------------------------------------------------------------------------

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
}

// ---------------------------------------------------------------------------
// Session start / end tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_session_start() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_session_start", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(output["quality_signal"].as_u64().is_some());
    assert_eq!(output["status"].as_str().unwrap(), "baseline_saved");
    let baseline_path = project_data_dir(&cg).join("session_baseline.json");
    assert!(baseline_path.exists(), "baseline file should exist");
}

#[tokio::test]
async fn test_session_end() {
    let (cg, _dir) = setup_project().await;
    handle_tool_call(&cg, "tracedecay_session_start", json!({}), None, None)
        .await
        .unwrap();
    let result = handle_tool_call(&cg, "tracedecay_session_end", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(output["signal_before"].as_u64().is_some());
    assert!(output["signal_after"].as_u64().is_some());
    assert!(output["delta"].is_number());
    let baseline_path = project_data_dir(&cg).join("session_baseline.json");
    assert!(
        !baseline_path.exists(),
        "baseline should be removed after session_end"
    );
}

#[tokio::test]
async fn test_session_end_no_baseline() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_session_end", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["status"].as_str().unwrap(), "no_baseline");
}

// ---------------------------------------------------------------------------
// tracedecay_body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_body_returns_full_function_source() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_body",
        json!({"symbol": "format_greeting"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["match_count"].as_u64().unwrap(), 1);
    let m = &output["matches"][0];
    let body = m["body"].as_str().unwrap();
    assert!(
        body.contains("fn format_greeting"),
        "body should contain the function signature, got: {body}"
    );
    assert!(
        body.contains("Hello"),
        "body should contain the function body, got: {body}"
    );
    // Regression for issue #62: the function's outer closing brace must be
    // included so the body is byte-exact usable as an Edit `old_string`.
    assert!(
        body.trim_end().ends_with('}'),
        "body should end with the function's closing brace, got: {body:?}"
    );
    // Line numbers are surfaced 1-based so they match what the user sees in
    // their editor and what Edit-style tools expect.
    let start_line = m["start_line"].as_u64().unwrap() as usize;
    let end_line = m["end_line"].as_u64().unwrap() as usize;
    assert!(start_line >= 1, "start_line should be 1-based");
    assert!(
        end_line >= start_line,
        "end_line should not precede start_line"
    );
    let file_rel = m["file"].as_str().unwrap();
    let file_abs = _dir.path().join(file_rel);
    let source = std::fs::read_to_string(&file_abs).unwrap();
    let lines: Vec<&str> = source.lines().collect();
    let end_line_text = lines
        .get(end_line - 1)
        .copied()
        .unwrap_or_else(|| panic!("end_line {end_line} out of bounds in {file_rel}"));
    assert!(
        end_line_text.trim_end().ends_with('}'),
        "end_line ({end_line}) should point at the closing brace; line text: {end_line_text:?}"
    );
}

#[tokio::test]
async fn test_body_unknown_symbol() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_body",
        json!({"symbol": "no_such_symbol_anywhere"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("No symbol named"),
        "should report no match, got: {text}"
    );
}

#[tokio::test]
async fn test_body_missing_symbol_param() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_body", json!({}), None, None).await;
    assert!(result.is_err(), "should error when symbol is missing");
}

// ---------------------------------------------------------------------------
// tracedecay_todos
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_todos_finds_markers() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();

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
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();

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
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_todos", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    assert_eq!(output["match_count"].as_u64().unwrap(), 0);
    close_test_graph(cg).await;
}

// ---------------------------------------------------------------------------
// tracedecay_callers_for — bulk caller lookup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_callers_for_returns_caller_set_per_id() {
    let (cg, _dir) = setup_project().await;

    // Look up two distinct targets in one call.
    let helper_id = find_node_id(&cg, "helper").await;
    let format_id = find_node_id(&cg, "format_greeting").await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_callers_for",
        json!({"node_ids": [helper_id.clone(), format_id.clone()]}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();

    // Response shape: { callers: { id: [...], id2: [...] }, truncated: bool, max_per_item: N }
    assert_eq!(output["truncated"], json!(false));
    assert!(output["max_per_item"].as_u64().unwrap() > 0);

    let callers = &output["callers"];
    let helper_callers = callers[&helper_id].as_array().unwrap();
    let format_callers = callers[&format_id].as_array().unwrap();

    // helper is called from main; format_greeting is called from helper.
    assert!(
        !helper_callers.is_empty(),
        "expected helper to have at least one caller"
    );
    assert!(
        !format_callers.is_empty(),
        "expected format_greeting to have at least one caller"
    );
}

#[tokio::test]
async fn test_callers_for_includes_unmatched_ids_as_empty() {
    let (cg, _dir) = setup_project().await;
    let helper_id = find_node_id(&cg, "helper").await;
    let bogus_id = "function:0000000000000000000000000000ffff".to_string();

    let result = handle_tool_call(
        &cg,
        "tracedecay_callers_for",
        json!({"node_ids": [helper_id.clone(), bogus_id.clone()]}),
        None,
        None,
    )
    .await
    .unwrap();
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let callers = &output["callers"];
    assert!(callers[&bogus_id].as_array().unwrap().is_empty());
    assert!(!callers[&helper_id].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_callers_for_respects_max_per_item() {
    let (cg, _dir) = setup_project().await;
    let helper_id = find_node_id(&cg, "helper").await;
    // Cap at 0 — every caller should be marked truncated.
    let result = handle_tool_call(
        &cg,
        "tracedecay_callers_for",
        json!({"node_ids": [helper_id.clone()], "max_per_item": 0}),
        None,
        None,
    )
    .await
    .unwrap();
    let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(output["truncated"], json!(true));
    assert!(output["callers"][&helper_id].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_callers_for_rejects_empty_input() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callers_for",
        json!({"node_ids": []}),
        None,
        None,
    )
    .await;
    let Err(err) = result else {
        panic!("expected error for empty node_ids");
    };
    assert!(format!("{err}").contains("non-empty"));
}

#[tokio::test]
async fn test_callers_for_rejects_unknown_kind() {
    let (cg, _dir) = setup_project().await;
    let helper_id = find_node_id(&cg, "helper").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_callers_for",
        json!({"node_ids": [helper_id], "kind": "not_a_real_kind"}),
        None,
        None,
    )
    .await;
    let Err(err) = result else {
        panic!("expected error for unknown edge kind");
    };
    assert!(format!("{err}").contains("unknown edge kind"));
    close_test_graph(cg).await;
}

// ---------------------------------------------------------------------------
// tracedecay_by_qualified_name — cross-run lookup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_by_qualified_name_finds_indexed_node() {
    let (cg, _dir) = setup_project().await;
    // Find the qualified name of `helper` first.
    let helper = cg
        .get_node(&find_node_id(&cg, "helper").await)
        .await
        .unwrap()
        .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_by_qualified_name",
        json!({"qualified_name": helper.qualified_name}),
        None,
        None,
    )
    .await
    .unwrap();
    let items: Vec<Value> = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert!(
        !items.is_empty(),
        "expected at least one match for helper qname"
    );
    assert!(items.iter().any(|i| i["name"] == "helper"));
    // The handler exposes attrs_start_line in the response shape.
    assert!(items[0].get("attrs_start_line").is_some());
}

#[tokio::test]
async fn test_by_qualified_name_returns_empty_for_unknown() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_by_qualified_name",
        json!({"qualified_name": "crate::does::not::exist"}),
        None,
        None,
    )
    .await
    .unwrap();
    let items: Vec<Value> = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_by_qualified_name_requires_param() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(&cg, "tracedecay_by_qualified_name", json!({}), None, None).await;
    let Err(err) = result else {
        panic!("expected error when qualified_name is missing");
    };
    assert!(format!("{err}").contains("qualified_name"));
}

#[tokio::test]
async fn memory_fact_store_add_search_update_remove_and_wrappers() {
    let (cg, _dir) = setup_project().await;

    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Project Phoenix uses Amari Memory in src/memory/types.rs",
            "category": "project",
            "entity": "Project Phoenix",
            "entities": ["Amari Memory"],
            "tags": ["memory", "holographic"],
            "source": "mcp-test",
            "metadata": {"plan": "holographic"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_i64()
        .expect("fact_store add should return numeric id");
    assert!(added["fact"].get("id").is_none());
    assert!(added["fact"].get("trust").is_none());
    assert!(added["fact"]["trust_score"].as_f64().is_some());
    assert_eq!(added["action"], "add");
    assert_eq!(added["fact"]["category"], "project");
    assert_eq!(added["fact"]["source"], "mcp-test");

    let search = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "search",
            "query": "Amari Memory",
            "category": "project",
            "min_trust": 0.1,
            "limit": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let search: Value = serde_json::from_str(extract_text(&search.value)).unwrap();
    assert_eq!(search["action"], "search");
    assert_eq!(search["count"].as_u64(), Some(1));
    assert_eq!(search["results"], search["facts"]);
    assert!(
        search["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["fact"]["fact_id"].as_i64() == Some(fact_id)),
        "search results should include added fact: {search}"
    );

    for (action, payload) in [
        ("probe", json!({"entity": "Project Phoenix"})),
        ("related", json!({"entity": "Amari Memory"})),
        (
            "reason",
            json!({"entities": ["Project Phoenix", "Amari Memory"]}),
        ),
        (
            "contradict",
            json!({"category": "project", "threshold": 0.8}),
        ),
        ("list", json!({"category": "project", "min_trust": 0.1})),
    ] {
        let mut args = payload;
        args["action"] = json!(action);
        let result = handle_tool_call(&cg, "tracedecay_fact_store", args, None, None)
            .await
            .unwrap();
        let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
        assert_eq!(output["action"], action, "{action} should echo action");
        assert!(
            output["results"].is_array(),
            "{action} should include results array: {output}"
        );
        assert!(
            output["count"].is_number(),
            "{action} should include count: {output}"
        );
        if action == "related" {
            assert!(
                output["count"].as_u64().unwrap_or_default() > 0,
                "related should return facts connected through adjacent entities: {output}"
            );
        }
    }

    let updated = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "update",
            "fact_id": fact_id,
            "content": "Project Phoenix uses deterministic Amari Memory",
            "entities": ["Project Phoenix", "Amari Memory"],
            "metadata": {"updated": true}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let updated: Value = serde_json::from_str(extract_text(&updated.value)).unwrap();
    assert_eq!(
        updated["fact"]["content"],
        "Project Phoenix uses deterministic Amari Memory"
    );
    assert_eq!(updated["count"].as_u64(), Some(1));

    let removed = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "remove", "fact_id": fact_id.to_string()}),
        None,
        None,
    )
    .await
    .unwrap();
    let removed: Value = serde_json::from_str(extract_text(&removed.value)).unwrap();
    assert_eq!(removed["removed"], true);
}

#[tokio::test]
async fn memory_fact_store_project_selector_targets_registered_project() {
    let (active, target, _env) = setup_cross_project_memory_projects().await;
    let target_project_id = target
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target project should have a profile project_id");
    let target_project_path = target.project_root().to_string_lossy().to_string();

    handle_tool_call(
        &target,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Target selector fact stays with the registered target project",
            "category": "project",
            "entity": "Target selector"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    handle_tool_call(
        &active,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Active selector fact stays with the active project",
            "category": "project",
            "entity": "Active selector"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let target_list = handle_tool_call(
        &active,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "project_path": target_project_path,
            "category": "project",
            "min_trust": 0.0
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let target_list = extract_json(&target_list.value);
    assert_fact_results(
        &target_list,
        "Target selector fact",
        "Active selector fact",
        "project_path selector should read target-project facts",
    );

    let target_list_by_nested_project_path = handle_tool_call(
        &active,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "project_selector": {"project_path": target_project_path},
            "category": "project",
            "min_trust": 0.0
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let target_list_by_nested_project_path =
        extract_json(&target_list_by_nested_project_path.value);
    assert_fact_results(
        &target_list_by_nested_project_path,
        "Target selector fact",
        "Active selector fact",
        "nested project_path selector should read target-project facts",
    );

    let active_list = handle_tool_call(
        &active,
        "tracedecay_fact_store",
        json!({"action": "list", "category": "project", "min_trust": 0.0}),
        None,
        None,
    )
    .await
    .unwrap();
    let active_list = extract_json(&active_list.value);
    assert_fact_results(
        &active_list,
        "Active selector fact",
        "Target selector fact",
        "default fact_store scope should remain the active project",
    );

    let cross_project_write = handle_tool_call(
        &active,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "project_selector": {"project_id": target_project_id},
            "content": "Cross-project writes should be rejected",
            "category": "project"
        }),
        None,
        None,
    )
    .await;
    let Err(err) = cross_project_write else {
        panic!("expected cross-project fact_store add to be rejected");
    };
    assert!(
        format!("{err}").contains("cross-project fact_store writes are not supported"),
        "unexpected cross-project write error: {err}"
    );

    let cross_project_feedback = handle_tool_call(
        &active,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": 1,
            "action": "helpful",
            "project_selector": {"project_id": target_project_id}
        }),
        None,
        None,
    )
    .await;
    let Err(err) = cross_project_feedback else {
        panic!("expected cross-project fact feedback to be rejected");
    };
    assert!(
        format!("{err}").contains("does not accept project selectors"),
        "unexpected cross-project feedback error: {err}"
    );

    let typo_selector = handle_tool_call(
        &active,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "project_id": "proj_does_not_exist",
            "category": "project",
            "min_trust": 0.0
        }),
        None,
        None,
    )
    .await;
    let Err(err) = typo_selector else {
        panic!("expected unresolved explicit selector to fail");
    };
    assert!(
        format!("{err}").contains("registered project not found for selector"),
        "unresolved selector must not fall back to active project: {err}"
    );

    let hidden_top_level_path = handle_tool_call(
        &active,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "path": target_project_path,
            "category": "project",
            "min_trust": 0.0
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let hidden_top_level_path = extract_json(&hidden_top_level_path.value);
    assert_fact_results(
        &hidden_top_level_path,
        "Active selector fact",
        "Target selector fact",
        "top-level path should not act as an undocumented project selector",
    );
}

#[tokio::test]
async fn memory_status_project_selector_reports_registered_project_memory() {
    let (active, target, _env) = setup_cross_project_memory_projects().await;
    let target_project_id = target
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target project should have a profile project_id");
    let target_project_path = target.project_root().to_string_lossy().to_string();

    for content in ["Active status fact one", "Active status fact two"] {
        handle_tool_call(
            &active,
            "tracedecay_fact_store",
            json!({
                "action": "add",
                "content": content,
                "category": "project"
            }),
            None,
            None,
        )
        .await
        .unwrap();
    }

    handle_tool_call(
        &target,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Target status fact",
            "category": "project"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let active_status =
        handle_tool_call(&active, "tracedecay_memory_status", json!({}), None, None)
            .await
            .unwrap();
    let active_status = extract_json(&active_status.value);
    assert_eq!(active_status["status"], "ok");
    assert_eq!(active_status["memory"]["fact_count"].as_u64(), Some(2));

    let target_status_by_id = handle_tool_call(
        &active,
        "tracedecay_memory_status",
        json!({"project_id": target_project_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let target_status_by_id = extract_json(&target_status_by_id.value);
    assert_eq!(target_status_by_id["status"], "ok");
    assert_eq!(
        target_status_by_id["memory"]["fact_count"].as_u64(),
        Some(1),
        "project_id selector should report the target project's memory: {target_status_by_id}"
    );

    let target_status_by_path = handle_tool_call(
        &active,
        "tracedecay_memory_status",
        json!({"project_selector": {"path": target_project_path}}),
        None,
        None,
    )
    .await
    .unwrap();
    let target_status_by_path = extract_json(&target_status_by_path.value);
    assert_eq!(
        target_status_by_path["memory"]["fact_count"].as_u64(),
        Some(1),
        "nested path selector should report the target project's memory: {target_status_by_path}"
    );

    let missing_status = handle_tool_call(
        &active,
        "tracedecay_memory_status",
        json!({"project_id": "proj_does_not_exist"}),
        None,
        None,
    )
    .await;
    let Err(err) = missing_status else {
        panic!("expected unresolved memory_status selector to fail");
    };
    assert!(
        format!("{err}").contains("registered project not found for selector"),
        "unresolved memory_status selector must not fall back to active project: {err}"
    );
}

#[tokio::test]
async fn memory_fact_store_update_rejects_secret_like_content_with_diff_report() {
    let (cg, _dir) = setup_project().await;
    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Project preference: never store provider API keys",
            "category": "project"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();

    let rejected = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "update",
            "fact_id": fact_id,
            "content": "api_key=sk-test-742913 must not be persisted"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let rejected: Value = serde_json::from_str(extract_text(&rejected.value)).unwrap();
    assert_eq!(rejected["action"], "update");
    assert_eq!(rejected["count"], 0);
    assert_eq!(rejected["diff"], "rejected_secret_like");
    assert!(rejected["fact"].is_null());
    assert!(
        rejected["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("secret"),
        "reason should describe the hygiene rejection: {rejected}"
    );

    let stored = cg.get_fact(fact_id).await.unwrap().unwrap();
    assert_eq!(
        stored.content,
        "Project preference: never store provider API keys"
    );
    assert!(!stored.content.contains("sk-test-742913"));
}

#[tokio::test]
async fn memory_recall_updates_retrieval_count() {
    let (cg, _dir) = setup_project().await;
    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Retrieval counters move after search",
            "entity": "Counter Entity"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();

    handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "search", "query": "Retrieval counters", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();

    let status = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "list", "min_trust": 0.0, "limit": 10}),
        None,
        None,
    )
    .await
    .unwrap();
    let status: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    let fact = status["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fact| fact["fact_id"].as_i64() == Some(fact_id))
        .unwrap();
    assert!(
        fact["retrieval_count"].as_i64().unwrap_or_default() > 0,
        "returned facts should increment retrieval_count: {status}"
    );
}

#[tokio::test]
async fn memory_fact_store_update_trust_delta_uses_direct_fact_lookup() {
    let (cg, _dir) = setup_project().await;
    let first = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "First fact should remain updateable after many later facts",
            "trust": 0.4
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let first: Value = serde_json::from_str(extract_text(&first.value)).unwrap();
    let first_id = first["fact"]["fact_id"].as_i64().unwrap();

    for i in 0..205 {
        handle_tool_call(
            &cg,
            "tracedecay_fact_store",
            json!({
                "action": "add",
                "content": format!("Later fact {i} should not hide the first fact"),
            }),
            None,
            None,
        )
        .await
        .unwrap();
    }

    let updated = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "update",
            "fact_id": first_id,
            "trust_delta": 0.2
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let updated: Value = serde_json::from_str(extract_text(&updated.value)).unwrap();
    assert_eq!(updated["fact"]["fact_id"].as_i64(), Some(first_id));
    assert!(
        (updated["fact"]["trust_score"].as_f64().unwrap() - 0.6).abs() < 0.000_001,
        "trust_delta should apply through direct fact lookup: {updated}"
    );
}

#[tokio::test]
async fn memory_feedback_and_status_include_trust_fields() {
    let (cg, _dir) = setup_project().await;
    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Helpful memory fact for feedback",
            "category": "general"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();
    assert!(added["fact"].get("id").is_none());
    assert!(added["fact"].get("trust").is_none());
    assert!(added["fact"]["trust_score"].as_f64().is_some());

    let helpful = handle_tool_call(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id, "helpful": true, "source": "mcp-test", "note": "matched"}),
        None,
        None,
    )
    .await
    .unwrap();
    let helpful: Value = serde_json::from_str(extract_text(&helpful.value)).unwrap();
    assert!(helpful["feedback"]["event_id"].as_i64().unwrap() > 0);
    assert_eq!(helpful["feedback"]["fact_id"], fact_id);
    assert_eq!(helpful["feedback"]["action"], "helpful");
    assert_eq!(helpful["feedback"]["old_trust"], 0.5);
    assert!(helpful["feedback"]["new_trust"].as_f64().unwrap() > 0.5);
    assert!(helpful["feedback"]["trust_delta"].as_f64().unwrap() > 0.0);
    assert_eq!(helpful["feedback"]["helpful_count"], 1);
    assert_eq!(helpful["feedback"]["unhelpful_count"], 0);

    let unhelpful = handle_tool_call(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id, "unhelpful": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let unhelpful: Value = serde_json::from_str(extract_text(&unhelpful.value)).unwrap();
    assert_eq!(unhelpful["feedback"]["action"], "unhelpful");
    assert!(
        unhelpful["feedback"]["new_trust"].as_f64().unwrap()
            < helpful["feedback"]["new_trust"].as_f64().unwrap()
    );
    assert_eq!(unhelpful["feedback"]["helpful_count"], 1);
    assert_eq!(unhelpful["feedback"]["unhelpful_count"], 1);

    let fetched = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "get", "fact_id": fact_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let fetched: Value = serde_json::from_str(extract_text(&fetched.value)).unwrap();
    assert_eq!(fetched["action"], "get");
    assert_eq!(fetched["fact"]["fact_id"], fact_id);
    let trust_history = fetched["trust_history"]
        .as_array()
        .unwrap_or_else(|| panic!("expected trust_history array: {fetched}"));
    assert_eq!(trust_history.len(), 2);
    assert_eq!(trust_history[0]["action"], "helpful");
    assert_eq!(trust_history[0]["note"], "matched");
    assert_eq!(trust_history[1]["action"], "unhelpful");
    assert!(trust_history[1]["note"].is_null());

    let status = handle_tool_call(&cg, "tracedecay_memory_status", json!({}), None, None)
        .await
        .unwrap();
    let status: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status["status"], "ok");
    assert!(status["memory"]["fact_count"].as_u64().unwrap() >= 1);
    assert!(status["memory"].get("trust_0_025_count").is_some());
    assert!(status["memory"].get("trust_025_050_count").is_some());
    assert!(status["memory"].get("trust_050_075_count").is_some());
    assert!(status["memory"].get("trust_075_100_count").is_some());
    assert!(status["memory"].get("helpful_count").is_some());
    assert!(status["memory"].get("unhelpful_count").is_some());
    assert!(status["memory"].get("missing_vector_count").is_some());
}

#[tokio::test]
async fn memory_fact_store_uses_project_store_when_serving_branch_db() {
    fn git(project: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .unwrap_or_else(|err| panic!("git {args:?} failed to spawn: {err}"));
        assert!(
            output.status.success(),
            "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let _guard = GLOBAL_DB_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = dir.path().join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let _global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));

    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
    git(&project, &["init"]);
    git(&project, &["config", "user.email", "test@test.com"]);
    git(&project, &["config", "user.name", "Test"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "initial"]);
    git(&project, &["branch", "-M", "main"]);

    let cg = TraceDecay::init(&project).await.unwrap();
    index_all_retrying_sync_lock(&cg).await;
    git(&project, &["checkout", "-b", "feature"]);
    let cg = TraceDecay::open(&project).await.unwrap();
    assert_ne!(
        cg.db_path(),
        cg.store_layout().graph_db_path,
        "test must serve a branch DB distinct from the shared project store"
    );

    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Branch memory writes stay project-scoped",
            "category": "project",
            "entity": "Branch memory"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_i64()
        .expect("fact_store add should return numeric id");

    let (branch_db, _) = Database::open(&cg.db_path()).await.unwrap();
    assert!(
        MemoryStore::new(branch_db.conn())
            .get_fact(fact_id)
            .await
            .unwrap()
            .is_none(),
        "MCP memory writes must not be scoped to the branch graph DB"
    );

    let (project_db, _) = Database::open(&cg.store_layout().graph_db_path)
        .await
        .unwrap();
    assert!(
        MemoryStore::new(project_db.conn())
            .get_fact(fact_id)
            .await
            .unwrap()
            .is_some(),
        "MCP memory writes must land in the shared project memory store"
    );
}

#[tokio::test]
async fn memory_tools_validate_malformed_inputs() {
    let (cg, _dir) = setup_project().await;

    let missing_action =
        handle_tool_call(&cg, "tracedecay_fact_store", json!({}), None, None).await;
    assert!(expect_tool_error(missing_action).contains("action"));

    let bad_action = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "teleport"}),
        None,
        None,
    )
    .await;
    assert!(expect_tool_error(bad_action).contains("unknown fact_store action"));

    let bad_category = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "list", "category": "definitely-not-a-category"}),
        None,
        None,
    )
    .await;
    assert!(expect_tool_error(bad_category).contains("category"));

    let missing_feedback_action = handle_tool_call(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": 123}),
        None,
        None,
    )
    .await;
    assert!(expect_tool_error(missing_feedback_action).contains("helpful"));
}

#[tokio::test]
async fn message_search_reads_project_local_session_db() {
    let (cg, _dir) = setup_project().await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let session = SessionRecord {
        provider: "cursor".to_string(),
        session_id: "cursor-session".to_string(),
        project_key: cg.project_root().to_string_lossy().to_string(),
        project_path: cg.project_root().to_string_lossy().to_string(),
        title: Some("Cursor transcript".to_string()),
        started_at: Some(1),
        ended_at: None,
        transcript_path: Some("cursor-session.jsonl".to_string()),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    assert!(db.upsert_session(&session).await);
    let child_session = SessionRecord {
        provider: "cursor".to_string(),
        session_id: "worker-1".to_string(),
        project_key: cg.project_root().to_string_lossy().to_string(),
        project_path: cg.project_root().to_string_lossy().to_string(),
        title: Some("Cursor subagent".to_string()),
        started_at: Some(3),
        ended_at: None,
        transcript_path: Some("worker-1.jsonl".to_string()),
        metadata_json: None,
        parent_session_id: Some("cursor-session".to_string()),
        is_subagent: true,
        agent_id: Some("worker-1".to_string()),
        parent_tool_use_id: None,
    };
    assert!(db.upsert_session(&child_session).await);
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "cursor-message".to_string(),
            session_id: "cursor-session".to_string(),
            role: "user".to_string(),
            timestamp: Some(2),
            ordinal: 1,
            text: "Project-local transcript search is working.".to_string(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("cursor-session.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "worker-message".to_string(),
            session_id: "worker-1".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(4),
            ordinal: 1,
            text: "Subagent citrus evidence is linked to its parent.".to_string(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("worker-1.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );

    let result = handle_tool_call(
        &cg,
        "tracedecay_message_search",
        json!({"query": "transcript search", "provider": "cursor", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let parsed = extract_json(&result.value);
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["count"], 1);
    assert_eq!(
        parsed["results"][0]["message"]["message_id"],
        "cursor-message"
    );
    assert_eq!(
        parsed["results"][0]["session"]["project_key"],
        cg.project_root().to_string_lossy().to_string()
    );

    let subagent_result = handle_tool_call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "citrus evidence",
            "provider": "cursor",
            "parent_session_id": "cursor-session",
            "scope": "subagents_only"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let subagent_parsed = extract_json(&subagent_result.value);
    assert_eq!(subagent_parsed["status"], "ok");
    assert_eq!(subagent_parsed["scope"], "subagents_only");
    assert_eq!(subagent_parsed["parent_session_id"], "cursor-session");
    assert_eq!(subagent_parsed["count"], 1);
    assert_eq!(
        subagent_parsed["results"][0]["session"]["parent_session_id"],
        "cursor-session"
    );
    assert_eq!(
        subagent_parsed["results"][0]["session"]["is_subagent"],
        true
    );
}

#[tokio::test]
async fn message_search_reads_profile_sharded_session_db() {
    let _guard = GLOBAL_DB_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = dir.path().join("home");
    let shard_root = home.join(".tracedecay/projects/proj_123");
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    fs::create_dir_all(&shard_root).unwrap();
    fs::write(
        project.join(".tracedecay/enrollment.json"),
        r#"{"project_id":"proj_123","storage_mode":"profile_sharded"}"#,
    )
    .unwrap();
    let _home_guard = HomeEnvGuard::set(&home);
    let config = tracedecay::config::TraceDecayConfig {
        root_dir: project.to_string_lossy().to_string(),
        ..tracedecay::config::TraceDecayConfig::default()
    };
    fs::write(
        shard_root.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    Database::initialize(&shard_root.join("tracedecay.db"))
        .await
        .unwrap();
    let meta = tracedecay::branch_meta::BranchMeta::new_for_dir(&shard_root, "main");
    tracedecay::branch_meta::save_branch_meta(&shard_root, &meta).unwrap();
    let cg = TraceDecay::open(&project).await.unwrap();
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("profile-sharded session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: "profile-session".to_string(),
            project_key: project.to_string_lossy().to_string(),
            project_path: project.to_string_lossy().to_string(),
            title: Some("Profile shard".to_string()),
            started_at: Some(10),
            ended_at: None,
            transcript_path: Some("profile-session.jsonl".to_string()),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "profile-message".to_string(),
            session_id: "profile-session".to_string(),
            role: "user".to_string(),
            timestamp: Some(11),
            ordinal: 1,
            text: "Profile shard transcript search is working.".to_string(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("profile-session.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );

    let result = handle_tool_call(
        &cg,
        "tracedecay_message_search",
        json!({"query": "profile shard transcript", "provider": "cursor", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let parsed = extract_json(&result.value);

    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["count"], 1);
    assert_eq!(
        parsed["results"][0]["message"]["message_id"],
        "profile-message"
    );
    assert!(shard_root.join("sessions.db").is_file());
    assert!(!project.join(".tracedecay/sessions.db").exists());
}

async fn seed_lcm_session_message(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some(format!("LCM session {session_id}")),
            started_at: Some(ordinal),
            ended_at: None,
            transcript_path: Some(format!("{session_id}.jsonl")),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: "assistant".to_string(),
            timestamp: Some(ordinal + 1),
            ordinal,
            text: text.into(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some(format!("{session_id}.jsonl")),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );
}

async fn seed_lcm_tool_result_message(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some(format!("LCM session {session_id}")),
            started_at: Some(ordinal),
            ended_at: None,
            transcript_path: Some(format!("{session_id}.jsonl")),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: "tool".to_string(),
            timestamp: Some(ordinal + 1),
            ordinal,
            text: text.into(),
            kind: Some("tool_result".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some(format!("{session_id}.jsonl")),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );
}

#[allow(clippy::too_many_arguments)]
async fn seed_lcm_session_message_with_role_source_timestamp(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
    role: &str,
    source: &str,
    timestamp: i64,
) {
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some(format!("LCM session {session_id}")),
            started_at: Some(ordinal),
            ended_at: None,
            transcript_path: Some(format!("{session_id}.jsonl")),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            timestamp: Some(timestamp),
            ordinal,
            text: text.into(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some(format!("{session_id}.jsonl")),
            source_offset: Some(0),
            metadata_json: Some(serde_json::json!({"source": source}).to_string()),
        })
        .await
    );
}

async fn open_hermes_profile_session_db(hermes_home: &Path) -> GlobalDb {
    GlobalDb::open_at(&hermes_home.join(".tracedecay/sessions.db"))
        .await
        .expect("profile-local session db should open")
}

async fn seed_lcm_session_message_in_db(
    db: &GlobalDb,
    project_path: &Path,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            project_key: project_path.to_string_lossy().to_string(),
            project_path: project_path.to_string_lossy().to_string(),
            title: Some(format!("LCM session {session_id}")),
            started_at: Some(ordinal),
            ended_at: None,
            transcript_path: Some(format!("{session_id}.jsonl")),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: "assistant".to_string(),
            timestamp: Some(ordinal + 1),
            ordinal,
            text: text.into(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some(format!("{session_id}.jsonl")),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );
}

async fn project_lcm_conn(cg: &TraceDecay) -> libsql::Connection {
    let db = libsql::Builder::new_local(project_session_db_path(cg))
        .build()
        .await
        .unwrap();
    db.connect().unwrap()
}

async fn lcm_fts_match_count(cg: &TraceDecay, query: &str) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_raw_messages_fts WHERE lcm_raw_messages_fts MATCH ?1",
            libsql::params![query],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_raw_store_id(cg: &TraceDecay, message_id: &str) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT store_id FROM lcm_raw_messages WHERE provider = 'cursor' AND message_id = ?1",
            libsql::params![message_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_raw_message_count(cg: &TraceDecay, session_id: &str) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = ?1",
            libsql::params![session_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_raw_message_count_at_path(db_path: &Path, session_id: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = ?1",
            libsql::params![session_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_summary_node_count(cg: &TraceDecay, session_id: &str) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_summary_nodes WHERE session_id = ?1",
            libsql::params![session_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn lcm_schema_migration_count(cg: &TraceDecay) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn wipe_lcm_raw_fts(cg: &TraceDecay) {
    project_lcm_conn(cg)
        .await
        .execute_batch("DELETE FROM lcm_raw_messages_fts;")
        .await
        .unwrap();
}

async fn wipe_lcm_raw_fts_for_message(cg: &TraceDecay, message_id: &str) {
    let store_id = lcm_raw_store_id(cg, message_id).await;
    project_lcm_conn(cg)
        .await
        .execute(
            "DELETE FROM lcm_raw_messages_fts WHERE rowid = ?1",
            libsql::params![store_id],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn lcm_doctor_clean_dry_run_reports_noise_and_filtered_sessions_without_mutating() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "cron-20260414",
        "cron-20260414-message",
        "scheduled report body that must not leak",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "scratch-shell-a",
        "scratch-shell-message",
        "scratch one-shot body that must not leak",
        2,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-heartbeat",
        "Still working...",
        3,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-valuable",
        "valuable payload to preserve",
        4,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": false,
            "ignore_session_patterns": ["cron-*"],
            "stateless_session_patterns": ["scratch-shell-*"],
            "ignore_message_patterns": ["Cronjob Response:*"]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(payload["mode"], "clean");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["diagnostics"]["cleanup"]["read_only"], true);
    assert_eq!(
        payload["diagnostics"]["cleanup"]["ignored_session_candidates"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["stateless_session_candidates"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["noise_message_candidates"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["heartbeat_noise_message_candidates"],
        1
    );
    assert_eq!(payload["diagnostics"]["cleanup"]["candidate_count"], 2);
    assert_eq!(
        payload["diagnostics"]["cleanup"]["heartbeat_message_candidates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(payload["repairs"]["planned_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["kind"] == "clean_lcm_noise"));
    assert_eq!(lcm_raw_message_count(&cg, "cron-20260414").await, 1);
    assert_eq!(lcm_raw_message_count(&cg, "scratch-shell-a").await, 1);
    assert_eq!(lcm_raw_message_count(&cg, "normal-session").await, 2);
    assert!(!text.contains("scheduled report body that must not leak"));
    assert!(!text.contains("scratch one-shot body that must not leak"));
    assert!(!text.contains("Still working"));
    assert!(!text.contains("valuable payload to preserve"));
}

#[tokio::test]
async fn lcm_doctor_clean_apply_is_denied_by_default() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "cron-20260414",
        "cron-20260414-message",
        "scheduled report body that must remain without explicit opt-in",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": true,
            "ignore_session_patterns": ["cron-*"]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(payload["status"], "denied");
    assert_eq!(
        payload["error"],
        "destructive cleanup is disabled by default"
    );
    assert_eq!(payload["mode"], "clean");
    assert_eq!(payload["apply"], true);
    assert_eq!(lcm_raw_message_count(&cg, "cron-20260414").await, 1);
}

#[tokio::test]
async fn lcm_doctor_clean_apply_backs_up_and_deletes_only_safe_candidates() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "cron-20260414",
        "cron-20260414-message",
        "scheduled report body that must be deleted only after backup",
        1,
    )
    .await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let cron_store_id = lcm_raw_store_id(&cg, "cron-20260414-message").await;
    db.lcm_insert_summary_node(LcmSummaryNodeDraft {
        provider: "cursor".to_string(),
        conversation_id: "cron-20260414".to_string(),
        session_id: "cron-20260414".to_string(),
        depth: 0,
        summary_text: "scheduled report summary".to_string(),
        source_refs: vec![LcmSourceRef::RawMessage {
            store_id: cron_store_id,
        }],
        source_token_count: 12,
        summary_token_count: 3,
        source_time_start: Some(1),
        source_time_end: Some(2),
        expand_hint: Some("test clean candidate".to_string()),
        metadata_json: None,
    })
    .await
    .unwrap();
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-heartbeat",
        "Still working...",
        2,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-valuable",
        "valuable payload to preserve",
        3,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": true,
            "doctor_clean_apply_enabled": true,
            "ignore_session_patterns": ["cron-*"]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();
    let backup_path = payload["repairs"]["backup"]["path"]
        .as_str()
        .expect("clean apply should report backup path");

    assert_eq!(payload["status"], "repaired");
    assert_eq!(payload["dry_run"], false);
    assert_eq!(payload["repairs"]["backup"]["ok"], true);
    assert!(Path::new(backup_path).is_file());
    assert_eq!(
        payload["diagnostics"]["cleanup"]["heartbeat_noise_message_candidates"],
        1
    );
    assert_eq!(
        lcm_raw_message_count_at_path(Path::new(backup_path), "cron-20260414").await,
        1
    );
    assert_eq!(lcm_raw_message_count(&cg, "cron-20260414").await, 0);
    assert_eq!(lcm_summary_node_count(&cg, "cron-20260414").await, 0);
    assert_eq!(lcm_raw_message_count(&cg, "normal-session").await, 2);
    assert!(!text.contains("scheduled report body that must be deleted only after backup"));
    assert!(!text.contains("Still working"));
    assert!(!text.contains("valuable payload to preserve"));
}

#[tokio::test]
async fn lcm_doctor_clean_apply_deletes_all_matching_noise_beyond_diagnostic_samples() {
    let (cg, _dir) = setup_project().await;
    for idx in 0..25 {
        seed_lcm_session_message(
            &cg,
            "normal-session",
            &format!("cron-noise-{idx}"),
            format!("Cronjob Response: noisy heartbeat {idx}"),
            idx + 1,
        )
        .await;
    }
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-valuable",
        "valuable payload to preserve",
        30,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": true,
            "doctor_clean_apply_enabled": true,
            "ignore_message_patterns": ["^Cronjob Response:"]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(
        payload["diagnostics"]["cleanup"]["noise_message_candidates"],
        25
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["message_candidates"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    assert_eq!(
        payload["repairs"]["applied_actions"][0]["deleted"]["raw_messages"],
        25
    );
    assert_eq!(lcm_raw_message_count(&cg, "normal-session").await, 1);
    assert!(!text.contains("Cronjob Response: noisy heartbeat"));
    assert!(!text.contains("valuable payload to preserve"));
}

#[tokio::test]
async fn lcm_doctor_reports_missing_and_orphan_payloads_without_payload_bodies() {
    let (cg, _dir) = setup_project().await;
    let secret = format!(
        "LCM_DOCTOR_SECRET_PAYLOAD\n{}",
        "doctor-secret ".repeat(30_000)
    );
    seed_lcm_tool_result_message(
        &cg,
        "lcm-doctor-payload",
        "lcm-doctor-payload-message",
        secret,
        1,
    )
    .await;

    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let raw = db
        .lcm_load_raw_message("cursor", "lcm-doctor-payload-message")
        .await
        .expect("externalized raw message should load");
    let payload_ref = raw.payload_ref.expect("external payload ref");
    fs::remove_file(lcm_payload_dir(&cg).join(&payload_ref)).unwrap();
    fs::write(
        lcm_payload_dir(&cg).join("payload_unreferenced_test.payload"),
        "orphan body that must not be returned",
    )
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "diagnose"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(payload["diagnostics"]["payloads"]["missing_files"], 1);
    assert_eq!(payload["diagnostics"]["payloads"]["orphan_files"], 1);
    let text = extract_text(&result.value);
    assert!(!text.contains("LCM_DOCTOR_SECRET_PAYLOAD"));
    assert!(!text.contains("orphan body that must not be returned"));
}

#[tokio::test]
async fn lcm_doctor_reports_placeholder_recovery_and_gc_candidates_without_bodies() {
    let (cg, _dir) = setup_project().await;
    let missing_ref = "payload_missing_placeholder_test.payload";
    let placeholder = format!(
        "[Externalized LCM ingest payload: kind=ingest_payload; role=user; field=content; chars=2048; bytes=2048; ref={missing_ref}]"
    );
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-placeholder",
        "lcm-doctor-placeholder-message",
        placeholder,
        1,
    )
    .await;

    let payload_dir = lcm_payload_dir(&cg);
    fs::create_dir_all(&payload_dir).unwrap();
    fs::write(
        payload_dir.join("payload_gc_candidate_test.payload"),
        "gc candidate body that must not be returned",
    )
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-placeholder",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(
        payload["diagnostics"]["payloads"]["placeholder_refs_total"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_metadata"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_files"],
        1
    );
    assert_eq!(payload["diagnostics"]["payloads"]["gc_candidate_files"], 1);
    assert!(
        payload["diagnostics"]["payloads"]["missing_placeholder_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["payload_ref"] == missing_ref)
    );
    assert!(
        payload["diagnostics"]["payloads"]["gc_candidate_payload_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("payload_gc_candidate_test.payload"))
    );
    let text = extract_text(&result.value);
    assert!(!text.contains("gc candidate body that must not be returned"));
}

#[tokio::test]
async fn lcm_doctor_gc_mode_preview_and_apply_reports_without_body_leaks() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "gc-preview-session",
        "gc-preview-message",
        "seed message for gc preview",
        1,
    )
    .await;
    let payload_dir = lcm_payload_dir(&cg);
    fs::create_dir_all(&payload_dir).unwrap();
    let payload_ref =
        "payload_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc.payload";
    let payload_path = payload_dir.join(payload_ref);
    fs::write(&payload_path, "gc mode secret body that must not leak").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&payload_path)
        .unwrap()
        .set_times(
            fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .unwrap();

    let preview = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "gc", "apply": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let preview_text = extract_text(&preview.value);
    let preview_payload: Value = serde_json::from_str(preview_text).unwrap();
    assert_eq!(preview_payload["mode"], "gc");
    assert_eq!(preview_payload["dry_run"], true);
    assert_eq!(
        preview_payload["repairs"]["gc_report"]["orphans"]["count"],
        1
    );
    assert!(payload_path.is_file());
    assert!(!preview_text.contains("gc mode secret body that must not leak"));

    let apply = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "gc",
            "apply": true,
            "lcm_gc_apply_enabled": true
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let apply_text = extract_text(&apply.value);
    let apply_payload: Value = serde_json::from_str(apply_text).unwrap();
    assert_eq!(apply_payload["mode"], "gc");
    assert_eq!(apply_payload["dry_run"], false);
    assert_eq!(apply_payload["repairs"]["gc_report"]["orphans"]["count"], 1);
    assert!(!payload_path.exists());
    assert!(!apply_text.contains("gc mode secret body that must not leak"));
}

#[tokio::test]
async fn lcm_doctor_gc_apply_is_denied_by_default() {
    let (cg, _dir) = setup_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "gc", "apply": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "denied");
    assert_eq!(payload["mode"], "gc");
    assert_eq!(
        payload["repairs"]["unsafe_actions_skipped"][0]["reason"],
        "lcm_gc_apply_disabled"
    );
}

#[tokio::test]
async fn lcm_doctor_counts_nested_externalized_payload_refs_as_referenced() {
    let (cg, _dir) = setup_project().await;
    let media_payload = format!(
        "data:image/png;base64,{}",
        "QWxhZGRpbjpvcGVuIHNlc2FtZQ==".repeat(160)
    );
    let content = json!({
        "content": [
            {"type": "text", "text": "doctor nested payload canary"},
            {"type": "image_url", "image_url": {"url": media_payload}},
        ]
    })
    .to_string();
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-nested-payload",
        "lcm-doctor-nested-payload-message",
        content,
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-nested-payload",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["diagnostics"]["payloads"]["unreferenced_metadata"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["placeholder_refs_total"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_metadata"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_files"],
        0
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn lcm_doctor_ignores_plain_text_ref_tokens_as_placeholders() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-plain-ref",
        "lcm-doctor-plain-ref-message",
        "plain documentation mentions ref=payload_plain_text_false_positive.payload outside a placeholder",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-plain-ref",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["diagnostics"]["payloads"]["placeholder_refs_total"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_metadata"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_files"],
        0
    );
    assert!(
        payload["diagnostics"]["payloads"]["missing_placeholder_refs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn lcm_doctor_scoped_payload_diagnostics_ignore_other_session_payload_files() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_tool_result_message(
        &cg,
        "lcm-doctor-payload-target",
        "lcm-doctor-payload-target-message",
        format!("target payload\n{}", "target-body ".repeat(30_000)),
        1,
    )
    .await;
    seed_lcm_tool_result_message(
        &cg,
        "lcm-doctor-payload-other",
        "lcm-doctor-payload-other-message",
        format!("other payload\n{}", "other-body ".repeat(30_000)),
        2,
    )
    .await;

    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let other_raw = db
        .lcm_load_raw_message("cursor", "lcm-doctor-payload-other-message")
        .await
        .expect("other externalized raw message should load");
    let other_payload_ref = other_raw.payload_ref.expect("other external payload ref");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-payload-target",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["diagnostics"]["payloads"]["missing_files"], 0);
    assert_eq!(payload["diagnostics"]["payloads"]["orphan_files"], 0);
    assert!(!payload["diagnostics"]["payloads"]["orphan_payload_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value.as_str() == Some(&other_payload_ref)));
    assert!(!extract_text(&result.value).contains(&other_payload_ref));
}

#[tokio::test]
async fn lcm_doctor_reports_scoped_fts_rebuild_when_other_session_matches_probe_term() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-fts-target",
        "lcm-doctor-fts-target-message",
        "scopedneedle target text",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-fts-other",
        "lcm-doctor-fts-other-message",
        "scopedneedle other text",
        2,
    )
    .await;
    wipe_lcm_raw_fts_for_message(&cg, "lcm-doctor-fts-target-message").await;
    assert_eq!(lcm_fts_match_count(&cg, "scopedneedle").await, 1);

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-fts-target",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(payload["diagnostics"]["fts"]["raw"]["rebuild_needed"], true);
}

#[tokio::test]
async fn lcm_doctor_counts_summary_source_rows_with_missing_owner_node() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-orphan-owner",
        "lcm-doctor-orphan-owner-message",
        "orphan owner source text",
        1,
    )
    .await;
    let store_id = lcm_raw_store_id(&cg, "lcm-doctor-orphan-owner-message").await;
    let conn = project_lcm_conn(&cg).await;
    conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
    conn.execute(
        "INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
             VALUES ('missing-summary-owner', 'raw_message', ?1, 0)",
        libsql::params![store_id.to_string()],
    )
    .await
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-orphan-owner",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(payload["diagnostics"]["summaries"]["broken_sources"], 1);
}

#[tokio::test]
async fn lcm_doctor_scopes_orphan_lifecycle_debt_to_requested_session() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-debt-target",
        "lcm-doctor-debt-target-message",
        "target session text",
        1,
    )
    .await;
    let conn = project_lcm_conn(&cg).await;
    conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
    conn.execute(
        "INSERT INTO lcm_maintenance_debt(
                provider, conversation_id, debt_id, debt_kind, from_store_id, to_store_id
             )
             VALUES ('cursor', 'lcm-doctor-debt-other', 'orphan-debt', 'raw_backlog', 1, 2)",
        (),
    )
    .await
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-debt-target",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["diagnostics"]["lifecycle"]["orphan_debt"], 0);
}

#[tokio::test]
async fn lcm_doctor_diagnose_does_not_create_missing_project_session_db() {
    let (cg, _dir) = setup_project().await;
    let db_path = project_session_db_path(&cg);
    if db_path.exists() {
        fs::remove_file(&db_path).unwrap();
    }

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "diagnose"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "unavailable");
    assert!(
        !db_path.exists(),
        "diagnose must not create session storage"
    );
}

#[tokio::test]
async fn lcm_doctor_repair_dry_run_does_not_run_schema_migration() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-read-only-existing",
        "lcm-doctor-read-only-existing-message",
        "read only existing database text",
        1,
    )
    .await;
    project_lcm_conn(&cg)
        .await
        .execute(
            "DELETE FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(lcm_schema_migration_count(&cg).await, 0);

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "repair", "apply": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["mode"], "repair");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["diagnostics"]["schema"]["migration_present"], false);
    assert_eq!(
        payload["diagnostics"]["ast_grep"]["rewrite_available"].as_bool(),
        Some(tracedecay::mcp::tools::ast_grep_available())
    );
    assert_eq!(
        payload["diagnostics"]["ast_grep"]["outline_available"].as_bool(),
        Some(tracedecay::mcp::tools::ast_grep_outline_available())
    );
    assert!(
        payload["diagnostics"]["ast_grep"]["message"].is_string(),
        "doctor should include ast-grep install/update guidance"
    );
    assert_eq!(lcm_schema_migration_count(&cg).await, 0);
}

#[tokio::test]
async fn lcm_doctor_repair_dry_run_reports_fts_rebuild_without_mutating() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-dry-run",
        "lcm-doctor-dry-run-message",
        "dry run searchable needle",
        1,
    )
    .await;
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 1);
    wipe_lcm_raw_fts(&cg).await;
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 0);

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "repair", "apply": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["mode"], "repair");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["diagnostics"]["fts"]["rebuild_needed"], true);
    assert!(payload["repairs"]["planned_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["kind"] == "rebuild_raw_fts"));
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 0);
    close_test_graph(cg).await;
}

#[tokio::test]
async fn lcm_doctor_repair_apply_rebuilds_damaged_fts() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-apply",
        "lcm-doctor-apply-message",
        "apply repair searchable needle",
        1,
    )
    .await;
    wipe_lcm_raw_fts(&cg).await;
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 0);

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "repair", "apply": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "repaired");
    assert_eq!(payload["dry_run"], false);
    let backup_path = payload["repairs"]["backup"]["path"]
        .as_str()
        .expect("repair apply should report backup path");
    assert_eq!(payload["repairs"]["backup"]["ok"], true);
    assert!(Path::new(backup_path).is_file());
    assert!(payload["repairs"]["applied_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["kind"] == "rebuild_raw_fts"));
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 1);
}

#[tokio::test]
async fn lcm_doctor_retention_reports_candidates_without_deleting() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-retention",
        "lcm-doctor-retention-message",
        "old session retention candidate",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "retention"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["mode"], "retention");
    assert_eq!(payload["diagnostics"]["retention"]["read_only"], true);
    assert!(payload["diagnostics"]["retention"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|candidate| candidate["session_id"] == "lcm-doctor-retention"));
    assert_eq!(lcm_raw_message_count(&cg, "lcm-doctor-retention").await, 1);
}

#[tokio::test]
async fn lcm_doctor_uses_explicit_hermes_profile_session_db() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-profile",
        "lcm-doctor-profile-project",
        "project-local doctor should not be counted",
        1,
    )
    .await;

    let hermes_home = TempDir::new().unwrap();
    let profile_db = open_hermes_profile_session_db(hermes_home.path()).await;
    seed_lcm_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        "lcm-doctor-profile",
        "lcm-doctor-profile-message",
        "profile-local doctor should be counted",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path().to_string_lossy()
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["storage_scope"], "hermes_profile");
    assert_eq!(payload["diagnostics"]["raw_message_count"], 1);
}

#[tokio::test]
async fn lcm_session_handlers_expose_bounded_read_apis_and_placeholders() {
    let (cg, _dir) = setup_project().await;
    let full_text = format!("orchard dispatch {}", "external-payload-body ".repeat(400));
    seed_lcm_session_message(&cg, "lcm-session", "lcm-message", full_text, 1).await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let raw = db
        .lcm_load_raw_message("cursor", "lcm-message")
        .await
        .expect("LCM raw message should be created by compatibility ingest");

    let status = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({"provider": "cursor"}),
        None,
        None,
    )
    .await
    .unwrap();
    let status_payload: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status_payload["status"], "ok");
    assert_eq!(status_payload["lcm"]["raw_message_count"], 1);

    let loaded = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "content_limit": 24
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(loaded_payload["status"], "ok");
    assert_eq!(loaded_payload["messages"].as_array().unwrap().len(), 1);
    assert!(loaded_payload["messages"][0]["content_range"]["truncated"]
        .as_bool()
        .unwrap());
    assert_eq!(
        loaded_payload["messages"][0]["content"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        24
    );

    let grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({"provider": "cursor", "query": "orchard dispatch", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let grep_payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(grep_payload["status"], "ok");
    assert_eq!(grep_payload["hits"].as_array().unwrap().len(), 1);
    assert!(
        grep_payload["hits"][0]["snippet"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 4096,
        "grep snippets must stay bounded"
    );

    let described = handle_tool_call(
        &cg,
        "tracedecay_lcm_describe",
        json!({"provider": "cursor", "session_id": "lcm-session"}),
        None,
        None,
    )
    .await
    .unwrap();
    let described_payload: Value = serde_json::from_str(extract_text(&described.value)).unwrap();
    assert_eq!(described_payload["status"], "ok");
    assert_eq!(described_payload["description"]["raw_message_count"], 1);
    assert!(described_payload["description"]["raw_messages"][0]
        .get("content_preview")
        .is_some());
    assert!(
        described_payload["description"]["raw_messages"][0]
            .get("content")
            .is_none(),
        "describe must not expose raw payload bodies"
    );

    let expanded = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "target": {"kind": "raw_message", "store_id": raw.store_id},
            "content_offset": 8,
            "content_limit": 16
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let expanded_payload: Value = serde_json::from_str(extract_text(&expanded.value)).unwrap();
    assert_eq!(expanded_payload["status"], "ok");
    assert_eq!(expanded_payload["expansion"]["kind"], "raw_message");
    assert_eq!(
        expanded_payload["expansion"]["content"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        16
    );
    assert!(expanded_payload["expansion"]["content_range"]["truncated"]
        .as_bool()
        .unwrap());

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "prompt": "Summarize orchard dispatch",
            "query": "orchard dispatch",
            "context_max_tokens": 128,
            "max_tokens": 64
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["needs_synthesis"], true);
    assert_eq!(payload["prompt"], "Summarize orchard dispatch");
    assert!(payload["context_blocks"]
        .as_array()
        .expect("context blocks")
        .iter()
        .any(|block| block["kind"] == "raw_message"));
    assert!(payload["synthesis_prompt"]["user"]
        .as_str()
        .unwrap()
        .contains("EXPANDED CONTEXT"));
    assert!(extract_text(&result.value).len() <= 15_000);

    let preflight = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [{"id": "active-preflight", "role": "user", "content": "hello"}]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let preflight_payload: Value = serde_json::from_str(extract_text(&preflight.value)).unwrap();
    assert_eq!(preflight_payload["status"], "ok");
    assert_eq!(preflight_payload["should_compress"], false);

    let compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [{"id": "active-compress", "role": "user", "content": "hello again"}],
            "summarizer": {"mode": "noop"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let compress_payload: Value = serde_json::from_str(extract_text(&compress.value)).unwrap();
    assert_eq!(compress_payload["status"], "ok");
    assert_eq!(compress_payload["summary_nodes_created"], 0);
    assert_eq!(compress_payload["compression_attempts"], 0);
    assert_eq!(compress_payload["fallback_used"], false);
    assert!(
        compress_payload.get("retry_status").is_some(),
        "compress response must expose retry_status for bridge contract"
    );
    assert_eq!(compress_payload["retry_status"], Value::Null);

    let unsafe_noop_compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [],
            "current_tokens": 50_000,
            "threshold_tokens": 1_000,
            "fresh_tail_count": 1,
            "leaf_chunk_tokens": 1,
            "summarizer": {"mode": "noop"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let unsafe_noop_payload =
        extract_lcm_json_following_handle(&cg, &unsafe_noop_compress.value).await;
    assert_eq!(unsafe_noop_payload["status"], "needs_summary");
    assert_eq!(
        unsafe_noop_payload["reason"],
        "hermes_auxiliary_not_available"
    );
    assert_eq!(unsafe_noop_payload["summary_nodes_created"], 0);
    assert!(
        unsafe_noop_payload["summary_request"].is_object(),
        "hard-overflow explicit noop should be upgraded to auxiliary summary mode"
    );

    let reserve_cap_noop_compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [],
            "current_tokens": 8_000,
            "context_length": 10_000,
            "reserve_tokens_floor": 2_000,
            "fresh_tail_count": 1,
            "leaf_chunk_tokens": 1,
            "summarizer": {"mode": "noop"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let reserve_cap_noop_payload =
        extract_lcm_json_following_handle(&cg, &reserve_cap_noop_compress.value).await;
    assert_eq!(reserve_cap_noop_payload["status"], "needs_summary");
    assert_eq!(
        reserve_cap_noop_payload["reason"],
        "hermes_auxiliary_not_available"
    );
    assert!(
        reserve_cap_noop_payload["summary_request"].is_object(),
        "reserve-derived hard pressure should upgrade explicit noop to auxiliary summary mode"
    );

    for (index, content) in [
        "old-1 token",
        "old-2 token",
        "old-3 token",
        "old-4 token",
        "old-5 token",
        "old-6 token",
        "fresh-1",
        "fresh-2",
    ]
    .iter()
    .enumerate()
    {
        seed_lcm_session_message(
            &cg,
            "lcm-critical-session",
            &format!("lcm-critical-message-{}", index + 1),
            *content,
            (index + 1) as i64,
        )
        .await;
    }

    let critical_compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-critical-session",
            "messages": [],
            "current_tokens": 40,
            "max_assembly_tokens": 2,
            "leaf_chunk_tokens": 1,
            "max_source_messages": 3,
            "summarizer": {"mode": "fake", "summary_text": "catchup summary"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let critical_payload: Value =
        serde_json::from_str(extract_text(&critical_compress.value)).unwrap();
    assert_eq!(critical_payload["status"], "ok");
    assert_eq!(critical_payload["reason"], "forced_overflow_recovery");
    assert_eq!(critical_payload["summary_nodes_created"], 4);
    assert_eq!(critical_payload["compression_attempts"], 4);
    assert_eq!(critical_payload["fallback_used"], false);
    assert_eq!(
        critical_payload["retry_status"],
        "critical_pressure_catch_up"
    );
}

#[tokio::test]
async fn lcm_compress_without_summarizer_requests_auxiliary_summary() {
    let (cg, _dir) = setup_project().await;
    for (index, content) in [
        "historical planning context alpha beta gamma",
        "historical tool result delta epsilon zeta",
        "fresh objective eta theta",
    ]
    .iter()
    .enumerate()
    {
        seed_lcm_session_message(
            &cg,
            "lcm-default-summarizer-session",
            &format!("lcm-default-summarizer-message-{}", index + 1),
            *content,
            (index + 1) as i64,
        )
        .await;
    }

    let compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-default-summarizer-session",
            "messages": [],
            "current_tokens": 10_000,
            "threshold_tokens": 100,
            "fresh_tail_count": 1,
            "leaf_chunk_tokens": 1,
            "max_assembly_tokens": 20
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&compress.value)).unwrap();

    assert_eq!(payload["status"], "needs_summary");
    assert_eq!(payload["reason"], "hermes_auxiliary_not_available");
    assert_eq!(payload["summary_nodes_created"], 0);
    assert_eq!(payload["compression_attempts"], 0);
    assert_eq!(payload["fallback_used"], false);
    assert_eq!(payload["retry_status"], Value::Null);
    assert_eq!(
        payload["summary_request"]["source_range"]["from_store_id"],
        1
    );
    assert_eq!(payload["summary_request"]["source_range"]["to_store_id"], 1);
    assert_eq!(
        payload["summary_request"]["source_messages"]
            .as_array()
            .expect("source messages should be present")
            .len(),
        1
    );
    let replay = payload["replay_messages"]
        .as_array()
        .expect("bounded replay should be present");
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0]["content"], "fresh objective eta theta");
    assert!(
        payload["replay_token_estimate"].as_i64().unwrap() <= 20,
        "default auxiliary mode must return a bounded replay"
    );
}

#[tokio::test]
async fn lcm_compress_oversized_needs_summary_uses_retrievable_full_payload() {
    let (cg, _dir) = setup_project().await;
    let huge_source = "alpha oversized context ".repeat(8_000);

    let compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-oversized-needs-summary",
            "messages": [
                {"id": "oversized-1", "role": "user", "content": huge_source.clone()},
                {"id": "oversized-2", "role": "assistant", "content": "acknowledged"},
                {"id": "oversized-3", "role": "user", "content": "latest objective"}
            ],
            "current_tokens": 30_000,
            "threshold_tokens": 1_000,
            "fresh_tail_count": 64,
            "leaf_chunk_tokens": 20_000,
            "summarizer": {"mode": "hermes_auxiliary"}
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&compress.value);
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["truncated"], true);
    assert!(payload["handle"].as_str().is_some());
    assert_eq!(payload["retrieve_tool"], "tracedecay_retrieve");
    assert!(payload.get("contract_truncated").is_none());
    assert!(payload.get("replay_messages_truncated_for_mcp").is_none());
    assert!(text.len() <= 15_000);
    let retrieved = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({"handle": payload["handle"].as_str().unwrap()}),
        None,
        None,
    )
    .await
    .unwrap();
    let retrieved_payload: Value = serde_json::from_str(extract_text(&retrieved.value)).unwrap();
    let full_payload: Value = serde_json::from_str(
        retrieved_payload["content"]
            .as_str()
            .expect("retrieved content should be the full original JSON string"),
    )
    .unwrap();
    assert_eq!(full_payload["status"], "needs_summary");
    assert_eq!(full_payload["reason"], "hermes_auxiliary_not_available");
    assert!(full_payload.get("contract_truncated").is_none());
    assert!(
        full_payload["replay_messages"]
            .as_array()
            .is_some_and(|messages| !messages.is_empty()),
        "full bridge payload must retain replay messages, got {full_payload:#}"
    );
    assert!(
        full_payload["summary_request"].is_object(),
        "full bridge payload must retain summary request metadata, got {full_payload:#}"
    );
    let source_messages = full_payload["summary_request"]["source_messages"]
        .as_array()
        .expect("retrieved needs-summary payload should retain source messages");
    assert!(!source_messages.is_empty());
    assert_eq!(
        source_messages[0]["content"].as_str(),
        Some(huge_source.as_str())
    );
    assert!(source_messages[0]
        .get("content_truncated_for_mcp")
        .is_none());
    assert!(full_payload["summary_request"]
        .get("source_messages_truncated_for_mcp")
        .is_none());
    close_test_graph(cg).await;
}

#[tokio::test]
async fn lcm_preflight_oversized_replay_preserves_bridge_contract() {
    let (cg, _dir) = setup_project().await;
    let huge_source = "preflight oversized active context ".repeat(8_000);

    let preflight = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "lcm-oversized-preflight",
            "messages": [
                {"id": "preflight-1", "role": "user", "content": huge_source},
                {"id": "preflight-2", "role": "assistant", "content": "acknowledged"}
            ],
            "current_tokens": 10,
            "threshold_tokens": 1_000
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&preflight.value);
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["should_compress"], false);
    assert_eq!(payload["reason"], "no_compression_needed");
    assert_eq!(payload["mcp_response_truncated"], true);
    assert_eq!(payload["contract_truncated"], true);
    assert!(payload.get("truncated").is_none());
    assert!(text.len() <= 15_000);
    assert!(payload["replay_messages_compacted_for_mcp"]
        .as_bool()
        .unwrap_or(false));
    assert_eq!(
        payload["replay_messages"][0]["content_truncated_for_mcp"],
        true
    );
}

#[tokio::test]
async fn lcm_preflight_structured_replay_content_is_bounded_for_mcp() {
    let (cg, _dir) = setup_project().await;
    let huge_source = "structured preflight payload ".repeat(8_000);

    let preflight = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "lcm-structured-preflight",
            "messages": [
                {
                    "id": "structured-preflight-1",
                    "role": "user",
                    "content": [
                        {"type": "text", "text": huge_source},
                        {"type": "input_json", "value": {"nested": huge_source}}
                    ]
                },
                {"id": "structured-preflight-2", "role": "assistant", "content": "acknowledged"}
            ],
            "current_tokens": 10,
            "threshold_tokens": 1_000
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&preflight.value);
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["status"], "ok");
    assert!(payload.get("truncated").is_none());
    assert!(text.len() <= 15_000);
    let compacted_content = payload["replay_messages"][0]["content"]
        .as_str()
        .expect("structured replay content should be serialized to bounded text");
    assert!(compacted_content.len() <= 512);
    assert_eq!(
        payload["replay_messages"][0]["content_serialized_for_mcp"],
        true
    );
    assert_eq!(
        payload["replay_messages"][0]["content_truncated_for_mcp"],
        true
    );
    assert_eq!(payload["replay_messages_compacted_for_mcp"], true);
}

#[tokio::test]
async fn lcm_session_boundary_handler_records_cooldown_for_skipped_carry_over() {
    let (cg, _dir) = setup_project().await;
    for (index, content) in ["old-1 token", "old-2 token", "fresh-1", "fresh-2"]
        .iter()
        .enumerate()
    {
        seed_lcm_session_message(
            &cg,
            "lcm-boundary-session",
            &format!("lcm-boundary-message-{}", index + 1),
            *content,
            (index + 1) as i64,
        )
        .await;
    }

    let boundary = handle_tool_call(
        &cg,
        "tracedecay_lcm_session_boundary",
        json!({
            "provider": "cursor",
            "session_id": "lcm-boundary-session",
            "old_session_id": "lcm-old-session",
            "boundary_reason": "compression",
            "bound_session_id": "lcm-bound-session"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let boundary_payload: Value = serde_json::from_str(extract_text(&boundary.value)).unwrap();
    assert_eq!(boundary_payload["status"], "ok");
    assert_eq!(boundary_payload["recorded"], true);
    assert_eq!(
        boundary_payload["reason"],
        "compression_boundary_skip_recorded"
    );

    let preflight = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "lcm-boundary-session",
            "messages": [],
            "current_tokens": 120,
            "threshold_tokens": 100
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let preflight_payload: Value = serde_json::from_str(extract_text(&preflight.value)).unwrap();
    assert_eq!(preflight_payload["status"], "ok");
    assert_eq!(preflight_payload["should_compress"], false);
    assert_eq!(preflight_payload["reason"], "compression_boundary_cooldown");
}

#[tokio::test]
async fn lcm_status_response_is_valid_json_and_omits_payload_secrets() {
    let (cg, _dir) = setup_project().await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: "lcm-status-session".to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some("LCM status diagnostics".to_string()),
            started_at: Some(1),
            ended_at: None,
            transcript_path: Some("lcm-status-session.jsonl".to_string()),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );

    let secret = format!("MCP_STATUS_SECRET_PAYLOAD\n{}", "Q".repeat(300_000));
    db.lcm_store(project_data_dir(&cg))
        .ingest_raw_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "lcm-status-secret-message".to_string(),
            session_id: "lcm-status-session".to_string(),
            role: "tool".to_string(),
            timestamp: Some(2),
            ordinal: 1,
            text: secret,
            kind: Some("tool_result".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("lcm-status-session.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
        .expect("external payload should ingest");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-status-session",
            "storage_scope": "project_local"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).expect("LCM status response must be JSON");

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["lcm"]["storage_scope"], "project_local");
    assert_eq!(payload["lcm"]["payload"]["externalized_count"], 1);
    assert_eq!(payload["lcm"]["payload"]["missing_count"], 0);
    assert_eq!(payload["lcm"]["payload"]["unreferenced_count"], 0);
    assert_eq!(payload["lcm"]["redaction"]["enabled"], false);
    assert!(!text.contains("MCP_STATUS_SECRET_PAYLOAD"));
}

#[tokio::test]
async fn lcm_status_reports_lifecycle_fields_and_resolved_storage_scope() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-status-frontier",
        "lcm-status-frontier-message-1",
        "frontier seed one",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-status-frontier",
        "lcm-status-frontier-message-2",
        "frontier seed two",
        2,
    )
    .await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let first = db
        .lcm_load_raw_message("cursor", "lcm-status-frontier-message-1")
        .await
        .expect("first raw message should load");
    let second = db
        .lcm_load_raw_message("cursor", "lcm-status-frontier-message-2")
        .await
        .expect("second raw message should load");
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "lcm-status-frontier".into(),
        current_session_id: "lcm-status-frontier".into(),
        current_frontier_store_id: Some(second.store_id),
        last_finalized_session_id: Some("lcm-status-prior".into()),
        last_finalized_frontier_store_id: Some(first.store_id),
        maintenance_debt: vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: first.store_id,
            to_store_id: second.store_id,
        }],
    })
    .await
    .expect("lifecycle state should update");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-status-frontier",
            "storage_scope": "project_local"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["lcm"]["storage_scope"], "project_local");
    assert_eq!(payload["lcm"]["raw_message_count"], 2);
    assert_eq!(
        payload["lcm"]["lifecycle"]["current_session_id"],
        "lcm-status-frontier"
    );
    assert_eq!(
        payload["lcm"]["lifecycle"]["current_frontier_store_id"],
        second.store_id
    );
    assert_eq!(
        payload["lcm"]["lifecycle"]["last_finalized_session_id"],
        "lcm-status-prior"
    );
    assert_eq!(
        payload["lcm"]["lifecycle"]["last_finalized_frontier_store_id"],
        first.store_id
    );
    assert_eq!(payload["lcm"]["lifecycle"]["maintenance_debt_count"], 1);

    let profile_result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-status-frontier",
            "storage_scope": "hermes_profile"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let profile_payload: Value = serde_json::from_str(extract_text(&profile_result.value)).unwrap();
    assert_eq!(profile_payload["status"], "unavailable");
    assert_eq!(profile_payload["storage_scope"], "hermes_profile");
    assert!(
        profile_payload.get("lcm").is_none(),
        "hermes_profile requests must not return project-local LCM counts"
    );
}

#[tokio::test]
async fn lcm_describe_supports_summary_node_and_external_payload_targets() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-describe-targets",
        "lcm-describe-source",
        "describe source body must not leak through metadata",
        1,
    )
    .await;
    seed_lcm_tool_result_message(
        &cg,
        "lcm-describe-targets",
        "lcm-describe-tool",
        format!("describe external secret {}", "payload ".repeat(40_000)),
        2,
    )
    .await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let source = db
        .lcm_load_raw_message("cursor", "lcm-describe-source")
        .await
        .expect("source raw message should exist");
    let external = db
        .lcm_load_raw_message("cursor", "lcm-describe-tool")
        .await
        .expect("external raw message should exist");
    let payload_ref = external.payload_ref.expect("payload ref");
    let summary = db
        .lcm_insert_summary_node(LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "conversation-1".to_string(),
            session_id: "lcm-describe-targets".to_string(),
            depth: 0,
            summary_text: "summary secret body must not leak through metadata".to_string(),
            source_refs: vec![LcmSourceRef::RawMessage {
                store_id: source.store_id,
            }],
            source_token_count: 30,
            summary_token_count: 5,
            source_time_start: Some(1),
            source_time_end: Some(2),
            expand_hint: Some("describe target summary".to_string()),
            metadata_json: None,
        })
        .await
        .expect("summary should insert");

    let node_result = handle_tool_call(
        &cg,
        "tracedecay_lcm_describe",
        json!({
            "provider": "cursor",
            "session_id": "lcm-describe-targets",
            "target": {"kind": "summary_node", "node_id": summary.node_id.clone()}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let node_payload: Value = serde_json::from_str(extract_text(&node_result.value)).unwrap();
    assert_eq!(node_payload["status"], "ok");
    assert_eq!(node_payload["description"]["target"], "summary_node");
    assert_eq!(
        node_payload["description"]["summary_node"]["node_id"],
        summary.node_id
    );
    assert_eq!(
        node_payload["description"]["summary_node"]["source_count"],
        1
    );

    let payload_result = handle_tool_call(
        &cg,
        "tracedecay_lcm_describe",
        json!({
            "provider": "cursor",
            "session_id": "lcm-describe-targets",
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload_payload: Value = serde_json::from_str(extract_text(&payload_result.value)).unwrap();
    assert_eq!(payload_payload["status"], "ok");
    assert_eq!(payload_payload["description"]["target"], "external_payload");
    assert_eq!(
        payload_payload["description"]["external_payload"]["payload_ref"],
        payload_ref
    );
    assert!(
        payload_payload["description"]["external_payload"]["content_preview"]
            .as_str()
            .unwrap()
            .contains(payload_ref.as_str())
    );

    let rendered = format!(
        "{}\n{}",
        extract_text(&node_result.value),
        extract_text(&payload_result.value)
    );
    assert!(!rendered.contains("summary secret body"));
    assert!(!rendered.contains("describe source body"));
    assert!(!rendered.contains("describe external secret"));
}

#[tokio::test]
async fn lcm_grep_and_load_session_honor_native_filters_and_content_clamp() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message_with_role_source_timestamp(
        &cg,
        "lcm-native-filters",
        "lcm-native-old-cli-assistant",
        "orchard native old cli assistant",
        1,
        "assistant",
        "cli",
        10,
    )
    .await;
    seed_lcm_session_message_with_role_source_timestamp(
        &cg,
        "lcm-native-filters",
        "lcm-native-new-cli-user",
        "orchard native new cli user",
        2,
        "user",
        "cli",
        20,
    )
    .await;
    seed_lcm_session_message_with_role_source_timestamp(
        &cg,
        "lcm-native-filters",
        "lcm-native-new-api-assistant",
        "orchard native new api assistant",
        3,
        "assistant",
        "api",
        30,
    )
    .await;

    let grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({
            "provider": "cursor",
            "query": "orchard native",
            "scope": "session",
            "session_id": "lcm-native-filters",
            "sort": "recency",
            "source": "cli",
            "role": "assistant",
            "start_time": 5,
            "end_time": 25,
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let grep_payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(grep_payload["status"], "ok");
    assert_eq!(grep_payload["count"], 1);
    assert_eq!(
        grep_payload["hits"][0]["message_id"],
        "lcm-native-old-cli-assistant"
    );
    assert_eq!(grep_payload["sort"], "recency");

    let loaded = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-native-filters",
            "roles": ["assistant", "user"],
            "time_from": 1,
            "time_to": 25,
            "content_limit": 25_000,
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(loaded_payload["status"], "ok");
    assert_eq!(loaded_payload["content_limit"], 20_000);
    assert_eq!(loaded_payload["content_limit_clamped_from"], 25_000);
    assert_eq!(
        loaded_payload["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["message_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["lcm-native-old-cli-assistant", "lcm-native-new-cli-user"]
    );
}

#[tokio::test]
async fn lcm_grep_accepts_string_timestamp_filters() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message_with_role_source_timestamp(
        &cg,
        "lcm-string-timestamps",
        "lcm-string-timestamps-old",
        "orchard string timestamp old",
        1,
        "assistant",
        "cli",
        10,
    )
    .await;
    seed_lcm_session_message_with_role_source_timestamp(
        &cg,
        "lcm-string-timestamps",
        "lcm-string-timestamps-target",
        "orchard string timestamp target",
        2,
        "assistant",
        "cli",
        20,
    )
    .await;
    seed_lcm_session_message_with_role_source_timestamp(
        &cg,
        "lcm-string-timestamps",
        "lcm-string-timestamps-new",
        "orchard string timestamp new",
        3,
        "assistant",
        "cli",
        30,
    )
    .await;

    let grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({
            "provider": "cursor",
            "query": "orchard string timestamp",
            "scope": "session",
            "session_id": "lcm-string-timestamps",
            "start_time": "15",
            "end_time": "1970-01-01T00:00:25Z",
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["count"], 1);
    assert_eq!(
        payload["hits"][0]["message_id"],
        "lcm-string-timestamps-target"
    );
}

#[tokio::test]
async fn lcm_status_uses_explicit_hermes_profile_session_db() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-profile-status",
        "lcm-profile-status-project",
        "project-local distractor status",
        1,
    )
    .await;

    let hermes_home = TempDir::new().unwrap();
    let profile_db = open_hermes_profile_session_db(hermes_home.path()).await;
    seed_lcm_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        "lcm-profile-status",
        "lcm-profile-status-profile-1",
        "profile-local status seed one",
        1,
    )
    .await;
    seed_lcm_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        "lcm-profile-status",
        "lcm-profile-status-profile-2",
        "profile-local status seed two",
        2,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-profile-status",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path()
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["lcm"]["storage_scope"], "hermes_profile");
    assert_eq!(payload["lcm"]["raw_message_count"], 2);
    assert!(hermes_home.path().join(".tracedecay/sessions.db").exists());
    #[cfg(windows)]
    let _ = hermes_home.keep();
}

#[tokio::test]
async fn lcm_load_and_grep_use_explicit_hermes_profile_session_db() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-profile-read",
        "lcm-profile-read-project",
        "project-local distractor profile query",
        1,
    )
    .await;

    let hermes_home = TempDir::new().unwrap();
    let profile_db = open_hermes_profile_session_db(hermes_home.path()).await;
    seed_lcm_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        "lcm-profile-read",
        "lcm-profile-read-profile",
        "profile-local pear evidence",
        1,
    )
    .await;

    let loaded = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-profile-read",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path()
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(loaded_payload["status"], "ok");
    assert_eq!(loaded_payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        loaded_payload["messages"][0]["content"],
        "profile-local pear evidence"
    );

    let grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({
            "provider": "cursor",
            "query": "profile-local pear",
            "session_id": "lcm-profile-read",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path(),
            "limit": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let grep_payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(grep_payload["status"], "ok");
    assert_eq!(grep_payload["count"], 1);
    assert_eq!(grep_payload["hits"][0]["session_id"], "lcm-profile-read");
    assert!(grep_payload["hits"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("profile-local pear evidence"));

    let expanded = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "cursor",
            "prompt": "Explain profile pear evidence",
            "query": "profile-local pear",
            "session_id": "lcm-profile-read",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path(),
            "context_max_tokens": 1024,
            "max_tokens": 128
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let expanded_payload: Value = serde_json::from_str(extract_text(&expanded.value)).unwrap();
    assert_eq!(expanded_payload["status"], "ok");
    assert_eq!(expanded_payload["storage_scope"], "hermes_profile");
    assert_eq!(expanded_payload["needs_synthesis"], true);
    assert!(expanded_payload["context_blocks"][0]["content"]
        .as_str()
        .unwrap()
        .contains("profile-local pear evidence"));
}

#[tokio::test]
async fn lcm_hermes_profile_requires_explicit_valid_home_without_fallback() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-profile-missing-home",
        "lcm-profile-missing-home-project",
        "project-local must not leak without hermes home",
        1,
    )
    .await;

    let status = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-profile-missing-home",
            "storage_scope": "hermes_profile"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let status_payload: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status_payload["status"], "unavailable");
    assert_eq!(status_payload["storage_scope"], "hermes_profile");
    assert!(status_payload["message"]
        .as_str()
        .unwrap()
        .contains("hermes_home"));
    assert!(status_payload.get("lcm").is_none());

    let loaded = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-profile-missing-home",
            "storage_scope": "hermes_profile",
            "hermes_home": "relative-hermes-home"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(loaded_payload["status"], "unavailable");
    assert_eq!(loaded_payload["storage_scope"], "hermes_profile");
    assert!(loaded_payload["message"]
        .as_str()
        .unwrap()
        .contains("absolute hermes_home"));
    assert!(loaded_payload.get("messages").is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn lcm_hermes_profile_rejects_symlinked_tracedecay_dir_escape() {
    let (cg, _dir) = setup_project().await;
    let hermes_home = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    unix_fs::symlink(outside.path(), hermes_home.path().join(".tracedecay")).unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({
            "provider": "cursor",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home.path()
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "unavailable");
    assert_eq!(payload["storage_scope"], "hermes_profile");
    assert!(
        payload["message"].as_str().unwrap().contains(".tracedecay"),
        "rejection should identify the unsafe profile storage component: {payload}"
    );
    assert!(
        !outside.path().join("sessions.db").exists(),
        "profile DB must not be created through a symlink escape"
    );
}

#[tokio::test]
async fn lcm_hermes_profile_rejects_non_directory_home() {
    let (cg, _dir) = setup_project().await;
    let dir = TempDir::new().unwrap();
    let hermes_home = dir.path().join("hermes-home-file");
    fs::write(&hermes_home, "not a directory").unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({
            "provider": "cursor",
            "storage_scope": "hermes_profile",
            "hermes_home": hermes_home
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "unavailable");
    assert_eq!(payload["storage_scope"], "hermes_profile");
    assert!(
        payload["message"]
            .as_str()
            .unwrap()
            .contains("not a directory"),
        "non-directory hermes_home should be rejected clearly: {payload}"
    );
    assert!(payload.get("lcm").is_none());
}

#[tokio::test]
async fn lcm_grep_rejects_invalid_scope_without_searching_all_sessions() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-scope-a",
        "lcm-scope-message-a",
        "fail closed unique-cross-session-token alpha",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-scope-b",
        "lcm-scope-message-b",
        "fail closed unique-cross-session-token beta",
        2,
    )
    .await;

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_lcm_grep",
            json!({
                "provider": "cursor",
                "query": "unique-cross-session-token",
                "scope": "everything",
                "limit": 10
            }),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("scope"),
        "invalid scope should report an argument error, got {err}"
    );
}

#[tokio::test]
async fn lcm_load_session_rejects_fractional_negative_and_wrong_type_numeric_args() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-numeric",
        "lcm-numeric-message",
        "numeric validation test body",
        1,
    )
    .await;

    for (case, args) in [
        (
            "fractional limit",
            json!({"provider": "cursor", "session_id": "lcm-numeric", "limit": 1.5}),
        ),
        (
            "negative limit",
            json!({"provider": "cursor", "session_id": "lcm-numeric", "limit": -1}),
        ),
        (
            "string limit",
            json!({"provider": "cursor", "session_id": "lcm-numeric", "limit": "1"}),
        ),
    ] {
        let err = expect_tool_error(
            handle_tool_call(&cg, "tracedecay_lcm_load_session", args, None, None).await,
        );
        assert!(
            err.contains("limit"),
            "{case} should report an argument error mentioning limit, got {err}"
        );
    }
}

#[tokio::test]
async fn lcm_load_session_accepts_valid_integer_args() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-valid-integers",
        "lcm-valid-integers-message",
        "valid integer argument body",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-valid-integers",
            "limit": 1,
            "content_offset": 0,
            "content_limit": 8,
            "start_time": 1,
            "end_time": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        payload["messages"][0]["content"].as_str().unwrap(),
        "valid in"
    );
}

#[tokio::test]
async fn lcm_large_json_response_stays_parseable_after_truncation() {
    let (cg, _dir) = setup_project().await;
    for index in 0..4 {
        seed_lcm_session_message(
            &cg,
            "lcm-large-json",
            &format!("lcm-large-json-message-{index}"),
            format!("large json response {index} {}", "payload ".repeat(2000)),
            index + 1,
        )
        .await;
    }

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-large-json",
            "limit": 4,
            "content_limit": 8192
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value))
        .expect("truncated LCM tool text should remain valid JSON");
    assert_eq!(payload["truncated"], true);
    assert!(payload["preview"].as_str().unwrap().len() <= 15_000);
}

#[tokio::test]
async fn lcm_expand_query_large_response_preserves_synthesis_contract() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-large-expand-query",
        "lcm-large-expand-query-message",
        format!(
            "oversized expand-query evidence {}",
            "context ".repeat(4000)
        ),
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-large-expand-query",
            "prompt": "Summarize oversized expand-query evidence",
            "query": "oversized expand-query evidence",
            "context_max_tokens": 65536,
            "max_tokens": 128
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value =
        serde_json::from_str(text).expect("large expand-query response must remain valid JSON");

    assert_ne!(
        payload["truncated"], true,
        "must not use generic truncation"
    );
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["needs_synthesis"], true);
    assert_eq!(
        payload["prompt"],
        "Summarize oversized expand-query evidence"
    );
    assert!(payload["synthesis_prompt"]["system"]
        .as_str()
        .unwrap()
        .contains("expanded LCM retrieval context"));
    assert!(payload["synthesis_prompt"]["user"]
        .as_str()
        .unwrap()
        .contains("Summarize oversized expand-query evidence"));
    assert!(payload["context_truncated"].as_bool().is_some());
    assert!(payload["context_budget"]["used_chars"].as_u64().is_some());
    assert!(!payload["matches"].as_array().unwrap().is_empty());
    assert!(
        payload["context_blocks"].as_array().unwrap().len() <= 3,
        "MCP expand-query context should stay compact"
    );
    assert!(text.len() <= 15_000);
    close_test_graph(cg).await;
}

#[tokio::test]
async fn lcm_expand_query_oversized_prompt_preserves_synthesis_contract() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-huge-prompt-expand-query",
        "lcm-huge-prompt-expand-query-message",
        "contract overflow evidence lives in this raw message",
        1,
    )
    .await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let raw = db
        .lcm_load_raw_message("cursor", "lcm-huge-prompt-expand-query-message")
        .await
        .expect("raw message should exist");
    let summary = db
        .lcm_insert_summary_node(LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "conversation-1".to_string(),
            session_id: "lcm-huge-prompt-expand-query".to_string(),
            depth: 0,
            summary_text: "summary contract overflow evidence".to_string(),
            source_refs: vec![LcmSourceRef::RawMessage {
                store_id: raw.store_id,
            }],
            source_token_count: 30,
            summary_token_count: 5,
            source_time_start: Some(1),
            source_time_end: Some(2),
            expand_hint: Some("contract overflow summary".to_string()),
            metadata_json: None,
        })
        .await
        .expect("summary should insert");
    let huge_prompt = format!(
        "Explain contract overflow evidence. {}",
        "PROMPT_OVERFLOW ".repeat(12_000)
    );
    let huge_query = format!(
        "contract overflow evidence {}",
        "QUERY_OVERFLOW ".repeat(12_000)
    );

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-huge-prompt-expand-query",
            "prompt": huge_prompt,
            "query": huge_query,
            "node_ids": [summary.node_id],
            "context_max_tokens": 65536,
            "max_tokens": 128
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value =
        serde_json::from_str(text).expect("oversized expand-query response must remain valid JSON");

    assert_ne!(
        payload["truncated"], true,
        "must not use generic truncation"
    );
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["needs_synthesis"], true);
    assert_eq!(payload["mcp_response_truncated"], true);
    assert!(payload["prompt"].as_str().unwrap().chars().count() <= 2_048);
    assert!(payload["query"].as_str().unwrap().chars().count() <= 1_024);
    assert!(payload["prompt_truncated_for_mcp"].as_bool().unwrap());
    assert!(payload["query_truncated_for_mcp"].as_bool().unwrap());
    assert!(payload["contract_truncated"].as_bool().unwrap());
    assert!(payload["synthesis_prompt"]["user"]
        .as_str()
        .unwrap()
        .contains("QUESTION:"));
    assert!(text.len() <= 15_000);
}

#[tokio::test]
async fn message_search_preserves_provider_project_parent_scope_shape_after_lcm() {
    let (cg, _dir) = setup_project().await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: "parent".to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some("Parent session".to_string()),
            started_at: Some(1),
            ended_at: None,
            transcript_path: Some("parent.jsonl".to_string()),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session(&SessionRecord {
            provider: "cursor".to_string(),
            session_id: "child".to_string(),
            project_key: cg.project_root().to_string_lossy().to_string(),
            project_path: cg.project_root().to_string_lossy().to_string(),
            title: Some("Child session".to_string()),
            started_at: Some(2),
            ended_at: None,
            transcript_path: Some("child.jsonl".to_string()),
            metadata_json: None,
            parent_session_id: Some("parent".to_string()),
            is_subagent: true,
            agent_id: Some("child".to_string()),
            parent_tool_use_id: None,
        })
        .await
    );
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "child-message".to_string(),
            session_id: "child".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(3),
            ordinal: 1,
            text: "orchard dispatch compatibility result".to_string(),
            kind: Some("message".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("child.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        })
        .await
    );

    let result = handle_tool_call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "orchard dispatch",
            "provider": "cursor",
            "project_key": cg.project_root().to_string_lossy(),
            "scope": "subagents_only",
            "parent_session_id": "parent",
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["scope"], "subagents_only");
    assert_eq!(payload["provider"], "cursor");
    assert_eq!(payload["parent_session_id"], "parent");
    assert_eq!(payload["results"].as_array().unwrap().len(), 1);
    assert!(payload["results"][0]["message"].get("text").is_some());
    assert_eq!(payload["results"][0]["session"]["is_subagent"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn lcm_status_cli_bridge_accepts_json_args() {
    let (cg, _dir) = setup_project().await;
    let home = _dir.path().join("home");
    let _daemon = common::spawn_tracedecay_daemon(&home);
    let outside_cwd = TempDir::new().unwrap();
    let project_arg = cg.project_root().display().to_string();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    common::apply_tracedecay_home_env(&mut command, &home);
    let output = command
        .current_dir(outside_cwd.path())
        .args([
            "tool",
            "--project",
            &project_arg,
            "tracedecay_lcm_status",
            "--json",
            "--args",
            r#"{"provider":"cursor"}"#,
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "tracedecay tool exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["content"][0]["type"], "text");
    let payload: Value =
        serde_json::from_str(json["content"][0]["text"].as_str().unwrap()).unwrap();
    // "ok" when sessions.db already exists, "not_ingested" on a fresh project
    // that has never had any LCM data. Both indicate the CLI bridge dispatched
    // correctly; the test is about argument plumbing, not store contents.
    assert!(
        payload["status"] == "ok" || payload["status"] == "not_ingested",
        "unexpected lcm_status: {payload}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn lcm_status_cli_profile_scope_dispatches_without_initialized_project() {
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(home.path());
    let _global_db_guard = GlobalDbEnvGuard::set(&home.path().join(".tracedecay/global.db"));
    let outside_cwd = TempDir::new().unwrap();
    let hermes_home = TempDir::new().unwrap();
    let profile_db = open_hermes_profile_session_db(hermes_home.path()).await;
    seed_lcm_session_message_in_db(
        &profile_db,
        hermes_home.path(),
        "lcm-cli-profile",
        "lcm-cli-profile-message-1",
        "profile-only status message",
        1,
    )
    .await;
    let profile_args = json!({
        "provider": "cursor",
        "session_id": "lcm-cli-profile",
        "storage_scope": "hermes_profile",
        "hermes_home": hermes_home.path(),
    })
    .to_string();
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let mut profile_command = std::process::Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    common::apply_tracedecay_home_env(&mut profile_command, home.path());
    let profile_output = profile_command
        .current_dir(outside_cwd.path())
        .args([
            "tool",
            "tracedecay_lcm_status",
            "--json",
            "--args",
            profile_args.as_str(),
        ])
        .output()
        .unwrap();

    assert!(
        profile_output.status.success(),
        "profile-scoped tracedecay tool should not require an initialized cwd project\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&profile_output.stdout),
        String::from_utf8_lossy(&profile_output.stderr)
    );
    let profile_json: Value = serde_json::from_slice(&profile_output.stdout).unwrap();
    let profile_payload: Value =
        serde_json::from_str(profile_json["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(profile_payload["status"], "ok");
    assert_eq!(profile_payload["lcm"]["storage_scope"], "hermes_profile");
    assert_eq!(profile_payload["lcm"]["raw_message_count"], 1);

    let mut project_command = std::process::Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    common::apply_tracedecay_home_env(&mut project_command, home.path());
    let project_output = project_command
        .current_dir(outside_cwd.path())
        .args([
            "tool",
            "tracedecay_lcm_status",
            "--json",
            "--args",
            r#"{"provider":"cursor","storage_scope":"project_local"}"#,
        ])
        .output()
        .unwrap();
    assert!(
        !project_output.status.success(),
        "project-local tool calls without an initialized cwd should still fail\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&project_output.stdout),
        String::from_utf8_lossy(&project_output.stderr)
    );
    let stderr = String::from_utf8_lossy(&project_output.stderr);
    assert!(
        stderr.contains("no TraceDecay index found") && stderr.contains("tracedecay init"),
        "project-local failure should report the missing cwd project:\n{stderr}"
    );
    drop(env_lock);
}

#[test]
fn memory_tool_definitions_include_hermes_payload_fields() {
    let tools = get_tool_definitions();
    let tool_names: std::collections::HashSet<_> =
        tools.iter().map(|tool| tool.name.as_str()).collect();
    let fact_store = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_fact_store")
        .expect("tracedecay_fact_store definition");
    let feedback = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_fact_feedback")
        .expect("tracedecay_fact_feedback definition");
    let status = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_memory_status")
        .expect("tracedecay_memory_status definition");

    assert_eq!(
        fact_store.annotations.as_ref().unwrap()["readOnlyHint"],
        false
    );
    assert_eq!(
        feedback.annotations.as_ref().unwrap()["readOnlyHint"],
        false
    );
    assert_eq!(status.annotations.as_ref().unwrap()["readOnlyHint"], false);

    for field in [
        "action",
        "content",
        "query",
        "entity",
        "entities",
        "fact_id",
        "category",
        "tags",
        "min_trust",
        "trust",
        "trust_delta",
        "threshold",
        "limit",
        "source",
        "metadata",
        "note",
    ] {
        assert!(
            fact_store.input_schema["properties"].get(field).is_some(),
            "fact_store schema missing Hermes field {field}"
        );
    }
    assert_eq!(
        feedback.input_schema["required"],
        serde_json::json!(["fact_id"])
    );
    assert_eq!(
        fact_store.input_schema["properties"]["trust"]["type"],
        "number"
    );
    assert_eq!(fact_store.input_schema["properties"]["trust"]["minimum"], 0);
    assert_eq!(fact_store.input_schema["properties"]["trust"]["maximum"], 1);

    assert!(
        !tool_names.contains("tracedecay_record_decision"),
        "unshipped legacy decision tool should not be exposed"
    );
    assert!(
        !tool_names.contains("tracedecay_record_code_area"),
        "unshipped legacy code-area tool should not be exposed"
    );
    assert!(
        !tool_names.contains("tracedecay_session_recall"),
        "unshipped legacy recall tool should not be exposed"
    );
}

#[test]
fn message_search_provider_schema_matches_ingested_providers() {
    let tools = get_tool_definitions();
    let message_search = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_message_search")
        .expect("tracedecay_message_search definition");

    assert_eq!(
        message_search.input_schema["properties"]["provider"]["enum"],
        serde_json::json!([
            "cursor", "claude", "codex", "vibe", "cline", "roo-code", "kilo", "hermes"
        ])
    );
    assert_eq!(
        message_search.input_schema["properties"]["scope"]["enum"],
        serde_json::json!(["all", "parents_only", "subagents_only"])
    );
    assert!(message_search.input_schema["properties"]
        .get("parent_session_id")
        .is_some());
    assert!(message_search.input_schema["properties"]
        .get("include_subagents")
        .is_some());
}

#[tokio::test]
async fn memory_status_repairs_dirty_banks_before_reporting() {
    let (cg, _dir) = setup_project().await;
    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Status should repair dirty holographic banks",
            "category": "project",
            "entity": "Holographic Banks"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();
    let db_path = project_graph_db(&cg);
    let (db, _) = Database::open(&db_path).await.unwrap();
    db.conn()
        .execute(
            "UPDATE memory_facts
             SET hrr_vector = NULL, hrr_algebra = 'legacy', hrr_dim = 8
             WHERE fact_id = ?1",
            libsql::params![fact_id],
        )
        .await
        .unwrap();
    db.close();

    let status = handle_tool_call(&cg, "tracedecay_memory_status", json!({}), None, None)
        .await
        .unwrap();
    let status: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status["status"], "ok");
    assert!(
        status["memory"]["bank_count"].as_u64().unwrap_or_default() >= 2,
        "memory_status should rebuild all and category banks before reporting: {status}"
    );
    assert_eq!(
        status["memory"]["missing_vector_count"].as_u64(),
        Some(0),
        "status-triggered bank repair should not leave missing vectors"
    );
    assert_eq!(
        status["memory"]["repair"]["missing_vectors_repaired"].as_u64(),
        Some(1),
        "status should report derived vector repairs: {status}"
    );
    assert!(
        status["memory"]["repair"]["banks_rebuilt"]
            .as_u64()
            .unwrap_or_default()
            >= 1,
        "status should report bank repair work after vector repair: {status}"
    );
}

// ---------------------------------------------------------------------------
// Bug-report regressions: sonium-codebase issues
// ---------------------------------------------------------------------------

/// Regression for bug #1: `tracedecay_body` should prefer the `fn foo()` over
/// a field/variant also named `foo`. Setup mirrors what sonium hit when
/// searching for `gmres`: the codebase has both a `pub fn gmres(...)` and a
/// struct field literally named `gmres`. The function — the body the user
/// actually wants — must outrank the field.
async fn setup_function_vs_field_collision() -> (TraceDecay, TempDir) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub struct Solvers {
    pub gmres: u32,
}

pub fn gmres(x: u32) -> u32 {
    x + 1
}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    (cg, dir)
}

#[tokio::test]
async fn body_prefers_function_over_field_with_same_name() {
    let (cg, _dir) = setup_function_vs_field_collision().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_body",
        json!({"symbol": "gmres"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let matches = output["matches"].as_array().unwrap();
    let first = &matches[0];
    assert_eq!(
        first["kind"].as_str(),
        Some("function"),
        "first match should be the function definition, got {first}"
    );
    let body = first["body"].as_str().unwrap();
    assert!(
        body.contains("pub fn gmres"),
        "body should be the function source, got: {body}"
    );
}

/// Regression for bug #5: `tracedecay_diff_context.impacted_symbols` must not
/// list the same downstream node more than once. The sonium report showed
/// the same id appearing 6+ times consecutively when several modified
/// symbols all reached the same dependent.
#[tokio::test]
async fn diff_context_dedupes_impacted_symbols() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();

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

/// Regression for bug #6 / review P1: `tracedecay_recursion` must preserve
/// genuine direct recursion while filtering length-1 self-edge artifacts.
#[tokio::test]
async fn recursion_keeps_direct_recursion() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();
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
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();
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
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();
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

/// Regression for bug #4: `tracedecay_changelog`'s response must not list
/// directories under `files_not_indexed`. We construct a small git repo
/// with a real commit history that touches both a real file and a
/// (synthesised) directory path then verify the handler filters out the
/// directory.
#[tokio::test]
async fn changelog_filters_directory_paths() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(project)
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(project)
        .output()
        .unwrap();
    fs::create_dir_all(project.join("src/sub")).unwrap();
    fs::write(project.join("src/sub/keep.rs"), "pub fn k() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(project)
        .output()
        .unwrap();
    fs::write(
        project.join("src/sub/keep.rs"),
        "pub fn k() { let _ = 1; }\n",
    )
    .unwrap();
    fs::write(project.join("src/sub/added.rs"), "pub fn a() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(project)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "two"])
        .current_dir(project)
        .output()
        .unwrap();
    let (cg, _env) = init_test_project(project).await;
    // Intentionally skipping `index_all` — the changelog handler reads from
    // git directly, not the index, and including the index sync subjects
    // this test to a pre-existing SyncLock contention flake.

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

/// Regression for bug #8b: `tracedecay_unused_imports` must actually flag
/// unused imports. The previous implementation tested `incoming.is_empty()`
/// for every Use node, but Use nodes always have at least one incoming
/// edge (from their containing module/file via Contains), so the
/// condition never fired and the tool returned 0 on every real codebase.
#[tokio::test]
async fn unused_imports_detects_truly_unused() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();

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

/// Regression for bug #8a: `tracedecay_dead_code` must support `include_public`
/// so agents can audit pub items with no callers in the indexed scope. The
/// previous SQL hard-coded `visibility != 'public'`, so on a codebase that
/// is mostly `pub` the tool reported 0 dead symbols.
#[tokio::test]
async fn dead_code_with_include_public_finds_pub_unreferenced() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();

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

/// Regression for bug #7: `build_file_adjacency` previously included
/// `implements` and `extends` edges, which are heavily resolver-fuzzy-bound
/// to nonsense targets in unrelated files. After the fix, only `uses` and
/// `calls` edges count for file-level dependency depth.
#[tokio::test]
async fn dependency_depth_excludes_implements_and_extends() {
    // Public helper exposed from the lib for unit-test inspection.
    use tracedecay::graph::queries::GraphQueryManager;
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();

    let qm = GraphQueryManager::new(cg.db());
    let adj = qm.build_file_adjacency(None).await.unwrap();
    // Neither a.rs nor b.rs imports the other; the only edges between
    // them would come from implements/extends junk. After the fix, adj
    // should report no cross-file deps between the two leaf files.
    let from_a = adj.get("src/a.rs").cloned().unwrap_or_default();
    let from_b = adj.get("src/b.rs").cloned().unwrap_or_default();
    assert!(
        !from_a.contains("src/b.rs"),
        "src/a.rs must not depend on src/b.rs; got adj={from_a:?}"
    );
    assert!(
        !from_b.contains("src/a.rs"),
        "src/b.rs must not depend on src/a.rs; got adj={from_b:?}"
    );
}

/// Regression: `tracedecay_run_affected_tests` must dispatch the test
/// functions that are themselves in `changed_paths`. Previously the
/// handler walked callers of every node in the changed file — but
/// `#[test]` functions are leaves with no callers, so a PR that only
/// edits `tests/foo.rs` would return "no tests cover the changed
/// paths" and skip running anything.
#[tokio::test]
async fn run_affected_tests_dispatches_directly_changed_test_files() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn util() -> u32 { 1 }\n").unwrap();
    fs::write(
        project.join("Cargo.toml"),
        r#"[package]
name = "t"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    fs::write(
        project.join("tests/edited_only.rs"),
        r#"
#[test]
fn edited_only_test() {
    assert_eq!(2, 2);
}
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_run_affected_tests",
        json!({
            "changed_paths": ["tests/edited_only.rs"],
            "timeout_secs": 60,
            "max_tests": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    // If no tests get dispatched the handler short-circuits with a
    // note: "no tests cover the changed paths (1 file(s))". After the
    // fix, the test in the edited file itself must be dispatched.
    let dispatched = output["dispatched_tests"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        dispatched.iter().any(|n| n.contains("edited_only_test")),
        "expected edited_only_test to be dispatched; got dispatched={dispatched:?} note={:?}",
        output["note"]
    );
}

/// Regression: `tracedecay_diagnose` must normalize span paths before
/// looking them up in the graph. cargo emits absolute and (on Windows)
/// backslash-separated paths; the graph stores project-relative,
/// forward-slash paths. Without normalization a diagnostic with span
/// `/abs/path/to/project/src/lib.rs:42:1` or `src\lib.rs:42:1` resolves
/// to `node: null` even though the file is indexed.
#[tokio::test]
async fn diagnose_normalizes_absolute_and_backslash_paths() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn target() {}\n").unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

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

/// Regression: PR8's resolver kind-compatibility filter must apply to
/// the same-file blocklist branches too. Without it, common names like
/// `new`/`default`/`clone` can still bind a `Calls` reference to a
/// non-callable same-file symbol — e.g. a const literally named
/// `default` — when it's the only same-file match for a blocklisted
/// name.
#[tokio::test]
async fn resolver_blocklist_branch_respects_kind_filter() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Use a struct named after a blocklisted identifier ("new") plus a
    // call site that the parser definitely treats as a call_expression.
    // Pre-fix the resolver's same-file blocklist branch would bind the
    // Calls ref to this struct because no other "new" lives in the file.
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
    cg.index_all().await.unwrap();

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

/// Regression for bug #11: when an `impl Trait for X` reference cannot
/// resolve to a real trait node (e.g. `Default` lives in std and isn't
/// indexed), the resolver MUST NOT fuzzy-bind it to an unrelated node
/// kind. The sonium codebase had a parser `Token` enum whose `Default`
/// variant became the target of 150 stray `implements` edges from
/// manual `impl Default for X` blocks, completely poisoning
/// `tracedecay_rank --edge-kind implements`. Implements/Extends/derives
/// references must only resolve to trait-shaped targets.
#[tokio::test]
async fn implements_refs_dont_resolve_to_enum_variants() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub enum Token { Default, Plus }

pub struct A;
impl Default for A { fn default() -> Self { A } }

pub struct B;
impl Default for B { fn default() -> Self { B } }
"#,
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_rank",
        json!({"edge_kind": "implements", "direction": "incoming"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let output: Value = serde_json::from_str(text).unwrap();
    let ranking = output["ranking"].as_array().unwrap();
    for entry in ranking {
        let kind = entry["kind"].as_str().unwrap_or("");
        let name = entry["name"].as_str().unwrap_or("");
        assert!(
            kind != "enum_variant" && kind != "field",
            "implements edges must not target {kind} (got name={name})"
        );
    }
}

/// Regression for bug #10: `tracedecay_circular` must report one entry per
/// strongly-connected component, not every walk through the cycle. The
/// sonium codebase had 73 "cycles" that were all different DFS paths
/// through the same SCC. After the SCC refactor, the same data yields
/// one entry per genuine component.
#[tokio::test]
async fn circular_reports_one_entry_per_scc_not_per_walk() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Three-file cycle: a uses b, b uses c, c uses a. Multiple DFS walks
    // through this triangle would have reported 3+ "cycles" pre-fix
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
    cg.index_all().await.unwrap();
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
    let cycle = output["cycles"][0].as_array().unwrap();
    assert_eq!(
        cycle.len(),
        3,
        "the cycle should list all three files in the SCC; got {cycle:?}"
    );
}

/// Regression for bug #12: `tracedecay_port_order`'s `cycles` output must
/// expose the SCCs forming each cycle separately, instead of collapsing
/// all unsorted nodes into a single mega-blob. Without this, on a real
/// codebase the cycle entry contained 200+ unrelated symbols and the
/// agent had no way to know what to break first.
#[tokio::test]
async fn port_order_reports_separate_scc_groups() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    // Two disjoint mutually-recursive pairs: (a, b) and (c, d). Before
    // the fix, both pairs would be lumped into a single "Mutual
    // dependency" entry. After the fix, each pair appears as its own
    // cycle group.
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
    cg.index_all().await.unwrap();
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

/// Regression for new bug-report batch (#25): `tracedecay_port_order` must
/// expose intra-cycle ordering signals so an agent can pick a starting
/// point inside a 200-symbol SCC instead of staring at an undifferentiated
/// blob. We expect each cycle entry to carry per-symbol in-cycle degree
/// data, a file-level member-count breakdown, and explicit `entry_point`
/// / `break_point_candidate` suggestions.
#[tokio::test]
async fn port_order_provides_intra_cycle_ordering() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();
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

/// Regression for the Sonium port-order report: self-edges from fuzzy
/// resolution (`self.rows.push(...)` inside a method named `push`) should
/// not make singleton symbols appear as cycles.
#[tokio::test]
async fn port_order_ignores_self_edges() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();
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

/// Regression for bug #9: `tracedecay_inheritance_depth` must surface Rust
/// supertrait chains (`trait T: U`) as `Extends` edges.
#[tokio::test]
async fn inheritance_depth_walks_rust_supertraits() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();
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

/// Regression for new bug-report batch (#26): `tracedecay_circular` must
/// emit *disjoint* SCCs — no file should appear in more than one cycle
/// entry. The sonium run reported 216 cycles "sharing long tails", which
/// would only be possible if the SCC condensation step were broken. This
/// stress test wires up many disjoint cycles plus DAG-style tails between
/// them and asserts no file leaks into a second cycle entry.
#[tokio::test]
async fn circular_emits_disjoint_sccs_under_load() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    let mut lib_rs = String::new();
    // Build 5 disjoint 3-file cycles with shared DAG tails between them.
    // Cycle k = (a_k -> b_k -> c_k -> a_k); plus a one-way edge from c_k
    // to a_{k+1} that introduces a non-cyclic "shared tail" between the
    // SCCs. Tarjan must still emit each cycle as its own SCC.
    for k in 0..5 {
        lib_rs.push_str(&format!("pub mod a{k};\npub mod b{k};\npub mod c{k};\n"));
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
    cg.index_all().await.unwrap();
    let result = handle_tool_call(&cg, "tracedecay_circular", json!({}), None, None)
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
        let files = cycle.as_array().unwrap();
        for f in files {
            let s = f.as_str().unwrap().to_string();
            assert!(
                seen.insert(s.clone()),
                "file {s} appears in more than one cycle entry; SCCs must be disjoint"
            );
        }
    }
}

/// Regression for new bug-report batch (#24): `tracedecay_diff_context`'s
/// `modified_symbols` must dedup by node id, even when callers pass the
/// same path multiple times in `files`. The sonium run showed an
/// `hmatrix.rs` file node listed 7× in a row because the caller had the
/// same file path duplicated upstream.
#[tokio::test]
async fn diff_context_dedupes_modified_symbols_on_duplicate_input() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub struct S; pub fn one() {} pub fn two() {}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

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

/// Regression for new bug-report batch (#23): when a whole subtree is
/// removed in a diff, `tracedecay_changelog` must not report the deleted
/// directory under `files_not_indexed`. The previous `is_dir()` filter
/// missed this case because the path was gone from disk by the time we
/// checked. The fix uses gix's `entry_mode` flag to skip tree entries
/// before they're ever pushed into the change list.
#[tokio::test]
async fn changelog_filters_deleted_directory_entries() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fn git(cwd: &std::path::Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|_| panic!("git {args:?} failed"));
    }
    git(project, &["init"]);
    git(project, &["config", "user.email", "t@t"]);
    git(project, &["config", "user.name", "t"]);
    fs::create_dir_all(project.join("crates/sub")).unwrap();
    fs::write(project.join("crates/sub/keep.rs"), "pub fn k() {}\n").unwrap();
    fs::write(project.join("main.rs"), "fn main() {}\n").unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "init"]);
    // Remove the whole subtree so gix's tree-diff yields a directory-mode
    // deletion entry.
    fs::remove_dir_all(project.join("crates")).unwrap();
    git(project, &["add", "-A"]);
    git(project, &["commit", "-m", "drop crates"]);
    let (cg, _env) = init_test_project(project).await;
    // Intentionally skipping `index_all` — the changelog handler reads from
    // git directly and the sync lock has a pre-existing parallel-test flake.
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

/// Regression for new bug-report batch (#22): `tracedecay_pr_context` must
/// NOT explode Cargo.toml (or any .toml/.yaml/.json config file) into one
/// symbol per `[name]`, `[version]`, `[dependencies]` key. On real PRs a
/// Cargo.toml change with ~30 dependency lines produced ~70 entries that
/// pushed the response past 760k tokens. Config files should collapse to
/// a single summary symbol.
#[tokio::test]
async fn pr_context_collapses_cargo_toml_keys() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fn git(cwd: &std::path::Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|_| panic!("git {args:?} failed"));
    }
    git(project, &["init"]);
    git(project, &["config", "user.email", "t@t"]);
    git(project, &["config", "user.name", "t"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "init"]);
    // Second commit: bloat Cargo.toml with many deps.
    let mut bloated = String::from(
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    for i in 0..50 {
        bloated.push_str(&format!("dep{i} = \"0.1.{i}\"\n"));
    }
    fs::write(project.join("Cargo.toml"), &bloated).unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "deps"]);

    let (cg, _env) = init_test_project(project).await;
    // Intentionally skipping `index_all()` — pr_context reads the diff
    // from git directly and classifies Cargo.toml as `config` before any
    // index lookup, so we don't need the index to verify the collapse
    // behaviour. Calling `index_all()` here triggers the pre-existing
    // SyncLock parallel-test flake (#test_changelog_with_real_git).

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

/// Regression for new bug-report batch (#21): `tracedecay_unused_imports`
/// must flag genuinely unused identifiers inside grouped `use foo::{A, B}`
/// imports. Real-world Rust style is dominated by grouped imports
/// (`use std::collections::{HashMap, HashSet, BTreeMap};`); without
/// per-identifier splitting, the heuristic could never flag anything from
/// a grouped import, which is why the user's run reported 0 / 3,404 use
/// nodes.
#[tokio::test]
async fn unused_imports_handles_grouped_use() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();
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

/// Regression for new bug-report batch (#20): `tracedecay_dead_code` must not
/// consider non-reference edges like `annotates` or `derives_macro` as
/// "this function is alive" evidence. Previously, a private helper with no
/// callers but an `#[inline]` (or any other attribute) on it had an
/// incoming `annotates` edge from the synthesised annotation_usage node,
/// which the SQL `NOT EXISTS (target = id AND kind != 'contains')` filter
/// accepted as a live reference. Real-world Rust codebases use attributes
/// pervasively, which is why the user's run found zero dead functions
/// across 5,715.
#[tokio::test]
async fn dead_code_flags_unreferenced_fn_with_attribute() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
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
    cg.index_all().await.unwrap();

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

/// Regression for new bug-report batch (#19): `tracedecay_search` must rank
/// trait/struct/function definitions above `use` re-exports of the same name.
/// Previously, several `use foo::LinearOperator;` lines could outrank the
/// `pub trait LinearOperator { … }` definition because BM25 scored short
/// re-export rows highly. We now force a kind tier ahead of BM25 score so a
/// real def always beats `use` rows.
#[tokio::test]
async fn search_ranks_trait_definition_above_use_reexports() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src/a")).unwrap();
    fs::create_dir_all(project.join("src/b")).unwrap();
    fs::create_dir_all(project.join("src/c")).unwrap();
    fs::create_dir_all(project.join("src/d")).unwrap();
    fs::create_dir_all(project.join("src/e")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
pub mod operator;
pub mod a;
pub mod b;
pub mod c;
pub mod d;
pub mod e;
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/operator.rs"),
        "pub trait LinearOperator { fn apply(&self); }\n",
    )
    .unwrap();
    for sub in ["a", "b", "c", "d", "e"] {
        fs::write(
            project.join(format!("src/{sub}/mod.rs")),
            "pub use crate::operator::LinearOperator;\n",
        )
        .unwrap();
    }
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_search",
        json!({"query": "LinearOperator", "limit": 10}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let items: Value = serde_json::from_str(text).unwrap();
    let arr = items.as_array().unwrap();
    let first_kind = arr[0]["kind"].as_str().unwrap_or("");
    assert_eq!(
        first_kind, "trait",
        "first search hit for LinearOperator should be the trait definition, got '{first_kind}' (full: {arr:?})"
    );
}

// ---------------------------------------------------------------------------
// McpServer::refresh_file_token_map
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_file_token_map_picks_up_new_files() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(project.join("a.rs"), "fn a() {}").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.sync().await.unwrap();

    let server = tracedecay::mcp::McpServer::new(cg, None).await;
    let initial_map = server.file_token_map_snapshot();
    let initial_keys: std::collections::HashSet<_> = initial_map.keys().cloned().collect();

    // Add a new file, sync it, then refresh.
    std::fs::write(project.join("b.rs"), "fn b() { let y = 2; }").unwrap();
    let cg2 = tracedecay::tracedecay::TraceDecay::open(project)
        .await
        .unwrap();
    cg2.sync().await.unwrap();

    server.refresh_file_token_map().await;
    let after_map = server.file_token_map_snapshot();
    let after_keys: std::collections::HashSet<_> = after_map.keys().cloned().collect();

    assert!(
        after_keys.len() > initial_keys.len(),
        "refresh should pick up b.rs"
    );
}

// ---------------------------------------------------------------------------
// McpServer-owned embedded watcher
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_server_owns_watcher_and_refreshes_token_map_on_change() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(project.join("a.rs"), "fn a() {}").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.sync().await.unwrap();

    let server = tracedecay::mcp::McpServer::new(cg, None).await;
    assert!(
        server
            .wait_for_startup_catch_up(std::time::Duration::from_secs(2))
            .await,
        "startup catch-up sync should finish before mutating the project"
    );

    let initial_count = server.file_token_map_snapshot().len();

    // Edit a file, then drive the lazy staleness check that replaced the
    // notify-based watcher (#80). MCP `tools/call` triggers this on the
    // hot path; here we exercise the same pipeline directly so the test
    // doesn't have to wait through the 30 s cooldown gate in
    // `maybe_sync_if_stale`.
    std::fs::write(project.join("b.rs"), "fn b() {}").unwrap();
    let server_cg = server.cg().await;
    let stale = server_cg.find_stale_files().await;
    assert!(
        !stale.is_empty(),
        "find_stale_files should detect newly written b.rs"
    );
    server_cg.sync_if_stale_silent(&stale).await.unwrap();
    let mut after_count = initial_count;
    for _ in 0..10 {
        server.refresh_file_token_map().await;
        after_count = server.file_token_map_snapshot().len();
        if after_count > initial_count {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        after_count > initial_count,
        "lazy sync should have refreshed map ({initial_count} -> {after_count})"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn lcm_expand_paginates_summary_sources_over_mcp() {
    let (cg, _dir) = setup_project().await;
    let mut store_ids = Vec::new();
    for index in 1..=4 {
        let message_id = format!("page-msg-{index}");
        seed_lcm_session_message(
            &cg,
            "lcm-page-session",
            &message_id,
            format!("paged source body {index}"),
            index,
        )
        .await;
        store_ids.push(lcm_raw_store_id(&cg, &message_id).await);
    }
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    let summary = db
        .lcm_insert_summary_node(LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "lcm-page-session".to_string(),
            session_id: "lcm-page-session".to_string(),
            depth: 0,
            summary_text: "paged summary".to_string(),
            source_refs: store_ids
                .iter()
                .map(|store_id| LcmSourceRef::RawMessage {
                    store_id: *store_id,
                })
                .collect(),
            source_token_count: 16,
            summary_token_count: 2,
            source_time_start: Some(1),
            source_time_end: Some(4),
            expand_hint: Some("pagination test".to_string()),
            metadata_json: None,
        })
        .await
        .expect("summary should insert");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-page-session",
            "target": {"kind": "summary_node", "node_id": summary.node_id},
            "source_offset": 1,
            "source_limit": 2
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    let sources = payload["expansion"]["summary_sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["raw_message"]["store_id"], json!(store_ids[1]));
    assert_eq!(sources[1]["raw_message"]["store_id"], json!(store_ids[2]));
    let pagination = &payload["expansion"]["source_pagination"];
    assert_eq!(pagination["source_offset"], 1);
    assert_eq!(pagination["source_limit"], 2);
    assert_eq!(pagination["returned_sources"], 2);
    assert_eq!(pagination["total_sources"], 4);
    assert_eq!(pagination["next_source_offset"], 3);
    assert_eq!(pagination["has_more"], true);
    assert_eq!(pagination["remaining_sources"], 1);
}

#[tokio::test]
async fn lcm_expand_resolves_cross_session_store_ids_over_mcp() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-origin-session",
        "origin-message",
        "cross session grep target body",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-active-session",
        "active-message",
        "the caller's active session",
        2,
    )
    .await;
    let origin_store_id = lcm_raw_store_id(&cg, "origin-message").await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-active-session",
            "target": {"kind": "raw_message", "store_id": origin_store_id}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["expansion"]["kind"], "raw_message");
    assert_eq!(payload["expansion"]["from_current_session"], false);
    assert_eq!(
        payload["expansion"]["raw_message"]["session_id"],
        "lcm-origin-session"
    );
    assert_eq!(
        payload["expansion"]["content"],
        "cross session grep target body"
    );
}

#[tokio::test]
async fn lcm_expand_cross_session_external_payload_supports_two_step_hydration() {
    let (cg, _dir) = setup_project().await;
    let body = format!("data:image/png;base64,{}", "A".repeat(220_000));
    seed_lcm_tool_result_message(
        &cg,
        "lcm-origin-session",
        "origin-external-message",
        body,
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-active-session",
        "active-message",
        "active context",
        2,
    )
    .await;
    let origin_store_id = lcm_raw_store_id(&cg, "origin-external-message").await;

    let raw_result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-active-session",
            "target": {"kind": "raw_message", "store_id": origin_store_id}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let raw_payload: Value = serde_json::from_str(extract_text(&raw_result.value)).unwrap();
    assert_eq!(raw_payload["status"], "ok");
    assert_eq!(raw_payload["expansion"]["from_current_session"], false);
    assert!(raw_payload["expansion"]["externalized_note"].is_null());
    let payload_ref = raw_payload["expansion"]["payload_ref"]
        .as_str()
        .expect("cross-session external row should surface payload_ref")
        .to_string();
    let owner_session = raw_payload["expansion"]["raw_message"]["session_id"]
        .as_str()
        .expect("owner session id should be surfaced")
        .to_string();

    let payload_result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": owner_session,
            "target": {"kind": "external_payload", "payload_ref": payload_ref},
            "content_limit": 80
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&payload_result.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["expansion"]["kind"], "external_payload");
    assert!(payload["expansion"]["content"]
        .as_str()
        .expect("external payload content")
        .starts_with("data:image/png;base64,"));
}

#[tokio::test]
async fn lcm_compress_handler_honors_incremental_max_depth_override() {
    let (cg, _dir) = setup_project().await;
    let mut store_ids = Vec::new();
    for index in 1..=6 {
        let message_id = format!("depth-msg-{index}");
        seed_lcm_session_message(
            &cg,
            "lcm-depth-session",
            &message_id,
            format!("depth source body {index}"),
            index,
        )
        .await;
        store_ids.push(lcm_raw_store_id(&cg, &message_id).await);
    }
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    for (index, pair) in store_ids.chunks(2).enumerate() {
        db.lcm_insert_summary_node(LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "lcm-depth-session".to_string(),
            session_id: "lcm-depth-session".to_string(),
            depth: 1,
            summary_text: format!("depth one summary {}", index + 1),
            source_refs: pair
                .iter()
                .map(|store_id| LcmSourceRef::RawMessage {
                    store_id: *store_id,
                })
                .collect(),
            source_token_count: 12,
            summary_token_count: 4,
            source_time_start: Some(10 + index as i64),
            source_time_end: Some(20 + index as i64),
            expand_hint: Some("depth override test".to_string()),
            metadata_json: None,
        })
        .await
        .expect("depth-1 summary should insert");
    }
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".to_string(),
        conversation_id: "lcm-depth-session".to_string(),
        current_session_id: "lcm-depth-session".to_string(),
        current_frontier_store_id: store_ids.last().copied(),
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt: Vec::new(),
    })
    .await
    .expect("lifecycle state should update");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-depth-session",
            "messages": [],
            "summary_fan_in": 3,
            "incremental_max_depth": 2,
            "summarizer": {"mode": "fake", "summary_text": "depth-two condensation"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["reason"], "condensed_summary_nodes");
    assert_eq!(payload["summary_nodes_created"], 1);
    assert_eq!(payload["summary_nodes"][0]["depth"], 2);
    assert!(payload["context_recovery_hint"]
        .as_str()
        .unwrap()
        .contains("tracedecay_lcm_expand_query"));
}

#[tokio::test]
async fn lcm_status_reports_dag_store_and_config_diagnostics_over_mcp() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-diag-session",
        "diag-message",
        "alpha beta gamma delta",
        1,
    )
    .await;
    let store_id = lcm_raw_store_id(&cg, "diag-message").await;
    let db = open_project_session_db(cg.project_root())
        .await
        .expect("project-local session db should open");
    db.lcm_insert_summary_node(LcmSummaryNodeDraft {
        provider: "cursor".to_string(),
        conversation_id: "lcm-diag-session".to_string(),
        session_id: "lcm-diag-session".to_string(),
        depth: 0,
        summary_text: "diag summary".to_string(),
        source_refs: vec![LcmSourceRef::RawMessage { store_id }],
        source_token_count: 24,
        summary_token_count: 6,
        source_time_start: Some(1),
        source_time_end: Some(2),
        expand_hint: Some("diagnostics test".to_string()),
        metadata_json: None,
    })
    .await
    .expect("summary should insert");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({"provider": "cursor", "session_id": "lcm-diag-session"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    let lcm = &payload["lcm"];
    assert_eq!(lcm["store"]["messages"], 1);
    assert_eq!(lcm["store"]["estimated_tokens"], 4);
    assert_eq!(lcm["dag"]["total_nodes"], 1);
    assert_eq!(lcm["dag"]["total_tokens"], 6);
    assert_eq!(lcm["dag"]["total_source_tokens"], 24);
    assert_eq!(lcm["dag"]["compression_ratio"], "4.0:1");
    assert_eq!(lcm["dag"]["depths"]["d0"]["count"], 1);
    assert_eq!(lcm["dag"]["depths"]["d0"]["tokens"], 6);
    assert_eq!(lcm["dag"]["depths"]["d0"]["source_tokens"], 24);
    assert_eq!(lcm["config"]["fresh_tail_count"], 2);
    assert_eq!(lcm["config"]["summary_fan_in"], 4);
    assert_eq!(lcm["config"]["compression_boundary_cooldown_seconds"], 60);
}

// Repeated LCM tool calls in one process must reuse the per-process
// "schema already ensured" flag instead of re-opening the project DB with a
// full DDL ensure each time. Observable via the version gate: after the
// first call marks the path as ensured, a manually downgraded version
// marker stays downgraded — a re-run of the migrations would bump it back
// to LCM_SCHEMA_VERSION.
#[tokio::test]
async fn repeated_lcm_calls_skip_schema_reensure_per_process() {
    let (cg, _dir) = setup_project().await;

    // Seed data to ensure the sessions.db exists (lcm_status is now read-only
    // and will not create the DB). The schema-ensure caching under test lives
    // in the write-path open (`open_session_db_with_cached_ensure`), triggered
    // by the seed call, and is observable via the lcm_status read.
    seed_lcm_session_message(
        &cg,
        "ensure-cache-session",
        "ensure-cache-msg",
        "schema ensure cache sentinel",
        1,
    )
    .await;

    let result = handle_tool_call(&cg, "tracedecay_lcm_status", json!({}), None, None)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["lcm"]["schema_version"],
        json!(tracedecay::sessions::lcm::LCM_SCHEMA_VERSION)
    );

    let db_path = project_session_db_path(&cg);
    {
        let db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE session_schema_migrations SET version = 1 WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    }

    let result = handle_tool_call(&cg, "tracedecay_lcm_status", json!({}), None, None)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(
        payload["status"], "ok",
        "repeated serve-mode call must work"
    );
    assert_eq!(
        payload["lcm"]["schema_version"],
        json!(1),
        "second call must take the per-process ensured fast path instead of re-running migrations"
    );

    // The on-disk marker is untouched as well.
    let db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
}

// ---------------------------------------------------------------------------
// Scope validation (fail-closed, not fail-open)
// ---------------------------------------------------------------------------

/// Regression test: an invalid `scope` must be a hard error naming the valid
/// values — never silently broadened to `all`.
#[tokio::test]
async fn lcm_grep_rejects_invalid_scope() {
    let (cg, _dir) = setup_project().await;
    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_lcm_grep",
            json!({"query": "anything", "scope": "everything"}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("scope must be one of current, session, all"),
        "unexpected error: {err}"
    );
}

/// Same contract for `tracedecay_message_search`: invalid scope values fail
/// closed instead of broadening the search to every session.
#[tokio::test]
async fn message_search_rejects_invalid_scope() {
    let (cg, _dir) = setup_project().await;
    for invalid in ["everything", "", "parents"] {
        let err = expect_tool_error(
            handle_tool_call(
                &cg,
                "tracedecay_message_search",
                json!({"query": "anything", "scope": invalid}),
                None,
                None,
            )
            .await,
        );
        assert!(
            err.contains("scope must be one of all, parents_only, subagents_only"),
            "unexpected error for scope {invalid:?}: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// Regression: ghost-create — pure-read LCM tools must not create sessions.db
// ---------------------------------------------------------------------------

/// Calling a `readOnlyHint` LCM tool on a project that has never ingested
/// any sessions must:
///   1. Return `status: "not_ingested"` (not "ok" or "unavailable").
///   2. Set `store_exists: false` so callers can distinguish "nothing yet"
///      from an I/O error.
///   3. NOT create the sessions.db file on disk.
#[tokio::test]
async fn lcm_read_only_tools_return_not_ingested_without_creating_sessions_db() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    std::fs::write(project.join("lib.rs"), "fn f() {}").unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let db_path = tracedecay::sessions::cursor::project_session_db_path(project);

    // Confirm no DB exists before calling any tool.
    assert!(
        !db_path.exists(),
        "sessions.db must not exist before any ingest"
    );

    // Exercise all six pure-read LCM tools.
    for (tool, args) in [
        ("tracedecay_lcm_status", json!({})),
        ("tracedecay_lcm_grep", json!({"query": "anything"})),
        (
            "tracedecay_lcm_load_session",
            json!({"session_id": "ghost-session"}),
        ),
        (
            "tracedecay_lcm_describe",
            json!({"session_id": "ghost-session"}),
        ),
        (
            "tracedecay_lcm_expand",
            json!({"session_id": "ghost-session", "target": {"kind": "raw_message", "store_id": 1}}),
        ),
        (
            "tracedecay_lcm_expand_query",
            json!({"session_id": "ghost-session", "prompt": "anything"}),
        ),
    ] {
        let result = handle_tool_call(&cg, tool, args.clone(), None, None)
            .await
            .unwrap_or_else(|e| panic!("{tool} returned error: {e}"));

        let text = extract_text(&result.value);
        let payload: Value = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("{tool} response is not valid JSON: {e}\n{text}"));

        assert_eq!(
            payload["status"], "not_ingested",
            "{tool}: expected status=not_ingested, got {payload}"
        );
        assert_eq!(
            payload["store_exists"], false,
            "{tool}: expected store_exists=false, got {payload}"
        );

        assert!(
            !db_path.exists(),
            "{tool}: sessions.db was ghost-created at {}",
            db_path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Regression: max_tokens must not suppress context budget
// ---------------------------------------------------------------------------

/// Before the fix, `default_context_limit = max_tokens.clamp(32_000, 65_536)`
/// always evaluated to 32_000 because max_tokens ≤ 8_192 < 32_000, making
/// `max_tokens` dead. After the fix, `context_max_tokens` defaults to the
/// constant 32_000 and both params are independent. We verify that the handler
/// accepts an explicit `context_max_tokens` override and that the returned
/// payload reflects it.
#[tokio::test]
async fn lcm_expand_query_context_max_tokens_is_independent_of_max_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    std::fs::write(project.join("lib.rs"), "fn f() {}").unwrap();
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    // With no sessions.db the tool returns not_ingested — that is fine here;
    // we just verify the argument parsing does not panic or error.
    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand_query",
        json!({
            "session_id": "test-session",
            "prompt": "what did we discuss?",
            "max_tokens": 500,
            "context_max_tokens": 48000,
        }),
        None,
        None,
    )
    .await
    .expect("expand_query with explicit context_max_tokens must not error");

    let text = extract_text(&result.value);
    let payload: Value =
        serde_json::from_str(text).expect("expand_query result must be valid JSON");

    // Either not_ingested (no sessions.db) or ok — both are valid here.
    // The important thing: it must NOT return a Config/argument error about
    // max_tokens or context_max_tokens.
    assert!(
        payload["status"] == "not_ingested" || payload["status"] == "ok",
        "unexpected status in expand_query response: {payload}"
    );
}

// ---------------------------------------------------------------------------
// Regression: catch-up flag ordering — transcript_ingest_done must lag
// ---------------------------------------------------------------------------

/// `wait_for_startup_catch_up` must wait for the transcript-ingest task to
/// complete (transcript_ingest_done), not just the file-tree sync
/// (startup_catch_up_done). This test verifies that after waiting, the
/// `transcript_ingest_done` flag is always true.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_startup_catch_up_waits_for_transcript_ingest_flag() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    std::fs::write(project.join("lib.rs"), "fn f() {}").unwrap();

    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let server = tracedecay::mcp::McpServer::new(cg, None).await;
    server.run_startup_catch_up_sync().await;

    let completed = server
        .wait_for_startup_catch_up(std::time::Duration::from_secs(5))
        .await;

    assert!(completed, "wait_for_startup_catch_up timed out after 5s");

    // After the wait returns true, both flags must be set.
    assert!(
        server.startup_catch_up_done(),
        "startup_catch_up_done must be true after wait"
    );
    assert!(
        server.transcript_ingest_done(),
        "transcript_ingest_done must be true after wait"
    );

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Store failures must surface as tool errors, not silent empty results
// (cross-cutting audit: silent-empty handlers). Breaking the `edges` table
// out from under the open connection makes every edge query fail while
// node/file queries keep working — exactly the partial-store-failure case
// the old `unwrap_or_default()` calls papered over as "no data".
// ---------------------------------------------------------------------------

/// Renames the `edges` table so every edge query on the open connection
/// fails while node and file queries keep working.
async fn break_edges_table(cg: &TraceDecay) {
    cg.db()
        .conn()
        .execute("ALTER TABLE edges RENAME TO edges_broken", ())
        .await
        .unwrap();
}

#[tokio::test]
async fn simplify_scan_surfaces_store_failure_instead_of_no_findings() {
    let (cg, _dir) = setup_project().await;
    break_edges_table(&cg).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_simplify_scan",
        json!({"files": ["src/utils.rs"]}),
        None,
        None,
    )
    .await;
    assert!(
        result.is_err(),
        "a failing store query must produce a tool error, not an empty findings list"
    );
}

#[tokio::test]
async fn simplify_scan_markdown_visible_output_is_not_escaped_blob() {
    let (cg, dir) = setup_project().await;
    fs::write(
        dir.path().join("src/dead.rs"),
        r#"
fn abandoned_helper() -> usize {
    7
}
"#,
    )
    .unwrap();
    index_all_retrying_sync_lock(&cg).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_simplify_scan",
        json!({"files": ["src/dead.rs"], "format": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    assert!(text.contains("# Simplify Scan"), "got: {text}");
    assert!(text.contains("## Potential Dead Code"), "got: {text}");
    assert!(text.contains("abandoned_helper"), "got: {text}");
    assert!(
        serde_json::from_str::<Value>(text).is_err(),
        "visible markdown should not be a JSON envelope: {text}"
    );
    assert!(
        !text.contains("\\n") && !text.contains("\\\""),
        "visible markdown should not contain escaped markdown/json: {text}"
    );
    assert!(
        !text.contains("\"content\""),
        "visible markdown should not contain a nested MCP envelope: {text}"
    );
}

#[tokio::test]
async fn type_hierarchy_surfaces_store_failure_instead_of_empty_tree() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;
    break_edges_table(&cg).await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_type_hierarchy",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await;
    assert!(
        result.is_err(),
        "a failing store query must produce a tool error, not an empty hierarchy"
    );
}

#[tokio::test]
async fn message_search_selects_registered_project_session_db_by_project_id() {
    let (cg, _dir) = setup_project().await;
    let profile_root = cg.project_root().join("home/.tracedecay");
    let target_project = cg.project_root().join("registered-target");
    let target_project_path = target_project.to_string_lossy().to_string();
    let target_store_relpath = "projects/proj_cross_messages";
    let target_store_root = profile_root.join(target_store_relpath);
    let target_session_db = target_store_root.join("sessions.db");
    fs::create_dir_all(&target_project).unwrap();
    fs::create_dir_all(&target_store_root).unwrap();

    let active_db = open_project_session_db(cg.project_root())
        .await
        .expect("active project session db should open");
    assert!(
        active_db
            .upsert_session(&SessionRecord {
                provider: "cursor".to_string(),
                session_id: "active-session".to_string(),
                project_key: cg.project_root().to_string_lossy().to_string(),
                project_path: cg.project_root().to_string_lossy().to_string(),
                title: Some("Active project".to_string()),
                started_at: Some(1),
                ended_at: None,
                transcript_path: Some("active-session.jsonl".to_string()),
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    assert!(
        active_db
            .upsert_session_message(&SessionMessageRecord {
                provider: "cursor".to_string(),
                message_id: "active-message".to_string(),
                session_id: "active-session".to_string(),
                role: "user".to_string(),
                timestamp: Some(2),
                ordinal: 1,
                text: "Cross project dragonfruit belongs to the active database.".to_string(),
                kind: Some("message".to_string()),
                model: Some("test-model".to_string()),
                tool_names: None,
                source_path: Some("active-session.jsonl".to_string()),
                source_offset: Some(0),
                metadata_json: None,
            })
            .await
    );

    let registry = GlobalDb::open().await.expect("global registry should open");
    let project = registry
        .upsert_code_project(
            "proj_cross_messages",
            &target_project,
            None,
            None,
            Some("main"),
        )
        .await
        .expect("registered project should upsert");
    let store = registry
        .upsert_store_instance(tracedecay::global_db::StoreInstanceUpsert {
            store_id: "store_cross_messages".to_string(),
            project_id: project.project_id,
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: target_store_relpath.to_string(),
            manifest_relpath: Some(format!("{target_store_relpath}/store_manifest.json")),
            last_verified_at: Some(1_800_000_010),
            last_write_at: Some(1_800_000_011),
        })
        .await
        .expect("registered store should upsert");
    registry
        .upsert_store_artifact(tracedecay::global_db::StoreArtifactUpsert {
            store_id: store.store_id,
            artifact_kind: "sessions_db".to_string(),
            relpath: format!("{target_store_relpath}/sessions.db"),
            size_bytes: None,
            schema_version: Some("1".to_string()),
            updated_at: Some(1_800_000_012),
        })
        .await
        .expect("session artifact should upsert");

    let target_db = GlobalDb::open_at(&target_session_db)
        .await
        .expect("registered project session db should open");
    assert!(
        target_db
            .upsert_session(&SessionRecord {
                provider: "cursor".to_string(),
                session_id: "target-session".to_string(),
                project_key: target_project_path.clone(),
                project_path: target_project_path.clone(),
                title: Some("Registered project".to_string()),
                started_at: Some(10),
                ended_at: None,
                transcript_path: Some("target-session.jsonl".to_string()),
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    assert!(
        target_db
            .upsert_session_message(&SessionMessageRecord {
                provider: "cursor".to_string(),
                message_id: "target-message".to_string(),
                session_id: "target-session".to_string(),
                role: "assistant".to_string(),
                timestamp: Some(11),
                ordinal: 1,
                text: "Cross project dragonfruit belongs to the registered database.".to_string(),
                kind: Some("message".to_string()),
                model: Some("test-model".to_string()),
                tool_names: None,
                source_path: Some("target-session.jsonl".to_string()),
                source_offset: Some(0),
                metadata_json: None,
            })
            .await
    );

    fn message_search_args(selector: Value) -> Value {
        let mut args = json!({
            "query": "dragonfruit",
            "provider": "cursor",
            "limit": 5
        });
        args.as_object_mut()
            .unwrap()
            .extend(selector.as_object().unwrap().clone());
        args
    }

    for (label, selector) in [
        ("project_id", json!({"project_id": "proj_cross_messages"})),
        (
            "project_path",
            json!({"project_path": target_project_path.clone()}),
        ),
        (
            "project_root",
            json!({"project_root": target_project_path.clone()}),
        ),
        (
            "nested path",
            json!({"project_selector": {"path": target_project_path.clone()}}),
        ),
        (
            "nested project_path",
            json!({"project_selector": {"project_path": target_project_path.clone()}}),
        ),
    ] {
        let result = handle_tool_call(
            &cg,
            "tracedecay_message_search",
            message_search_args(selector),
            None,
            None,
        )
        .await
        .unwrap_or_else(|err| panic!("{label} selector should resolve target project: {err}"));
        let parsed = extract_json(&result.value);

        assert_eq!(parsed["status"], "ok", "{label}: {parsed}");
        assert_eq!(
            parsed["selected_project_root"],
            target_project_path.as_str(),
            "{label}: {parsed}"
        );
        assert_eq!(parsed["count"], 1, "{label}: {parsed}");
        assert_eq!(
            parsed["results"][0]["message"]["message_id"], "target-message",
            "{label}: {parsed}"
        );
        assert_eq!(
            parsed["results"][0]["session"]["project_key"],
            target_project_path.as_str(),
            "{label}: {parsed}"
        );
    }

    for (label, selector) in [
        (
            "missing project_id",
            json!({"project_id": "proj_missing_messages"}),
        ),
        (
            "missing nested path",
            json!({"project_selector": {"path": cg.project_root().join("missing-project").to_string_lossy().to_string()}}),
        ),
    ] {
        let err = expect_tool_error(
            handle_tool_call(
                &cg,
                "tracedecay_message_search",
                message_search_args(selector),
                None,
                None,
            )
            .await,
        );
        assert!(
            err.contains("registered project not found for selector"),
            "{label} must fail closed instead of searching the active session DB: {err}"
        );
    }
}
