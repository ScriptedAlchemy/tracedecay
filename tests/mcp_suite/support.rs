#![allow(clippy::too_many_arguments, clippy::clone_on_copy)] // test builders
//! Shared fixtures and helpers for the MCP handler test domains,
//! split mechanically from `mcp_handler_test.rs`.

use crate::common;
use crate::fixture;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::{Mutex, MutexGuard};
use tracedecay::errors::TraceDecayError;
use tracedecay::global_db::GlobalDb;
use tracedecay::mcp::{McpServer, McpTransport, ToolResult};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::storage::{
    resolve_layout_for_current_profile, resolve_lcm_payload_root, resolve_response_handle_root,
};
use tracedecay::store::{GlobalDbObservationStore, GlobalDbSessionTemporalStore};
use tracedecay::tracedecay::TraceDecay;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ComponentVersion,
    DurableObservationV1, MessageOccurrenceIdV1, MessageOccurrenceRecordV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadAccessState, PayloadReferenceV1, ProjectId,
    ProjectionGenerationId, ProjectionOutputOrdinalV1, ProviderId, RetentionClass,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionCursorKeyIdV1, SessionCursorVersionV1, SessionId, SessionProjectionGenerationV1,
    SignedCursorKeyRefV1, UtcMicros, derive_exact_observation_anchor_id,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    SessionFrozenWatermarksV1, SessionGenerationActivationRequestV1,
    SessionGenerationRebuildRequestV1, SessionTemporalCapabilitiesV1, SessionTemporalCapabilityV1,
    SessionTemporalProjectionBatchV1, SessionTemporalProjectionStore, SessionTemporalSnapshotV1,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

pub(crate) static GLOBAL_DB_ENV_LOCK: Mutex<()> = Mutex::const_new(());

pub(crate) const MCP_TEST_RESPONSE_CHAR_LIMIT: usize = 15_000;

#[derive(Default)]
pub(crate) struct CaptureTransport {
    pub(crate) output: String,
}

impl McpTransport for CaptureTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        Ok(None)
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.output.push_str(line);
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) async fn handle_real_server_tool_call(
    server: &McpServer,
    tool_name: &str,
    mut arguments: Value,
) -> Value {
    if let Some(arguments) = arguments.as_object_mut() {
        arguments
            .entry("format".to_string())
            .or_insert_with(|| json!("json"));
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments,
        }
    });
    let mut transport = CaptureTransport::default();
    server
        .handle_and_write(&request.to_string(), &mut transport)
        .await
        .expect("real MCP server tool call");
    let response: Value = serde_json::from_str(transport.output.trim()).expect("JSON-RPC response");
    assert!(response["error"].is_null(), "{response}");
    response["result"].clone()
}

pub(crate) fn extract_real_server_text(result: &Value) -> &str {
    result["content"][0]["text"]
        .as_str()
        .expect("MCP text result")
}

pub(crate) struct TestDbConnection {
    pub(crate) _db: libsql::Database,
    pub(crate) conn: libsql::Connection,
}

