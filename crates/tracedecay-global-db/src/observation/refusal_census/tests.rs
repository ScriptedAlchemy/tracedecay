//! Falsifiable coverage for the read-only refusal census.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{ObservationRefusalCensusV1, ObservationRefusalCountV1};
use crate::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_store::ObservationCoverageReason;

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
async fn unknown_reason_strings_stay_visible_as_opaque_fingerprints() {
    let temporary = tempfile::TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(temporary.path())
        .await
        .expect("profile runtime");
    let short_secret = "provider-private-transcript-secret";
    let long_secret = format!("provider-private-transcript-{}", "x".repeat(16 * 1024));

    insert_advance(&runtime, "cursor", 0, short_secret).await;
    insert_advance(&runtime, "cursor", 1, &long_secret).await;

    let census = runtime
        .profile_database()
        .observation_refusal_census()
        .await;

    let serialized = serde_json::to_string(&census).expect("serialize census");
    assert!(!serialized.contains(short_secret));
    assert!(!serialized.contains(&long_secret));
    let ObservationRefusalCensusV1::Observed { refusals } = census else {
        panic!("available census must remain observed");
    };
    assert_eq!(refusals.len(), 2);
    assert!(refusals.iter().all(|refusal| {
        refusal.provider == "cursor"
            && refusal.reason.starts_with("sha256:")
            && refusal.reason.len() == "sha256:".len() + 64
            && refusal.count == 1
    }));
}

#[tokio::test]
async fn refusal_census_labels_identity_collisions_without_a_generic_admission_bucket() {
    let temporary = tempfile::TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(temporary.path())
        .await
        .expect("profile runtime");

    insert_advance(
        &runtime,
        "cursor",
        0,
        ObservationCoverageReason::ObservationIdentityCollision.as_str(),
    )
    .await;

    let census = runtime
        .profile_database()
        .observation_refusal_census()
        .await;

    assert_eq!(
        census,
        ObservationRefusalCensusV1::Observed {
            refusals: vec![ObservationRefusalCountV1 {
                provider: "cursor".to_owned(),
                reason: "observation_identity_collision".to_owned(),
                count: 1,
            }],
        }
    );
}
