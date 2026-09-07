//! Git fixture and MCP tool-payload helpers shared by the
//! production-composition journey tests.

use std::path::Path;

use serde_json::Value;

use tracedecay_mcp::JsonRpcResponse;

pub(super) fn git(project: &Path, arguments: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(project)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output")
        .trim()
        .to_owned()
}

pub(super) fn tool_payload(response: &JsonRpcResponse) -> Value {
    assert!(response.error.is_none(), "tool failed: {response:?}");
    let result = response.result.as_ref().expect("tool result");
    assert_ne!(result["isError"], true, "tool failed: {result}");
    let text = result["content"][0]["text"].as_str().expect("tool text");
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tool did not return JSON: {error}; result={result}; text={text}")
    })
}

/// Wall-clock attribution ledger for the long semantic journeys (#838).
///
/// Measurement only: recording a row never changes what a journey step does.
/// Rows are process-global so a helper that discards its evaluation report
/// (activation returns a digest, not a report) can still contribute the stages
/// it owns, and so the table survives the panic that ends a failing journey.
///
/// Exactly one journey may own the ledger at a time: `arm` takes the process
/// flag by compare-and-swap, clears the previous run's rows, and the report
/// guard releases the flag on drop. A second journey running in the same test
/// binary records nothing rather than folding its stages into another
/// journey's table.
mod stage_ledger {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
    use std::time::Duration;

    pub(super) struct StageRowV1 {
        pub stage: String,
        /// The stage whose wall clock encloses this one, if any. A nested row
        /// is a breakdown of its parent, never an additional slice of the
        /// journey, so its share is reported against the parent.
        pub enclosing: Option<String>,
        pub wall: Duration,
        pub count: u64,
        pub unit: &'static str,
        pub vm_rss_kb: u64,
        pub vm_hwm_kb: u64,
    }

    static ARMED: AtomicBool = AtomicBool::new(false);

    pub(super) fn rows() -> &'static Mutex<Vec<StageRowV1>> {
        static ROWS: OnceLock<Mutex<Vec<StageRowV1>>> = OnceLock::new();
        ROWS.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// Serialized sample identities already folded into the table. The
    /// evaluator shares one measured sample across every profile and partition
    /// that has the same inputs, so the same observation appears in several
    /// report outputs; folding it once per appearance would report the same
    /// microseconds several times.
    pub(super) fn folded_samples() -> &'static Mutex<BTreeSet<String>> {
        static FOLDED: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
        FOLDED.get_or_init(|| Mutex::new(BTreeSet::new()))
    }

    pub(super) fn lock_rows() -> MutexGuard<'static, Vec<StageRowV1>> {
        rows().lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take the ledger for one journey. Returns false when another journey in
    /// this test binary already owns it.
    pub(super) fn arm() -> bool {
        if ARMED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        lock_rows().clear();
        folded_samples()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        true
    }

    pub(super) fn disarm() {
        ARMED.store(false, Ordering::SeqCst);
        lock_rows().clear();
        folded_samples()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    pub(super) fn armed() -> bool {
        ARMED.load(Ordering::SeqCst)
    }
}

use stage_ledger::StageRowV1;

/// `VmRSS` and `VmHWM` of this process, in kB.
///
/// The production composition harness runs the daemon in-process, so these are
/// the daemon-side numbers (#852). `VmHWM` is a process-lifetime peak and only
/// ever rises, so a row's `VmHWM` is the highest resident size the whole test
/// process had reached by the time that row was recorded -- never that stage's
/// own peak.
pub(super) fn process_memory_kb() -> (u64, u64) {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return (0, 0);
    };
    let field = |name: &str| {
        status
            .lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
    };
    (field("VmRSS:"), field("VmHWM:"))
}

fn stage_row(
    stage: impl Into<String>,
    wall: std::time::Duration,
    count: u64,
    unit: &'static str,
) -> StageRowV1 {
    let (vm_rss_kb, vm_hwm_kb) = process_memory_kb();
    StageRowV1 {
        stage: stage.into(),
        enclosing: None,
        wall,
        count,
        unit,
        vm_rss_kb,
        vm_hwm_kb,
    }
}

pub(super) fn record_stage(
    stage: impl Into<String>,
    wall: std::time::Duration,
    count: u64,
    unit: &'static str,
) {
    if !stage_ledger::armed() {
        return;
    }
    let row = stage_row(stage, wall, count, unit);
    stage_ledger::lock_rows().push(row);
}

