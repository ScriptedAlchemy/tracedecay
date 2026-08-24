use tempfile::TempDir;
use tracedecay::global_db::ParseOffset;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_store::{TranscriptStore, TranscriptStoreError, TranscriptWriteBatch};
use tracedecay_usecases::host_admission::HostAdmissionScope;

use crate::common::{global_message as sample_message, global_session as sample_session};

async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

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
    runtime: &HostAdmissionTestRuntimeV1,
    provider: &str,
    session_id: &str,
    transcript_path: &std::path::Path,
) -> StoreCounts {
    let counts = runtime
        .transcript_store_counts_for_test(
            HostAdmissionScope::Profile,
            provider,
            session_id,
            transcript_path,
        )
        .await
        .unwrap();
    StoreCounts {
        sessions: counts.0,
        projections: counts.1,
        raw_messages: counts.2,
        raw_fts: counts.3,
        all_raw_fts: counts.4,
        summaries: counts.5,
        cursors: counts.6,
    }
}

fn summary_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
) -> tracedecay_sessions::runtime::SessionMessageRecord {
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

    let db = profile_runtime(&tmp).await;
    db.transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap()
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

    let reopened = profile_runtime(&tmp).await;
    assert_eq!(
        reopened
            .parse_offset_for_test(
                HostAdmissionScope::Profile,
                transcript_path.to_string_lossy().as_ref(),
            )
            .await
            .unwrap(),
        Some(offset)
    );
    reopened
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap()
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(session, messages, offset, offset).unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        store_counts(&reopened, "cursor", "restart-session", &transcript_path).await,
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
    let payload_dir =
        tracedecay_sessions::runtime::lcm::payload::payload_dir(&tmp.path().join(".tracedecay"));
    std::fs::create_dir_all(&payload_dir).unwrap();
    let sentinel_path = payload_dir.join("preexisting.payload");
    std::fs::write(&sentinel_path, "must survive rollback").unwrap();
    let mut session = sample_session("codex", "atomic-session", "project-a");
    session.transcript_path = Some(transcript_path.to_string_lossy().to_string());
    let mut source = sample_message(
        "codex",
        "source-message",
        "atomic-session",
        &format!("oversized tool payload\n{}", "P".repeat(300_000)),
    );
    source.role = "tool".to_string();
    source.kind = Some("tool_result".to_string());
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

    let db = profile_runtime(&tmp).await;
    db.set_parse_offset_insert_failure_for_test(HostAdmissionScope::Profile, true)
        .await
        .unwrap();

    let error = db
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap()
        .persist_transcript_batch(batch.clone())
        .await
        .expect_err("the late cursor write must fail the batch");
    assert!(matches!(error, TranscriptStoreError::Storage { .. }));
    assert_eq!(
        db.parse_offset_for_test(
            HostAdmissionScope::Profile,
            transcript_path.to_string_lossy().as_ref(),
        )
        .await
        .unwrap(),
        None
    );

    assert_eq!(
        std::fs::read_to_string(&sentinel_path).unwrap(),
        "must survive rollback"
    );
    let remaining_payload_files = std::fs::read_dir(&payload_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        remaining_payload_files,
        vec![std::ffi::OsString::from("preexisting.payload")]
    );

    assert_eq!(
        store_counts(&db, "codex", "atomic-session", &transcript_path).await,
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

    db.set_parse_offset_insert_failure_for_test(HostAdmissionScope::Profile, false)
        .await
        .unwrap();
    drop(db);

    let reopened = profile_runtime(&tmp).await;
    reopened
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap()
        .persist_transcript_batch(batch)
        .await
        .unwrap();
    let raw = reopened
        .lcm_load_raw_message_for_test("codex", "source-message")
        .await
        .expect("oversized message must persist on retry");
    let payload_ref = raw.payload_ref.expect("oversized message must externalize");
    assert!(payload_dir.join(payload_ref).is_file());
    assert_eq!(
        std::fs::read_to_string(&sentinel_path).unwrap(),
        "must survive rollback"
    );
    assert_eq!(
        store_counts(&reopened, "codex", "atomic-session", &transcript_path).await,
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
async fn invalid_batch_mutates_no_transcript_state() {
    let tmp = TempDir::new().unwrap();
    let transcript_path = tmp.path().join("invalid-batch.jsonl");
    let db = profile_runtime(&tmp).await;
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
        store_counts(&db, "cursor", "expected-session", &transcript_path).await,
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
async fn stale_higher_batch_is_rejected_until_reparsed_from_durable_cursor() {
    let tmp = TempDir::new().unwrap();
    let transcript_path = tmp.path().join("concurrent.jsonl");
    let db = profile_runtime(&tmp).await;
    let store = db
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let mut session = sample_session("cursor", "concurrent-session", "project-a");
    session.transcript_path = Some(transcript_path.to_string_lossy().to_string());
    let first_message = sample_message(
        "cursor",
        "concurrent-first-message",
        "concurrent-session",
        "first committed transcript message",
    );
    let mut second_message = sample_message(
        "cursor",
        "concurrent-second-message",
        "concurrent-session",
        "second committed transcript message",
    );
    second_message.ordinal = 2;
    let mut summary = summary_message("cursor", "concurrent-summary", "concurrent-session");
    summary.ordinal = 3;
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

    store
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                session.clone(),
                vec![first_message.clone()],
                ParseOffset::default(),
                first_offset,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let stale_higher_error = store
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                session.clone(),
                vec![
                    first_message.clone(),
                    second_message.clone(),
                    summary.clone(),
                ],
                ParseOffset::default(),
                second_offset,
            )
            .unwrap(),
        )
        .await
        .expect_err("a pre-parsed batch must not change its observed cursor and retry");
    assert!(matches!(
        stale_higher_error,
        TranscriptStoreError::Conflict {
            expected,
            actual,
            ..
        } if expected == ParseOffset::default() && actual == first_offset
    ));

    assert_eq!(
        db.parse_offset_for_test(
            HostAdmissionScope::Profile,
            transcript_path.to_string_lossy().as_ref(),
        )
        .await
        .unwrap(),
        Some(first_offset)
    );
    assert!(
        db.session_message_for_test(
            HostAdmissionScope::Profile,
            "cursor",
            "concurrent-second-message",
        )
        .await
        .unwrap()
        .is_none(),
        "the stale parse products must roll back with the cursor conflict"
    );

    // A runtime boundary may re-read the winner and reparse the suffix/full
    // source. That fresh batch carries the actually observed durable cursor.
    store
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                session,
                vec![first_message, second_message, summary],
                first_offset,
                second_offset,
            )
            .unwrap(),
        )
        .await
        .expect("a freshly parsed batch may advance from the durable winner");

    let mut stale_session = sample_session("cursor", "concurrent-session", "project-a");
    stale_session.transcript_path = Some(transcript_path.to_string_lossy().to_string());
    let stale_error = store
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                stale_session,
                vec![sample_message(
                    "cursor",
                    "concurrent-stale-message",
                    "concurrent-session",
                    "stale transcript message",
                )],
                ParseOffset::default(),
                first_offset,
            )
            .unwrap(),
        )
        .await
        .expect_err("a lower stale cursor must not replace the converged maximum");
    assert!(matches!(
        stale_error,
        TranscriptStoreError::Conflict {
            actual,
            ..
        } if actual == second_offset
    ));
    assert_eq!(
        store_counts(&db, "cursor", "concurrent-session", &transcript_path).await,
        StoreCounts {
            sessions: 1,
            projections: 3,
            raw_messages: 3,
            raw_fts: 3,
            all_raw_fts: 3,
            summaries: 0,
            cursors: 1,
        }
    );
}

