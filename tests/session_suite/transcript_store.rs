use tempfile::TempDir;
use tracedecay::global_db::ParseOffset;
use tracedecay::store::GlobalDbTranscriptStore;
use tracedecay_store::{TranscriptStore, TranscriptStoreError, TranscriptWriteBatch};

use crate::common::{
    global_message as sample_message, global_session as sample_session, isolated_lcm_db_path,
    open_lcm_db,
};

#[derive(Debug, PartialEq, Eq)]
struct StoreCounts {
    sessions: i64,
    projections: i64,
    raw_messages: i64,
    raw_fts: i64,
    all_raw_fts: i64,
    summaries: i64,
    cursors: i64,
}

async fn store_counts(
    tmp: &TempDir,
    provider: &str,
    session_id: &str,
    transcript_path: &std::path::Path,
) -> StoreCounts {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT
                (SELECT COUNT(*) FROM sessions
                 WHERE provider = ?1 AND session_id = ?2),
                (SELECT COUNT(*) FROM session_messages
                 WHERE provider = ?1 AND session_id = ?2),
                (SELECT COUNT(*) FROM lcm_raw_messages
                 WHERE provider = ?1 AND session_id = ?2),
                (SELECT COUNT(*) FROM lcm_raw_messages_fts
                 JOIN lcm_raw_messages raw
                   ON raw.store_id = lcm_raw_messages_fts.rowid
                 WHERE raw.provider = ?1 AND raw.session_id = ?2),
                (SELECT COUNT(*) FROM lcm_raw_messages_fts),
                (SELECT COUNT(*) FROM lcm_summary_nodes
                 WHERE provider = ?1 AND session_id = ?2),
                (SELECT COUNT(*) FROM parse_offsets
                 WHERE file_path = ?3)",
            libsql::params![
                provider,
                session_id,
                transcript_path.to_string_lossy().as_ref(),
            ],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    StoreCounts {
        sessions: row.get(0).unwrap(),
        projections: row.get(1).unwrap(),
        raw_messages: row.get(2).unwrap(),
        raw_fts: row.get(3).unwrap(),
        all_raw_fts: row.get(4).unwrap(),
        summaries: row.get(5).unwrap(),
        cursors: row.get(6).unwrap(),
    }
}

fn summary_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
) -> tracedecay::sessions::SessionMessageRecord {
    let mut summary = sample_message(
        provider,
        message_id,
        session_id,
        "Compacted transcript summary.",
    );
    summary.ordinal = 2;
    summary.kind = Some("summary".to_string());
    summary.metadata_json = Some(
        serde_json::json!({
            "source": "codex_context_compacted",
            "summary_body": "plaintext",
            "codex_compaction_depth": 1
        })
        .to_string(),
    );
    summary
}

#[tokio::test]
async fn transcript_batch_survives_restart_and_replay_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let transcript_path = tmp.path().join("cursor-restart.jsonl");
    let mut session = sample_session("cursor", "restart-session", "project-a");
    session.transcript_path = Some(transcript_path.to_string_lossy().to_string());
    let mut second = sample_message(
        "cursor",
        "restart-message-2",
        "restart-session",
        "Second durable restart message.",
    );
    second.ordinal = 2;
    let messages = vec![
        sample_message(
            "cursor",
            "restart-message-1",
            "restart-session",
            "First durable restart message.",
        ),
        second,
    ];
    let offset = ParseOffset {
        byte_offset: 512,
        mtime: 1_800_000_000,
        file_id: 42,
    };

    let db = open_lcm_db(&tmp).await;
    GlobalDbTranscriptStore::new(&db)
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                session.clone(),
                messages.clone(),
                ParseOffset::default(),
                offset,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    drop(db);

    let reopened = open_lcm_db(&tmp).await;
    assert_eq!(
        reopened
            .get_parse_offset(transcript_path.to_string_lossy().as_ref())
            .await,
        Some(offset)
    );
    GlobalDbTranscriptStore::new(&reopened)
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(session, messages, offset, offset).unwrap(),
        )
        .await
        .unwrap();
    drop(reopened);

    assert_eq!(
        store_counts(&tmp, "cursor", "restart-session", &transcript_path).await,
        StoreCounts {
            sessions: 1,
            projections: 2,
            raw_messages: 2,
            raw_fts: 2,
            all_raw_fts: 2,
            summaries: 0,
            cursors: 1,
        }
    );
}