/// Time one journey stage and attribute its wall clock.
///
/// Every row recorded while `work` is in flight is a breakdown of this stage,
/// so the stage is inserted ahead of them and adopts the ones no inner stage
/// already claimed. The journey stages run one after another on one task, so
/// the rows recorded between this stage's start and its end are exactly its
/// own; a journey that ran two timed stages concurrently would need explicit
/// parent handles instead.
pub(super) async fn timed_stage<T>(stage: &str, work: impl std::future::Future<Output = T>) -> T {
    let first_child = stage_ledger::lock_rows().len();
    let started = std::time::Instant::now();
    let outcome = work.await;
    let elapsed = started.elapsed();
    if stage_ledger::armed() {
        let row = stage_row(stage, elapsed, 0, "");
        let mut rows = stage_ledger::lock_rows();
        for child in rows.iter_mut().skip(first_child) {
            if child.enclosing.is_none() {
                child.enclosing = Some(stage.to_owned());
            }
        }
        let insert_at = first_child.min(rows.len());
        rows.insert(insert_at, row);
    }
    outcome
}

/// Fold one native evaluation report's already-measured stage micros into the
/// ledger. The report is the only authority for the daemon-internal stages
/// (cold model load, per-scale projection, projection cases, evaluation
/// queries); it is read, never extended, because its own digest binds it.
///
/// The evaluator reuses one measured sample for every profile and partition
/// that shares its inputs, so the identical observation is carried by several
/// report outputs. Each distinct sample is folded once; the repeats are
/// counted and reported instead of being added to the wall clock again.
pub(super) fn record_evaluation_report(
    label: &str,
    report: &tracedecay_query::search_quality::DirectEvaluationReportV1,
) {
    use std::time::Duration;
    use tracedecay_query::search_quality::semantic_native::SemanticNativeStageResultV1;

    if !stage_ledger::armed() {
        return;
    }
    let micros = Duration::from_micros;
    let mut reused_samples = 0_u64;
    for output in &report.raw_outputs {
        let Some(evidence) = output.native_resources.as_ref() else {
            continue;
        };
        for (scale, result) in &evidence.samples {
            let SemanticNativeStageResultV1::Complete(sample) = result else {
                continue;
            };
            let identity = serde_json::to_string(sample)
                .unwrap_or_else(|_| format!("{}/{}/{scale}", output.profile_id, output.partition));
            let first_fold = stage_ledger::folded_samples()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(identity);
            if !first_fold {
                reused_samples += 1;
                continue;
            }
            let at = |stage: &str| {
                format!(
                    "{label}/{}/{}/{scale}/{stage}",
                    output.profile_id, output.partition
                )
            };
            record_stage(
                at("model.cold_load"),
                micros(sample.cold_model_load_samples_us.iter().sum::<u64>()),
                sample.cold_model_load_samples_us.len() as u64,
                "session opens",
            );
            record_stage(
                at("projection.clean_build"),
                micros(sample.clean_projection_build_samples_us.iter().sum::<u64>()),
                sample.eligible_chunks,
                "eligible chunks",
            );
            record_stage(
                at("projection.incremental_rebuild"),
                micros(sample.incremental_rebuild_samples_us.iter().sum::<u64>()),
                0,
                "",
            );
            for (case, measurement) in &sample.projection_cases {
                record_stage(
                    at(&format!("projection.case.{case:?}")),
                    micros(measurement.elapsed_micros),
                    measurement.projection_calls,
                    "projections",
                );
            }
            record_stage(
                at("evaluation.queries"),
                micros(sample.latency_samples_us.iter().sum::<u64>()),
                sample.measured_queries,
                "queries",
            );
            record_stage(
                at("cpu_time(not wall)"),
                micros(sample.cpu_time_us.unwrap_or_default()),
                sample.peak_rss_bytes.unwrap_or_default() / 1024,
                "peak rss kB",
            );
        }
    }
    if reused_samples > 0 {
        record_stage(
            format!("{label}/reused_samples(not added to wall)"),
            Duration::ZERO,
            reused_samples,
            "repeat appearances",
        );
    }
}

/// How many enclosing stages a row sits inside. Bounded by the row count: a
/// stage can only be enclosed by a stage recorded before it.
fn stage_depth(rows: &[StageRowV1], stage: &str) -> usize {
    let mut depth = 0_usize;
    let mut current = stage.to_owned();
    while let Some(parent) = rows
        .iter()
        .find(|row| row.stage == current)
        .and_then(|row| row.enclosing.clone())
    {
        depth += 1;
        if depth > rows.len() {
            break;
        }
        current = parent;
    }
    depth
}

