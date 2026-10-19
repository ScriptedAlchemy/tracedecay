use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_sessions::runtime::SessionProvider;

use crate::codex::write_jsonl;
use crate::restart_atomicity::{
    ProjectSessionTestRuntime, ingest_global_sources_for_provider, open_project_session_db,
};
use crate::support::setup;

const THREAD: &str = "codex-revert-thread";
const REVERT_ROLLOUT: &str = "codex-revert-rollout";

fn rollout(home: &Path, day: &str, file_ids: &str, cwd: &Path, reply: &str) -> PathBuf {
    let dir = home.join(format!(".codex/sessions/2026/01/{day}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-{day}T00-00-00-{file_ids}.jsonl"));
    write_jsonl(
        &path,
        &[
            serde_json::json!({
                "timestamp": format!("2026-01-{day}T00:00:00.000Z"),
                "type": "session_meta",
                "payload": {"id": THREAD, "cwd": cwd.to_string_lossy()}
            }),
            serde_json::json!({
                "timestamp": format!("2026-01-{day}T00:00:01.000Z"),
                "type": "event_msg",
                "payload": {"type": "agent_message", "message": reply}
            }),
        ],
    );
    path
}

async fn replies(db: &ProjectSessionTestRuntime) -> Vec<String> {
    let mut texts: Vec<String> = db
        .search_session_messages("codex", None, "ready", 20)
        .await
        .into_iter()
        .filter(|hit| hit.message.session_id == THREAD)
        .map(|hit| hit.message.text)
        .collect();
    texts.sort();
    texts
}

#[tokio::test]
async fn reverted_codex_thread_projects_both_live_rollouts() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let thread_rollout = rollout(
        &home,
        "01",
        THREAD,
        &project,
        "The ledger migration is ready.",
    );
    rollout(
        &home,
        "02",
        &format!("{THREAD}_{REVERT_ROLLOUT}"),
        &project,
        "The invoice export retry is ready.",
    );

    let db = open_project_session_db(&project).await.unwrap();
    for _ in 0..2 {
        ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Codex))
            .await;
        assert_eq!(
            replies(&db).await,
            [
                "The invoice export retry is ready.",
                "The ledger migration is ready.",
            ],
            "each rollout of the thread keeps its own messages on every pass"
        );
    }
    let session = db
        .get_session("codex", THREAD)
        .await
        .expect("codex session should be stored");
    crate::support::assert_path_text_eq(
        session
            .transcript_path
            .as_deref()
            .expect("session transcript path"),
        &thread_rollout,
    );
}

#[tokio::test]
async fn rewriting_a_revert_rollout_supersedes_only_its_own_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    rollout(
        &home,
        "01",
        THREAD,
        &project,
        "The ledger migration is ready.",
    );
    let revert_ids = format!("{THREAD}_{REVERT_ROLLOUT}");
    rollout(
        &home,
        "02",
        &revert_ids,
        &project,
        "The invoice export retry is ready.",
    );
    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Codex)).await;

    rollout(
        &home,
        "02",
        &revert_ids,
        &project,
        "The invoice export rollback is ready.",
    );
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Codex)).await;

    assert_eq!(
        replies(&db).await,
        [
            "The invoice export rollback is ready.",
            "The ledger migration is ready.",
        ],
        "the rewrite retires the revert rollout's old reply and leaves the original rollout alone"
    );
}
