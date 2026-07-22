use std::fs;

use tracedecay_sqlite_parity_protocol::{
    Command, ErrorCode, ErrorPayload, PROTOCOL_VERSION, Request, ResponseOutcome,
};

use crate::{service::handle_request_bytes, snapshot, snapshot::ReadOnlyDriver};

use super::support::{copied_database, fixture, missing_copied_database};

#[test]
fn missing_snapshot_is_rejected_without_creation() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("missing.db");
    let response = handle_request_bytes(
        &serde_json::to_vec(&Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "missing".to_string(),
            database: missing_copied_database(&path),
            command: Command::Metadata,
        })
        .expect("serialize request"),
    );
    assert!(matches!(
        response.outcome,
        ResponseOutcome::Error {
            error: ErrorPayload {
                code: ErrorCode::InvalidPath,
                ..
            }
        }
    ));
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn final_component_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let fixture = fixture();
    let link = fixture.path.parent().unwrap().join("copied-link.db");
    symlink(&fixture.path, &link).expect("create fixture symlink");
    let error = snapshot::validate_copied_path(&link).expect_err("symlink must be rejected");
    assert_eq!(error.code, ErrorCode::InvalidPath);
}

#[test]
fn sealed_snapshot_provenance_is_required_and_revalidated() {
    let fixture = fixture();
    let mut database = copied_database(&fixture.path);
    database.provenance.byte_len += 1;
    let response = handle_request_bytes(
        &serde_json::to_vec(&Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "changed-provenance".to_owned(),
            database,
            command: Command::Metadata,
        })
        .expect("serialize request"),
    );
    assert!(response.verified_snapshot.is_none());
    assert!(matches!(
        response.outcome,
        ResponseOutcome::Error {
            error: ErrorPayload {
                code: ErrorCode::InvalidSnapshotProvenance,
                ..
            }
        }
    ));

    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: "sealed-provenance".to_owned(),
        database: copied_database(&fixture.path),
        command: Command::Metadata,
    };
    let response = handle_request_bytes(&serde_json::to_vec(&request).expect("serialize request"));
    assert_eq!(
        response
            .verified_snapshot
            .as_ref()
            .map(|snapshot| &snapshot.canonical_path),
        Some(&fs::canonicalize(&fixture.path).expect("canonicalize copied fixture"))
    );
    assert!(matches!(response.outcome, ResponseOutcome::Ok { .. }));

    let database = copied_database(&fixture.path);
    let mut bytes = fs::read(&fixture.path).expect("read fixture before same-size mutation");
    *bytes.last_mut().expect("nonempty SQLite fixture") ^= 1;
    fs::write(&fixture.path, bytes).expect("mutate fixture without changing its length");
    let response = handle_request_bytes(
        &serde_json::to_vec(&Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "content-changed".to_owned(),
            database,
            command: Command::Metadata,
        })
        .expect("serialize content-changed request"),
    );
    assert!(matches!(
        response.outcome,
        ResponseOutcome::Error {
            error: ErrorPayload {
                code: ErrorCode::InvalidSnapshotProvenance,
                ..
            }
        }
    ));
}

#[test]
fn connection_is_immutable_query_only_and_rejects_writes() {
    let fixture = fixture();
    let before = fs::read(&fixture.path).expect("fixture before probe");
    let verified = snapshot::verify_copied_snapshot(&copied_database(&fixture.path))
        .expect("verify copied fixture");
    let driver = ReadOnlyDriver::open(&verified).expect("open read-only driver");
    let error = driver
        .connection
        .execute(
            "INSERT INTO metadata(key, value) VALUES ('blocked', 'write')",
            [],
        )
        .expect_err("write must fail");
    let message = error.to_string().to_ascii_lowercase();
    assert!(message.contains("readonly") || message.contains("read-only"));
    drop(driver);
    assert_eq!(
        before,
        fs::read(&fixture.path).expect("fixture after probe")
    );
    assert!(!fixture.path.with_extension("db-wal").exists());
    assert!(!fixture.path.with_extension("db-shm").exists());
}
