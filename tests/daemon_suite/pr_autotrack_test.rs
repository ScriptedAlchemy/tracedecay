//! End-to-end lifecycle tests for daemon PR-branch auto-tracking.
//!
//! These drive the real discovery + reconcile path against a fixture repo whose
//! `origin` is a local bare repo carrying `refs/pull/N/head` refs (created with
//! `git update-ref`), exactly the shape GitHub exposes. Same-repo PRs are tracked
//! through the normal branch machinery (fetch → owned synthetic worktree →
//! `add_branch_tracking`); fork PRs (a `refs/pull/N/head` whose SHA matches no
//! origin head) are skipped. The tests assert: a PR branch is tracked and its
//! *own* content is indexed, a second poll is a no-op, closing the PR untracks it
//! and cleans the store + worktree, and the per-cycle new-track cap ramps.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use crate::common::IsolatedEnv;
use fs2::FileExt;
use tracedecay::branch_meta::{load_branch_meta, save_branch_meta};
use tracedecay::daemon::pr_autotrack;
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
    git(
        project,
        &["add", "--all", "--", ".", ":(exclude).tracedecay"],
    );
    git(project, &["commit", "-m", message]);
}

fn project_data_dir(graph: &TraceDecay) -> PathBuf {
    graph.store_layout().data_root.clone()
}

fn seed_branch_only_fact(database_path: &Path, content: &str) -> i64 {
    let connection = rusqlite::Connection::open(database_path).unwrap();
    connection
        .execute(
            "INSERT INTO memory_facts(
                 content, category, tags, trust_score, access_count,
                 created_at, updated_at, source, metadata, hrr_precision
             ) VALUES(?1, 'project', '[\"branch-cutover\"]', 0.9, 1,
                      42, 42, 'legacy-pr-branch', '{\"fixture\":\"production-pr-autotrack\"}',
                      'f32')",
            [content],
        )
        .unwrap();
    connection.last_insert_rowid()
}

/// Fixture: an indexed project on `main` with a local bare `origin` it has been
/// pushed to. Returns `(env, project, origin_bare, retained_graph)`.
async fn indexed_repo_with_origin() -> (IsolatedEnv, PathBuf, PathBuf, Arc<TraceDecay>) {
    let (env, project) = IsolatedEnv::acquire().await;
    git(&project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
    commit_all(&project, "initial commit");

    let main = Arc::new(TraceDecay::init(&project).await.unwrap());
    main.index_all().await.unwrap();

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

    (env, project, origin, main)
}

async fn reconcile(
    graph: &Arc<TraceDecay>,
    project: &Path,
    data_root: &Path,
    discovery: &pr_autotrack::PrDiscovery,
    cap: usize,
) -> pr_autotrack::ReconcileReport {
    pr_autotrack::reconcile_project(Arc::clone(graph), project, data_root, discovery, cap).await
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

/// Advances an existing same-repo PR's head: adds a commit carrying `symbol` on
/// top of `origin/feature-<n>`, pushes it, and re-points `refs/pull/<n>/head` at
/// the new tip. Leaves the project checked out on `main`.
fn bump_pr(project: &Path, origin: &Path, n: u64, symbol: &str) {
    let branch = format!("feature-{n}");
    git(
        project,
        &["checkout", "-B", &branch, &format!("origin/{branch}")],
    );
    fs::write(
        project.join(format!("src/pr_{n}_{symbol}.rs")),
        format!("pub fn {symbol}() {{}}\n"),
    )
    .unwrap();
    commit_all(project, &format!("bump PR {n}"));
    git(project, &["push", "origin", &branch]);
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
}

#[tokio::test]
async fn production_reconciliation_publishes_managed_pr_ref() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 1, "managed_ref_symbol");
    let data_root = project_data_dir(&graph);
    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");

    let report = reconcile(&graph, &project, &data_root, &discovery, 10).await;

    assert_eq!(report.tracked, vec!["tracedecay/autotrack/pr/1".to_owned()]);
    assert!(
        git_out(&project, &["rev-parse", "--verify", "refs/tracedecay/pr/1"])
            .status
            .success()
    );
}

