//! Process-wide code-index worker policy and its dedicated Rayon pool.
//!
//! Automatic sizing races small machines at full width and reserves half of a
//! larger host for serving. A conservative per-worker resident budget then
//! caps that CPU target. Exact profile or environment selections are never
//! silently narrowed: an unsafe count is a typed startup refusal.

use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicUsize, Ordering},
};

use tracedecay_domain::configuration::{
    CodeIndexWorkerLimitingReasonV1, CodeIndexWorkerSelectionV1, CodeIndexWorkerStatusV1,
};
use tracedecay_private_fs::background_cpu::{
    BackgroundCpuInstallErrorV1, install_process_background_cpu, process_background_cpu,
};

/// Operator override for the indexing width. It has higher precedence than
/// the profile setting and must be a positive `u16`.
const INDEXING_WORKERS_ENV: &str = "TRACEDECAY_INDEX_WORKERS";

/// Conservative V1 scratch-RSS admission per active parser/artifact worker.
///
/// One worker may simultaneously build a tree-sitter syntax tree, extraction
/// rows, chunks, and graph edges. The scheduler reserves `effective_workers *
/// this value` around every production build. Source snapshot retention is
/// measured and charged separately, so `memory_safe_workers` truthfully
/// describes this modeled worker scratch only. Measured profiling may revise
/// this versioned budget, but configuration must never choose it merely to
/// reach a preferred width on one host.
pub const INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1: u64 = 128 * 1024 * 1024;

/// Memory deliberately left outside worker scratch admission for retained
/// source snapshots and the process's other canonical resident components.
/// The larger of this floor and 25% of currently available authority is held
/// back before `memory_safe_workers` is derived.
const INDEX_NON_WORKER_MEMORY_HEADROOM_BYTES_V1: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CodeIndexWorkerPlanV1 {
    configured: CodeIndexWorkerSelectionV1,
    environment_override_workers: Option<u16>,
    requested_workers: usize,
    effective_workers: usize,
    available_logical_cpus: usize,
    memory_safe_workers: usize,
    memory_headroom_bytes: u64,
    limiting_reason: CodeIndexWorkerLimitingReasonV1,
    reservation_bytes: u64,
}

impl CodeIndexWorkerPlanV1 {
    fn status(self) -> CodeIndexWorkerStatusV1 {
        CodeIndexWorkerStatusV1 {
            configured: self.configured,
            environment_override_workers: self.environment_override_workers,
            effective_workers: saturating_u16(self.effective_workers),
            available_logical_cpus: saturating_u16(self.available_logical_cpus),
            memory_safe_workers: saturating_u16(self.memory_safe_workers),
            limiting_reason: self.limiting_reason,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodeIndexWorkerPlanErrorV1 {
    MalformedEnvironment {
        value: String,
    },
    NonUnicodeEnvironment,
    NoMemorySafeWorker {
        available_bytes: u64,
    },
    InvalidExactWorkerCount {
        workers: u16,
    },
    ExplicitWidthExceedsLogicalCpus {
        requested_workers: usize,
        available_logical_cpus: usize,
    },
    ExplicitWidthExceedsMemorySafe {
        requested_workers: usize,
        memory_safe_workers: usize,
    },
}

impl fmt::Display for CodeIndexWorkerPlanErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedEnvironment { value } => write!(
                formatter,
                "{INDEXING_WORKERS_ENV} must be a positive integer no greater than {}, got {value:?}",
                u16::MAX
            ),
            Self::NonUnicodeEnvironment => {
                write!(formatter, "{INDEXING_WORKERS_ENV} must be valid Unicode")
            }
            Self::NoMemorySafeWorker { available_bytes } => write!(
                formatter,
                "code-index worker admission needs {INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1} scratch bytes after non-worker headroom, but only {available_bytes} total bytes are available"
            ),
            Self::InvalidExactWorkerCount { workers } => write!(
                formatter,
                "exact code-index worker count must be positive, got {workers}"
            ),
            Self::ExplicitWidthExceedsLogicalCpus {
                requested_workers,
                available_logical_cpus,
            } => write!(
                formatter,
                "explicit code-index width {requested_workers} exceeds the {available_logical_cpus} available logical CPUs"
            ),
            Self::ExplicitWidthExceedsMemorySafe {
                requested_workers,
                memory_safe_workers,
            } => write!(
                formatter,
                "explicit code-index width {requested_workers} exceeds the memory-safe width {memory_safe_workers}"
            ),
        }
    }
}

