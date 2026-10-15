use std::path::Path;

use rusqlite::{Connection, params};
use serde_json::json;
use tracedecay_domain::{ObservationScopeV1, ProjectId};
use tracedecay_runtime_core::config::ProfileRoot;

use crate::admission::test_support::MemoryHostAdmission;
use crate::admission::{HostAdmissionOutcome, HostAdmissionStatus};
use crate::observation::ObservationCancellation;
use crate::runtime::SessionProvider;
use crate::runtime::hosts::codex::CodexDiscoveryHub;
use crate::runtime::hosts::opencode::opencode_data_dir;
use crate::runtime::ingest::project_provider::ProjectProviderRun;
use crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;

use super::project::with_transcript_source_profile;
use super::quarantine_contract::REFUSED_REASON;
use super::quarantine_snapshot_tests::{
    home_with_project, provider_keeps_refused_source_contract, settle,
};

fn message_payload(created: i64) -> String {
    json!({"role": "user", "time": {"created": created}}).to_string()
}

/// One message per `(session, message)` entry, in insertion order, so every
/// message lands in the same 32-record capture window.
fn write_database(database: &Path, project: &Path, messages: &[(&str, &str)]) {
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    let connection = Connection::open(database).unwrap();
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
    for (session, message) in messages {
        connection
            .execute(
                "INSERT OR IGNORE INTO session(id, directory) VALUES (?1, ?2)",
                params![session, project.to_string_lossy()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO message(id, session_id, time_created, data) VALUES (?1, ?2, 1, ?3)",
                params![message, session, message_payload(1)],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO part(id, message_id, session_id, data) VALUES (?1, ?2, ?3, ?4)",
                params![
                    format!("part_{message}"),
                    message,
                    session,
                    json!({"type": "text", "text": "first turn"}).to_string()
                ],
            )
            .unwrap();
    }
}

#[tokio::test]
async fn opencode_keeps_the_refused_source_contract() {
    let (_temp, home, project) = home_with_project();
    let database = opencode_data_dir(&home).join("opencode.db");
    write_database(
        &database,
        &project,
        &[
            ("ses_refused", "msg_ses_refused"),
            ("ses_healthy", "msg_ses_healthy"),
        ],
    );
    settle(&database);

    provider_keeps_refused_source_contract(
        SessionProvider::OpenCode,
        &home,
        &project,
        "ses_refused",
        "ses_healthy",
        || {
            Connection::open(&database)
                .unwrap()
                .execute(
                    "UPDATE message SET data = ?1 WHERE id = 'msg_ses_refused'",
                    params![message_payload(2)],
                )
                .unwrap();
            settle(&database);
        },
    )
    .await;
}

#[tokio::test]
async fn a_refusal_after_a_committed_message_is_quarantined_at_the_refused_message() {
    install_test_shared_jsonl_preparation_authority();
    let (_temp, home, project) = home_with_project();
    let database = opencode_data_dir(&home).join("opencode.db");
    write_database(
        &database,
        &project,
        &[("ses_split", "msg_first"), ("ses_split", "msg_second")],
    );
    settle(&database);
    let hub = CodexDiscoveryHub::default();
    hub.register("project", Some(&home));
    let project_id = ProjectId::new("project-opencode-split-refusal").unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let admission = MemoryHostAdmission::default();
    admission.fail_captures_for_session_from(
        "ses_split",
        1,
        HostAdmissionOutcome {
            status: HostAdmissionStatus::Degraded,
            retryable: false,
            reason_code: Some(REFUSED_REASON),
            recovery: None,
            cause: None,
        },
    );
    let cancellation = ObservationCancellation::default();
    let pass = || {
        let run = ProjectProviderRun {
            project_root: &project,
            project_id: &project_id,
            facade: &admission,
            scope: &scope,
            candidate: SessionProvider::OpenCode,
            max_new_bytes: u64::MAX,
            cancellation: &cancellation,
            codex_discovery: Some((&hub, "project")),
        };
        with_transcript_source_profile(ProfileRoot::under_home(home.clone()), async move {
            run.run().await.outcome
        })
    };
    let reports = |outcome: &super::failure::ProviderRunOutcome| {
        outcome
            .failures
            .iter()
            .map(|failure| (failure.reason_code, failure.retryable))
            .collect::<Vec<_>>()
    };

    let first = pass().await;
    assert_eq!(reports(&first), [(REFUSED_REASON, false)]);
    assert_eq!(
        admission.observations().len(),
        1,
        "the first message commits"
    );
    let attempts = admission.failed_session_capture_attempts();

    let second = pass().await;
    assert_eq!(reports(&second), [], "the refusal is reported once");
    assert_eq!(
        admission.failed_session_capture_attempts(),
        attempts,
        "the refused message is not attempted again"
    );
}
