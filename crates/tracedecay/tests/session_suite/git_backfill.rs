//! Integration coverage for historical session↔git convergence
//! (`tracedecay sessions git-sync`).
//!
//! Seeds a `sessions.db` with two sessions — one spanning a mid-session branch
//! switch — against a real git repo carrying commits on both branches, runs
//! the backfill core directly (no binary spawn), and asserts `sessions_for`
//! returns the expected branch/commit attribution, including the branch-switch
//! case. A fake [`GitReflogSource`] supplies the branch timeline so the switch
//! lands deterministically relative to each session's activity window, while
//! commit times come from the real repo.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::ProjectId;
use tracedecay_sessions::runtime::git_correlation::{
    BackfillOptions, BranchTimelineEntry, CommitRelationFilter, GitRefFilter, GitReflogSource,
    SessionsForQuery, normalize_worktree,
};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
use tracedecay_usecases::host_admission::HostAdmissionScope;

use crate::common;

const T_BASE: i64 = 1_780_000_000;

fn run_git(dir: &Path, args: &[&str], date: Option<i64>) {
    let mut command = Command::new(common::git_program());
    command.args(args).current_dir(dir);
    if let Some(ts) = date {
        let value = format!("{ts} +0000");
        command
            .env("GIT_AUTHOR_DATE", &value)
            .env("GIT_COMMITTER_DATE", &value);
    }
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("git {args:?} should spawn: {e}"));
    assert!(status.success(), "git {args:?} should succeed");
}

/// Builds a repo on `main` with one commit at `T_BASE + 100`, a `feature`
/// branch with a commit at `T_BASE + 400`, and a second `main` commit at
/// `T_BASE + 700`. Returns `(base, repo_root, main_shas, feature_shas)`.
fn build_repo() -> (TempDir, PathBuf, Vec<String>, Vec<String>) {
    let base = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let repo = base.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap_or_else(|e| panic!("repo dir: {e}"));
    run_git(&repo, &["init", "-b", "main"], None);
    run_git(&repo, &["config", "user.email", "t@t.com"], None);
    run_git(&repo, &["config", "user.name", "Test"], None);

    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    run_git(&repo, &["add", "."], None);
    run_git(&repo, &["commit", "-m", "main one"], Some(T_BASE + 100));

    run_git(&repo, &["checkout", "-b", "feature"], None);
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    run_git(&repo, &["add", "."], None);
    run_git(&repo, &["commit", "-m", "feature one"], Some(T_BASE + 400));

    run_git(&repo, &["checkout", "main"], None);
    std::fs::write(repo.join("a.txt"), "one-two\n").unwrap();
    run_git(&repo, &["add", "."], None);
    run_git(&repo, &["commit", "-m", "main two"], Some(T_BASE + 700));

    let main_shas = rev_list(&repo, "main");
    let feature_shas = rev_list(&repo, "feature");
    (base, repo, main_shas, feature_shas)
}

