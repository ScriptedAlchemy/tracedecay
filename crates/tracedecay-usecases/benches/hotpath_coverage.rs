//! Shared Hotpath coverage support for this crate's bench binaries.
//!
//! The workload benches (`work_rollup`, `semantic_vector_commit_scale`)
//! compile in two modes and this module makes both modes self-checking:
//!
//! - feature off (`default`): every hotpath macro in the workload is a
//!   no-op. [`init`] records the state of any operator-named
//!   `HOTPATH_OUTPUT_PATH` and [`finish`] asserts the run never created or
//!   modified a report there - profiling must not leak into default builds.
//! - feature on (`--features hotpath`): [`init`] forces the metrics server
//!   off (these workloads are specified to open no socket) and installs the
//!   process-boundary guard. When the operator did not name a report
//!   destination the run self-verifies: the report goes to a scratch JSON
//!   file and [`finish`] drops the guard, parses the report, and asserts the
//!   expected static `crate.area.verb` labels were recorded. When the
//!   operator did name `HOTPATH_OUTPUT_PATH` the profile belongs to them:
//!   the guard honors their configuration and no verification synthesizes
//!   extra work into their report.
//!
//! Labels passed to [`finish`] must be labels this crate already stamps
//! (`#[hotpath::measure]`, `measure_block!`, `gauge!`); this module never
//! introduces its own measurement labels.
//!
//! The environment mutation in [`init`] is sound for the same reason as in
//! `tracedecay-index-bench`: it runs as the first statement of `main`,
//! before the Tokio runtime, the guard, or any other thread exists.

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
impl HotpathCoverage {
    /// True when this run owns a scratch report and label verification will
    /// run on [`finish`]. Operator-owned profiling runs return false so
    /// coverage probes never synthesize work into a real profile.
    #[allow(dead_code)]
    pub fn verifying(&self) -> bool {
        self.verified_report.is_some()
    }
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
    let report = std::fs::read_to_string(&report_path).expect(
        "a feature-on bench run must write a Hotpath JSON report when its guard drops",
    );
    assert!(!report.is_empty(), "Hotpath report must not be empty");
    serde_json::from_str::<serde_json::Value>(&report)
        .expect("Hotpath report must be valid JSON");
    for label in expected_labels {
        assert!(
            report.contains(label),
            "Hotpath report at {} is missing the static label {label:?}",
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
pub struct HotpathCoverage {
    bench: &'static str,
    operator_report: Option<(std::path::PathBuf, Option<Vec<u8>>)>,
}

#[cfg(not(feature = "hotpath"))]
pub fn init(bench: &'static str) -> HotpathCoverage {
    // Feature off, every hotpath macro in the workload expands to a no-op
    // and nothing reads the report environment. Capture the pre-run state of
    // any operator-named destination so `finish` can prove that stayed true.
    let operator_report = std::env::var_os("HOTPATH_OUTPUT_PATH")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .map(|path| {
            let prior = std::fs::read(&path).ok();
            (path, prior)
        });
    HotpathCoverage {
        bench,
        operator_report,
    }
}

#[cfg(not(feature = "hotpath"))]
pub fn finish(coverage: HotpathCoverage, _expected_labels: &[&str]) {
    if let Some((path, prior)) = coverage.operator_report {
        let current = std::fs::read(&path).ok();
        assert_eq!(
            current,
            prior,
            "a feature-off bench build must never write a Hotpath report to {}",
            path.display(),
        );
    }
    println!("hotpath_coverage,bench={},mode=feature_off", coverage.bench);
}