impl Deref for TestDbConnection {
    type Target = libsql::Connection;

    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

pub(crate) async fn open_test_db_connection(db_path: &Path) -> TestDbConnection {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    // Mirror the pragma choices of src/db/connection.rs, including the
    // CI-only TRACEDECAY_SQLITE_UNSAFE_FAST=1 escape hatch, so this helper
    // never fights the journal mode the product code selected.
    let unsafe_fast = std::env::var(tracedecay::db::SQLITE_UNSAFE_FAST_ENV).as_deref() == Ok("1");
    let (journal_mode, synchronous) = if unsafe_fast {
        ("MEMORY", "OFF")
    } else if cfg!(windows) {
        ("DELETE", "FULL")
    } else {
        ("WAL", "NORMAL")
    };
    if cfg!(windows) {
        conn.execute_batch("PRAGMA mmap_size = 0;").await.unwrap();
    }
    conn.execute_batch(&format!(
        "PRAGMA journal_mode = {journal_mode};
         PRAGMA busy_timeout = 5000;
         PRAGMA synchronous = {synchronous};
         PRAGMA foreign_keys = ON;"
    ))
    .await
    .unwrap();
    TestDbConnection { _db: db, conn }
}

pub(crate) struct TemporalLcmProjectionInput {
    pub(crate) occurrence: MessageOccurrenceRecordV1,
    pub(crate) source_frontier: u64,
}

pub(crate) async fn activate_test_temporal_generation(
    db: &GlobalDb,
    session_id: &str,
    inputs: Vec<TemporalLcmProjectionInput>,
) -> u64 {
    let session_id = SessionId::new(session_id).unwrap();
    let source_frontier = inputs
        .iter()
        .map(|input| input.source_frontier)
        .max()
        .expect("temporal fixture requires canonical observations");
    let snapshot_at = inputs
        .iter()
        .map(|input| input.occurrence.knowledge_at.0)
        .max()
        .unwrap_or_default()
        .max(99)
        .saturating_add(1);
    let active_generation = SessionProjectionGenerationV1::new(1).unwrap();
    let candidate_generation = SessionProjectionGenerationV1::new(2).unwrap();
    let cursor_key = SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new("cursor.test").unwrap(),
        version: SessionCursorVersionV1::new(1).unwrap(),
    };
    let temporal = open_test_db_connection(db.db_path()).await;
    temporal
        .execute(
            "INSERT INTO session_query_cursor_keys (
                 key_id, key_version, key_material, created_at, retired_at
             )
             SELECT ?1, 1, ?2, 1, NULL
             WHERE NOT EXISTS (SELECT 1 FROM session_query_cursor_keys)",
            libsql::params![cursor_key.key_id.as_str(), vec![0x45_u8; 32]],
        )
        .await
        .unwrap();
    let watermarks =
        SessionFrozenWatermarksV1::new(active_generation, source_frontier, source_frontier, 0)
            .with_cursor_key(cursor_key);
    let snapshot = SessionTemporalSnapshotV1::new(
        session_id.clone(),
        UtcMicros(snapshot_at),
        watermarks.clone(),
        SessionTemporalCapabilitiesV1::new([
            SessionTemporalCapabilityV1::FrozenWatermarks,
            SessionTemporalCapabilityV1::GenerationRebuild,
        ]),
    );
    let store = GlobalDbSessionTemporalStore::new(db);
    store
        .begin_session_generation_rebuild(
            SessionGenerationRebuildRequestV1::new(
                session_id.clone(),
                candidate_generation,
                snapshot.clone(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    store
        .persist_session_temporal_projection_batch(
            SessionTemporalProjectionBatchV1::new(
                session_id.clone(),
                candidate_generation,
                watermarks,
                inputs.into_iter().map(|input| input.occurrence).collect(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(session_id, candidate_generation, snapshot)
                .unwrap(),
        )
        .await
        .unwrap();
    candidate_generation.value()
}

pub(crate) async fn handle_tool_call(
    cg: &TraceDecay,
    tool_name: &str,
    mut args: serde_json::Value,
    server_stats: Option<serde_json::Value>,
    scope_prefix: Option<&str>,
) -> tracedecay::errors::Result<ToolResult> {
    let owns_format = tracedecay::mcp::tools::tool_defaults_to_markdown(tool_name);
    if !owns_format && let Some(obj) = args.as_object_mut() {
        obj.entry("format".to_string())
            .or_insert_with(|| serde_json::json!("json"));
    }
    // The project-session server path needs the test-transport feature (the
    // in-process MCP harness and the for-test server constructor live behind
    // it); without the feature these tools take the generic path below.
    #[cfg(feature = "test-transport")]
    if matches!(
        tool_name,
        "tracedecay_message_search"
            | "tracedecay_lcm_load_session"
            | "tracedecay_lcm_grep"
            | "tracedecay_lcm_describe"
            | "tracedecay_lcm_expand"
            | "tracedecay_lcm_expand_query"
    ) {
        let session_db_path = project_session_db_path(cg);
        let server = if session_db_path.is_file() {
            let session_db = GlobalDb::open_at(&session_db_path).await.ok_or_else(|| {
                TraceDecayError::Config {
                    message: format!("{tool_name} project session authority is unavailable"),
                }
            })?;
            let server = McpServer::new_with_project_session_db_for_test(
                TraceDecay::open(cg.project_root()).await?,
                None,
                Arc::new(session_db),
            )
            .await;
            if !server.has_project_session_retrieval_service_for_test() {
                return Err(TraceDecayError::Config {
                    message: format!("{tool_name} project retrieval service was not constructed"),
                });
            }
            server
        } else {
            McpServer::new(TraceDecay::open(cg.project_root()).await?, None).await
        };
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args,
            },
        })
        .to_string();
        let response = crate::mcp_server_test::run_server_with_messages(server, vec![request])
            .await
            .into_iter()
            .next()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("{tool_name} returned no MCP response"),
            })?;
        let response: Value =
            serde_json::from_str(&response).map_err(|error| TraceDecayError::Config {
                message: format!("{tool_name} returned invalid MCP JSON: {error}"),
            })?;
        if let Some(error) = response.get("error") {
            return Err(TraceDecayError::Config {
                message: format!("{tool_name} failed over MCP: {error}"),
            });
        }
        return Ok(ToolResult::new(response["result"].clone(), Vec::new()));
    }
    tracedecay::mcp::handle_tool_call(cg, tool_name, args, server_stats, scope_prefix).await
}

