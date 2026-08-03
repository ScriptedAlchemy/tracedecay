use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::{Connection, TransactionBehavior, params};

use crate::common::{
    BATCH, CaseMetrics, CrashMetrics, DIM, QUERY_IDS, ROWS, Timing, cosine_distance_bytes,
    directory_bytes, peak_rss_kib, query_metrics, synthetic_vector, top_k, vector_bytes,
};

const DB_NAME: &str = "vectors.sqlite3";

pub fn run(root: &Path, executable: &Path) -> Result<CaseMetrics> {
    std::fs::create_dir_all(root)?;
    let db_path = root.join(DB_NAME);
    let mut connection = open(&db_path)?;
    create_schema(&connection)?;

    let ingest_started = Instant::now();
    for start in (0..ROWS).step_by(BATCH) {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO staged_vectors(build_id, chunk_id, vector) VALUES (1, ?1, ?2)",
            )?;
            for id in start..(start + BATCH).min(ROWS) {
                insert.execute(params![
                    id as i64,
                    vector_bytes(&synthetic_vector(id as u64, 0))
                ])?;
            }
        }
        transaction.commit()?;
    }
    let ingest_ms = ingest_started.elapsed().as_secs_f64() * 1_000.0;

    let publish_started = Instant::now();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO active_vectors(chunk_id, vector)
         SELECT chunk_id, vector FROM staged_vectors WHERE build_id = 1",
        [],
    )?;
    let changed = transaction.execute(
        "UPDATE active_state
         SET revision = 1, generation_id = 'generation-1'
         WHERE singleton = 1 AND revision = 0",
        [],
    )?;
    anyhow::ensure!(changed == 1, "initial pointer compare-and-swap failed");
    transaction.execute("DELETE FROM staged_vectors WHERE build_id = 1", [])?;
    transaction.commit()?;
    let initial_publish_ms = publish_started.elapsed().as_secs_f64() * 1_000.0;

    let mut deltas = Vec::new();
    for (delta_index, rows) in [1_usize, 100, 4_096].into_iter().enumerate() {
        let revision = delta_index as u64 + 2;
        let started = Instant::now();
        publish_delta(&mut connection, revision, rows)?;
        deltas.push(Timing {
            operation: format!("publish_delta_{rows}"),
            rows,
            millis: started.elapsed().as_secs_f64() * 1_000.0,
        });
    }

    let exact = exact_queries(&connection)?;
    let crash = crash_test(&db_path, executable)?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let active_revision = connection.query_row(
        "SELECT revision FROM active_state WHERE singleton = 1",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;

    Ok(CaseMetrics {
        engine: "normalized-sqlite".to_owned(),
        rows: ROWS,
        dimensions: DIM,
        ingest_ms,
        initial_publish_ms,
        deltas,
        exact,
        ann: None,
        ann_recall_at_10: None,
        peak_rss_kib: peak_rss_kib(),
        disk_bytes: directory_bytes(root)?,
        crash,
        active_revision,
        active_native_version: None,
        publication_primitive:
            "single SQLite BEGIN IMMEDIATE transaction + revision-guarded pointer UPDATE".to_owned(),
        native_expected_version_cas: true,
    })
}

pub fn crash_child(db_path: &Path, commit: bool) -> Result<()> {
    let mut connection = open(db_path)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let revision = transaction.query_row(
        "SELECT revision FROM active_state WHERE singleton = 1",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;
    transaction.execute(
        "UPDATE active_state SET revision = ?1, generation_id = ?2
         WHERE singleton = 1 AND revision = ?3",
        params![
            (revision + 1) as i64,
            format!("crash-generation-{}", revision + 1),
            revision as i64
        ],
    )?;
    if commit {
        transaction.commit()?;
    }
    unsafe { libc::_exit(86) }
}

fn open(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;",
    )?;
    Ok(connection)
}

