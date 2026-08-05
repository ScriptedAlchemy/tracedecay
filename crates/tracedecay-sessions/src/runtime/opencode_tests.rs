use std::fs;

use rusqlite::Connection;
use serde_json::json;
use tracedecay_domain::ObservationScopeV1;

use crate::admission::{HostAdmission, test_support::MemoryHostAdmission};
use crate::observation::ObservationCancellation;
use crate::runtime::source::TranscriptIngestError;

use super::{OpenCodeSource, capture_opencode_observations};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::TempDir::new().unwrap();
    let project = temp.path().join("project");
    let other = temp.path().join("other");
    let database = temp.path().join("isolated-opencode.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                directory TEXT NOT NULL
             );
             CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                data BLOB NOT NULL
             );
             CREATE INDEX message_session_time_created_id_idx
                ON message(session_id, time_created, id);
             CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                data BLOB NOT NULL
             );
             CREATE INDEX part_message_id_id_idx ON part(message_id, id);",
        )
        .unwrap();
    for (session, directory) in [("ses_project", &project), ("ses_other", &other)] {
        connection
            .execute(
                "INSERT INTO session(id, directory) VALUES (?1, ?2)",
                rusqlite::params![session, directory.to_string_lossy()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO message(id, session_id, time_created, data)
                 VALUES (?1, ?2, 1, ?3)",
                rusqlite::params![
                    format!("msg_{session}"),
                    session,
                    json!({"role": "user", "time": {"created": 1}}).to_string()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO part(id, message_id, session_id, data)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    format!("part_{session}"),
                    format!("msg_{session}"),
                    session,
                    json!({"type": "text", "text": format!("secret-{session}")}).to_string()
                ],
            )
            .unwrap();
    }
    drop(connection);
    (temp, project, database)
}

#[tokio::test]
async fn immutable_database_read_is_scoped_resumable_and_budgeted() {
    let (_temp, project, database) = fixture();
    let source = OpenCodeSource::with_database_for_project(database, project);
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();

    let deferred = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        Some(0),
        &cancellation,
    )
    .await
    .unwrap();
    assert!(deferred.deferred_by_byte_cap);
    assert!(admission.observations().is_empty());

    let resumed = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &cancellation,
    )
    .await
    .unwrap();
    assert_eq!(resumed.stats.messages_upserted, 1);
    assert_eq!(admission.observations().len(), 1);

    let replay = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &cancellation,
    )
    .await
    .unwrap();
    assert_eq!(replay.stats.messages_upserted, 0);
    assert_eq!(admission.observations().len(), 1);
}