pub(crate) async fn index_all_retrying_sync_lock(cg: &TraceDecay) {
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

pub(crate) struct GlobalDbEnvGuard {
    pub(crate) previous: Option<OsString>,
}

impl GlobalDbEnvGuard {
    pub(crate) fn set(db_path: &Path) -> Self {
        let previous = std::env::var_os("TRACEDECAY_GLOBAL_DB");
        let db_path = canonicalize_test_db_path(db_path);
        unsafe {
            std::env::set_var("TRACEDECAY_GLOBAL_DB", db_path);
        }
        Self { previous }
    }
}

impl Drop for GlobalDbEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var("TRACEDECAY_GLOBAL_DB", value),
                None => std::env::remove_var("TRACEDECAY_GLOBAL_DB"),
            }
        }
    }
}

pub(crate) struct HomeEnvGuard {
    pub(crate) previous_home: Option<OsString>,
    pub(crate) previous_userprofile: Option<OsString>,
    pub(crate) previous_data_dir: Option<OsString>,
}

pub(crate) struct TestEnvVarGuard {
    pub(crate) key: &'static str,
    pub(crate) previous: Option<OsString>,
}

impl TestEnvVarGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for TestEnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

impl HomeEnvGuard {
    pub(crate) fn set(home: &Path) -> Self {
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let previous_data_dir = std::env::var_os(tracedecay::config::USER_DATA_DIR_ENV);
        let home = canonicalize_test_dir(home);
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
            std::env::set_var(
                tracedecay::config::USER_DATA_DIR_ENV,
                home.join(tracedecay::config::TRACEDECAY_DIR),
            );
        }
        Self {
            previous_home,
            previous_userprofile,
            previous_data_dir,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        unsafe {
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
}

pub(crate) fn canonicalize_test_dir(path: &Path) -> PathBuf {
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

pub(crate) fn canonicalize_test_db_path(path: &Path) -> PathBuf {
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
pub(crate) struct TestTempDir {
    pub(crate) dir: Option<TempDir>,
}

impl TestTempDir {
    pub(crate) fn new() -> Self {
        Self {
            dir: Some(TempDir::new().unwrap()),
        }
    }
}

impl std::ops::Deref for TestTempDir {
    type Target = TempDir;

    fn deref(&self) -> &Self::Target {
        self.dir.as_ref().expect("test temp dir already kept")
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(dir) = self.dir.take() {
            let _ = dir.keep();
        }
    }
}

pub(crate) fn test_temp_dir() -> TestTempDir {
    TestTempDir::new()
}

pub(crate) struct TestProject {
    pub(crate) dir: Option<TestTempDir>,
    pub(crate) _home_guard: HomeEnvGuard,
    pub(crate) _global_db_guard: GlobalDbEnvGuard,
    // Field order is load-bearing: fields drop in declaration order, so the
    // env lock must be declared last. Releasing it before the guards restore
    // `HOME` / the global DB override would let the next waiting test install
    // its own env, only for these guards to clobber it.
    pub(crate) _env_lock: MutexGuard<'static, ()>,
}

impl std::ops::Deref for TestProject {
    type Target = TempDir;

    fn deref(&self) -> &Self::Target {
        self.dir.as_ref().expect("test project dir already kept")
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = self.dir.take();
    }
}

pub(crate) struct TestEnv {
    pub(crate) _home_guard: HomeEnvGuard,
    pub(crate) _global_db_guard: GlobalDbEnvGuard,
    // Drop order = declaration order: the env lock must outlive the guards
    // above so their env restores happen while the lock is still held.
    pub(crate) _env_lock: MutexGuard<'static, ()>,
}

pub(crate) struct CrossProjectMemoryEnv {
    pub(crate) _dir: TestTempDir,
    pub(crate) _storage_guard: common::TraceDecayStorageEnvGuard,
    // Drop order = declaration order: the env lock must outlive the storage
    // guard above so its env restore happens while the lock is still held.
    pub(crate) _env_lock: MutexGuard<'static, ()>,
}

pub(crate) struct TestTraceDecay {
    pub(crate) inner: Option<TraceDecay>,
}

impl TestTraceDecay {
    pub(crate) fn new(cg: TraceDecay) -> Self {
        Self { inner: Some(cg) }
    }

    pub(crate) async fn close(mut self) {
        if let Some(cg) = self.inner.take() {
            cg.checkpoint().await.unwrap();
            cg.close();
        }
    }

    pub(crate) fn into_inner(mut self) -> TraceDecay {
        self.inner.take().expect("test graph already closed")
    }
}

impl Deref for TestTraceDecay {
    type Target = TraceDecay;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().expect("test graph already closed")
    }
}

impl DerefMut for TestTraceDecay {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().expect("test graph already closed")
    }
}

#[cfg(windows)]
impl Drop for TestTraceDecay {
    fn drop(&mut self) {
        if let Some(cg) = self.inner.take() {
            let close_thread = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test teardown runtime");
                runtime.block_on(async {
                    let _ = cg.checkpoint().await;
                });
                // Windows CI aborts inside libsql/SQLite teardown for these
                // short-lived test graphs. Each nextest case runs in its own
                // process, so leaking the fixture after a checkpoint is safer
                // than exercising the native destructor path at process exit.
                std::mem::forget(cg);
            });
            let _ = close_thread.join();
        }
    }
}

