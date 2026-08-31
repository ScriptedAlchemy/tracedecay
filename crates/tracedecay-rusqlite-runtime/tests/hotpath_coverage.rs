//! Coverage for this crate's existing `#[hotpath::measure]` hot paths in
//! both feature modes.
//!
//! With `--features hotpath` the test wraps one writer/ledger/reader
//! workload in a `HotpathGuard` that writes a JSON report to a temp file,
//! then asserts the report carries the measure labels the workload crossed —
//! proving the instrumentation fires rather than compiling to an empty
//! report. With the feature off the identical workload runs with every
//! macro expanded to a no-op.
//!
//! Exactly one `#[test]` lives in this binary on purpose: hotpath permits
//! one live guard per process, and `cargo test` runs a binary's tests
//! concurrently on shared process state.

#[path = "../../../tests/storage_runtime_rusqlite_suite/runtime_test_support.rs"]
mod runtime_test_support;

use std::sync::Arc;
use std::time::Duration;

use tracedecay_rusqlite_runtime::reader::ReaderPool;
use tracedecay_store::{AdmissionConfigV1, RuntimeSubmitOutcomeV1};

use runtime_test_support::{
    CountExecutor, Probe, TestDatabase, outbox_request, read_request, reader_locator,
    reader_runtime_fixture, run, writer, writer_runtime_fixture,
};

/// Drives the measured writer, ledger, and reader hot paths once.
///
/// The workload mirrors the acceptance suites: an authorized submit lands in
/// the transactional outbox and commits (`rusqlite_runtime.writer.*`,
/// `rusqlite.ledger.*`), then a reader pool lane pins a snapshot and serves
/// a read (`rusqlite_runtime.reader.acquire_lane`).
fn exercise_measured_hot_paths() {
    let fixture = writer_runtime_fixture();
    let database = TestDatabase::new("hotpath-coverage-writer.sqlite3");
    let request = outbox_request(
        &fixture.origin_binding,
        &fixture.target_binding,
        "operation.hotpath.coverage",
        fixture.effect_id,
        fixture.ordering_key,
    );
    let writer = Arc::new(writer(&database, &fixture.origin_binding));
    let outcome = run({
        let writer = Arc::clone(&writer);
        let probe = Probe::for_submit(&request);
        async move { writer.submit(request, probe).await }
    })
    .expect("execute coverage submit");
    assert!(
        matches!(outcome, RuntimeSubmitOutcomeV1::Committed { .. }),
        "expected committed coverage submit, got {outcome:?}"
    );
    Arc::try_unwrap(writer)
        .unwrap_or_else(|_| panic!("coverage submit retained the writer"))
        .shutdown_and_join()
        .expect("close coverage writer");

    let reader_fixture = reader_runtime_fixture();
    let reader_database = TestDatabase::new("hotpath-coverage-reader.sqlite3");
    reader_database
        .connect()
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE acceptance_rows(value INTEGER NOT NULL);
             INSERT INTO acceptance_rows(value) VALUES (1);",
        )
        .expect("seed coverage reader authority");
    let pool = ReaderPool::start(
        reader_locator(&reader_fixture.binding, &reader_database.path),
        AdmissionConfigV1::default().readers,
        CountExecutor,
    )
    .expect("start coverage reader pool");
    let request = read_request(&reader_fixture.binding, "foreground");
    let probe = Probe::for_read(&request);
    let mut reader = pool
        .acquire(&request, &probe, Duration::ZERO)
        .expect("acquire coverage reader lane");
    let mut snapshot = reader.begin_snapshot().expect("begin coverage snapshot");
    snapshot
        .execute(request, &probe)
        .expect("execute coverage snapshot read");
}

/// Measure labels the workload above must have crossed. Each label already
/// exists in `src/`; this test adds no instrumentation of its own.
#[cfg(feature = "hotpath")]
const EXPECTED_MEASURE_LABELS: &[&str] = &[
    "rusqlite_runtime.writer.submit_authorized",
    "rusqlite_runtime.writer.transaction_batch",
    "rusqlite_runtime.writer.execution_batch",
    "rusqlite.ledger.outbox_insert",
    "rusqlite.ledger.record_commit",
    "rusqlite.ledger.idempotency_lookup",
    "rusqlite_runtime.reader.acquire_lane",
];

#[cfg(feature = "hotpath")]
#[test]
fn measured_hot_paths_emit_a_hotpath_report() {
    // Guard construction binds a localhost metrics server unless disabled.
    // Tests must not open sockets, and parallel test processes would race on
    // the port. SAFETY: this binary holds exactly one test, so nothing else
    // reads or writes the environment concurrently — the same ordering the
    // `tracedecay-index-bench` entrypoint relies on.
    if std::env::var_os("HOTPATH_METRICS_SERVER_OFF").is_none() {
        unsafe {
            std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        }
    }
    let report_dir = tempfile::tempdir().expect("create hotpath report directory");
    let report_path = report_dir.path().join("hotpath-coverage.json");
    // The default report keeps only the top functions by share of runtime;
    // cheap-but-covered measures must not fall off the end of the table.
    let guard = hotpath::HotpathGuardBuilder::new("rusqlite-runtime-hotpath-coverage")
        .format(hotpath::Format::Json)
        .output_path(&report_path)
        .functions_limit(512)
        .build();

    exercise_measured_hot_paths();

    // The report is written when the guard drops; it must observe the whole
    // workload above.
    drop(guard);

    let report = std::fs::read_to_string(&report_path).expect("read hotpath JSON report");
    let parsed: serde_json::Value =
        serde_json::from_str(&report).expect("hotpath report is valid JSON");
    assert!(
        parsed.is_object(),
        "hotpath report should be a JSON object, got: {parsed}"
    );
    for label in EXPECTED_MEASURE_LABELS {
        assert!(
            report.contains(label),
            "hotpath report is missing measure label {label:?}; report: {report}"
        );
    }
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn measured_hot_paths_run_with_the_feature_off() {
    // Every `#[hotpath::measure]` in the crate expands to a no-op here; the
    // workload succeeding is the proof the instrumentation stays inert.
    exercise_measured_hot_paths();
}