#[tokio::test]
async fn concurrent_full_batches_converge_without_split_brain_or_partial_writes() {
    let tmp = TempDir::new().unwrap();
    let transcript_path = tmp.path().join("concurrent-full-batches.jsonl");
    let db = profile_runtime(&tmp).await;
    let first_store = db
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let second_store = db
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let mut session = sample_session("cursor", "concurrent-full-session", "project-a");
    session.transcript_path = Some(transcript_path.to_string_lossy().to_string());
    let first_message = sample_message(
        "cursor",
        "concurrent-full-message-1",
        "concurrent-full-session",
        "first concurrent transcript message",
    );
    let mut second_message = sample_message(
        "cursor",
        "concurrent-full-message-2",
        "concurrent-full-session",
        "second concurrent transcript message",
    );
    second_message.ordinal = 2;
    let mut summary = summary_message(
        "cursor",
        "concurrent-full-summary",
        "concurrent-full-session",
    );
    summary.ordinal = 3;
    let first_offset = ParseOffset {
        byte_offset: 100,
        mtime: 1_000,
        file_id: 7,
    };
    let higher_offset = ParseOffset {
        byte_offset: 200,
        mtime: 2_000,
        file_id: 7,
    };
    let first_batch = TranscriptWriteBatch::upsert(
        session.clone(),
        vec![first_message.clone()],
        ParseOffset::default(),
        first_offset,
    )
    .unwrap();
    let competing_batch = TranscriptWriteBatch::upsert(
        session.clone(),
        vec![second_message.clone()],
        ParseOffset::default(),
        first_offset,
    )
    .unwrap();

    let (first_result, competing_result) = tokio::join!(
        first_store.persist_transcript_batch(first_batch),
        second_store.persist_transcript_batch(competing_batch),
    );

    let conflict_actual = match (first_result, competing_result) {
        (Ok(()), Err(TranscriptStoreError::Conflict { actual, .. }))
        | (Err(TranscriptStoreError::Conflict { actual, .. }), Ok(())) => actual,
        outcomes => panic!("exactly one concurrent full batch must commit, got {outcomes:?}"),
    };
    assert_eq!(conflict_actual, first_offset);
    assert_eq!(
        db.parse_offset_for_test(
            HostAdmissionScope::Profile,
            transcript_path.to_string_lossy().as_ref(),
        )
        .await
        .unwrap(),
        Some(first_offset)
    );
    assert_eq!(
        store_counts(&db, "cursor", "concurrent-full-session", &transcript_path,).await,
        StoreCounts {
            sessions: 1,
            projections: 1,
            raw_messages: 1,
            raw_fts: 1,
            all_raw_fts: 1,
            summaries: 0,
            cursors: 1,
        }
    );

    first_store
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                session.clone(),
                vec![first_message, second_message, summary],
                conflict_actual,
                higher_offset,
            )
            .unwrap(),
        )
        .await
        .expect("a freshly parsed batch must advance from the returned durable cursor");
    assert_eq!(
        db.parse_offset_for_test(
            HostAdmissionScope::Profile,
            transcript_path.to_string_lossy().as_ref(),
        )
        .await
        .unwrap(),
        Some(higher_offset)
    );

    let stale_error = first_store
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                session.clone(),
                vec![sample_message(
                    "cursor",
                    "concurrent-full-stale-message",
                    "concurrent-full-session",
                    "stale owner must not mutate state",
                )],
                first_offset,
                ParseOffset {
                    byte_offset: 150,
                    mtime: 1_500,
                    file_id: 7,
                },
            )
            .unwrap(),
        )
        .await
        .expect_err("a stale owner behind the durable maximum must be rejected");
    assert!(matches!(
        stale_error,
        TranscriptStoreError::Conflict { actual, .. } if actual == higher_offset
    ));

    let competing_error = second_store
        .persist_transcript_batch(
            TranscriptWriteBatch::upsert(
                session,
                vec![sample_message(
                    "cursor",
                    "concurrent-full-competing-message",
                    "concurrent-full-session",
                    "competing file identity must not mutate state",
                )],
                ParseOffset::default(),
                ParseOffset {
                    byte_offset: 400,
                    mtime: 4_000,
                    file_id: 8,
                },
            )
            .unwrap(),
        )
        .await
        .expect_err("a competing file identity must be rejected");
    assert!(matches!(
        competing_error,
        TranscriptStoreError::Conflict { actual, .. } if actual == higher_offset
    ));

    assert_eq!(
        store_counts(&db, "cursor", "concurrent-full-session", &transcript_path,).await,
        StoreCounts {
            sessions: 1,
            projections: 3,
            raw_messages: 3,
            raw_fts: 3,
            all_raw_fts: 3,
            summaries: 1,
            cursors: 1,
        }
    );
}

