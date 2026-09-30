use tempfile::TempDir;
use tracedecay_global_db::ParseOffset;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_store::{TranscriptStore, TranscriptStoreError, TranscriptWriteBatch};

async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct StoreCounts {
    sessions: i64,
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
        raw_messages: counts.1,
        raw_fts: counts.2,
        all_raw_fts: counts.3,
        summaries: counts.4,
        cursors: counts.5,
    }
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
