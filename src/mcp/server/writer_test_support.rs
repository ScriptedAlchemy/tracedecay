use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;

use crate::config::PinnedUserDataDir;
use crate::host_admission::HostAdmissionTestRuntimeV1;
use crate::mcp::server::McpServerConstructionContext;
use crate::tracedecay::TraceDecay;

pub(super) fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new(crate::git::git_program())
        .current_dir(root)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) struct WriterTestFixtureAuthority {
    _pin: PinnedUserDataDir,
    runtime: Arc<HostAdmissionTestRuntimeV1>,
}

impl WriterTestFixtureAuthority {
    pub(super) async fn reopen_project_graph(&self, project_root: &Path) -> TraceDecay {
        self.runtime
            .open_project_graph_for_test(
                project_root,
                crate::tracedecay::TraceDecayOpenOptions {
                    profile_root: Some(self.runtime.profile_root_for_test().to_path_buf()),
                    global_db_path: None,
                },
            )
            .await
            .expect("reopen registered project graph")
    }

    /// Releases the retained runtime — the profile's only session-relation
    /// writer — while keeping the profile pin alive, so a test can reopen the
    /// profile the way a fresh process would.
    pub(super) fn release_runtime_for_reopen(self) -> PinnedUserDataDir {
        self._pin
    }
}

/// The registered project runtime the fixture retained at init. The profile
/// session-relation graph has exactly one writer, so tests must share this
/// runtime instead of constructing a second one on the same profile. This is
/// the registry authority the daemon holds in production, so a server built
/// from it resolves path selectors — including hook workspace routes —
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
    let mut context = registered_runtime(authority)
        .mcp_server_context_for_test(cg, None)
        .expect("registered MCP server context");
    // These fixtures assert exact code-index reconcile-sink accounting.
    // Startup catch-up admits one reconciliation through that same sink
    // (its contract is covered by `background_refresh_writer_tests`), so it
    // must not race those counters.
    context.startup_catch_up_enabled = false;
    context
}

pub(crate) async fn init_indexed_repo() -> (TraceDecay, TempDir, WriterTestFixtureAuthority) {
    let pin = PinnedUserDataDir::new();
    let dir = TempDir::new().expect("temp repo");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "t@t.com"]);
    git(root, &["config", "user.name", "T"]);
    std::fs::write(root.join(".gitignore"), ".tracedecay/\n").expect("write gitignore");
    std::fs::create_dir_all(root.join("src")).expect("create src");
    std::fs::write(root.join("src/a.rs"), "pub fn a() {}\n").expect("write source");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);
    let (cg, runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(root, "project.mcp-writer")
            .await
            .expect("init");
    (cg, dir, WriterTestFixtureAuthority { _pin: pin, runtime })
}
