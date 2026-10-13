use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;
use tempfile::TempDir;
use tracedecay_domain::HostIntegrationIdV1;
use tracedecay_hooks::core_events::DaemonHookEvent;
use tracedecay_host_admission::{
    HostAdmissionBroker, HostAdmissionRuntime, SharedHostAdmissionBroker, SpoolBounds,
};

use crate::mcp::server::{CodeIndexReconcileSink, McpServer, McpServerConstructionContext};
use crate::test_support::git::git;
use tracedecay_project::project::TraceDecay;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;

pub(crate) struct WriterTestFixtureAuthority {
    profile: TempDir,
    runtime: Arc<HostAdmissionTestRuntimeV1>,
}

impl WriterTestFixtureAuthority {
    pub(super) async fn reopen_project_graph(&self, project_root: &Path) -> TraceDecay {
        self.runtime
            .open_project_graph_for_test(
                project_root,
                tracedecay_project::project::TraceDecayOpenOptions {
                    profile_root: Some(self.runtime.profile_root_for_test().to_path_buf()),
                    global_db_path: None,
                },
            )
            .await
            .expect("reopen registered project graph")
    }

    /// The fixture's isolated profile data directory.
    pub(super) fn profile_root(&self) -> &Path {
        self.profile.path()
    }

    /// Releases the retained runtime, the profile's only session-relation
    /// writer, while keeping the profile directory alive, so a test can
    /// reopen the profile the way a fresh process would.
    pub(super) fn release_runtime_for_reopen(self) -> TempDir {
        self.profile
    }
}

/// The registered project runtime the fixture retained at init. The profile
/// session-relation graph has exactly one writer, so tests must share this
/// runtime instead of constructing a second one on the same profile. This is
/// the registry authority the daemon holds in production, so a server built
/// from it resolves path selectors, including hook workspace routes,
/// instead of reporting the project unregistered.
pub(super) fn registered_runtime(
    authority: &WriterTestFixtureAuthority,
) -> Arc<HostAdmissionTestRuntimeV1> {
    Arc::clone(&authority.runtime)
}

/// A registered construction context for `cg`, built from the fixture's
/// retained runtime.
///
/// Pair it with [`crate::mcp::server::McpServer::new_with_registered_test_context`],
/// which adds the retained project-graph resolver.
pub(super) fn registered_context(
    cg: TraceDecay,
    authority: &WriterTestFixtureAuthority,
) -> McpServerConstructionContext {
    let mut context = crate::test_support::host_admission::mcp_server_context_for_test(
        registered_runtime(authority),
        cg,
        None,
    )
    .expect("registered MCP server context");
    // These fixtures assert exact code-index reconcile-sink accounting.
    // Startup catch-up admits one reconciliation through that same sink
    // (its contract is covered by `background_refresh_writer_tests`), so it
    // must not race those counters.
    context.startup_catch_up_enabled = false;
    context
}

pub(crate) async fn init_indexed_repo() -> (TraceDecay, TempDir, WriterTestFixtureAuthority) {
    let profile = TempDir::new().expect("isolated profile");
    let temporary_root = std::env::temp_dir()
        .canonicalize()
        .expect("canonical temporary root");
    let dir = TempDir::new_in(temporary_root).expect("canonical temp repo");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "t@t.com"]);
    git(root, &["config", "user.name", "T"]);
    std::fs::write(root.join(".gitignore"), ".tracedecay/\n").expect("write gitignore");
    std::fs::create_dir_all(root.join("src")).expect("create src");
    std::fs::write(root.join("src/a.rs"), "pub fn a() {}\n").expect("write source");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);
    let (cg, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        profile.path(),
        root,
        "project.mcp-writer",
    )
    .await
    .expect("init");
    (cg, dir, WriterTestFixtureAuthority { profile, runtime })
}

/// A host-admission broker over `spool` with default bounds.
pub(super) fn default_spool_broker(spool: &Path) -> Arc<HostAdmissionBroker> {
    let runtime = HostAdmissionRuntime::open(spool, SpoolBounds::default())
        .unwrap()
        .0;
    Arc::new(HostAdmissionBroker::new(runtime))
}

/// The wire `sessionStart` hook event a Codex host sends from `root`.
pub(super) fn session_start(root: PathBuf) -> Value {
    serde_json::to_value(DaemonHookEvent::session_start(
        HostIntegrationIdV1::Codex,
        root,
    ))
    .unwrap()
}

/// A registered test server for `cg` that admits hooks through `broker`.
pub(super) async fn server_with_broker(
    cg: TraceDecay,
    authority: &WriterTestFixtureAuthority,
    broker: SharedHostAdmissionBroker,
    reconcile_sink: CodeIndexReconcileSink,
) -> Arc<McpServer> {
    let mut context =
        registered_context(cg, authority).with_code_index_reconcile_sink(reconcile_sink);
    context.host_admission_broker = Some(broker);
    McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server")
}

/// A reconcile sink that queues every demand and counts the attempts.
pub(super) fn counting_reconcile_sink(attempts: Arc<AtomicUsize>) -> CodeIndexReconcileSink {
    Arc::new(move |_request, _demand| {
        let attempts = Arc::clone(&attempts);
        Box::pin(async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            super::CodeIndexDemandAdmissionV1::Queued
        })
    })
}
