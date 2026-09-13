//! Shared Hotpath coverage support for this crate's bench binaries.
//!
//! The workload benches (`work_rollup`, `semantic_vector_commit_scale`)
//! compile in two modes and this module makes both modes self-checking:
//!
//! - feature off (`default`): every hotpath macro in the workload is a
//!   no-op. [`init`] records a fingerprint of any operator-named
//!   `HOTPATH_OUTPUT_PATH` and [`finish`] asserts the run never created or
//!   modified a report there — profiling must not leak into default builds.
//!   The fingerprint is a length plus digest; prior report bytes are not
//!   retained across the workload (they would corrupt peak-memory samples).
//! - feature on (`--features hotpath`): [`init`] forces the metrics server
//!   off (these workloads are specified to open no socket) and installs the
//!   process-boundary guard. When the operator did not name a report
//!   destination the run self-verifies: the report goes to a scratch JSON
//!   file and [`finish`] drops the guard, parses the report, and asserts the
//!   expected static labels were recorded as exact `name` / `label` fields.
//!   When the operator did name `HOTPATH_OUTPUT_PATH` the profile belongs to
//!   them: the guard honors their configuration and no verification
//!   synthesizes extra work into their report.
//!
//! Labels passed to [`finish`] must come from the measured workload path
//! (`#[hotpath::measure]`, `measure_block!`, `gauge!`). This module never
//! introduces its own measurement labels or extra reads.
//!
//! The environment mutation in [`init`] is sound for the same reason as in
//! `tracedecay-index-bench`: it runs as the first statement of `main`,
//! before the Tokio runtime, the guard, or any other thread exists.

#[cfg(feature = "hotpath")]
use std::collections::BTreeSet;

#[cfg(feature = "hotpath")]
pub struct HotpathCoverage {
    bench: &'static str,
    guard: Option<hotpath::HotpathGuard>,
    verified_report: Option<std::path::PathBuf>,
}

#[cfg(feature = "hotpath")]
pub fn init(bench: &'static str) -> HotpathCoverage {
    // Hotpath binds a localhost metrics server on guard construction. These
    // benches open no socket, so the server stays off unless an operator
    // explicitly asked for it.
    if std::env::var_os("HOTPATH_METRICS_SERVER_OFF").is_none() {
        unsafe {
            std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        }
    }
    let operator_owns_report = std::env::var_os("HOTPATH_OUTPUT_PATH")
        .is_some_and(|path| path.to_str().is_some_and(|path| !path.is_empty()));
    let verified_report = if operator_owns_report {
        None
    } else {
        // Self-verified mode: the report goes to a scratch file rather than
        // stdout (stdout carries each bench's machine-readable lines) and is
        // parsed on `finish`. `functions-timing` and `futures` carry
        // `#[hotpath::measure]` spans; `debug` carries `gauge!` keys.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "{bench}-hotpath-coverage-{}-{unique:x}.json",
            std::process::id()
        ));
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
            std::env::set_var("HOTPATH_OUTPUT_PATH", &path);
        }
        if std::env::var_os("HOTPATH_REPORT").is_none() {
            unsafe {
                std::env::set_var("HOTPATH_REPORT", "functions-timing,futures,debug");
            }
        }
        Some(path)
    };
    let guard = hotpath::HotpathGuardBuilder::new(bench).build();
    HotpathCoverage {
        bench,
        guard: Some(guard),
        verified_report,
    }
}

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
            "data": [{ "name": "application.topology.rollup.read_model" }]
        }
    });
    let labels = exact_report_labels(&parsed);
    assert!(labels.contains("application.topology.rollup.read_model"));
    assert!(
        !labels.contains("application.topology.rollup.read"),
        "a prefix sibling must not satisfy an exact label match: {labels:?}"
    );
}

#[cfg(feature = "hotpath")]
pub fn finish(mut coverage: HotpathCoverage, expected_labels: &[&str]) {
    // Dropping the guard at this graceful boundary (never `process::exit`)
    // is what emits the exit report.
    drop(coverage.guard.take());
    let Some(report_path) = coverage.verified_report.take() else {
        println!(
            "hotpath_coverage,bench={},mode=operator_report",
            coverage.bench
        );
        return;
    };
    assert!(
        !expected_labels.is_empty(),
        "self-verified feature-on runs must require at least one exact workload label"
    );
    assert_exact_label_comparison_rejects_prefix_siblings();
    let report = std::fs::read_to_string(&report_path)
        .expect("a feature-on bench run must write a Hotpath JSON report when its guard drops");
    assert!(!report.is_empty(), "Hotpath report must not be empty");
    let parsed: serde_json::Value =
        serde_json::from_str(&report).expect("Hotpath report must be valid JSON");
    let labels = exact_report_labels(&parsed);
    for label in expected_labels {
        assert!(
            labels.contains(label),
            "Hotpath report at {} is missing the exact static label {label:?}; labels: {labels:?}",
            report_path.display(),
        );
    }
    std::fs::remove_file(&report_path).expect("remove scratch Hotpath report");
    println!(
        "hotpath_coverage,bench={},mode=verified,labels_verified={},report_bytes={}",
        coverage.bench,
        expected_labels.len(),
        report.len(),
    );
}

#[cfg(not(feature = "hotpath"))]
struct ReportFingerprint {
    len: u64,
    digest: [u8; 32],
}

#[cfg(not(feature = "hotpath"))]
pub struct HotpathCoverage {
    bench: &'static str,
    operator_report: Option<(std::path::PathBuf, Option<ReportFingerprint>)>,
}

#[cfg(not(feature = "hotpath"))]
fn report_fingerprint(path: &std::path::Path) -> Option<ReportFingerprint> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    let mut len = 0_u64;
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        len = len.checked_add(read as u64)?;
        hasher.update(&buffer[..read]);
    }
    Some(ReportFingerprint {
        len,
        digest: hasher.finalize().into(),
    })
}

#[cfg(not(feature = "hotpath"))]
pub fn init(bench: &'static str) -> HotpathCoverage {
    // Feature off, every hotpath macro in the workload expands to a no-op
    // and nothing reads the report environment. Fingerprint any operator-
    // named destination so `finish` can prove that stayed true without
    // retaining the prior file's bytes through peak-memory samples.
    let operator_report = std::env::var_os("HOTPATH_OUTPUT_PATH")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .map(|path| {
            let fingerprint = report_fingerprint(&path);
            (path, fingerprint)
        });
    HotpathCoverage {
        bench,
        operator_report,
    }
}

#[cfg(not(feature = "hotpath"))]
pub fn finish(coverage: HotpathCoverage, _expected_labels: &[&str]) {
    if let Some((path, prior)) = coverage.operator_report {
        let current = report_fingerprint(&path);
        assert_eq!(
            current
                .as_ref()
                .map(|fingerprint| (fingerprint.len, fingerprint.digest)),
            prior
                .as_ref()
                .map(|fingerprint| (fingerprint.len, fingerprint.digest)),
            "a feature-off bench build must never write a Hotpath report to {}",
            path.display(),
        );
    }
    println!("hotpath_coverage,bench={},mode=feature_off", coverage.bench);
}
