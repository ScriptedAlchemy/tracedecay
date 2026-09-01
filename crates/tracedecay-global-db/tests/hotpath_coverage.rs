//! Coverage for this crate's existing `#[hotpath::measure]` hot paths in
//! both feature modes.
//!
//! With `--features hotpath` the test wraps one registered-store workload in
//! a `HotpathGuard` that writes a JSON report to a temp file, then asserts
//! the report carries the measure labels the workload crossed — proving the
//! instrumentation fires rather than compiling to an empty report. Because
//! this crate's `hotpath` feature forwards to its storage dependencies, the
//! same report must also carry a `tracedecay-rusqlite-runtime` label,
//! pinning the cross-crate feature wiring. With the feature off the
//! identical workload runs with every macro expanded to a no-op.
//!
//! Exactly one `#[test]` lives in this binary on purpose: hotpath permits
//! one live guard per process, and `cargo test` runs a binary's tests
//! concurrently on shared process state.

#[cfg(feature = "hotpath")]
use std::collections::BTreeSet;

use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_store::SessionRecord;

const PROVIDER: &str = "claude";
const SESSION: &str = "hotpath-coverage";

fn session_record() -> SessionRecord {
    SessionRecord {
        provider: PROVIDER.to_owned(),
        session_id: SESSION.to_owned(),
        project_key: "/project".to_owned(),
        project_path: "/project".to_owned(),
        title: None,
        started_at: None,
        ended_at: None,
        transcript_path: Some(format!("/tmp/{SESSION}.jsonl")),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

/// Drives the measured registered-store hot paths once: profile open
/// (admission and schema convergence), a committed write transaction, and a
/// session-activity read, mirroring the `session_activity_reads` bench.
fn exercise_measured_hot_paths() {
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build coverage runtime");
    let profile = tempfile::tempdir().expect("temporary coverage profile");
    tokio.block_on(async {
        let runtime = RegisteredGlobalDbTestRuntime::profile(profile.path())
            .await
            .expect("open registered-store coverage fixture");
        let database = runtime.profile_database();
        assert!(database.upsert_session(&session_record()).await);
        let transaction = database
            .begin_write_transaction()
            .await
            .expect("begin coverage transaction");
        transaction
            .execute_batch(&format!(
                "INSERT INTO session_messages(
                     provider, message_id, session_id, role, timestamp, ordinal, text,
                     kind, model, tool_names, source_path, source_offset, metadata_json
                 )
                 VALUES
                     ('{PROVIDER}', 'message-000000', '{SESSION}', 'assistant', 1, 1,
                      'payload', 'activity', NULL, 'tool', NULL, NULL, NULL),
                     ('{PROVIDER}', 'message-000001', '{SESSION}', 'assistant', 2, 2,
                      'payload', 'activity', NULL, 'tool', NULL, NULL, NULL);"
            ))
            .await
            .expect("seed coverage session activity");
        transaction.commit().await.expect("commit coverage rows");
        let rows = database
            .session_messages_after(PROVIDER, SESSION, 0, 16)
            .await
            .expect("read coverage session activity");
        assert_eq!(rows.len(), 2);
    });
}

/// Measure labels the workload above must have crossed. Each label already
/// exists in `src/` (or, for the `rusqlite_runtime.` entry, in the storage
/// dependency this crate's `hotpath` feature forwards to); this test adds no
/// instrumentation of its own.
#[cfg(feature = "hotpath")]
const EXPECTED_MEASURE_LABELS: &[&str] = &[
    "global_db.registered.admit",
    "global_db.schema.persist.install",
    "global_db.registered.txn.begin",
    "global_db.registered.txn.commit",
    "global_db.registered_sessions.after",
    "rusqlite_runtime.exact_sql.execute_query",
];

/// Exact `name` / `label` strings from the already-parsed report. Substring
/// search of the serialized bytes would treat `global_db.registered.admit`
/// as present when only `global_db.registered.admit_daemon` fired.
#[cfg(feature = "hotpath")]
fn exact_report_labels(parsed: &serde_json::Value) -> BTreeSet<&str> {
    let mut labels = BTreeSet::new();
    collect_exact_report_labels(parsed, &mut labels);
    labels
}

#[cfg(feature = "hotpath")]
fn collect_exact_report_labels<'a>(value: &'a serde_json::Value, labels: &mut BTreeSet<&'a str>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if matches!(key.as_str(), "name" | "label")
                    && let Some(label) = child.as_str()
                {
                    labels.insert(label);
                }
                collect_exact_report_labels(child, labels);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_exact_report_labels(child, labels);
            }
        }
        _ => {}
    }
}

#[cfg(feature = "hotpath")]
fn assert_exact_label_comparison_rejects_prefix_siblings() {
    let parsed = serde_json::json!({
        "functions_timing": {
            "data": [{ "name": "global_db.registered.admit_daemon" }]
        }
    });
    let labels = exact_report_labels(&parsed);
    assert!(labels.contains("global_db.registered.admit_daemon"));
    assert!(
        !labels.contains("global_db.registered.admit"),
        "a prefix sibling must not satisfy an exact label match: {labels:?}"
    );
    assert!(
        parsed.to_string().contains("global_db.registered.admit"),
        "serialized bytes still contain the shorter label as a substring"
    );
}

#[cfg(feature = "hotpath")]
#[test]
fn measured_hot_paths_emit_a_hotpath_report() {
    assert_exact_label_comparison_rejects_prefix_siblings();
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
    let guard = hotpath::HotpathGuardBuilder::new("global-db-hotpath-coverage")
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
    let labels = exact_report_labels(&parsed);
    for label in EXPECTED_MEASURE_LABELS {
        assert!(
            labels.contains(label),
            "hotpath report is missing exact measure label {label:?}; labels: {labels:?}; report: {report}"
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
