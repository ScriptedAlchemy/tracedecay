use rusqlite::{Connection, ErrorCode, params};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::{CodeLexicalArtifactErrorV1, ShiftV1, shift_row_chunk_documents, shift_rowid_keys};

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
             CREATE TABLE row_chunks(
                 chunk_id BLOB PRIMARY KEY, document_id INTEGER NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE independent(value TEXT NOT NULL);
             INSERT INTO independent VALUES ('live');",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    {
        let mut block = transaction
            .prepare("INSERT INTO row_blocks(first_document,payload) VALUES(?1,?2)")
            .unwrap();
        let mut chunk = transaction
            .prepare("INSERT INTO row_chunks(chunk_id,document_id) VALUES(?1,?2)")
            .unwrap();
        for n in 1_i64..=16_000 {
            block.execute(params![n, [7_u8; 32].as_slice()]).unwrap();
            chunk.execute(params![n.to_be_bytes().as_slice(), n]).unwrap();
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

fn chunks(c: &Connection) -> Vec<(Vec<u8>, i64)> {
    let mut query = c
        .prepare("SELECT chunk_id,document_id FROM row_chunks ORDER BY chunk_id")
        .unwrap();
    query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn integrity(c: &Connection) {
    for schema in ["main", "temp"] {
        let actual: String = c
            .query_row(&format!("PRAGMA {schema}.integrity_check"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(actual, "ok", "{schema} remains intact");
    }
}

#[test]
fn lexical_row_carry_is_disk_durable_and_collision_safe() {
    let (directory, mut connection) = fixture();
    let original_blocks = blocks(&connection);
    let original_chunks = chunks(&connection);
    assert_eq!(original_blocks.len(), 16_000);
    assert_eq!(original_chunks.len(), 16_000);
    let shifts = [
        ShiftV1::new([((11, 20), 5), ((36, 39), 6)]).unwrap(),
        ShiftV1::new([((11, 20), 12), ((36, 39), 1)]).unwrap(),
    ];
    // The old positive-key update really collides without negative staging.
    {
        let tx = connection.transaction().unwrap();
        let error = tx
            .execute("UPDATE row_blocks SET first_document=first_document+1", [])
            .unwrap_err();
        assert_eq!(error.sqlite_error_code(), Some(ErrorCode::ConstraintViolation));
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
        let expected_chunks: Vec<_> = original_chunks
            .iter()
            .filter_map(|(key, document)| {
                shift
                    .map(u64::try_from(*document).unwrap())
                    .unwrap()
                    .map(|mapped| (key.clone(), i64::try_from(mapped).unwrap()))
            })
            .collect();
        let tx = connection.transaction().unwrap();
        for (table, column) in [("row_blocks", "first_document"), ("row_chunks", "document_id")] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE {}", shift.sql_replaced(column)),
                [],
            )
            .unwrap();
        }
        shift_rowid_keys(&tx, "row_blocks", "first_document", shift, &Active).unwrap();
        shift_row_chunk_documents(&tx, shift, &Active).unwrap();
        assert_eq!(blocks(&tx), expected_blocks);
        assert_eq!(chunks(&tx), expected_chunks);
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
            chunks(&connection),
            if index == 0 {
                original_chunks.clone()
            } else {
                expected_chunks
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
fn interrupted_row_carry_rolls_back_both_key_families() {
    let (directory, mut connection) = fixture();
    let original_blocks = blocks(&connection);
    let original_chunks = chunks(&connection);
    let shift = ShiftV1::new([((1, 1), 1)]).unwrap();
    struct CancelAfter {
        probes: std::sync::atomic::AtomicUsize,
        limit: usize,
    }
    impl CodeIndexExecutionControlV1 for CancelAfter {
        fn is_cancelled(&self) -> bool {
            self.probes.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= self.limit
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }
    for chunk_family in [false, true] {
        let tx = connection.transaction().unwrap();
        let cancel = CancelAfter {
            probes: std::sync::atomic::AtomicUsize::new(0),
            limit: 4_100,
        };
        let error = if chunk_family {
            shift_row_chunk_documents(&tx, &shift, &cancel).unwrap_err()
        } else {
            shift_rowid_keys(&tx, "row_blocks", "first_document", &shift, &cancel).unwrap_err()
        };
        assert!(matches!(error, CodeLexicalArtifactErrorV1::Interrupted(_)));
        if chunk_family {
            assert_ne!(chunks(&tx), original_chunks);
        } else {
            assert_ne!(blocks(&tx), original_blocks);
        }
        integrity(&tx);
        drop(tx);
        drop(connection);
        connection = Connection::open(directory.path().join("carry.sqlite")).unwrap();
        assert_eq!(blocks(&connection), original_blocks);
        assert_eq!(chunks(&connection), original_chunks);
        integrity(&connection);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn lexical_row_carry_avoids_live_unlinked_sqlite_spills() {
    use std::io;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;

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

    const CHILD: &str = "TRACEDECAY_ROW_CARRY_SPILL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("lexical_row_carry_avoids_live_unlinked_sqlite_spills")
            .arg("--test-threads=1")
            .arg("--nocapture")
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("1 passed"), "child test was vacuous: {stdout}");
        return;
    }
    let (_directory, mut connection) = fixture();
    let shift = ShiftV1::new([((1, 1), 1)]).unwrap();
    let original_blocks = blocks(&connection);
    let original_chunks = chunks(&connection);
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
    );
    {
        let tx = connection.transaction().unwrap();
        tx.execute(
            "UPDATE row_blocks SET first_document=-first_document-1 WHERE first_document>0",
            [],
        )
        .unwrap();
        tx.rollback().unwrap();
    }
    assert!(!unreadable.load(Ordering::Relaxed));
    assert!(peak.load(Ordering::Relaxed) > 0, "old bulk update must spill");
    assert_eq!(
        connection.pragma_query_value(Some("temp"), "page_count", |r| r.get::<_, i64>(0)).unwrap(),
        0,
        "TEMP pager counts cannot observe the transient statement spill"
    );
    assert_eq!(blocks(&connection), original_blocks);
    assert_eq!(spill_bytes().unwrap(), 0);
    peak.store(0, Ordering::Relaxed);
    {
        let tx = connection.transaction().unwrap();
        shift_rowid_keys(&tx, "row_blocks", "first_document", &shift, &Active).unwrap();
        shift_row_chunk_documents(&tx, &shift, &Active).unwrap();
        tx.commit().unwrap();
    }
    connection.progress_handler(0, None::<fn() -> bool>);
    assert!(!unreadable.load(Ordering::Relaxed));
    assert_eq!(peak.load(Ordering::Relaxed), 0, "bounded remap must not spill");
    assert_eq!(spill_bytes().unwrap(), 0);
    assert_ne!(blocks(&connection), original_blocks);
    assert_ne!(chunks(&connection), original_chunks);
    integrity(&connection);
}