impl std::error::Error for CodeIndexWorkerPlanErrorV1 {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodeIndexWorkerPlanInstallErrorV1 {
    Invalid(CodeIndexWorkerPlanErrorV1),
    PoolBuild {
        message: String,
    },
    BackgroundCpu(BackgroundCpuInstallErrorV1),
    ConflictingPlan {
        existing: CodeIndexWorkerStatusV1,
        requested: CodeIndexWorkerStatusV1,
    },
}

impl fmt::Display for CodeIndexWorkerPlanInstallErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(formatter),
            Self::PoolBuild { message } => {
                write!(
                    formatter,
                    "code-index worker pool could not start: {message}"
                )
            }
            Self::BackgroundCpu(error) => error.fmt(formatter),
            Self::ConflictingPlan {
                existing,
                requested,
            } => write!(
                formatter,
                "code-index worker plan is already installed as {existing:?}, not requested {requested:?}"
            ),
        }
    }
}

impl std::error::Error for CodeIndexWorkerPlanInstallErrorV1 {}

impl From<CodeIndexWorkerPlanErrorV1> for CodeIndexWorkerPlanInstallErrorV1 {
    fn from(error: CodeIndexWorkerPlanErrorV1) -> Self {
        Self::Invalid(error)
    }
}

struct InstalledCodeIndexWorkerRuntimeV1 {
    plan: CodeIndexWorkerPlanV1,
    pool: rayon::ThreadPool,
}

static WORKER_RUNTIME: OnceLock<InstalledCodeIndexWorkerRuntimeV1> = OnceLock::new();
static WORKER_RUNTIME_INSTALL: Mutex<()> = Mutex::new(());
static STANDALONE_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
static STANDALONE_POOL_INSTALL: Mutex<()> = Mutex::new(());

/// Automatic CPU target: use every logical CPU through eight, then floor half.
#[must_use]
pub fn indexing_worker_target(total_cores: usize) -> usize {
    let total = total_cores.max(1);
    if total <= 8 { total } else { total / 2 }
}

/// Worker width that still leaves the typed non-worker headroom on this
/// remaining authority. Source snapshots and other canonical resident
/// components are charged separately while a worker reservation is held, so
/// callers must not treat `available / INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1`
/// as an affordable width.
#[must_use]
pub fn memory_safe_worker_count(available_bytes: u64) -> usize {
    usize::try_from(
        worker_memory_budget_bytes(available_bytes) / INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
    )
    .unwrap_or(usize::MAX)
}

#[must_use]
fn worker_memory_headroom_bytes(available_bytes: u64) -> u64 {
    INDEX_NON_WORKER_MEMORY_HEADROOM_BYTES_V1
        .max(available_bytes / 4)
        .min(available_bytes)
}

#[must_use]
fn worker_memory_budget_bytes(available_bytes: u64) -> u64 {
    available_bytes.saturating_sub(worker_memory_headroom_bytes(available_bytes))
}

#[must_use]
pub fn worker_reservation_bytes(workers: usize) -> u64 {
    u64::try_from(workers)
        .unwrap_or(u64::MAX)
        .saturating_mul(INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1)
}

