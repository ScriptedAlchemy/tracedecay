use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;

use crate::application::host_admission::HostAdmissionTestRuntimeV1;
use crate::config::PinnedUserDataDir;
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
    _runtime: Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>,
}

impl WriterTestFixtureAuthority {
    pub(super) async fn reopen_project_graph(&self, project_root: &Path) -> TraceDecay {
        self._runtime
            .open_project_graph_for_test(
                project_root,
                crate::tracedecay::TraceDecayOpenOptions {
                    profile_root: Some(self._runtime.profile_root_for_test().to_path_buf()),
                    global_db_path: None,
                },
            )
            .await
            .expect("reopen registered project graph")
    }
}

/// A registered project runtime for `cg`, rooted at the isolated profile the
/// fixture already pinned. This is the registry authority the daemon holds in
/// production, so a server built from it resolves path selectors — including
/// hook workspace routes — instead of reporting the project unregistered.
pub(super) async fn registered_runtime(cg: &TraceDecay) -> HostAdmissionTestRuntimeV1 {
    let project_id = tracedecay_domain::ProjectId::new(
        cg.store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("project identity"),
    )
    .expect("typed project identity");
    HostAdmissionTestRuntimeV1::project(
        crate::config::user_data_dir().expect("isolated profile root"),
        cg.project_root(),
        project_id,
    )
    .await
    .expect("registered host-admission runtime")
}

/// A registered construction context for `cg`.
///
/// Pair it with [`crate::mcp::server::McpServer::new_with_registered_test_context`],
/// which adds the retained project-graph resolver.
pub(super) async fn registered_context(cg: TraceDecay) -> McpServerConstructionContext {
    registered_runtime(&cg)
        .await
        .into_mcp_server_context_for_test(cg, None)
        .expect("registered MCP server context")
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
    cg.index_all().await.expect("index");
    let mut config = crate::config::load_config(root).expect("load config");
    config.sync.session_start_sync = false;
    crate::config::save_config(root, &config).expect("disable startup sync");
    (
        cg,
        dir,
        WriterTestFixtureAuthority {
            _pin: pin,
            _runtime: runtime,
        },
    )
}
