//! Indexing parallelism: bounded race to idle with serving capacity reserved.
//!
//! Indexing is a batch job with a finish line, but each concurrent parser and
//! artifact builder retains substantially more than its source bytes. Running
//! at every non-serving core multiplied that transient state to 90 workers on
//! a 96-core host and forced the daemon into swap. The shared pool therefore
//! races to idle within a memory-safe default ceiling while preserving the
//! barrier-free pipeline. See `docs/SERVING-PATH-PERFORMANCE.md` Principle 2.
//!
//! Interactive latency is protected by a *reservation*, not by throttling:
//! the indexing pool is sized to `min(total_cores - reserve, 8)`, so the
//! daemon's request runtime always has runnable CPU and parser memory cannot
//! scale with a large host's full logical-core count. The reserve is deliberately small
//! (`max(2, cores/16)`) because serving work is latency-bound, not
//! throughput-bound — a handful of cores answers reads at full speed.
//!
//! Everything here is sizing policy only. It never changes what is computed,
//! so generation bytes and digests are identical at any width.

use std::fmt;
use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};

/// Floor on the serving reservation. Two cores keep the tokio request
/// workers and the store's blocking pool runnable on any host.
const MIN_SERVING_RESERVED_CORES: usize = 2;

/// Fraction of a large host handed to serving: `cores / 16` (6 of 96).
const SERVING_RESERVE_DIVISOR: usize = 16;

/// Default ceiling for concurrent parse/artifact workers. Each worker can hold
/// source, syntax trees, extraction rows, chunks, and graph edges at once, so
/// machine-width fan-out is a memory multiplier rather than a free speedup.
const DEFAULT_MAX_INDEXING_WORKERS: usize = 8;

/// Operator override for the indexing width, for hosts where memory rather
/// than CPU is the binding constraint (each worker holds a tree-sitter
/// parser). Values below 1 are ignored; the daemon's canonical CPU-thread
/// ceiling remains an upper bound.
const INDEXING_WORKERS_ENV: &str = "TRACEDECAY_INDEX_WORKERS";

/// Daemon-owned upper bound for this dedicated pool. Zero means the process is
/// a one-shot caller and has not installed a daemon CPU budget.
static DAEMON_WORKER_CEILING: AtomicUsize = AtomicUsize::new(0);
static CONFIGURED_INDEXING_WORKERS: OnceLock<usize> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonWorkerCeilingInstallError {
    PoolAlreadyInitialized { workers: usize },
    ConflictingCeiling { existing: usize, requested: usize },
}

impl fmt::Display for DaemonWorkerCeilingInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PoolAlreadyInitialized { workers } => write!(
                formatter,
                "indexing pool width was already initialized at {workers} workers"
            ),
            Self::ConflictingCeiling {
                existing,
                requested,
            } => write!(
                formatter,
                "daemon worker ceiling is already {existing}, not requested {requested}"
            ),
        }
    }
}

/// Cores held back from indexing so interactive requests never wait for a
/// free CPU. Never reserves the whole machine.
#[must_use]
pub fn serving_reserved_cores(total_cores: usize) -> usize {
    let total = total_cores.max(1);
    let reserve = MIN_SERVING_RESERVED_CORES.max(total / SERVING_RESERVE_DIVISOR);
    reserve.min(total.saturating_sub(1))
}

/// Indexing width for a host with `total_cores` logical CPUs.
#[must_use]
pub fn indexing_worker_target(total_cores: usize) -> usize {
    let total = total_cores.max(1);
    total
        .saturating_sub(serving_reserved_cores(total))
        .clamp(1, DEFAULT_MAX_INDEXING_WORKERS)
}

fn detected_cores() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

fn honor_daemon_worker_ceiling(requested: usize, daemon_ceiling: Option<usize>) -> usize {
    daemon_ceiling.map_or(requested, |ceiling| requested.min(ceiling.max(1)))
}

/// Installs the daemon's canonical CPU-thread ceiling before the indexing pool
/// is first used. Repeating the same value is idempotent; a conflicting second
/// owner is refused.
pub fn install_daemon_worker_ceiling(
    threads: usize,
) -> Result<(), DaemonWorkerCeilingInstallError> {
    let threads = threads.max(1);
    if let Some(&workers) = CONFIGURED_INDEXING_WORKERS.get() {
        return Err(DaemonWorkerCeilingInstallError::PoolAlreadyInitialized { workers });
    }
    match DAEMON_WORKER_CEILING.compare_exchange(0, threads, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Ok(()),
        Err(existing) if existing == threads => Ok(()),
        Err(existing) => Err(DaemonWorkerCeilingInstallError::ConflictingCeiling {
            existing,
            requested: threads,
        }),
    }
}