#[tokio::test]
async fn steady_state_restart_keeps_high_water_without_per_row_durability_reads() {
    let (_temp, project, database) = fixture();
    let admission = MemoryHostAdmission::default();

    capture_opencode_observations(
        &admission,
        &OpenCodeSource::with_database_for_project(database.clone(), project.clone()),
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    let frontier = admission
        .get_parse_offset(
            &ObservationScopeV1::Profile,
            super::OPENCODE_SQL_FRONTIER_KEY,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        frontier.byte_offset > 0,
        "a completed sweep must retain its durable high-water rowid"
    );
    let coverage = admission
        .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://opencode/v1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        coverage.file_id,
        crate::runtime::source::HostProviderCoverage::Complete as u64
    );
    assert_eq!(coverage.byte_offset, 0);
    let reads_after_first_sweep = admission.session_message_read_count();

    let restarted = capture_opencode_observations(
        &admission,
        &OpenCodeSource::with_database_for_project(database, project),
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    assert_eq!(restarted.stats.messages_upserted, 0);
    assert_eq!(
        admission.session_message_read_count(),
        reads_after_first_sweep,
        "a quiet restart must not repeat per-row durability lookups"
    );
}

#[tokio::test]
async fn wal_part_append_replaces_the_durable_message_with_complete_content() {
    let (_temp, project, database) = fixture();
    let source = OpenCodeSource::with_database_for_project(database.clone(), project);
    let admission = MemoryHostAdmission::default();

    capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    let initial_part_frontier = admission
        .get_parse_offset(
            &ObservationScopeV1::Profile,
            super::OPENCODE_PART_FRONTIER_KEY,
        )
        .await
        .unwrap()
        .unwrap();
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .execute(
            "INSERT INTO part(id, message_id, session_id, data)
             VALUES ('part_late', 'msg_ses_project', 'ses_project', ?1)",
            [json!({"type": "text", "text": "late-part"}).to_string()],
        )
        .unwrap();
    assert!(database.with_extension("db-wal").is_file());

    let updated = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    let updated_part_frontier = admission
        .get_parse_offset(
            &ObservationScopeV1::Profile,
            super::OPENCODE_PART_FRONTIER_KEY,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(updated_part_frontier.byte_offset > initial_part_frontier.byte_offset);
    assert_eq!(updated.scan_non_durable_units, 0);
    assert!(admission.observations().iter().any(|stored| {
        let payload = stored.observation().payload().to_string();
        payload.contains("secret-ses_project") && payload.contains("late-part")
    }));
    assert_eq!(updated.stats.messages_upserted, 1);
    drop(writer);
}

#[tokio::test]
async fn content_generation_sweep_recovers_deleted_and_reused_rowids() {
    let (_temp, project, database) = fixture();
    let source = OpenCodeSource::with_database_for_project(database.clone(), project);
    let admission = MemoryHostAdmission::default();

    capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    let connection = Connection::open(&database).unwrap();
    connection.execute("DELETE FROM part", ()).unwrap();
    connection.execute("DELETE FROM message", ()).unwrap();
    connection
        .execute(
            "INSERT INTO message(id, session_id, time_created, data)
             VALUES ('msg_reused', 'ses_project', 3, ?1)",
            [json!({"role": "assistant", "time": {"created": 3}}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part(id, message_id, session_id, data)
             VALUES ('part_reused', 'msg_reused', 'ses_project', ?1)",
            [json!({"type": "text", "text": "reused-rowid-content"}).to_string()],
        )
        .unwrap();
    let reused_rowid: i64 = connection
        .query_row(
            "SELECT rowid FROM message WHERE id = 'msg_reused'",
            (),
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reused_rowid, 1);
    drop(connection);

    let recovered = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    assert_eq!(recovered.stats.messages_upserted, 1);
    assert!(admission.observations().iter().any(|stored| {
        stored
            .observation()
            .payload()
            .to_string()
            .contains("reused-rowid-content")
    }));
}

#[tokio::test]
async fn profile_scope_is_the_complement_of_registered_project_scope() {
    let (_temp, project, database) = fixture();
    let source = OpenCodeSource::with_database_for_user(database, vec![project]);
    let admission = MemoryHostAdmission::default();

    let outcome = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.stats.messages_upserted, 1);
    let observations = admission.observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].observation().source().session_id().as_str(),
        "ses_other"
    );
}

#[tokio::test]
async fn malformed_suffix_defers_without_hiding_committed_prefix() {
    let (_temp, project, database) = fixture();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO message(id, session_id, time_created, data)
             VALUES ('msg_z_malformed', 'ses_project', 2, '{')",
            (),
        )
        .unwrap();
    drop(connection);
    let source = OpenCodeSource::with_database_for_project(database, project);
    let admission = MemoryHostAdmission::default();

    let outcome = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    assert!(outcome.deferred_by_byte_cap);
    assert_eq!(outcome.scan_non_durable_units, 1);
    assert_eq!(outcome.stats.messages_upserted, 1);
    assert_eq!(admission.observations().len(), 1);
}

#[tokio::test]
async fn oversized_prefix_cannot_exhaust_scan_budget_or_starve_later_record() {
    let (_temp, project, database) = fixture();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE message SET rowid = 10 WHERE id = 'msg_ses_project'",
            (),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message(rowid, id, session_id, time_created, data)
             VALUES (1, 'msg_a_oversized', 'ses_project', 0, ?1)",
            [json!({"role": "user", "time": {"created": 0}}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part(id, message_id, session_id, data)
             VALUES ('part_a_oversized', 'msg_a_oversized', 'ses_project', ?1)",
            [json!({
                "type": "text",
                "text": "x".repeat(super::MAX_NATIVE_JSON_BYTES + 1)
            })
            .to_string()],
        )
        .unwrap();
    drop(connection);
    let source = OpenCodeSource::with_database_for_project(database, project);
    let admission = MemoryHostAdmission::default();
    let byte_cap = 64 * 1024;

    let outcome = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        Some(byte_cap),
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    assert!(outcome.bytes_consumed <= byte_cap);
    assert!(outcome.deferred_by_byte_cap);
    assert!(outcome.scan_input_bound_reached);
    assert_eq!(admission.observations().len(), 1);
}

#[tokio::test]
async fn malformed_prefix_cannot_starve_later_record() {
    let (_temp, project, database) = fixture();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE message SET rowid = 10 WHERE id = 'msg_ses_project'",
            (),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message(rowid, id, session_id, time_created, data)
             VALUES (1, 'msg_a_malformed', 'ses_project', 0, '{')",
            (),
        )
        .unwrap();
    drop(connection);
    let source = OpenCodeSource::with_database_for_project(database, project);
    let admission = MemoryHostAdmission::default();

    let outcome = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    assert!(outcome.deferred_by_byte_cap);
    assert_eq!(outcome.scan_non_durable_units, 1);
    assert_eq!(admission.observations().len(), 1);
}

#[tokio::test]
async fn wrong_typed_sqlite_ids_and_data_are_row_local_non_durable_records() {
    let (_temp, project, database) = fixture();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE message SET rowid = 10 WHERE id = 'msg_ses_project'",
            (),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message(rowid, id, session_id, time_created, data)
             VALUES (1, X'0102', 'ses_project', 0, '{}')",
            (),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message(rowid, id, session_id, time_created, data)
             VALUES (3, 'msg_wrong_data', 'ses_project', 0, 42)",
            (),
        )
        .unwrap();
    drop(connection);
    let admission = MemoryHostAdmission::default();

    let outcome = capture_opencode_observations(
        &admission,
        &OpenCodeSource::with_database_for_project(database, project),
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.scan_non_durable_units, 2);
    assert_eq!(outcome.stats.messages_upserted, 1);
    assert_eq!(admission.observations().len(), 1);
}

