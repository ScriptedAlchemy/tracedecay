use super::*;

async fn pinned_wal_reader() -> (tempfile::TempDir, GlobalDb, libsql::Database, Connection) {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at_without_structured_backfill(&db_path)
        .await
        .unwrap();
    db.conn
        .execute_batch(
            "PRAGMA wal_autocheckpoint=0;
             PRAGMA busy_timeout=1;
             CREATE TABLE checkpoint_probe(value INTEGER NOT NULL);
             INSERT INTO checkpoint_probe(value) VALUES (1);",
        )
        .await
        .unwrap();

    let reader_db = Builder::new_local(&db_path)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .unwrap();
    let reader = reader_db.connect().unwrap();
    reader.execute("BEGIN", ()).await.unwrap();
    let mut rows = reader
        .query("SELECT COUNT(*) FROM checkpoint_probe", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
    drop(rows);

    db.conn
        .execute("INSERT INTO checkpoint_probe(value) VALUES (2)", ())
        .await
        .unwrap();
    (dir, db, reader_db, reader)
}

#[tokio::test]
async fn checkpoint_result_reports_busy_and_recovers_after_reader_finishes() {
    let (_dir, db, _reader_db, reader) = pinned_wal_reader().await;

    let error = db.checkpoint_result().await.unwrap_err().to_string();
    assert!(error.contains("WAL checkpoint incomplete"), "{error}");
    assert!(error.contains("busy=1"), "{error}");
    assert!(error.contains("log_frames="), "{error}");
    assert!(error.contains("checkpointed_frames="), "{error}");

    reader.execute("COMMIT", ()).await.unwrap();
    db.checkpoint_result().await.unwrap();
}

#[tokio::test]
async fn public_checkpoint_remains_best_effort_when_reader_is_busy() {
    let (_dir, db, _reader_db, reader) = pinned_wal_reader().await;

    db.checkpoint().await;

    reader.execute("COMMIT", ()).await.unwrap();
    db.checkpoint_result().await.unwrap();
}
