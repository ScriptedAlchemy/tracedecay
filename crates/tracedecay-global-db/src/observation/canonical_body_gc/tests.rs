use tracedecay_runtime_core::db::engine::{
    Connection, Executor, QueryExecutor, TestConnection, TransactionBehavior, params,
};
use tracedecay_store::{
    BODY_REF_KEY, CANONICAL_BODIES_TABLE_SQL, StoredCanonicalBody, UPSERT_CANONICAL_BODY_SQL,
};

use super::{CanonicalBodyGcReport, has_candidates, read_progress, schema, step};
use crate::tests::harness::{RegisteredGlobalDbHarness, writer_telemetry};

struct Store {
    runtime: TestConnection,
    _directory: tempfile::TempDir,
}

impl Store {
    async fn open() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let runtime = TestConnection::open(&directory.path().join("canonical-bodies.db"));
        runtime
            .execute_batch(CANONICAL_BODIES_TABLE_SQL)
            .await
            .unwrap();
        runtime
            .execute_batch(
                "CREATE TABLE lcm_raw_messages (
                    store_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    storage_kind TEXT NOT NULL, content TEXT, content_hash TEXT NOT NULL
                 );
                 CREATE TABLE observations (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                    observation_json TEXT NOT NULL
                 );",
            )
            .await
            .unwrap();
        Self {
            runtime,
            _directory: directory,
        }
    }

    fn conn(&self) -> &Connection {
        &self.runtime
    }

    async fn install(&self) {
        let transaction = self
            .conn()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        schema::install(&transaction).await.unwrap();
        transaction.commit().await.unwrap();
    }

    async fn tick(&self, limit: usize) -> CanonicalBodyGcReport {
        let transaction = self
            .conn()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let report = step(&transaction, limit).await.unwrap();
        transaction.commit().await.unwrap();
        report
    }
}

async fn insert_body(conn: &(impl Executor + ?Sized), body: &StoredCanonicalBody) {
    conn.execute(
        UPSERT_CANONICAL_BODY_SQL,
        params![
            body.content_hash.as_str(),
            body.encoding,
            body.blob.as_slice(),
            body.uncompressed_bytes
        ],
    )
    .await
    .unwrap();
}

async fn body_exists(conn: &(impl QueryExecutor + ?Sized), hash: &str) -> bool {
    conn.query(
        "SELECT 1 FROM session_canonical_bodies WHERE content_hash = ?1",
        params![hash],
    )
    .await
    .unwrap()
    .next()
    .await
    .unwrap()
    .is_some()
}

