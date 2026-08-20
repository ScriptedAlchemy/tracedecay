//! Bounds the write cost of the foreign/WAL read-snapshot path.
//!
//! Measured through `/proc/self/io` `wchar` (bytes passed to write syscalls by
//! this process), so the whole suite is Linux-only. `cargo nextest` runs each
//! test in its own process; the single test per binary keeps a plain
//! `cargo test` process equally quiet.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::Path;
use std::time::Instant;

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_runtime_core::sqlite_read_snapshot::{SnapshotReadControl, open_foreign_in};

const MAIN_TARGET_BYTES: i64 = 32 * 1024 * 1024;
const WAL_TARGET_BYTES: i64 = 4 * 1024 * 1024;
const CHECKPOINT_AND_NOISE_ALLOWANCE: u64 = 4 * 1024 * 1024;

fn written_bytes() -> u64 {
    let io = fs::read_to_string("/proc/self/io").expect("read /proc/self/io");
    io.lines()
        .find_map(|line| line.strip_prefix("wchar: "))
        .expect("/proc/self/io must report wchar")
        .trim()
        .parse()
        .expect("wchar must be an integer")
}

fn file_bytes(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    std::path::PathBuf::from(value)
}

/// Capturing a foreign WAL family must cost about one family copy plus one
/// WAL checkpoint of that copy — never another full rewrite of the main
/// database. The previous materialization backed the copy up into a second
/// standalone file, which rewrote every main-database page again; the budget
/// asserted here is structurally below that behavior on both reflink and
/// plain-copy filesystems.
#[tokio::test]
async fn foreign_wal_snapshot_write_cost_stays_near_one_family_copy() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("foreign.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE durable(id INTEGER PRIMARY KEY, value BLOB NOT NULL);",
        )
        .unwrap();
    writer
        .execute(
            "INSERT INTO durable(id, value) VALUES (1, zeroblob(?1))",
            [MAIN_TARGET_BYTES],
        )
        .unwrap();
    writer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    writer
        .execute(
            "INSERT INTO durable(id, value) VALUES (2, zeroblob(?1))",
            [WAL_TARGET_BYTES],
        )
        .unwrap();

    let main_bytes = file_bytes(&path);
    let wal_bytes = file_bytes(&sidecar(&path, "-wal"));
    let shm_bytes = file_bytes(&sidecar(&path, "-shm"));
    assert!(
        main_bytes >= MAIN_TARGET_BYTES as u64,
        "fixture main database holds the bulk: {main_bytes} bytes"
    );
    assert!(
        wal_bytes >= WAL_TARGET_BYTES as u64 && wal_bytes < main_bytes / 2,
        "fixture WAL must be non-trivial but much smaller than the main file: {wal_bytes} bytes"
    );

    // The snapshot reflinks the main file when the filesystem supports it and
    // falls back to a plain copy otherwise. Probe the same filesystem the
    // same way so the budget matches whichever branch the capture takes.
    let probe = temp.path().join("reflink-probe.db");
    let reflink = reflink_copy::reflink(&path, &probe).is_ok();
    if reflink {
        fs::remove_file(&probe).unwrap();
    }
    let copy_bytes = if reflink {
        wal_bytes + shm_bytes
    } else {
        main_bytes + wal_bytes + shm_bytes
    };
    // One family copy, plus checkpointing the copied WAL frames into the
    // copy, plus a fixed allowance for SQLite bookkeeping and harness noise.
    let budget = copy_bytes + wal_bytes + CHECKPOINT_AND_NOISE_ALLOWANCE;
    assert!(
        budget < copy_bytes + main_bytes,
        "budget must itself exclude a second full main-database rewrite: \
         budget {budget}, copy {copy_bytes}, main {main_bytes}"
    );

    let before = written_bytes();
    let started = Instant::now();
    let snapshot = open_foreign_in(
        &path,
        &temp.path().join("scratch"),
        SnapshotReadControl::unlimited(),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    let written = written_bytes() - before;

    let mut rows = snapshot
        .connection()
        .query("SELECT id, length(value) FROM durable ORDER BY id", ())
        .await
        .unwrap();
    let mut lengths = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        lengths.push((row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap()));
    }
    assert_eq!(
        lengths,
        [(1, MAIN_TARGET_BYTES), (2, WAL_TARGET_BYTES)],
        "the snapshot must expose both checkpointed and WAL-resident rows"
    );

    assert!(
        written <= budget,
        "foreign WAL snapshot wrote {written} bytes, over the {budget}-byte budget \
         (reflink {reflink}, main {main_bytes}, wal {wal_bytes}, shm {shm_bytes}); \
         a materialization that rewrites the full main database exceeds this"
    );
    println!(
        "foreign WAL snapshot wrote {written} bytes in {elapsed:?} \
         (budget {budget}, reflink {reflink}, main {main_bytes}, wal {wal_bytes})"
    );
    drop(writer);
}
