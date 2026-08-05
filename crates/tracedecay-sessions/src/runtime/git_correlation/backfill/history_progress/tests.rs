use std::collections::BTreeMap;

use tracedecay_runtime_core::db::engine::{TestConnection, TransactionBehavior};

use super::*;
use crate::runtime::git_correlation::ensure_git_correlation_schema;

fn progress(key: GitHistoryProgressKey) -> GitHistoryProgressRow {
    GitHistoryProgressRow {
        key,
        provider: "codex".to_string(),
        session_id: "session-1".to_string(),
        project_path: "/repo/linked".to_string(),
        window_start: 100,
        window_end: 200,
        worktree: b"/repo/linked".to_vec(),
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
            ("refs/heads/main".to_string(), Some("aaaaaaaa".to_string())),
            ("refs/tags/missing".to_string(), None),
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

#[tokio::test]
async fn progress_survives_reopen_and_cas_enforces_two_pass_source_seal() {
    let directory = tempfile::tempdir().expect("temporary sessions database");
    let path = directory.path().join("sessions.db");
    let key = GitHistoryProgressKey {
        activity_timestamp: 201,
        source_rowid: 7,
    };
    let initial = progress(key);
    {
        let conn = TestConnection::open(&path);
        ensure_git_correlation_schema(&conn)
            .await
            .expect("fresh schema");
        let mut invalid = initial.clone();
        invalid.scan_mode = GitHistoryScanMode::Graph;
        assert!(insert_progress(&conn, &invalid).await.is_err());
        assert!(insert_progress(&conn, &initial).await.unwrap());
    }

    let conn = TestConnection::open(&path);
    ensure_git_correlation_schema(&conn)
        .await
        .expect("idempotent reopen");
    assert_eq!(
        read_progress(&conn, key).await.unwrap(),
        Some(initial.clone())
    );

    let mut captured = initial;
    captured.generation = 1;
    captured.scan_mode = GitHistoryScanMode::ReflogVerify;
    captured.reflog_byte_offset = 128;
    captured.capture_target_offset = Some(128);
    captured.reflog_digest = "sha256:captured".to_string();
    assert!(
        compare_and_swap_progress(&conn, 0, &captured)
            .await
            .unwrap()
    );
    assert!(
        !compare_and_swap_progress(&conn, 0, &captured)
            .await
            .unwrap()
    );
    let mut drifted = captured.clone();
    drifted.generation = 2;
    drifted.segment_cursor = 1;
    assert!(!compare_and_swap_progress(&conn, 1, &drifted).await.unwrap());

    let mut verified = captured;
    verified.generation = 2;
    verified.scan_mode = GitHistoryScanMode::Graph;
    verified.verify_byte_offset = 128;
    verified.verify_digest.clone_from(&verified.reflog_digest);
    assert!(
        compare_and_swap_progress(&conn, 1, &verified)
            .await
            .unwrap()
    );

    let mut regressed = verified.clone();
    regressed.generation = 3;
    regressed.scan_mode = GitHistoryScanMode::ReflogVerify;
    assert!(
        !compare_and_swap_progress(&conn, 2, &regressed)
            .await
            .unwrap()
    );
    let mut rewritten = verified.clone();
    rewritten.generation = 3;
    rewritten.cursor_oid = "bbbbbbbb".to_string();
    assert!(
        !compare_and_swap_progress(&conn, 2, &rewritten)
            .await
            .unwrap()
    );

    let mut resealed = verified.clone();
    resealed.generation = 3;
    resealed.source_head_oid = "bbbbbbbb".to_string();
    assert!(
        !compare_and_swap_progress(&conn, 2, &resealed)
            .await
            .unwrap()
    );
    assert_eq!(read_progress(&conn, key).await.unwrap(), Some(verified));
}

#[tokio::test]
async fn exact_reset_cascades_children_and_transaction_rollback_leaves_no_state() {
    let directory = tempfile::tempdir().expect("temporary sessions database");
    let conn = TestConnection::open(&directory.path().join("sessions.db"));
    ensure_git_correlation_schema(&conn)
        .await
        .expect("fresh schema");
    let key = GitHistoryProgressKey {
        activity_timestamp: 201,
        source_rowid: 7,
    };
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
    assert!(seen_exists(&conn, key, 0, &seen.oid).await.unwrap());

    assert!(reset_progress(&conn, key).await.unwrap());
    assert!(read_segment(&conn, key, 0).await.unwrap().is_none());
    assert!(
        read_pending(&conn, key, 0, "bbbbbbbb")
            .await
            .unwrap()
            .is_none()
    );
    assert!(!seen_exists(&conn, key, 0, &seen.oid).await.unwrap());

    let rolled_back_key = GitHistoryProgressKey {
        activity_timestamp: 202,
        source_rowid: 8,
    };
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
