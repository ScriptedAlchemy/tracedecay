use std::collections::BTreeMap;

use tracedecay_runtime_core::db::engine::{TestConnection, TransactionBehavior};

use super::*;
use crate::runtime::git_correlation::ensure_git_correlation_receipt_schema_in_transaction;

fn progress(key: GitHistoryProgressKey) -> GitHistoryProgressRow {
    GitHistoryProgressRow {
        key,
        activity_timestamp: 201,
        provider: "codex".to_string(),
        session_id: "session-1".to_string(),
        project_path: "/repo/linked".to_string(),
        window_start: 100,
        window_end: 200,
        worktree: b"/repo/linked".to_vec(),
        worktree_identity: b"worktree-identity".to_vec(),
        git_dir: b"/repo/.git/worktrees/linked".to_vec(),
        git_dir_identity: b"git-dir-identity".to_vec(),
        common_dir: b"/repo/.git".to_vec(),
        common_dir_identity: b"common-dir-identity".to_vec(),
        generation: 0,
        scan_mode: GitHistoryScanMode::ReflogCapture,
        reflog_path: b"/repo/.git/logs/HEAD".to_vec(),
        reflog_byte_offset: 512,
        reflog_byte_length: 512,
        source_generation: "sha256:source-generation".to_string(),
        reflog_digest: initial_reflog_content_chain().to_string(),
        capture_target_offset: None,
        verify_byte_offset: 512,
        verify_digest: initial_reflog_content_chain().to_string(),
        source_head_referent: Some(b"refs/heads/main".to_vec()),
        source_head_oid: "aaaaaaaa".to_string(),
        cursor_head_state: GitHistoryCursorHeadState::LocalBranch,
        cursor_head_branch: Some("main".to_string()),
        cursor_oid: "aaaaaaaa".to_string(),
        segment_end: 200,
        segment_tip_oid: "aaaaaaaa".to_string(),
        segment_cursor: 0,
        emitted_count: 0,
        consulted_refs: BTreeMap::from([
            (b"refs/heads/main".to_vec(), Some("aaaaaaaa".to_string())),
            (b"refs/tags/\xffbinary".to_vec(), None),
        ]),
    }
}

fn segment(key: GitHistoryProgressKey) -> GitHistorySegmentRow {
    GitHistorySegmentRow {
        key,
        ordinal: 0,
        branch: Some("main".to_string()),
        start_ts: 100,
        end_ts: 200,
        tip_oid: "aaaaaaaa".to_string(),
        applied: true,
        completed: false,
    }
}