fn rev_list(repo: &Path, branch: &str) -> Vec<String> {
    let out = Command::new(common::git_program())
        .args(["rev-list", branch])
        .current_dir(repo)
        .output()
        .unwrap_or_else(|e| panic!("rev-list {branch}: {e}"));
    assert!(out.status.success(), "rev-list {branch} should succeed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn session(session_id: &str, project_path: &str, started: i64, ended: i64) -> SessionRecord {
    SessionRecord {
        provider: "claude".to_string(),
        session_id: session_id.to_string(),
        project_key: project_path.to_string(),
        project_path: project_path.to_string(),
        title: Some(format!("Session {session_id}")),
        started_at: Some(started),
        ended_at: Some(ended),
        transcript_path: Some(format!("{session_id}.jsonl")),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn message(session_id: &str, message_id: &str, ts: i64) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: "claude".to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        timestamp: Some(ts),
        ordinal: 1,
        text: "work".to_string(),
        kind: Some("message".to_string()),
        model: Some("test-model".to_string()),
        tool_names: None,
        source_path: Some(format!("{session_id}.jsonl")),
        source_offset: Some(0),
        metadata_json: None,
    }
}

/// Reflog stub: every worktree reports the same timeline and current branch.
struct FakeGit {
    timeline: Vec<BranchTimelineEntry>,
    current: Option<String>,
    real_repo: PathBuf,
}

impl GitReflogSource for FakeGit {
    fn reflog(&self, _worktree: &Path) -> Option<String> {
        // Rendered newest-first, the shape branch_timeline_from_reflog parses.
        let mut lines: Vec<String> = self
            .timeline
            .iter()
            .map(|(ts, branch)| {
                let target = branch.clone().unwrap_or_else(|| "a1b2c3d4e5f6".to_string());
                format!("abc123 HEAD@{{{ts}}}: checkout: moving from prev to {target}")
            })
            .collect();
        lines.reverse();
        Some(lines.join("\n"))
    }

    fn current_branch(&self, _worktree: &Path) -> Option<String> {
        self.current.clone()
    }

    fn commit_log(&self, _worktree: &Path, branch: &str, since: i64) -> Option<String> {
        // Delegate to the real repo so commit shas/times are authentic.
        let out = Command::new(common::git_program())
            .args([
                "log",
                branch,
                "--pretty=%H %ct",
                &format!("--since={since}"),
            ])
            .current_dir(&self.real_repo)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    }
}

async fn open_seeded_db(repo: &Path) -> (TempDir, HostAdmissionTestRuntimeV1, String) {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("db tmpdir: {e}"));
    let db = HostAdmissionTestRuntimeV1::project(
        tmp.path().join(".tracedecay"),
        repo,
        ProjectId::new("project.git-backfill").unwrap(),
    )
    .await
    .unwrap_or_else(|error| panic!("open registered sessions runtime: {error}"));
    assert!(
        db.database_path(HostAdmissionScope::Project).is_some(),
        "registered project sessions database should be mounted"
    );
    let project = repo.to_string_lossy().to_string();

    // s_switch spans the whole run: main → feature → main.
    assert!(
        db.upsert_session_for_test(
            HostAdmissionScope::Project,
            &session("s_switch", &project, T_BASE, T_BASE + 900),
        )
        .await
        .unwrap()
    );
    assert!(
        db.upsert_session_message_for_test(
            HostAdmissionScope::Project,
            &message("s_switch", "m1", T_BASE + 50),
        )
        .await
        .unwrap()
    );
    assert!(
        db.upsert_session_message_for_test(
            HostAdmissionScope::Project,
            &message("s_switch", "m2", T_BASE + 850),
        )
        .await
        .unwrap()
    );

    // s_main only overlaps the first main stretch.
    assert!(
        db.upsert_session_for_test(
            HostAdmissionScope::Project,
            &session("s_main", &project, T_BASE + 60, T_BASE + 250),
        )
        .await
        .unwrap()
    );
    assert!(
        db.upsert_session_message_for_test(
            HostAdmissionScope::Project,
            &message("s_main", "m3", T_BASE + 200),
        )
        .await
        .unwrap()
    );

    (tmp, db, project)
}

#[tokio::test]
async fn backfill_attributes_branch_switch_and_commits() {
    let (_base, repo, main_shas, feature_shas) = build_repo();
    let worktree = normalize_worktree(&repo.to_string_lossy());
    let (_db_tmp, db, _project) = open_seeded_db(&repo).await;

    // HEAD held main until +300, switched to feature at +300, back to main at
    // +600. current_branch is the floor before the first entry (main).
    let git = FakeGit {
        timeline: vec![
            (T_BASE + 300, Some("feature".to_string())),
            (T_BASE + 600, Some("main".to_string())),
        ],
        current: Some("main".to_string()),
        real_repo: repo.clone(),
    };
    let opts = BackfillOptions {
        since: T_BASE - 1,
        ..Default::default()
    };

    let stats = db
        .run_git_backfill_for_test(&[], &git, &opts)
        .await
        .unwrap_or_else(|e| panic!("backfill: {e}"));
    assert_eq!(stats.sessions_scanned, 2);
    assert_eq!(
        stats.skipped_total(),
        0,
        "both sessions map to the repo worktree"
    );
    assert!(stats.spans_written >= 2);

    // s_switch touched both branches; s_main only main.
    let feature_hits = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("feature".to_string()),
                since: None,
                until: None,
                limit: 20,
            },
            CommitRelationFilter::Observed,
        )
        .await
        .unwrap();
    let feature_ids: Vec<&str> = feature_hits.iter().map(|h| h.session_id.as_str()).collect();
    assert_eq!(
        feature_ids,
        vec!["s_switch"],
        "only the switching session hit feature"
    );
    assert_eq!(feature_hits[0].worktree.as_deref(), Some(worktree.as_str()));

    let main_hits = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: None,
                until: None,
                limit: 20,
            },
            CommitRelationFilter::Observed,
        )
        .await
        .unwrap();
    let mut main_ids: Vec<String> = main_hits.iter().map(|h| h.session_id.clone()).collect();
    main_ids.sort();
    assert_eq!(main_ids, vec!["s_main".to_string(), "s_switch".to_string()]);

    // The feature commit falls in s_switch's feature segment [+300, +600].
    let feature_sha = feature_shas
        .iter()
        .find(|sha| !main_shas.contains(sha))
        .expect("feature-only commit");
    let commit_hits = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Commit(feature_sha.clone()),
                since: None,
                until: None,
                limit: 20,
            },
            CommitRelationFilter::Observed,
        )
        .await
        .unwrap();
    let commit_ids: Vec<&str> = commit_hits.iter().map(|h| h.session_id.as_str()).collect();
    assert_eq!(
        commit_ids,
        vec!["s_switch"],
        "feature commit attributed to the switching session"
    );
    assert_eq!(
        commit_hits[0].commit_sha.as_deref(),
        Some(feature_sha.as_str())
    );
}

