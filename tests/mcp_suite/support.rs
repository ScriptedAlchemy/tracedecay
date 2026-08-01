#![allow(clippy::too_many_arguments, clippy::clone_on_copy)] // test builders
//! Shared fixtures and helpers for the MCP handler test domains.

#[cfg(feature = "test-transport")]
use crate::common;
use crate::fixture;
use serde_json::Value;
#[cfg(feature = "test-transport")]
use serde_json::json;
use std::ffi::OsString;
use std::fs;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
#[cfg(feature = "test-transport")]
use std::process::Command;
#[cfg(feature = "test-transport")]
use std::sync::Arc;
#[cfg(feature = "test-transport")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::{Mutex, MutexGuard};
#[cfg(feature = "test-transport")]
use tracedecay::application::host_admission::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, ProjectScopedTestRuntimeV1,
};
#[cfg(feature = "test-transport")]
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::errors::TraceDecayError;
use tracedecay::mcp::ToolResult;
#[cfg(feature = "test-transport")]
use tracedecay::mcp::{McpServer, McpTransport};
#[cfg(feature = "test-transport")]
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::tracedecay::TraceDecay;
#[cfg(feature = "test-transport")]
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
    SessionId, SessionProjectionGenerationV1, UtcMicros, derive_exact_observation_anchor_id,
};
#[cfg(feature = "test-transport")]
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    SessionFrozenWatermarksV1, SessionGenerationActivationRequestV1,
    SessionGenerationRebuildRequestV1, SessionTemporalCapabilitiesV1, SessionTemporalCapabilityV1,
    SessionTemporalProjectionBatchV1, SessionTemporalProjectionStore, SessionTemporalSnapshotV1,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

pub(crate) static GLOBAL_DB_ENV_LOCK: Mutex<()> = Mutex::const_new(());

pub(crate) const MCP_TEST_RESPONSE_CHAR_LIMIT: usize = 15_000;
#[cfg(feature = "test-transport")]
static NEXT_SOURCE_EDIT_TEST_KEY: AtomicU64 = AtomicU64::new(1);

#[cfg(feature = "test-transport")]
const SOURCE_EDIT_TOOL_NAMES: &[&str] = &[
    "tracedecay_str_replace",
    "tracedecay_multi_str_replace",
    "tracedecay_insert_at",
    "tracedecay_ast_grep_rewrite",
    "tracedecay_replace_symbol",
    "tracedecay_insert_at_symbol",
    "tracedecay_move_symbol",
];

#[cfg(feature = "test-transport")]
#[derive(Default)]
pub(crate) struct CaptureTransport {
    pub(crate) output: String,
}

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
pub(crate) async fn handle_real_server_tool_call(
    server: &McpServer,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let response = handle_real_server_tool_call_raw(server, tool_name, arguments).await;
    assert!(response["error"].is_null(), "{response}");
    response["result"].clone()
}

/// The whole JSON-RPC response, including a protocol-level `error`.
///
/// Handlers that return `Err` are mapped to a JSON-RPC error rather than an
/// `isError` tool result (see `crate::mcp::server::tool_errors`), so tests
/// asserting on infrastructure failures need the envelope, not just `result`.
#[cfg(feature = "test-transport")]
pub(crate) async fn handle_real_server_tool_call_raw(
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
    serde_json::from_str(transport.output.trim()).expect("JSON-RPC response")
}

#[cfg(feature = "test-transport")]
pub(crate) fn extract_real_server_text(result: &Value) -> &str {
    result["content"][0]["text"]
        .as_str()
        .expect("MCP text result")
}

#[cfg(feature = "test-transport")]
pub(crate) struct ProductionCompositionFixture {
    pub(crate) harness: ProductionProjectCompositionHarnessV1,
    pub(crate) project_root: PathBuf,
    _isolation: TestTempDir,
}

