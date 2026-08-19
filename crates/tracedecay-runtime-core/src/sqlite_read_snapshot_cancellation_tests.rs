use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tempfile::TempDir;

use super::{SnapshotReadControl, family_state, open_foreign_in};

#[tokio::test]
async fn foreign_snapshot_cancellation_interrupts_mid_capture() {
    // Bulk stays WAL-resident so cancel can land in copy or in the
    // progress_handler-backed journal_mode=DELETE fold. Either path must
    // return Interrupted, never Ok.
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("large-foreign.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE durable(value BLOB NOT NULL);
             INSERT INTO durable(value) VALUES (zeroblob(16777216));",
        )
        .unwrap();
    assert!(
        super::with_suffix(&path, "-wal").metadata().unwrap().len() >= 16777216,
        "fixture must keep its bulk WAL-resident so cancellation lands mid-copy"
    );
    let before = family_state(&path).unwrap();
    let checkpoints = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&checkpoints);
    let control = SnapshotReadControl::new(Instant::now() + Duration::from_secs(30), move || {
        observed.fetch_add(1, Ordering::Relaxed) >= 8
    });

    let error = match open_foreign_in(&path, &temp.path().join("scratch"), control).await {
        Ok(_) => panic!("cooperative cancellation must interrupt the snapshot capture"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(checkpoints.load(Ordering::Relaxed) >= 8);
    assert_eq!(family_state(&path).unwrap(), before);
    drop(writer);
}