#[tokio::test]
async fn tracks_same_repo_pr_indexes_its_content_and_untracks_on_close() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    let branch_only_content = "PR branch retirement preserves this project-memory fact identity";
    let head_branch = add_same_repo_pr(&project, &origin, 1, "pr_one_symbol");
    add_fork_pr(&project, 2, "fork_symbol");
    git(&project, &["branch", "pr/1", "main"]);
    let user_branch_sha =
        String::from_utf8(git_out(&project, &["rev-parse", "refs/heads/pr/1"]).stdout)
            .unwrap()
            .trim()
            .to_string();

    let data_root = project_data_dir(&graph);
    let tracking_label = "tracedecay/autotrack/pr/1";

    // Discovery: PR 1 is a tracked same-repo PR; PR 2 is a skipped fork.
    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    assert_eq!(discovery.open.len(), 1, "one same-repo PR expected");
    assert_eq!(discovery.open[0].number, 1);
    assert_eq!(discovery.open[0].head_branch, head_branch);
    assert!(!discovery.open[0].head_sha.is_empty());
    assert_eq!(discovery.skipped_forks, vec![2]);

    // Reconcile → PR 1 tracked.
    let report =
        pr_autotrack::reconcile_project(Arc::clone(&graph), &project, &data_root, &discovery, 10)
            .await;
    assert_eq!(report.tracked, vec![tracking_label.to_string()]);
    assert_eq!(report.skipped_forks, vec![2]);

    // Branch metadata + state reflect the tracked PR.
    let meta = load_branch_meta(&data_root).expect("branch meta exists");
    assert!(
        meta.is_tracked(tracking_label),
        "the internal PR ref should be a tracked branch"
    );
    let summary = pr_autotrack::managed_summary(&data_root);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].pr, 1);
    assert_eq!(summary[0].head_branch, head_branch);
    let user_branch_after_track =
        String::from_utf8(git_out(&project, &["rev-parse", "refs/heads/pr/1"]).stdout)
            .unwrap()
            .trim()
            .to_string();
    assert_eq!(user_branch_after_track, user_branch_sha);

    // Seed a legacy-only row into the real production-created branch database,
    // matching the checked-in pre-cutover fixtures. It exists nowhere in the
    // project store before the head-update cleanup.
    let tracked_entry = meta.branches.get(tracking_label).unwrap();
    let tracked_database = data_root.join(&tracked_entry.db_file);
    assert_eq!(
        rusqlite::Connection::open_with_flags(
            &tracked_database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'pr_one_symbol'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1,
        "pr/1 store should contain the PR head's symbol (indexed from its worktree)"
    );
    let branch_fact_id = seed_branch_only_fact(&tracked_database, branch_only_content);
    let branch_fact = rusqlite::Connection::open_with_flags(
        &tracked_database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT fact_id, content FROM memory_facts WHERE content = ?1",
        [branch_only_content],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )
    .unwrap();
    assert_eq!(
        branch_fact,
        (branch_fact_id, branch_only_content.to_owned())
    );
    assert!(
        graph
            .list_facts(None, Some(0.0), 100)
            .await
            .unwrap()
            .iter()
            .all(|fact| fact.content != branch_only_content)
    );

    // Idempotent: a second reconcile with the same discovery changes nothing.
    let again =
        pr_autotrack::reconcile_project(Arc::clone(&graph), &project, &data_root, &discovery, 10)
            .await;
    assert!(again.tracked.is_empty(), "no re-track on repeat poll");
    assert!(again.untracked.is_empty());
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);

    // Advance the remote PR head. Reconciliation must rebuild and sync the
    // managed graph instead of serving the previous commit indefinitely.
    git(
        &project,
        &[
            "checkout",
            "-b",
            &head_branch,
            &format!("origin/{head_branch}"),
        ],
    );
    fs::write(
        project.join("src/pr_1_updated.rs"),
        "pub fn pr_one_updated_symbol() {}\n",
    )
    .unwrap();
    commit_all(&project, "update PR 1 content");
    git(&project, &["push", "origin", &head_branch]);
    git(
        &origin,
        &[
            "update-ref",
            "refs/pull/1/head",
            &format!("refs/heads/{head_branch}"),
        ],
    );
    git(&project, &["checkout", "main"]);
    git(&project, &["branch", "-D", &head_branch]);

    let refreshed_discovery =
        pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    assert_ne!(
        refreshed_discovery.open[0].head_sha,
        discovery.open[0].head_sha
    );
    let refreshed = pr_autotrack::reconcile_project(
        Arc::clone(&graph),
        &project,
        &data_root,
        &refreshed_discovery,
        10,
    )
    .await;
    assert_eq!(refreshed.tracked, vec![tracking_label.to_string()]);
    let refreshed_database =
        data_root.join(&load_branch_meta(&data_root).unwrap().branches[tracking_label].db_file);
    assert_eq!(
        rusqlite::Connection::open_with_flags(
            refreshed_database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'pr_one_updated_symbol'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1,
        "changed PR head must replace the stale branch graph",
    );
    let restored = graph
        .list_facts(None, Some(0.0), 100)
        .await
        .unwrap()
        .into_iter()
        .find(|fact| fact.content == branch_only_content)
        .expect("head refresh must cut branch-only memory over before retirement");
    assert_eq!(restored.content, branch_only_content);
    let restored_fact_id = restored.fact_id;

    // Close PR 1 (delete its pull ref) → discovery no longer lists it → untrack.
    let canonical_branch_paths = load_branch_meta(&data_root)
        .unwrap()
        .branches
        .values()
        .filter(|entry| entry.db_file.starts_with("branches/"))
        .map(|entry| entry.db_file.clone())
        .collect::<std::collections::BTreeSet<_>>();
    git(&origin, &["update-ref", "-d", "refs/pull/1/head"]);
    let after_close = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    assert!(
        after_close.open.iter().all(|p| p.number != 1),
        "closed PR must not be discovered"
    );
    let closing =
        pr_autotrack::reconcile_project(Arc::clone(&graph), &project, &data_root, &after_close, 10)
            .await;
    assert_eq!(closing.untracked, vec![tracking_label.to_string()]);

    let meta = load_branch_meta(&data_root).expect("branch meta exists");
    assert!(!meta.is_tracked(tracking_label));
    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
    assert!(
        !data_root.join("pr-worktrees/pr-1").exists(),
        "worktree should be removed on untrack"
    );
    let user_branch_after_close =
        String::from_utf8(git_out(&project, &["rev-parse", "refs/heads/pr/1"]).stdout)
            .unwrap()
            .trim()
            .to_string();
    assert_eq!(user_branch_after_close, user_branch_sha);
    let retired_fact = graph
        .get_fact(restored_fact_id)
        .await
        .unwrap()
        .expect("branch retirement must preserve the fact's canonical identity");
    assert_eq!(retired_fact.content, branch_only_content);

    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(data_root.join("memory-branch-cutover.json")).unwrap())
            .unwrap();
    let covered = receipt["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            assert!(
                source["generation"]
                    .as_str()
                    .is_some_and(|generation| generation.starts_with("sha256:"))
            );
            source["relative_path"].as_str().unwrap().to_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        covered, canonical_branch_paths,
        "the production receipt must cover every canonical branch family"
    );
}

