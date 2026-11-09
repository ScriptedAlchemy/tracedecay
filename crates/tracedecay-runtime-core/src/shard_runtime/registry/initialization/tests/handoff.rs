//! Admission at the publication event must not inherit any raw SQLite pins.
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, Error, ErrorCode, OpenFlags};
use tempfile::TempDir;
use tracing::{Event, Metadata, Subscriber, span};

use super::{InitializationDirectory, SIDECARS, owned_files, seed, with_suffix};
use crate::logging::set_tracing_capture;

const HANDOFF: &str = "sqlite.initialization.handoff";
const DATABASE: &str = "TRACEDECAY_HANDOFF_DATABASE";
const JOURNAL: &str = "TRACEDECAY_HANDOFF_JOURNAL";
const BLOCKED: &str = "TRACEDECAY_HANDOFF_BLOCKED";
const COMPLETE: &str = "tracedecay-handoff-probe-complete";

// Use a real connection at the completed-publication telemetry boundary. This
// deterministic schedule needs no sleeps or test-only production callbacks.
struct ConnectionAtHandoff {
    path: PathBuf,
    journal: &'static str,
    writer: bool,
    connection: Arc<Mutex<Option<Connection>>>,
}

impl Subscriber for ConnectionAtHandoff {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.name() == HANDOFF
    }

    fn new_span(&self, _attributes: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        if event.metadata().name() != HANDOFF {
            return;
        }
        InitializationDirectory::verify_read_only_handoff(&self.path).unwrap();
        let flags = if self.writer {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let connection = Connection::open_with_flags(&self.path, flags).unwrap();
        connection.busy_timeout(Duration::ZERO).unwrap();
        connection
            .execute_batch(if self.writer {
                "BEGIN IMMEDIATE"
            } else {
                "BEGIN DEFERRED"
            })
            .unwrap();
        assert_eq!(value(&connection), 7);
        if self.writer {
            connection.execute_batch("UPDATE data SET value = 17").unwrap();
        }
        probe(&self.path, self.journal, self.writer, true);
        let mut slot = self.connection.lock().unwrap();
        assert!(slot.is_none(), "handoff must occur exactly once");
        *slot = Some(connection);
    }
}

fn probe(path: &Path, journal: &str, writer: bool, blocked: bool) {
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{module}::external_handoff_probe"),
            "--nocapture",
        ])
        .env(DATABASE, path)
        .env(JOURNAL, if writer { "writer" } else { journal })
        .env(BLOCKED, blocked.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains(COMPLETE),
        "external SQLite {journal} probe did not complete: {output:?}"
    );
}

