//! Falsifiable coverage for the read-only refusal census.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::ingest_refusal_read_from_censuses;
use crate::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_contracts::doctor::{IngestRefusalCensusReadV1, IngestRefusalV1};
use tracedecay_store::ObservationCoverageReason;

async fn insert_advance(
    runtime: &RegisteredGlobalDbTestRuntime,
    provider: &str,
    session: &str,
    offset: u64,
    reason: &str,
) {
    let source_json = format!("{{\"provider\":\"{provider}\",\"session_id\":\"{session}\"}}");
    let coverage_json = format!(
        "{{\"generation\":7,\"ordering_domain\":\"file_bytes\",\"range\":{{\"start\":{offset},\"end\":{}}}}}",
        offset + 100
    );
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

fn refusal(provider: &str, session: &str, reason: &str, start: u64) -> IngestRefusalV1 {
    IngestRefusalV1 {
        provider: provider.to_owned(),
        session_id: session.to_owned(),
        reason: reason.to_owned(),
        start,
        end: start + 100,
    }
}

#[tokio::test]
async fn refusal_census_names_each_refused_record_and_skips_expected_dispositions() {
    let temporary = tempfile::TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(temporary.path())
        .await
        .expect("profile runtime");

    insert_advance(
        &runtime,
        "cursor",
        "445777ad",
        364_052,
        ObservationCoverageReason::ObservationIdentityCollision.as_str(),
    )
    .await;
    insert_advance(&runtime, "codex", "codex-a", 0, "admission_refused").await;
    insert_advance(&runtime, "cursor", "445777ad", 10, "unsupported_fact").await;
    insert_advance(&runtime, "cursor", "445777ad", 11, "blank_frame").await;
    insert_advance(&runtime, "codex", "codex-a", 12, "out_of_scope").await;

    let census = runtime
        .profile_database()
        .observation_refusal_census()
        .await;

    assert_eq!(
        census,
        IngestRefusalCensusReadV1::Observed {
            refusals: vec![
                refusal("codex", "codex-a", "admission_refused", 0),
                refusal(
                    "cursor",
                    "445777ad",
                    "observation_identity_collision",
                    364_052
                ),
            ],
        },
        "refusal-shaped reasons are named; expected dispositions are not"
    );
}

#[tokio::test]
async fn refusal_census_is_empty_without_refusals() {
    let temporary = tempfile::TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(temporary.path())
        .await
        .expect("profile runtime");

    insert_advance(&runtime, "cursor", "s", 0, "unsupported_fact").await;

    let census = runtime
        .profile_database()
        .observation_refusal_census()
        .await;

    assert_eq!(
        census,
        IngestRefusalCensusReadV1::Observed {
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

    insert_advance(&runtime, "cursor", "s", 0, short_secret).await;
    insert_advance(&runtime, "cursor", "s", 1, &long_secret).await;

    let census = runtime
        .profile_database()
        .observation_refusal_census()
        .await;

    let serialized = serde_json::to_string(&census).expect("serialize census");
    assert!(!serialized.contains(short_secret));
    assert!(!serialized.contains(&long_secret));
    let IngestRefusalCensusReadV1::Observed { refusals } = census else {
        panic!("available census must remain observed");
    };
    assert_eq!(refusals.len(), 2);
    assert!(refusals.iter().all(|refusal| {
        refusal.provider == "cursor"
            && refusal.reason.starts_with("sha256:")
            && refusal.reason.len() == "sha256:".len() + 64
    }));
}

#[tokio::test]
async fn a_refusal_row_whose_source_no_longer_decodes_makes_the_census_unknown() {
    let temporary = tempfile::TempDir::new().unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::profile(temporary.path())
        .await
        .expect("profile runtime");
    runtime
        .profile_database()
        .writer_connection()
        .expect("writer connection")
        .execute(
            "INSERT INTO source_cursor_advances
                 (source_json, scope_json, coverage_json, reason, receipt_id)
             VALUES ('{\"provider\":\"cursor\"}', '{}', '{}', 'admission_refused', NULL)",
            (),
        )
        .await
        .expect("insert cursor advance");

    assert_eq!(
        runtime
            .profile_database()
            .observation_refusal_census()
            .await,
        IngestRefusalCensusReadV1::Unknown
    );
}

#[test]
fn refusal_censuses_merge_every_store_in_order() {
    let merged = ingest_refusal_read_from_censuses(&[
        IngestRefusalCensusReadV1::Observed {
            refusals: vec![refusal("cursor", "b", "admission_refused", 5)],
        },
        IngestRefusalCensusReadV1::Observed {
            refusals: vec![
                refusal("cursor", "a", "admission_refused", 9),
                refusal("codex", "c", "admission_refused", 1),
            ],
        },
    ]);

    assert_eq!(
        merged,
        IngestRefusalCensusReadV1::Observed {
            refusals: vec![
                refusal("codex", "c", "admission_refused", 1),
                refusal("cursor", "a", "admission_refused", 9),
                refusal("cursor", "b", "admission_refused", 5),
            ],
        }
    );
}

#[test]
fn one_unavailable_refusal_census_makes_the_merged_read_unknown() {
    let merged = ingest_refusal_read_from_censuses(&[
        IngestRefusalCensusReadV1::Observed {
            refusals: vec![refusal("cursor", "a", "admission_refused", 1)],
        },
        IngestRefusalCensusReadV1::Unknown,
    ]);

    assert_eq!(merged, IngestRefusalCensusReadV1::Unknown);
}