#[cfg(feature = "test-transport")]
pub(crate) async fn production_composition_fixture() -> ProductionCompositionFixture {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    fs::create_dir_all(&project_root).expect("production composition project");
    fixture::write_indexed_fixture_sources(&project_root);
    let init = Command::new(common::git_program())
        .args(["init", "-q"])
        .current_dir(&project_root)
        .status()
        .expect("git init");
    assert!(init.success(), "git init must succeed");
    let add = Command::new(common::git_program())
        .args(["add", "."])
        .current_dir(&project_root)
        .status()
        .expect("git add");
    assert!(add.success(), "git add must succeed");
    let commit = Command::new(common::git_program())
        .args([
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "production composition fixture",
        ])
        .current_dir(&project_root)
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit must succeed");
    let harness =
        ProductionProjectCompositionHarnessV1::open(isolation.path(), vec![project_root.clone()])
            .await
            .expect("production composition harness");
    ProductionCompositionFixture {
        harness,
        project_root,
        _isolation: isolation,
    }
}

#[cfg(feature = "test-transport")]
pub(crate) struct TemporalLcmProjectionInput {
    pub(crate) occurrence: MessageOccurrenceRecordV1,
    pub(crate) source_frontier: u64,
}

#[cfg(feature = "test-transport")]
pub(crate) async fn activate_test_temporal_generation(
    runtime: &HostAdmissionTestRuntimeV1,
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
    let cursor_key = runtime
        .ensure_session_cursor_key_for_test(HostAdmissionScope::Project)
        .await
        .expect("registered project cursor key");
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
    let store = runtime
        .session_temporal_store_for_test(HostAdmissionScope::Project)
        .expect("registered project temporal store");
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
    #[cfg(feature = "test-transport")]
    if SOURCE_EDIT_TOOL_NAMES.contains(&tool_name) {
        return handle_project_open_source_edit_tool_call(cg, tool_name, args).await;
    }
    // The project-session server path needs the test-transport feature (the
    // in-process MCP harness and the for-test server constructor live behind
    // it); without the feature these tools take the generic path below.
    #[cfg(feature = "test-transport")]
    if matches!(
        tool_name,
        "tracedecay_message_search" | "tracedecay_session_start" | "tracedecay_session_end"
    ) || tool_name.starts_with("tracedecay_lcm_")
    {
        let session_db_path = project_session_db_path(cg);
        if !session_db_path.is_file() {
            return tracedecay::mcp::handle_tool_call(
                cg,
                tool_name,
                args,
                server_stats,
                scope_prefix,
            )
            .await;
        }
        let runtime = open_active_project_scoped_runtime(cg).await;
        let server = McpServer::new_with_host_admission_test_runtime_for_test(
            TraceDecay::open(cg.project_root()).await?,
            None,
            runtime,
        )
        .await?;
        if !server.has_project_session_retrieval_service_for_test() {
            return Err(TraceDecayError::Config {
                message: format!("{tool_name} project retrieval service was not constructed"),
            });
        }
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

#[cfg(feature = "test-transport")]
pub(crate) async fn handle_tool_call_with_runtime(
    cg: &TraceDecay,
    runtime: &HostAdmissionTestRuntimeV1,
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
    runtime
        .call_mcp_tool_for_test(cg, tool_name, args, server_stats, scope_prefix)
        .await
}

#[cfg(feature = "test-transport")]
async fn handle_project_open_source_edit_tool_call(
    cg: &TraceDecay,
    tool_name: &str,
    mut args: Value,
) -> tracedecay::errors::Result<ToolResult> {
    let graph = TraceDecay::open(cg.project_root()).await?;
    let server = McpServer::new(graph, None).await;
    server
        .install_project_open_source_edit_authority_for_test()
        .await?;

    let dry_run = args.get("dry_run").and_then(Value::as_bool);
    let apply = if tool_name == "tracedecay_move_symbol" {
        dry_run == Some(false)
    } else {
        dry_run != Some(true)
    };
    if apply && (args.get("idempotency_key").is_none() || args.get("expected_state").is_none()) {
        let mut preview_args = args.clone();
        preview_args
            .as_object_mut()
            .expect("source edit arguments are an object")
            .insert("dry_run".to_owned(), Value::Bool(true));
        let preview =
            call_project_open_source_edit_server(&server, tool_name, preview_args).await?;
        let preview_value: Value =
            serde_json::from_str(extract_text(&preview.value)).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("source edit preview returned invalid JSON: {error}"),
                }
            })?;
        let expected_state = preview_value["expected_state"]
            .as_str()
            .ok_or_else(|| TraceDecayError::Config {
                message: "source edit preview returned no expected state".to_owned(),
            })?
            .to_owned();
        let args = args
            .as_object_mut()
            .expect("source edit arguments are an object");
        args.entry("expected_state".to_owned())
            .or_insert(Value::String(expected_state));
        args.entry("idempotency_key".to_owned()).or_insert_with(|| {
            Value::String(format!(
                "mcp-test.source-edit.{}",
                NEXT_SOURCE_EDIT_TEST_KEY.fetch_add(1, Ordering::Relaxed)
            ))
        });
    }
    call_project_open_source_edit_server(&server, tool_name, args).await
}