/// A contended coordinator removal must leave every owned artifact in place so
/// the following poll can retry safely instead of serving an untracked state
/// whose worktree/ref was already deleted.
#[tokio::test]
async fn busy_untrack_retains_managed_state_until_a_later_poll_succeeds() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    let retry_content = "Retried PR cleanup merges this fact exactly once";
    add_same_repo_pr(&project, &origin, 6, "pr_six_symbol");
    let data_root = project_data_dir(&graph);
    let label = "tracedecay/autotrack/pr/6";
    let worktree = data_root.join("pr-worktrees/pr-6");
    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let tracked =
        pr_autotrack::reconcile_project(Arc::clone(&graph), &project, &data_root, &discovery, 10)
            .await;
    assert_eq!(tracked.tracked, vec![label.to_string()]);
    let branch_entry = load_branch_meta(&data_root).unwrap().branches[label].clone();
    seed_branch_only_fact(&data_root.join(branch_entry.db_file), retry_content);

    // The branch-administration coordinator takes this same metadata lock. It
    // must fail closed while another mutation owns it, not delete Git artifacts
    // after an unsuccessful store removal.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(data_root.join(".branch-add.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();

    let busy = pr_autotrack::reconcile_project(
        Arc::clone(&graph),
        &project,
        &data_root,
        &pr_autotrack::PrDiscovery::default(),
        10,
    )
    .await;
    assert!(busy.untracked.is_empty());
    assert!(
        busy.failures.iter().any(|(branch, _)| branch == label),
        "the failed coordinator removal must be reported"
    );
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
    assert!(load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(worktree.exists(), "busy removal must retain its worktree");
    assert!(
        git_out(
            &project,
            &[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/6",
            ],
        )
        .status
        .success(),
        "busy removal must retain its synthetic branch"
    );
    assert!(
        git_out(&project, &["rev-parse", "--verify", "refs/tracedecay/pr/6"])
            .status
            .success(),
        "busy removal must retain its owned fetch ref"
    );
    let restored_on_busy = graph
        .list_facts(None, Some(0.0), 100)
        .await
        .unwrap()
        .into_iter()
        .find(|fact| fact.content == retry_content)
        .expect("the cutover completes before fail-closed branch cleanup");

    lock.unlock().unwrap();

    let retried = pr_autotrack::reconcile_project(
        Arc::clone(&graph),
        &project,
        &data_root,
        &pr_autotrack::PrDiscovery::default(),
        10,
    )
    .await;
    assert_eq!(retried.untracked, vec![label.to_string()]);
    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
    assert!(!load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(!worktree.exists(), "successful retry removes the worktree");
    assert!(
        !git_out(
            &project,
            &[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/6",
            ],
        )
        .status
        .success(),
        "successful retry removes the synthetic branch"
    );
    assert!(
        !git_out(&project, &["rev-parse", "--verify", "refs/tracedecay/pr/6"])
            .status
            .success(),
        "successful retry removes the owned fetch ref"
    );
    let matching_facts = graph
        .list_facts(None, Some(0.0), 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|fact| fact.content == retry_content)
        .collect::<Vec<_>>();
    assert_eq!(
        matching_facts.len(),
        1,
        "cutover retries must be idempotent"
    );
    assert_eq!(matching_facts[0].fact_id, restored_on_busy.fact_id);
}

#[tokio::test]
async fn deferred_tracking_is_not_persisted_and_retries_next_cycle() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 7, "pr_seven_symbol");
    let data_root = project_data_dir(&graph);
    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");

    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(data_root.join(".branch-add.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();

    let deferred =
        pr_autotrack::reconcile_project(Arc::clone(&graph), &project, &data_root, &discovery, 10)
            .await;
    assert!(deferred.tracked.is_empty());
    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
    assert!(
        data_root.join("pr-worktrees/pr-7").exists(),
        "contended rollback must retain its worktree for the next poll"
    );

    lock.unlock().unwrap();
    let retried =
        pr_autotrack::reconcile_project(Arc::clone(&graph), &project, &data_root, &discovery, 10)
            .await;
    assert_eq!(
        retried.tracked,
        vec!["tracedecay/autotrack/pr/7".to_string()]
    );
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
}

#[tokio::test]
async fn complete_orphan_is_rebuilt_after_interrupted_state_write() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 8, "pr_eight_symbol");
    let data_root = project_data_dir(&graph);
    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let label = "tracedecay/autotrack/pr/8";

    let first =
        pr_autotrack::reconcile_project(Arc::clone(&graph), &project, &data_root, &discovery, 10)
            .await;
    assert_eq!(first.tracked, vec![label.to_string()]);
    fs::remove_file(data_root.join("pr-autotrack.json")).unwrap();

    let recovered = reconcile(&graph, &project, &data_root, &discovery, 10).await;
    assert_eq!(recovered.tracked, vec![label.to_string()]);
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
    assert!(load_branch_meta(&data_root).unwrap().is_tracked(label));
}