fn detected_cores() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn worker_plan_from(
    configured: CodeIndexWorkerSelectionV1,
    available_logical_cpus: usize,
    available_memory_bytes: u64,
    environment_override: Option<&str>,
) -> Result<CodeIndexWorkerPlanV1, CodeIndexWorkerPlanErrorV1> {
    let environment_override_workers = parse_environment_override(environment_override)?;
    let available_logical_cpus = available_logical_cpus.max(1);
    let (requested_workers, explicit, mut limiting_reason) =
        if let Some(workers) = environment_override_workers {
            (
                usize::from(workers),
                true,
                CodeIndexWorkerLimitingReasonV1::EnvironmentOverride,
            )
        } else {
            match configured {
                CodeIndexWorkerSelectionV1::Automatic {} => (
                    indexing_worker_target(available_logical_cpus),
                    false,
                    if available_logical_cpus <= 8 {
                        CodeIndexWorkerLimitingReasonV1::AutomaticAllCores
                    } else {
                        CodeIndexWorkerLimitingReasonV1::AutomaticHalfCores
                    },
                ),
                CodeIndexWorkerSelectionV1::Exact { workers } if workers > 0 => (
                    usize::from(workers),
                    true,
                    CodeIndexWorkerLimitingReasonV1::ConfiguredExact,
                ),
                CodeIndexWorkerSelectionV1::Exact { workers } => {
                    return Err(CodeIndexWorkerPlanErrorV1::InvalidExactWorkerCount { workers });
                }
            }
        };
    let memory_safe_workers = memory_safe_worker_count(available_memory_bytes);
    if memory_safe_workers == 0 {
        return Err(CodeIndexWorkerPlanErrorV1::NoMemorySafeWorker {
            available_bytes: available_memory_bytes,
        });
    }
    if explicit && requested_workers > available_logical_cpus {
        return Err(
            CodeIndexWorkerPlanErrorV1::ExplicitWidthExceedsLogicalCpus {
                requested_workers,
                available_logical_cpus,
            },
        );
    }
    if explicit && requested_workers > memory_safe_workers {
        return Err(CodeIndexWorkerPlanErrorV1::ExplicitWidthExceedsMemorySafe {
            requested_workers,
            memory_safe_workers,
        });
    }
    let effective_workers = requested_workers.min(memory_safe_workers);
    if effective_workers < requested_workers {
        limiting_reason = CodeIndexWorkerLimitingReasonV1::ResidentMemory;
    }
    Ok(CodeIndexWorkerPlanV1 {
        configured,
        environment_override_workers,
        requested_workers,
        effective_workers,
        available_logical_cpus,
        memory_safe_workers,
        memory_headroom_bytes: worker_memory_headroom_bytes(available_memory_bytes),
        limiting_reason,
        reservation_bytes: worker_reservation_bytes(effective_workers),
    })
}

fn parse_environment_override(
    environment_override: Option<&str>,
) -> Result<Option<u16>, CodeIndexWorkerPlanErrorV1> {
    environment_override
        .map(|raw| {
            raw.trim()
                .parse::<u16>()
                .ok()
                .filter(|workers| *workers > 0)
                .ok_or_else(|| CodeIndexWorkerPlanErrorV1::MalformedEnvironment {
                    value: raw.to_owned(),
                })
        })
        .transpose()
}

fn environment_override_value() -> Result<Option<String>, CodeIndexWorkerPlanErrorV1> {
    match std::env::var(INDEXING_WORKERS_ENV) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(CodeIndexWorkerPlanErrorV1::NonUnicodeEnvironment)
        }
    }
}

/// Preview the worker status without constructing a pool or installing any
/// process authority. `available_memory_bytes` must come from the caller's
/// canonical resident-memory authority (`limit - used`), never a second
/// estimator. Environment precedence and every typed refusal are identical to
/// [`install_worker_plan`].
pub fn preview_worker_plan(
    configured: CodeIndexWorkerSelectionV1,
    available_memory_bytes: u64,
) -> Result<CodeIndexWorkerStatusV1, CodeIndexWorkerPlanErrorV1> {
    let environment_override = environment_override_value()?;
    preview_worker_plan_from(
        configured,
        detected_cores(),
        available_memory_bytes,
        environment_override.as_deref(),
    )
}

fn preview_worker_plan_from(
    configured: CodeIndexWorkerSelectionV1,
    available_logical_cpus: usize,
    available_memory_bytes: u64,
    environment_override: Option<&str>,
) -> Result<CodeIndexWorkerStatusV1, CodeIndexWorkerPlanErrorV1> {
    worker_plan_from(
        configured,
        available_logical_cpus,
        available_memory_bytes,
        environment_override,
    )
    .map(CodeIndexWorkerPlanV1::status)
}

