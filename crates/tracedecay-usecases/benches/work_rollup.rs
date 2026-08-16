use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[allow(dead_code, unused_imports)]
#[path = "../tests/observability_runtime_contract.rs"]
mod observability_runtime_contract;

use observability_runtime_contract::work_rollup_harness::{
    READ_TRIPWIRE, SOURCE_COUNT, TRIPWIRE, WORK_ROLLUP_BENCHMARK_ARTIFACT_SCHEMA_VERSION,
    WorkRollupBenchmarkArtifactV1, WorkRollupFixtureV1, WorkRollupFreshStoreMeasurementV1,
    WorkRollupJourneyV1, WorkRollupLatencyV1, WorkRollupMeasurementScopeV1,
    WorkRollupObservationTimestampWindowV1, WorkRollupRateUnavailableReasonV1, WorkRollupRateV1,
    WorkRollupReconciliationScopeV1, WorkRollupReport, WorkRollupSettledWindowMeasurementV1,
    run_settled_work_rollup_case, run_work_rollup_case, work_rollup_resource_deltas,
};
use tracedecay_domain::CoverageStateV1;

const WARMUP_REPETITIONS: usize = 3;
const MEASURED_REPETITIONS: usize = 30;
const SETTLED_WINDOW_COUNT: usize = 3;
const SETTLED_REPETITIONS_PER_WINDOW: usize = 10;
const MIN_RECEIPTS_PER_SECOND: f64 = 256.0;
const ARTIFACT_PATH_ENV: &str = "TRACEDECAY_WORK_ROLLUP_ARTIFACT_PATH";

fn percentile95(samples: &[Duration]) -> Duration {
    let mut samples = samples.to_vec();
    samples.sort_unstable();
    let rank = (samples.len() * 95).div_ceil(100);
    samples[rank.saturating_sub(1)]
}

fn duration_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).expect("benchmark duration fits in u64 microseconds")
}

fn operations_per_second(operation_count: usize, elapsed: Duration) -> WorkRollupRateV1 {
    if elapsed.is_zero() {
        return WorkRollupRateV1::Unavailable {
            reason: WorkRollupRateUnavailableReasonV1::ZeroElapsedClock,
        };
    }
    WorkRollupRateV1::Measured {
        operations_per_second: operation_count as f64 / elapsed.as_secs_f64(),
    }
}

fn measured_rate_value(rate: &WorkRollupRateV1, context: &str) -> f64 {
    match rate {
        WorkRollupRateV1::Measured {
            operations_per_second,
        } => *operations_per_second,
        WorkRollupRateV1::Unavailable { reason } => {
            panic!("{context} rate was unavailable: {reason:?}")
        }
    }
}

fn explicit_artifact_path() -> PathBuf {
    let path = std::env::var_os(ARTIFACT_PATH_ENV)
        .map(PathBuf::from)
        .expect("set TRACEDECAY_WORK_ROLLUP_ARTIFACT_PATH to a JSONL output path");
    assert!(
        !path.as_os_str().is_empty(),
        "TRACEDECAY_WORK_ROLLUP_ARTIFACT_PATH must not be empty"
    );
    path
}

fn write_jsonl_artifact(
    path: &Path,
    artifact: &WorkRollupBenchmarkArtifactV1,
) -> std::io::Result<()> {
    let mut output = File::create(path)?;
    serde_json::to_writer(&mut output, artifact).map_err(std::io::Error::other)?;
    output.write_all(b"\n")?;
    output.sync_data()
}

fn validate_completion(report: &WorkRollupReport) {
    assert_eq!(report.offered_sources, SOURCE_COUNT);
    assert_eq!(report.dropped_sources, 0);
    assert_eq!(report.durable_sources, SOURCE_COUNT);
    assert_eq!(report.fragment_count, 1);
    assert_eq!(report.fragment_coverage, CoverageStateV1::Known);
    assert!(report.fragment_is_application_canonical);
    assert_eq!(report.raw_coverage, CoverageStateV1::Known);
    assert_eq!(report.coverage, CoverageStateV1::Known);
}