#[tokio::test]
async fn validated_branch_ref_orphan_without_worktree_is_rebuilt() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 11, "pr_eleven_symbol");
    let data_root = project_data_dir(&graph);
    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let label = "tracedecay/autotrack/pr/11";
    let worktree = data_root.join("pr-worktrees/pr-11");

    let first = reconcile(&graph, &project, &data_root, &discovery, 10).await;
    assert_eq!(first.tracked, vec![label.to_string()]);
    fs::remove_file(data_root.join("pr-autotrack.json")).unwrap();
    let mut meta = load_branch_meta(&data_root).unwrap();
    let entry = meta.remove_branch(label).unwrap();
    save_branch_meta(&data_root, &meta).unwrap();
    fs::rename(
        data_root.join(entry.db_file),
        data_root.join("interrupted-branch-store.db"),
    )
    .unwrap();
    git(
        &project,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );

    let recovered = reconcile(&graph, &project, &data_root, &discovery, 10).await;
    assert_eq!(recovered.tracked, vec![label.to_string()]);
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
    assert!(worktree.exists());
}

#[tokio::test]
async fn state_persistence_failure_rolls_back_completed_tracking() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 10, "pr_ten_symbol");
    let data_root = project_data_dir(&graph);
    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let label = "tracedecay/autotrack/pr/10";
    fs::create_dir(data_root.join("pr-autotrack.json")).unwrap();

    let report = reconcile(&graph, &project, &data_root, &discovery, 10).await;
    assert!(report.tracked.is_empty());
    assert!(
        report
            .failures
            .iter()
            .any(|(branch, reason)| branch == label && reason.contains("persist"))
    );
    assert!(!load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(!data_root.join("pr-worktrees/pr-10").exists());
    assert!(
        !git_out(
            &project,
            &[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/10"
            ],
        )
        .status
        .success()
    );
}