async fn candidate_count(conn: &(impl QueryExecutor + ?Sized)) -> i64 {
    let mut rows = conn
        .query("SELECT COUNT(*) FROM canonical_body_gc_candidates", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn observation(hash: &str) -> String {
    serde_json::json!({
        "payload": {"text": null},
        (BODY_REF_KEY): {"/payload/text": hash}
    })
    .to_string()
}

async fn drain(store: &Store, limit: usize) -> (u64, u64) {
    let mut deleted = 0;
    let mut bytes = 0;
    for _ in 0..32 {
        let report = store.tick(limit).await;
        assert!(report.roots_scanned <= limit as u64);
        assert!(report.bodies_scanned <= limit as u64);
        if !report.bootstrap_complete {
            assert_eq!(report.bodies_deleted, 0, "bootstrap never collects");
        }
        deleted += report.bodies_deleted;
        bytes += report.stored_bytes_deleted;
        if !report.has_more {
            return (deleted, bytes);
        }
    }
    panic!("bounded fixture must converge");
}

#[tokio::test]
async fn bootstrap_and_owner_release_preserve_shared_bodies_until_the_final_reference() {
    let store = Store::open().await;
    let shared =
        StoredCanonicalBody::pack("shared complete text ".repeat(1024).as_bytes()).unwrap();
    let orphan =
        StoredCanonicalBody::pack("unreferenced old text ".repeat(1024).as_bytes()).unwrap();
    insert_body(store.conn(), &shared).await;
    insert_body(store.conn(), &orphan).await;
    store.conn().execute(
        "INSERT INTO lcm_raw_messages(storage_kind, content, content_hash) VALUES ('inline', NULL, ?1)",
        params![shared.content_hash.as_str()],
    ).await.unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO observations(observation_json) VALUES (?1)",
            params![observation(&shared.content_hash)],
        )
        .await
        .unwrap();
    store.install().await;

    let first = store.tick(1).await;
    assert_eq!(first.roots_scanned, 1);
    assert!(
        !first.bootstrap_complete,
        "observation and body seed are still pending"
    );
    assert_eq!(first.bodies_deleted, 0);
    assert!(body_exists(store.conn(), &orphan.content_hash).await);
    let (deleted, bytes) = drain(&store, 1).await;
    assert_eq!(deleted, 1);
    assert_eq!(bytes, orphan.blob.len() as u64);
    assert!(body_exists(store.conn(), &shared.content_hash).await);
    assert!(
        store
            .conn()
            .execute(
                "DELETE FROM session_canonical_bodies WHERE content_hash = ?1",
                params![shared.content_hash.as_str()]
            )
            .await
            .is_err(),
        "the reference index also enforces its body foreign key"
    );

    store
        .conn()
        .execute("DELETE FROM lcm_raw_messages", ())
        .await
        .unwrap();
    assert_eq!(
        store.tick(1).await.bodies_deleted,
        0,
        "observation still owns the body"
    );
    store.conn().execute("UPDATE observations SET observation_json = '{\"__retention_released\":\"observation\"}'", ()).await.unwrap();
    let released = store.tick(1).await;
    assert_eq!(released.bodies_deleted, 1);
    assert_eq!(released.stored_bytes_deleted, shared.blob.len() as u64);
    assert!(released.stored_bytes_deleted < shared.uncompressed_bytes as u64);
    assert!(!released.has_more);
}

#[tokio::test]
async fn bootstrap_survives_interruption_and_tracks_owners_changed_between_pages() {
    let store = Store::open().await;
    let old = StoredCanonicalBody::pack("old complete body ".repeat(1024).as_bytes()).unwrap();
    let new = StoredCanonicalBody::pack("new complete body ".repeat(1024).as_bytes()).unwrap();
    insert_body(store.conn(), &old).await;
    store
        .conn()
        .execute(
            "INSERT INTO lcm_raw_messages(storage_kind, content_hash) VALUES ('inline', ?1)",
            params![old.content_hash.as_str()],
        )
        .await
        .unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO observations(observation_json) VALUES (?1)",
            params![observation(&old.content_hash)],
        )
        .await
        .unwrap();
    store.install().await;

    let transaction = store
        .conn()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    assert_eq!(step(&transaction, 1).await.unwrap().roots_scanned, 1);
    transaction.rollback().await.unwrap();
    assert_eq!(
        read_progress(store.conn()).await.unwrap().raw_through,
        0,
        "interruption rolls back the ownership page and frontier together"
    );
    assert!(body_exists(store.conn(), &old.content_hash).await);
    assert_eq!(store.tick(1).await.roots_scanned, 1);

    let transaction = store
        .conn()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    insert_body(&transaction, &new).await;
    transaction
        .execute(
            "INSERT INTO lcm_raw_messages(storage_kind, content_hash) VALUES ('inline', ?1)",
            params![new.content_hash.as_str()],
        )
        .await
        .unwrap();
    transaction
        .execute("DELETE FROM lcm_raw_messages WHERE store_id = 1", ())
        .await
        .unwrap();
    transaction
        .execute(
            "UPDATE observations SET observation_json = ?1",
            params![observation(&new.content_hash)],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let (deleted, _) = drain(&store, 1).await;
    assert_eq!(deleted, 1);
    assert!(!body_exists(store.conn(), &old.content_hash).await);
    assert!(
        body_exists(store.conn(), &new.content_hash).await,
        "owners above the initial frontier are tracked by writer triggers"
    );
    store
        .conn()
        .execute("DELETE FROM observations", ())
        .await
        .unwrap();
    assert_eq!(
        store.tick(1).await.bodies_deleted,
        0,
        "the raw owner added during bootstrap survives"
    );
}

#[tokio::test]
async fn damaged_guards_and_unresolvable_old_references_block_collection() {
    let store = Store::open().await;
    let orphan = StoredCanonicalBody::pack(b"unreferenced body").unwrap();
    insert_body(store.conn(), &orphan).await;
    store
        .conn()
        .execute(
            "INSERT INTO observations(observation_json) VALUES (?1)",
            params![observation("missing-body")],
        )
        .await
        .unwrap();
    store.install().await;
    let transaction = store
        .conn()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    assert!(
        step(&transaction, 1).await.is_err(),
        "missing referenced body must not earn bootstrap completeness"
    );
    transaction.rollback().await.unwrap();
    assert_eq!(
        read_progress(store.conn())
            .await
            .unwrap()
            .observations_through,
        0
    );
    assert!(body_exists(store.conn(), &orphan.content_hash).await);
    store
        .conn()
        .execute_batch("DROP TRIGGER canonical_body_gc_observation_insert")
        .await
        .unwrap();
    let transaction = store
        .conn()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let error = step(&transaction, 1).await.unwrap_err();
    assert!(error.to_string().contains("schema is incomplete"));
    transaction.rollback().await.unwrap();
    assert!(body_exists(store.conn(), &orphan.content_hash).await);
}

#[tokio::test]
async fn registered_collection_commits_and_lost_authority_rolls_back_deleted_bodies() {
    let mut harness =
        crate::tests::harness::RegisteredGlobalDbHarness::open("canonical-body-gc").await;
    let database = harness.registered.clone();
    let first = StoredCanonicalBody::pack(b"first unowned canonical body").unwrap();
    let retained = StoredCanonicalBody::pack(b"body whose collection is revoked").unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    insert_body(&transaction, &first).await;
    transaction.commit().await.unwrap();
    let report = database.run_canonical_body_gc(1).await.unwrap();
    assert!(report.bootstrap_complete);
    assert_eq!(report.bodies_deleted, 1);
    assert_eq!(report.stored_bytes_deleted, first.blob.len() as u64);

    let transaction = database.begin_write_transaction().await.unwrap();
    insert_body(&transaction, &retained).await;
    transaction.commit().await.unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    assert_eq!(step(&transaction, 1).await.unwrap().bodies_deleted, 1);
    harness.revoke();
    assert!(
        transaction.commit().await.is_err(),
        "revoked writer cannot publish collection"
    );
    assert!(body_exists(&database.read_connection(), &retained.content_hash).await);
    assert!(database.run_canonical_body_gc(1).await.is_err());
}

#[tokio::test]
async fn registered_legacy_bootstrap_and_live_references_survive_restarts() {
    let mut harness =
        crate::tests::harness::RegisteredGlobalDbHarness::open("canonical-body-gc-restart").await;
    let live = StoredCanonicalBody::pack(b"retained by the registered raw owner").unwrap();
    let orphan = StoredCanonicalBody::pack(b"unowned before additive installation").unwrap();
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    insert_body(&transaction, &live).await;
    insert_body(&transaction, &orphan).await;
    transaction
        .execute_batch(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES ('codex', 'gc-session', '/gc-project', '/gc-project');",
        )
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO lcm_raw_messages(provider, message_id, session_id, role, ordinal,
            content, content_hash, storage_kind)
         VALUES ('codex', 'gc-message', 'gc-session', 'assistant', 1, NULL, ?1, 'inline')",
            params![live.content_hash.as_str()],
        )
        .await
        .unwrap();
    // Model an older admitted store: remove only derived GC objects while
    // retaining canonical bodies and both source-root tables.
    for &(name, sql) in schema::OBJECTS.iter().rev() {
        let kind = if sql.starts_with("CREATE TRIGGER") {
            "TRIGGER"
        } else if sql.starts_with("CREATE INDEX") {
            "INDEX"
        } else {
            "TABLE"
        };
        transaction
            .execute_batch(&format!("DROP {kind} {name}"))
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();
    harness = harness.restart().await;
    let report = harness.registered.run_canonical_body_gc(1).await.unwrap();
    assert_eq!(report.roots_scanned, 1);
    assert!(!report.bootstrap_complete);
    assert_eq!(report.bodies_deleted, 0);
    let before = read_progress(&harness.registered.read_connection())
        .await
        .unwrap();
    harness = harness.restart().await;
    let after = read_progress(&harness.registered.read_connection())
        .await
        .unwrap();
    assert_eq!(after.raw_through, before.raw_through);
    assert_eq!(after.bodies_after, before.bodies_after);
    schema::validate(&harness.registered.read_connection())
        .await
        .unwrap();
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    assert!(
        transaction
            .execute(
                "DELETE FROM session_canonical_bodies WHERE content_hash = ?1",
                params![live.content_hash.as_str()],
            )
            .await
            .is_err(),
        "the registered production writer enforces the owner foreign key"
    );
    transaction.rollback().await.unwrap();
    let seed = harness.registered.run_canonical_body_gc(1).await.unwrap();
    assert_eq!(seed.bodies_scanned, 1);
    harness = harness.restart().await;
    assert!(
        has_candidates(&harness.registered.read_connection())
            .await
            .unwrap(),
        "candidate seed persists across reopen"
    );
    let mut deleted = 0;
    for _ in 0..8 {
        let report = harness.registered.run_canonical_body_gc(1).await.unwrap();
        deleted += report.bodies_deleted;
        if !report.has_more {
            break;
        }
    }
    assert_eq!(deleted, 1);
    assert!(body_exists(&harness.registered.read_connection(), &live.content_hash).await);
    assert!(!body_exists(&harness.registered.read_connection(), &orphan.content_hash).await);
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "DELETE FROM lcm_raw_messages WHERE provider = 'codex' AND message_id = 'gc-message'",
            (),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(
        harness
            .registered
            .run_canonical_body_gc(1)
            .await
            .unwrap()
            .bodies_deleted,
        1
    );
}

#[tokio::test]
async fn nonpositive_legacy_owner_ids_cannot_be_skipped_by_bootstrap() {
    for raw_owner in [true, false] {
        let store = Store::open().await;
        let body = StoredCanonicalBody::pack(b"retained legacy body").unwrap();
        insert_body(store.conn(), &body).await;
        if raw_owner {
            store
                .conn()
                .execute(
                    "INSERT INTO lcm_raw_messages(store_id, storage_kind, content, content_hash)
                 VALUES (0, 'inline', NULL, ?1)",
                    params![body.content_hash.as_str()],
                )
                .await
                .unwrap();
        } else {
            store
                .conn()
                .execute(
                    "INSERT INTO observations(sequence, observation_json) VALUES (-1, ?1)",
                    params![observation(&body.content_hash)],
                )
                .await
                .unwrap();
        }
        let transaction = store
            .conn()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let error = schema::install(&transaction).await.unwrap_err();
        assert!(error.to_string().contains("positive source identities"));
        transaction.rollback().await.unwrap();
        assert!(body_exists(store.conn(), &body.content_hash).await);
        assert!(
            store
                .conn()
                .query(
                    "SELECT 1 FROM sqlite_master WHERE name GLOB 'canonical_body_gc_*' LIMIT 1",
                    ()
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn ignored_body_delete_rolls_back_candidates_then_reclaims_distinct_pages() {
    let store = Store::open().await;
    store.install().await;
    let mut hashes = Vec::new();
    for ordinal in 0..52 {
        let body =
            StoredCanonicalBody::pack(format!("unreferenced candidate {ordinal}").as_bytes())
                .unwrap();
        insert_body(store.conn(), &body).await;
        hashes.push(body.content_hash);
    }
    assert_eq!(candidate_count(store.conn()).await, 52);
    store
        .conn()
        .execute_batch(
            "CREATE TRIGGER refuse_body_gc_delete
             BEFORE DELETE ON session_canonical_bodies BEGIN
                 SELECT RAISE(IGNORE);
             END",
        )
        .await
        .unwrap();

    let transaction = store
        .conn()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let error = step(&transaction, 48).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("collection left an unreferenced body behind")
    );
    transaction.rollback().await.unwrap();
    assert_eq!(
        candidate_count(store.conn()).await,
        52,
        "a vetoed DELETE cannot discard the only future collection opportunity"
    );
    for hash in &hashes {
        assert!(body_exists(store.conn(), hash).await);
    }

    store
        .conn()
        .execute_batch("DROP TRIGGER refuse_body_gc_delete")
        .await
        .unwrap();
    let first = store.tick(48).await;
    assert_eq!(first.bodies_scanned, 48);
    assert_eq!(first.bodies_deleted, 48);
    assert_eq!(first.candidates_discharged, 48);
    assert!(first.has_more);
    assert_eq!(candidate_count(store.conn()).await, 4);
    let second = store.tick(48).await;
    assert_eq!(second.bodies_scanned, 4);
    assert_eq!(second.bodies_deleted, 4);
    assert_eq!(second.candidates_discharged, 4);
    assert!(!second.has_more);
    assert_eq!(candidate_count(store.conn()).await, 0);
    for hash in &hashes {
        assert!(!body_exists(store.conn(), hash).await);
    }
    assert_eq!(
        store.tick(48).await,
        CanonicalBodyGcReport {
            bootstrap_complete: true,
            ..CanonicalBodyGcReport::default()
        }
    );
}

#[tokio::test]
async fn zero_delete_pages_discharge_distinct_candidates_and_quiesce() {
    let store = Store::open().await;
    for ordinal in 0..52 {
        let body =
            StoredCanonicalBody::pack(format!("still referenced {ordinal}").as_bytes()).unwrap();
        insert_body(store.conn(), &body).await;
        store
            .conn()
            .execute(
                "INSERT INTO lcm_raw_messages(storage_kind, content, content_hash)
                 VALUES ('inline', NULL, ?1)",
                params![body.content_hash.as_str()],
            )
            .await
            .unwrap();
    }
    store.install().await;

    let mut discharged_pages = Vec::new();
    for _ in 0..16 {
        let report = store.tick(48).await;
        assert_eq!(report.bodies_deleted, 0, "referenced bodies stay live");
        if report.candidates_discharged > 0 {
            assert_eq!(report.candidates_discharged, report.bodies_scanned);
            discharged_pages.push(report.candidates_discharged);
        }
        if !report.has_more {
            break;
        }
    }
    assert_eq!(discharged_pages, vec![48, 4]);
    assert!(!has_candidates(store.conn()).await.unwrap());
    assert_eq!(
        store.tick(48).await,
        CanonicalBodyGcReport {
            bootstrap_complete: true,
            ..CanonicalBodyGcReport::default()
        }
    );
    let mut rows = store
        .conn()
        .query("SELECT COUNT(*) FROM session_canonical_bodies", ())
        .await
        .unwrap();
    let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(count, 52);
}

#[tokio::test]
async fn bootstrap_reports_each_committed_sql_frontier_without_claiming_collection() {
    let store = Store::open().await;
    let body = StoredCanonicalBody::pack(b"referenced bootstrap progress").unwrap();
    insert_body(store.conn(), &body).await;
    store
        .conn()
        .execute(
            "INSERT INTO lcm_raw_messages(storage_kind, content, content_hash)
             VALUES ('inline', NULL, ?1)",
            params![body.content_hash.as_str()],
        )
        .await
        .unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO observations(observation_json) VALUES (?1)",
            params![observation(&body.content_hash)],
        )
        .await
        .unwrap();
    store.install().await;

    let frontiers = [
        ("raw:0".to_owned(), "raw:1".to_owned()),
        ("observations:0".to_owned(), "observations:1".to_owned()),
        (
            "bodies:".to_owned(),
            format!("bodies:{}", body.content_hash),
        ),
    ];
    for (phase, (before, after)) in frontiers.into_iter().enumerate() {
        let report = store.tick(1).await;
        assert_eq!(
            report.bootstrap_frontier_before.as_deref(),
            Some(before.as_str())
        );
        assert_eq!(
            report.bootstrap_frontier_after.as_deref(),
            Some(after.as_str())
        );
        let persisted = read_progress(store.conn()).await.unwrap();
        let stored_after = match phase {
            0 => format!("raw:{}", persisted.raw_through),
            1 => format!("observations:{}", persisted.observations_through),
            _ => format!("bodies:{}", persisted.bodies_after),
        };
        assert_eq!(stored_after, after, "reported cursor must be durable");
        assert_eq!(report.roots_scanned + report.bodies_scanned, 1);
        assert_eq!(report.bodies_deleted, 0);
        assert_eq!(report.candidates_discharged, 0);
        assert!(report.has_more);
    }
    let discharged = store.tick(1).await;
    assert_eq!(discharged.candidates_discharged, 1);
    assert_eq!(discharged.bodies_deleted, 0);
    assert!(discharged.bootstrap_frontier_before.is_none());
    assert!(discharged.bootstrap_frontier_after.is_none());
    assert!(!discharged.has_more);
    assert!(body_exists(store.conn(), &body.content_hash).await);
    assert_eq!(
        store.tick(1).await,
        CanonicalBodyGcReport {
            bootstrap_complete: true,
            ..CanonicalBodyGcReport::default()
        }
    );
}

#[tokio::test]
async fn empty_bootstrap_page_reports_its_committed_frontier_once() {
    let store = Store::open().await;
    store.conn().execute(
        "INSERT INTO lcm_raw_messages(storage_kind, content, content_hash) VALUES ('inline', 'text', 'fixture')", ()
    ).await.unwrap();
    store.install().await;
    store
        .conn()
        .execute("DELETE FROM lcm_raw_messages", ())
        .await
        .unwrap();

    let page = store.tick(48).await;
    assert_eq!((page.roots_scanned, page.bodies_scanned), (0, 0));
    assert_eq!(page.bootstrap_frontier_before.as_deref(), Some("raw:0"));
    assert_eq!(page.bootstrap_frontier_after.as_deref(), Some("raw:1"));
    assert_eq!(read_progress(store.conn()).await.unwrap().raw_through, 1);
    assert!(page.has_committed_work());
    assert!(!store.tick(48).await.has_committed_work());
}

#[tokio::test]
async fn rewritten_bootstrap_checkpoint_refuses_commit_and_recovers_across_48_row_pages() {
    let store = Store::open().await;
    let shared = StoredCanonicalBody::pack(b"shared canonical body").unwrap();
    insert_body(store.conn(), &shared).await;
    for _ in 0..52 {
        store
            .conn()
            .execute(
                "INSERT INTO lcm_raw_messages(storage_kind, content, content_hash)
                 VALUES ('inline', NULL, ?1)",
                params![shared.content_hash.as_str()],
            )
            .await
            .unwrap();
    }
    store.install().await;
    // Even with recursive triggers enabled, the inner write terminates at zero.
    // SQLite reports one updated row for the outer statement, but a plain
    // affected-row check would miss the nonadvancing durable frontier.
    store
        .conn()
        .execute_batch(
            "CREATE TRIGGER corrupt_gc_bootstrap AFTER UPDATE ON canonical_body_gc_progress
             WHEN NEW.raw_through > 0 BEGIN
                 UPDATE canonical_body_gc_progress SET raw_through = 0 WHERE singleton = 1;
             END",
        )
        .await
        .unwrap();
    let transaction = store
        .conn()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let error = step(&transaction, 48).await.unwrap_err();
    assert!(error.to_string().contains("frontier was not persisted"));
    let mut staged = transaction
        .query("SELECT COUNT(*) FROM canonical_body_gc_refs", ())
        .await
        .unwrap();
    let staged_count: i64 = staged.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        staged_count, 48,
        "the page was enrolled before checkpoint refusal"
    );
    drop(staged);
    transaction.rollback().await.unwrap();
    assert_eq!(read_progress(store.conn()).await.unwrap().raw_through, 0);
    let mut durable = store
        .conn()
        .query("SELECT COUNT(*) FROM canonical_body_gc_refs", ())
        .await
        .unwrap();
    let durable_count: i64 = durable.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(durable_count, 0, "the refused page must roll back its refs");
    drop(durable);
    assert!(body_exists(store.conn(), &shared.content_hash).await);

    store
        .conn()
        .execute_batch("DROP TRIGGER corrupt_gc_bootstrap")
        .await
        .unwrap();
    let first = store.tick(48).await;
    assert_eq!(first.roots_scanned, 48);
    assert_eq!(first.bootstrap_frontier_before.as_deref(), Some("raw:0"));
    assert_eq!(first.bootstrap_frontier_after.as_deref(), Some("raw:48"));
    assert!(first.has_more);
    assert_eq!(read_progress(store.conn()).await.unwrap().raw_through, 48);
    let second = store.tick(48).await;
    assert_eq!(second.roots_scanned, 4);
    assert_eq!(second.bootstrap_frontier_before.as_deref(), Some("raw:48"));
    assert_eq!(second.bootstrap_frontier_after.as_deref(), Some("raw:52"));
    assert_eq!(read_progress(store.conn()).await.unwrap().raw_through, 52);
    let (deleted, bytes) = drain(&store, 48).await;
    assert_eq!((deleted, bytes), (0, 0), "live bodies remain referenced");
    assert!(body_exists(store.conn(), &shared.content_hash).await);
    assert!(!has_candidates(store.conn()).await.unwrap());
    let idle = store.tick(48).await;
    assert!(idle.bootstrap_complete);
    assert_eq!(idle.roots_scanned, 0);
    assert_eq!(idle.bodies_scanned, 0);
    assert!(!idle.has_more);
}

#[tokio::test]
async fn reinserted_candidate_page_refuses_false_discharge_and_recovers() {
    let store = Store::open().await;
    for ordinal in 0..52 {
        let body = StoredCanonicalBody::pack(format!("retained {ordinal}").as_bytes()).unwrap();
        insert_body(store.conn(), &body).await;
        store
            .conn()
            .execute(
                "INSERT INTO lcm_raw_messages(storage_kind, content, content_hash)
                 VALUES ('inline', NULL, ?1)",
                params![body.content_hash.as_str()],
            )
            .await
            .unwrap();
    }
    store.install().await;
    let mut bootstrap_finished = false;
    for _ in 0..16 {
        if store.tick(48).await.bootstrap_complete {
            bootstrap_finished = true;
            break;
        }
    }
    assert!(
        bootstrap_finished,
        "candidate collection must become eligible"
    );
    assert_eq!(candidate_count(store.conn()).await, 52);

    // The explicit DELETE returns 48 affected rows even when an AFTER DELETE
    // trigger restores all of them. Only a same-transaction readback catches
    // the false progress that otherwise repeats this exact 48-row page.
    store
        .conn()
        .execute_batch(
            "CREATE TRIGGER keep_candidates_after_delete
             AFTER DELETE ON canonical_body_gc_candidates BEGIN
                 INSERT OR IGNORE INTO canonical_body_gc_candidates(content_hash)
                 VALUES (OLD.content_hash);
             END",
        )
        .await
        .unwrap();
    let transaction = store
        .conn()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let error = step(&transaction, 48).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("candidate discharge was not persisted"),
        "an apparent 48-row DELETE must not count as committed progress: {error}"
    );
    assert_eq!(
        candidate_count(&transaction).await,
        52,
        "the trigger left every candidate in the still-open transaction"
    );
    transaction.rollback().await.unwrap();
    assert_eq!(
        candidate_count(store.conn()).await,
        52,
        "the refused page must leave all candidate identities durable"
    );
    store
        .conn()
        .execute_batch("DROP TRIGGER keep_candidates_after_delete")
        .await
        .unwrap();

    let first = store.tick(48).await;
    assert_eq!(first.bodies_scanned, 48);
    assert_eq!(first.candidates_discharged, 48);
    assert_eq!(first.bodies_deleted, 0);
    assert!(first.has_more);
    let second = store.tick(48).await;
    assert_eq!(second.bodies_scanned, 4);
    assert_eq!(second.candidates_discharged, 4);
    assert_eq!(second.bodies_deleted, 0);
    assert!(!second.has_more);
    assert_eq!(candidate_count(store.conn()).await, 0);
    let idle = store.tick(48).await;
    assert_eq!(idle.bodies_scanned, 0);
    assert!(!idle.has_more);
}

#[tokio::test]
async fn idle_registered_gc_avoids_writer_and_rechecks_new_candidates() {
    let harness = RegisteredGlobalDbHarness::open("canonical-body-idle-writer").await;
    let before = writer_telemetry(&harness.registered).transactions;
    for _ in 0..3 {
        assert_eq!(
            harness.registered.run_canonical_body_gc(48).await.unwrap(),
            CanonicalBodyGcReport {
                bootstrap_complete: true,
                ..CanonicalBodyGcReport::default()
            }
        );
        assert_eq!(
            writer_telemetry(&harness.registered).transactions,
            before,
            "a drained retention store must not enter the SQLite writer"
        );
    }

    let unowned = StoredCanonicalBody::pack(b"arrived after an idle retention probe").unwrap();
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    insert_body(&transaction, &unowned).await;
    transaction.commit().await.unwrap();
    let after_seed = writer_telemetry(&harness.registered).transactions;
    let collected = harness.registered.run_canonical_body_gc(48).await.unwrap();
    assert_eq!(collected.bodies_scanned, 1);
    assert_eq!(collected.bodies_deleted, 1);
    assert_eq!(collected.candidates_discharged, 1);
    assert!(!collected.has_more);
    assert!(
        writer_telemetry(&harness.registered).transactions.commands > after_seed.commands,
        "a new candidate must still enter the real writer"
    );
    assert!(!body_exists(&harness.registered.read_connection(), &unowned.content_hash).await);
}
