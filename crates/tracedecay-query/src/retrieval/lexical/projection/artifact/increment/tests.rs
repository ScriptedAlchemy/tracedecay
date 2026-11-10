use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::types::Value;
use rusqlite::{Connection, ErrorCode, params};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::{CodeLexicalArtifactErrorV1, ShiftV1, carry_row_chunks, shift_rowid_keys};

struct Active;
impl CodeIndexExecutionControlV1 for Active {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

fn fixture() -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let mut connection = Connection::open(directory.path().join("carry.sqlite")).unwrap();
    connection
        .execute_batch(
            "PRAGMA temp_store=FILE;
             PRAGMA cache_size=8;
             PRAGMA temp.cache_size=8;
             CREATE TABLE row_blocks(
                 first_document INTEGER PRIMARY KEY, payload BLOB NOT NULL
             );
             CREATE TABLE clone_occurrences(
                 ordinal INTEGER PRIMARY KEY,
                 symbol_key BLOB NOT NULL UNIQUE,
                 payload_ordinal INTEGER NOT NULL
             );
             CREATE TABLE independent(value TEXT NOT NULL);
             INSERT INTO independent VALUES ('live');",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    {
        let mut block = transaction
            .prepare("INSERT INTO row_blocks(first_document,payload) VALUES(?1,?2)")
            .unwrap();
        let mut occurrence = transaction
            .prepare(
                "INSERT INTO clone_occurrences(ordinal,symbol_key,payload_ordinal)
                 VALUES(?1,?2,?3)",
            )
            .unwrap();
        for n in 1_i64..=16_000 {
            block.execute(params![n, [7_u8; 32].as_slice()]).unwrap();
            occurrence
                .execute(params![n, n.to_be_bytes().as_slice(), n % 31])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    (directory, connection)
}

fn blocks(c: &Connection) -> Vec<(i64, Vec<u8>)> {
    let mut query = c
        .prepare("SELECT first_document,payload FROM row_blocks ORDER BY first_document")
        .unwrap();
    query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn occurrences(c: &Connection) -> Vec<(i64, Vec<u8>, i64)> {
    let mut query = c
        .prepare(
            "SELECT ordinal,symbol_key,payload_ordinal
             FROM clone_occurrences ORDER BY ordinal",
        )
        .unwrap();
    query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn integrity(c: &Connection) {
    for schema in ["main", "temp"] {
        let actual: String = c
            .query_row(&format!("PRAGMA {schema}.integrity_check"), [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(actual, "ok", "{schema} remains intact");
    }
}

#[test]
fn lexical_rowid_carry_preserves_disk_rows_across_replacements() {
    let (directory, mut connection) = fixture();
    let original_blocks = blocks(&connection);
    let original_occurrences = occurrences(&connection);
    assert_eq!(original_blocks.len(), 16_000);
    assert_eq!(original_occurrences.len(), 16_000);
    let shifts = [
        ShiftV1::new([((11, 20), 5), ((36, 39), 6)]).unwrap(),
        ShiftV1::new([((11, 20), 12), ((36, 39), 1)]).unwrap(),
    ];
    // Positive collision control: old in-place positive keys cannot just move.
    {
        let tx = connection.transaction().unwrap();
        let error = tx
            .execute("UPDATE row_blocks SET first_document=first_document+1", [])
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(ErrorCode::ConstraintViolation)
        );
        tx.rollback().unwrap();
    }
    for (index, shift) in shifts.iter().enumerate() {
        let expected_blocks: Vec<_> = original_blocks
            .iter()
            .filter_map(|(key, blob)| {
                shift
                    .map(u64::try_from(*key).unwrap())
                    .unwrap()
                    .map(|mapped| (i64::try_from(mapped).unwrap(), blob.clone()))
            })
            .collect();
        let expected_occurrences: Vec<_> = original_occurrences
            .iter()
            .filter_map(|(key, symbol, payload)| {
                shift
                    .map(u64::try_from(*key).unwrap())
                    .unwrap()
                    .map(|mapped| (i64::try_from(mapped).unwrap(), symbol.clone(), *payload))
            })
            .collect();
        let tx = connection.transaction().unwrap();
        for (table, column) in [
            ("row_blocks", "first_document"),
            ("clone_occurrences", "ordinal"),
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE {}", shift.sql_replaced(column)),
                [],
            )
            .unwrap();
            shift_rowid_keys(&tx, table, column, shift, &Active).unwrap();
        }
        assert_eq!(blocks(&tx), expected_blocks);
        assert_eq!(occurrences(&tx), expected_occurrences);
        assert_eq!(
            tx.pragma_query_value(Some("temp"), "page_count", |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        integrity(&tx);
        if index == 0 {
            tx.rollback().unwrap();
        } else {
            tx.commit().unwrap();
        }
        drop(connection);
        connection = Connection::open(directory.path().join("carry.sqlite")).unwrap();
        assert_eq!(
            blocks(&connection),
            if index == 0 {
                original_blocks.clone()
            } else {
                expected_blocks
            }
        );
        assert_eq!(
            occurrences(&connection),
            if index == 0 {
                original_occurrences.clone()
            } else {
                expected_occurrences
            }
        );
        let live: String = connection
            .query_row("SELECT value FROM independent", [], |r| r.get(0))
            .unwrap();
        assert_eq!(live, "live");
        integrity(&connection);
    }
}

#[test]
fn interrupted_rowid_carry_rolls_back_partial_key_staging() {
    let (directory, mut connection) = fixture();
    let original_blocks = blocks(&connection);
    let original_occurrences = occurrences(&connection);
    let shift = ShiftV1::new([((1, 1), 1)]).unwrap();
    struct CancelAfter {
        probes: std::sync::atomic::AtomicUsize,
        limit: usize,
    }
    impl CodeIndexExecutionControlV1 for CancelAfter {
        fn is_cancelled(&self) -> bool {
            self.probes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                >= self.limit
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }
    for occurrences_family in [false, true] {
        let tx = connection.transaction().unwrap();
        let cancel = CancelAfter {
            probes: std::sync::atomic::AtomicUsize::new(0),
            limit: 4_100,
        };
        let (table, column) = if occurrences_family {
            ("clone_occurrences", "ordinal")
        } else {
            ("row_blocks", "first_document")
        };
        let error = shift_rowid_keys(&tx, table, column, &shift, &cancel).unwrap_err();
        assert!(matches!(error, CodeLexicalArtifactErrorV1::Interrupted(_)));
        if occurrences_family {
            assert_ne!(occurrences(&tx), original_occurrences);
        } else {
            assert_ne!(blocks(&tx), original_blocks);
        }
        integrity(&tx);
        drop(tx);
        drop(connection);
        connection = Connection::open(directory.path().join("carry.sqlite")).unwrap();
        assert_eq!(blocks(&connection), original_blocks);
        assert_eq!(occurrences(&connection), original_occurrences);
        integrity(&connection);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn lexical_rowid_shift_does_not_spill_unlinked_sqlite_files() {
    use std::io;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    fn spill_bytes() -> io::Result<u64> {
        let mut total = 0;
        for entry in std::fs::read_dir("/proc/self/fd")? {
            let path = entry?.path();
            let target = match std::fs::read_link(&path) {
                Ok(target) => target,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if target
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("etilqs_"))
            {
                total += std::fs::metadata(path)?.len();
            }
        }
        Ok(total)
    }

    const CHILD: &str = "TRACEDECAY_ROWID_CARRY_SPILL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("lexical_rowid_shift_does_not_spill_unlinked_sqlite_files")
            .arg("--test-threads=1")
            .arg("--nocapture")
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(
            stdout.contains("1 passed"),
            "child test was vacuous: {stdout}"
        );
        return;
    }
    let (_directory, mut connection) = fixture();
    let shift = ShiftV1::new([((1, 1), 1)]).unwrap();
    let original = blocks(&connection);
    let peak = Arc::new(AtomicU64::new(0));
    let unreadable = Arc::new(AtomicBool::new(false));
    let observer_peak = Arc::clone(&peak);
    let observer_failed = Arc::clone(&unreadable);
    connection.progress_handler(
        1_024,
        Some(move || match spill_bytes() {
            Ok(bytes) => {
                observer_peak.fetch_max(bytes, Ordering::Relaxed);
                false
            }
            Err(_) => {
                observer_failed.store(true, Ordering::Relaxed);
                true
            }
        }),
    )
    .unwrap();
    {
        let tx = connection.transaction().unwrap();
        tx.execute(
            "UPDATE row_blocks SET first_document=-first_document-1 WHERE first_document>0",
            [],
        )
        .unwrap();
        // The second bulk UPDATE materializes the transient rowid set.
        tx.execute(
            "UPDATE row_blocks SET first_document=-first_document-1 WHERE first_document<0",
            [],
        )
        .unwrap();
        tx.rollback().unwrap();
    }
    assert!(!unreadable.load(Ordering::Relaxed));
    assert!(
        peak.load(Ordering::Relaxed) > 0,
        "old bulk rowid rewrite must spill"
    );
    assert_eq!(
        connection
            .pragma_query_value(Some("temp"), "page_count", |r| r.get::<_, i64>(0))
            .unwrap(),
        0,
        "TEMP pager counts cannot observe the transient statement spill"
    );
    assert_eq!(blocks(&connection), original);
    assert_eq!(spill_bytes().unwrap(), 0);
    peak.store(0, Ordering::Relaxed);
    {
        let tx = connection.transaction().unwrap();
        shift_rowid_keys(&tx, "row_blocks", "first_document", &shift, &Active).unwrap();
        tx.commit().unwrap();
    }
    connection.progress_handler(0, None::<fn() -> bool>).unwrap();
    assert!(!unreadable.load(Ordering::Relaxed));
    assert_eq!(
        peak.load(Ordering::Relaxed),
        0,
        "bounded rowid shift must not spill"
    );
    assert_eq!(spill_bytes().unwrap(), 0);
    assert_ne!(blocks(&connection), original);
    integrity(&connection);
}

#[test]
fn carried_row_chunks_check_cancellation_and_preserve_disk_rows() {
    struct StopAfter {
        checks: AtomicUsize,
        deadline: bool,
    }
    impl CodeIndexExecutionControlV1 for StopAfter {
        fn is_cancelled(&self) -> bool {
            !self.deadline && self.checks.fetch_add(1, Ordering::Relaxed) >= 4_100
        }

        fn is_deadline_exceeded(&self) -> bool {
            self.deadline && self.checks.fetch_add(1, Ordering::Relaxed) >= 4_100
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("row-chunks.sqlite");
    let mut connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA temp_store=FILE;
             PRAGMA cache_size=8;
             CREATE TABLE row_chunks(
                 chunk_id BLOB NOT NULL PRIMARY KEY,
                 document_id INTEGER NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE independent(value TEXT NOT NULL);
             INSERT INTO independent VALUES ('live');",
        )
        .unwrap();
    {
        let tx = connection.transaction().unwrap();
        let mut insert = tx
            .prepare("INSERT INTO row_chunks(chunk_id, document_id) VALUES (?1, ?2)")
            .unwrap();
        for document in 1_i64..=12_000 {
            // Canonical chunk digests are BLOB keys; other valid IDs retain
            // TEXT storage. Both must survive the same mixed-key scan.
            let chunk_id = if document % 2 == 0 {
                Value::Text(format!("literal-chunk:{document:05}"))
            } else {
                Value::Blob(document.to_be_bytes().repeat(4))
            };
            insert.execute(params![chunk_id, document]).unwrap();
        }
        drop(insert);
        tx.commit().unwrap();
    }

    let chunks = |db: &Connection| -> Vec<(Value, i64)> {
        let mut query = db
            .prepare("SELECT chunk_id, document_id FROM row_chunks ORDER BY chunk_id")
            .unwrap();
        query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let original = chunks(&connection);
    assert_eq!(original.len(), 12_000);
    let (blob_keys, text_keys): (i64, i64) = connection
        .query_row(
            "SELECT SUM(typeof(chunk_id) = 'blob'), SUM(typeof(chunk_id) = 'text')
             FROM row_chunks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((blob_keys, text_keys), (6_000, 6_000));
    let shift = ShiftV1::new([((8, 14), 4), ((45, 52), 10)]).unwrap();
    // Reference mapping is deliberately independent of ShiftV1::map:
    // two replaced ranges remove 13 old chunks, shifting the intervening
    // documents down by two and the trailing suffix up by one.
    let expected: Vec<_> = original
        .iter()
        .filter_map(|(chunk, document)| {
            let mapped = match *document {
                8..=13 | 45..=51 => None,
                14..=44 => Some(*document - 2),
                52.. => Some(*document + 1),
                _ => Some(*document),
            };
            mapped.map(|mapped| (chunk.clone(), mapped))
        })
        .collect();
    assert_eq!(expected.len(), 11_987);
    assert_ne!(expected, original);

    for deadline in [false, true] {
        let tx = connection.transaction().unwrap();
        let control = StopAfter {
            checks: AtomicUsize::new(0),
            deadline,
        };
        let error = carry_row_chunks(&tx, &shift, &[], &control).unwrap_err();
        assert!(
            matches!(error, CodeLexicalArtifactErrorV1::Interrupted(_)),
            "both cancellation and deadline must interrupt the real remap: {error:?}"
        );
        let partial = chunks(&tx);
        assert_ne!(partial, original, "the interrupted carry did actual writes");
        assert_ne!(partial, expected, "it stopped before completing the remap");
        integrity(&tx);
        tx.rollback().unwrap();
        drop(connection);
        connection = Connection::open(&path).unwrap();
        assert_eq!(
            chunks(&connection),
            original,
            "rollback must restore every chunk"
        );
        integrity(&connection);
    }

    let tx = connection.transaction().unwrap();
    carry_row_chunks(&tx, &shift, &[], &Active).unwrap();
    assert_eq!(chunks(&tx), expected);
    integrity(&tx);
    tx.commit().unwrap();
    drop(connection);
    let reopened = Connection::open(&path).unwrap();
    assert_eq!(
        chunks(&reopened),
        expected,
        "commit must survive a fresh connection"
    );
    assert_eq!(
        reopened
            .query_row("SELECT value FROM independent", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "live"
    );
    integrity(&reopened);
}
