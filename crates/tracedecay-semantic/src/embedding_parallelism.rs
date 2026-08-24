//! Embedding width: bounded race to idle on the code-index CPU pool.
//!
//! Embedding a published code generation is batch work with a finish line, so
//! it gets the same treatment as extraction (`docs/SERVING-PATH-PERFORMANCE.md`
//! Principle 2): use a bounded, barrier-free pool, finish, and get out of the
//! way. Semantic sessions have their own resident model and intra-op limits,
//! while sharing the already admitted code-index Rayon CPU budget.
//!
//! Two knobs make up the width, and they are *not* interchangeable:
//!
//! - **Intra-op threads** are how many CPUs ONNX Runtime uses inside one
//!   tensor invocation. The artifact declares the maximum, and the installed
//!   process CPU authority may narrow it on smaller hosts so native work never
//!   exceeds the admitted background budget.
//! - **Session width** is how many independent batches are in flight at once.
//!   Each batch is a separate invocation of the same graph over the same
//!   tensor shape, so results are bit-identical at any width. This is the
//!   knob that scales within the code-index CPU budget.
//!
//! Session fan-out is sizing policy only: for one admitted intra-op plan,
//! vector bytes are identical at width 1 and at full width. Native intra-op
//! width is separately bounded by the process authority before a session is
//! opened.

/// Operator override for concurrently embedding sessions, for hosts where
/// memory rather than CPU binds. Values below 1 are ignored.
const EMBED_SESSIONS_ENV: &str = "TRACEDECAY_EMBED_SESSIONS";

/// Never open more concurrent sessions than this regardless of host width:
/// each session is a full resident copy of the model graph.
const MAX_EMBEDDING_SESSIONS: usize = 16;

/// Maximum intra-op threads requested by the shipped default configuration.
pub const DEFAULT_INTRA_THREADS: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddingSessionLimitingReasonV1 {
    SharedCodeIndexCpuBudget,
    EnvironmentOverride,
    ConfiguredMaximum,
    ResidentSessionLimit,
}

fn detected_cores() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

fn env_width(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|width| *width >= 1)
}

/// Shared code-index CPU budget. Semantic work executes on that same pool, so
/// its ONNX session fan-out cannot add a second independent half-host pool.
#[must_use]
pub fn embedding_cpu_target(total_cores: usize) -> usize {
    tracedecay_code_index::parallelism::indexing_worker_target(total_cores)
}

fn installed_cpu_budget() -> usize {
    tracedecay_code_index::parallelism::installed_worker_status()
        .map_or_else(
            || embedding_cpu_target(detected_cores()),
            |status| usize::from(status.effective_workers),
        )
        .max(1)
}

fn embedding_intra_threads_for(shared_cpu_budget: usize, configured_threads: u32) -> usize {
    (configured_threads as usize)
        .max(1)
        .min(shared_cpu_budget.max(1))
}

/// Native ONNX intra-op demand admitted against the installed process CPU
/// authority. The artifact value is a ceiling, not permission to oversubscribe
/// a narrower host.
#[must_use]
pub(crate) fn embedding_intra_threads(configured_threads: u32) -> usize {
    embedding_intra_threads_for(installed_cpu_budget(), configured_threads)
}

/// Concurrent embedding sessions for a host with `total_cores` logical CPUs,
/// given the intra-op thread ceiling admitted by the artifact.
///
/// `sessions * intra_threads` is held to the semantic CPU budget. Session-pool
/// resident-memory admission remains the independent memory authority.
#[must_use]
pub fn embedding_session_width_for(
    total_cores: usize,
    intra_threads: u32,
    configured_max_sessions: u32,
) -> usize {
    embedding_session_plan_for(
        embedding_cpu_target(total_cores),
        intra_threads,
        configured_max_sessions,
        None,
    )
    .0
}

