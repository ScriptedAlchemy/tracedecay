//! Sessions captured from a linked worktree are Git evidence for that
//! worktree. The linked worktree shares the primary checkout's project store,
//! so `sessions_for` on the shared project must list them under the worktree
//! they ran in and not under the primary checkout.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_domain::ProjectId;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_sessions::runtime::git_correlation::{
    CommitRelationFilter, GitRefFilter, SessionsForQuery, normalize_worktree,
};
use tracedecay_sessions::runtime::{SessionProvider, with_transcript_source_profile};

use crate::codex::write_jsonl;
use crate::support::{init_project_at, run_git};

/// A primary checkout with one commit on `main`, its linked worktrees, and the
/// home directory the host transcripts are written under.
struct LinkedRepository {
    tmp: TempDir,
    home: PathBuf,
    project: PathBuf,
}

impl LinkedRepository {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("project");
        init_project_at(&project);
        run_git(&project, &["checkout", "-q", "-b", "main"]);
        std::fs::write(project.join("README.md"), "linked worktree fixture\n").unwrap();
        run_git(&project, &["add", "README.md"]);
        run_git(
            &project,
            &[
                "-c",
                "user.name=TraceDecay Tests",
                "-c",
                "user.email=tests@example.invalid",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        let project = project.canonicalize().unwrap();
        Self { tmp, home, project }
    }

    fn add_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let worktree = self.tmp.path().join(name);
        run_git(
            &self.project,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                worktree.to_str().unwrap(),
            ],
        );
        worktree.canonicalize().unwrap()
    }

    async fn ingest(&self) -> HostAdmissionTestRuntimeV1 {
        let project_id = ProjectId::new("project.linked-worktree-sessions").unwrap();
        assert!(
            tracedecay_runtime_core::storage::write_repository_identity_marker(
                &self.project,
                project_id.as_str()
            )
            .unwrap()
        );
        let runtime = HostAdmissionTestRuntimeV1::project(
            self.home.join(".tracedecay"),
            &self.project,
            project_id,
        )
        .await
        .unwrap();
        for provider in [SessionProvider::Claude, SessionProvider::Codex] {
            with_transcript_source_profile(
                ProfileRoot::new(runtime.profile_root_for_test()).with_home(&self.home),
                runtime.ingest_project_provider_for_test(&self.project, Some(provider)),
            )
            .await
            .unwrap();
        }
        runtime
    }
}

