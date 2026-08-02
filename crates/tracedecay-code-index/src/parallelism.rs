//! Indexing parallelism: race to idle, reserve a slice for serving.
//!
//! Indexing is a batch job with a finish line. The cheapest way to keep the
//! interference window short is to run it at full machine width and finish,
//! not to pace it down and stay in the way for longer. See
//! `docs/SERVING-PATH-PERFORMANCE.md` Principle 2.
//!
//! Interactive latency is protected by a *reservation*, not by throttling:
//! the indexing pool is sized to `total_cores - reserve`, so the daemon's
//! request runtime always has runnable CPU even while a full reindex is
//! saturating everything else. The reserve is deliberately small
//! (`max(2, cores/16)`) because serving work is latency-bound, not
//! throughput-bound — a handful of cores answers reads at full speed.
//!
//! Everything here is sizing policy only. It never changes what is computed,
//! so generation bytes and digests are identical at any width.

use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};

/// Floor on the serving reservation. Two cores keep the tokio request
/// workers and the store's blocking pool runnable on any host.
const MIN_SERVING_RESERVED_CORES: usize = 2;

/// Fraction of a large host handed to serving: `cores / 16` (6 of 96).
const SERVING_RESERVE_DIVISOR: usize = 16;

/// Operator override for the indexing width, for hosts where memory rather
/// than CPU is the binding constraint (each worker holds a tree-sitter
/// parser). Values below 1 are ignored.
const INDEXING_WORKERS_ENV: &str = "TRACEDECAY_INDEX_WORKERS";

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
        .max(1)
}

fn detected_cores() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// Host width: the operator override if set, otherwise the reservation
/// target. Fixed for the life of the process; this is what the pool is
/// built at.
fn configured_indexing_workers() -> usize {
    static CONFIGURED: OnceLock<usize> = OnceLock::new();
    *CONFIGURED.get_or_init(|| {
        std::env::var(INDEXING_WORKERS_ENV)
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|workers| *workers >= 1)
            .unwrap_or_else(|| indexing_worker_target(detected_cores()))
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

/// The process-wide indexing pool, or `None` when the host is narrow enough
/// that the caller should just run inline.
fn indexing_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let workers = configured_indexing_workers();
        if workers < 2 {
            return None;
        }
        rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
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
/// being multiplied by pipeline depth. Falls back to running inline when no
/// pool exists.
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
    fn indexing_races_to_idle_on_wide_hosts() {
        assert_eq!(indexing_worker_target(1), 1);
        assert_eq!(indexing_worker_target(2), 1);
        assert_eq!(indexing_worker_target(4), 2);
        assert_eq!(indexing_worker_target(16), 14);
        assert_eq!(indexing_worker_target(96), 90);
        assert_eq!(indexing_worker_target(128), 120);
    }

    #[test]
    fn every_host_keeps_at_least_one_indexing_worker_and_one_reserved_core() {
        for cores in 1..=512usize {
            let workers = indexing_worker_target(cores);
            assert!(workers >= 1, "cores={cores} left no indexing worker");
            assert!(workers <= cores, "cores={cores} oversubscribed");
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