/// Host width: the operator override if set, otherwise the reservation
/// target, then clamped to the daemon's canonical ceiling when installed.
/// Fixed for the life of the process; this is what the pool is built at.
fn configured_indexing_workers() -> usize {
    *CONFIGURED_INDEXING_WORKERS.get_or_init(|| {
        let requested = std::env::var(INDEXING_WORKERS_ENV)
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|workers| *workers >= 1)
            .unwrap_or_else(|| indexing_worker_target(detected_cores()));
        let daemon_ceiling = match DAEMON_WORKER_CEILING.load(Ordering::Acquire) {
            0 => None,
            ceiling => Some(ceiling),
        };
        honor_daemon_worker_ceiling(requested, daemon_ceiling)
    })
}

/// 0 means "use the configured host width".
static FORCED_WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Indexing width callers should fan out to. A width below 2 means "run
/// inline".
#[must_use]
pub fn indexing_workers() -> usize {
    match FORCED_WORKERS.load(Ordering::Relaxed) {
        0 => configured_indexing_workers(),
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

/// The process-wide indexing pool. Always built, even at width 1: the pool
/// is what CONFINES indexing to its reservation. Without it, the nested
/// chunk-level `par_iter` sweeps would land on rayon's global pool, which is
/// sized to every logical CPU — the reservation would leak exactly where the
/// work is heaviest.
fn indexing_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(configured_indexing_workers())
            .thread_name(|index| format!("tracedecay-index-{index}"))
            .build()
            .ok()
    })
    .as_ref()
}

/// Run `operation` on the reserved-width indexing pool.
///
/// Nested calls (chunking fanning out inside a per-file extraction) stay on
/// the same pool, so the reservation holds for the whole pipeline rather than
/// being multiplied by pipeline depth. Falls back to running inline only if
/// the pool could not be built at all.
pub fn install<R, F>(operation: F) -> R
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    match indexing_pool() {
        Some(pool) => pool.install(operation),
        None => operation(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_scales_with_the_host_but_never_takes_it_all() {
        assert_eq!(serving_reserved_cores(1), 0);
        assert_eq!(serving_reserved_cores(2), 1);
        assert_eq!(serving_reserved_cores(4), 2);
        assert_eq!(serving_reserved_cores(16), 2);
        assert_eq!(serving_reserved_cores(96), 6);
        assert_eq!(serving_reserved_cores(128), 8);
    }

    #[test]
    fn indexing_stays_within_the_default_memory_safe_width() {
        assert_eq!(indexing_worker_target(1), 1);
        assert_eq!(indexing_worker_target(2), 1);
        assert_eq!(indexing_worker_target(4), 2);
        assert_eq!(indexing_worker_target(16), 8);
        assert_eq!(indexing_worker_target(96), 8);
        assert_eq!(indexing_worker_target(128), 8);
    }

    #[test]
    fn daemon_ceiling_clamps_requested_width() {
        assert_eq!(honor_daemon_worker_ceiling(8, None), 8);
        assert_eq!(honor_daemon_worker_ceiling(90, Some(16)), 16);
        assert_eq!(honor_daemon_worker_ceiling(8, Some(16)), 8);
        assert_eq!(honor_daemon_worker_ceiling(8, Some(4)), 4);
    }

    #[test]
    fn every_host_keeps_at_least_one_indexing_worker_and_one_reserved_core() {
        for cores in 1..=512usize {
            let workers = indexing_worker_target(cores);
            assert!(workers >= 1, "cores={cores} left no indexing worker");
            assert!(workers <= cores, "cores={cores} oversubscribed");
            assert!(
                workers <= DEFAULT_MAX_INDEXING_WORKERS,
                "cores={cores} exceeded the default memory-safe width"
            );
            if cores > 1 {
                assert!(
                    serving_reserved_cores(cores) >= 1,
                    "cores={cores} reserved nothing for serving"
                );
            }
        }
    }

    #[test]
    fn install_runs_the_operation_exactly_once() {
        let value = install(|| 7_usize);
        assert_eq!(value, 7);
    }
}
