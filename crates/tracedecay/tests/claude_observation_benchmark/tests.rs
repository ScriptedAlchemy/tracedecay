use std::fs;

use super::baseline::ProviderBaseline;
use super::runner::{Fixture, exercise_provider_paths_once};
use super::{RECORDS_PER_REPETITION, baseline, manifest};

#[test]
fn provider_baselines_match_the_checked_in_workload_manifest() {
    manifest::validate();
    let serialized = serde_json::to_value(baseline::catalog()).unwrap();
    let baselines: Vec<ProviderBaseline> =
        serde_json::from_value(serialized["baselines"].clone()).unwrap();
    baseline::validate(&baselines);
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
