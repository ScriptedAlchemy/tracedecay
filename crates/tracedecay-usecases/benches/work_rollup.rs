use std::time::Duration;

#[allow(dead_code, unused_imports)]
#[path = "../tests/observability_runtime_contract.rs"]
mod observability_runtime_contract;

use observability_runtime_contract::work_rollup_harness::{
    READ_TRIPWIRE, SOURCE_COUNT, TRIPWIRE, WorkRollupReport, run_work_rollup_case,
};
use tracedecay_domain::CoverageStateV1;

const WARMUP_REPETITIONS: usize = 3;
const MEASURED_REPETITIONS: usize = 30;
const MIN_RECEIPTS_PER_SECOND: f64 = 256.0;

fn percentile95(samples: &[Duration]) -> Duration {
    let mut samples = samples.to_vec();
    samples.sort_unstable();
    let rank = (samples.len() * 95).div_ceil(100);
    samples[rank.saturating_sub(1)]
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
}