#[cfg(feature = "test-transport")]
async fn call_project_open_source_edit_server(
    server: &McpServer,
    tool_name: &str,
    arguments: Value,
) -> tracedecay::errors::Result<ToolResult> {
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
        .await?;
    let response: Value =
        serde_json::from_str(transport.output.trim()).map_err(|error| TraceDecayError::Config {
            message: format!("source edit MCP response was invalid JSON: {error}"),
        })?;
    if !response["error"].is_null() {
        return Err(TraceDecayError::Config {
            message: format!("source edit MCP call failed: {}", response["error"]),
        });
    }
    Ok(ToolResult::new(response["result"].clone(), Vec::new()))
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

#[cfg(feature = "test-transport")]
pub(crate) struct TestEnvVarGuard {
    pub(crate) key: &'static str,
    pub(crate) previous: Option<OsString>,
}

#[cfg(feature = "test-transport")]
impl TestEnvVarGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
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
                // Windows CI aborts inside native SQLite teardown for these
                // short-lived test graphs. Each nextest case runs in its own
                // process, so leaking the fixture after a checkpoint is safer
                // than exercising the native destructor path at process exit.
                std::mem::forget(cg);
            });
            let _ = close_thread.join();
        }
    }
}

#[cfg(feature = "test-transport")]
pub(crate) async fn real_mcp_server(cg: TestTraceDecay) -> Arc<McpServer> {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("test project id");
    let project_root = cg.project_root().to_path_buf();
    let runtime = open_active_project_scoped_runtime(&cg).await;
    runtime
        .upsert_code_project(&project_id, &project_root, None, None, None)
        .await
        .expect("register test project");
    McpServer::new_with_host_admission_test_runtime_for_test(cg.into_inner(), None, runtime)
        .await
        .expect("registered test server")
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

#[cfg(feature = "test-transport")]
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

    // Both graphs must be enrolled in *one* profile. A default-option open
    // gives each test project its own standalone test profile, and a project
    // store is keyed by (profile, project), so a selector resolved against the
    // active profile could never reach a store enrolled in another one.
    let shared_profile = tracedecay::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(storage_guard.profile_root().to_path_buf()),
        global_db_path: Some(storage_guard.global_db_path().to_path_buf()),
    };
    let active = TestTraceDecay::new(
        fixture::init_project_from_template_with_options(&active_project, shared_profile.clone())
            .await
            .unwrap(),
    );
    let target = TestTraceDecay::new(
        fixture::init_project_from_template_with_options(&target_project, shared_profile)
            .await
            .unwrap(),
    );

    // A selector resolves against the registry of the runtime serving the
    // call, which is the active project's. Both roots therefore have to be
    // registered there — under the identity their store was opened with —
    // because initializing a graph does not register it, and registering the
    // target through its own runtime lands in a registry no reader consults.
    let registry = active
        .test_runtime_for_test()
        .expect("cross-project fixture active runtime");
    for graph in [&active, &target] {
        let project_id = graph
            .store_layout()
            .identity
            .project_id
            .clone()
            .expect("cross-project fixture project identity");
        registry
            .upsert_code_project(&project_id, graph.project_root(), None, None, Some("main"))
            .await
            .expect("cross-project fixture registers both projects");
    }

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

pub(crate) fn project_data_dir(cg: &TraceDecay) -> PathBuf {
    cg.store_layout().data_root.clone()
}

