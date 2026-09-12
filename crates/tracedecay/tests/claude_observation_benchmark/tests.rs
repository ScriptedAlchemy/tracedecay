use std::fs;

use super::runner::{Fixture, exercise_provider_paths_once};
use super::{RECORDS_PER_REPETITION, baseline};

#[test]
fn provider_baselines_are_versioned_bounded_and_redacted() {
    let catalog = baseline::catalog();
    let serialized = serde_json::to_value(&catalog).unwrap();
    assert_eq!(serialized["schema_version"], 1);
    assert_eq!(
        serialized["catalog_id"],
        "provider-observation-baselines-v1"
    );
    assert_eq!(
        serialized["compatibility"],
        "v1_additive_optional_measurement_field"
    );
    let baselines = serialized["baselines"].as_array().unwrap();
    assert_eq!(baselines.len(), 8);
    assert_eq!(
        baselines
            .iter()
            .map(|baseline| baseline["provider"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "claude", "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
        ]
    );
    for baseline in baselines {
        let checks = baseline["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 10);
        assert_eq!(
            checks.last().and_then(|value| value.as_str()),
            Some("peak_resource")
        );
        assert!(
            checks
                .iter()
                .any(|check| check.as_str() == Some("fairness")),
            "provider baseline must machine-assert fairness"
        );
        assert_eq!(
            baseline["bounds"]["records_per_repetition"],
            baseline::PROVIDER_RECORDS_PER_REPETITION
        );
        assert_eq!(
            baseline["bounds"]["replay_limit"],
            baseline::PROVIDER_RECORDS_PER_REPETITION + 1
        );
        assert_eq!(
            baseline["bounds"]["max_backlog_records"],
            baseline::PROVIDER_RECORDS_PER_REPETITION
        );
        assert_eq!(baseline["bounds"]["fair_rotation_providers"], 8);
        let fixture = &baseline["fixture"];
        assert_eq!(fixture["format"], "checked_in_native_bounded_copy_v1");
        assert!(!fixture["source_paths"].as_array().unwrap().is_empty());
        assert!(
            fixture["redacted_secret"]
                .as_str()
                .unwrap()
                .contains("redacted")
        );
        assert!(
            !fixture["redacted_secret"]
                .as_str()
                .unwrap()
                .contains("benchmark-secret-")
        );
        let measurement = &baseline["measurement"];
        assert_eq!(
            measurement["required_metrics"].as_array().unwrap().len(),
            12
        );
        assert_eq!(measurement["harness_measures_performance"], true);
        assert_eq!(
            measurement["result_schema"],
            "provider-observation-performance-result-v1"
        );
        assert_ne!(
            measurement["harness_path"],
            "pending_provider_observation_ingest"
        );
    }
}

#[tokio::test]
async fn every_provider_executes_a_production_path_and_exact_no_op() {
    assert_eq!(
        exercise_provider_paths_once().await,
        [
            "claude", "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
        ]
    );
}

#[tokio::test]
async fn claude_observation_path_measures_real_payload_bytes() {
    let fixture = Fixture::new(10_001).await;
    let input_bytes = fs::metadata(&fixture.transcript)
        .expect("benchmark transcript metadata")
        .len();
    assert!(input_bytes > 0);
    let source = fixture.source();
    let started = std::time::Instant::now();
    let stats = fixture.ingest(&source).await;
    let wall_time_ns = started.elapsed().as_nanos();
    assert!(wall_time_ns > 0);
    assert_eq!(
        stats.observations_committed as usize,
        RECORDS_PER_REPETITION
    );
    let observations = fixture.replay().await;
    let mut total_payload_bytes = 0_usize;
    for observation in &observations {
        let payload = observation.observation().payload().to_string();
        assert!(
            observation.observation().receipt().payload().is_some(),
            "observation lacks a payload-bound sanitization receipt"
        );
        assert!(
            !payload.contains("benchmark-secret-"),
            "observation payload retained secret canary"
        );
        total_payload_bytes += payload.len();
    }
    assert!(total_payload_bytes > 0);
    assert_eq!(observations.len(), RECORDS_PER_REPETITION);
}

#[tokio::test]
async fn production_fixture_proves_redaction_and_folded_v1_state() {
    let fixture = Fixture::new(10_000).await;
    let input = fs::read(&fixture.transcript).expect("read generated benchmark transcript");
    assert_eq!(
        std::str::from_utf8(&input)
            .expect("benchmark transcript must be UTF-8")
            .lines()
            .count(),
        RECORDS_PER_REPETITION
    );
    let source = fixture.source();
    let stats = fixture.ingest(&source).await;
    assert_eq!(
        stats.observations_committed as usize,
        RECORDS_PER_REPETITION,
        "unexpected production ingest counters for {} input bytes: {stats:?}",
        input.len()
    );
    assert_eq!(stats.projections_completed as usize, RECORDS_PER_REPETITION);
    assert_eq!(stats.transcript.sessions_upserted, 1);
    assert_eq!(
        stats.transcript.messages_upserted as usize,
        RECORDS_PER_REPETITION
    );
    let observations = fixture.replay().await;
    fixture.verify_committed_state(&observations).await;
}