fn compare_installed_plan(
    existing: &CodeIndexWorkerPlanV1,
    requested: &CodeIndexWorkerPlanV1,
) -> Result<(), CodeIndexWorkerPlanInstallErrorV1> {
    if existing == requested {
        Ok(())
    } else {
        Err(CodeIndexWorkerPlanInstallErrorV1::ConflictingPlan {
            existing: existing.status(),
            requested: requested.status(),
        })
    }
}

fn record_plan(plan: CodeIndexWorkerPlanV1) {
    hotpath::gauge!("code_index_workers_requested").set(plan.requested_workers);
    hotpath::gauge!("code_index_workers_effective").set(plan.effective_workers);
    hotpath::gauge!("code_index_workers_memory_safe").set(plan.memory_safe_workers);
    hotpath::gauge!("code_index_workers_memory_headroom_bytes")
        .set(plan.memory_headroom_bytes as f64);
    hotpath::gauge!("code_index_workers_limiting_reason").set(match plan.limiting_reason {
        CodeIndexWorkerLimitingReasonV1::AutomaticAllCores => 1,
        CodeIndexWorkerLimitingReasonV1::AutomaticHalfCores => 2,
        CodeIndexWorkerLimitingReasonV1::ResidentMemory => 3,
        CodeIndexWorkerLimitingReasonV1::ConfiguredExact => 4,
        CodeIndexWorkerLimitingReasonV1::EnvironmentOverride => 5,
    });
    hotpath::gauge!("code_index_workers_reservation_bytes").set(plan.reservation_bytes as f64);
}

/// Install the process-resident plan before the first code-index build.
/// Repeating the byte-identical plan is idempotent; a second owner asking for
/// a different process-wide pool is refused.
pub fn install_worker_plan(
    configured: CodeIndexWorkerSelectionV1,
    available_memory_bytes: u64,
) -> Result<CodeIndexWorkerStatusV1, CodeIndexWorkerPlanInstallErrorV1> {
    // Planning and pool construction are one initialization transaction. This
    // prevents concurrent registrars from constructing duplicate large pools
    // from different instantaneous memory snapshots before the OnceLock wins.
    let _installation = WORKER_RUNTIME_INSTALL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let environment_override = environment_override_value()?;
    let environment_override_workers = parse_environment_override(environment_override.as_deref())?;
    if let Some(installed) = WORKER_RUNTIME.get()
        && installed.plan.configured == configured
        && installed.plan.environment_override_workers == environment_override_workers
    {
        record_plan(installed.plan);
        return Ok(installed.plan.status());
    }
    let requested = worker_plan_from(
        configured,
        detected_cores(),
        available_memory_bytes,
        environment_override.as_deref(),
    )?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(requested.effective_workers)
        .thread_name(|index| format!("tracedecay-index-{index}"))
        .build()
        .map_err(|error| CodeIndexWorkerPlanInstallErrorV1::PoolBuild {
            message: error.to_string(),
        })?;
    install_process_background_cpu(
        NonZeroUsize::new(requested.effective_workers).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(CodeIndexWorkerPlanInstallErrorV1::BackgroundCpu)?;
    match WORKER_RUNTIME.set(InstalledCodeIndexWorkerRuntimeV1 {
        plan: requested,
        pool,
    }) {
        Ok(()) => {
            record_plan(requested);
            Ok(requested.status())
        }
        Err(_) => {
            let Some(existing) = WORKER_RUNTIME.get() else {
                return Err(CodeIndexWorkerPlanInstallErrorV1::PoolBuild {
                    message: "worker runtime installation did not settle".to_owned(),
                });
            };
            compare_installed_plan(&existing.plan, &requested)?;
            record_plan(existing.plan);
            Ok(existing.plan.status())
        }
    }
}

/// Canonical runtime status for configuration/dashboard projection.
#[must_use]
pub fn installed_worker_status() -> Option<CodeIndexWorkerStatusV1> {
    WORKER_RUNTIME.get().map(|runtime| runtime.plan.status())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodeIndexParallelismErrorV1 {
    PoolBuild {
        message: String,
    },
    BackgroundCpuNotInstalled,
    /// A per-item unit fanned out on the indexing pool panicked. Per-file work
    /// runs over arbitrary user source, so a malformed input must be contained
    /// to its own unit and named, never allowed to unwind out of the pool and
    /// take an entire generation with it.
    WorkerPanic {
        index: usize,
        message: String,
    },
}

impl CodeIndexParallelismErrorV1 {
    /// Recover the panic message from a caught unwind payload, falling back to
    /// a stable label when the payload is not a string.
    #[must_use]
    pub fn from_panic_payload(index: usize, payload: &(dyn std::any::Any + Send)) -> Self {
        let message = payload
            .downcast_ref::<&'static str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        Self::WorkerPanic { index, message }
    }
}

impl fmt::Display for CodeIndexParallelismErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PoolBuild { message } => {
                write!(
                    formatter,
                    "code-index worker pool is unavailable: {message}"
                )
            }
            Self::BackgroundCpuNotInstalled => {
                write!(
                    formatter,
                    "process background CPU authority is not installed"
                )
            }
            Self::WorkerPanic { index, message } => {
                write!(
                    formatter,
                    "code-index worker unit {index} panicked: {message}"
                )
            }
        }
    }
}