fn main() {
    let artifact_path = explicit_artifact_path();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Work rollup benchmark runtime");

    for _ in 0..WARMUP_REPETITIONS {
        validate_completion(&runtime.block_on(run_work_rollup_case()));
    }

    let mut reports = Vec::with_capacity(MEASURED_REPETITIONS);
    for repetition in 1..=MEASURED_REPETITIONS {
        let report = runtime.block_on(run_work_rollup_case());
        validate_completion(&report);
        println!(
            "work_rollup repetition={repetition} setup_ms={:.3} offer_ms={:.3} \
             fragment_ready_ms={:.3} raw_read_ms={:.3} application_read_ms={:.3} total_ms={:.3}",
            report.setup_elapsed.as_secs_f64() * 1_000.0,
            report.offer_elapsed.as_secs_f64() * 1_000.0,
            report.fragment_ready_elapsed.as_secs_f64() * 1_000.0,
            report.raw_read_elapsed.as_secs_f64() * 1_000.0,
            report.application_read_elapsed.as_secs_f64() * 1_000.0,
            report.total_elapsed.as_secs_f64() * 1_000.0,
        );
        assert!(
            report.total_elapsed <= TRIPWIRE,
            "measured repetition {repetition} exceeded the two-second tripwire: {report:#?}"
        );
        reports.push(report);
    }

    let fragment_ready = reports
        .iter()
        .map(|report| report.fragment_ready_elapsed)
        .collect::<Vec<_>>();
    let application_reads = reports
        .iter()
        .map(|report| report.application_read_elapsed)
        .collect::<Vec<_>>();
    let total = reports
        .iter()
        .map(|report| report.total_elapsed)
        .collect::<Vec<_>>();
    let setup = reports
        .iter()
        .map(|report| report.setup_elapsed)
        .collect::<Vec<_>>();
    let journey = reports
        .iter()
        .map(|report| report.total_elapsed.saturating_sub(report.setup_elapsed))
        .collect::<Vec<_>>();
    let journey_elapsed = journey.iter().sum();
    let throughput = operations_per_second(SOURCE_COUNT * reports.len(), journey_elapsed);
    let fragment_ready_p95 = percentile95(&fragment_ready);
    let application_read_p95 = percentile95(&application_reads);
    let setup_p95 = percentile95(&setup);
    let journey_p95 = percentile95(&journey);
    let total_p95 = percentile95(&total);

    assert_eq!(reports.len(), MEASURED_REPETITIONS);
    assert!(fragment_ready_p95 <= TRIPWIRE);
    assert!(measured_rate_value(&throughput, "fresh-store") >= MIN_RECEIPTS_PER_SECOND);
    assert!(application_read_p95 <= READ_TRIPWIRE);

    println!(
        "work_rollup warmups={WARMUP_REPETITIONS} measured={} completed={} \
         receipts_per_rep={SOURCE_COUNT} durable_sources_per_rep={SOURCE_COUNT} \
         fragments_per_rep=1 drops=0 coverage=known \
         end_to_end_throughput_receipts_per_second={:.2} setup_p95_ms={:.3} \
         fragment_ready_p95_ms={:.3} application_read_p95_ms={:.3} journey_p95_ms={:.3} \
         full_repetition_p95_ms={:.3}",
        MEASURED_REPETITIONS,
        reports.len(),
        measured_rate_value(&throughput, "fresh-store"),
        setup_p95.as_secs_f64() * 1_000.0,
        fragment_ready_p95.as_secs_f64() * 1_000.0,
        application_read_p95.as_secs_f64() * 1_000.0,
        journey_p95.as_secs_f64() * 1_000.0,
        total_p95.as_secs_f64() * 1_000.0,
    );

    let settled = runtime.block_on(run_settled_work_rollup_case(
        SETTLED_WINDOW_COUNT,
        SETTLED_REPETITIONS_PER_WINDOW,
    ));
    assert_eq!(settled.control_operations, 1);
    assert_eq!(
        settled.repeated_operations,
        SETTLED_WINDOW_COUNT * SETTLED_REPETITIONS_PER_WINDOW
    );
    assert_eq!(
        settled.repetition_elapsed.len(),
        settled.repeated_operations
    );
    assert_eq!(settled.windows.len(), SETTLED_WINDOW_COUNT);
    assert!(settled.semantic_identity_equal);
    assert!(settled.observation_timestamps_nondecreasing);

    let mut settled_windows = Vec::with_capacity(settled.windows.len());
    for window in &settled.windows {
        let operation_end = window.first_operation + window.operation_count;
        let elapsed = &settled.repetition_elapsed[window.first_operation..operation_end];
        let total_elapsed = elapsed.iter().sum();
        settled_windows.push(WorkRollupSettledWindowMeasurementV1 {
            window_index: window.window_index,
            first_operation: window.first_operation,
            operation_count: window.operation_count,
            latency: WorkRollupLatencyV1 {
                p95_micros: duration_micros(percentile95(elapsed)),
                max_micros: duration_micros(
                    *elapsed
                        .iter()
                        .max()
                        .expect("every settled window has at least one repetition"),
                ),
            },
            throughput: operations_per_second(window.operation_count, total_elapsed),
            semantic_identity_equal: window.semantic_identity_equal,
            observation_timestamps: WorkRollupObservationTimestampWindowV1 {
                control_observed_at_micros: window
                    .observation_timestamps
                    .control_observed_at_micros,
                first_repeated_observed_at_micros: window
                    .observation_timestamps
                    .first_repeated_observed_at_micros,
                last_repeated_observed_at_micros: window
                    .observation_timestamps
                    .last_repeated_observed_at_micros,
                nondecreasing: window.observation_timestamps.nondecreasing,
            },
            resources: work_rollup_resource_deltas(
                &window.resources_before,
                &window.resources_after,
            ),
        });
    }

    let artifact = WorkRollupBenchmarkArtifactV1 {
        schema_version: WORK_ROLLUP_BENCHMARK_ARTIFACT_SCHEMA_VERSION,
        scope: WorkRollupMeasurementScopeV1 {
            journey: WorkRollupJourneyV1::FreshStoreAndSettledRetainedRollupReads,
            reconciliation_measurement: WorkRollupReconciliationScopeV1::OwnedByMemoryPlateauSuite,
        },
        fixture: WorkRollupFixtureV1 {
            offered_sources: SOURCE_COUNT,
            warmup_repetitions: WARMUP_REPETITIONS,
            fresh_measured_repetitions: MEASURED_REPETITIONS,
            settled_window_count: SETTLED_WINDOW_COUNT,
            settled_repetitions_per_window: SETTLED_REPETITIONS_PER_WINDOW,
        },
        fresh_store: WorkRollupFreshStoreMeasurementV1 {
            throughput,
            setup_p95_micros: duration_micros(setup_p95),
            fragment_ready_p95_micros: duration_micros(fragment_ready_p95),
            application_read_p95_micros: duration_micros(application_read_p95),
            journey_p95_micros: duration_micros(journey_p95),
            full_repetition_p95_micros: duration_micros(total_p95),
        },
        settled_windows,
    };
    write_jsonl_artifact(&artifact_path, &artifact).expect("write Work rollup JSONL artifact");
    println!("work_rollup artifact={}", artifact_path.display());
}
