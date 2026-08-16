use std::time::Duration;

#[allow(dead_code, unused_imports)]
#[path = "../tests/observability_runtime_contract.rs"]
mod observability_runtime_contract;

use observability_runtime_contract::work_rollup_harness::{
    READ_TRIPWIRE, SOURCE_COUNT, TRIPWIRE, WorkRollupMeasurement, WorkRollupReport,
    WorkRollupResourceSample, run_settled_work_rollup_case, run_work_rollup_case,
};
use tracedecay_domain::CoverageStateV1;

const WARMUP_REPETITIONS: usize = 3;
const MEASURED_REPETITIONS: usize = 30;
const SETTLED_REPETITIONS: usize = 30;
const MIN_RECEIPTS_PER_SECOND: f64 = 256.0;

fn percentile95(samples: &[Duration]) -> Duration {
    let mut samples = samples.to_vec();
    samples.sort_unstable();
    let rank = (samples.len() * 95).div_ceil(100);
    samples[rank.saturating_sub(1)]
}

fn measurement_json(
    measurement: WorkRollupMeasurement,
    unavailable_reason: &str,
) -> serde_json::Value {
    match measurement {
        WorkRollupMeasurement::Measured(value) => serde_json::json!({
            "state": "measured",
            "value": value,
        }),
        WorkRollupMeasurement::Unavailable => serde_json::json!({
            "state": "unavailable",
            "reason": unavailable_reason,
        }),
    }
}

fn resource_delta_json(
    before: WorkRollupMeasurement,
    after: WorkRollupMeasurement,
) -> serde_json::Value {
    match (before, after) {
        (WorkRollupMeasurement::Measured(before), WorkRollupMeasurement::Measured(after)) => {
            serde_json::json!({
                "state": "measured",
                "before": before,
                "after": after,
                "delta": i128::from(after) - i128::from(before),
            })
        }
        _ => serde_json::json!({"state": "unavailable"}),
    }
}

fn resource_deltas_json(
    before: &WorkRollupResourceSample,
    after: &WorkRollupResourceSample,
) -> serde_json::Value {
    serde_json::json!({
        "rss_bytes": resource_delta_json(before.rss_bytes, after.rss_bytes),
        "rss_anon_bytes": resource_delta_json(before.rss_anon_bytes, after.rss_anon_bytes),
        "open_file_descriptors": resource_delta_json(
            before.open_file_descriptors,
            after.open_file_descriptors,
        ),
        "task_count": resource_delta_json(before.task_count, after.task_count),
    })
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
    assert!(report.raw_rollup_equal);
}

fn main() {
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
    let journey_seconds = journey.iter().map(Duration::as_secs_f64).sum::<f64>();
    let throughput = (SOURCE_COUNT * reports.len()) as f64 / journey_seconds;
    let fragment_ready_p95 = percentile95(&fragment_ready);
    let application_read_p95 = percentile95(&application_reads);
    let setup_p95 = percentile95(&setup);
    let journey_p95 = percentile95(&journey);
    let total_p95 = percentile95(&total);

    assert_eq!(reports.len(), MEASURED_REPETITIONS);
    assert!(fragment_ready_p95 <= TRIPWIRE);
    assert!(throughput >= MIN_RECEIPTS_PER_SECOND);
    assert!(application_read_p95 <= READ_TRIPWIRE);

    println!(
        "work_rollup warmups={WARMUP_REPETITIONS} measured={} completed={} \
         receipts_per_rep={SOURCE_COUNT} durable_sources_per_rep={SOURCE_COUNT} \
         fragments_per_rep=1 drops=0 coverage=known raw_rollup_equal=true \
         end_to_end_throughput_receipts_per_second={throughput:.2} setup_p95_ms={:.3} \
         fragment_ready_p95_ms={:.3} application_read_p95_ms={:.3} journey_p95_ms={:.3} \
         full_repetition_p95_ms={:.3}",
        MEASURED_REPETITIONS,
        reports.len(),
        setup_p95.as_secs_f64() * 1_000.0,
        fragment_ready_p95.as_secs_f64() * 1_000.0,
        application_read_p95.as_secs_f64() * 1_000.0,
        journey_p95.as_secs_f64() * 1_000.0,
        total_p95.as_secs_f64() * 1_000.0,
    );

    let settled = runtime.block_on(run_settled_work_rollup_case(SETTLED_REPETITIONS));
    assert_eq!(settled.control_operations, 1);
    assert_eq!(settled.repeated_operations, SETTLED_REPETITIONS);
    assert_eq!(settled.repetition_elapsed.len(), SETTLED_REPETITIONS);
    assert!(settled.semantic_output_identity);

    let settled_p95 = percentile95(&settled.repetition_elapsed);
    let settled_total_seconds = settled
        .repetition_elapsed
        .iter()
        .map(Duration::as_secs_f64)
        .sum::<f64>();
    let settled_throughput = SETTLED_REPETITIONS as f64 / settled_total_seconds;
    let settled_measurement = serde_json::json!({
        "schema_version": 1,
        "journey": "work_rollup_settled",
        "fixture": {
            "offered_sources": SOURCE_COUNT,
            "warmup_repetitions": WARMUP_REPETITIONS,
            "fresh_measured_repetitions": MEASURED_REPETITIONS,
            "settled_repetitions": SETTLED_REPETITIONS,
        },
        "warm_latency": {
            "p95_ms": settled_p95.as_secs_f64() * 1_000.0,
            "max_ms": settled
                .repetition_elapsed
                .iter()
                .max()
                .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1_000.0),
        },
        "throughput": {
            "operations_per_second": settled_throughput,
        },
        "settled_workload": {
            "operations": {
                "control": settled.control_operations,
                "repeated": settled.repeated_operations,
            },
            "semantic_output_identity": settled.semantic_output_identity,
            "reconciliations": measurement_json(
                settled.reconciliations,
                "no_work_rollup_reconciliation_counter_is_mounted",
            ),
            "database_reads": measurement_json(
                settled.database_reads,
                "no_per_operation_database_read_counter_is_mounted",
            ),
            "resources": resource_deltas_json(
                &settled.resources_before,
                &settled.resources_after,
            ),
        },
    });
    println!("{settled_measurement}");
}