#[tokio::test]
async fn cancellation_during_admission_is_typed_and_persists_no_payloads() {
    let (_temp, project, database) = fixture();
    let source = OpenCodeSource::with_database_for_project(database, project);
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    admission.cancel_on_next_cursor_read(cancellation.clone());

    let error = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &cancellation,
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        TranscriptIngestError::Cancelled {
            provider: "opencode"
        }
    ));
    assert!(admission.observations().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn unavailable_database_is_typed_instead_of_empty_success() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::TempDir::new().unwrap();
    let target = temp.path().join("outside.db");
    Connection::open(&target).unwrap();
    let database = temp.path().join("opencode.db");
    symlink(&target, &database).unwrap();
    let source =
        OpenCodeSource::with_database_for_project(database.clone(), temp.path().to_path_buf());

    let error = capture_opencode_observations(
        &MemoryHostAdmission::default(),
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        crate::runtime::source::TranscriptIngestError::ScanIo {
            operation: "stat OpenCode database",
            path,
            ..
        } if path == database
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn linked_database_sidecar_is_rejected_before_snapshotting() {
    use std::os::unix::fs::symlink;

    let (temp, project, database) = fixture();
    let external = temp.path().join("external-wal");
    fs::write(&external, b"foreign database bytes").unwrap();
    let wal = database.with_file_name(format!(
        "{}-wal",
        database.file_name().unwrap().to_string_lossy()
    ));
    symlink(&external, &wal).unwrap();
    let source = OpenCodeSource::with_database_for_project(database, project);

    let error = capture_opencode_observations(
        &MemoryHostAdmission::default(),
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        crate::runtime::source::TranscriptIngestError::ScanIo {
            operation: "stat OpenCode database",
            path,
            ..
        } if path == wal
    ));
}

#[tokio::test]
async fn wal_resident_rows_are_captured_from_one_coherent_generation() {
    let (_temp, project, database) = fixture();
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .execute(
            "INSERT INTO message(id, session_id, time_created, data)
             VALUES ('msg_wal', 'ses_project', 2, ?1)",
            [json!({"role": "assistant", "time": {"created": 2}}).to_string()],
        )
        .unwrap();
    writer
        .execute(
            "INSERT INTO part(id, message_id, session_id, data)
             VALUES ('part_wal', 'msg_wal', 'ses_project', ?1)",
            [json!({"type": "text", "text": "wal-resident"}).to_string()],
        )
        .unwrap();
    assert!(database.with_extension("db-wal").is_file());

    let admission = MemoryHostAdmission::default();
    let outcome = capture_opencode_observations(
        &admission,
        &OpenCodeSource::with_database_for_project(database, project),
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.stats.messages_upserted, 2);
    assert!(admission.observations().iter().any(|stored| {
        stored
            .observation()
            .payload()
            .to_string()
            .contains("wal-resident")
    }));
    drop(writer);
}

#[tokio::test]
async fn durable_sql_frontier_reaches_rows_beyond_a_poisoned_pass_after_restart() {
    let (_temp, project, database) = fixture();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("DELETE FROM part WHERE session_id = 'ses_project'", ())
        .unwrap();
    connection
        .execute("DELETE FROM message WHERE session_id = 'ses_project'", ())
        .unwrap();
    for ordinal in 0..super::MAX_MESSAGES_PER_PASS {
        connection
            .execute(
                "INSERT INTO message(id, session_id, time_created, data)
                 VALUES (?1, 'ses_project', ?2, '{')",
                rusqlite::params![format!("poison-{ordinal:05}"), ordinal as i64],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO message(id, session_id, time_created, data)
             VALUES ('after-poison', 'ses_project', 999999, ?1)",
            [json!({"role": "user", "time": {"created": 999999}}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part(id, message_id, session_id, data)
             VALUES ('after-poison-part', 'after-poison', 'ses_project', ?1)",
            [json!({"type": "text", "text": "fair-restart"}).to_string()],
        )
        .unwrap();
    drop(connection);

    let source = OpenCodeSource::with_database_for_project(database, project);
    let admission = MemoryHostAdmission::default();
    let first = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(first.stats.messages_upserted, 0);
    let frontier = admission
        .get_parse_offset(
            &ObservationScopeV1::Profile,
            super::OPENCODE_SQL_FRONTIER_KEY,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(frontier.byte_offset > 0);

    let resumed = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.stats.messages_upserted, 1);
    assert!(admission.observations().iter().any(|stored| {
        stored
            .observation()
            .payload()
            .to_string()
            .contains("fair-restart")
    }));
}
