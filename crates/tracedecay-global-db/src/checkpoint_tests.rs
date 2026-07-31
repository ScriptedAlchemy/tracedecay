use crate::tests::harness::RegisteredGlobalDbHarness;

async fn pinned_wal_reader() -> (RegisteredGlobalDbHarness, crate::db::engine::ReadSnapshot) {
    let harness = RegisteredGlobalDbHarness::open("pinned-wal-reader").await;
    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute_batch(
            "PRAGMA wal_autocheckpoint=0;
             PRAGMA busy_timeout=1;
             CREATE TABLE checkpoint_probe(value INTEGER NOT NULL);
             INSERT INTO checkpoint_probe(value) VALUES (1);",
        )
        .await
        .unwrap();

    let reader = harness.registered.read_snapshot().await.unwrap();
    let mut rows = reader
        .query("SELECT COUNT(*) FROM checkpoint_probe", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
    drop(rows);

    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute("INSERT INTO checkpoint_probe(value) VALUES (2)", ())
        .await
        .unwrap();
    (harness, reader)
}

#[tokio::test]
async fn checkpoint_result_reports_busy_and_recovers_after_reader_finishes() {
    let (harness, reader) = pinned_wal_reader().await;

    let error = harness
        .registered
        .checkpoint_result()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("WAL checkpoint incomplete"), "{error}");
    assert!(error.contains("busy=1"), "{error}");
    assert!(error.contains("log_frames="), "{error}");
    assert!(error.contains("checkpointed_frames="), "{error}");

    drop(reader);
    harness.registered.checkpoint_result().await.unwrap();
}

#[tokio::test]
async fn public_checkpoint_remains_best_effort_when_reader_is_busy() {
    let (harness, reader) = pinned_wal_reader().await;

    harness.registered.checkpoint().await;

    drop(reader);
    harness.registered.checkpoint_result().await.unwrap();
}