#[tokio::test]
async fn backfill_is_idempotent_and_dry_run_writes_nothing() {
    let (_base, repo, _main, _feature) = build_repo();
    let (_db_tmp, db, _project) = open_seeded_db(&repo).await;
    let git = FakeGit {
        timeline: vec![
            (T_BASE + 300, Some("feature".to_string())),
            (T_BASE + 600, Some("main".to_string())),
        ],
        current: Some("main".to_string()),
        real_repo: repo.clone(),
    };
    let opts = BackfillOptions {
        since: T_BASE - 1,
        ..Default::default()
    };

    // Dry run writes nothing: no spans, so sessions_for is empty afterward.
    let dry = db
        .run_git_backfill_for_test(
            &[],
            &git,
            &BackfillOptions {
                dry_run: true,
                ..opts.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(dry.sessions_scanned, 2);
    let after_dry = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: None,
                until: None,
                limit: 20,
            },
            CommitRelationFilter::Observed,
        )
        .await
        .unwrap();
    assert!(after_dry.is_empty(), "dry-run must not write spans");

    // First real run writes; second run writes nothing new.
    let first = db
        .run_git_backfill_for_test(&[], &git, &opts)
        .await
        .unwrap();
    assert!(first.commits_attributed >= 1);
    let second = db
        .run_git_backfill_for_test(&[], &git, &opts)
        .await
        .unwrap();
    assert_eq!(
        second.commits_attributed, 0,
        "re-run must not re-attribute commits (INSERT OR IGNORE)"
    );

    // Span rows also converge: the main-branch session set is unchanged.
    let hits = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: None,
                until: None,
                limit: 20,
            },
            CommitRelationFilter::Observed,
        )
        .await
        .unwrap();
    let mut ids: Vec<String> = hits.iter().map(|h| h.session_id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["s_main".to_string(), "s_switch".to_string()]);
}

/// Watermark key mirrored from `git_correlation::AUTO_BACKFILL_WATERMARK_KEY`
/// (that const is `pub(crate)`, so integration tests reference the literal).
const AUTO_BACKFILL_WATERMARK_KEY: &str = "auto_backfill_activity_watermark";

fn incremental_git(repo: &Path) -> FakeGit {
    FakeGit {
        timeline: vec![
            (T_BASE + 300, Some("feature".to_string())),
            (T_BASE + 600, Some("main".to_string())),
        ],
        current: Some("main".to_string()),
        real_repo: repo.to_path_buf(),
    }
}

