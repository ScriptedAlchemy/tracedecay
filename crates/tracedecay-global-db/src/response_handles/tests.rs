use std::sync::{Arc, Mutex};

use tracedecay_runtime_core::db::engine::params;

use super::*;
use crate::tests::harness::RegisteredGlobalDbHarness;

const NOW: i64 = 1_000_000;

async fn found(db: &RegisteredGlobalDb, handle: &str, now: i64) -> ResponseHandleRecord {
    match db
        .lookup_response_handle(handle, now)
        .await
        .expect("lookup")
    {
        ResponseHandleLookup::Found(record) => record,
        other => panic!("expected a live record, found {other:?}"),
    }
}

async fn overwrite(db: &RegisteredGlobalDb, sql: &'static str, first: String, handle: String) {
    let writer = db.writer_connection().expect("writer");
    let changed = writer
        .execute(sql, params![first, handle])
        .await
        .expect("overwrite fixture row");
    assert_eq!(changed, 1, "fixture overwrite must target one row");
}

#[tokio::test(flavor = "multi_thread")]
async fn publication_survives_restart_and_pages_utf8_exactly() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-restart").await;
    let content = "é🦀 κόσμος\n".repeat(4_000);
    let published = harness
        .registered
        .publish_response_handle(&content, NOW)
        .await
        .expect("publish");
    assert_eq!(published.handle, response_handle_for(&content));
    assert_eq!(published.expires_at, NOW + RESPONSE_HANDLE_TTL_SECS);

    let harness = harness.restart().await;
    let record = found(&harness.registered, &published.handle, NOW + 1).await;
    assert_eq!(record, published);
    assert_eq!(record.original_chars(), content.chars().count());
    let rebuilt: String = (0..record.original_chars())
        .step_by(997)
        .map(|offset| {
            record
                .content
                .chars()
                .skip(offset)
                .take(997)
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        rebuilt, content,
        "character-offset pages reconstruct exactly"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn identical_replays_reuse_then_renew_past_half_life() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-renew").await;
    let db = &harness.registered;
    let first = db.publish_response_handle("same", NOW).await.unwrap();
    let replay = db.publish_response_handle("same", NOW + 10).await.unwrap();
    assert_eq!(replay, first, "a live replay answers without renewing");

    let renewal_at = NOW + RESPONSE_HANDLE_TTL_SECS / 2 + 1;
    let renewed = db
        .publish_response_handle("same", renewal_at)
        .await
        .unwrap();
    assert_eq!(renewed.handle, first.handle);
    assert_eq!(renewed.created_at, renewal_at);
    let harness = harness.restart().await;
    assert_eq!(
        found(&harness.registered, &first.handle, first.expires_at + 1)
            .await
            .created_at,
        renewal_at,
        "the renewed record outlives the original expiry across a restart"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_lookup_is_typed_and_non_mutating_until_republished() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-expiry").await;
    let db = &harness.registered;
    let record = db.publish_response_handle("stale", NOW).await.unwrap();
    let expired_at = NOW + RESPONSE_HANDLE_TTL_SECS;
    for _ in 0..2 {
        assert_eq!(
            db.lookup_response_handle(&record.handle, expired_at)
                .await
                .unwrap(),
            ResponseHandleLookup::Expired {
                created_at: NOW,
                expires_at: expired_at,
            }
        );
    }
    assert_eq!(
        db.inventory_response_handles().await.unwrap().record_count,
        1
    );
    let renewed = db
        .publish_response_handle("stale", expired_at)
        .await
        .unwrap();
    assert_eq!(renewed.created_at, expired_at);
    assert_eq!(found(db, &record.handle, expired_at).await.content, "stale");
    assert_eq!(
        db.lookup_response_handle("rh_000000000000000000000000", NOW)
            .await
            .unwrap(),
        ResponseHandleLookup::Missing
    );
    assert!(matches!(
        db.lookup_response_handle("rh_ABCDEF000000000000000000", NOW)
            .await,
        Err(ResponseHandleError::InvalidHandle)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn truncated_digest_collisions_refuse_and_corrupt_rows_are_never_served() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-collision").await;
    let db = &harness.registered;
    let record = db.publish_response_handle("victim", NOW).await.unwrap();
    let digest = content_digest("victim");
    // Same truncated handle, different full digest: a genuine collision.
    let colliding = format!("{}{}", &digest[..HANDLE_HEX_CHARS], "0".repeat(40));
    assert_ne!(colliding, digest);
    overwrite(
        db,
        "UPDATE response_handles SET content_sha256 = ?1 WHERE handle = ?2",
        colliding,
        record.handle.clone(),
    )
    .await;
    assert!(matches!(
        db.publish_response_handle("victim", NOW + 1).await,
        Err(ResponseHandleError::Collision { handle }) if handle == record.handle
    ));
    assert!(matches!(
        db.lookup_response_handle(&record.handle, NOW + 1).await,
        Err(ResponseHandleError::CorruptRecord { .. })
    ));

    // Tampered text under an intact digest is corrupt on read, and an
    // identical publication repairs it without waiting for renewal.
    overwrite(
        db,
        "UPDATE response_handles SET content_sha256 = ?1, content = 'victiM' WHERE handle = ?2",
        digest,
        record.handle.clone(),
    )
    .await;
    assert!(matches!(
        db.lookup_response_handle(&record.handle, NOW + 2).await,
        Err(ResponseHandleError::CorruptRecord { .. })
    ));
    db.publish_response_handle("victim", NOW + 2).await.unwrap();
    assert_eq!(found(db, &record.handle, NOW + 2).await.content, "victim");
}

#[tokio::test(flavor = "multi_thread")]
async fn namespaces_isolate_lookup_cleanup_and_inventory() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-namespace").await;
    let db = &harness.registered;
    let own = db.response_handle_namespace().unwrap();
    let foreign = "foreign-project".to_owned();
    let record = db
        .publish_response_handle_in(foreign.clone(), "foreign payload".into(), NOW, None)
        .await
        .unwrap();

    assert_eq!(
        db.lookup_response_handle(&record.handle, NOW)
            .await
            .unwrap(),
        ResponseHandleLookup::Missing,
        "a digest alone grants no access to another namespace"
    );
    let expired_at = NOW + RESPONSE_HANDLE_TTL_SECS;
    assert_eq!(
        db.cleanup_expired_response_handles(expired_at)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db.inventory_response_handles_in(&foreign)
            .await
            .unwrap()
            .record_count,
        1,
        "cleanup never removes a foreign namespace's rows"
    );
    assert_eq!(
        db.inventory_response_handles_in(&own).await.unwrap(),
        ResponseHandleInventory::default()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_removes_only_expired_rows_in_bounded_batches() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-cleanup").await;
    let db = &harness.registered;
    let expired_rows = CLEANUP_BATCH_ROWS as usize + 7;
    for index in 0..expired_rows {
        db.publish_response_handle(&format!("expired {index}"), NOW)
            .await
            .unwrap();
    }
    let live_at = NOW + RESPONSE_HANDLE_TTL_SECS;
    let live = db.publish_response_handle("live", live_at).await.unwrap();

    let inventory = db.inventory_response_handles().await.unwrap();
    assert_eq!(inventory.record_count, expired_rows as u64 + 1);
    assert_eq!(
        inventory.oldest_expires_at,
        Some(NOW + RESPONSE_HANDLE_TTL_SECS)
    );
    assert_eq!(inventory.newest_expires_at, Some(live.expires_at));
    let expected_bytes: usize = (0..expired_rows)
        .map(|index| format!("expired {index}").len())
        .sum::<usize>()
        + "live".len();
    assert_eq!(inventory.total_bytes, expected_bytes as u64);

    assert_eq!(
        db.cleanup_expired_response_handles(live_at).await.unwrap(),
        expired_rows as u64
    );
    assert_eq!(
        db.inventory_response_handles().await.unwrap().record_count,
        1
    );
    assert_eq!(found(db, &live.handle, live_at).await.content, "live");
}

/// Inventory and expiry selection are answered by the covering index alone,
/// so neither reads a payload page.
#[tokio::test(flavor = "multi_thread")]
async fn metadata_queries_read_only_the_covering_index() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-plan").await;
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    for (sql, mut rows) in [
        (
            "inventory",
            snapshot
                .query(
                    "EXPLAIN QUERY PLAN SELECT count(*), coalesce(sum(content_bytes), 0), \
                     min(expires_at), max(expires_at) FROM response_handles \
                     INDEXED BY idx_response_handles_expiry WHERE namespace = ?1",
                    params!["namespace"],
                )
                .await
                .unwrap(),
        ),
        (
            "expiry selection",
            snapshot
                .query(
                    "EXPLAIN QUERY PLAN SELECT rowid FROM response_handles \
                     WHERE namespace = ?1 AND expires_at <= ?2 LIMIT ?3",
                    params!["namespace", NOW, CLEANUP_BATCH_ROWS],
                )
                .await
                .unwrap(),
        ),
    ] {
        let mut plan = String::new();
        while let Some(row) = rows.next().await.unwrap() {
            plan.push_str(&row.get::<String>(3).unwrap());
            plan.push('\n');
        }
        assert!(
            plan.contains("COVERING INDEX idx_response_handles_expiry"),
            "{sql}\n{plan}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_publish_lookup_and_cleanup_settle_consistently() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-concurrency").await;
    let db = Arc::new(harness.mount().await);
    let tasks: Vec<_> = (0..16)
        .map(|writer| {
            let db = Arc::clone(&db);
            tokio::spawn(async move {
                let content = format!("payload {}", writer % 4);
                let published = db.publish_response_handle(&content, NOW).await.unwrap();
                db.cleanup_expired_response_handles(NOW).await.unwrap();
                {
                    let lookup = db.lookup_response_handle(&published.handle, NOW).await;
                    (content, published.handle, lookup)
                }
            })
        })
        .collect();
    for task in tasks {
        let (content, handle, lookup) = task.await.unwrap();
        assert!(matches!(
            lookup.unwrap(),
            ResponseHandleLookup::Found(record) if record.content == content && record.handle == handle
        ));
    }
    assert_eq!(
        db.inventory_response_handles().await.unwrap().record_count,
        4
    );
}

/// A publication refused before commit leaves no record and issues no handle.
#[tokio::test(flavor = "multi_thread")]
async fn revoked_write_authority_refuses_publication_without_a_record() {
    let mut harness = RegisteredGlobalDbHarness::open("response-handle-revoked").await;
    harness
        .registered
        .publish_response_handle("before", NOW)
        .await
        .unwrap();
    harness.revoke();
    assert!(matches!(
        harness
            .registered
            .publish_response_handle("after", NOW)
            .await,
        Err(ResponseHandleError::Store(_))
    ));
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT count(*) FROM response_handles WHERE handle = ?1",
            params![response_handle_for("after")],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

/// The `Commit` authority check is the last gate before `COMMIT`, reached
/// after the job body has already written the handle row. A publication whose
/// authority refuses there must roll the written row back, issue no handle,
/// and leave the writer usable: the row is never durable without its handle.
#[tokio::test(flavor = "multi_thread")]
async fn refused_commit_rolls_back_the_written_handle_row() {
    /// Admits every intent up to the row write and refuses only `Commit`,
    /// recording what it admitted so the test proves the row was written.
    struct RefuseCommit(Mutex<Vec<ExactSqlWriteIntent>>);
    impl ExactSqlWriteAuthority for RefuseCommit {
        fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
            self.0.lock().unwrap().push(intent);
            match intent {
                ExactSqlWriteIntent::Commit => {
                    Err(ExactSqlError::AuthorityDenied("commit refused".to_owned()))
                }
                _ => Ok(()),
            }
        }
    }

    let harness = RegisteredGlobalDbHarness::open("response-handle-commit-gate").await;
    let db = &harness.registered;
    let namespace = db.response_handle_namespace().unwrap();
    let guard = Arc::new(RefuseCommit(Mutex::new(Vec::new())));
    let refused = db
        .publish_response_handle_in(
            namespace,
            "must not land".to_owned(),
            NOW,
            Some(guard.clone()),
        )
        .await;
    assert!(
        matches!(refused, Err(ResponseHandleError::Store(_))),
        "a refused commit must refuse the publication: {refused:?}"
    );
    let admitted = guard.0.lock().unwrap().clone();
    let row_write = admitted
        .iter()
        .position(|intent| *intent == ExactSqlWriteIntent::Execute)
        .expect("the handle row must be written before the commit is refused");
    let refused_commit = admitted
        .iter()
        .position(|intent| *intent == ExactSqlWriteIntent::Commit)
        .expect("the commit gate must be reached");
    assert!(
        row_write < refused_commit,
        "the row write must precede the refused commit: {admitted:?}"
    );
    assert_eq!(
        db.inventory_response_handles().await.unwrap(),
        ResponseHandleInventory::default(),
        "the written handle row must roll back with the refused commit"
    );
    assert_eq!(
        db.lookup_response_handle(&response_handle_for("must not land"), NOW)
            .await
            .unwrap(),
        ResponseHandleLookup::Missing
    );

    db.publish_response_handle("must not land", NOW)
        .await
        .unwrap();
    assert_eq!(
        found(db, &response_handle_for("must not land"), NOW)
            .await
            .content,
        "must not land",
        "the writer must stay usable after the refused commit"
    );
}

#[tokio::test]
async fn replay_repairs_length_metadata_and_future_dated_rows() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-metadata-repair").await;
    let db = &harness.registered;
    let content = "é🦀\0full content";
    let record = db.publish_response_handle(content, NOW).await.unwrap();
    db.writer_connection()
        .unwrap()
        .execute(
            "UPDATE response_handles SET content_bytes = 1, content_chars = 1 WHERE handle = ?1",
            params![record.handle.as_str()],
        )
        .await
        .unwrap();
    assert!(matches!(
        db.lookup_response_handle(&record.handle, NOW).await,
        Err(ResponseHandleError::CorruptRecord { .. }),
    ));
    db.publish_response_handle(content, NOW + 1).await.unwrap();
    assert_eq!(found(db, &record.handle, NOW + 1).await.content, content);

    let future = NOW + RESPONSE_HANDLE_TTL_SECS * 10;
    db.writer_connection()
        .unwrap()
        .execute(
            "UPDATE response_handles SET created_at = ?1, expires_at = ?2 WHERE handle = ?3",
            params![
                future,
                future + RESPONSE_HANDLE_TTL_SECS,
                record.handle.as_str()
            ],
        )
        .await
        .unwrap();
    assert!(matches!(
        db.lookup_response_handle(&record.handle, NOW + 2).await,
        Err(ResponseHandleError::CorruptRecord { .. }),
    ));
    let repaired = db.publish_response_handle(content, NOW + 2).await.unwrap();
    assert_eq!(repaired.created_at, NOW + 2);
    assert_eq!(repaired.expires_at, NOW + 2 + RESPONSE_HANDLE_TTL_SECS);
    assert_eq!(found(db, &record.handle, NOW + 2).await.content, content);
}

#[tokio::test]
async fn request_deadline_refuses_publication_while_writer_is_held() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-writer-deadline").await;
    let db = &harness.registered;
    let writer = db.runtime_database().writer().await.unwrap();
    let deadline = Deadline::new(tracedecay_domain::UtcMicros(now_micros().0 + 50_000)).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        db.publish_response_handle_with_control(
            "must not persist",
            deadline,
            CancellationToken::new(),
            None,
        ),
    )
    .await
    .expect("publication must stop without acquiring the held writer");
    assert!(matches!(result, Err(ResponseHandleError::Store(error))
        if error.project_route_context().is_some_and(|(code, _, _)| code == "response_handle_publication_timed_out")));
    drop(writer);
    db.publish_response_handle("writer is usable", NOW)
        .await
        .unwrap();
    assert_eq!(
        db.lookup_response_handle(&response_handle_for("must not persist"), NOW)
            .await
            .unwrap(),
        ResponseHandleLookup::Missing,
    );
}

#[tokio::test]
async fn request_cancellation_refuses_queued_publication_and_uses_owner_clock() {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    let harness = RegisteredGlobalDbHarness::open("response-handle-writer-cancel").await;
    let db = &harness.registered;
    let writer = db.runtime_database().writer().await.unwrap();
    let cancellation = CancellationToken::new();
    let deadline = Deadline::new(tracedecay_domain::UtcMicros(now_micros().0 + 3_000_000)).unwrap();
    let publication = db.publish_response_handle_with_control(
        "cancelled payload",
        deadline.clone(),
        cancellation.clone(),
        None,
    );
    tokio::pin!(publication);
    poll_fn(|cx| {
        assert!(publication.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), publication)
        .await
        .unwrap();
    assert!(matches!(result, Err(ResponseHandleError::Store(error))
        if error.project_route_context().is_some_and(|(code, _, _)| code == "response_handle_publication_cancelled")));
    drop(writer);
    let before = now_micros().0.div_euclid(1_000_000);
    let published = db
        .publish_response_handle_with_control(
            "owner clock",
            deadline,
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    let after = now_micros().0.div_euclid(1_000_000);
    assert!((before..=after).contains(&published.created_at));
    assert_eq!(
        published.expires_at,
        published.created_at + RESPONSE_HANDLE_TTL_SECS
    );
    assert_eq!(
        db.lookup_response_handle(&response_handle_for("cancelled payload"), before)
            .await
            .unwrap(),
        ResponseHandleLookup::Missing,
    );
}

#[tokio::test]
async fn live_request_signal_refuses_publication_without_a_token_relay() {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    let harness = RegisteredGlobalDbHarness::open("response-handle-live-signal").await;
    let db = &harness.registered;
    let writer = db.runtime_database().writer().await.unwrap();
    let cancellation = CancellationToken::new();
    let signal = CancellationSignal::active("response-handle-live-signal").unwrap();
    let deadline = Deadline::new(tracedecay_domain::UtcMicros(now_micros().0 + 3_000_000)).unwrap();
    let publication = db.publish_response_handle_with_control(
        "cancelled directly",
        deadline,
        cancellation.clone(),
        Some(signal.clone()),
    );
    tokio::pin!(publication);
    poll_fn(|cx| {
        assert!(publication.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    signal.cancel(now_micros());
    assert!(
        !cancellation.is_cancelled(),
        "no asynchronous relay is needed"
    );
    let result = tokio::time::timeout(Duration::from_secs(1), publication)
        .await
        .unwrap();
    assert!(matches!(result, Err(ResponseHandleError::Store(error))
        if error.project_route_context().is_some_and(|(code, _, _)| code == "response_handle_publication_cancelled")));
    drop(writer);
    db.publish_response_handle("writer is usable", NOW)
        .await
        .unwrap();
    assert_eq!(
        db.inventory_response_handles().await.unwrap().record_count,
        1
    );
    assert_eq!(
        db.lookup_response_handle(&response_handle_for("cancelled directly"), NOW)
            .await
            .unwrap(),
        ResponseHandleLookup::Missing,
    );
}
