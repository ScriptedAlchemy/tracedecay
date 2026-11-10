//! Measure real bootstrap write submissions without replacing the SQLite executor.

use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_runtime_core::db::engine::{
    Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows, TestConnection,
    TransactionBehavior, WriteStatement, params,
};
use tracedecay_store::{CANONICAL_BODIES_TABLE_SQL, StoredCanonicalBody, UPSERT_CANONICAL_BODY_SQL};

use super::{CanonicalBodyGcReport, read_progress, schema, step};

struct CountWrites<'a, E: ?Sized> {
    inner: &'a E,
    submissions: AtomicUsize,
}

impl<E: Executor + ?Sized> QueryExecutor for CountWrites<'_, E> {
    async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
    where
        P: IntoParams,
    {
        self.inner.query(sql, params).await
    }
}

impl<E: Executor + ?Sized> Executor for CountWrites<'_, E> {
    async fn execute<P>(&self, sql: &str, params: P) -> EngineResult<u64>
    where
        P: IntoParams,
    {
        self.submissions.fetch_add(1, Ordering::Relaxed);
        self.inner.execute(sql, params).await
    }

    async fn execute_statements(&self, statements: Vec<WriteStatement>) -> EngineResult<Vec<u64>> {
        self.submissions.fetch_add(1, Ordering::Relaxed);
        self.inner.execute_statements(statements).await
    }

    async fn execute_batch(&self, sql: &str) -> EngineResult<()> {
        self.submissions.fetch_add(1, Ordering::Relaxed);
        self.inner.execute_batch(sql).await
    }
}

struct Fixture {
    conn: TestConnection,
    bodies: Vec<StoredCanonicalBody>,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn open() -> Self {
        Self::with_observations(false).await
    }