#[tokio::test]
async fn concurrent_empty_advances_converge_to_highest_compatible_offset_without_rows() {
    let tmp = TempDir::new().unwrap();
    let db = profile_runtime(&tmp).await;
    let store = db
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap();
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

    match (first_result, second_result) {
        (Ok(()), Ok(())) | (Err(TranscriptStoreError::Conflict { .. }), Ok(())) => {}
        outcomes => panic!("higher compatible cursor must converge, got {outcomes:?}"),
    }

    assert_eq!(
        db.parse_offset_for_test(
            HostAdmissionScope::Profile,
            transcript_path.to_string_lossy().as_ref(),
        )
        .await
        .unwrap(),
        Some(second_offset)
    );
    let incompatible_error = store
        .persist_transcript_batch(
            TranscriptWriteBatch::advance_offset(
                transcript_path.clone(),
                ParseOffset::default(),
                ParseOffset {
                    byte_offset: 320,
                    mtime: 3_000,
                    file_id: 10,
                },
            )
            .unwrap(),
        )
        .await
        .expect_err("a different file identity must not be merged as an append");
    assert!(matches!(
        incompatible_error,
        TranscriptStoreError::Conflict {
            actual,
            ..
        } if actual == second_offset
    ));
    let regressing_mtime_error = store
        .persist_transcript_batch(
            TranscriptWriteBatch::advance_offset(
                transcript_path.clone(),
                ParseOffset::default(),
                ParseOffset {
                    byte_offset: 320,
                    mtime: 500,
                    file_id: second_offset.file_id,
                },
            )
            .unwrap(),
        )
        .await
        .expect_err("a higher byte offset must not move the file mtime backwards");
    assert!(matches!(
        regressing_mtime_error,
        TranscriptStoreError::Conflict { actual, .. } if actual == second_offset
    ));
    assert_eq!(
        store_counts(&db, "cursor", "parsed-but-empty", &transcript_path).await,
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

#[tokio::test]
async fn duplicate_empty_advances_are_idempotent_under_concurrency() {
    let tmp = TempDir::new().unwrap();
    let db = profile_runtime(&tmp).await;
    let first_store = db
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let second_store = db
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let transcript_path = tmp.path().join("duplicate-empty.jsonl");
    let next_offset = ParseOffset {
        byte_offset: 96,
        mtime: 1_500,
        file_id: 11,
    };

    let (first_result, second_result) = tokio::join!(
        first_store.persist_transcript_batch(
            TranscriptWriteBatch::advance_offset(
                transcript_path.clone(),
                ParseOffset::default(),
                next_offset,
            )
            .unwrap(),
        ),
        second_store.persist_transcript_batch(
            TranscriptWriteBatch::advance_offset(
                transcript_path.clone(),
                ParseOffset::default(),
                next_offset,
            )
            .unwrap(),
        ),
    );

    assert!(first_result.is_ok(), "first duplicate: {first_result:?}");
    assert!(second_result.is_ok(), "second duplicate: {second_result:?}");
    assert_eq!(
        db.parse_offset_for_test(
            HostAdmissionScope::Profile,
            transcript_path.to_string_lossy().as_ref(),
        )
        .await
        .unwrap(),
        Some(next_offset)
    );
    assert_eq!(
        store_counts(&db, "cursor", "duplicate-empty", &transcript_path).await,
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

#[tokio::test]
async fn content_hash_offsets_never_retry_by_numeric_order() {
    let tmp = TempDir::new().unwrap();
    let db = profile_runtime(&tmp).await;
    let store = db
        .transcript_store_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let transcript_path = tmp.path().join("content-hash.json");
    let durable_hash = ParseOffset {
        byte_offset: 900,
        mtime: 1_000,
        file_id: 0,
    };

    store
        .persist_transcript_batch(
            TranscriptWriteBatch::advance_offset(
                transcript_path.clone(),
                ParseOffset::default(),
                durable_hash,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let error = store
        .persist_transcript_batch(
            TranscriptWriteBatch::advance_offset(
                transcript_path.clone(),
                ParseOffset::default(),
                ParseOffset {
                    byte_offset: 1_200,
                    mtime: 1_000,
                    file_id: 0,
                },
            )
            .unwrap(),
        )
        .await
        .expect_err("content hashes are identities, not monotonic byte offsets");

    assert!(matches!(
        error,
        TranscriptStoreError::Conflict {
            actual,
            ..
        } if actual == durable_hash
    ));
    assert_eq!(
        db.parse_offset_for_test(
            HostAdmissionScope::Profile,
            transcript_path.to_string_lossy().as_ref(),
        )
        .await
        .unwrap(),
        Some(durable_hash)
    );
}
