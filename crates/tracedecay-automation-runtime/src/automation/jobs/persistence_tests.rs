use std::fs;

use serde_json::{Value, json};
use tracedecay_domain::errors::TraceDecayError;

use super::{ScheduledJobDefinitions, jobs_path, load_jobs, save_jobs};

async fn write_document(root: &std::path::Path, value: Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(&value).expect("fixture document serializes");
    fs::write(jobs_path(root), &bytes).expect("fixture document writes");
    bytes
}

async fn assert_load_refused(root: &std::path::Path, document: Value, context: &str) {
    let original = write_document(root, document).await;
    let error = load_jobs(root)
        .await
        .expect_err("incomplete stored jobs must not become an editable list");
    let TraceDecayError::Config { message } = error else {
        panic!("stored jobs failure must remain a typed configuration error");
    };
    assert!(message.contains(context), "{message}");
    assert!(!message.contains("private-job-payload"), "{message}");
    assert_eq!(
        fs::read(jobs_path(root)).expect("document remains readable"),
        original
    );
}

#[tokio::test]
async fn mixed_rows_refuse_complete_load_and_scheduler_admission() {
    let root = tempfile::tempdir().expect("temporary dashboard root");
    let document = json!({
        "schema_version": 1,
        "jobs": [
            {"id":"job-a", "name":"A", "prompt":"Do A"},
            {"id":"job-b", "name":"B", "private":"private-job-payload"}
        ]
    });
    let original = write_document(root.path(), document).await;

    let error = load_jobs(root.path())
        .await
        .expect_err("one bad row must not be filtered from a rewrite source");
    let TraceDecayError::Config { message } = error else {
        panic!("mixed stored rows must fail as configuration");
    };
    assert!(message.contains("index 1"), "{message}");
    assert!(!message.contains("private-job-payload"), "{message}");
    assert_eq!(
        fs::read(jobs_path(root.path())).expect("document remains readable"),
        original
    );
    assert!(
        ScheduledJobDefinitions::new(root.path())
            .read()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unsupported_and_incomplete_envelopes_are_not_current_empty_files() {
    let root = tempfile::tempdir().expect("temporary dashboard root");
    for (document, context) in [
        (json!({"schema_version": 9, "jobs": []}), "schema_version"),
        (json!({"schema_version": 1}), "jobs"),
        (json!({"jobs": []}), "schema_version"),
        (
            json!({"schema_version": "private-job-payload", "jobs": []}),
            "envelope",
        ),
        (json!({"schema_version": 1, "jobs": {}}), "envelope"),
        (
            json!({
                "schema_version": 1,
                "jobs": [],
                "private-job-payload": "private-job-payload"
            }),
            "envelope",
        ),
    ] {
        assert_load_refused(root.path(), document, context).await;
    }
}

#[tokio::test]
async fn all_invalid_rows_are_not_reported_as_an_empty_configuration() {
    let root = tempfile::tempdir().expect("temporary dashboard root");
    assert_load_refused(
        root.path(),
        json!({
            "schema_version": 1,
            "jobs": [{"name":"missing id and prompt"}]
        }),
        "index 0",
    )
    .await;
}

#[tokio::test]
async fn malformed_delivery_refuses_the_whole_document() {
    let root = tempfile::tempdir().expect("temporary dashboard root");
    assert_load_refused(
        root.path(),
        json!({
            "schema_version": 1,
            "jobs": [{
                "id":"job-a",
                "name":"A",
                "prompt":"Do A",
                "delivery":{"mode":"private-job-payload", "url":"private-job-payload"}
            }]
        }),
        "invalid delivery at index 0",
    )
    .await;
}

#[tokio::test]
async fn valid_job_extension_fields_survive_load_and_save() {
    let root = tempfile::tempdir().expect("temporary dashboard root");
    write_document(
        root.path(),
        json!({
            "schema_version": 1,
            "jobs": [{
                "id":"job-a",
                "name":"A",
                "prompt":"Do A",
                "future_field":{"mode":"preserve", "rank":3}
            }]
        }),
    )
    .await;

    let jobs = load_jobs(root.path())
        .await
        .expect("valid current jobs load");
    assert_eq!(jobs.len(), 1);
    save_jobs(root.path(), &jobs)
        .await
        .expect("valid current jobs save");
    let saved: Value = serde_json::from_slice(
        &fs::read(jobs_path(root.path())).expect("saved document is readable"),
    )
    .expect("saved document remains JSON");
    assert_eq!(
        saved["jobs"][0]["future_field"],
        json!({"mode":"preserve", "rank":3})
    );
}