pub(crate) async fn real_mcp_server(cg: TestTraceDecay) -> Arc<McpServer> {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("test project id");
    let project_root = cg.project_root().to_path_buf();
    let registry = Arc::new(GlobalDb::open().await.expect("test registry"));
    registry
        .upsert_code_project(&project_id, &project_root, None, None, None)
        .await
        .expect("register test project");
    McpServer::new_with_dbs(cg.into_inner(), None, None, Some(registry), false).await
}

/// Creates a temporary Rust project with cross-file calls, structs, impls,
/// test files, and doc comments, then initialises and indexes a `TraceDecay`.
pub(crate) async fn setup_project() -> (TestTraceDecay, TestProject) {
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let dir = test_temp_dir();
    let project = dir.path();
    let home = project.join("home");
    let home_guard = HomeEnvGuard::set(&home);
    let global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));

    // Fast path: seed the pre-indexed template store instead of paying
    // schema creation + indexing in every test process.
    let cg = match fixture::open_indexed_project_from_template(project).await {
        Some(cg) => cg,
        None => {
            fixture::write_indexed_fixture_sources(project);
            let cg = TraceDecay::init(project).await.unwrap();
            index_all_retrying_sync_lock(&cg).await;
            cg
        }
    };
    (
        TestTraceDecay::new(cg),
        TestProject {
            dir: Some(dir),
            _env_lock: env_lock,
            _home_guard: home_guard,
            _global_db_guard: global_db_guard,
        },
    )
}

pub(crate) async fn close_test_graph(cg: TestTraceDecay) {
    cg.close().await;
}

pub(crate) async fn init_test_project(project: &Path) -> (TestTraceDecay, TestEnv) {
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = project.join("home");
    let home_guard = HomeEnvGuard::set(&home);
    let global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    let cg = fixture::init_project_from_template(project).await.unwrap();
    (
        TestTraceDecay::new(cg),
        TestEnv {
            _env_lock: env_lock,
            _home_guard: home_guard,
            _global_db_guard: global_db_guard,
        },
    )
}

pub(crate) async fn setup_empty_project() -> (TestTraceDecay, TestEnv, TestTempDir) {
    let dir = test_temp_dir();
    let (cg, env) = init_test_project(dir.path()).await;
    (cg, env, dir)
}