pub(crate) fn project_graph_db(cg: &TraceDecay) -> PathBuf {
    cg.store_layout().graph_db_path.clone()
}

/// The directory `store_response_handle` actually writes to.
///
/// It resolves the layout for the current profile from the project root rather
/// than reusing the graph's cached layout, so a test that blocks or inspects
/// handle storage has to resolve it the same way or it acts on a path the
/// writer never touches.
pub(crate) fn response_handle_dir(cg: &TraceDecay) -> PathBuf {
    tracedecay::storage::resolve_response_handle_root(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test response handle root: {err}"))
}

#[cfg(feature = "test-transport")]
pub(crate) fn lcm_payload_dir(cg: &TraceDecay) -> PathBuf {
    cg.store_layout().lcm_payload_root.clone()
}

pub(crate) fn project_session_db_path(cg: &TraceDecay) -> PathBuf {
    cg.store_layout().sessions_db_path.clone()
}

#[cfg(feature = "test-transport")]
pub(crate) async fn open_active_project_session_db(
    cg: &TraceDecay,
) -> Arc<HostAdmissionTestRuntimeV1> {
    cg.test_runtime_for_test()
        .expect("test graph should retain its registered project-local session runtime")
}

/// The active graph's retained runtime, promoted to the project scope the MCP
/// test server constructor requires.
#[cfg(feature = "test-transport")]
pub(crate) async fn open_active_project_scoped_runtime(
    cg: &TraceDecay,
) -> ProjectScopedTestRuntimeV1 {
    ProjectScopedTestRuntimeV1::new(open_active_project_session_db(cg).await)
        .expect("active test graph should retain a project-scoped runtime")
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

/// The first content item that parses as JSON.
///
/// The server prepends advisory text blocks (fallback-branch and staleness
/// banners) ahead of a tool's payload, so a test that always reads
/// `content[0]` sees the banner instead of the result.
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

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
pub(crate) async fn seed_project_registry(
    db_path: &Path,
    project_root: &Path,
) -> HostAdmissionTestRuntimeV1 {
    let profile_root = db_path
        .parent()
        .expect("test registry path should have a profile root");
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root)
        .await
        .expect("registered profile test runtime");
    assert_eq!(
        runtime
            .profile_relative_path_for_test(db_path)
            .expect("test registry path must be inside the runtime profile"),
        Path::new("global.db"),
        "test registry path must be the runtime-owned profile database"
    );
    let project = runtime
        .upsert_code_project(
            "proj_alpha",
            project_root,
            None,
            Some("https://token:secret@example.test/alpha.git"),
            Some("main"),
        )
        .await
        .unwrap();
    runtime
        .upsert_project_alias(Path::new("registered-alias"), &project.project_id)
        .await
        .unwrap();
    let store = runtime
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
    runtime
        .upsert_graph_scope(tracedecay::global_db::GraphScopeUpsert {
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
    runtime
        .upsert_store_artifact(tracedecay::global_db::StoreArtifactUpsert {
            store_id: store.store_id,
            artifact_kind: "graph_db".to_string(),
            relpath: "projects/proj_alpha/tracedecay.db".to_string(),
            size_bytes: Some(128),
            schema_version: Some("1".to_string()),
            updated_at: Some(1_800_000_003),
        })
        .await
        .unwrap();
    runtime
        .upsert_code_project(
            "proj_beta",
            &project_root.with_file_name("beta"),
            None,
            Some("https://example.test/beta.git"),
            Some("main"),
        )
        .await
        .unwrap();
    runtime
}

/// Searches the indexed fixture for `name` and returns its exact node id.
pub(crate) async fn find_node_id(cg: &TraceDecay, name: &str) -> String {
    cg.search(name, 10)
        .await
        .unwrap()
        .iter()
        .find(|result| result.node.name == name)
        .unwrap_or_else(|| panic!("node '{name}' not found in indexed fixture"))
        .node
        .id
        .clone()
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

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
pub(crate) async fn seed_lcm_session_message_for_provider(
    cg: &TraceDecay,
    provider: &str,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    let runtime = open_active_project_session_db(cg).await;
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &SessionRecord {
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
                }
            )
            .await
            .unwrap()
    );
    assert!(
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &SessionMessageRecord {
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
                },
            )
            .await
            .unwrap()
    );
}

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
pub(crate) async fn seed_lcm_tool_result_message_for_provider(
    cg: &TraceDecay,
    provider: &str,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    let runtime = open_active_project_session_db(cg).await;
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &SessionRecord {
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
                }
            )
            .await
            .unwrap()
    );
    assert!(
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &SessionMessageRecord {
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
                },
            )
            .await
            .unwrap()
    );
}

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
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

