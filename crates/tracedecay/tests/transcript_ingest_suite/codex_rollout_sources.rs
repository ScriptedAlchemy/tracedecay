use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_domain::ObservationScopeV1;
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_sessions::runtime::SessionProvider;
use tracedecay_sessions::runtime::hosts::claude_observation::drain_projection_queue;
use tracedecay_sessions::runtime::hosts::codex::try_admit_codex_rollout_under_shipped_v2_source_for_test;

use crate::codex::write_jsonl;
use crate::restart_atomicity::{
    ProjectSessionTestRuntime, ingest_global_sources_for_provider, open_project_session_db,
};
use crate::support::setup;

const THREAD: &str = "codex-revert-thread";
const REVERT_ROLLOUT: &str = "codex-revert-rollout";
const LEDGER_MIGRATION: &str = "The ledger migration is ready.";
const LEDGER_BACKFILL: &str = "The ledger backfill is ready.";
const INVOICE_RETRY: &str = "The invoice export retry is ready.";
const INVOICE_ROLLBACK: &str = "The invoice export rollback is ready.";
const SHARED_QUESTION: &str = "The shared ledger question is ready.";

fn session_meta(day: &str, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "timestamp": format!("2026-01-{day}T00:00:00.000Z"),
        "type": "session_meta",
        "payload": {"id": THREAD, "cwd": cwd.to_string_lossy()}
    })
}

fn agent_message(day: &str, second: usize, reply: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": format!("2026-01-{day}T00:00:{second:02}.000Z"),
        "type": "event_msg",
        "payload": {"type": "agent_message", "message": reply}
    })
}

fn rollout_path(home: &Path, day: &str, file_ids: &str) -> PathBuf {
    let dir = home.join(format!(".codex/sessions/2026/01/{day}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("rollout-2026-01-{day}T00-00-00-{file_ids}.jsonl"))
}

fn rollout(home: &Path, day: &str, file_ids: &str, cwd: &Path, replies: &[&str]) -> PathBuf {
    let mut lines = vec![session_meta(day, cwd)];
    lines.extend(
        replies
            .iter()
            .enumerate()
            .map(|(index, reply)| agent_message(day, index + 1, reply)),
    );
    let path = rollout_path(home, day, file_ids);
    write_jsonl(&path, &lines);
    path
}

fn thread_rollout(home: &Path, cwd: &Path, replies: &[&str]) -> PathBuf {
    rollout(home, "01", THREAD, cwd, replies)
}

fn revert_rollout(home: &Path, cwd: &Path, replies: &[&str]) -> PathBuf {
    rollout(
        home,
        "02",
        &format!("{THREAD}_{REVERT_ROLLOUT}"),
        cwd,
        replies,
    )
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

fn sorted(replies: &[&str]) -> Vec<String> {
    let mut texts: Vec<String> = replies.iter().map(|reply| (*reply).to_owned()).collect();
    texts.sort();
    texts
}

async fn ingest(home: &Path, db: &ProjectSessionTestRuntime, project: &Path) {
    ingest_global_sources_for_provider(home, db, project, Some(SessionProvider::Codex)).await;
}

async fn shipped_v2_pass(db: &ProjectSessionTestRuntime, project: &Path, rollouts: &[&Path]) {
    let facade = db.runtime().facade();
    for path in rollouts {
        try_admit_codex_rollout_under_shipped_v2_source_for_test(
            path,
            project,
            db.project_id().clone(),
            &facade,
        )
        .await
        .unwrap();
    }
    drain_projection_queue(
        &facade,
        &ObservationScopeV1::Project {
            project_id: db.project_id().clone(),
        },
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
}

async fn assert_upgrade_leaves(
    home: &Path,
    db: &ProjectSessionTestRuntime,
    project: &Path,
    before: &[&str],
    expected: &[&str],
) {
    assert_eq!(replies(db).await, sorted(before), "before the upgrade");
    for pass in 1..=2 {
        ingest(home, db, project).await;
        let after = replies(db).await;
        for reply in sorted(before) {
            assert!(
                after.contains(&reply),
                "pass {pass} lost {reply:?} that was searchable before the upgrade: {after:?}"
            );
        }
        assert_eq!(after, sorted(expected), "pass {pass}");
    }
}

#[tokio::test]
async fn reverted_codex_thread_projects_both_live_rollouts() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let thread = thread_rollout(&home, &project, &[LEDGER_MIGRATION, LEDGER_BACKFILL]);
    revert_rollout(&home, &project, &[INVOICE_RETRY, INVOICE_ROLLBACK]);

    let db = open_project_session_db(&project).await.unwrap();
    for _ in 0..2 {
        ingest(&home, &db, &project).await;
        assert_eq!(
            replies(&db).await,
            sorted(&[
                LEDGER_MIGRATION,
                LEDGER_BACKFILL,
                INVOICE_RETRY,
                INVOICE_ROLLBACK
            ]),
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
        &thread,
    );
}

#[tokio::test]
async fn rewriting_a_revert_rollout_supersedes_only_its_own_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    thread_rollout(&home, &project, &[LEDGER_MIGRATION]);
    revert_rollout(&home, &project, &[INVOICE_RETRY]);
    let db = open_project_session_db(&project).await.unwrap();
    ingest(&home, &db, &project).await;

    revert_rollout(&home, &project, &[INVOICE_ROLLBACK]);
    ingest(&home, &db, &project).await;

    assert_eq!(
        replies(&db).await,
        sorted(&[LEDGER_MIGRATION, INVOICE_ROLLBACK]),
        "the rewrite retires the revert rollout's old reply and leaves the thread rollout alone"
    );
}

#[tokio::test]
async fn rewriting_the_thread_rollout_supersedes_only_its_own_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    thread_rollout(&home, &project, &[LEDGER_MIGRATION]);
    revert_rollout(&home, &project, &[INVOICE_RETRY]);
    let db = open_project_session_db(&project).await.unwrap();
    ingest(&home, &db, &project).await;

    thread_rollout(&home, &project, &[LEDGER_BACKFILL]);
    ingest(&home, &db, &project).await;

    assert_eq!(
        replies(&db).await,
        sorted(&[LEDGER_BACKFILL, INVOICE_RETRY]),
        "the rewrite retires the thread rollout's old reply and leaves the revert rollout alone"
    );
}

#[tokio::test]
async fn a_record_both_rollouts_hold_outlives_a_rewrite_of_either() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let shared = agent_message("01", 1, SHARED_QUESTION);
    let thread = rollout_path(&home, "01", THREAD);
    let revert = rollout_path(&home, "02", &format!("{THREAD}_{REVERT_ROLLOUT}"));
    write_jsonl(
        &thread,
        &[
            session_meta("01", &project),
            shared.clone(),
            agent_message("01", 2, LEDGER_MIGRATION),
        ],
    );
    write_jsonl(
        &revert,
        &[
            session_meta("02", &project),
            shared.clone(),
            agent_message("02", 2, INVOICE_RETRY),
        ],
    );
    let db = open_project_session_db(&project).await.unwrap();
    ingest(&home, &db, &project).await;
    assert_eq!(
        replies(&db).await,
        sorted(&[SHARED_QUESTION, LEDGER_MIGRATION, INVOICE_RETRY])
    );

    write_jsonl(
        &revert,
        &[
            session_meta("02", &project),
            agent_message("02", 2, INVOICE_ROLLBACK),
        ],
    );
    ingest(&home, &db, &project).await;
    assert_eq!(
        replies(&db).await,
        sorted(&[SHARED_QUESTION, LEDGER_MIGRATION, INVOICE_ROLLBACK]),
        "the thread rollout still holds the shared record"
    );

    write_jsonl(
        &thread,
        &[
            session_meta("01", &project),
            agent_message("01", 2, LEDGER_BACKFILL),
        ],
    );
    ingest(&home, &db, &project).await;
    assert_eq!(
        replies(&db).await,
        sorted(&[LEDGER_BACKFILL, INVOICE_ROLLBACK]),
        "no rollout holds the shared record any more"
    );
}

#[tokio::test]
async fn upgrade_keeps_both_rollouts_the_shipped_v2_binary_imported_together() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let thread = thread_rollout(&home, &project, &[LEDGER_MIGRATION, LEDGER_BACKFILL]);
    let revert = revert_rollout(&home, &project, &[INVOICE_RETRY, INVOICE_ROLLBACK]);
    let db = open_project_session_db(&project).await.unwrap();
    shipped_v2_pass(&db, &project, &[&revert, &thread]).await;

    assert_upgrade_leaves(
        &home,
        &db,
        &project,
        &[LEDGER_MIGRATION, LEDGER_BACKFILL],
        &[
            LEDGER_MIGRATION,
            LEDGER_BACKFILL,
            INVOICE_RETRY,
            INVOICE_ROLLBACK,
        ],
    )
    .await;
}