#[tokio::test]
async fn late_cursor_failure_rolls_back_every_transcript_write_then_retries() {
    let tmp = TempDir::new().unwrap();
    let transcript_path = tmp.path().join("late-cursor-failure.jsonl");
    let mut session = sample_session("codex", "atomic-session", "project-a");
    session.transcript_path = Some(transcript_path.to_string_lossy().to_string());
    let source = sample_message(
        "codex",
        "source-message",
        "atomic-session",
        "Visible source before compaction.",
    );
    let mut summary = sample_message(
        "codex",
        "summary-message",
        "atomic-session",
        "Compacted transcript summary.",
    );
    summary.ordinal = 2;
    summary.kind = Some("summary".to_string());
    summary.metadata_json = Some(
        serde_json::json!({
            "source": "codex_context_compacted",
            "summary_body": "plaintext",
            "codex_compaction_depth": 1
        })
        .to_string(),
    );
    let batch = TranscriptWriteBatch::upsert(
        session,
        vec![source, summary],
        ParseOffset::default(),
        ParseOffset {
            byte_offset: 384,
            mtime: 1_800_000_100,
            file_id: 43,
        },
    )
    .unwrap();

    let db = open_lcm_db(&tmp).await;
    let trigger_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let trigger_conn = trigger_db.connect().unwrap();
    trigger_conn
        .execute_batch(
            "CREATE TRIGGER fail_parse_offset_insert
             BEFORE INSERT ON parse_offsets
             BEGIN
                SELECT RAISE(ABORT, 'late parse offset failure');
             END;",
        )
        .await
        .unwrap();

    let error = GlobalDbTranscriptStore::new(&db)
        .persist_transcript_batch(batch.clone())
        .await
        .expect_err("the late cursor write must fail the batch");
    assert!(matches!(error, TranscriptStoreError::Storage { .. }));
    drop(db);

    assert_eq!(
        store_counts(&tmp, "codex", "atomic-session", &transcript_path).await,
        StoreCounts {
            sessions: 0,
            projections: 0,
            raw_messages: 0,
            raw_fts: 0,
            all_raw_fts: 0,
            summaries: 0,
            cursors: 0,
        }
    );

    trigger_conn
        .execute("DROP TRIGGER fail_parse_offset_insert", ())
        .await
        .unwrap();
    drop(trigger_conn);
    drop(trigger_db);

    let reopened = open_lcm_db(&tmp).await;
    GlobalDbTranscriptStore::new(&reopened)
        .persist_transcript_batch(batch)
        .await
        .unwrap();
    drop(reopened);

    assert_eq!(
        store_counts(&tmp, "codex", "atomic-session", &transcript_path).await,
        StoreCounts {
            sessions: 1,
            projections: 2,
            raw_messages: 2,
            raw_fts: 2,
            all_raw_fts: 2,
            summaries: 1,
            cursors: 1,
        }
    );
}

#[tokio::test]
async fn invalid_batch_mutates_no_transcript_state() {
    let tmp = TempDir::new().unwrap();
    let transcript_path = tmp.path().join("invalid-batch.jsonl");
    drop(open_lcm_db(&tmp).await);
    let mut session = sample_session("cursor", "expected-session", "project-a");
    session.transcript_path = Some(transcript_path.to_string_lossy().to_string());
    let error = TranscriptWriteBatch::upsert(
        session,
        vec![sample_message(
            "cursor",
            "invalid-message",
            "invalid-session",
            "must never be persisted",
        )],
        ParseOffset::default(),
        ParseOffset {
            byte_offset: 77,
            mtime: 1_715_000_350,
            file_id: 12,
        },
    )
    .expect_err("a mismatched message identity must be rejected");

    assert!(matches!(
        error,
        TranscriptStoreError::MessageIdentityMismatch { .. }
    ));
    assert_eq!(
        store_counts(&tmp, "cursor", "expected-session", &transcript_path).await,
        StoreCounts {
            sessions: 0,
            projections: 0,
            raw_messages: 0,
            raw_fts: 0,
            all_raw_fts: 0,
            summaries: 0,
            cursors: 0,
        }
    );
}