/// Print the attribution table. Dropped last so a failing journey still
/// reports every stage it did reach.
pub(super) struct StageLedgerReportV1 {
    pub title: &'static str,
    pub started: std::time::Instant,
    /// False when another journey in this test binary already owned the
    /// ledger; this guard then prints nothing and releases nothing.
    owns_ledger: bool,
}

impl StageLedgerReportV1 {
    pub(super) fn arm(title: &'static str) -> Self {
        let owns_ledger = stage_ledger::arm();
        if !owns_ledger {
            eprintln!(
                "{title}: another journey already owns the #838 stage ledger; \
                 this run records no stages"
            );
        }
        Self {
            title,
            started: std::time::Instant::now(),
            owns_ledger,
        }
    }
}

impl Drop for StageLedgerReportV1 {
    fn drop(&mut self) {
        if !self.owns_ledger {
            return;
        }
        // Take the rows so the report's lookups borrow a plain vector and the
        // shared ledger is released for the next journey in this binary.
        let rows = std::mem::take(&mut *stage_ledger::lock_rows());
        let total = self.started.elapsed();
        let (vm_rss_kb, vm_hwm_kb) = process_memory_kb();
        let depth = |stage: &str| stage_depth(&rows, stage);
        let enclosing_wall = |name: &str| {
            rows.iter()
                .find(|row| row.stage == name)
                .map(|row| row.wall.as_secs_f64())
        };
        let width = rows
            .iter()
            .map(|row| row.stage.len() + 2 * depth(&row.stage))
            .chain(std::iter::once(24))
            .max()
            .unwrap_or(24);
        eprintln!("\n=== {} stage attribution (#838) ===", self.title);
        eprintln!(
            "{:<width$}  {:>10}  {:>6}  {:>10}  {:>10}  {:<28}  count",
            "stage", "wall_s", "share", "vm_rss_kB", "vm_hwm_kB", "share of",
        );
        let mut exclusive = 0.0_f64;
        for row in &rows {
            let (basis, basis_label) = match row.enclosing.as_ref() {
                Some(name) => (
                    enclosing_wall(name).unwrap_or(total.as_secs_f64()),
                    name.as_str(),
                ),
                None => {
                    exclusive += row.wall.as_secs_f64();
                    (total.as_secs_f64(), "JOURNEY TOTAL")
                }
            };
            let share = if basis > 0.0 {
                row.wall.as_secs_f64() / basis * 100.0
            } else {
                0.0
            };
            let count = if row.unit.is_empty() {
                String::new()
            } else {
                format!("{} {}", row.count, row.unit)
            };
            let indent = "  ".repeat(depth(&row.stage));
            eprintln!(
                "{:<width$}  {:>10.3}  {:>5.1}%  {:>10}  {:>10}  {:<28}  {count}",
                format!("{indent}{}", row.stage),
                row.wall.as_secs_f64(),
                share,
                row.vm_rss_kb,
                row.vm_hwm_kb,
                basis_label,
            );
        }
        eprintln!(
            "{:<width$}  {:>10.3}  {:>5.1}%  {:>10}  {:>10}",
            "TOP-LEVEL SUM",
            exclusive,
            if total.as_secs_f64() > 0.0 {
                exclusive / total.as_secs_f64() * 100.0
            } else {
                0.0
            },
            "",
            "",
        );
        eprintln!(
            "{:<width$}  {:>10.3}  {:>5.1}%  {:>10}  {:>10}",
            "UNATTRIBUTED",
            total.as_secs_f64() - exclusive,
            if total.as_secs_f64() > 0.0 {
                (total.as_secs_f64() - exclusive) / total.as_secs_f64() * 100.0
            } else {
                0.0
            },
            "",
            "",
        );
        eprintln!(
            "{:<width$}  {:>10.3}  {:>5.1}%  {:>10}  {:>10}  vm_hwm is a process-lifetime peak",
            "JOURNEY TOTAL",
            total.as_secs_f64(),
            100.0,
            vm_rss_kb,
            vm_hwm_kb,
        );
        eprintln!("=== end {} stage attribution ===\n", self.title);
        stage_ledger::disarm();
    }
}