#[tokio::test]
async fn incremental_backfill_advances_watermark_and_is_idempotent() {
    let (_base, repo, _main, _feature) = build_repo();
    let (_db_tmp, db, _project) = open_seeded_db(&repo).await;
    let git = incremental_git(&repo);

    // No pass has run yet, so no watermark is recorded.
    assert_eq!(
        db.git_correlation_meta_for_test(AUTO_BACKFILL_WATERMARK_KEY)
            .await
            .unwrap(),
        None
    );

    // First pass drains both seeded sessions and writes their spans.
    let first = db
        .run_incremental_git_backfill_for_test(&git, 50)
        .await
        .unwrap();
    assert_eq!(first.sessions_scanned, 2);
    assert!(
        first.spans_written >= 2,
        "both sessions map to the worktree"
    );

    // The watermark advances to the newest session activity: s_switch's last
    // message at T_BASE + 850.
    assert_eq!(
        db.git_correlation_meta_for_test(AUTO_BACKFILL_WATERMARK_KEY)
            .await
            .unwrap(),
        Some(T_BASE + 850)
    );

    // A second pass finds nothing newer than the watermark: no rescans.
    let second = db
        .run_incremental_git_backfill_for_test(&git, 50)
        .await
        .unwrap();
    assert_eq!(second.sessions_scanned, 0);
    assert_eq!(second.spans_written, 0);

    // The spans written by the first pass are intact and queryable.
    let hits = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: None,
                until: None,
                limit: 20,
            },
            CommitRelationFilter::Observed,
        )
        .await
        .unwrap();
    let mut ids: Vec<String> = hits.iter().map(|h| h.session_id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["s_main".to_string(), "s_switch".to_string()]);
}

#[tokio::test]
async fn incremental_backfill_cap_drains_history_oldest_first_across_passes() {
    let (_base, repo, _main, _feature) = build_repo();
    let (_db_tmp, db, _project) = open_seeded_db(&repo).await;
    let git = incremental_git(&repo);

    // A cap of one session per pass drains oldest-first. s_main's activity
    // (last message T_BASE + 200) precedes s_switch's (T_BASE + 850).
    let pass1 = db
        .run_incremental_git_backfill_for_test(&git, 1)
        .await
        .unwrap();
    assert_eq!(pass1.sessions_scanned, 1);
    assert_eq!(
        db.git_correlation_meta_for_test(AUTO_BACKFILL_WATERMARK_KEY)
            .await
            .unwrap(),
        Some(T_BASE + 200),
        "oldest session processed first"
    );

    let pass2 = db
        .run_incremental_git_backfill_for_test(&git, 1)
        .await
        .unwrap();
    assert_eq!(pass2.sessions_scanned, 1);
    assert_eq!(
        db.git_correlation_meta_for_test(AUTO_BACKFILL_WATERMARK_KEY)
            .await
            .unwrap(),
        Some(T_BASE + 850)
    );

    // History fully drained: the next pass has nothing to do.
    let pass3 = db
        .run_incremental_git_backfill_for_test(&git, 1)
        .await
        .unwrap();
    assert_eq!(pass3.sessions_scanned, 0);

    // Both sessions ended up attributed to main across the two passes.
    let hits = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: None,
                until: None,
                limit: 20,
            },
            CommitRelationFilter::Observed,
        )
        .await
        .unwrap();
    let mut ids: Vec<String> = hits.iter().map(|h| h.session_id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["s_main".to_string(), "s_switch".to_string()]);
}

#[tokio::test]
async fn backfill_skips_non_worktree_sessions() {
    let (_base, repo, _main, _feature) = build_repo();
    let tmp = tempfile::tempdir().unwrap();
    let db = HostAdmissionTestRuntimeV1::project(
        tmp.path().join(".tracedecay"),
        &repo,
        ProjectId::new("project.git-backfill-non-worktree").unwrap(),
    )
    .await
    .unwrap();

    // A session whose project_path is not a git repo.
    let not_repo = tmp.path().join("plain-dir").to_string_lossy().to_string();
    std::fs::create_dir_all(&not_repo).unwrap();
    assert!(
        db.upsert_session_for_test(
            HostAdmissionScope::Project,
            &session("s_orphan", &not_repo, T_BASE, T_BASE + 100),
        )
        .await
        .unwrap()
    );
    assert!(
        db.upsert_session_message_for_test(
            HostAdmissionScope::Project,
            &message("s_orphan", "m1", T_BASE + 50),
        )
        .await
        .unwrap()
    );

    let git = FakeGit {
        timeline: vec![],
        current: Some("main".to_string()),
        real_repo: repo.clone(),
    };
    let stats = db
        .run_git_backfill_for_test(
            &[],
            &git,
            &BackfillOptions {
                since: T_BASE - 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(stats.sessions_scanned, 1);
    assert_eq!(stats.skipped_not_worktree, 1);
    assert_eq!(stats.spans_written, 0);
}
