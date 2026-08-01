use std::fs;
use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use super::artifact::{
    EvidenceIndex, git_snapshot, is_lower_hex, sha256_file, status_output_is_dirty,
    validate_evidence_directory, validate_git_snapshots, validate_release_profile,
    verify_git_toplevel, workload_identity,
};
use super::metrics::{
    PhaseAggregate, aggregate_samples, parse_clock_ticks_per_second, parse_cpu_identity,
    parse_proc_stat_cpu_ticks, parse_proc_value, ticks_to_ms, validate_no_op_invariants,
};
use super::model::{
    BenchmarkResult, BuildIdentity, Distribution, EvidenceStatus, GitSnapshot, NoOpTotals,
    PROVIDER_COMMIT_SCOPE, PROVIDER_PARSE_SCOPE, PROVIDER_REPLAY_SCOPE, ProviderBenchmarkResult,
    ProviderBenchmarkSuiteResult, ProviderFairnessResult, ProviderPhaseResult,
    ProviderScheduleTurn, RawPhaseSample, RawProviderPhaseSample,
};
use super::runner::{Fixture, exercise_provider_paths_once};
use super::{
    BENCHMARK_COMMAND, HARNESS_SOURCES, MEASURED_REPETITIONS, NATIVE_PROVIDER_FIXTURES,
    PROVIDER_PIPELINE_SCOPE, RECORDS_PER_REPETITION, RESULT_SCHEMA_VERSION, WARMUP_REPETITIONS,
    WORKLOAD_ID, WORKLOAD_MANIFEST,
};
use super::{baseline, manifest};

type CounterMutation = (&'static str, fn(&mut NoOpTotals));

#[test]
fn workload_manifest_matches_executable_contract() {
    manifest::validate();
    let identity = workload_identity();
    assert_eq!(identity.manifest_sha256.len(), 64);
    assert_eq!(identity.harness_sha256.len(), 64);
    assert_eq!(identity.harness_paths.len(), HARNESS_SOURCES.len());
    assert_eq!(identity.native_fixtures_sha256.len(), 64);
    assert_eq!(
        identity.native_fixture_paths.len(),
        NATIVE_PROVIDER_FIXTURES.len()
    );
}

#[test]
fn checked_in_evidence_preserves_providerless_historical_results() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/pr5-observation");
    let acceptance = validate_evidence_directory(&directory, false).unwrap();
    let index: serde_json::Value = serde_json::from_slice(
        &fs::read(directory.join("evidence-index.json")).expect("read evidence index"),
    )
    .expect("parse evidence index");
    assert_eq!(acceptance.as_deref(), index["current_acceptance"].as_str());
    let mut providerless_results = 0;
    for name in index["historical_stale"]
        .as_array()
        .expect("historical evidence list")
    {
        let result: serde_json::Value = serde_json::from_slice(
            &fs::read(
                directory.join(
                    name.as_str()
                        .expect("historical evidence filename must be a string"),
                ),
            )
            .expect("read historical evidence"),
        )
        .expect("parse historical evidence");
        assert_eq!(result["evidence_status"], "historical_stale");
        providerless_results +=
            usize::from(result.get("provider_observation_performance").is_none());
    }
    assert!(
        providerless_results > 0,
        "providerless historical evidence must exercise compatibility path"
    );
}

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