#[test]
fn consulted_ref_seal_is_byte_exact_canonical_and_rejects_bad_entries() {
    let refs = BTreeMap::from([(b"a".to_vec(), Some("oid".to_string())), (vec![0xff], None)]);
    let json = encode_consulted_refs(&refs).unwrap();
    assert_eq!(
        json,
        r#"[{"name_hex":"61","oid":"oid"},{"name_hex":"ff","oid":null}]"#
    );
    assert_eq!(decode_consulted_refs(&json).unwrap(), refs);
    for invalid in [
        r#"[{"name_hex":"zz","oid":null}]"#,
        r#"[{"name_hex":"61","oid":null},{"name_hex":"61","oid":"oid"}]"#,
        r#"[{"name_hex":"62","oid":null},{"name_hex":"61","oid":null}]"#,
        r#"[{"name_hex":"FF","oid":null}]"#,
        r#"[{"name_hex":"61","oid":null,"extra":true}]"#,
    ] {
        assert!(decode_consulted_refs(invalid).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn progress_survives_reopen_and_cas_enforces_two_pass_source_seal() {
    let directory = tempfile::tempdir().expect("temporary sessions database");
    let path = directory.path().join("sessions.db");
    let key = GitHistoryProgressKey { source_rowid: 7 };
    let initial = progress(key);
    {
        let conn = TestConnection::open(&path);
        ensure_git_correlation_receipt_schema_in_transaction(&conn)
            .await
            .expect("fresh schema");
        let mut invalid = initial.clone();
        invalid.scan_mode = GitHistoryScanMode::Graph;
        assert!(insert_progress(&conn, &invalid).await.is_err());
        assert!(insert_progress(&conn, &initial).await.unwrap());
    }

    let conn = TestConnection::open(&path);
    ensure_git_correlation_receipt_schema_in_transaction(&conn)
        .await
        .expect("idempotent reopen");
    assert_eq!(
        read_progress(&conn, key).await.unwrap(),
        Some(initial.clone())
    );

    let mut partial = initial;
    partial.generation = 1;
    partial.reflog_byte_offset = 256;
    partial.segment_cursor = 1;
    assert!(compare_and_swap_progress(&conn, 0, &partial).await.unwrap());
    let mut rewound = partial.clone();
    rewound.generation = 2;
    rewound.reflog_byte_offset = 300;
    assert!(!compare_and_swap_progress(&conn, 1, &rewound).await.unwrap());
    let mut counter_back = partial.clone();
    counter_back.generation = 2;
    counter_back.reflog_byte_offset = 200;
    counter_back.segment_cursor = 0;
    assert!(
        !compare_and_swap_progress(&conn, 1, &counter_back)
            .await
            .unwrap()
    );

    let mut captured = partial;
    captured.generation = 2;
    captured.scan_mode = GitHistoryScanMode::ReflogVerify;
    captured.reflog_byte_offset = 128;
    captured.capture_target_offset = Some(128);
    captured.reflog_digest = "sha256:captured".to_string();
    assert!(
        compare_and_swap_progress(&conn, 1, &captured)
            .await
            .unwrap()
    );
    assert!(
        !compare_and_swap_progress(&conn, 1, &captured)
            .await
            .unwrap()
    );
    let mut drifted = captured.clone();
    drifted.generation = 3;
    drifted.segment_cursor = 2;
    assert!(!compare_and_swap_progress(&conn, 2, &drifted).await.unwrap());

    let mut verified = captured;
    verified.generation = 3;
    verified.scan_mode = GitHistoryScanMode::Graph;
    verified.verify_byte_offset = 128;
    verified.verify_digest.clone_from(&verified.reflog_digest);
    assert!(
        compare_and_swap_progress(&conn, 2, &verified)
            .await
            .unwrap()
    );

    let mut regressed = verified.clone();
    regressed.generation = 4;
    regressed.scan_mode = GitHistoryScanMode::ReflogVerify;
    assert!(
        !compare_and_swap_progress(&conn, 3, &regressed)
            .await
            .unwrap()
    );
    let mut rewritten = verified.clone();
    rewritten.generation = 4;
    rewritten.cursor_oid = "bbbbbbbb".to_string();
    assert!(
        !compare_and_swap_progress(&conn, 3, &rewritten)
            .await
            .unwrap()
    );

    let mut resealed = verified.clone();
    resealed.generation = 4;
    resealed.source_head_oid = "bbbbbbbb".to_string();
    assert!(
        !compare_and_swap_progress(&conn, 3, &resealed)
            .await
            .unwrap()
    );
    let mut replaced_repository = verified.clone();
    replaced_repository.generation = 4;
    replaced_repository.common_dir_identity = b"replacement-common-dir".to_vec();
    assert!(
        !compare_and_swap_progress(&conn, 3, &replaced_repository)
            .await
            .unwrap()
    );
    let mut advanced = verified;
    advanced.generation = 4;
    advanced.segment_cursor = 2;
    advanced.emitted_count = 1;
    assert!(
        compare_and_swap_progress(&conn, 3, &advanced)
            .await
            .unwrap()
    );
    let mut unverified_publish = advanced.clone();
    unverified_publish.generation = 5;
    unverified_publish.scan_mode = GitHistoryScanMode::Publish;
    assert!(
        !compare_and_swap_progress(&conn, 4, &unverified_publish)
            .await
            .unwrap()
    );
    let mut publish_verify = advanced.clone();
    publish_verify.generation = 5;
    publish_verify.scan_mode = GitHistoryScanMode::PublishVerify;
    assert!(
        compare_and_swap_progress(&conn, 4, &publish_verify)
            .await
            .unwrap()
    );
    let mut publish = publish_verify;
    publish.generation = 6;
    publish.scan_mode = GitHistoryScanMode::Publish;
    assert!(compare_and_swap_progress(&conn, 5, &publish).await.unwrap());
    let mut backslid = advanced.clone();
    backslid.generation = 7;
    backslid.segment_cursor = 1;
    backslid.emitted_count = 0;
    assert!(
        !compare_and_swap_progress(&conn, 6, &backslid)
            .await
            .unwrap()
    );
    assert_eq!(read_progress(&conn, key).await.unwrap(), Some(publish));
}

#[tokio::test]
async fn stable_source_key_preserves_the_older_active_candidate() {
    let directory = tempfile::tempdir().expect("temporary sessions database");
    let conn = TestConnection::open(&directory.path().join("sessions.db"));
    ensure_git_correlation_receipt_schema_in_transaction(&conn)
        .await
        .expect("fresh schema");
    let key = GitHistoryProgressKey { source_rowid: 7 };
    let older = progress(key);
    assert!(insert_progress(&conn, &older).await.unwrap());

    let mut newer = older.clone();
    newer.activity_timestamp = 301;
    newer.window_end = 300;
    newer.segment_end = 300;
    assert!(!insert_progress(&conn, &newer).await.unwrap());
    assert_eq!(
        read_oldest_progress(&conn).await.unwrap(),
        Some(older.clone())
    );
    assert_eq!(read_progress(&conn, key).await.unwrap(), Some(older));
}

#[tokio::test]
async fn exact_reset_cascades_children_and_transaction_rollback_leaves_no_state() {
    let directory = tempfile::tempdir().expect("temporary sessions database");
    let conn = TestConnection::open(&directory.path().join("sessions.db"));
    ensure_git_correlation_receipt_schema_in_transaction(&conn)
        .await
        .expect("fresh schema");
    let key = GitHistoryProgressKey { source_rowid: 7 };
    insert_progress(&conn, &progress(key)).await.unwrap();
    upsert_segment(&conn, &segment(key)).await.unwrap();
    for oid in ["cccccccc", "bbbbbbbb"] {
        upsert_pending(
            &conn,
            &GitHistoryPendingRow {
                key,
                segment_ordinal: 0,
                oid: oid.to_string(),
            },
        )
        .await
        .unwrap();
    }
    let seen = GitHistorySeenRow {
        key,
        segment_ordinal: 0,
        oid: "dddddddd".to_string(),
    };
    assert!(insert_seen(&conn, &seen).await.unwrap());
    assert!(
        !upsert_pending(
            &conn,
            &GitHistoryPendingRow {
                key,
                segment_ordinal: 0,
                oid: seen.oid.clone(),
            },
        )
        .await
        .unwrap()
    );
    assert_eq!(
        read_pending_page(&conn, key, 0, 1).await.unwrap()[0].oid,
        "bbbbbbbb"
    );
    assert!(read_pending_page(&conn, key, 0, 0).await.is_err());
    let span = GitHistoryStagedSpanRow {
        key,
        segment_ordinal: 0,
        boundary: 0,
        branch: Some("main".to_string()),
        timestamp: 100,
    };
    let commit = GitHistoryStagedCommitRow {
        key,
        segment_ordinal: 0,
        oid: "eeeeeeee".to_string(),
        branch: Some("main".to_string()),
        committed_at: 150,
    };
    assert!(upsert_staged_span(&conn, &span).await.unwrap());
    assert!(upsert_staged_commit(&conn, &commit).await.unwrap());
    let mut conflicting_span = span.clone();
    conflicting_span.timestamp = 101;
    assert!(!upsert_staged_span(&conn, &conflicting_span).await.unwrap());
    let mut conflicting_commit = commit.clone();
    conflicting_commit.committed_at = 151;
    assert!(
        !upsert_staged_commit(&conn, &conflicting_commit)
            .await
            .unwrap()
    );
    assert_eq!(
        read_staged_span_page(&conn, key, 128).await.unwrap(),
        vec![span]
    );
    assert_eq!(
        read_staged_commit_page(&conn, key, 128).await.unwrap(),
        vec![commit]
    );

    assert!(reset_progress(&conn, key).await.unwrap());
    assert!(read_segment(&conn, key, 0).await.unwrap().is_none());
    assert!(
        read_pending_page(&conn, key, 0, 128)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(insert_seen(&conn, &seen).await.is_err());
    assert!(
        read_staged_span_page(&conn, key, 128)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        read_staged_commit_page(&conn, key, 128)
            .await
            .unwrap()
            .is_empty()
    );

    let rolled_back_key = GitHistoryProgressKey { source_rowid: 8 };
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    insert_progress(&transaction, &progress(rolled_back_key))
        .await
        .unwrap();
    upsert_segment(&transaction, &segment(rolled_back_key))
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    assert!(
        read_progress(&conn, rolled_back_key)
            .await
            .unwrap()
            .is_none()
    );
}
