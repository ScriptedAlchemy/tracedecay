use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::{Connection, ErrorCode, params};
use tempfile::{TempDir, tempdir};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::super::CARRY_SCAN_ROWS;
use super::{ClonePostingKey, CodeLexicalArtifactErrorV1, ShiftV1, shift_clone_posting_keys};

struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct CancelAfter {
    checks: AtomicUsize,
    limit: usize,
}

impl CodeIndexExecutionControlV1 for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::Relaxed) >= self.limit
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

fn open_fixture(root: &Path) -> Connection {
    let connection = Connection::open(root.join("clones.sqlite")).unwrap();
    connection
        .execute_batch(
            "PRAGMA temp_store = FILE;
             PRAGMA cache_size = 8;
             PRAGMA temp.cache_size = 8;",
        )
        .unwrap();
    connection
}

fn fixture() -> (TempDir, Connection) {
    let root = tempdir().unwrap();
    let mut connection = open_fixture(root.path());
    connection
        .execute_batch(
            "CREATE TABLE clone_exact_postings (
                 class INTEGER NOT NULL,
                 normalization_revision INTEGER NOT NULL,
                 digest BLOB NOT NULL,
                 occurrence_ordinal INTEGER NOT NULL,
                 PRIMARY KEY(class, normalization_revision, digest, occurrence_ordinal)
             ) WITHOUT ROWID;
             CREATE TABLE independent(value TEXT NOT NULL);
             INSERT INTO independent VALUES ('survivor');",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO clone_exact_postings
                 (class, normalization_revision, digest, occurrence_ordinal)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .unwrap();
        // Consecutive ordinals share a key prefix. Expansion really collides
        // without staging, and each family crosses multiple scan windows.
        let digest = [7_u8; 32];
        for ordinal in 1_i64..=16_000 {
            for class in 0_i64..3 {
                insert
                    .execute(params![class, class + 1, digest.as_slice(), ordinal])
                    .unwrap();
            }
        }
    }
    transaction.commit().unwrap();
    (root, connection)
}

