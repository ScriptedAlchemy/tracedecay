//! Codex ingest mechanics: incremental cursors, archived rollouts, turn
//! cwd/git attribution, subagent parent links, and path relocation.

use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_sessions::runtime::SessionProvider;
use tracedecay_sessions::runtime::codex::CodexSource;
use tracedecay_sessions::runtime::source::{StoredCursor, TranscriptSource};

use crate::codex::{write_codex_rollout, write_jsonl};
use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::restart_atomicity::{
    durable_table_count, ingest_global_sources_for_provider, mark_test_project,
    open_project_session_db, try_ingest_source,
};
use crate::support::{
    assert_metadata_path_eq, create_git_repo_with_linked_worktree, init_git_repo, setup,
};

fn write_codex_subagent_rollout(
    home: &std::path::Path,
    project: &std::path::Path,
    parent_session: &str,
    child_session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-10-{child_session}.jsonl"));
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:10.000Z",
            "type": "session_meta",
            "payload": {
                "id": child_session,
                "cwd": project.to_string_lossy(),
                "model_provider": "openai",
                "thread_source": "subagent",
                "forked_from_id": parent_session,
                "agent_nickname": "Euler",
                "agent_role": "explorer",
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": parent_session,
                            "agent_nickname": "Euler",
                            "agent_role": "explorer",
                            "depth": 1
                        }
                    }
                }
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:11.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "The child worker verified Codex layout evidence."}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_codex_rollout_at(
    home: &Path,
    project: &Path,
    session: &str,
    relative_dir: &str,
    file_name: &str,
) -> PathBuf {
    let dir = home.join(".codex").join(relative_dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file_name);
    let contents = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Investigate the billing pipeline regression"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": "The billing pipeline regression is fixed."
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

/// Archived rollouts (`~/.codex/archived_sessions/rollout-*.jsonl`, flat
/// layout) are real transcripts and must be swept like live ones. The real
/// machine had 22 of them invisible to ingestion before this fix.
#[tokio::test]
async fn codex_archived_rollout_is_ingested() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    // Native joins keep the expected path separator-identical to the stored
    // transcript_path on Windows.
    let dir = home.join(".codex").join("archived_sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-00-archived-sess.jsonl");
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": "archived-sess", "cwd": project.to_string_lossy()}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Archived rollout probe"}
        }),
    );
    std::fs::write(&path, contents).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.sessions_upserted, 1);
    assert_eq!(stats.messages_upserted, 1);
    let session = db
        .get_session("codex", "archived-sess")
        .await
        .expect("archived rollout session should be stored");
    assert_eq!(
        session.transcript_path.as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
}
#[tokio::test]
async fn codex_rollout_ingest_is_incremental() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_codex_rollout(&home, &project, "codex-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        2
    );
    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        0
    );

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        f,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:01:00.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Added a regression test."}
        })
    )
    .unwrap();
    drop(f);

    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        1
    );
}

#[tokio::test]
async fn codex_messages_keep_turn_cwd_and_session_git_updates() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-00-branch-sess.jsonl");
    let main_cwd = project.to_string_lossy();
    let linked_cwd = linked_worktree.to_string_lossy();
    write_jsonl(
        &path,
        &[
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": "branch-sess",
                    "cwd": main_cwd,
                    "model_provider": "openai",
                    "git": {
                        "branch": "main",
                        "commit_hash": "1111111111111111111111111111111111111111",
                        "repository_url": "git@example.com:repo/project.git"
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:00.500Z",
                "type": "turn_context",
                "payload": {"turn_id": "t1", "cwd": main_cwd, "model": "gpt-5.3-codex"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "First branch attribution marker"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "session_meta",
                "payload": {
                    "id": "branch-sess",
                    "cwd": main_cwd,
                    "model_provider": "openai",
                    "git": {
                        "branch": "feature/worktree",
                        "commit_hash": "2222222222222222222222222222222222222222",
                        "repository_url": "git@example.com:repo/project.git"
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:02.500Z",
                "type": "turn_context",
                "payload": {"turn_id": "t2", "cwd": linked_cwd, "model": "gpt-5.5"}
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:03.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "agent_message",
                    "message": "Second branch attribution marker"
                }
            }),
        ],
    );

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();

    let hits = db
        .search_session_messages("codex", None, "attribution", 10)
        .await;
    assert_eq!(hits.len(), 2);
    let session_metadata: serde_json::Value =
        serde_json::from_str(hits[0].session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&session_metadata["codex_session_cwd"], &project);
    assert_metadata_path_eq(&session_metadata["codex_session_worktree"], &project);
    assert_eq!(
        session_metadata["codex_session_location_provenance"].as_str(),
        Some("session_meta")
    );
    assert_eq!(session_metadata["codex_git_branch"], "main");
    let metadata_of = |needle: &str| -> serde_json::Value {
        let hit = hits
            .iter()
            .find(|hit| hit.message.text.contains(needle))
            .unwrap_or_else(|| panic!("message containing {needle:?} should exist"));
        serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap()
    };

    let first = metadata_of("First branch");
    assert_metadata_path_eq(&first["codex_turn_cwd"], &project);
    assert_metadata_path_eq(&first["codex_turn_worktree"], &project);
    assert_eq!(
        first["codex_turn_location_provenance"].as_str(),
        Some("codex_context")
    );
    assert_eq!(first["codex_git_branch"], "main");
    assert_eq!(
        first["codex_git_commit_hash"],
        "1111111111111111111111111111111111111111"
    );

    let second = metadata_of("Second branch");
    assert_metadata_path_eq(&second["codex_turn_cwd"], &linked_worktree);
    assert_metadata_path_eq(&second["codex_turn_worktree"], &linked_worktree);
    assert_eq!(
        second["codex_turn_location_provenance"].as_str(),
        Some("codex_context")
    );
    assert_eq!(second["codex_git_branch"], "feature/worktree");
    assert_eq!(
        second["codex_git_commit_hash"],
        "2222222222222222222222222222222222222222"
    );
}

