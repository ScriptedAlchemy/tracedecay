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
/// The ledger stays silent until a journey arms it, which keeps a shared test
/// binary from mixing two journeys into one table.
mod stage_ledger {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    pub(super) struct StageRowV1 {
        pub stage: String,
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

    pub(super) fn arm() {
        ARMED.store(true, Ordering::SeqCst);
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
/// ever rises.
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

/// Start attributing wall clock to this journey's stages.
pub(super) fn arm_stage_ledger() {
    stage_ledger::arm();
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
    let (vm_rss_kb, vm_hwm_kb) = process_memory_kb();
    stage_ledger::rows()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(StageRowV1 {
            stage: stage.into(),
            wall,
            count,
            unit,
            vm_rss_kb,
            vm_hwm_kb,
        });
}

/// Time one journey stage and attribute its wall clock.
pub(super) async fn timed_stage<T>(stage: &str, work: impl std::future::Future<Output = T>) -> T {
    let started = std::time::Instant::now();
    let outcome = work.await;
    record_stage(stage, started.elapsed(), 0, "");
    outcome
}

/// Fold one native evaluation report's already-measured stage micros into the
/// ledger. The report is the only authority for the daemon-internal stages
/// (cold model load, per-scale projection, projection cases, evaluation
/// queries); it is read, never extended, because its own digest binds it.
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
    for output in &report.raw_outputs {
        let Some(evidence) = output.native_resources.as_ref() else {
            continue;
        };
        for (scale, result) in &evidence.samples {
            let SemanticNativeStageResultV1::Complete(sample) = result else {
                continue;
            };
            let at = |stage: &str| format!("{label}/{}/{scale}/{stage}", output.profile_id);
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
}

/// Print the attribution table. Dropped last so a failing journey still
/// reports every stage it did reach.
pub(super) struct StageLedgerReportV1 {
    pub title: &'static str,
    pub started: std::time::Instant,
}

impl StageLedgerReportV1 {
    pub(super) fn arm(title: &'static str) -> Self {
        arm_stage_ledger();
        Self {
            title,
            started: std::time::Instant::now(),
        }
    }
}

impl Drop for StageLedgerReportV1 {
    fn drop(&mut self) {
        let rows = stage_ledger::rows()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let total = self.started.elapsed();
        let (vm_rss_kb, vm_hwm_kb) = process_memory_kb();
        let width = rows
            .iter()
            .map(|row| row.stage.len())
            .chain(std::iter::once(24))
            .max()
            .unwrap_or(24);
        eprintln!("\n=== {} stage attribution (#838) ===", self.title);
        eprintln!(
            "{:<width$}  {:>10}  {:>6}  {:>10}  {:>10}  count",
            "stage", "wall_s", "share", "vm_rss_kB", "vm_hwm_kB",
        );
        for row in rows.iter() {
            let share = if total.as_secs_f64() > 0.0 {
                row.wall.as_secs_f64() / total.as_secs_f64() * 100.0
            } else {
                0.0
            };
            let count = if row.unit.is_empty() {
                String::new()
            } else {
                format!("{} {}", row.count, row.unit)
            };
            eprintln!(
                "{:<width$}  {:>10.3}  {:>5.1}%  {:>10}  {:>10}  {count}",
                row.stage,
                row.wall.as_secs_f64(),
                share,
                row.vm_rss_kb,
                row.vm_hwm_kb,
            );
        }
        eprintln!(
            "{:<width$}  {:>10.3}  {:>5.1}%  {:>10}  {:>10}",
            "JOURNEY TOTAL",
            total.as_secs_f64(),
            100.0,
            vm_rss_kb,
            vm_hwm_kb,
        );
        eprintln!("=== end {} stage attribution ===\n", self.title);
    }
}
