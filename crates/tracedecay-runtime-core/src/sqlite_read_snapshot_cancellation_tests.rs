use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tempfile::TempDir;

use super::{SnapshotReadControl, family_state, open_foreign_in};

#[tokio::test]
async fn foreign_snapshot_cancellation_interrupts_mid_materialization() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("large-foreign.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE durable(value BLOB NOT NULL);
             INSERT INTO durable(value) VALUES (zeroblob(16777216));",
        )
        .unwrap();
    drop(connection);
    let before = family_state(&path).unwrap();
    let checkpoints = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&checkpoints);
    let control = SnapshotReadControl::new(Instant::now() + Duration::from_secs(30), move || {
        observed.fetch_add(1, Ordering::Relaxed) >= 8
    });

    let error = match open_foreign_in(&path, &temp.path().join("scratch"), control).await {
        Ok(_) => panic!("cooperative cancellation must interrupt snapshot materialization"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(checkpoints.load(Ordering::Relaxed) >= 8);
    assert_eq!(family_state(&path).unwrap(), before);
}