fn embedding_session_plan_for(
    shared_cpu_budget: usize,
    intra_threads: u32,
    configured_max_sessions: u32,
    environment_override: Option<usize>,
) -> (usize, EmbeddingSessionLimitingReasonV1) {
    let shared_cpu_budget = shared_cpu_budget.max(1);
    let intra = embedding_intra_threads_for(shared_cpu_budget, intra_threads);
    let configured = (configured_max_sessions as usize).max(1);
    let cpu_safe_sessions = (shared_cpu_budget / intra).max(1);
    let requested = environment_override.unwrap_or(configured);
    let effective = requested
        .min(configured)
        .min(MAX_EMBEDDING_SESSIONS)
        .min(cpu_safe_sessions);
    let limiting_reason = if effective < requested {
        if effective == cpu_safe_sessions {
            EmbeddingSessionLimitingReasonV1::SharedCodeIndexCpuBudget
        } else if effective == configured {
            EmbeddingSessionLimitingReasonV1::ConfiguredMaximum
        } else {
            EmbeddingSessionLimitingReasonV1::ResidentSessionLimit
        }
    } else if environment_override.is_some() {
        EmbeddingSessionLimitingReasonV1::EnvironmentOverride
    } else {
        EmbeddingSessionLimitingReasonV1::ConfiguredMaximum
    };
    (effective, limiting_reason)
}

/// Concurrent embedding sessions on this host, honouring the operator
/// override. Always at least 1.
#[must_use]
pub fn embedding_session_width(intra_threads: u32, configured_max_sessions: u32) -> usize {
    let configured = (configured_max_sessions as usize).max(1);
    let shared_cpu_budget = installed_cpu_budget();
    let intra = embedding_intra_threads_for(shared_cpu_budget, intra_threads);
    let cpu_safe_sessions = (shared_cpu_budget / intra).max(1);
    let environment_override = env_width(EMBED_SESSIONS_ENV);
    let requested = environment_override.unwrap_or(configured);
    let (effective, limiting_reason) = embedding_session_plan_for(
        shared_cpu_budget,
        intra_threads,
        configured_max_sessions,
        environment_override,
    );
    hotpath::gauge!("semantic_embedding_sessions_requested").set(requested);
    hotpath::gauge!("semantic_embedding_sessions_effective").set(effective);
    hotpath::gauge!("semantic_embedding_sessions_cpu_safe").set(cpu_safe_sessions);
    hotpath::gauge!("semantic_embedding_sessions_limiting_reason").set(match limiting_reason {
        EmbeddingSessionLimitingReasonV1::SharedCodeIndexCpuBudget => 1,
        EmbeddingSessionLimitingReasonV1::EnvironmentOverride => 2,
        EmbeddingSessionLimitingReasonV1::ConfiguredMaximum => 3,
        EmbeddingSessionLimitingReasonV1::ResidentSessionLimit => 4,
    });
    effective
}

/// Session-pool sizing that lets the derived concurrency actually be used.
///
/// The pool's own memory ceiling still applies; this only stops the pool from
/// becoming the binding constraint before the reservation is. The extra slot
/// keeps an interactive query session warm while a rebuild holds the
/// projection sessions.
#[must_use]
pub fn embedding_pool_sessions(intra_threads: u32, configured_max_sessions: u32) -> usize {
    embedding_session_width(intra_threads, configured_max_sessions).saturating_add(1)
}

/// Host-derived default for the configuration's concurrent-session ceiling.
///
/// Configuration stays authoritative — an operator who pins a lower value
/// keeps it. This only changes what "unset" means, from "one session on every
/// host" to "as many as the serving reservation leaves room for".
#[must_use]
pub fn default_max_concurrent_sessions() -> u32 {
    default_max_concurrent_sessions_for(detected_cores())
}

#[must_use]
pub fn default_max_concurrent_sessions_for(total_cores: usize) -> u32 {
    let width = embedding_session_width_for(
        total_cores,
        DEFAULT_INTRA_THREADS,
        MAX_EMBEDDING_SESSIONS as u32,
    );
    u32::try_from(width.max(1)).unwrap_or(1)
}