#[tokio::test]
async fn legacy_close_recovers_empty_head_sha_before_owned_ref_cleanup() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 9, "pr_nine_symbol");
    let data_root = project_data_dir(&graph);
    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let current_label = "tracedecay/autotrack/pr/9";
    let legacy_label = "pr/9";
    let first = reconcile(&graph, &project, &data_root, &discovery, 10).await;
    assert_eq!(first.tracked, vec![current_label.to_string()]);

    let mut state = pr_autotrack::load_state(&data_root);
    let mut managed = state.managed.remove(current_label).unwrap();
    managed.head_sha.clear();
    state.managed.insert(legacy_label.to_string(), managed);
    fs::write(
        data_root.join("pr-autotrack.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();

    let mut meta = load_branch_meta(&data_root).unwrap();
    let entry = meta.remove_branch(current_label).unwrap();
    let parent = entry.parent.as_deref().unwrap_or("main");
    meta.add_branch(legacy_label, &entry.db_file, parent);
    save_branch_meta(&data_root, &meta).unwrap();
    let worktree = data_root.join("pr-worktrees/pr-9");
    git(&worktree, &["branch", "-m", legacy_label]);

    git(&origin, &["update-ref", "-d", "refs/pull/9/head"]);
    let closed = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let report = reconcile(&graph, &project, &data_root, &closed, 10).await;
    assert_eq!(report.untracked, vec![legacy_label.to_string()]);
    assert!(
        !load_branch_meta(&data_root)
            .unwrap()
            .is_tracked(legacy_label)
    );
    assert!(!worktree.exists());
    assert!(
        !git_out(&project, &["rev-parse", "--verify", "refs/tracedecay/pr/9"],)
            .status
            .success(),
        "owned fetch ref must be removed after recovering its legacy SHA"
    );
    assert!(
        git_out(&project, &["rev-parse", "--verify", "refs/heads/pr/9"])
            .status
            .success(),
        "ambiguous legacy local branch must be preserved"
    );
}

#[tokio::test]
async fn caps_new_tracks_per_cycle_and_ramps() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 1, "pr_one");
    add_same_repo_pr(&project, &origin, 2, "pr_two");
    add_same_repo_pr(&project, &origin, 3, "pr_three");

    let data_root = project_data_dir(&graph);
    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    assert_eq!(discovery.open.len(), 3);

    // First cycle with cap=2 tracks only two and flags the cap.
    let first = reconcile(&graph, &project, &data_root, &discovery, 2).await;
    assert_eq!(first.tracked.len(), 2, "cap holds back the third PR");
    assert!(first.capped, "cap flag set when additions are held back");
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 2);

    // Second cycle tracks the remaining PR.
    let second = reconcile(&graph, &project, &data_root, &discovery, 2).await;
    assert_eq!(second.tracked.len(), 1);
    assert!(!second.capped);
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 3);
}