impl std::error::Error for CodeIndexParallelismErrorV1 {}

/// 0 means "use the configured host width".
static FORCED_WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Indexing width callers should fan out to. A width below 2 means "run
/// inline".
#[must_use]
pub fn indexing_workers() -> usize {
    match FORCED_WORKERS.load(Ordering::Relaxed) {
        0 => WORKER_RUNTIME.get().map_or_else(
            || indexing_worker_target(detected_cores()),
            |runtime| runtime.plan.effective_workers,
        ),
        forced => forced,
    }
}

/// Force the indexing width for an equivalence test.
///
/// Width is sizing policy, never semantics: the same inputs must produce the
/// same generation bytes at any width. This exists so one test process can
/// build a fixture at width 1 and at full width and compare the sealed
/// digests directly. It is not a supported runtime control — production sizing
/// comes from [`indexing_workers`].
#[doc(hidden)]
pub fn force_indexing_workers_for_test(workers: usize) {
    FORCED_WORKERS.store(workers.max(1), Ordering::Relaxed);
}

/// Restore production sizing after [`force_indexing_workers_for_test`].
#[doc(hidden)]
pub fn clear_forced_indexing_workers_for_test() {
    FORCED_WORKERS.store(0, Ordering::Relaxed);
}

/// Run one active work unit under the process background CPU authority.
/// Standalone callers without an installed daemon plan run directly.
pub fn with_background_cpu_permits<R>(requested_units: usize, operation: impl FnOnce() -> R) -> R {
    if let Some(authority) = process_background_cpu() {
        return authority.with_permits(requested_units, operation);
    }
    operation()
}

/// One-unit convenience for ordinary index/session preparation work.
pub fn with_background_cpu_permit<R>(operation: impl FnOnce() -> R) -> R {
    with_background_cpu_permits(1, operation)
}

/// Run `operation` on the configured indexing pool.
///
/// CPU admission happens inside each active parallel work unit through
/// [`with_background_cpu_permit`] or [`with_background_cpu_permits`], allowing
/// indexing, semantic inference, and session preparation to share idle width.
/// A standalone caller without registration shares one process-wide automatic
/// pool. Building one all-core pool per request oversubscribes concurrent
/// tests and profiling harnesses, which can turn bounded parser work into
/// false timeout/unsupported-document results.
#[hotpath::measure(label = "code_index.workers.install")]
pub fn install<R, F>(operation: F) -> Result<R, CodeIndexParallelismErrorV1>
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    hotpath::gauge!("code_index_worker_count").set(indexing_workers());
    if let Some(runtime) = WORKER_RUNTIME.get() {
        let authority = process_background_cpu()
            .ok_or(CodeIndexParallelismErrorV1::BackgroundCpuNotInstalled)?;
        return Ok(authority.with_yielded_permits(|| runtime.pool.install(operation)));
    }
    let pool = standalone_pool()?;
    Ok(pool.install(operation))
}

