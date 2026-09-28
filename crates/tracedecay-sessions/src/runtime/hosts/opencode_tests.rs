use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::json;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, CanonicalObservationFactV1, CanonicalWorkflowEvidenceKindV1,
    ObservationScopeV1,
};

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
                parent_id TEXT,
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
async fn task_spawn_and_child_parent_reach_the_canonical_envelopes() {
    let (_temp, project, database) = fixture();
    let writer = Connection::open(&database).unwrap();
    writer
        .execute(
            "INSERT INTO session(id, parent_id, directory) VALUES ('ses_child', 'ses_project', ?1)",
            [project.to_string_lossy()],
        )
        .unwrap();
    for (message, session, part) in [
        (
            "msg_task",
            "ses_project",
            json!({
                "type": "tool", "tool": "task", "callID": "call_649e87f9",
                "state": {
                    "status": "completed", "input": {"description": "Survey the loom"},
                    "metadata": {"parentSessionId": "ses_project", "sessionId": "ses_child"},
                    "output": "done"
                }
            }),
        ),
        (
            "msg_child",
            "ses_child",
            json!({"type": "text", "text": "Survey the loom"}),
        ),
    ] {
        writer
            .execute(
                "INSERT INTO message(id, session_id, time_created, data) VALUES (?1, ?2, 2, ?3)",
                rusqlite::params![
                    message,
                    session,
                    json!({"role": "assistant", "time": {"created": 2}}).to_string()
                ],
            )
            .unwrap();
        writer
            .execute(
                "INSERT INTO part(id, message_id, session_id, data) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    format!("part_{message}"),
                    message,
                    session,
                    part.to_string()
                ],
            )
            .unwrap();
    }
    drop(writer);
    let admission = MemoryHostAdmission::default();

    capture_opencode_observations(
        &admission,
        &OpenCodeSource::with_database_for_project(database, project),
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    let envelopes: Vec<CanonicalObservationEnvelopeV1> = admission
        .observations()
        .iter()
        .map(|stored| {
            CanonicalObservationEnvelopeV1::deserialize(stored.observation().payload()).unwrap()
        })
        .collect();
    let child = envelopes
        .iter()
        .find(|envelope| envelope.relations().session_id().as_str() == "ses_child")
        .expect("child message captured");
    assert_eq!(
        child
            .relations()
            .parent_session_id()
            .map(|parent| parent.as_str()),
        Some("ses_project")
    );
    let spawn = CanonicalObservationFactV1::Workflow {
        evidence_kind: CanonicalWorkflowEvidenceKindV1::Subagent,
        reference: Some("ses_child".to_owned()),
        content: Some(json!({"tool_use_id": "call_649e87f9", "text": "Survey the loom"})),
    };
    assert!(
        envelopes.iter().any(|envelope| {
            envelope.relations().session_id().as_str() == "ses_project"
                && envelope.facts().contains(&spawn)
        }),
        "the parent's task part records the spawn edge"
    );
    assert!(
        envelopes
            .iter()
            .filter(|envelope| envelope.relations().session_id().as_str() == "ses_project")
            .all(|envelope| envelope.relations().parent_session_id().is_none()),
        "a root session records no parent"
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
    connection.execute_batch("BEGIN").unwrap();
    {
        let mut poison = connection
            .prepare(
                "INSERT INTO message(id, session_id, time_created, data)
                 VALUES (?1, 'ses_project', ?2, '{')",
            )
            .unwrap();
        for ordinal in 0..super::MAX_MESSAGES_PER_PASS {
            poison
                .execute(rusqlite::params![
                    format!("poison-{ordinal:05}"),
                    ordinal as i64
                ])
                .unwrap();
        }
    }
    connection.execute_batch("COMMIT").unwrap();
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
    let mut frontier = admission
        .get_parse_offset(
            &ObservationScopeV1::Profile,
            super::OPENCODE_SQL_FRONTIER_KEY,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(frontier.byte_offset > 0);

    // How many restarts the poison costs is a property of the host, not of the
    // frontier contract. A pass ends at whichever bound trips first, and
    // `HOST_SCAN_WINDOW` trips before the `MAX_MESSAGES_PER_PASS` row budget on
    // any machine that cannot scan the whole poisoned span inside that window,
    // so "one restart clears every poisoned row" only holds on a fast, idle
    // host. What must hold everywhere is that the persisted frontier never
    // rewinds across a restart and that the row past the poison is eventually
    // admitted exactly once. Each restart that makes progress retires at least
    // one `MAX_MESSAGES_PER_PAGE` page, which bounds the poisoned span.
    let restart_bound = super::MAX_MESSAGES_PER_PASS / super::MAX_MESSAGES_PER_PAGE + 1;
    let mut upserted = first.stats.messages_upserted;
    let mut restarts = 0_usize;
    while upserted == 0 {
        restarts += 1;
        assert!(
            restarts <= restart_bound,
            "the poisoned span never yielded the row past it in {restart_bound} restarts; \
             the durable frontier stalled at {}",
            frontier.byte_offset
        );
        let resumed = capture_opencode_observations(
            &admission,
            &source,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();
        upserted += resumed.stats.messages_upserted;
        let resumed_frontier = admission
            .get_parse_offset(
                &ObservationScopeV1::Profile,
                super::OPENCODE_SQL_FRONTIER_KEY,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            resumed_frontier.byte_offset >= frontier.byte_offset,
            "restart {restarts} rewound the durable frontier from {} to {}",
            frontier.byte_offset,
            resumed_frontier.byte_offset
        );
        frontier = resumed_frontier;
    }
    assert_eq!(upserted, 1);
    assert!(admission.observations().iter().any(|stored| {
        stored
            .observation()
            .payload()
            .to_string()
            .contains("fair-restart")
    }));
}

#[tokio::test]
async fn oversized_opencode_database_admits_its_project_session() {
    let (_temp, project, database) = fixture();
    let source = OpenCodeSource::with_database_for_project(database.clone(), project.clone());
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

    let writer = Connection::open(&database).unwrap();
    writer
        .execute(
            "INSERT INTO session(id, directory) VALUES ('ses_large', ?1)",
            [project.to_string_lossy()],
        )
        .unwrap();
    writer
        .execute(
            "INSERT INTO message(id, session_id, time_created, data)
             VALUES ('msg_large', 'ses_large', 4, ?1)",
            [json!({"role": "user", "time": {"created": 4}}).to_string()],
        )
        .unwrap();
    writer
        .execute(
            "INSERT INTO part(id, message_id, session_id, data)
             VALUES ('part_large', 'msg_large', 'ses_large', ?1)",
            [json!({"type": "text", "text": "oversized-db-visible"}).to_string()],
        )
        .unwrap();
    drop(writer);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&database)
        .unwrap();
    file.set_len(560 * 1024 * 1024).unwrap();
    drop(file);
    assert!(std::fs::metadata(&database).unwrap().len() > 512 * 1024 * 1024);

    let outcome = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    let coverage = admission
        .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://opencode/v1")
        .await
        .unwrap()
        .unwrap();
    let admitted = admission.observations().iter().any(|stored| {
        stored.observation().source().session_id().as_str() == "ses_large"
            && stored
                .observation()
                .payload()
                .to_string()
                .contains("oversized-db-visible")
    });
    assert!(
        admitted,
        "oversized OpenCode database must admit ses_large; admitted={admitted} coverage_file_id={} deferred_units={} messages_upserted={}",
        coverage.file_id, coverage.byte_offset, outcome.stats.messages_upserted
    );
    assert_eq!(
        crate::runtime::source::HostProviderCoverage::from_file_id(coverage.file_id),
        Some(crate::runtime::source::HostProviderCoverage::Complete)
    );
    assert_eq!(coverage.byte_offset, 0);
}

#[tokio::test]
async fn missing_opencode_database_names_its_coverage_reason() {
    let temp = tempfile::TempDir::new().unwrap();
    let source = OpenCodeSource::with_database_for_project(
        temp.path().join("missing-opencode.db"),
        temp.path().join("project"),
    );
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

    let coverage = admission
        .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://opencode/v1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        crate::runtime::source::HostProviderCoverage::from_file_id(coverage.file_id),
        Some(crate::runtime::source::HostProviderCoverage::Unavailable)
    );
    assert_eq!(
        crate::runtime::source::HostProviderCoverage::coverage_reason_name(coverage.file_id),
        Some("database_missing")
    );
}

#[tokio::test]
async fn wal_reuse_updates_existing_parts_without_changing_file_headers() {
    let (_temp, project, database) = fixture();
    let source = OpenCodeSource::with_database_for_project(database.clone(), project);
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    for index in 0..3 {
        writer
            .execute(
                "INSERT INTO message(id, session_id, time_created, data)
             VALUES (?1, 'ses_project', 2, ?2)",
                rusqlite::params![
                    format!("unchanged-{index}"),
                    json!({"role": "user"}).to_string()
                ],
            )
            .unwrap();
        writer
            .execute(
                "INSERT INTO part(id, message_id, session_id, data)
             VALUES (?1, ?1, 'ses_project', ?2)",
                rusqlite::params![
                    format!("unchanged-{index}"),
                    json!({"type": "text", "text": format!("unchanged sibling {index}")})
                        .to_string()
                ],
            )
            .unwrap();
    }
    let update = |text: &str| {
        writer
            .execute(
                "UPDATE part SET data = ?1 WHERE id = 'part_ses_project'",
                [json!({"type": "text", "text": text}).to_string()],
            )
            .unwrap();
    };
    for index in 0..32 {
        update(&format!("warm-{index}"));
    }
    writer
        .execute_batch("PRAGMA wal_checkpoint(RESTART)")
        .unwrap();
    update("first-reused-wal-content");
    let header = fs::read(&database).unwrap()[..100].to_vec();
    let wal = database.with_extension("db-wal");
    let wal_header = fs::read(&wal).unwrap()[..32].to_vec();
    let wal_len = fs::metadata(&wal).unwrap().len();
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
    assert_eq!(first.stats.messages_upserted, 4);
    update("second-reused-wal-content");
    assert_eq!(header, fs::read(&database).unwrap()[..100]);
    assert_eq!(wal_header, fs::read(&wal).unwrap()[..32]);
    assert_eq!(wal_len, fs::metadata(&wal).unwrap().len());
    let updated = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(updated.stats.messages_upserted, 1);
    assert!(admission.observations().iter().any(|stored| {
        stored
            .observation()
            .payload()
            .to_string()
            .contains("second-reused-wal-content")
    }));
    let calls_before = admission.capture_call_counts();
    let unchanged = capture_opencode_observations(
        &admission,
        &source,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(unchanged.stats.messages_upserted, 0);
    let calls_after = admission.capture_call_counts();
    assert_eq!(calls_after.0 - calls_before.0, 0, "no scalar fallback");
    assert!(
        calls_after.1 - calls_before.1 <= 1,
        "unchanged siblings share one admission window"
    );
}

#[tokio::test]
async fn retained_read_snapshot_keeps_reference_scope_and_payload_together() {
    use super::{
        OpenCodePageCursor, OpenCodeScanKind, OpenCodeScanSource, OpenCodeSourceScope,
        materialize_reference_page, scan_reference_page,
    };
    use crate::runtime::host_scan::{HOST_SCAN_WINDOW, HostScanBudget};
    use crate::runtime::hosts::opencode_snapshot::{OpenedOpenCodeDatabase, open_database};
    let (_temp, project, database) = fixture();
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    let budget = HostScanBudget::new(
        1024 * 1024,
        100,
        std::time::Instant::now() + HOST_SCAN_WINDOW,
        ObservationCancellation::default(),
    );
    let (opened, budget) = open_database(database.clone(), budget).await.unwrap();
    let OpenedOpenCodeDatabase::Ready(reader) = opened else {
        panic!("read snapshot refused")
    };
    let source = OpenCodeScanSource {
        source_path: database,
        scope: OpenCodeSourceScope::Project(project),
    };
    let (references, budget) = scan_reference_page(
        reader.reader.connection(),
        &source,
        OpenCodeScanKind::Messages,
        OpenCodePageCursor::default(),
        budget,
    )
    .unwrap();
    assert_eq!(references.references.len(), 1);
    // Reuse the selected rowid for another project's payload between the two reads.
    writer
        .execute_batch(
            "BEGIN;
        DELETE FROM part WHERE session_id = 'ses_project';
        DELETE FROM message WHERE session_id = 'ses_project';
        INSERT INTO message(rowid, id, session_id, time_created, data)
        VALUES (1, 'foreign-replacement', 'ses_other', 2, '{\"role\":\"user\"}');
        INSERT INTO part(id, message_id, session_id, data)
        VALUES ('foreign-part', 'foreign-replacement', 'ses_other',
        '{\"type\":\"text\",\"text\":\"foreign-secret\"}');
        COMMIT;",
        )
        .unwrap();
    let (page, _) = materialize_reference_page(
        reader.reader.connection(),
        &source,
        references.references,
        budget,
    )
    .unwrap();
    assert_eq!(page.records.len(), 1);
    let payload = String::from_utf8(page.records[0].payload.clone()).unwrap();
    assert!(payload.contains("secret-ses_project"));
    assert!(!payload.contains("foreign-secret"));
    assert_eq!(page.records[0].session_id, "ses_project");
}

#[tokio::test]
async fn page_snapshot_refuses_a_replaced_database_identity() {
    use super::{
        OpenCodePageCursor, OpenCodeScanKind, OpenCodeScanSource, OpenCodeSourceScope,
        scan_materialized_page,
    };
    use crate::runtime::host_scan::{HOST_SCAN_WINDOW, HostScanBudget};
    use crate::runtime::hosts::opencode_snapshot::{OpenedOpenCodeDatabase, open_database};
    let (_temp, project, database) = fixture();
    let (_other_temp, _, replacement) = fixture();
    let budget = HostScanBudget::new(
        1024 * 1024,
        100,
        std::time::Instant::now() + HOST_SCAN_WINDOW,
        ObservationCancellation::default(),
    );
    let (opened, budget) = open_database(database.clone(), budget).await.unwrap();
    let OpenedOpenCodeDatabase::Ready(opened) = opened else {
        panic!("read snapshot refused")
    };
    let identity = opened.source_file_identity;
    drop(opened);
    fs::remove_file(&database).unwrap();
    fs::rename(replacement, &database).unwrap();
    let source = OpenCodeScanSource {
        source_path: database,
        scope: OpenCodeSourceScope::Project(project),
    };
    assert!(matches!(
        scan_materialized_page(
            &source,
            OpenCodeScanKind::Messages,
            OpenCodePageCursor::default(),
            budget,
            false,
            identity
        ),
        Err(TranscriptIngestError::ScanGenerationChanged { .. })
    ));
}