#[test]
fn hook_telemetry_readiness_uses_direct_fixtures_and_is_honest() {
    baseline::validate_hook_telemetry_readiness();
    let readiness = serde_json::to_value(baseline::hook_telemetry_readiness()).unwrap();
    assert_eq!(
        readiness["artifact_kind"],
        "readiness_and_fixture_identity_not_runtime_contract"
    );
    assert_eq!(readiness["canonical_contract"]["schema_version"], 1);
    assert_eq!(
        readiness["canonical_contract"]["metrics"]["hook_wall_time"],
        json!(["hook_wall_time_us", "hook_wall_time_ms"])
    );
    assert_eq!(
        readiness["canonical_contract"]["metrics"]["daemon_rtt"],
        json!(["daemon_rtt_us", "daemon_call_count"])
    );
    assert_eq!(
        readiness["canonical_contract"]["metrics"]["payload_bytes"],
        json!(["payload_bytes", "daemon_ipc_payload_bytes"])
    );
    assert_eq!(
        readiness["canonical_contract"]["latency_semantics"]["host_ipc_rtt"]["role"],
        "true_host_ipc_rtt"
    );
    assert_eq!(
        readiness["unavailable_measurements"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        readiness["unavailable_measurements"][0]["metric"],
        "daemon_processing_duration_distribution"
    );
    assert_eq!(
        readiness["readiness_distributions"]["source_event"],
        "hook_completed"
    );
    assert_eq!(
        readiness["readiness_distributions"]["collection_status"],
        "no_samples"
    );
    assert_eq!(
        readiness["readiness_distributions"]["input_rows_received"],
        0
    );
    assert_eq!(
        readiness["readiness_distributions"]["input_rows_processed"],
        0
    );
    assert_eq!(
        readiness["readiness_distributions"]["input_rows_dropped_at_cap"],
        0
    );
    assert_eq!(readiness["readiness_distributions"]["events_considered"], 0);
    assert!(
        readiness["readiness_distributions"]["hook_wall_time_distribution"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        readiness["readiness_distributions"]["host_ipc_rtt_distribution"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for host in readiness["host_fixture_measurements"].as_array().unwrap() {
        assert!(
            host["fixture_path"]
                .as_str()
                .unwrap()
                .starts_with("tests/fixtures/host_events/")
        );
        assert_eq!(host["fixture_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(
            host["canonical_request_payload_bytes"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }
}

#[test]
fn hook_telemetry_readiness_aggregates_supplied_completed_rows() {
    let rows = [json!({
        "event": "hook_completed",
        "schema_version": 1,
        "host": "cursor",
        "hook_wall_time_us": 11,
        "daemon_rtt_us": 0,
        "daemon_ipc_payload_bytes": 23,
        "timeout": {
            "budget_ms": 750,
            "timed_out": false
        },
        "disposition": {
            "status": "supported",
            "retryable": false,
            "reason_code": null,
            "class": "application"
        }
    })];

    let readiness =
        serde_json::to_value(baseline::hook_telemetry_readiness_from_rows(&rows)).unwrap();
    assert_eq!(
        readiness["readiness_distributions"]["collection_status"],
        "measured"
    );
    assert_eq!(
        readiness["readiness_distributions"]["input_rows_received"],
        1
    );
    assert_eq!(
        readiness["readiness_distributions"]["input_rows_processed"],
        1
    );
    assert_eq!(readiness["readiness_distributions"]["events_considered"], 1);
    assert_eq!(
        readiness["readiness_distributions"]["host_ipc_rtt_distribution"][0]["summary"]["min"],
        0
    );
    assert_eq!(
        readiness["readiness_distributions"]["host_ipc_rtt_distribution"][0]["summary"]["absent_count"],
        0
    );
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

#[test]
fn workload_manifest_rejects_missing_or_unvalidated_contract_fields() {
    let mut missing = serde_json::from_str::<serde_json::Value>(WORKLOAD_MANIFEST).unwrap();
    missing.as_object_mut().unwrap().remove("metrics");
    assert!(!manifest::accepts_value(missing));

    let mut missing_provider_result =
        serde_json::from_str::<serde_json::Value>(WORKLOAD_MANIFEST).unwrap();
    missing_provider_result
        .as_object_mut()
        .unwrap()
        .remove("provider_result");
    assert!(!manifest::accepts_value(missing_provider_result));

    let mut missing_hook_readiness =
        serde_json::from_str::<serde_json::Value>(WORKLOAD_MANIFEST).unwrap();
    missing_hook_readiness
        .as_object_mut()
        .unwrap()
        .remove("hook_telemetry_readiness");
    assert!(!manifest::accepts_value(missing_hook_readiness));

    let mut missing_fairness_check =
        serde_json::from_str::<serde_json::Value>(WORKLOAD_MANIFEST).unwrap();
    missing_fairness_check["provider_baselines"][0]["checks"]
        .as_array_mut()
        .unwrap()
        .retain(|check| check.as_str() != Some("fairness"));
    assert!(!manifest::accepts_value(missing_fairness_check));

    let mut missing_fairness_fields =
        serde_json::from_str::<serde_json::Value>(WORKLOAD_MANIFEST).unwrap();
    missing_fairness_fields["provider_result"]
        .as_object_mut()
        .unwrap()
        .remove("required_fairness_fields");
    assert!(!manifest::accepts_value(missing_fairness_fields));

    let mut missing_fair_rotation_bound =
        serde_json::from_str::<serde_json::Value>(WORKLOAD_MANIFEST).unwrap();
    missing_fair_rotation_bound["provider_baselines"][0]["bounds"]
        .as_object_mut()
        .unwrap()
        .remove("fair_rotation_providers");
    assert!(!manifest::accepts_value(missing_fair_rotation_bound));

    let mut extra = serde_json::from_str::<serde_json::Value>(WORKLOAD_MANIFEST).unwrap();
    extra
        .get_mut("input")
        .and_then(serde_json::Value::as_object_mut)
        .unwrap()
        .insert("unvalidated".to_string(), json!(true));
    assert!(!manifest::accepts_value(extra));
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

#[test]
fn distribution_uses_nearest_rank_and_sample_standard_deviation() {
    let distribution = Distribution::from_samples(&[4, 1, 3, 2]);
    assert_eq!(distribution.repetitions, 4);
    assert_eq!(distribution.min_ns, 1);
    assert_eq!(distribution.p50_ns, 2);
    assert_eq!(distribution.p95_ns, 4);
    assert_eq!(distribution.p99_ns, 4);
    assert_eq!(distribution.max_ns, 4);
    assert!((distribution.mean_ns - 2.5).abs() < f64::EPSILON);
    assert!((distribution.sample_stddev_ns - 1.290_994_448_735_805_6).abs() < f64::EPSILON);

    let singleton = Distribution::from_samples(&[7]);
    assert_eq!(singleton.p50_ns, 7);
    assert!(singleton.sample_stddev_ns.abs() < f64::EPSILON);
}

#[test]
fn phase_aggregation_sums_counters_and_uses_peak_rss_maximum() {
    let samples = [
        sample(0, 3, 5, 7, 11, RECORDS_PER_REPETITION),
        sample(1, 13, 17, 19, 23, RECORDS_PER_REPETITION),
    ];
    assert_eq!(
        aggregate_samples(&samples),
        PhaseAggregate {
            cpu_ticks: 16,
            process_write_bytes: 22,
            database_storage_growth_bytes: 26,
            peak_rss_kib: 23,
        }
    );
}

#[test]
fn proc_parsers_handle_names_spacing_units_and_cpu_architectures() {
    let stat = "77 (worker with ) parenthesis) S 1 2 3 4 5 6 7 8 9 10 11 12 13";
    assert_eq!(parse_proc_stat_cpu_ticks(stat).unwrap(), 23);
    assert!(parse_proc_stat_cpu_ticks("77 malformed").is_err());

    let status = "Name:\tworker\nVmHWM:      2048 kB\n";
    assert_eq!(parse_proc_value(status, "VmHWM:").unwrap(), 2048);
    assert!(parse_proc_value(status, "VmRSS:").is_err());

    let arm = "processor : 0\nHardware : BCM2711\n";
    assert_eq!(parse_cpu_identity(arm).as_deref(), Some("BCM2711"));
    let x86 = "processor : 0\nmodel name : Example x86 CPU\n";
    assert_eq!(parse_cpu_identity(x86).as_deref(), Some("Example x86 CPU"));
}

#[test]
fn porcelain_status_output_detects_clean_and_dirty_worktrees() {
    assert!(!status_output_is_dirty(b""));
    assert!(status_output_is_dirty(b" M tracked.rs\n"));
    assert!(status_output_is_dirty(b"?? untracked\n"));
}

#[test]
fn build_profile_clock_and_executable_attestation_reject_invalid_inputs() {
    assert!(validate_release_profile(false, Some("release")).is_ok());
    assert!(validate_release_profile(true, Some("release")).is_err());
    assert!(validate_release_profile(false, Some("debug")).is_err());
    assert!(validate_release_profile(false, None).is_err());
    assert_eq!(parse_clock_ticks_per_second("100\n").unwrap(), 100);
    assert!(parse_clock_ticks_per_second("0").is_err());
    assert!(parse_clock_ticks_per_second("not-a-number").is_err());

    let temp = TempDir::new().unwrap();
    let executable = temp.path().join("executable");
    fs::write(&executable, b"attested executable bytes").unwrap();
    let (digest, size) = sha256_file(&executable).unwrap();
    assert_eq!(
        digest,
        "fc9c58e82c9294ce7df3b3cf88b65c9641925306084792c47f26ba50f2ee135c"
    );
    assert_eq!(size, 25);
    assert!(is_lower_hex(&digest, 64));
    assert!(!is_lower_hex(&"A".repeat(64), 64));
    assert!(!is_lower_hex(&"a".repeat(63), 64));
}

#[test]
fn git_snapshot_validation_rejects_dirty_or_changed_states() {
    let clean = GitSnapshot {
        commit: "commit-a".to_string(),
        tree: "tree-a".to_string(),
        dirty: false,
    };
    assert!(validate_git_snapshots(&clean, &clean).is_ok());
    let dirty_before = GitSnapshot {
        dirty: true,
        ..clean.clone()
    };
    assert!(validate_git_snapshots(&dirty_before, &clean).is_err());
    let changed_commit = GitSnapshot {
        commit: "commit-b".to_string(),
        ..clean.clone()
    };
    assert!(validate_git_snapshots(&clean, &changed_commit).is_err());
    let changed_tree = GitSnapshot {
        tree: "tree-b".to_string(),
        ..clean.clone()
    };
    assert!(validate_git_snapshots(&clean, &changed_tree).is_err());
    let dirty_after = GitSnapshot {
        dirty: true,
        ..clean.clone()
    };
    assert!(validate_git_snapshots(&clean, &dirty_after).is_err());
}

#[test]
fn git_commands_are_scoped_to_the_manifest_repository() {
    verify_git_toplevel();
    let snapshot = git_snapshot();
    assert!(!snapshot.commit.is_empty());
    assert!(!snapshot.tree.is_empty());
}

#[test]
fn no_op_invariants_reject_every_kind_of_durable_work() {
    let valid = (0..MEASURED_REPETITIONS)
        .map(|repetition| sample(repetition, 0, 0, 0, 1, 0))
        .collect::<Vec<_>>();
    let totals = NoOpTotals::default();
    assert!(validate_no_op_invariants(&valid, 0, &totals).is_ok());
    assert!(validate_no_op_invariants(&valid, 1, &totals).is_err());
    assert!(validate_no_op_invariants(&valid[..valid.len() - 1], 0, &totals).is_err());
    let mut out_of_order = valid.clone();
    out_of_order[1].repetition = 0;
    assert!(validate_no_op_invariants(&out_of_order, 0, &totals).is_err());

    let mut wrote = valid.clone();
    wrote[0].process_write_bytes = 1;
    assert!(validate_no_op_invariants(&wrote, 0, &totals).is_err());
    let mut grew = valid.clone();
    grew[0].database_storage_growth_bytes = 1;
    assert!(validate_no_op_invariants(&grew, 0, &totals).is_err());
    let mut overproduced = valid.clone();
    overproduced[0].replayed_observations += 1;
    assert!(validate_no_op_invariants(&overproduced, 0, &totals).is_err());
    let mutations: [CounterMutation; 13] = [
        ("source_bytes_scanned", |totals| {
            totals.source_bytes_scanned = 1;
        }),
        ("sessions_upserted", |totals| totals.sessions_upserted = 1),
        ("messages_upserted", |totals| totals.messages_upserted = 1),
        ("observations_committed", |totals| {
            totals.observations_committed = 1;
        }),
        ("observation_duplicates", |totals| {
            totals.observation_duplicates = 1;
        }),
        ("cursor_advances", |totals| totals.cursor_advances = 1),
        ("cursor_duplicates", |totals| totals.cursor_duplicates = 1),
        ("records_rejected", |totals| totals.records_rejected = 1),
        ("records_quarantined", |totals| {
            totals.records_quarantined = 1;
        }),
        ("projections_completed", |totals| {
            totals.projections_completed = 1;
        }),
        ("projections_skipped", |totals| {
            totals.projections_skipped = 1;
        }),
        ("projection_duplicates", |totals| {
            totals.projection_duplicates = 1;
        }),
        ("deferred_sources", |totals| totals.deferred_sources = 1),
    ];
    for (field, mutate) in mutations {
        let mut durable_work = totals.clone();
        mutate(&mut durable_work);
        assert!(
            validate_no_op_invariants(&valid, 0, &durable_work).is_err(),
            "nonzero {field} must reject no-op evidence"
        );
    }
}

#[test]
fn strict_evidence_gate_requires_exactly_one_fully_typed_acceptance() {
    let temp = TempDir::new().unwrap();
    let directory = temp.path();
    write_evidence_index(directory, None);
    assert!(validate_evidence_directory(directory, false).is_ok());
    assert!(validate_evidence_directory(directory, true).is_err());

    let result_name = "result-2099-01-01-aaaaaaaa.json";
    write_evidence_index(directory, Some(result_name));
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&synthetic_acceptance_result()).unwrap(),
    )
    .unwrap();
    let current = validate_evidence_directory(directory, true).unwrap();
    assert_eq!(current.as_deref(), Some(result_name));

    fs::write(
        directory.join("result-2099-01-02-bbbbbbbb.json"),
        serde_json::to_vec(&synthetic_acceptance_result()).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());
    fs::remove_file(directory.join("result-2099-01-02-bbbbbbbb.json")).unwrap();

    let mut unknown = serde_json::to_value(synthetic_acceptance_result()).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("unvalidated".to_string(), json!(true));
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&unknown).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());

    let mut unknown_sample = serde_json::to_value(synthetic_acceptance_result()).unwrap();
    unknown_sample["pipeline_raw_samples"][0]
        .as_object_mut()
        .unwrap()
        .insert("unvalidated".to_string(), json!(true));
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&unknown_sample).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());

    let mut malformed_samples = synthetic_acceptance_result();
    malformed_samples.no_op_replay_raw_samples.pop();
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&malformed_samples).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());

    let mut missing_provider_performance = synthetic_acceptance_result();
    missing_provider_performance.provider_observation_performance = None;
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&missing_provider_performance).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());

    let mut missing_hook_readiness = synthetic_acceptance_result();
    missing_hook_readiness.hook_telemetry_readiness = None;
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&missing_hook_readiness).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());

    let mut unbounded_provider_backlog = synthetic_acceptance_result();
    unbounded_provider_backlog
        .provider_observation_performance
        .as_mut()
        .unwrap()
        .providers[0]
        .max_backlog_records += 1;
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&unbounded_provider_backlog).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());

    let mut unfair_provider_order = synthetic_acceptance_result();
    unfair_provider_order
        .provider_observation_performance
        .as_mut()
        .unwrap()
        .fairness
        .turns
        .swap(0, 1);
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&unfair_provider_order).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());

    let mut unattested_source = synthetic_acceptance_result();
    unattested_source.build_identity.source_mode = "mutable_worktree".to_string();
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&unattested_source).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());

    let mut malformed_provider_phase = synthetic_acceptance_result();
    malformed_provider_phase
        .provider_observation_performance
        .as_mut()
        .unwrap()
        .providers[0]
        .parse
        .raw_samples[0]
        .record_count += 1;
    fs::write(
        directory.join(result_name),
        serde_json::to_vec(&malformed_provider_phase).unwrap(),
    )
    .unwrap();
    assert!(validate_evidence_directory(directory, true).is_err());
}

