//! End-to-end lifecycle tests for daemon PR-branch auto-tracking.
//!
//! These drive the real discovery + reconcile path against a fixture repo whose
//! `origin` is a local bare repo carrying `refs/pull/N/head` refs (created with
//! `git update-ref`), exactly the shape GitHub exposes. Same-repo PRs are tracked
//! through the normal branch machinery (fetch → detached worktree →
//! `add_branch_tracking`); fork PRs (a `refs/pull/N/head` whose SHA matches no
//! origin head) are skipped. The tests assert: a PR branch is tracked and its
//! *own* content is indexed, a second poll is a no-op, closing the PR untracks it
//! and cleans the store + worktree, and the per-cycle new-track cap ramps.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use crate::common::IsolatedEnv;
use tracedecay::branch_meta::load_branch_meta;
use tracedecay::daemon::pr_autotrack;
use tracedecay::storage::resolve_layout_for_current_profile;
use tracedecay::tracedecay::TraceDecay;

fn git_out(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(["-c", "core.hooksPath=.git/no-hooks"])
        .args(["-c", "gc.auto=0"])
        .args(["-c", "user.name=TraceDecay Test"])
        .args(["-c", "user.email=tracedecay-test@example.com"])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"))
}

fn git(cwd: &Path, args: &[&str]) {
    let output = git_out(cwd, args);
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn head_sha(cwd: &Path) -> String {
    let out = git_out(cwd, &["rev-parse", "HEAD"]);
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit_all(project: &Path, message: &str) {
    git(project, &["add", "."]);
    git(project, &["commit", "-m", message]);
}

fn project_data_dir(project: &Path) -> PathBuf {
    resolve_layout_for_current_profile(project)
        .unwrap_or_else(|err| panic!("failed to resolve test project storage layout: {err}"))
        .data_root
}

/// Fixture: an indexed project on `main` with a local bare `origin` it has been
/// pushed to. Returns `(env, project, origin_bare)`.
async fn indexed_repo_with_origin() -> (IsolatedEnv, PathBuf, PathBuf) {
    let (env, project) = IsolatedEnv::acquire().await;
    git(&project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
    commit_all(&project, "initial commit");

    let main = TraceDecay::init(&project).await.unwrap();
    main.index_all().await.unwrap();
    drop(main);

    // A sibling bare repo acts as `origin`. Keep it next to the project so it
    // lives as long as the isolated env dir.
    let origin = project.join("..").join("origin.git");
    git(&project, &["init", "--bare", &origin.to_string_lossy()]);
    let origin = origin.canonicalize().unwrap();
    git(
        &project,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&project, &["push", "origin", "main"]);

    (env, project, origin)
}

/// Creates a same-repo PR: a new branch on `origin` with unique content, plus a
/// matching `refs/pull/<n>/head`. Returns the PR head branch name. Leaves the
/// project checked out on `main`.
fn add_same_repo_pr(project: &Path, origin: &Path, n: u64, symbol: &str) -> String {
    let branch = format!("feature-{n}");
    git(project, &["checkout", "-b", &branch, "main"]);
    fs::write(
        project.join(format!("src/pr_{n}.rs")),
        format!("pub fn {symbol}() {{}}\n"),
    )
    .unwrap();
    commit_all(project, &format!("PR {n} content"));
    git(project, &["push", "origin", &branch]);
    // Mirror GitHub's refs/pull/<n>/head at the branch tip.
    git(
        origin,
        &[
            "update-ref",
            &format!("refs/pull/{n}/head"),
            &format!("refs/heads/{branch}"),
        ],
    );
    git(project, &["checkout", "main"]);
    git(project, &["branch", "-D", &branch]);
    branch
}

/// Creates a fork PR: a `refs/pull/<n>/head` on `origin` whose SHA matches no
/// origin head (so discovery classifies it as a fork).
fn add_fork_pr(project: &Path, n: u64, symbol: &str) {
    git(project, &["checkout", "-b", "tmp-fork", "main"]);
    fs::write(
        project.join("src/fork.rs"),
        format!("pub fn {symbol}() {{}}\n"),
    )
    .unwrap();
    commit_all(project, "fork content");
    let sha = head_sha(project);
    git(project, &["checkout", "main"]);
    git(project, &["branch", "-D", "tmp-fork"]);
    fs::remove_file(project.join("src/fork.rs")).ok();
    // Push the bare commit object to origin under the pull ref only — no head.
    git(
        project,
        &["push", "origin", &format!("{sha}:refs/pull/{n}/head")],
    );
}

#[tokio::test]
async fn tracks_same_repo_pr_indexes_its_content_and_untracks_on_close() {
    let (_env, project, origin) = indexed_repo_with_origin().await;
    let head_branch = add_same_repo_pr(&project, &origin, 1, "pr_one_symbol");
    add_fork_pr(&project, 2, "fork_symbol");

    let data_root = project_data_dir(&project);

    // Discovery: PR 1 is a tracked same-repo PR; PR 2 is a skipped fork.
    let discovery = pr_autotrack::discover_open_prs(&project);
    assert_eq!(discovery.open.len(), 1, "one same-repo PR expected");
    assert_eq!(discovery.open[0].number, 1);
    assert_eq!(discovery.open[0].head_branch, head_branch);
    assert_eq!(discovery.skipped_forks, vec![2]);

    // Reconcile → PR 1 tracked.
    let report = pr_autotrack::reconcile_project(&project, &data_root, &discovery, 10).await;
    assert_eq!(report.tracked, vec!["pr/1".to_string()]);
    assert_eq!(report.skipped_forks, vec![2]);

    // Branch metadata + state reflect the tracked PR.
    let meta = load_branch_meta(&data_root).expect("branch meta exists");
    assert!(meta.is_tracked("pr/1"), "pr/1 should be a tracked branch");
    let summary = pr_autotrack::managed_summary(&data_root);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].pr, 1);
    assert_eq!(summary[0].head_branch, head_branch);

    // The PR branch DB indexed the PR's OWN content, not main's working tree.
    let branch_cg = TraceDecay::open_branch(&project, "pr/1").await.unwrap();
    let hits = branch_cg.search("pr_one_symbol", 10).await.unwrap();
    assert!(
        !hits.is_empty(),
        "pr/1 store should contain the PR head's symbol (indexed from its worktree)"
    );
    drop(branch_cg);

    // Idempotent: a second reconcile with the same discovery changes nothing.
    let again = pr_autotrack::reconcile_project(&project, &data_root, &discovery, 10).await;
    assert!(again.tracked.is_empty(), "no re-track on repeat poll");
    assert!(again.untracked.is_empty());
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);

    // Close PR 1 (delete its pull ref) → discovery no longer lists it → untrack.
    git(&origin, &["update-ref", "-d", "refs/pull/1/head"]);
    let after_close = pr_autotrack::discover_open_prs(&project);
    assert!(
        after_close.open.iter().all(|p| p.number != 1),
        "closed PR must not be discovered"
    );
    let closing = pr_autotrack::reconcile_project(&project, &data_root, &after_close, 10).await;
    assert_eq!(closing.untracked, vec!["pr/1".to_string()]);

    let meta = load_branch_meta(&data_root).expect("branch meta exists");
    assert!(!meta.is_tracked("pr/1"), "pr/1 should be untracked");
    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
    assert!(
        !data_root.join("pr-worktrees/pr-1").exists(),
        "worktree should be removed on untrack"
    );
}

#[tokio::test]
async fn caps_new_tracks_per_cycle_and_ramps() {
    let (_env, project, origin) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 1, "pr_one");
    add_same_repo_pr(&project, &origin, 2, "pr_two");
    add_same_repo_pr(&project, &origin, 3, "pr_three");

    let data_root = project_data_dir(&project);
    let discovery = pr_autotrack::discover_open_prs(&project);
    assert_eq!(discovery.open.len(), 3);

    // First cycle with cap=2 tracks only two and flags the cap.
    let first = pr_autotrack::reconcile_project(&project, &data_root, &discovery, 2).await;
    assert_eq!(first.tracked.len(), 2, "cap holds back the third PR");
    assert!(first.capped, "cap flag set when additions are held back");
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 2);

    // Second cycle tracks the remaining PR.
    let second = pr_autotrack::reconcile_project(&project, &data_root, &discovery, 2).await;
    assert_eq!(second.tracked.len(), 1);
    assert!(!second.capped);
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 3);
}