fn postings(connection: &Connection) -> Vec<ClonePostingKey> {
    let mut statement = connection
        .prepare(
            "SELECT class, normalization_revision, digest, occurrence_ordinal
             FROM clone_exact_postings
             ORDER BY class, normalization_revision, digest, occurrence_ordinal",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn integrity(connection: &Connection, schema: &str) {
    let result: String = connection
        .query_row(&format!("PRAGMA {schema}.integrity_check"), [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(result, "ok", "{schema} page integrity");
}

fn expected_postings(original: &[ClonePostingKey], shift: &ShiftV1) -> Vec<ClonePostingKey> {
    let mut expected: Vec<_> = original
        .iter()
        .filter_map(|(class, revision, digest, ordinal)| {
            shift
                .map(u64::try_from(*ordinal).unwrap())
                .unwrap()
                .map(|mapped| {
                    (
                        *class,
                        *revision,
                        digest.clone(),
                        i64::try_from(mapped).unwrap(),
                    )
                })
        })
        .collect();
    expected.sort();
    expected
}

#[test]
fn carried_clone_keys_shift_without_retained_file_backed_temp_pages() {
    let (root, mut connection) = fixture();
    let original = postings(&connection);
    assert_eq!(original.len(), 48_000);
    assert_eq!(
        connection
            .pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .pragma_query_value(Some("temp"), "auto_vacuum", |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    for (index, shift) in [
        ShiftV1::new([((11, 20), 5), ((36, 39), 6)]).unwrap(),
        ShiftV1::new([((11, 20), 12), ((36, 39), 1)]).unwrap(),
    ]
    .iter()
    .enumerate()
    {
        let expected = expected_postings(&original, shift);
        assert_eq!(expected.len(), 47_964);
        assert_ne!(expected, original);
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                &format!(
                    "DELETE FROM clone_exact_postings WHERE {}",
                    shift.sql_replaced("occurrence_ordinal")
                ),
                [],
            )
            .unwrap();
        shift_clone_posting_keys(&transaction, shift, &ActiveControl).unwrap();
        assert_eq!(postings(&transaction), expected);
        assert_eq!(
            transaction
                .pragma_query_value(Some("temp"), "page_count", |row| row.get::<_, i64>(0))
                .unwrap(),
            0,
            "the remap must not retain TEMP pager pages"
        );
        integrity(&transaction, "main");
        integrity(&transaction, "temp");
        if index == 0 {
            transaction.rollback().unwrap();
        } else {
            transaction.commit().unwrap();
        }
        drop(connection);
        connection = open_fixture(root.path());
        let committed = if index == 0 { &original } else { &expected };
        assert_eq!(
            &postings(&connection),
            committed,
            "a fresh on-disk connection must see exactly the committed keys"
        );
        let survivor: String = connection
            .query_row("SELECT value FROM independent", [], |row| row.get(0))
            .unwrap();
        assert_eq!(survivor, "survivor");
        integrity(&connection, "main");
    }
    connection
        .execute_batch(
            "CREATE TEMP TABLE old_style_clone_copy AS
                 SELECT class, normalization_revision, digest, occurrence_ordinal
                 FROM clone_exact_postings;
             DROP TABLE temp.old_style_clone_copy;",
        )
        .unwrap();
    let retained_pages: i64 = connection
        .pragma_query_value(Some("temp"), "page_count", |row| row.get(0))
        .unwrap();
    assert!(
        retained_pages > 32,
        "positive TEMP-copy control: {retained_pages}"
    );
    integrity(&connection, "main");
    integrity(&connection, "temp");
}

#[test]
fn clone_remap_stages_colliding_keys_and_rolls_back_interrupted_windows() {
    let (root, mut connection) = fixture();
    let original = postings(&connection);
    let shift = ShiftV1::new([((1, 1), 1)]).unwrap();
    {
        let transaction = connection.transaction().unwrap();
        let error = transaction
            .execute(
                "UPDATE clone_exact_postings SET occurrence_ordinal = occurrence_ordinal + 1",
                [],
            )
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(ErrorCode::ConstraintViolation)
        );
        transaction.rollback().unwrap();
    }
    // Interrupt after at least one complete window in each staging direction.
    for limit in [CARRY_SCAN_ROWS + 2, original.len() + CARRY_SCAN_ROWS + 32] {
        let control = CancelAfter {
            checks: AtomicUsize::new(0),
            limit,
        };
        {
            let transaction = connection.transaction().unwrap();
            let error = shift_clone_posting_keys(&transaction, &shift, &control).unwrap_err();
            assert!(matches!(error, CodeLexicalArtifactErrorV1::Interrupted(_)));
            let negative: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM clone_exact_postings WHERE occurrence_ordinal < 0",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(negative > 0 && negative < 48_000);
            integrity(&transaction, "main");
            // Drop must roll back partially staged/restored keys, not publish them.
        }
        drop(connection);
        connection = open_fixture(root.path());
        assert_eq!(postings(&connection), original);
        integrity(&connection, "main");
    }
    let transaction = connection.transaction().unwrap();
    shift_clone_posting_keys(&transaction, &shift, &ActiveControl).unwrap();
    assert_eq!(postings(&transaction), expected_postings(&original, &shift));
    transaction.commit().unwrap();
}

#[cfg(target_os = "linux")]
mod physical_spill {
    use std::io;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use super::{
        ActiveControl, ShiftV1, expected_postings, fixture, postings, shift_clone_posting_keys,
    };

    fn spill_bytes() -> io::Result<u64> {
        let mut bytes = 0;
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
                bytes += std::fs::metadata(path)?.len();
            }
        }
        Ok(bytes)
    }

    #[test]
    fn clone_posting_remap_has_no_implicit_file_spill() {
        const CHILD: &str = "TRACEDECAY_CLONE_SPILL_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Other unit tests may spill concurrently; only measure our child.
            let output = Command::new(std::env::current_exe().unwrap())
                .arg("clone_posting_remap_has_no_implicit_file_spill")
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
                "child did not execute: {stdout}"
            );
            return;
        }
        let (_root, mut connection) = fixture();
        let original = postings(&connection);
        let shift = ShiftV1::new([((1, 1), 1)]).unwrap();
        let peak = Arc::new(AtomicU64::new(0));
        let unavailable = Arc::new(AtomicBool::new(false));
        let observed_peak = Arc::clone(&peak);
        let failed_probe = Arc::clone(&unavailable);
        connection.progress_handler(
            4096,
            Some(move || match spill_bytes() {
                Ok(bytes) => {
                    observed_peak.fetch_max(bytes, Ordering::Relaxed);
                    false
                }
                Err(_) => {
                    failed_probe.store(true, Ordering::Relaxed);
                    true
                }
            }),
        );
        {
            let transaction = connection.transaction().unwrap();
            // Execute the actual pre-fix, bulk two-phase SQL as a positive
            // control. The production rowid helper is intentionally bounded now.
            transaction
                .execute(
                    "UPDATE clone_exact_postings
                     SET occurrence_ordinal = -occurrence_ordinal - 2
                     WHERE occurrence_ordinal > 0",
                    [],
                )
                .unwrap();
            transaction
                .execute(
                    "UPDATE clone_exact_postings
                     SET occurrence_ordinal = -occurrence_ordinal - 1
                     WHERE occurrence_ordinal < 0",
                    [],
                )
                .unwrap();
            transaction.rollback().unwrap();
        }
        assert!(!unavailable.load(Ordering::Relaxed));
        assert!(
            peak.load(Ordering::Relaxed) > 0,
            "bulk UPDATE must really spill"
        );
        assert_eq!(
            connection
                .pragma_query_value(Some("temp"), "page_count", |row| row.get::<_, i64>(0))
                .unwrap(),
            0,
            "TEMP page_count alone misses the just-freed statement spill"
        );
        assert_eq!(postings(&connection), original);
        assert_eq!(spill_bytes().unwrap(), 0);
        peak.store(0, Ordering::Relaxed);
        {
            let transaction = connection.transaction().unwrap();
            shift_clone_posting_keys(&transaction, &shift, &ActiveControl).unwrap();
            transaction.commit().unwrap();
        }
        connection.progress_handler(0, None::<fn() -> bool>);
        assert!(!unavailable.load(Ordering::Relaxed));
        assert_eq!(peak.load(Ordering::Relaxed), 0, "no implicit remap spill");
        assert_eq!(spill_bytes().unwrap(), 0);
        assert_eq!(postings(&connection), expected_postings(&original, &shift));
    }
}