#[tokio::test]
async fn codex_incremental_ingest_reconstructs_prior_turn_cwd_and_git() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-00-branch-incremental.jsonl");
    let main_cwd = project.to_string_lossy();
    let linked_cwd = linked_worktree.to_string_lossy();
    let prior_lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": "branch-incremental",
                "cwd": main_cwd,
                "model_provider": "openai",
                "git": {
                    "branch": "main",
                    "commit_hash": "1111111111111111111111111111111111111111"
                }
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "First incremental branch marker"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "session_meta",
            "payload": {
                "id": "branch-incremental",
                "cwd": main_cwd,
                "model_provider": "openai",
                "git": {
                    "branch": "feature/worktree",
                    "commit_hash": "2222222222222222222222222222222222222222"
                }
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.500Z",
            "type": "turn_context",
            "payload": {"turn_id": "t2", "cwd": linked_cwd, "model": "gpt-5.5"}
        }),
    ];
    let prior = prior_lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let resumed_line = serde_json::json!({
        "timestamp": "2026-01-01T00:00:03.000Z",
        "type": "event_msg",
        "payload": {
            "type": "agent_message",
            "message": "Second incremental branch marker"
        }
    })
    .to_string()
        + "\n";
    std::fs::write(&path, format!("{prior}{resumed_line}")).unwrap();

    let source = CodexSource::with_home(&home);
    let parsed = source
        .parse_new(
            &path,
            StoredCursor {
                position: prior.len() as u64,
                mtime: 0,
                file_id: 0,
            },
            &project,
            None,
        )
        .expect("resumed parse should produce the appended message");
    assert_eq!(parsed.messages.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(parsed.messages[0].metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata["codex_turn_cwd"], &linked_worktree);
    assert_eq!(
        metadata["codex_turn_location_provenance"].as_str(),
        Some("codex_context")
    );
    assert_eq!(metadata["codex_git_branch"], "feature/worktree");
    assert_metadata_path_eq(&metadata["codex_turn_worktree"], &linked_worktree);
    assert_eq!(
        metadata["codex_git_commit_hash"],
        "2222222222222222222222222222222222222222"
    );
}

#[tokio::test]
async fn codex_subagent_rollout_uses_parent_link_from_session_meta() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout(&home, &project, "codex-parent");
    write_codex_subagent_rollout(&home, &project, "codex-parent", "codex-child");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 3);

    let child = db
        .get_session("codex", "codex-child")
        .await
        .expect("subagent session should be stored");
    assert_eq!(child.parent_session_id.as_deref(), Some("codex-parent"));
    assert!(child.is_subagent);
    assert_eq!(child.agent_id.as_deref(), Some("Euler"));

    let results = db
        .search_session_messages("codex", None, "layout evidence", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "codex-child");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_jsonl_path_relocation_keeps_session_identity_on_production_observation_path() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let session = "codex-path-reloc-prod";
    let original = write_codex_rollout_at(
        &home,
        &project,
        session,
        "sessions/2026/01/01",
        &format!("rollout-2026-01-01T00-00-00-{session}.jsonl"),
    );

    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Codex))
            .await
            .messages_upserted,
        2
    );
    assert_eq!(db.session_message_count().await.unwrap(), 2);
    let observations_before = durable_table_count(&db, "observations").await;
    assert!(observations_before >= 1);
    assert!(db.get_session("codex", session).await.is_some());
    drop(db);

    // Relocate the same real transcript bytes to another Codex discovery path.
    let original_bytes = std::fs::read(&original).unwrap();
    let relocated = home.join(format!(
        ".codex/sessions/2026/02/02/rollout-relocated-{session}.jsonl"
    ));
    std::fs::create_dir_all(relocated.parent().unwrap()).unwrap();
    std::fs::write(&relocated, &original_bytes).unwrap();
    assert_ne!(original, relocated);
    std::fs::remove_file(&original).unwrap();

    let relocated_db = open_project_session_db(&project).await.unwrap();
    let retry =
        ingest_global_sources_for_provider(&relocated_db, &project, Some(SessionProvider::Codex))
            .await;
    // Content-addressed observation identity + session_meta.payload.id keep the
    // logical session stable across filesystem path relocation; redelivery is a
    // durable no-op (no overwrite / no duplicate searchable rows).
    assert_eq!(retry.messages_upserted, 0);
    assert_eq!(relocated_db.session_message_count().await.unwrap(), 2);
    assert_eq!(
        durable_table_count(&relocated_db, "observations").await,
        observations_before
    );
    assert!(relocated_db.get_session("codex", session).await.is_some());
    assert_eq!(
        relocated_db
            .search_session_messages("codex", None, "fixed", 10)
            .await
            .len(),
        1
    );
}