pub(crate) async fn setup_generated_dir_project(
    include_dist: bool,
) -> (TestTraceDecay, TestEnv, TestTempDir) {
    let dir = test_temp_dir();
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

pub(crate) async fn setup_cross_project_memory_projects()
-> (TestTraceDecay, TestTraceDecay, CrossProjectMemoryEnv) {
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let dir = test_temp_dir();
    let storage_guard = common::isolated_tracedecay_storage(&dir);

    let active_project = dir.path().join("active");
    let target_project = dir.path().join("target");
    fs::create_dir_all(active_project.join("src")).unwrap();
    fs::create_dir_all(target_project.join("src")).unwrap();
    fs::write(active_project.join("src/lib.rs"), "pub fn active() {}\n").unwrap();
    fs::write(target_project.join("src/lib.rs"), "pub fn target() {}\n").unwrap();

    let active = fixture::init_project_from_template(&active_project)
        .await
        .unwrap();
    let target = fixture::init_project_from_template(&target_project)
        .await
        .unwrap();

    (
        TestTraceDecay::new(active),
        TestTraceDecay::new(target),
        CrossProjectMemoryEnv {
            _dir: dir,
            _env_lock: env_lock,
            _storage_guard: storage_guard,
        },
    )
}

pub(crate) fn project_data_dir(cg: &TraceDecay) -> PathBuf {
    resolve_layout_for_current_profile(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test project storage layout: {err}"))
        .data_root
}

pub(crate) fn project_graph_db(cg: &TraceDecay) -> PathBuf {
    resolve_layout_for_current_profile(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test project storage layout: {err}"))
        .graph_db_path
}

pub(crate) fn response_handle_dir(cg: &TraceDecay) -> PathBuf {
    resolve_response_handle_root(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test response handle root: {err}"))
}

pub(crate) fn lcm_payload_dir(cg: &TraceDecay) -> PathBuf {
    resolve_lcm_payload_root(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test LCM payload root: {err}"))
}

pub(crate) fn project_session_db_path(cg: &TraceDecay) -> PathBuf {
    cg.store_layout().sessions_db_path.clone()
}

pub(crate) async fn open_active_project_session_db(cg: &TraceDecay) -> GlobalDb {
    GlobalDb::open_at(&project_session_db_path(cg))
        .await
        .expect("active project-local session db should open")
}

/// Creates a small Rust library with an integration-style test that calls a
/// public entry point, which then reaches an internal helper. This exercises
/// the calibrated depth-3 attribution path in `tracedecay_test_risk`.
pub(crate) async fn setup_integration_test_risk_project() -> (TestTraceDecay, TestProject) {
    let dir = test_temp_dir();
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

    let cg = fixture::init_project_from_template(project).await.unwrap();
    cg.index_all().await.unwrap();
    (
        TestTraceDecay::new(cg),
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
pub(crate) async fn setup_test_risk_non_src_fixture() -> (TestTraceDecay, TestProject) {
    let dir = test_temp_dir();
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

    let cg = fixture::init_project_from_template(project).await.unwrap();
    cg.index_all().await.unwrap();
    (
        TestTraceDecay::new(cg),
        TestProject {
            dir: Some(dir),
            _env_lock: env_lock,
            _home_guard: home_guard,
            _global_db_guard: global_db_guard,
        },
    )
}

/// Builds a TypeScript project whose only tests are written with the
/// `describe`/`it` framework style (no `#[test]`-style annotations). Exercises
/// the TS test-attribution path: the `it` callback becomes an executable
/// Function node that calls the source under test, so `tracedecay_test_risk`
/// must attribute the source as directly unit-tested and `tracedecay_test_map`
/// must list the `it` title as the covering test.
pub(crate) async fn setup_ts_describe_it_project() -> (TestTraceDecay, TestProject) {
    let dir = test_temp_dir();
    let project = dir.path();
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = project.join("home");
    let home_guard = HomeEnvGuard::set(&home);
    let global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("package.json"),
        r#"{
  "name": "ts-describe-it-fixture",
  "version": "0.1.0"
}
"#,
    )
    .unwrap();

    // Source under test.
    fs::write(
        project.join("src/math.ts"),
        r#"
export function add(a: number, b: number): number {
    return a + b;
}
"#,
    )
    .unwrap();

    // Test written in describe/it style. The it() callback directly calls add().
    fs::write(
        project.join("src/math.test.ts"),
        r#"
import { add } from "./math";

describe('math', () => {
  it('adds two numbers', () => {
    const result = add(1, 2);
  });
});
"#,
    )
    .unwrap();

    let cg = fixture::init_project_from_template(project).await.unwrap();
    cg.index_all().await.unwrap();
    (
        TestTraceDecay::new(cg),
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
pub(crate) fn extract_text(value: &Value) -> &str {
    value["content"][0]["text"]
        .as_str()
        .unwrap_or("<missing text>")
}

pub(crate) fn extract_json(value: &Value) -> Value {
    serde_json::from_str(extract_text(value)).unwrap()
}

#[cfg(unix)]
pub(crate) fn extract_first_json_content(value: &Value) -> Value {
    value["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                let text = item["text"].as_str()?;
                serde_json::from_str(text).ok()
            })
        })
        .unwrap_or_else(|| panic!("missing JSON content item in {value}"))
}

pub(crate) fn assert_fact_results(payload: &Value, included: &str, excluded: &str, context: &str) {
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

pub(crate) async fn extract_lcm_json_following_handle(cg: &TraceDecay, value: &Value) -> Value {
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

pub(crate) fn expect_tool_error<T>(result: tracedecay::errors::Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected tool call to fail"),
        Err(err) => format!("{err}"),
    }
}

pub(crate) async fn seed_project_registry(db_path: &Path, project_root: &Path) {
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
    db.upsert_project_alias(Path::new("registered-alias"), &project.project_id)
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

/// Searches for `name` via the search handler and returns the first matching
/// node id whose name field equals `name`.
pub(crate) async fn find_node_id(cg: &TraceDecay, name: &str) -> String {
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
pub(crate) fn tool_properties<'a>(
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

pub(crate) async fn seed_lcm_session_message(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    seed_lcm_session_message_for_provider(cg, "cursor", session_id, message_id, text, ordinal)
        .await;
}

pub(crate) async fn seed_lcm_session_message_for_provider(
    cg: &TraceDecay,
    provider: &str,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    let db = open_active_project_session_db(cg).await;
    assert!(
        db.upsert_session(&SessionRecord {
            provider: provider.to_string(),
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
            provider: provider.to_string(),
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

pub(crate) async fn seed_lcm_tool_result_message(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    seed_lcm_tool_result_message_for_provider(cg, "cursor", session_id, message_id, text, ordinal)
        .await;
}

pub(crate) async fn seed_lcm_tool_result_message_for_provider(
    cg: &TraceDecay,
    provider: &str,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    let db = open_active_project_session_db(cg).await;
    assert!(
        db.upsert_session(&SessionRecord {
            provider: provider.to_string(),
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
            provider: provider.to_string(),
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

pub(crate) async fn seed_temporal_lcm_session_message(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) -> TemporalLcmProjectionInput {
    persist_temporal_lcm_observation(
        cg,
        "cursor",
        session_id,
        message_id,
        text.into(),
        CanonicalMessageRoleV1::Assistant,
        ordinal,
        ordinal + 1,
        UtcMicros(ordinal + 1),
    )
    .await
}

pub(crate) async fn seed_temporal_lcm_session_message_with_access(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
    payload_access: PayloadAccessState,
) -> TemporalLcmProjectionInput {
    persist_temporal_lcm_observation_with_access(
        cg,
        "cursor",
        session_id,
        message_id,
        text.into(),
        CanonicalMessageRoleV1::Assistant,
        ordinal,
        ordinal + 1,
        UtcMicros(ordinal + 1),
        payload_access,
    )
    .await
}

pub(crate) async fn seed_temporal_lcm_session_message_for_provider(
    cg: &TraceDecay,
    provider: &str,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) -> TemporalLcmProjectionInput {
    persist_temporal_lcm_observation(
        cg,
        provider,
        session_id,
        message_id,
        text.into(),
        CanonicalMessageRoleV1::Assistant,
        ordinal,
        ordinal + 1,
        UtcMicros(ordinal + 1),
    )
    .await
}

pub(crate) async fn seed_temporal_lcm_session_message_at(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    role: CanonicalMessageRoleV1,
    ordinal: i64,
    timestamp: i64,
) -> TemporalLcmProjectionInput {
    persist_temporal_lcm_observation(
        cg,
        "cursor",
        session_id,
        message_id,
        text.into(),
        role,
        ordinal,
        timestamp,
        UtcMicros(timestamp.saturating_mul(1_000_000)),
    )
    .await
}

pub(crate) async fn seed_temporal_lcm_session_message_at_micros(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    role: CanonicalMessageRoleV1,
    ordinal: i64,
    timestamp: i64,
) -> TemporalLcmProjectionInput {
    persist_temporal_lcm_observation(
        cg,
        "cursor",
        session_id,
        message_id,
        text.into(),
        role,
        ordinal,
        timestamp,
        UtcMicros(timestamp),
    )
    .await
}

pub(crate) async fn seed_temporal_lcm_tool_result_message(
    cg: &TraceDecay,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) -> TemporalLcmProjectionInput {
    let projection = persist_temporal_lcm_observation(
        cg,
        "cursor",
        session_id,
        message_id,
        text.into(),
        CanonicalMessageRoleV1::Tool,
        ordinal,
        ordinal + 1,
        UtcMicros(ordinal + 1),
    )
    .await;
    let db = open_active_project_session_db(cg).await;
    let projected = db
        .get_session_message("cursor", message_id)
        .await
        .expect("canonical tool result must project to the compatibility store");
    assert!(
        db.upsert_session_message(&projected).await,
        "canonical compatibility output must apply the bounded payload policy"
    );
    projection
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_temporal_lcm_observation(
    cg: &TraceDecay,
    provider: &str,
    session_id: &str,
    message_id: &str,
    text: String,
    role: CanonicalMessageRoleV1,
    ordinal: i64,
    message_timestamp: i64,
    ingested_at: UtcMicros,
) -> TemporalLcmProjectionInput {
    persist_temporal_lcm_observation_with_access(
        cg,
        provider,
        session_id,
        message_id,
        text,
        role,
        ordinal,
        message_timestamp,
        ingested_at,
        PayloadAccessState::Eligible,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_temporal_lcm_observation_with_access(
    cg: &TraceDecay,
    provider: &str,
    session_id: &str,
    message_id: &str,
    text: String,
    role: CanonicalMessageRoleV1,
    ordinal: i64,
    message_timestamp: i64,
    ingested_at: UtcMicros,
    payload_access: PayloadAccessState,
) -> TemporalLcmProjectionInput {
    let provider = ProviderId::new(provider).unwrap();
    let session_id = SessionId::new(session_id).unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: ProjectId::new(
            cg.store_layout()
                .identity
                .project_id
                .clone()
                .expect("test project id"),
        )
        .unwrap(),
    };
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let source_frontier = u64::try_from(ordinal).unwrap().saturating_add(1);
    let range = ObservationSourceRangeV1::new(source_frontier - 1, source_frontier).unwrap();
    let stable_record_id =
        ObservationId::new(format!("record.mcp.{session_id}.{message_id}")).unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(ObservationId::new(message_id).unwrap());
    let facts = match role {
        CanonicalMessageRoleV1::Tool => vec![CanonicalObservationFactV1::ToolResult {
            invocation_id: None,
            content: Value::String(text),
            success: Some(true),
        }],
        _ => vec![CanonicalObservationFactV1::Message {
            role,
            content: Value::String(text),
            model: Some("test-model".to_string()),
            timestamp: Some(message_timestamp),
        }],
    };
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        stable_record_id.clone(),
        relations,
        facts,
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.mcp.{session_id}.{message_id}")).unwrap(),
            ComponentVersion::new("sanitizer.mcp-fixture.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            scope,
            ObservationSourceGenerationV1::new(1).unwrap(),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            stable_record_id,
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.mcp-fixture").unwrap(),
        payload,
    )
    .unwrap();
    let db = open_active_project_session_db(cg).await;
    let observation_store = GlobalDbObservationStore::new(&db);
    let previous_cursor = observation_store
        .get_source_cursor(observation.source(), observation.scope())
        .await
        .unwrap();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation.clone(), previous_cursor, next_cursor).unwrap();
    let projection_generation = ProjectionGenerationId::new("projection.mcp-fixture.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(&observation, "observation-capture.v1")
            .unwrap();
    let base_anchor = build_observation_retrieval_anchor_v2(
        &observation,
        projection_generation.clone(),
        ingested_at,
        authorization,
    )
    .unwrap();
    let anchor = RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: base_anchor.target().clone(),
        owner: base_anchor.owner().clone(),
        aliases: base_anchor.aliases().to_vec(),
        occurred_at: base_anchor.occurred_at(),
        ingested_at: base_anchor.ingested_at(),
        evidence_class: base_anchor.evidence_class(),
        source_generation: base_anchor.source_generation().clone(),
        projection_generation: projection_generation.clone(),
        projection_watermark: base_anchor.projection_watermark().clone(),
        coverage: base_anchor.coverage().clone(),
        source_observations: base_anchor.source_observations().to_vec(),
        source_anchors: base_anchor.source_anchors().to_vec(),
        authorization: base_anchor.authorization().clone(),
        payload_access,
        retention_class: base_anchor.retention_class().clone(),
        durability: base_anchor.durability().clone(),
    })
    .unwrap();
    observation_store
        .persist_observation(
            AnchoredObservationWrite::new(write, anchor.clone(), projection_generation).unwrap(),
        )
        .await
        .unwrap();
    observation_store
        .project_observation(observation.observation_id())
        .await
        .unwrap();
    let output_ordinal = ProjectionOutputOrdinalV1::new(0);
    let occurrence = serde_json::from_value(json!({
        "occurrence_id": MessageOccurrenceIdV1::derive(
            observation.observation_id(),
            output_ordinal,
        ),
        "source_observation_id": observation.observation_id(),
        "projection_output_ordinal": output_ordinal,
        "retrieval_anchor_id": derive_exact_observation_anchor_id(
            observation.scope(),
            observation.observation_id(),
        ).unwrap(),
        "session_id": session_id,
        "thread_id": null,
        "thread_grouping": null,
        "turn_id": null,
        "turn_grouping": null,
        "message_id": message_id,
        "agent_id": null,
        "role": role,
        "knowledge_at": ingested_at,
        "valid_time": {"kind": "unknown"},
        "evidence": {
            "authority": "canonical_observation",
            "evidence_class": anchor.evidence_class(),
            "source_anchor_id": anchor.anchor_id(),
            "sanitization_receipt": observation.receipt().receipt(),
        },
    }))
    .unwrap();
    TemporalLcmProjectionInput {
        occurrence,
        source_frontier,
    }
}

pub(crate) async fn seed_lcm_session_message_in_db(
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

pub(crate) async fn project_lcm_conn(cg: &TraceDecay) -> TestDbConnection {
    open_test_db_connection(&project_session_db_path(cg)).await
}

pub(crate) async fn lcm_fts_match_count(cg: &TraceDecay, query: &str) -> i64 {
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

pub(crate) async fn lcm_raw_store_id(cg: &TraceDecay, message_id: &str) -> i64 {
    lcm_raw_store_id_for_provider(cg, "cursor", message_id).await
}

pub(crate) async fn lcm_raw_store_id_for_provider(
    cg: &TraceDecay,
    provider: &str,
    message_id: &str,
) -> i64 {
    let conn = project_lcm_conn(cg).await;
    let mut rows = conn
        .query(
            "SELECT store_id FROM lcm_raw_messages WHERE provider = ?1 AND message_id = ?2",
            libsql::params![provider, message_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

pub(crate) async fn lcm_raw_message_count(cg: &TraceDecay, session_id: &str) -> i64 {
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

pub(crate) async fn lcm_raw_message_count_at_path(db_path: &Path, session_id: &str) -> i64 {
    let conn = open_test_db_connection(db_path).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = ?1",
            libsql::params![session_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

pub(crate) async fn lcm_summary_node_count(cg: &TraceDecay, session_id: &str) -> i64 {
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

pub(crate) async fn lcm_schema_migration_count(cg: &TraceDecay) -> i64 {
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

pub(crate) async fn wipe_lcm_raw_fts(cg: &TraceDecay) {
    project_lcm_conn(cg)
        .await
        .execute_batch("DELETE FROM lcm_raw_messages_fts;")
        .await
        .unwrap();
}

pub(crate) async fn wipe_lcm_raw_fts_for_message(cg: &TraceDecay, message_id: &str) {
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

// ---------------------------------------------------------------------------
// Bug-report regressions: sonium-codebase issues
// ---------------------------------------------------------------------------

/// Regression for bug #1: `tracedecay_body` should prefer the `fn foo()` over
/// a field/variant also named `foo`. Setup mirrors what sonium hit when
/// searching for `gmres`: the codebase has both a `pub fn gmres(...)` and a
/// struct field literally named `gmres`. The function — the body the user
/// actually wants — must outrank the field.
pub(crate) async fn setup_function_vs_field_collision() -> (TestTraceDecay, TestTempDir) {
    let dir = test_temp_dir();
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

// ---------------------------------------------------------------------------
// Store failures must surface as tool errors, not silent empty results
// (cross-cutting audit: silent-empty handlers). Breaking the `edges` table
// out from under the open connection makes every edge query fail while
// node/file queries keep working — exactly the partial-store-failure case
// the old `unwrap_or_default()` calls papered over as "no data".
// ---------------------------------------------------------------------------

/// Renames the `edges` table so every edge query on the open connection
/// fails while node and file queries keep working.
pub(crate) async fn break_edges_table(cg: &TraceDecay) {
    cg.db()
        .execute_write(
            "break edges table fixture",
            "ALTER TABLE edges RENAME TO edges_broken",
            (),
        )
        .await
        .unwrap();
}

/// Builds a crate that plants a needless `unsafe { }` block inside an
/// otherwise-safe function — mirroring the agent-adoption eval fixture's
/// `src/audit.rs::raw_total_len` — so `tracedecay_unsafe_patterns` has a
/// concrete, unambiguous site to surface.
pub(crate) async fn setup_unsafe_block_fixture() -> (TestTraceDecay, TestProject) {
    let dir = test_temp_dir();
    let project = dir.path();
    let env_lock = GLOBAL_DB_ENV_LOCK.lock().await;
    let home = project.join("home");
    let home_guard = HomeEnvGuard::set(&home);
    let global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("Cargo.toml"),
        r#"
[package]
name = "unsafe_fixture"
version = "0.1.0"
edition = "2021"
"#,
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

    let cg = fixture::init_project_from_template(project).await.unwrap();
    cg.index_all().await.unwrap();
    (
        TestTraceDecay::new(cg),
        TestProject {
            dir: Some(dir),
            _env_lock: env_lock,
            _home_guard: home_guard,
            _global_db_guard: global_db_guard,
        },
    )
}