fn write_evidence_index(directory: &Path, current_acceptance: Option<&str>) {
    let index = EvidenceIndex {
        schema_version: 1,
        current_acceptance: current_acceptance.map(str::to_string),
        historical_stale: Vec::new(),
    };
    fs::write(
        directory.join("evidence-index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
}

fn synthetic_acceptance_result() -> BenchmarkResult {
    let pipeline_raw_samples = (0..MEASURED_REPETITIONS)
        .map(|repetition| sample(repetition, 1, 2, 3, 4, RECORDS_PER_REPETITION))
        .collect::<Vec<_>>();
    let no_op_replay_raw_samples = (0..MEASURED_REPETITIONS)
        .map(|repetition| sample(repetition, 0, 0, 0, 4, 0))
        .collect::<Vec<_>>();
    let pipeline_latencies = pipeline_raw_samples
        .iter()
        .map(|sample| sample.latency_ns)
        .collect::<Vec<_>>();
    let no_op_latencies = no_op_replay_raw_samples
        .iter()
        .map(|sample| sample.latency_ns)
        .collect::<Vec<_>>();
    let pipeline = aggregate_samples(&pipeline_raw_samples);
    let no_op = aggregate_samples(&no_op_replay_raw_samples);
    let total_pipeline_ns = pipeline_latencies.iter().sum::<u64>();
    let git_before = GitSnapshot {
        commit: "a".repeat(40),
        tree: "b".repeat(40),
        dirty: false,
    };
    let git_after = GitSnapshot {
        commit: git_before.commit.clone(),
        tree: git_before.tree.clone(),
        dirty: false,
    };
    BenchmarkResult {
        schema_version: RESULT_SCHEMA_VERSION,
        workload_id: WORKLOAD_ID.to_string(),
        evidence_status: EvidenceStatus::Acceptance,
        workload_identity: workload_identity(),
        build_identity: BuildIdentity {
            commit: git_before.commit.clone(),
            tree: git_before.tree.clone(),
            profile: "release".to_string(),
            source_mode: "git_archive_read_only_v1".to_string(),
            source_manifest_sha256: "c".repeat(64),
            source_file_count: 1,
            target_triple: "synthetic-target".to_string(),
            rustc_version: "rustc synthetic".to_string(),
            cargo_version: "cargo synthetic".to_string(),
            rustflags: "normalized-empty".to_string(),
            rustc_wrapper: "environment:none;cargo_config:synthetic".to_string(),
            rustc_workspace_wrapper: "environment:none;cargo_config:synthetic".to_string(),
            cargo_config_identity: "d".repeat(40),
            data_root_basis: "current_executable_parent".to_string(),
            executable_sha256: "a".repeat(64),
            executable_size_bytes: 1,
        },
        git_before,
        git_after,
        command: BENCHMARK_COMMAND.to_string(),
        rustc: "rustc synthetic".to_string(),
        cargo: "cargo synthetic".to_string(),
        kernel: "kernel synthetic".to_string(),
        cpu_identity: "cpu synthetic".to_string(),
        logical_cpu_count: 1,
        memory_total_kib: 1,
        clock_ticks_per_second: 100,
        warmup_repetitions: WARMUP_REPETITIONS,
        measured_repetitions: MEASURED_REPETITIONS,
        records_per_repetition: RECORDS_PER_REPETITION,
        measured_records: MEASURED_REPETITIONS * RECORDS_PER_REPETITION,
        pipeline_raw_samples,
        pipeline_batch_latency: Distribution::from_samples(&pipeline_latencies),
        pipeline_records_per_second: (MEASURED_REPETITIONS * RECORDS_PER_REPETITION) as f64
            * 1_000_000_000.0
            / total_pipeline_ns as f64,
        pipeline_cpu_ticks: pipeline.cpu_ticks,
        pipeline_cpu_ms: ticks_to_ms(pipeline.cpu_ticks, 100),
        pipeline_process_write_bytes: pipeline.process_write_bytes,
        database_storage_growth_bytes: pipeline.database_storage_growth_bytes,
        peak_rss_kib: pipeline.peak_rss_kib.max(no_op.peak_rss_kib),
        no_op_replay_raw_samples,
        no_op_replay_latency: Distribution::from_samples(&no_op_latencies),
        no_op_replay_cpu_ticks: no_op.cpu_ticks,
        no_op_replay_cpu_ms: ticks_to_ms(no_op.cpu_ticks, 100),
        no_op_replay_process_write_bytes: no_op.process_write_bytes,
        no_op_replay_database_storage_growth_bytes: no_op.database_storage_growth_bytes,
        no_op_replay_observation_count_delta: 0,
        no_op_replay_totals: NoOpTotals::default(),
        provider_observation_performance: Some(synthetic_provider_performance(100)),
        hook_telemetry_readiness: Some(baseline::hook_telemetry_readiness()),
    }
}

fn synthetic_provider_performance(clock_ticks_per_second: u64) -> ProviderBenchmarkSuiteResult {
    let providers = baseline::PROVIDERS
        .iter()
        .map(|provider| {
            let pipeline_raw_samples = (0..MEASURED_REPETITIONS)
                .map(|repetition| {
                    sample(
                        repetition,
                        1,
                        2,
                        3,
                        4,
                        baseline::PROVIDER_RECORDS_PER_REPETITION,
                    )
                })
                .collect::<Vec<_>>();
            let no_op_raw_samples = (0..MEASURED_REPETITIONS)
                .map(|repetition| sample(repetition, 0, 0, 0, 4, 0))
                .collect::<Vec<_>>();
            let pipeline_latencies = pipeline_raw_samples
                .iter()
                .map(|sample| sample.latency_ns)
                .collect::<Vec<_>>();
            let no_op_latencies = no_op_raw_samples
                .iter()
                .map(|sample| sample.latency_ns)
                .collect::<Vec<_>>();
            let pipeline = aggregate_samples(&pipeline_raw_samples);
            let no_op = aggregate_samples(&no_op_raw_samples);
            let total_pipeline_ns = pipeline_latencies.iter().sum::<u64>();
            ProviderBenchmarkResult {
                provider: (*provider).to_string(),
                production_path: format!("{provider}_production_observation_pipeline_v1"),
                pipeline_scope: PROVIDER_PIPELINE_SCOPE.to_string(),
                measured_repetitions: MEASURED_REPETITIONS,
                observations_per_repetition: baseline::PROVIDER_RECORDS_PER_REPETITION,
                replay_limit: baseline::PROVIDER_RECORDS_PER_REPETITION + 1,
                max_backlog_records: baseline::PROVIDER_RECORDS_PER_REPETITION,
                parse: synthetic_provider_phase(PROVIDER_PARSE_SCOPE, clock_ticks_per_second),
                commit: synthetic_provider_phase(PROVIDER_COMMIT_SCOPE, clock_ticks_per_second),
                replay: synthetic_provider_phase(PROVIDER_REPLAY_SCOPE, clock_ticks_per_second),
                pipeline_raw_samples,
                pipeline_latency: Distribution::from_samples(&pipeline_latencies),
                pipeline_records_per_second: (MEASURED_REPETITIONS
                    * baseline::PROVIDER_RECORDS_PER_REPETITION)
                    as f64
                    * 1_000_000_000.0
                    / total_pipeline_ns as f64,
                pipeline_cpu_ticks: pipeline.cpu_ticks,
                pipeline_cpu_ms: ticks_to_ms(pipeline.cpu_ticks, clock_ticks_per_second),
                pipeline_process_write_bytes: pipeline.process_write_bytes,
                pipeline_database_storage_growth_bytes: pipeline.database_storage_growth_bytes,
                peak_rss_kib: pipeline.peak_rss_kib.max(no_op.peak_rss_kib),
                no_op_raw_samples,
                no_op_latency: Distribution::from_samples(&no_op_latencies),
                no_op_cpu_ticks: no_op.cpu_ticks,
                no_op_cpu_ms: ticks_to_ms(no_op.cpu_ticks, clock_ticks_per_second),
                no_op_process_write_bytes: no_op.process_write_bytes,
                no_op_database_storage_growth_bytes: no_op.database_storage_growth_bytes,
                no_op_observation_count_delta: 0,
            }
        })
        .collect();
    ProviderBenchmarkSuiteResult {
        schema_version: 1,
        workload_id: WORKLOAD_ID.to_string(),
        fairness: ProviderFairnessResult {
            policy: "round_robin_v1".to_string(),
            rounds: MEASURED_REPETITIONS,
            providers_per_round: baseline::PROVIDERS.len(),
            max_provider_turn_distance: baseline::PROVIDERS.len(),
            turns: (0..MEASURED_REPETITIONS)
                .flat_map(|round| {
                    baseline::PROVIDERS
                        .iter()
                        .enumerate()
                        .map(move |(position, provider)| ProviderScheduleTurn {
                            round,
                            position,
                            provider: (*provider).to_string(),
                        })
                })
                .collect(),
        },
        providers,
    }
}

fn synthetic_provider_phase(scope: &str, clock_ticks_per_second: u64) -> ProviderPhaseResult {
    let raw_samples = (0..MEASURED_REPETITIONS)
        .map(|repetition| RawProviderPhaseSample {
            repetition,
            latency_ns: u64::try_from(repetition).unwrap_or(u64::MAX) + 1,
            cpu_ticks: 1,
            process_write_bytes: 2,
            database_storage_growth_bytes: 3,
            peak_rss_kib: 4,
            record_count: baseline::PROVIDER_RECORDS_PER_REPETITION,
        })
        .collect::<Vec<_>>();
    let latencies = raw_samples
        .iter()
        .map(|sample| sample.latency_ns)
        .collect::<Vec<_>>();
    let cpu_ticks: u64 = raw_samples.iter().map(|sample| sample.cpu_ticks).sum();
    ProviderPhaseResult {
        scope: scope.to_string(),
        latency: Distribution::from_samples(&latencies),
        cpu_ticks,
        cpu_ms: ticks_to_ms(cpu_ticks, clock_ticks_per_second),
        process_write_bytes: raw_samples
            .iter()
            .map(|sample| sample.process_write_bytes)
            .sum(),
        database_storage_growth_bytes: raw_samples
            .iter()
            .map(|sample| sample.database_storage_growth_bytes)
            .sum(),
        peak_rss_kib: raw_samples
            .iter()
            .map(|sample| sample.peak_rss_kib)
            .max()
            .unwrap(),
        raw_samples,
    }
}

fn sample(
    repetition: usize,
    cpu_ticks: u64,
    process_write_bytes: u64,
    database_storage_growth_bytes: u64,
    peak_rss_kib: u64,
    replayed_observations: usize,
) -> RawPhaseSample {
    RawPhaseSample {
        repetition,
        latency_ns: u64::try_from(repetition).unwrap_or(u64::MAX) + 1,
        cpu_ticks,
        process_write_bytes,
        database_storage_growth_bytes,
        peak_rss_kib,
        replayed_observations,
    }
}
