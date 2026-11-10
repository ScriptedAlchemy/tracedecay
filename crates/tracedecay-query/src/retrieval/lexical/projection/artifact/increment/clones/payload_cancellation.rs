use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, params};
use tempfile::tempdir;
use tracedecay_code_index::production::{CodeIndexExecutionControlV1, CodeIndexInterruptionV1};

use super::{CodeLexicalArtifactErrorV1, renumber_clone_payloads};

type PayloadRow = (i64, Vec<u8>, Vec<u8>);

struct WriteControl {
    stopped: Arc<AtomicBool>,
    deadline: bool,
}

impl CodeIndexExecutionControlV1 for WriteControl {
    fn is_cancelled(&self) -> bool {
        !self.deadline && self.stopped.load(Ordering::Relaxed)
    }

    fn is_deadline_exceeded(&self) -> bool {
        self.deadline && self.stopped.load(Ordering::Relaxed)
    }
}

fn payload_rows(connection: &Connection) -> Vec<PayloadRow> {
    let mut query = connection
        .prepare(
            "SELECT ordinal, payload_digest, payload
             FROM clone_body_payloads ORDER BY ordinal",
        )
        .unwrap();
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn occurrences(connection: &Connection) -> Vec<(i64, i64)> {
    let mut query = connection
        .prepare("SELECT ordinal, payload_ordinal FROM clone_occurrences ORDER BY ordinal")
        .unwrap();
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn integrity(connection: &Connection) {
    let result: String = connection
        .query_row("PRAGMA main.integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(result, "ok");
}

#[test]
fn payload_completion_honors_interruption_and_reopens_original_rows() {
    // Cover an interrupted first write and an expired final write in each
    // completion phase. The trigger observes actual SQLite mutations, not
    // a particular number of implementation checkpoints or a racing timer.
    for (fresh, stop_at, deadline) in [
        (true, 1_usize, false),
        (true, 4, true),
        (false, 1, false),
        (false, 4, true),
    ] {
        let root = tempdir().unwrap();
        let path = root.path().join("payloads.sqlite");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA temp_store = FILE;
                 CREATE TABLE clone_body_payloads(
                     ordinal INTEGER PRIMARY KEY,
                     payload_digest BLOB NOT NULL,
                     payload BLOB NOT NULL
                 );
                 CREATE TABLE clone_occurrences(
                     ordinal INTEGER PRIMARY KEY,
                     payload_ordinal INTEGER NOT NULL
                 );
                 CREATE TABLE independent(value TEXT NOT NULL);
                 INSERT INTO independent VALUES ('survivor');
                 CREATE TEMP TABLE live_temp(value TEXT NOT NULL);
                 INSERT INTO live_temp VALUES ('live TEMP survivor');",
            )
            .unwrap();
        let parent: BTreeMap<i64, Vec<u8>> = (1_u8..=4)
            .map(|value| (i64::from(value), vec![value; 32]))
            .collect();
        let transaction = connection.transaction().unwrap();
        for value in 1_u8..=4 {
            transaction
                .execute(
                    "INSERT INTO clone_body_payloads VALUES (?1, ?2, ?3)",
                    params![i64::from(value), vec![value; 32], vec![value; 64]],
                )
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO clone_occurrences VALUES (?1, ?2)",
                    params![i64::from(value), 5 - i64::from(value)],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
        let original_payloads = payload_rows(&connection);
        let original_occurrences = occurrences(&connection);
        let successors: BTreeMap<i64, ([u8; 32], Vec<u8>)> = if fresh {
            (1_u8..=4)
                .map(|value| (i64::from(value), ([value + 10; 32], vec![value + 10; 64])))
                .collect()
        } else {
            BTreeMap::new()
        };
        let borrowed: BTreeMap<i64, ([u8; 32], &[u8])> = successors
            .iter()
            .map(|(&ordinal, (digest, payload))| (ordinal, (*digest, payload.as_slice())))
            .collect();
        let expected_payloads: Vec<PayloadRow> = if fresh {
            successors
                .iter()
                .map(|(&ordinal, (digest, payload))| (ordinal, digest.to_vec(), payload.clone()))
                .collect()
        } else {
            original_payloads
                .iter()
                .rev()
                .enumerate()
                .map(|(index, (_, digest, payload))| {
                    (i64::try_from(index).unwrap() + 1, digest.clone(), payload.clone())
                })
                .collect()
        };
        let expected_occurrences: Vec<_> = (1_i64..=4).map(|ordinal| (ordinal, ordinal)).collect();
        let stopped = Arc::new(AtomicBool::new(false));
        let writes = Arc::new(AtomicUsize::new(0));
        let stop_observer = Arc::clone(&stopped);
        let write_observer = Arc::clone(&writes);
        connection
            .create_scalar_function(
                "stop_payload_carry",
                0,
                FunctionFlags::SQLITE_UTF8,
                move |_| {
                    if write_observer.fetch_add(1, Ordering::Relaxed) + 1 == stop_at {
                        stop_observer.store(true, Ordering::Relaxed);
                    }
                    Ok(0_i64)
                },
            )
            .unwrap();
        let control = WriteControl {
            stopped: Arc::clone(&stopped),
            deadline,
        };
        {
            let transaction = connection.transaction().unwrap();
            let event = if fresh {
                "INSERT ON clone_body_payloads"
            } else {
                "UPDATE OF payload_ordinal ON clone_occurrences"
            };
            transaction
                .execute_batch(&format!(
                    "CREATE TEMP TRIGGER interrupt_payload_write AFTER {event}
                     BEGIN SELECT stop_payload_carry(); END;"
                ))
                .unwrap();
            let error = renumber_clone_payloads(&transaction, &parent, &borrowed, &control)
                .unwrap_err();
            if deadline {
                assert!(matches!(
                    error,
                    CodeLexicalArtifactErrorV1::Interrupted(CodeIndexInterruptionV1::DeadlineExceeded)
                ));
            } else {
                assert!(matches!(
                    error,
                    CodeLexicalArtifactErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
                ));
            }
            assert_eq!(writes.load(Ordering::Relaxed), stop_at);
            if fresh {
                assert_eq!(payload_rows(&transaction), expected_payloads[..stop_at].to_vec());
                assert_eq!(occurrences(&transaction), original_occurrences);
            } else {
                assert_eq!(payload_rows(&transaction), expected_payloads);
                let partial: Vec<_> = original_occurrences
                    .iter()
                    .enumerate()
                    .map(|(index, &(ordinal, payload))| {
                        (ordinal, if index < stop_at { ordinal } else { payload })
                    })
                    .collect();
                assert_eq!(occurrences(&transaction), partial);
            }
            integrity(&transaction);
            // Dropping the transaction must undo both staged payload keys
            // and the observed completion writes, including the last write.
        }
        let live_temp: String = connection
            .query_row("SELECT value FROM live_temp", [], |row| row.get(0))
            .unwrap();
        assert_eq!(live_temp, "live TEMP survivor");
        drop(connection);
        connection = Connection::open(&path).unwrap();
        assert_eq!(payload_rows(&connection), original_payloads);
        assert_eq!(occurrences(&connection), original_occurrences);
        integrity(&connection);

        // The interrupted attempt must remain retryable from durable rows.
        stopped.store(false, Ordering::Relaxed);
        let transaction = connection.transaction().unwrap();
        renumber_clone_payloads(&transaction, &parent, &borrowed, &control).unwrap();
        transaction.commit().unwrap();
        drop(connection);
        let reopened = Connection::open(&path).unwrap();
        assert_eq!(payload_rows(&reopened), expected_payloads);
        assert_eq!(occurrences(&reopened), expected_occurrences);
        let survivor: String = reopened
            .query_row("SELECT value FROM independent", [], |row| row.get(0))
            .unwrap();
        assert_eq!(survivor, "survivor");
        integrity(&reopened);
    }
}