    async fn with_observations(include_observations: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let conn = TestConnection::open(&directory.path().join("bootstrap-writes.db"));
        conn.execute_batch(CANONICAL_BODIES_TABLE_SQL).await.unwrap();
        conn.execute_batch(
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
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let mut bodies = Vec::new();
        for ordinal in 1..=52_i64 {
            let text = format!("retained bootstrap body {ordinal}");
            let body = StoredCanonicalBody::pack(text.as_bytes()).unwrap();
            transaction
                .execute(
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
            // Holes in the key space must not turn the page into an ID count.
            transaction
                .execute(
                    "INSERT INTO lcm_raw_messages(store_id, storage_kind, content_hash)
                     VALUES (?1, 'inline', ?2)",
                    params![ordinal * 2, body.content_hash.as_str()],
                )
                .await
                .unwrap();
            if include_observations {
                // Three distinct hashes from the third owner onward, so a
                // 48-owner page crosses the 128-write submission boundary.
                let current = body.content_hash.as_str();
                let previous = bodies
                    .last()
                    .map_or(current, |body: &StoredCanonicalBody| body.content_hash.as_str());
                let earlier = bodies
                    .iter()
                    .rev()
                    .nth(1)
                    .map_or(current, |body| body.content_hash.as_str());
                let json = serde_json::json!({
                    "payload": {"a": null, "b": null, "c": null},
                    "_tracedecay_canonical_body_refs": {
                        "/payload/a": current,
                        "/payload/b": previous,
                        "/payload/c": earlier
                    }
                })
                .to_string();
                transaction
                    .execute(
                        "INSERT INTO observations(sequence, observation_json) VALUES (?1, ?2)",
                        params![ordinal * 2, json],
                    )
                    .await
                    .unwrap();
            }
            bodies.push(body);
        }
        schema::install(&transaction).await.unwrap();
        transaction.commit().await.unwrap();
        Self {
            conn,
            bodies,
            _directory: directory,
        }
    }

    async fn tick(&self) -> (CanonicalBodyGcReport, usize) {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let (report, submissions) = {
            let counted = CountWrites {
                inner: &transaction,
                submissions: AtomicUsize::new(0),
            };
            let report = step(&counted, 48).await.unwrap();
            (report, counted.submissions.load(Ordering::Relaxed))
        };
        transaction.commit().await.unwrap();
        (report, submissions)
    }

    async fn assert_bodies_preserved(&self) {
        let count = scalar(&self.conn, "SELECT COUNT(*) FROM session_canonical_bodies").await;
        assert_eq!(count, self.bodies.len() as i64);
        for body in &self.bodies {
            let mut rows = self
                .conn
                .query(
                    "SELECT body FROM session_canonical_bodies WHERE content_hash = ?1",
                    params![body.content_hash.as_str()],
                )
                .await
                .unwrap();
            let stored = rows.next().await.unwrap().unwrap();
            assert_eq!(stored.get::<Vec<u8>>(0).unwrap(), body.blob);
        }
    }
}

async fn scalar(conn: &(impl QueryExecutor + ?Sized), sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

async fn raw_owners(conn: &(impl QueryExecutor + ?Sized)) -> Vec<i64> {
    let mut rows = conn
        .query(
            "SELECT owner_id FROM canonical_body_gc_refs WHERE owner_kind = 'raw'
             ORDER BY owner_id",
            (),
        )
        .await
        .unwrap();
    let mut owners = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        owners.push(row.get(0).unwrap());
    }
    owners
}

#[asupersync::test]
async fn bootstrap_pages_batch_writes_and_preserve_sparse_frontiers() {
    let fixture = Fixture::open().await;
    for (scanned, through, owners) in [(48_u64, 96_i64, 48_i64), (4, 104, 52)] {
        let (report, submissions) = fixture.tick().await;
        let progress = read_progress(&fixture.conn).await.unwrap();
        assert_eq!(report.roots_scanned, scanned);
        assert_eq!(submissions, 2, "one seed submission plus the checkpoint");
        assert_eq!(progress.raw_through, through);
        assert_eq!(
            raw_owners(&fixture.conn).await,
            (1..=owners).map(|ordinal| ordinal * 2).collect::<Vec<_>>()
        );
        assert_eq!(report.bodies_deleted, 0);
        assert!(report.has_more);
    }

    let mut hashes = fixture
        .bodies
        .iter()
        .map(|body| body.content_hash.clone())
        .collect::<Vec<_>>();
    hashes.sort();
    for (scanned, total) in [(48_u64, 48_usize), (4, 52)] {
        let (report, submissions) = fixture.tick().await;
        let progress = read_progress(&fixture.conn).await.unwrap();
        let candidates = scalar(
            &fixture.conn,
            "SELECT COUNT(*) FROM canonical_body_gc_candidates",
        )
        .await;
        assert_eq!(report.bodies_scanned, scanned);
        assert_eq!(submissions, 2, "one candidate seed plus the checkpoint");
        assert_eq!(progress.bodies_after, hashes[total - 1]);
        assert_eq!(candidates, total as i64);
        assert_eq!(report.bodies_deleted, 0);
        assert!(report.has_more);
    }

    for scanned in [48_u64, 4] {
        let (report, _) = fixture.tick().await;
        assert_eq!(report.bodies_scanned, scanned);
        assert_eq!(report.candidates_discharged, scanned);
        assert_eq!(report.bodies_deleted, 0);
        assert_eq!(report.has_more, scanned == 48);
    }
    let (idle, submissions) = fixture.tick().await;
    assert!(!idle.has_committed_work());
    assert!(!idle.has_more);
    assert_eq!(submissions, 0);
    fixture.assert_bodies_preserved().await;
}

#[asupersync::test]
async fn bootstrap_batch_refusal_rolls_back_and_recovers() {
    for bodies_phase in [false, true] {
        let fixture = Fixture::open().await;
        if bodies_phase {
            fixture.tick().await;
            fixture.tick().await;
        }
        let before = read_progress(&fixture.conn).await.unwrap();
        let table = if bodies_phase {
            "canonical_body_gc_candidates"
        } else {
            "canonical_body_gc_refs"
        };
        // FAIL deliberately leaves the first 47 inserts in the transaction.
        // The collector's rollback must discard them with the uncommitted page.
        fixture
            .conn
            .execute_batch(&format!(
                "CREATE TRIGGER test_bootstrap_refusal BEFORE INSERT ON {table}
                 WHEN (SELECT COUNT(*) FROM {table}) = 47 BEGIN
                    SELECT RAISE(FAIL, 'injected bootstrap batch refusal');
                 END"
            ))
            .await
            .unwrap();
        let transaction = fixture
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let error = step(&transaction, 48).await.unwrap_err();
        assert!(error.to_string().contains("injected bootstrap batch refusal"));
        transaction.rollback().await.unwrap();
        assert_eq!(read_progress(&fixture.conn).await.unwrap(), before);
        let count = scalar(&fixture.conn, &format!("SELECT COUNT(*) FROM {table}")).await;
        assert_eq!(count, 0);
        fixture.assert_bodies_preserved().await;
        fixture
            .conn
            .execute_batch("DROP TRIGGER test_bootstrap_refusal")
            .await
            .unwrap();
        for scanned in [48_u64, 4] {
            let (report, submissions) = fixture.tick().await;
            let actual = if bodies_phase {
                report.bodies_scanned
            } else {
                report.roots_scanned
            };
            assert_eq!(submissions, 2);
            assert_eq!(actual, scanned);
            assert_eq!(report.bodies_deleted, 0);
        }
        let count = scalar(&fixture.conn, &format!("SELECT COUNT(*) FROM {table}")).await;
        assert_eq!(count, 52);
        fixture.assert_bodies_preserved().await;
    }
}

#[asupersync::test]
async fn bootstrap_batch_eligibility_does_not_change_the_page_boundary() {
    let fixture = Fixture::open().await;
    fixture
        .conn
        .execute_batch(
            "UPDATE lcm_raw_messages SET storage_kind = 'external' WHERE store_id = 94;
             UPDATE lcm_raw_messages SET content = 'owned inline text' WHERE store_id = 96;",
        )
        .await
        .unwrap();
    let (report, submissions) = fixture.tick().await;
    let progress = read_progress(&fixture.conn).await.unwrap();
    assert_eq!(report.roots_scanned, 48);
    assert_eq!(submissions, 2);
    assert_eq!(progress.raw_through, 96);
    assert_eq!(
        raw_owners(&fixture.conn).await,
        (1..=46_i64).map(|ordinal| ordinal * 2).collect::<Vec<_>>()
    );
    assert!(report.has_more);
    let (tail, submissions) = fixture.tick().await;
    let progress = read_progress(&fixture.conn).await.unwrap();
    assert_eq!(tail.roots_scanned, 4);
    assert_eq!(submissions, 2);
    assert_eq!(progress.raw_through, 104);
    let expected = (1..=52_i64)
        .filter(|ordinal| ![47, 48].contains(ordinal))
        .map(|ordinal| ordinal * 2)
        .collect::<Vec<_>>();
    assert_eq!(raw_owners(&fixture.conn).await, expected);
    fixture.assert_bodies_preserved().await;
}

async fn observation_owners(conn: &(impl QueryExecutor + ?Sized)) -> Vec<i64> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT owner_id FROM canonical_body_gc_refs
             WHERE owner_kind = 'observation' ORDER BY owner_id",
            (),
        )
        .await
        .unwrap();
    let mut owners = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        owners.push(row.get(0).unwrap());
    }
    owners
}

