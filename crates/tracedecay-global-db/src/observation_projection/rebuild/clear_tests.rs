use std::collections::BTreeMap;

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::{
    SESSION_MESSAGE_PROJECTOR_VERSION, clear_active_projection, read_message, write_staged_message,
};
use crate::tests::harness::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, seed_projected_messages,
};

async fn scalar(conn: &impl QueryExecutor, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn raw_bodies(conn: &impl QueryExecutor) -> BTreeMap<(String, String), String> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, content FROM lcm_raw_messages
             ORDER BY provider, message_id",
            (),
        )
        .await
        .unwrap();
    let mut bodies = BTreeMap::new();
    while let Some(row) = rows.next().await.unwrap() {
        bodies.insert(
            (row.get(0).unwrap(), row.get(1).unwrap()),
            row.get(2).unwrap(),
        );
    }
    bodies
}

#[tokio::test]
async fn clear_rebuild_uses_durable_output_owners_without_retaining_temp_pages() {
    const GENERATION: &str = "generation.output-owner-clear";
    const OTHER_PROJECTOR: &str = "retained-session-projector.v1";
    const OUTPUTS: usize = 60;
    let directory = tempfile::tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .unwrap();
    let seeded = seed_projected_messages(&runtime, 0..OUTPUTS).await;
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    let original = raw_bodies(&transaction).await;
    assert_eq!(original.len(), OUTPUTS);
    let mut expected = original.clone();
    transaction
        .execute(
            "INSERT INTO observation_projection_rebuilds
             (projector_version, generation, frontier_sequence, state)
             VALUES (?1, ?2, 0, 'ready')",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, GENERATION],
        )
        .await
        .unwrap();

    for (index, (observation, _)) in seeded.iter().enumerate() {
        let observation_id = observation.observation_id().as_str();
        let mut rows = transaction
            .query(
                "SELECT output_provider, output_message_id
                 FROM observation_projection_provenance
                 WHERE projector_version = ?1 AND observation_id = ?2",
                params![SESSION_MESSAGE_PROJECTOR_VERSION, observation_id],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let provider: String = row.get(0).unwrap();
        let message_id: String = row.get(1).unwrap();
        drop(rows);

        // Exercise created, staged, shared, adopted, shared-adopted and
        // other-projector-only output ownership over real admitted messages.
        let kind = index % 6;
        if kind == 0 {
            expected.remove(&(provider.clone(), message_id.clone()));
        }
        if kind == 1 {
            let message = read_message(&transaction, &provider, &message_id)
                .await
                .unwrap()
                .unwrap();
            write_staged_message(&transaction, GENERATION, &message)
                .await
                .unwrap();
        }
        if matches!(kind, 3 | 4) {
            transaction
                .execute(
                    "UPDATE observation_projection_provenance SET message_created = 0
                     WHERE projector_version = ?1 AND observation_id = ?2",
                    params![SESSION_MESSAGE_PROJECTOR_VERSION, observation_id],
                )
                .await
                .unwrap();
        }
        if matches!(kind, 2 | 4 | 5) {
            transaction
                .execute(
                    "INSERT INTO observation_projection_provenance (
                        projector_version, observation_id, output_ordinal, receipt_id,
                        output_provider, output_message_id, output_digest, message_created,
                        retrieval_anchor_id
                     ) SELECT ?1, observation_id, output_ordinal, receipt_id,
                              output_provider, output_message_id, output_digest,
                              message_created, retrieval_anchor_id
                       FROM observation_projection_provenance
                       WHERE projector_version = ?2 AND observation_id = ?3",
                    params![
                        OTHER_PROJECTOR,
                        SESSION_MESSAGE_PROJECTOR_VERSION,
                        observation_id
                    ],
                )
                .await
                .unwrap();
        }
        if kind == 5 {
            transaction
                .execute(
                    "DELETE FROM observation_projection_provenance
                     WHERE projector_version = ?1 AND observation_id = ?2",
                    params![SESSION_MESSAGE_PROJECTOR_VERSION, observation_id],
                )
                .await
                .unwrap();
        }
    }

    let baseline_temp_pages = scalar(&transaction, "PRAGMA temp.page_count").await;
    for _ in 0..2 {
        clear_active_projection(&transaction, GENERATION)
            .await
            .unwrap();
        assert_eq!(raw_bodies(&transaction).await, expected);
        let mut rows = transaction
            .query(
                "SELECT projector_version, COUNT(*)
                 FROM observation_projection_provenance GROUP BY projector_version",
                (),
            )
            .await
            .unwrap();
        let mut owners = BTreeMap::<String, i64>::new();
        while let Some(row) = rows.next().await.unwrap() {
            owners.insert(row.get(0).unwrap(), row.get(1).unwrap());
        }
        drop(rows);
        assert_eq!(owners[SESSION_MESSAGE_PROJECTOR_VERSION], 20);
        assert_eq!(owners[OTHER_PROJECTOR], 30);
        assert_eq!(owners.len(), 2);
        assert_eq!(
            scalar(&transaction, "SELECT COUNT(*) FROM observations").await,
            60
        );
        assert_eq!(
            scalar(&transaction, "SELECT COUNT(*) FROM sanitization_receipts").await,
            60,
        );
        assert_eq!(
            scalar(&transaction, "PRAGMA temp.page_count").await,
            baseline_temp_pages,
            "repeated rebuild clearing must not retain a payload-sized TEMP database",
        );
    }
    transaction.rollback().await.unwrap();

    // The production caller owns the transaction. A failure after raw-row
    // deletion must roll back every deleted output, not leave a partial clear.
    let transaction = database.begin_write_transaction().await.unwrap();
    assert_eq!(raw_bodies(&transaction).await, original);
    transaction
        .execute_batch(
            "CREATE TRIGGER refuse_projection_clear
             BEFORE DELETE ON observation_projection_provenance
             BEGIN SELECT RAISE(ABORT, 'injected projection clear failure'); END;",
        )
        .await
        .unwrap();
    assert!(
        clear_active_projection(&transaction, GENERATION)
            .await
            .is_err()
    );
    assert!(raw_bodies(&transaction).await.is_empty());
    transaction.rollback().await.unwrap();
    let snapshot = database.read_snapshot().await.unwrap();
    assert_eq!(raw_bodies(&snapshot).await, original);
    let mut integrity = snapshot.query("PRAGMA integrity_check", ()).await.unwrap();
    assert_eq!(
        integrity
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "ok",
    );
}
