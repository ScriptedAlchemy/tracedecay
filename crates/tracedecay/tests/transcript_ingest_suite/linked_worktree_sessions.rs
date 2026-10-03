//! Sessions captured from a linked worktree are Git evidence for that
//! worktree. The linked worktree shares the primary checkout's project store,
//! so `sessions_for` on the shared project must list them under the worktree
//! they ran in and not under the primary checkout.

use std::collections::BTreeSet;

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

fn write_claude_transcript(home: &std::path::Path, cwd: &std::path::Path, session: &str) {
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
                "gitBranch": "linked-worktree",
                "uuid": format!("{session}-u1"),
                "timestamp": "2026-02-01T00:00:00.000Z",
                "message": {"role": "user", "content": format!("{session} asks about the worktree")}
            }),
            serde_json::json!({
                "type": "assistant",
                "cwd": cwd,
                "sessionId": session,
                "gitBranch": "linked-worktree",
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

fn write_codex_rollout(home: &std::path::Path, cwd: &std::path::Path, session: &str) {
    let dir = home.join(".codex/sessions/2026/02/01");
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = cwd.to_string_lossy();
    write_jsonl(
        &dir.join(format!("rollout-2026-02-01T00-00-00-{session}.jsonl")),
        &[
            serde_json::json!({
                "timestamp": "2026-02-01T00:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": session,
                    "cwd": cwd,
                    "model_provider": "openai",
                    "git": {"branch": "linked-worktree"}
                }
            }),
            serde_json::json!({
                "timestamp": "2026-02-01T00:00:01.000Z",
                "type": "event_msg",
                "payload": {"type": "user_message", "message": format!("{session} prompt")}
            }),
            serde_json::json!({
                "timestamp": "2026-02-01T00:00:02.000Z",
                "type": "event_msg",
                "payload": {"type": "agent_message", "message": format!("{session} reply")}
            }),
        ],
    );
}

fn worktree_query(worktree: &std::path::Path) -> SessionsForQuery {
    SessionsForQuery {
        git_ref: GitRefFilter::Worktree(worktree.to_string_lossy().into_owned()),
        since: None,
        until: None,
        limit: 20,
    }
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
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    let linked = tmp.path().join("linked");
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
    run_git(
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-worktree",
            linked.to_str().unwrap(),
        ],
    );
    let project = project.canonicalize().unwrap();
    let linked = linked.canonicalize().unwrap();
    let nested = linked.join("src");
    std::fs::create_dir_all(&nested).unwrap();

    write_claude_transcript(&home, &linked, "claude-linked");
    write_claude_transcript(&home, &project, "claude-primary");
    write_codex_rollout(&home, &nested, "codex-linked");

    let project_id = ProjectId::new("project.linked-worktree-sessions").unwrap();
    assert!(
        tracedecay_runtime_core::storage::write_repository_identity_marker(
            &project,
            project_id.as_str()
        )
        .unwrap()
    );
    let runtime =
        HostAdmissionTestRuntimeV1::project(home.join(".tracedecay"), &project, project_id)
            .await
            .unwrap();
    for provider in [SessionProvider::Claude, SessionProvider::Codex] {
        with_transcript_source_profile(
            ProfileRoot::new(runtime.profile_root_for_test()).with_home(&home),
            runtime.ingest_project_provider_for_test(&project, Some(provider)),
        )
        .await
        .unwrap();
    }

    let linked_key = Some(normalize_worktree(&linked.to_string_lossy()));
    let primary_key = Some(normalize_worktree(&project.to_string_lossy()));
    assert_eq!(
        sessions_in(&runtime, &worktree_query(&linked)).await,
        BTreeSet::from([
            (
                "claude".to_owned(),
                "claude-linked".to_owned(),
                linked_key.clone()
            ),
            (
                "codex".to_owned(),
                "codex-linked".to_owned(),
                linked_key.clone()
            ),
        ]),
        "sessions whose cwd is the linked worktree are evidence for that worktree"
    );
    assert_eq!(
        sessions_in(&runtime, &worktree_query(&project)).await,
        BTreeSet::from([(
            "claude".to_owned(),
            "claude-primary".to_owned(),
            primary_key
        )]),
        "the primary checkout lists only the session that ran there"
    );

    let branch_query = SessionsForQuery {
        git_ref: GitRefFilter::Branch("linked-worktree".to_owned()),
        since: None,
        until: None,
        limit: 20,
    };
    let on_branch = sessions_in(&runtime, &branch_query).await;
    assert!(
        on_branch.contains(&("codex".to_owned(), "codex-linked".to_owned(), linked_key)),
        "the rollout's recorded branch keeps the linked worktree: {on_branch:?}"
    );
    assert!(
        !on_branch
            .iter()
            .any(|(_, session_id, _)| session_id == "claude-primary"),
        "a primary-checkout session never joins the linked branch: {on_branch:?}"
    );
}