/// Run `operation` on the shared, canonically bounded code-index pool.
pub fn install<R, F>(operation: F) -> Result<R, String>
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    tracedecay_code_index::parallelism::install(operation).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tracedecay_runtime_core::background_cpu::install_process_background_cpu;

    use super::*;

    #[test]
    fn session_width_uses_the_shared_code_index_cpu_budget() {
        assert_eq!(embedding_session_width_for(96, 4, 64), 12);
        assert_eq!(embedding_session_width_for(16, 4, 64), 2);
        assert_eq!(embedding_session_width_for(4, 4, 64), 1);
        assert_eq!(embedding_session_width_for(1, 4, 64), 1);
    }

    #[test]
    fn configured_ceiling_is_never_exceeded() {
        assert_eq!(embedding_session_width_for(96, 4, 1), 1);
        assert_eq!(embedding_session_width_for(96, 4, 2), 2);
    }

    #[test]
    fn forced_sessions_are_clamped_to_the_shared_cpu_budget() {
        assert_eq!(
            embedding_session_plan_for(8, 4, 64, Some(12)),
            (
                2,
                EmbeddingSessionLimitingReasonV1::SharedCodeIndexCpuBudget
            )
        );
        assert_eq!(
            embedding_session_plan_for(64, 4, 1, Some(12)),
            (1, EmbeddingSessionLimitingReasonV1::ConfiguredMaximum)
        );
    }

    #[test]
    fn wider_intra_threads_narrow_the_session_width() {
        assert_eq!(embedding_session_width_for(96, 1, 64), 16);
        assert_eq!(embedding_session_width_for(96, 32, 64), 1);
        assert_eq!(embedding_session_width_for(96, 128, 64), 1);
    }

    #[test]
    fn low_width_native_thread_demand_fits_the_shared_cpu_authority() {
        for shared_cpu_budget in 1..=3usize {
            let intra_threads =
                embedding_intra_threads_for(shared_cpu_budget, DEFAULT_INTRA_THREADS);
            let (sessions, _) = embedding_session_plan_for(
                shared_cpu_budget,
                DEFAULT_INTRA_THREADS,
                MAX_EMBEDDING_SESSIONS as u32,
                None,
            );

            assert_eq!(intra_threads, shared_cpu_budget);
            assert!(sessions * intra_threads <= shared_cpu_budget);
        }
    }

    #[test]
    fn semantic_work_uses_the_shared_code_index_cpu_budget() {
        assert_eq!(embedding_cpu_target(8), 8);
        assert_eq!(embedding_cpu_target(20), 10);
        assert_eq!(embedding_cpu_target(128), 64);
        assert_eq!(install(|| 17usize).expect("semantic pool"), 17);
        assert_eq!(
            install(|| install(|| 19usize)).expect("outer shared operation"),
            Ok(19),
            "nested shared-pool work must reuse admission instead of deadlocking"
        );
    }

    #[test]
    fn concurrent_index_and_semantic_units_share_width_and_both_progress() {
        let authority =
            install_process_background_cpu(NonZeroUsize::new(4).expect("nonzero background width"))
                .expect("background CPU authority");
        let maximum = Arc::new(AtomicUsize::new(0));
        let index_completed = Arc::new(AtomicUsize::new(0));
        let semantic_completed = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(7));
        let run = |semantic: bool| {
            let authority = Arc::clone(&authority);
            let maximum = Arc::clone(&maximum);
            let index_completed = Arc::clone(&index_completed);
            let semantic_completed = Arc::clone(&semantic_completed);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                let operation = || {
                    maximum.fetch_max(authority.active_units(), Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(5));
                    if semantic {
                        semantic_completed.fetch_add(1, Ordering::SeqCst);
                    } else {
                        index_completed.fetch_add(1, Ordering::SeqCst);
                    }
                };
                if semantic {
                    install(|| {
                        tracedecay_code_index::parallelism::with_background_cpu_permits(
                            DEFAULT_INTRA_THREADS as usize,
                            operation,
                        );
                    })
                    .expect("semantic shared operation");
                } else {
                    tracedecay_code_index::parallelism::install(|| {
                        tracedecay_code_index::parallelism::with_background_cpu_permit(operation);
                    })
                    .expect("code-index shared operation");
                }
            })
        };
        let workers = [
            run(false),
            run(true),
            run(false),
            run(true),
            run(false),
            run(false),
        ];
        start.wait();
        for worker in workers {
            worker.join().expect("background CPU operation");
        }

        assert_eq!(maximum.load(Ordering::SeqCst), authority.width().get());
        assert_eq!(index_completed.load(Ordering::SeqCst), 4);
        assert_eq!(semantic_completed.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn every_host_keeps_at_least_one_session() {
        for cores in 1..=256usize {
            assert!(embedding_session_width_for(cores, 4, 64) >= 1);
            assert!(default_max_concurrent_sessions_for(cores) >= 1);
        }
    }
}
