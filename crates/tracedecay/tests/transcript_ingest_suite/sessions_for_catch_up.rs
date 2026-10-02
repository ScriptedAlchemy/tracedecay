use std::path::Path;

use tempfile::TempDir;
use tracedecay_sessions::runtime::git_correlation::{
    CommitRelationFilter, GitRefFilter, SessionsForQuery, SystemGit, normalize_worktree,
};

use crate::restart_atomicity::{
    ProjectSessionTestRuntime, ingest_global_sources_for_provider, open_project_session_db,
};
use crate::support::{init_project_at, run_git};

fn init_repo(project: &Path) {
    std::fs::create_dir_all(project).unwrap();
    run_git(project, &["-c", "init.defaultBranch=main", "init", "-q"]);
    std::fs::write(project.join("README.md"), "sessions_for fixture\n").unwrap();
    run_git(project, &["add", "README.md"]);
    run_git(
        project,
        &[
            "-c",
            "user.email=t@t.invalid",
            "-c",
            "user.name=Test",
            "commit",
            "-qm",
            "init",
        ],
    );
    init_project_at(project);
}

fn write_codex_rollout(home: &Path, project: &Path, session: &str, day: &str) {
    let dir = home.join(format!(".codex/sessions/2026/{}", day.replace('-', "/")));
    std::fs::create_dir_all(&dir).unwrap();
    let rows = [
        serde_json::json!({
            "timestamp": format!("2026-{day}T00:00:00.000Z"),
            "type": "session_meta",
            "payload": {"id": session, "cwd": project, "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": format!("2026-{day}T00:00:01.000Z"),
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Investigate the billing regression"}
        }),
        serde_json::json!({
            "timestamp": format!("2026-{day}T00:00:02.000Z"),
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Tracing it."}
        }),
    ];
    std::fs::write(
        dir.join(format!("rollout-2026-{day}T00-00-00-{session}.jsonl")),
        rows.map(|row| format!("{row}\n")).concat(),
    )
    .unwrap();
}

fn write_claude_session(home: &Path, project: &Path, session: &str, day: &str) {
    let dir = home.join(".claude/projects/-project");
    std::fs::create_dir_all(&dir).unwrap();
    let rows = [
        serde_json::json!({
            "type": "user", "cwd": project, "sessionId": session,
            "uuid": format!("{session}-u1"), "timestamp": format!("2026-{day}T00:00:00.000Z"),
            "message": {"role": "user", "content": "Investigate the billing regression"}
        }),
        serde_json::json!({
            "type": "assistant", "cwd": project, "sessionId": session,
            "uuid": format!("{session}-u2"), "parentUuid": format!("{session}-u1"),
            "timestamp": format!("2026-{day}T00:00:05.000Z"),
            "message": {"role": "assistant", "content": [{"type": "text", "text": "Tracing it."}]}
        }),
    ];
    std::fs::write(
        dir.join(format!("{session}.jsonl")),
        rows.map(|row| format!("{row}\n")).concat(),
    )
    .unwrap();
}

async fn import_and_converge(home: &Path, db: &ProjectSessionTestRuntime, project: &Path) {
    ingest_global_sources_for_provider(home, db, project, None).await;
    db.runtime()
        .converge_git_evidence_for_test(&SystemGit, Some(project))
        .await
        .unwrap();
}

async fn worktree_sessions(
    db: &ProjectSessionTestRuntime,
    project: &Path,
) -> Vec<(String, String)> {
    let hits = db
        .runtime()
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Worktree(normalize_worktree(&project.to_string_lossy())),
                since: None,
                until: None,
                limit: 50,
            },
            CommitRelationFilter::All,
        )
        .await
        .unwrap();
    let mut sessions = hits
        .into_iter()
        .map(|hit| (hit.provider, hit.session_id))
        .collect::<Vec<_>>();
    sessions.sort_unstable();
    sessions.dedup();
    sessions
}

#[tokio::test]
async fn sessions_imported_after_the_catch_up_frontier_are_listed_for_their_worktree() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    init_repo(&project);
    let db = open_project_session_db(&project).await.unwrap();

    write_codex_rollout(&home, &project, "codex-recent", "06-01");
    import_and_converge(&home, &db, &project).await;
    assert_eq!(
        worktree_sessions(&db, &project).await,
        [("codex".to_owned(), "codex-recent".to_owned())]
    );

    write_codex_rollout(&home, &project, "codex-late", "01-01");
    write_claude_session(&home, &project, "claude-late", "01-01");
    import_and_converge(&home, &db, &project).await;

    assert_eq!(
        worktree_sessions(&db, &project).await,
        [
            ("claude".to_owned(), "claude-late".to_owned()),
            ("codex".to_owned(), "codex-late".to_owned()),
            ("codex".to_owned(), "codex-recent".to_owned()),
        ]
    );
}
