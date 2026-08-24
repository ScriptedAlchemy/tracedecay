//! Falsifiable proof that the exact-SQL "begin" measurements are two
//! genuinely distinct Hotpath spans, not one metric wearing two names.
//!
//! `rusqlite.exact_sql.begin_immediate` (caller side, `ExactSqlHandle::begin_immediate`
//! in `exact_sql/mod.rs`) times the whole round trip: dispatching
//! `BeginTransaction` to the writer thread, waiting on the reply channel, and
//! the lock acquisition the worker performs before it replies.
//! `rusqlite.exact_sql.write_lock` (`command::begin_transaction_with_busy_retry`)
//! times only the worker-side busy-retry loop that actually takes SQLite's
//! write lock. Collapsing them into one label — which is what
//! `rusqlite.begin_immediate` used to do before they were split — sums a
//! channel wait and a lock wait into a population whose mean and p95 describe
//! neither.
//!
//! This only asserts that both span *names* were recorded at least once and
//! that the names differ; it never asserts on elapsed time, because Hotpath's
//! own timings swing double digits in percent run over run on shared CI
//! hardware, so a duration threshold here would be flaky by construction.
//! Only compiled when this crate's `hotpath` feature is enabled: without it,
//! `hotpath::measure_block!` is a no-op and there is no report to inspect.

use super::*;

#[test]
fn begin_immediate_and_write_lock_are_distinct_spans() {
    // Ambient `HOTPATH_OUTPUT_FORMAT`/`HOTPATH_OUTPUT_PATH` env vars would
    // silently override the explicit `.format(..)`/`.output_path(..)` below.
    // SAFETY: this process may run other tests concurrently, but none of them
    // read or write these two specific variables (grep confirms the crate's
    // only other env-var test, `remote_client_proxy`, lives in a different
    // crate and touches unrelated proxy variables), so this mutation cannot
    // race with another test's observation of the same keys.
    unsafe {
        std::env::remove_var("HOTPATH_OUTPUT_FORMAT");
        std::env::remove_var("HOTPATH_OUTPUT_PATH");
    }

    let report_dir = TempDir::new().unwrap();
    let report_path = report_dir.path().join("exact-sql-begin-spans.json");

    let guard = hotpath::HotpathGuardBuilder::new("exact_sql_begin_span_split_test")
        .format(hotpath::Format::Json)
        .output_path(&report_path)
        .build();

    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE hotpath_begin_spans (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    // `begin_immediate` records the caller round trip; the worker it
    // dispatches to records `write_lock` for the same call, on its own
    // thread, before replying.
    let transaction = channel.begin_immediate().unwrap();
    transaction
        .execute(statement(
            "INSERT INTO hotpath_begin_spans VALUES (?)",
            vec![ExactSqlValue::Integer(1)],
        ))
        .unwrap();
    transaction.commit().unwrap();

    // Guard::drop flushes the accumulated registry to `report_path`.
    drop(guard);

    let report_text = std::fs::read_to_string(&report_path)
        .expect("hotpath guard must write its report to output_path on drop");
    let report: serde_json::Value =
        serde_json::from_str(&report_text).expect("hotpath report must be valid JSON");
    let functions = report["functions_timing"]["data"]
        .as_array()
        .expect("functions_timing.data must be present in a timing report");

    let calls_for = |name: &str| -> Option<u64> {
        functions
            .iter()
            .find(|entry| entry["name"] == name)
            .and_then(|entry| entry["calls"].as_u64())
    };

    let begin_immediate_calls = calls_for("rusqlite.exact_sql.begin_immediate");
    let write_lock_calls = calls_for("rusqlite.exact_sql.write_lock");

    // Real output, pasted verbatim in the PR description this test backs.
    eprintln!(
        "exact-sql begin span report: begin_immediate={begin_immediate_calls:?} calls, \
         write_lock={write_lock_calls:?} calls\nfull report: {report_text}"
    );

    assert!(
        begin_immediate_calls.unwrap_or(0) >= 1,
        "expected rusqlite.exact_sql.begin_immediate to be recorded at least once; \
         full report: {report_text}"
    );
    assert!(
        write_lock_calls.unwrap_or(0) >= 1,
        "expected rusqlite.exact_sql.write_lock to be recorded at least once; \
         full report: {report_text}"
    );
    assert_ne!(
        "rusqlite.exact_sql.begin_immediate", "rusqlite.exact_sql.write_lock",
        "the caller round trip and the worker lock acquisition must stay distinct span names"
    );
}