/// Finding 1: a *failed* discovery command must be distinguishable from "zero
/// open PRs". If `discover_open_prs` collapsed a git/gh failure into an empty
/// discovery, reconcile would mass-untrack every managed PR on one transient
/// network/credential blip. It must return `Err` instead so the daemon skips
/// the cycle and leaves the managed set intact.
#[tokio::test]
async fn failed_discovery_returns_error_and_never_mass_untracks() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 1, "pr_one_symbol");
    let data_root = project_data_dir(&graph);
    let label = "tracedecay/autotrack/pr/1";

    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let report = reconcile(&graph, &project, &data_root, &discovery, 10).await;
    assert_eq!(report.tracked, vec![label.to_string()]);

    // Break `origin` so every ls-remote/gh discovery command fails.
    git(
        &project,
        &["remote", "set-url", "origin", "/definitely/not/a/repo.git"],
    );

    let result = pr_autotrack::discover_open_prs(&project);
    assert!(
        result.is_err(),
        "a failed discovery command must surface as Err, not an empty Ok"
    );
    // The daemon skips reconcile on Err, so the managed PR is untouched.
    assert_eq!(pr_autotrack::managed_summary(&data_root).len(), 1);
    assert!(load_branch_meta(&data_root).unwrap().is_tracked(label));
}

/// Finding 2: daemon death mid-track leaves the synthetic branch at an old SHA
/// with no state and no DB. Once the PR head advances, the old `-b` path wedged
/// forever on "branch already exists" (and leaked the worktree). Tracking must
/// now be idempotent: adopt/reset the orphan branch to the new head and recover.
#[tokio::test]
async fn interrupted_track_with_advanced_head_recovers_instead_of_wedging() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 12, "pr_twelve_v1");
    let data_root = project_data_dir(&graph);
    let label = "tracedecay/autotrack/pr/12";
    let worktree = data_root.join("pr-worktrees/pr-12");

    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let first = reconcile(&graph, &project, &data_root, &discovery, 10).await;
    assert_eq!(first.tracked, vec![label.to_string()]);

    // Reproduce the mid-track crash aftermath: state gone, DB gone, worktree
    // gone — but the synthetic branch survives pointing at the OLD head.
    fs::remove_file(data_root.join("pr-autotrack.json")).unwrap();
    let mut meta = load_branch_meta(&data_root).unwrap();
    let entry = meta.remove_branch(label).unwrap();
    save_branch_meta(&data_root, &meta).unwrap();
    fs::rename(
        data_root.join(entry.db_file),
        data_root.join("interrupted-branch-store.db"),
    )
    .unwrap();
    git(
        &project,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );
    assert!(
        git_out(
            &project,
            &[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/12"
            ],
        )
        .status
        .success(),
        "orphan synthetic branch must survive to reproduce the wedge"
    );

    // Advance the PR head so the orphan branch (old SHA) no longer matches.
    bump_pr(&project, &origin, 12, "pr_twelve_v2");
    let refreshed = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    assert_ne!(refreshed.open[0].head_sha, discovery.open[0].head_sha);

    let recovered = reconcile(&graph, &project, &data_root, &refreshed, 10).await;
    assert_eq!(
        recovered.tracked,
        vec![label.to_string()],
        "an interrupted track with an advanced head must recover, not wedge"
    );
    assert!(worktree.exists(), "recovery re-creates the worktree");
    let cg = TraceDecay::open_branch(&project, label).await.unwrap();
    assert!(
        !cg.search("pr_twelve_v2", 10).await.unwrap().is_empty(),
        "recovered branch graph serves the advanced head's content"
    );
}