fn write_claude_transcript(home: &Path, cwd: &Path, session: &str, branch: &str) {
    let dir = home.join(".claude/projects/-linked-worktree-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = cwd.to_string_lossy();
    write_jsonl(
        &dir.join(format!("{session}.jsonl")),
        &[
            serde_json::json!({
                "type": "user",
                "cwd": cwd,
                "sessionId": session,
                "gitBranch": branch,
                "uuid": format!("{session}-u1"),
                "timestamp": "2026-02-01T00:00:00.000Z",
                "message": {"role": "user", "content": format!("{session} asks about the worktree")}
            }),
            serde_json::json!({
                "type": "assistant",
                "cwd": cwd,
                "sessionId": session,
                "gitBranch": branch,
                "uuid": format!("{session}-a1"),
                "timestamp": "2026-02-01T00:00:05.000Z",
                "message": {
                    "role": "assistant",
                    "model": "claude-opus-4-8",
                    "content": [{"type": "text", "text": format!("{session} answers in the worktree")}]
                }
            }),
        ],
    );
}

/// A rollout that starts in `turns[0]` and moves to each later cwd through a
/// `turn_context` record, exchanging one prompt and reply in every cwd. Its
/// activity follows every Claude transcript, so the history pass that runs
/// after Codex ingestion reaches it past the Claude sessions' frontier.
fn write_codex_rollout(home: &Path, session: &str, turns: &[&Path]) {
    let dir = home.join(".codex/sessions/2026/02/01");
    std::fs::create_dir_all(&dir).unwrap();
    let mut records = Vec::new();
    for (turn, cwd) in turns.iter().enumerate() {
        let cwd = cwd.to_string_lossy();
        let minute = turn + 1;
        let at = |second: usize| format!("2026-02-01T00:{minute:02}:{second:02}.000Z");
        records.push(if turn == 0 {
            serde_json::json!({
                "timestamp": at(0),
                "type": "session_meta",
                "payload": {
                    "id": session,
                    "cwd": cwd,
                    "model_provider": "openai",
                    "git": {"branch": "linked-worktree"}
                }
            })
        } else {
            serde_json::json!({
                "timestamp": at(0),
                "type": "turn_context",
                "payload": {"turn_id": format!("{session}-turn-{turn}"), "cwd": cwd}
            })
        });
        records.push(serde_json::json!({
            "timestamp": at(1),
            "type": "event_msg",
            "payload": {"type": "user_message", "message": format!("{session} prompt {turn}")}
        }));
        records.push(serde_json::json!({
            "timestamp": at(2),
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": format!("{session} reply {turn}")}
        }));
    }
    write_jsonl(
        &dir.join(format!("rollout-2026-02-01T00-00-00-{session}.jsonl")),
        &records,
    );
}

fn worktree_query(worktree: &Path) -> SessionsForQuery {
    SessionsForQuery {
        git_ref: GitRefFilter::parse("worktree", &worktree.to_string_lossy()).unwrap(),
        since: None,
        until: None,
        limit: 20,
    }
}

fn branch_query(branch: &str) -> SessionsForQuery {
    SessionsForQuery {
        git_ref: GitRefFilter::Branch(branch.to_owned()),
        since: None,
        until: None,
        limit: 20,
    }
}

fn hit(provider: &str, session: &str, worktree: &Path) -> (String, String, Option<String>) {
    (
        provider.to_owned(),
        session.to_owned(),
        Some(normalize_worktree(&worktree.to_string_lossy())),
    )
}

async fn sessions_in(
    runtime: &HostAdmissionTestRuntimeV1,
    query: &SessionsForQuery,
) -> BTreeSet<(String, String, Option<String>)> {
    runtime
        .git_sessions_for_for_test(query, CommitRelationFilter::All)
        .await
        .unwrap()
        .into_iter()
        .map(|hit| (hit.provider, hit.session_id, hit.worktree))
        .collect()
}

#[tokio::test]
async fn sessions_for_lists_linked_worktree_sessions_under_their_worktree() {
    let repository = LinkedRepository::new();
    let project = repository.project.clone();
    let linked = repository.add_worktree("linked", "linked-worktree");
    let nested = linked.join("src");
    std::fs::create_dir_all(&nested).unwrap();

    write_claude_transcript(
        &repository.home,
        &linked,
        "claude-linked",
        "linked-worktree",
    );
    write_claude_transcript(&repository.home, &project, "claude-primary", "main");
    write_codex_rollout(&repository.home, "codex-linked", &[&nested]);
    let runtime = repository.ingest().await;

    let linked_sessions = BTreeSet::from([
        hit("claude", "claude-linked", &linked),
        hit("codex", "codex-linked", &linked),
    ]);
    let primary_sessions = BTreeSet::from([hit("claude", "claude-primary", &project)]);
    assert_eq!(
        sessions_in(&runtime, &worktree_query(&linked)).await,
        linked_sessions,
        "sessions whose cwd is the linked worktree are evidence for that worktree"
    );
    assert_eq!(
        sessions_in(&runtime, &worktree_query(&project)).await,
        primary_sessions,
        "the primary checkout lists only the session that ran there"
    );
    assert_eq!(
        sessions_in(&runtime, &branch_query("linked-worktree")).await,
        linked_sessions,
        "the recorded branch keeps each linked-worktree session"
    );
    assert_eq!(
        sessions_in(&runtime, &branch_query("main")).await,
        primary_sessions,
        "Claude's recorded gitBranch is branch evidence"
    );
}

#[tokio::test]
async fn codex_turn_context_moves_later_activity_to_the_new_worktree() {
    let repository = LinkedRepository::new();
    let first = repository.add_worktree("first", "first-worktree");
    let second = repository.add_worktree("second", "second-worktree");
    write_codex_rollout(&repository.home, "codex-moved", &[&first, &second]);
    let runtime = repository.ingest().await;

    assert_eq!(
        sessions_in(&runtime, &worktree_query(&first)).await,
        BTreeSet::from([hit("codex", "codex-moved", &first)]),
        "activity before the turn context is evidence for the starting worktree"
    );
    assert_eq!(
        sessions_in(&runtime, &worktree_query(&second)).await,
        BTreeSet::from([hit("codex", "codex-moved", &second)]),
        "activity after the turn context is evidence for the worktree it moved to"
    );
}

#[tokio::test]
async fn a_deleted_cwd_inside_a_linked_worktree_stays_on_that_worktree() {
    let repository = LinkedRepository::new();
    let linked = repository.add_worktree("linked", "linked-worktree");
    let removed = linked.join("tmp/task");
    write_claude_transcript(
        &repository.home,
        &removed,
        "claude-removed-cwd",
        "linked-worktree",
    );
    let runtime = repository.ingest().await;

    assert_eq!(
        sessions_in(&runtime, &worktree_query(&linked)).await,
        BTreeSet::from([hit("claude", "claude-removed-cwd", &linked)]),
        "a cwd removed before ingestion resolves through its surviving linked worktree"
    );
    assert_eq!(
        sessions_in(&runtime, &worktree_query(&repository.project)).await,
        BTreeSet::new(),
        "the primary checkout gains no evidence from a removed linked-worktree cwd"
    );
}