#[cfg(feature = "test-transport")]
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
    let runtime = open_active_project_session_db(cg).await;
    let projected = runtime
        .session_message_for_test(HostAdmissionScope::Project, "cursor", message_id)
        .await
        .unwrap()
        .expect("canonical tool result must project to the compatibility store");
    assert!(
        runtime
            .upsert_session_message_for_test(HostAdmissionScope::Project, &projected)
            .await
            .unwrap(),
        "canonical compatibility output must apply the bounded payload policy"
    );
    projection
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "test-transport")]
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
#[cfg(feature = "test-transport")]
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
    let runtime = open_active_project_session_db(cg).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Project)
        .expect("registered project observation store");
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

#[cfg(feature = "test-transport")]
pub(crate) async fn seed_lcm_session_message_in_db(
    runtime: &HostAdmissionTestRuntimeV1,
    project_path: &Path,
    session_id: &str,
    message_id: &str,
    text: impl Into<String>,
    ordinal: i64,
) {
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &SessionRecord {
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
                }
            )
            .await
            .unwrap()
    );
    assert!(
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &SessionMessageRecord {
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
                },
            )
            .await
            .unwrap()
    );
}

#[cfg(feature = "test-transport")]
pub(crate) async fn project_lcm_conn(cg: &TraceDecay) -> Arc<HostAdmissionTestRuntimeV1> {
    open_active_project_session_db(cg).await
}

#[cfg(feature = "test-transport")]
pub(crate) async fn lcm_fts_match_count(cg: &TraceDecay, query: &str) -> i64 {
    project_lcm_conn(cg)
        .await
        .lcm_raw_message_fts_count_for_test(query)
        .await
        .unwrap()
}

#[cfg(feature = "test-transport")]
pub(crate) async fn lcm_raw_store_id(cg: &TraceDecay, message_id: &str) -> i64 {
    lcm_raw_store_id_for_provider(cg, "cursor", message_id).await
}

#[cfg(feature = "test-transport")]
pub(crate) async fn lcm_raw_store_id_for_provider(
    cg: &TraceDecay,
    provider: &str,
    message_id: &str,
) -> i64 {
    project_lcm_conn(cg)
        .await
        .lcm_load_raw_message_for_test(provider, message_id)
        .await
        .expect("LCM raw message fixture")
        .store_id
}

#[cfg(feature = "test-transport")]
pub(crate) async fn lcm_raw_message_count(cg: &TraceDecay, session_id: &str) -> i64 {
    project_lcm_conn(cg)
        .await
        .lcm_raw_message_count_for_test(HostAdmissionScope::Project, session_id)
        .await
        .unwrap()
}

#[cfg(feature = "test-transport")]
pub(crate) async fn lcm_raw_message_count_at_path(db_path: &Path, session_id: &str) -> i64 {
    let conn = tracedecay_rusqlite_runtime::open_immutable_reader(db_path).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )
    .unwrap()
}

#[cfg(feature = "test-transport")]
pub(crate) async fn wipe_lcm_raw_fts(cg: &TraceDecay) {
    project_lcm_conn(cg)
        .await
        .wipe_lcm_raw_fts_for_test(HostAdmissionScope::Project, None)
        .await
        .unwrap();
}

#[cfg(feature = "test-transport")]
pub(crate) async fn wipe_lcm_raw_fts_for_message(cg: &TraceDecay, message_id: &str) {
    project_lcm_conn(cg)
        .await
        .wipe_lcm_raw_fts_for_test(HostAdmissionScope::Project, Some(message_id))
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
