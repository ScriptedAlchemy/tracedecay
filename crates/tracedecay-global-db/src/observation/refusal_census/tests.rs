//! Falsifiable coverage for the read-only refusal census.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{ObservationRefusalCensusV1, ObservationRefusalCountV1};
use crate::tests::harness::RegisteredGlobalDbTestRuntime;

async fn insert_advance(
    runtime: &RegisteredGlobalDbTestRuntime,
    provider: &str,
    offset: u64,
    reason: &str,
) {
    let source_json =
        format!("{{\"provider\":\"{provider}\",\"native_session_id\":\"census-session\"}}");
    let coverage_json = format!("{{\"start\":{offset},\"end\":{}}}", offset + 1);
    runtime
        .profile_database()
        .writer_connection()
        .expect("writer connection")
        .execute(
            "INSERT INTO source_cursor_advances
                 (source_json, scope_json, coverage_json, reason, receipt_id)
             VALUES (?1, '{\"scope\":\"profile\"}', ?2, ?3, NULL)",
            [source_json.as_str(), coverage_json.as_str(), reason],
        )
        .await
        .expect("insert cursor advance");
}

#[tokio::test]
async fn refusal_census_counts_refusals_per_provider_and_reason() {
    let temporary = tempfile::TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(temporary.path())
        .await
        .expect("profile runtime");

    // The live defect shape: cursor and codex records durably refused by
    // deterministic admission, plus expected dispositions that are NOT
    // refusals and must not degrade the census.
    for offset in 0..3 {
        insert_advance(&runtime, "cursor", offset, "admission_refused").await;
    }
    insert_advance(&runtime, "codex", 0, "admission_refused").await;
    insert_advance(&runtime, "cursor", 10, "unsupported_fact").await;
    insert_advance(&runtime, "cursor", 11, "blank_frame").await;
    insert_advance(&runtime, "codex", 12, "out_of_scope").await;

    let census = runtime
        .profile_database()
        .observation_refusal_census()
        .await;

    assert_eq!(
        census,
        ObservationRefusalCensusV1::Observed {
            refusals: vec![
                ObservationRefusalCountV1 {
                    provider: "codex".to_owned(),
                    reason: "admission_refused".to_owned(),
                    count: 1,
                },
                ObservationRefusalCountV1 {
                    provider: "cursor".to_owned(),
                    reason: "admission_refused".to_owned(),
                    count: 3,
                },
            ],
        },
        "refusal-shaped reasons are counted; expected dispositions are not"
    );
}

#[tokio::test]
async fn refusal_census_is_empty_without_refusals() {
    let temporary = tempfile::TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(temporary.path())
        .await
        .expect("profile runtime");

    insert_advance(&runtime, "cursor", 0, "unsupported_fact").await;

    let census = runtime
        .profile_database()
        .observation_refusal_census()
        .await;

    assert_eq!(
        census,
        ObservationRefusalCensusV1::Observed {
            refusals: Vec::new()
        }
    );
}

#[tokio::test]
async fn unknown_reason_strings_stay_visible_in_the_census() {
    let temporary = tempfile::TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(temporary.path())
        .await
        .expect("profile runtime");

    insert_advance(&runtime, "cursor", 0, "future_disposition").await;

    let census = runtime
        .profile_database()
        .observation_refusal_census()
        .await;

    assert_eq!(
        census,
        ObservationRefusalCensusV1::Observed {
            refusals: vec![ObservationRefusalCountV1 {
                provider: "cursor".to_owned(),
                reason: "future_disposition".to_owned(),
                count: 1,
            }],
        },
        "a reason this binary does not recognize must never be classified as benign"
    );
}
