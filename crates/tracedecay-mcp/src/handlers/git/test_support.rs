//! Repository fixtures, admitted-binding shorthands, and the ref-read
//! serializer shared by this family's tests.

use std::path::Path;
use std::sync::LazyLock;

use crate::tool_context::{McpToolBinding, McpToolContext, RequestControls};

/// The binding a standalone (non-daemon) server produces: a worktree root and
/// no admitted authority at all.
pub(super) fn standalone_context(project_root: &Path) -> McpToolContext<'_> {
    McpToolContext::standalone(project_root).expect("an absolute root binds standalone")
}

/// The same standalone binding with the branch git resolved for the worktree.
pub(super) fn standalone_context_on_branch<'a>(
    project_root: &'a Path,
    active_branch: &'a str,
) -> McpToolContext<'a> {
    McpToolContext::bind(McpToolBinding {
        project_root,
        active_branch: Some(active_branch),
        controls: RequestControls::default(),
        scope: None,
        project_session_store: None,
        code_index: None,
    })
    .expect("an absolute root binds standalone")
}

/// The branch-ref admission semaphore is process-wide, and
/// `branch::tests::branch_ref_route_reports_capacity_without_queueing`
/// deliberately drains it. Every test that performs a real ref read takes
/// this lock first, so a drained semaphore never reaches a sibling test as a
/// spurious capacity failure.
static REF_READ_SERIALIZER: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

pub(super) async fn ref_read_guard() -> tokio::sync::MutexGuard<'static, ()> {
    REF_READ_SERIALIZER.lock().await
}

pub(super) fn test_git(root: &std::path::Path, args: &[&str]) {
    let git = tracedecay_runtime_core::git::try_git_program()
        .expect("absolute git executable should resolve");
    let output = std::process::Command::new(git)
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository whose `feature` branch has one commit past `main`, so a PR
/// comparison and a branch diff both have a real merge base to anchor on.
pub(super) fn branched_repository() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temp repo");
    let root = temp.path();
    test_git(root, &["init", "-b", "main"]);
    std::fs::write(root.join("lib.rs"), "pub fn before() {}\n").expect("write base");
    test_git(root, &["add", "."]);
    test_git(root, &["commit", "-m", "initial"]);
    test_git(root, &["switch", "-c", "feature"]);
    std::fs::write(
        root.join("lib.rs"),
        "pub fn before() {}\npub fn after() {}\n",
    )
    .expect("write change");
    test_git(root, &["add", "."]);
    test_git(root, &["commit", "-m", "change source"]);
    temp
}