#[asupersync::test]
async fn observation_bootstrap_batches_across_a_bounded_write_submission() {
    let fixture = Fixture::with_observations(true).await;
    for _ in 0..2 {
        fixture.tick().await;
    }
    for (scanned, frontier, owners, refs, submissions) in [
        (48_u64, 96_i64, 48_i64, 141_i64, 3_usize),
        (4, 104, 52, 153, 2),
    ] {
        let (report, actual_submissions) = fixture.tick().await;
        assert_eq!(report.roots_scanned, scanned);
        assert_eq!(report.bootstrap_frontier_after, Some(format!("observations:{frontier}")));
        assert_eq!(read_progress(&fixture.conn).await.unwrap().observations_through, frontier);
        assert_eq!(actual_submissions, submissions);
        assert_eq!(
            observation_owners(&fixture.conn).await,
            (1..=owners).map(|ordinal| ordinal * 2).collect::<Vec<_>>()
        );
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) FROM canonical_body_gc_refs
                WHERE owner_kind = 'observation'")
                .await,
            refs,
            "duplicate references within an observation must stay deduplicated"
        );
        assert_eq!(report.bodies_deleted, 0);
        assert!(report.has_more);
    }
    let mut finished = false;
    for _ in 0..6 {
        let (report, _) = fixture.tick().await;
        assert_eq!(report.bodies_deleted, 0);
        if !report.has_more {
            finished = true;
            break;
        }
    }
    assert!(finished, "bounded bootstrap and candidate discharge must terminate");
    fixture.assert_bodies_preserved().await;
    let (idle, submissions) = fixture.tick().await;
    assert!(!idle.has_more);
    assert!(!idle.has_committed_work());
    assert_eq!(submissions, 0);
}

#[asupersync::test]
async fn observation_bootstrap_failure_after_a_full_batch_rolls_back_and_recovers() {
    let fixture = Fixture::with_observations(true).await;
    for _ in 0..2 {
        fixture.tick().await;
    }
    let before = read_progress(&fixture.conn).await.unwrap();
    fixture
        .conn
        .execute_batch(
            "CREATE TRIGGER refuse_observation_seed BEFORE INSERT ON canonical_body_gc_refs
             WHEN NEW.owner_kind = 'observation' AND NEW.owner_id = 94 BEGIN
                 SELECT RAISE(FAIL, 'injected observation batch refusal');
             END",
        )
        .await
        .unwrap();
    let transaction = fixture
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let error = step(&transaction, 48).await.unwrap_err();
    assert!(error.to_string().contains("injected observation batch refusal"));
    transaction.rollback().await.unwrap();
    assert_eq!(read_progress(&fixture.conn).await.unwrap(), before);
    assert!(observation_owners(&fixture.conn).await.is_empty());
    fixture.assert_bodies_preserved().await;

    fixture
        .conn
        .execute_batch("DROP TRIGGER refuse_observation_seed")
        .await
        .unwrap();
    for (frontier, references, submissions) in [(96_i64, 141_i64, 3_usize), (104, 153, 2)] {
        let (report, actual_submissions) = fixture.tick().await;
        assert_eq!(report.bootstrap_frontier_after, Some(format!("observations:{frontier}")));
        assert_eq!(actual_submissions, submissions);
        assert_eq!(
            scalar(&fixture.conn, "SELECT COUNT(*) FROM canonical_body_gc_refs
                WHERE owner_kind = 'observation'")
                .await,
            references
        );
    }
    fixture.assert_bodies_preserved().await;
}