/// Finding 4: the per-cycle new-track cap must bound only NEW tracks — an
/// already-managed PR whose head advanced must still be refreshed the same
/// cycle, even when its label sorts after the capped new PRs. (Managed PR 9
/// sorts lexically after new PRs 10/11/12.)
#[tokio::test]
async fn cap_does_not_starve_head_updates_for_managed_prs() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 9, "pr_nine_v1");
    let data_root = project_data_dir(&graph);
    let label = "tracedecay/autotrack/pr/9";

    let d0 = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let r0 = reconcile(&graph, &project, &data_root, &d0, 10).await;
    assert_eq!(r0.tracked, vec![label.to_string()]);

    // Open three NEW PRs (cap will be 2) and advance managed PR 9's head.
    add_same_repo_pr(&project, &origin, 10, "pr_ten");
    add_same_repo_pr(&project, &origin, 11, "pr_eleven");
    add_same_repo_pr(&project, &origin, 12, "pr_twelve");
    bump_pr(&project, &origin, 9, "pr_nine_v2");

    let d1 = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let r1 = reconcile(&graph, &project, &data_root, &d1, 2).await;

    assert!(r1.capped, "three new PRs under cap=2 flags the cap");
    assert!(
        r1.tracked.contains(&label.to_string()),
        "managed PR 9's head refresh must run despite the new-track cap"
    );
    let new_tracked = r1.tracked.iter().filter(|l| l.as_str() != label).count();
    assert_eq!(new_tracked, 2, "cap still bounds NEW tracks to 2");

    let cg = TraceDecay::open_branch(&project, label).await.unwrap();
    assert!(
        !cg.search("pr_nine_v2", 10).await.unwrap().is_empty(),
        "the refreshed managed branch serves the advanced head, not the stale one"
    );
}
/// Finding 7: turning the feature off must tear down managed PR state rather
/// than stranding worktrees, refs, synthetic branches and stores forever.
#[tokio::test]
async fn teardown_removes_all_managed_state_on_disable() {
    let (_env, project, origin, graph) = indexed_repo_with_origin().await;
    add_same_repo_pr(&project, &origin, 4, "pr_four_symbol");
    let data_root = project_data_dir(&graph);
    let label = "tracedecay/autotrack/pr/4";

    let discovery = pr_autotrack::discover_open_prs(&project).expect("PR discovery succeeds");
    let report = reconcile(&graph, &project, &data_root, &discovery, 10).await;
    assert_eq!(report.tracked, vec![label.to_string()]);

    // The daemon runs this when it observes the feature disabled.
    pr_autotrack::teardown_disabled_project(Arc::clone(&graph), &project).await;

    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
    assert!(!load_branch_meta(&data_root).unwrap().is_tracked(label));
    assert!(!data_root.join("pr-worktrees/pr-4").exists());
    assert!(
        !git_out(
            &project,
            &[
                "rev-parse",
                "--verify",
                "refs/heads/tracedecay/autotrack/pr/4"
            ],
        )
        .status
        .success(),
        "synthetic branch must be removed on teardown"
    );
    assert!(
        !git_out(&project, &["rev-parse", "--verify", "refs/tracedecay/pr/4"])
            .status
            .success(),
        "owned fetch ref must be removed on teardown"
    );

    // Idempotent: a second teardown with nothing managed is a no-op.
    pr_autotrack::teardown_disabled_project(Arc::clone(&graph), &project).await;
    assert!(pr_autotrack::managed_summary(&data_root).is_empty());
}
