//! Full-text candidate work is bounded inside the engine, not only by the rows
//! a caller keeps. The temporal candidate shape asks for every hit of a phrase
//! and keeps the newest one, so the index hands the statement every match.
//! When the index streams those hits through VM operations, the statement's
//! progress guard sees each of them, and the per-statement operation budget
//! (the native driver's `LimitExceeded`) bounds the search. An index that
//! materialized its hits inside one operation would do unbounded work the
//! budget never sees.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tempfile::TempDir;
use tracedecay_runtime_core::db::{engine::BackendKind, native_search::SearchIndex};
use tracedecay_sql::{Connection, ConnectionControl, EnginePolicy, OptionalExtension};

fn occurrence_store(rows: usize) -> (TempDir, Connection) {
    let directory = TempDir::new().unwrap();
    let mut connection = Connection::open(directory.path().join("fts-budget.db")).unwrap();
    connection.apply_engine_policy().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session_occurrences (
                 occurrence_id TEXT NOT NULL PRIMARY KEY,
                 knowledge_at INTEGER NOT NULL,
                 index_text TEXT NOT NULL
             );",
        )
        .unwrap();
    connection
        .execute_batch(match BackendKind::EXACT_SQL {
            BackendKind::Sqlite => {
                "CREATE VIRTUAL TABLE session_occurrences_fts USING fts5(
                     index_text, content='session_occurrences', content_rowid='rowid'
                 );"
            }
            BackendKind::NativeTurso => SearchIndex::Occurrence.create_sql(),
        })
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for row in 0..rows {
        transaction
            .execute(
                "INSERT INTO session_occurrences VALUES (?1, ?2, ?3)",
                (
                    format!("occurrence-{row:06}"),
                    i64::try_from(row).unwrap(),
                    format!("needle filler words {row}"),
                ),
            )
            .unwrap();
    }
    if BackendKind::EXACT_SQL == BackendKind::Sqlite {
        transaction
            .execute_batch(
                "INSERT INTO session_occurrences_fts(rowid, index_text)
                 SELECT rowid, index_text FROM session_occurrences;",
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    (directory, connection)
}

/// The temporal occurrence candidate shape on the linked engine: every hit of
/// the phrase, newest first, one row kept.
fn newest_hit_sql() -> &'static str {
    match BackendKind::EXACT_SQL {
        BackendKind::Sqlite => {
            "SELECT o.occurrence_id
             FROM session_occurrences_fts
             JOIN session_occurrences AS o ON o.rowid = session_occurrences_fts.rowid
             WHERE session_occurrences_fts MATCH ?1
             ORDER BY o.knowledge_at DESC, o.occurrence_id
             LIMIT 1"
        }
        BackendKind::NativeTurso => {
            "WITH indexed_hits AS MATERIALIZED (
                 SELECT rowid AS hit_rowid FROM session_occurrences
                 WHERE fts_match(index_text, ?1)
             )
             SELECT o.occurrence_id
             FROM indexed_hits AS search_hits
             JOIN session_occurrences AS o ON o.rowid = search_hits.hit_rowid
             ORDER BY o.knowledge_at DESC, o.occurrence_id
             LIMIT 1"
        }
    }
}

/// Progress-guard polls one newest-hit search takes, and the hit it keeps.
fn guarded_search(connection: &Connection) -> (u64, Option<String>) {
    let polls = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&polls);
    let newest = connection
        .guard_scope(
            move || {
                counted.fetch_add(1, Ordering::Relaxed);
                false
            },
            |connection| {
                connection
                    .query_row(newest_hit_sql(), ["needle"], |row| row.get::<_, String>(0))
                    .optional()
            },
        )
        .unwrap()
        .unwrap();
    (polls.load(Ordering::Relaxed), newest)
}

#[test]
fn full_text_candidate_work_is_metered_per_hit_and_stoppable() {
    let (_small_directory, small) = occurrence_store(1_000);
    let (_large_directory, large) = occurrence_store(8_000);
    let (small_polls, small_newest) = guarded_search(&small);
    let (large_polls, large_newest) = guarded_search(&large);
    assert_eq!(small_newest.as_deref(), Some("occurrence-000999"));
    assert_eq!(large_newest.as_deref(), Some("occurrence-007999"));
    assert!(
        small_polls > 0 && large_polls >= small_polls * 4,
        "eight times the hits must cost the guard at least four times the polls: \
         {small_polls} -> {large_polls}"
    );

    let stopped = large
        .guard_scope(
            || true,
            |connection| {
                connection
                    .query_row(newest_hit_sql(), ["needle"], |row| row.get::<_, String>(0))
                    .optional()
            },
        )
        .unwrap();
    assert!(
        stopped.is_err(),
        "a guard that trips during the hit stream must stop the search: {stopped:?}"
    );
}