#[hotpath::measure(label = "code_index.workers.standalone_pool")]
fn standalone_pool() -> Result<&'static rayon::ThreadPool, CodeIndexParallelismErrorV1> {
    if let Some(pool) = STANDALONE_POOL.get() {
        return Ok(pool);
    }
    let _installation =
        STANDALONE_POOL_INSTALL
            .lock()
            .map_err(|_| CodeIndexParallelismErrorV1::PoolBuild {
                message: "standalone code-index worker pool installation lock is poisoned"
                    .to_owned(),
            })?;
    if let Some(pool) = STANDALONE_POOL.get() {
        return Ok(pool);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(indexing_worker_target(detected_cores()))
        .thread_name(|index| format!("tracedecay-index-standalone-{index}"))
        .build()
        .map_err(|error| CodeIndexParallelismErrorV1::PoolBuild {
            message: error.to_string(),
        })?;
    STANDALONE_POOL
        .set(pool)
        .map_err(|_| CodeIndexParallelismErrorV1::PoolBuild {
            message: "standalone code-index worker pool installation raced".to_owned(),
        })?;
    STANDALONE_POOL
        .get()
        .ok_or_else(|| CodeIndexParallelismErrorV1::PoolBuild {
            message: "standalone code-index worker pool installation did not settle".to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::configuration::CodeIndexWorkerSelectionV1;

    #[test]
    fn standalone_install_reuses_one_process_pool() {
        let first = standalone_pool().expect("first standalone pool");
        let second = standalone_pool().expect("reused standalone pool");

        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn automatic_width_uses_small_hosts_and_half_of_large_hosts() {
        assert_eq!(indexing_worker_target(1), 1);
        assert_eq!(indexing_worker_target(4), 4);
        assert_eq!(indexing_worker_target(8), 8);
        assert_eq!(indexing_worker_target(20), 10);
        assert_eq!(indexing_worker_target(128), 64);
    }

    #[test]
    fn exact_selection_uses_the_configured_core_count() {
        let plan = worker_plan_from(
            CodeIndexWorkerSelectionV1::Exact { workers: 7 },
            8,
            32 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
            None,
        )
        .expect("exact selection");

        assert_eq!(plan.requested_workers, 7);
        assert_eq!(plan.effective_workers, 7);
        assert_eq!(
            plan.limiting_reason,
            CodeIndexWorkerLimitingReasonV1::ConfiguredExact
        );
    }

    #[test]
    fn exact_selection_cannot_oversubscribe_logical_cpus() {
        assert_eq!(
            worker_plan_from(
                CodeIndexWorkerSelectionV1::Exact { workers: 17 },
                8,
                32 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
                None,
            ),
            Err(
                CodeIndexWorkerPlanErrorV1::ExplicitWidthExceedsLogicalCpus {
                    requested_workers: 17,
                    available_logical_cpus: 8,
                }
            )
        );
    }

    #[test]
    fn zero_exact_selection_is_a_typed_refusal() {
        assert_eq!(
            worker_plan_from(
                CodeIndexWorkerSelectionV1::Exact { workers: 0 },
                8,
                8 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
                None,
            ),
            Err(CodeIndexWorkerPlanErrorV1::InvalidExactWorkerCount { workers: 0 })
        );
    }

    #[test]
    fn environment_override_has_highest_precedence() {
        let plan = worker_plan_from(
            CodeIndexWorkerSelectionV1::Exact { workers: 3 },
            128,
            64 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
            Some("11"),
        )
        .expect("environment override");

        assert_eq!(plan.requested_workers, 11);
        assert_eq!(plan.effective_workers, 11);
        assert_eq!(plan.environment_override_workers, Some(11));
        assert_eq!(
            plan.limiting_reason,
            CodeIndexWorkerLimitingReasonV1::EnvironmentOverride
        );
    }

    #[test]
    fn malformed_environment_override_is_a_typed_refusal() {
        assert!(matches!(
            worker_plan_from(
                CodeIndexWorkerSelectionV1::Automatic {},
                8,
                8 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
                Some("many"),
            ),
            Err(CodeIndexWorkerPlanErrorV1::MalformedEnvironment { .. })
        ));
        assert!(matches!(
            worker_plan_from(
                CodeIndexWorkerSelectionV1::Automatic {},
                8,
                8 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
                Some("0"),
            ),
            Err(CodeIndexWorkerPlanErrorV1::MalformedEnvironment { .. })
        ));
    }

    #[test]
    fn automatic_width_is_capped_by_the_resident_budget() {
        let plan = worker_plan_from(
            CodeIndexWorkerSelectionV1::Automatic {},
            128,
            12 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
            None,
        )
        .expect("memory-capped automatic selection");

        assert_eq!(plan.requested_workers, 64);
        assert_eq!(plan.memory_safe_workers, 8);
        assert_eq!(plan.effective_workers, 8);
        assert_eq!(
            plan.limiting_reason,
            CodeIndexWorkerLimitingReasonV1::ResidentMemory
        );
    }

    #[test]
    fn default_authority_preserves_source_and_component_headroom() {
        let available = 6 * 1024 * 1024 * 1024;
        let plan = worker_plan_from(
            CodeIndexWorkerSelectionV1::Automatic {},
            96,
            available,
            None,
        )
        .expect("default-memory automatic selection");

        assert_eq!(plan.requested_workers, 48);
        assert_eq!(plan.memory_safe_workers, 36);
        assert_eq!(plan.effective_workers, 36);
        assert_eq!(plan.memory_headroom_bytes, 1536 * 1024 * 1024);
        assert_eq!(
            plan.reservation_bytes + plan.memory_headroom_bytes,
            available
        );
    }

    #[test]
    fn explicit_width_above_the_resident_budget_is_refused() {
        let error = worker_plan_from(
            CodeIndexWorkerSelectionV1::Exact { workers: 9 },
            128,
            12 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
            None,
        )
        .expect_err("unsafe exact width must be refused");

        assert_eq!(
            error,
            CodeIndexWorkerPlanErrorV1::ExplicitWidthExceedsMemorySafe {
                requested_workers: 9,
                memory_safe_workers: 8,
            }
        );
    }

    #[test]
    fn worker_reservation_scales_linearly_with_the_documented_rss_budget() {
        assert_eq!(
            worker_reservation_bytes(1),
            INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1
        );
        assert_eq!(
            worker_reservation_bytes(16),
            16 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1
        );
        assert_eq!(memory_safe_worker_count(6 * 1024 * 1024 * 1024), 36);
        assert_eq!(
            worker_memory_headroom_bytes(6 * 1024 * 1024 * 1024),
            1536 * 1024 * 1024
        );
        assert_eq!(
            worker_memory_budget_bytes(6 * 1024 * 1024 * 1024)
                + worker_memory_headroom_bytes(6 * 1024 * 1024 * 1024),
            6 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn plan_identity_accepts_repetition_and_refuses_conflict() {
        let plan = worker_plan_from(
            CodeIndexWorkerSelectionV1::Automatic {},
            20,
            20 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
            None,
        )
        .expect("automatic plan");

        assert_eq!(compare_installed_plan(&plan, &plan), Ok(()));
        let conflicting = worker_plan_from(
            CodeIndexWorkerSelectionV1::Exact { workers: 4 },
            20,
            20 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
            None,
        )
        .expect("exact plan");
        assert!(matches!(
            compare_installed_plan(&plan, &conflicting),
            Err(CodeIndexWorkerPlanInstallErrorV1::ConflictingPlan { .. })
        ));
    }

    #[test]
    fn preview_matches_install_derivation_without_installing_runtime() {
        let runtime_before = installed_worker_status();
        let available = 20 * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1;
        let preview = preview_worker_plan_from(
            CodeIndexWorkerSelectionV1::Automatic {},
            20,
            available,
            None,
        )
        .expect("worker-plan preview");
        let installation_derivation = worker_plan_from(
            CodeIndexWorkerSelectionV1::Automatic {},
            20,
            available,
            None,
        )
        .expect("worker-plan installation derivation")
        .status();

        assert_eq!(preview, installation_derivation);
        assert_eq!(installed_worker_status(), runtime_before);
    }
}