fn create_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE active_state(
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             revision INTEGER NOT NULL,
             generation_id TEXT NOT NULL
         );
         INSERT INTO active_state VALUES(1, 0, 'none');
         CREATE TABLE staged_vectors(
             build_id INTEGER NOT NULL,
             chunk_id INTEGER NOT NULL,
             vector BLOB NOT NULL,
             PRIMARY KEY(build_id, chunk_id)
         ) WITHOUT ROWID;
         CREATE TABLE active_vectors(
             chunk_id INTEGER PRIMARY KEY,
             vector BLOB NOT NULL
         );
         CREATE TABLE generation_deltas(
             generation_id TEXT NOT NULL,
             chunk_id INTEGER NOT NULL,
             before_vector BLOB NOT NULL,
             after_vector BLOB NOT NULL,
             PRIMARY KEY(generation_id, chunk_id)
         ) WITHOUT ROWID;",
    )?;
    Ok(())
}

fn publish_delta(connection: &mut Connection, revision: u64, count: usize) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    {
        let mut before =
            transaction.prepare("SELECT vector FROM active_vectors WHERE chunk_id = ?1")?;
        let mut delta = transaction.prepare(
            "INSERT INTO generation_deltas(
                 generation_id, chunk_id, before_vector, after_vector
             ) VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut update =
            transaction.prepare("UPDATE active_vectors SET vector = ?1 WHERE chunk_id = ?2")?;
        for id in 0..count {
            let old = before.query_row([id as i64], |row| row.get::<_, Vec<u8>>(0))?;
            let new = vector_bytes(&synthetic_vector(id as u64, revision));
            delta.execute(params![
                format!("generation-{revision}"),
                id as i64,
                old,
                new
            ])?;
            update.execute(params![new, id as i64])?;
        }
    }
    let changed = transaction.execute(
        "UPDATE active_state SET revision = ?1, generation_id = ?2
         WHERE singleton = 1 AND revision = ?3",
        params![
            revision as i64,
            format!("generation-{revision}"),
            (revision - 1) as i64
        ],
    )?;
    anyhow::ensure!(changed == 1, "delta pointer compare-and-swap failed");
    transaction.commit()?;
    Ok(())
}

fn exact_queries(connection: &Connection) -> Result<crate::common::QueryMetrics> {
    let mut timings = Vec::new();
    let mut all_results = Vec::new();
    for query_id in QUERY_IDS {
        let query = synthetic_vector(query_id, 0);
        let started = Instant::now();
        let mut statement = connection.prepare("SELECT chunk_id, vector FROM active_vectors")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut scored = Vec::with_capacity(ROWS);
        for row in rows {
            let (id, bytes) = row?;
            scored.push((id, cosine_distance_bytes(&query, &bytes)?));
        }
        all_results.push(top_k(scored));
        timings.push(started.elapsed());
    }
    Ok(query_metrics("exact-full-scan", timings, all_results))
}

fn crash_test(db_path: &Path, executable: &Path) -> Result<CrashMetrics> {
    let before_revision = revision(db_path)?;
    invoke_crash_child(executable, "sqlite-crash", db_path, "before")?;
    let before_commit_old_visible = revision(db_path)? == before_revision;
    invoke_crash_child(executable, "sqlite-crash", db_path, "after")?;
    let after_commit_new_visible = revision(db_path)? == before_revision + 1;
    Ok(CrashMetrics {
        before_commit_old_visible,
        after_commit_new_visible,
    })
}

fn invoke_crash_child(executable: &Path, command: &str, db_path: &Path, phase: &str) -> Result<()> {
    let status = Command::new(executable)
        .arg(command)
        .arg(db_path)
        .arg(phase)
        .status()
        .context("launch SQLite crash child")?;
    anyhow::ensure!(status.code() == Some(86), "unexpected crash child {status}");
    Ok(())
}

fn revision(db_path: &Path) -> Result<u64> {
    Ok(open(db_path)?.query_row(
        "SELECT revision FROM active_state WHERE singleton = 1",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64)
}