#[tokio::test]
async fn concurrent_batches_reject_stale_owner_without_duplicate_rows() {
    let tmp = TempDir::new().unwrap();
    let transcript_path = tmp.path().join("concurrent.jsonl");
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbTranscriptStore::new(&db);
    let mut first_session = sample_session("cursor", "concurrent-first", "project-a");
    first_session.transcript_path = Some(transcript_path.to_string_lossy().to_string());
    let mut second_session = sample_session("cursor", "concurrent-second", "project-a");
    second_session.transcript_path = Some(transcript_path.to_string_lossy().to_string());
    let first_message = sample_message(
        "cursor",
        "concurrent-first-message",
        "concurrent-first",
        "first committed transcript message",
    );
    let second_message = sample_message(
        "cursor",
        "concurrent-second-message",
        "concurrent-second",
        "second committed transcript message",
    );
    let first_summary = summary_message("cursor", "concurrent-first-summary", "concurrent-first");
    let second_summary =
        summary_message("cursor", "concurrent-second-summary", "concurrent-second");
    let first_offset = ParseOffset {
        byte_offset: 100,
        mtime: 1_000,
        file_id: 7,
    };
    let second_offset = ParseOffset {
        byte_offset: 200,
        mtime: 2_000,
        file_id: 7,
    };

    let (first_result, second_result) = tokio::join!(
        store.persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                first_session,
                vec![first_message, first_summary],
                ParseOffset::default(),
                first_offset,
            )
            .unwrap(),
        ),
        store.persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                second_session,
                vec![second_message, second_summary],
                ParseOffset::default(),
                second_offset,
            )
            .unwrap(),
        ),
    );

    let (winner_offset, winner_session, loser_session, conflict) =
        match (first_result, second_result) {
            (Ok(()), Err(conflict @ TranscriptStoreError::Conflict { .. })) => (
                first_offset,
                "concurrent-first",
                "concurrent-second",
                conflict,
            ),
            (Err(conflict @ TranscriptStoreError::Conflict { .. }), Ok(())) => (
                second_offset,
                "concurrent-second",
                "concurrent-first",
                conflict,
            ),
            outcomes => panic!("expected exactly one commit and one conflict, got {outcomes:?}"),
        };
    match conflict {
        TranscriptStoreError::Conflict {
            transcript_path: conflict_path,
            expected,
            actual,
        } => {
            assert_eq!(conflict_path, transcript_path);
            assert_eq!(expected, ParseOffset::default());
            assert_eq!(actual, winner_offset);
        }
        _ => unreachable!(),
    }

    assert_eq!(
        db.get_parse_offset(transcript_path.to_string_lossy().as_ref())
            .await,
        Some(winner_offset)
    );
    drop(db);
    assert_eq!(
        store_counts(&tmp, "cursor", winner_session, &transcript_path).await,
        StoreCounts {
            sessions: 1,
            projections: 2,
            raw_messages: 2,
            raw_fts: 2,
            all_raw_fts: 2,
            summaries: 1,
            cursors: 1,
        }
    );
    assert_eq!(
        store_counts(&tmp, "cursor", loser_session, &transcript_path).await,
        StoreCounts {
            sessions: 0,
            projections: 0,
            raw_messages: 0,
            raw_fts: 0,
            all_raw_fts: 2,
            summaries: 0,
            cursors: 1,
        }
    );
}

#[tokio::test]
async fn concurrent_empty_advances_reject_stale_owner_without_rows() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbTranscriptStore::new(&db);
    let transcript_path = tmp.path().join("parsed-but-empty.jsonl");
    let first_offset = ParseOffset {
        byte_offset: 80,
        mtime: 1_000,
        file_id: 9,
    };
    let second_offset = ParseOffset {
        byte_offset: 160,
        mtime: 2_000,
        file_id: 9,
    };

    let (first_result, second_result) = tokio::join!(
        store.persist_transcript_batch(
            TranscriptWriteBatch::advance_offset(
                transcript_path.clone(),
                ParseOffset::default(),
                first_offset,
            )
            .unwrap(),
        ),
        store.persist_transcript_batch(
            TranscriptWriteBatch::advance_offset(
                transcript_path.clone(),
                ParseOffset::default(),
                second_offset,
            )
            .unwrap(),
        ),
    );

    let (winner_offset, conflict) = match (first_result, second_result) {
        (Ok(()), Err(conflict @ TranscriptStoreError::Conflict { .. })) => (first_offset, conflict),
        (Err(conflict @ TranscriptStoreError::Conflict { .. }), Ok(())) => {
            (second_offset, conflict)
        }
        outcomes => panic!("expected exactly one advance and one conflict, got {outcomes:?}"),
    };
    match conflict {
        TranscriptStoreError::Conflict {
            transcript_path: conflict_path,
            expected,
            actual,
        } => {
            assert_eq!(conflict_path, transcript_path);
            assert_eq!(expected, ParseOffset::default());
            assert_eq!(actual, winner_offset);
        }
        _ => unreachable!(),
    }

    assert_eq!(
        db.get_parse_offset(transcript_path.to_string_lossy().as_ref())
            .await,
        Some(winner_offset)
    );
    drop(db);
    assert_eq!(
        store_counts(&tmp, "cursor", "parsed-but-empty", &transcript_path).await,
        StoreCounts {
            sessions: 0,
            projections: 0,
            raw_messages: 0,
            raw_fts: 0,
            all_raw_fts: 0,
            summaries: 0,
            cursors: 1,
        }
    );
}