#[test]
fn external_handoff_probe() {
    let Some(path) = env::var_os(DATABASE) else {
        return;
    };
    let journal = env::var(JOURNAL).unwrap();
    let blocked: bool = env::var(BLOCKED).unwrap().parse().unwrap();
    let writer =
        Connection::open_with_flags(PathBuf::from(path), OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    assert_eq!(value(&writer), 7);
    let result = if journal == "writer" {
        writer.execute_batch("BEGIN IMMEDIATE; ROLLBACK")
    } else if journal == "WAL" {
        writer
            .query_row("PRAGMA journal_mode = DELETE", [], |row| {
                row.get::<_, String>(0)
            })
            .map(|mode| assert_eq!(mode, "delete"))
    } else {
        writer.execute_batch("BEGIN IMMEDIATE; UPDATE data SET value = 99; COMMIT")
    };
    if blocked {
        assert!(
            matches!(
                &result,
                Err(Error::SqliteFailure(error, _)) if error.code == ErrorCode::DatabaseBusy
            ),
            "the admitted connection must retain its OS lock: {result:?}"
        );
    } else {
        result.expect("write after the connection releases its snapshot");
    }
    println!("{COMPLETE}");
}

fn value(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT value FROM data", [], |row| row.get(0))
        .unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Publication {
    Fresh,
    Prepared,
    RecoverPublished,
}

fn check_handoff(publication: Publication, writer: bool) {
    let journals: &[&str] = if writer { &["WAL"] } else { &["DELETE", "WAL"] };
    for &journal in journals {
        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        let mut stage = InitializationDirectory::begin(destination.clone()).unwrap();
        seed(&stage.database(), 7);
        let database = Connection::open(stage.database()).unwrap();
        let mode: String = database
            .query_row(&format!("PRAGMA journal_mode = {journal}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(mode, journal.to_ascii_lowercase());
        drop(database);
        let identity = crate::db::sqlite_generation_identity(&stage.database()).unwrap();
        let files = owned_files(&stage);
        let connection = Arc::new(Mutex::new(None));
        let capture = set_tracing_capture(ConnectionAtHandoff {
            path: destination.clone(),
            journal,
            writer,
            connection: Arc::clone(&connection),
        });
        match publication {
            Publication::Fresh => stage.publish(identity, files).unwrap(),
            Publication::Prepared => {
                // A prepared resume no longer has the creator's descriptors;
                // its reopened verification pins must obey the same handoff.
                drop(stage.prepare(identity, files).unwrap());
                assert!(InitializationDirectory::verify_read_only_handoff(&destination).is_err());
                assert_eq!(stage.publish_prepared(Vec::new()).unwrap(), identity);
                drop(stage);
            }
            Publication::RecoverPublished => {
                let pins = stage.prepare(identity, files).unwrap();
                for suffix in SIDECARS {
                    fs::rename(stage.reservation(suffix), with_suffix(&destination, suffix))
                        .unwrap();
                }
                fs::rename(stage.database(), &destination).unwrap();
                let durable = stage.prepared.as_ref().unwrap().has_durable_generations();
                drop(pins);
                drop(stage);
                let result = InitializationDirectory::recover_published(&destination);
                if !durable {
                    // Do not fabricate generation support for APFS/overlayfs.
                    // Those filesystems must preserve the unresolved receipt.
                    assert!(result.is_err());
                    assert!(connection.lock().unwrap().is_none());
                    assert!(
                        InitializationDirectory::verify_read_only_handoff(&destination).is_err()
                    );
                    let database = Connection::open(&destination).unwrap();
                    assert_eq!(value(&database), 7);
                    continue;
                }
                result.unwrap();
            }
        }
        drop(capture);
        let connection = connection
            .lock()
            .unwrap()
            .take()
            .expect("the publication event must admit a real read snapshot");
        // Old drop ordering loses this connection's process-owned main/SHM locks
        // after announcing the handoff, even though its transaction is live.
        probe(&destination, journal, writer, true);
        assert_eq!(value(&connection), if writer { 17 } else { 7 });
        // Roll back so the child can prove ordinary acquisition after release
        // against the original committed value in both access modes.
        connection.execute_batch("ROLLBACK").unwrap();
        drop(connection);
        probe(&destination, journal, writer, false);
        let reopened = Connection::open(&destination).unwrap();
        reopened.execute_batch("UPDATE data SET value = 23").unwrap();
        drop(reopened);
        let reopened =
            Connection::open_with_flags(&destination, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(value(&reopened), 23);
        let integrity: String = reopened
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }
}

#[test]
fn fresh_publication_handoff_preserves_read_locks() {
    check_handoff(Publication::Fresh, false);
}

#[test]
fn prepared_publication_handoff_preserves_read_locks() {
    check_handoff(Publication::Prepared, false);
}

#[test]
fn published_recovery_preserves_read_locks_or_refuses_unproven_generation() {
    check_handoff(Publication::RecoverPublished, false);
}

#[test]
fn publication_handoff_preserves_independent_wal_writer_lock() {
    for publication in [
        Publication::Fresh,
        Publication::Prepared,
        Publication::RecoverPublished,
    ] {
        check_handoff(publication, true);
    }
}
