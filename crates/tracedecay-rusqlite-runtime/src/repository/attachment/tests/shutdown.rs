use std::{
    env,
    path::Path,
    process::Command,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;
use tracedecay_store::AdmissionConfigV1;

use super::super::RepositoryPhysicalAttachmentFactory;
#[cfg(unix)]
use super::super::close_lost_file;
use super::{binding, locator};
#[cfg(unix)]
use crate::connection::path_identity_tests::probe_external_connection;

const SNAPSHOT_PATH: &str = "TRACEDECAY_SHUTDOWN_SNAPSHOT_PATH";
const SNAPSHOT_DONE: &str = "tracedecay-shutdown-snapshot-complete";

fn assert_external_snapshot(path: &Path) {
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{module}::external_shutdown_snapshot"),
            "--nocapture",
        ])
        .env(SNAPSHOT_PATH, path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed") && stdout.contains(SNAPSHOT_DONE),
        "external snapshot failed: {stdout} {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn external_shutdown_snapshot() {
    let Some(path) = env::var_os(SNAPSHOT_PATH) else {
        return;
    };
    let connection =
        Connection::open_with_flags(Path::new(&path), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT value FROM sample", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        41
    );
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    println!("{SNAPSHOT_DONE}");
}

#[derive(Clone, Copy)]
enum CloseMode {
    InitializationReaders,
    Attachment,
}

fn exercise_late_reader_owner(mode: CloseMode) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("shutdown.sqlite3");
    let binding = binding();
    let attachment = Arc::new(
        RepositoryPhysicalAttachmentFactory
            .start_reserved_initialization(
                binding.clone(),
                locator(&binding),
                path.clone(),
                AdmissionConfigV1::default(),
                None,
            )
            .unwrap(),
    );
    attachment
        .exact_sql_handle()
        .unwrap()
        .execute_batch(
            "CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(41)".to_owned(),
        )
        .unwrap();
    let pin = attachment
        .lock_state()
        .initialization_file
        .as_ref()
        .unwrap()
        .try_clone()
        .unwrap();
    if matches!(mode, CloseMode::Attachment) {
        attachment.commit_initialization().unwrap();
    }
    let independent_writer = Connection::open(&path).unwrap();
    independent_writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE sample SET value=99")
        .unwrap();
    let (late_tx, late_rx) = mpsc::channel();
    let weak = {
        let state = attachment.lock_state();
        let readers = state.readers.as_ref().unwrap();
        let weak = readers.downgrade();
        let upgrading = weak.clone();
        readers.run_once_before_shutdown_claim(move || {
            late_tx.send(upgrading.upgrade().unwrap()).unwrap();
        });
        weak
    };
    #[cfg(unix)]
    probe_external_connection(&path, "write", true);
    assert_external_snapshot(&path);
    let (finished_tx, finished_rx) = mpsc::channel();
    let closing = Arc::clone(&attachment);
    let join = thread::spawn(move || {
        let result = match mode {
            CloseMode::InitializationReaders => closing.close_initialization_readers(),
            CloseMode::Attachment => closing.drain(),
        };
        finished_tx.send(result).unwrap();
    });

    // The hook upgrades only after the production caller has observed
    // quiescence, forcing the count-check/retirement race without a sleep.
    let late = late_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(
        finished_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(!late.workers_exited());
    assert!(pin.try_clone().is_ok());
    assert!(attachment.lock_state().readers.is_some());

    independent_writer.execute_batch("ROLLBACK").unwrap();
    drop(late);
    assert_eq!(
        finished_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Ok(())
    );
    join.join().unwrap();
    assert!(weak.upgrade().is_none());
    match mode {
        CloseMode::InitializationReaders => {
            drop(attachment.close_pinned_initialization().unwrap());
        }
        CloseMode::Attachment => attachment.close_and_join().unwrap(),
    }
    assert!(pin.try_clone().is_err());
    #[cfg(unix)]
    probe_external_connection(&path, "write", false);
    assert_external_snapshot(&path);
}

#[test]
fn initialization_reader_close_waits_for_a_late_weak_upgrade() {
    exercise_late_reader_owner(CloseMode::InitializationReaders);
}

#[test]
fn attachment_drain_waits_for_a_late_weak_upgrade() {
    exercise_late_reader_owner(CloseMode::Attachment);
}

#[cfg(unix)]
#[test]
fn lost_notice_during_out_of_lock_join_preserves_pins_and_writer_exclusion() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("joining.sqlite3");
    let binding = binding();
    let attachment = Arc::new(
        RepositoryPhysicalAttachmentFactory
            .start_reserved_initialization(
                binding.clone(),
                locator(&binding),
                path.clone(),
                AdmissionConfigV1::default(),
                None,
            )
            .unwrap(),
    );
    attachment
        .exact_sql_handle()
        .unwrap()
        .execute_batch(
            "CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(41)".to_owned(),
        )
        .unwrap();
    attachment.commit_initialization().unwrap();
    let pin = attachment
        .lock_state()
        .retained_file_pin
        .as_ref()
        .unwrap()
        .try_clone()
        .unwrap();
    let independent_writer = Connection::open(&path).unwrap();
    independent_writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE sample SET value=99")
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    attachment.lock_state().before_worker_join = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        // A failed parent assertion drops the sender and also releases this join.
        let _ = release_rx.recv();
    }));
    let closing = Arc::clone(&attachment);
    let join = thread::spawn(move || closing.drain());
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // Deliver the real writer callback at the exact interval where its owner
    // has left the state slots but its SQLite connection has not been joined.
    // Keep the files in place so an independent OS process can probe the locks.
    close_lost_file(&attachment.state);
    assert!(attachment.file_lost());
    assert!(!attachment.lost_file_closed());
    assert!(attachment.close_and_join().is_err());
    assert!(pin.try_clone().is_ok());
    probe_external_connection(&path, "write", true);
    assert_external_snapshot(&path);

    independent_writer.execute_batch("ROLLBACK").unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(join.join().unwrap(), Ok(()));
    attachment.close_and_join().unwrap();
    assert!(attachment.lost_file_closed());
    assert!(pin.try_clone().is_err());
    probe_external_connection(&path, "write", false);
    assert_external_snapshot(&path);
}