#[tokio::test]
async fn upgrade_revives_a_revert_rollout_the_shipped_v2_binary_retired() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let thread = thread_rollout(&home, &project, &[LEDGER_MIGRATION, LEDGER_BACKFILL]);
    let db = open_project_session_db(&project).await.unwrap();
    shipped_v2_pass(&db, &project, &[&thread]).await;
    assert_eq!(
        replies(&db).await,
        sorted(&[LEDGER_MIGRATION, LEDGER_BACKFILL])
    );
    let revert = revert_rollout(&home, &project, &[INVOICE_RETRY, INVOICE_ROLLBACK]);
    shipped_v2_pass(&db, &project, &[&revert, &thread]).await;

    assert_upgrade_leaves(
        &home,
        &db,
        &project,
        &[],
        &[INVOICE_RETRY, INVOICE_ROLLBACK],
    )
    .await;
}

#[tokio::test]
async fn upgrade_keeps_a_revert_rollout_a_shipped_v2_hook_admitted_last() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let thread = thread_rollout(&home, &project, &[LEDGER_MIGRATION, LEDGER_BACKFILL]);
    let revert = revert_rollout(&home, &project, &[INVOICE_RETRY, INVOICE_ROLLBACK]);
    let db = open_project_session_db(&project).await.unwrap();
    shipped_v2_pass(&db, &project, &[&thread, &revert]).await;

    assert_upgrade_leaves(
        &home,
        &db,
        &project,
        &[INVOICE_RETRY, INVOICE_ROLLBACK],
        &[INVOICE_RETRY, INVOICE_ROLLBACK],
    )
    .await;
}
