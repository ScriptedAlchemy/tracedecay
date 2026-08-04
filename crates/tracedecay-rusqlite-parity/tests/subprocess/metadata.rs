use std::fs;

use rusqlite::Connection;
use serde_json::json;

use crate::support::{fixture, invoke, request};

#[test]
fn journal_mode_distinguishes_wal_source_header_from_immutable_delete() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("wal-copy.db");
    let connection = Connection::open(&path).expect("create WAL fixture");
    assert_eq!(
        connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row
                .get::<_, String>(0))
            .expect("enable WAL fixture mode"),
        "wal"
    );
    connection
        .execute_batch("CREATE TABLE evidence (value INTEGER); INSERT INTO evidence VALUES (1);")
        .expect("seed WAL fixture");
    connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("checkpoint WAL fixture");
    drop(connection);
    assert!(!path.with_extension("db-wal").exists());
    assert!(!path.with_extension("db-shm").exists());

    let before = fs::read(&path).expect("WAL fixture before helper");
    let response = invoke(&request(&path, json!({ "type": "journal_mode" })));
    assert_eq!(response["status"], "ok");
    assert_eq!(response["output"]["source_header"]["read_version"], 2);
    assert_eq!(response["output"]["source_header"]["write_version"], 2);
    assert_eq!(response["output"]["source_header"]["mode"], "wal");
    assert_eq!(response["output"]["mode"], "delete");
    assert_eq!(response["output"]["immutable_effective_mode"], "delete");
    assert_eq!(
        response["output"]["normalization"],
        "wal_source_immutable_delete"
    );
    assert_eq!(before, fs::read(&path).expect("WAL fixture after helper"));
    assert!(!path.with_extension("db-wal").exists());
    assert!(!path.with_extension("db-shm").exists());
}

#[test]
fn protocol_rejects_short_invalid_and_inconsistent_sqlite_headers() {
    let fixture = fixture();
    let original = fs::read(&fixture.path).expect("read fixture bytes");
    let cases = [
        ("short", vec![0_u8; 19]),
        ("signature", {
            let mut bytes = original.clone();
            bytes[0] = b'X';
            bytes
        }),
        ("inconsistent", {
            let mut bytes = original.clone();
            bytes[18] = 1;
            bytes[19] = 2;
            bytes
        }),
        ("unknown", {
            let mut bytes = original;
            bytes[18] = 3;
            bytes[19] = 3;
            bytes
        }),
    ];
    for (label, bytes) in cases {
        let path = fixture.path.parent().unwrap().join(format!("{label}.db"));
        fs::write(&path, &bytes).expect("write malformed fixture");
        let response = invoke(&request(&path, json!({ "type": "journal_mode" })));
        assert_eq!(response["status"], "error", "case {label}: {response:#}");
        assert_eq!(response["error"]["code"], "invalid_sqlite_header");
        assert_eq!(
            bytes,
            fs::read(&path).expect("malformed fixture after helper")
        );
    }
}

#[test]
fn subprocess_reports_version_options_and_metadata() {
    let fixture = fixture();
    let metadata = invoke(&request(&fixture.path, json!({ "type": "metadata" })));
    assert_eq!(metadata["protocol_version"], 1);
    assert_eq!(metadata["status"], "ok");
    assert!(metadata["output"]["sqlite_version"].as_str().is_some());
    assert!(
        metadata["output"]["compile_options"]
            .as_array()
            .is_some_and(|options| options.iter().any(|option| option == "ENABLE_FTS5"))
    );
}
