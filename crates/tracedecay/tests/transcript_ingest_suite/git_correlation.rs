use tempfile::TempDir;
use tracedecay_sessions::SessionProvider;
use tracedecay_sessions::runtime::git_correlation::{
    CommitRelationFilter, GitRefFilter, SessionsForQuery, SystemGit, normalize_worktree,
};

use crate::claude::write_claude_transcript;
use crate::restart_atomicity::{ingest_global_sources_for_provider, open_project_session_db};
use crate::support::{init_project_at, run_git};

#[tokio::test]
async fn sessions_for_lists_a_claude_session_by_its_cwd() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    run_git(&project, &["-c", "init.defaultBranch=main", "init", "-q"]);
    std::fs::write(project.join("README.md"), "sessions_for fixture\n").unwrap();
    run_git(&project, &["add", "README.md"]);
    run_git(
        &project,
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
    init_project_at(&project);
    write_claude_transcript(&home, &project, "claude-sess");

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Claude)).await;
    db.runtime()
        .converge_git_evidence_for_test(&SystemGit)
        .await
        .unwrap();

    for git_ref in [
        GitRefFilter::Worktree(normalize_worktree(&project.to_string_lossy())),
        GitRefFilter::Branch("main".to_owned()),
    ] {
        let hits = db
            .runtime()
            .git_sessions_for_for_test(
                &SessionsForQuery {
                    git_ref: git_ref.clone(),
                    since: None,
                    until: None,
                    limit: 10,
                },
                CommitRelationFilter::All,
            )
            .await
            .unwrap();
        let mut sessions = hits
            .iter()
            .map(|hit| (hit.provider.as_str(), hit.session_id.as_str()))
            .collect::<Vec<_>>();
        sessions.sort_unstable();
        sessions.dedup();
        assert_eq!(sessions, [("claude", "claude-sess")], "{git_ref:?}");
    }
}
