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

use tracedecay_semantic_contracts::{
    DEFAULT_SEMANTIC_RESIDENT_BYTES, MAX_SEMANTIC_RESIDENT_BYTES, SemanticResourceCeilings,
};

/// Operator override for concurrently embedding sessions, for hosts where
/// memory rather than CPU binds. Values below 1 are ignored.
const EMBED_SESSIONS_ENV: &str = "TRACEDECAY_EMBED_SESSIONS";

/// Share of process resident admission reserved for semantic sessions by
/// default. The remainder stays available to graph/text generations, queries,
/// and model-load transients.
const DEFAULT_RESIDENT_FRACTION_DENOMINATOR: u64 = 8;

/// Intra-op width at which independent sessions remain the preferred way to
/// fill the shared CPU authority. The execution planner only widens a session
/// beyond this when the admitted resident-session capacity is the binding
/// constraint.
const BASELINE_INTRA_THREADS: usize = 4;

/// Maximum intra-op threads requested by the shipped default configuration.
/// Smaller hosts and hosts able to retain more independent sessions are
/// narrowed by [`embedding_execution_plan_for`].
pub const DEFAULT_INTRA_THREADS: u32 = 12;

#[must_use]
pub fn default_max_intra_threads_for(total_cores: usize) -> u32 {
    u32::try_from(total_cores.max(1))
        .unwrap_or(u32::MAX)
        .min(DEFAULT_INTRA_THREADS)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EmbeddingExecutionPlanV1 {
    pub intra_threads: usize,
    pub sessions: usize,
    pub limiting_reason: EmbeddingSessionLimitingReasonV1,
}

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

/// Concurrent embedding sessions for a host with `total_cores` logical CPUs,
/// given the intra-op thread ceiling admitted by the artifact.
///
/// `sessions * intra_threads` is held to the semantic CPU budget. Session-pool
/// resident-memory admission remains the independent memory authority.
#[must_use]
pub fn embedding_session_width_for(
    total_cores: usize,
    max_intra_threads: u32,
    configured_max_sessions: u32,
) -> usize {
    embedding_execution_plan_for(
        embedding_cpu_target(total_cores),
        max_intra_threads,
        configured_max_sessions,
        configured_max_sessions as usize,
        None,
    )
    .sessions
}

fn embedding_execution_plan_for(
    shared_cpu_budget: usize,
    configured_max_intra_threads: u32,
    configured_max_sessions: u32,
    resident_session_limit: usize,
    environment_override: Option<usize>,
) -> EmbeddingExecutionPlanV1 {
    let shared_cpu_budget = shared_cpu_budget.max(1);
    let configured = (configured_max_sessions as usize).max(1);
    let resident = resident_session_limit.max(1);
    let preferred_intra_threads = BASELINE_INTRA_THREADS
        .min(configured_max_intra_threads as usize)
        .max(1);
    let cpu_safe_sessions = (shared_cpu_budget / preferred_intra_threads).max(1);
    let requested = environment_override.unwrap_or(configured);
    let sessions = requested
        .min(configured)
        .min(resident)
        .min(cpu_safe_sessions);
    let intra_threads = embedding_intra_threads_for(
        shared_cpu_budget / sessions.max(1),
        configured_max_intra_threads,
    );
    let limiting_reason = if sessions < requested {
        if sessions == resident {
            EmbeddingSessionLimitingReasonV1::ResidentSessionLimit
        } else if sessions == cpu_safe_sessions {
            EmbeddingSessionLimitingReasonV1::SharedCodeIndexCpuBudget
        } else if sessions == configured {
            EmbeddingSessionLimitingReasonV1::ConfiguredMaximum
        } else {
            EmbeddingSessionLimitingReasonV1::SharedCodeIndexCpuBudget
        }
    } else if environment_override.is_some() {
        EmbeddingSessionLimitingReasonV1::EnvironmentOverride
    } else {
        EmbeddingSessionLimitingReasonV1::ConfiguredMaximum
    };
    EmbeddingExecutionPlanV1 {
        intra_threads,
        sessions,
        limiting_reason,
    }
}

/// Joint runtime plan for native inference and resident session fan-out.
///
/// Session count and intra-op width must be selected together. Planning them
/// independently leaves admitted CPUs idle whenever the pool can retain fewer
/// sessions than CPU arithmetic requested.
#[must_use]
pub(crate) fn embedding_execution_plan(
    configured_max_intra_threads: u32,
    configured_max_sessions: u32,
    resident_session_limit: usize,
) -> EmbeddingExecutionPlanV1 {
    let shared_cpu_budget = installed_cpu_budget();
    let environment_override = env_width(EMBED_SESSIONS_ENV);
    let plan = embedding_execution_plan_for(
        shared_cpu_budget,
        configured_max_intra_threads,
        configured_max_sessions,
        resident_session_limit,
        environment_override,
    );
    record_execution_plan(
        plan,
        environment_override.unwrap_or(configured_max_sessions as usize),
        shared_cpu_budget,
        resident_session_limit,
    );
    plan
}

fn record_execution_plan(
    plan: EmbeddingExecutionPlanV1,
    requested_sessions: usize,
    shared_cpu_budget: usize,
    resident_session_limit: usize,
) {
    hotpath::gauge!("semantic_embedding_sessions_requested").set(requested_sessions);
    hotpath::gauge!("semantic_embedding_sessions_effective").set(plan.sessions);
    hotpath::gauge!("semantic_embedding_sessions_cpu_safe")
        .set((shared_cpu_budget / plan.intra_threads.clamp(1, BASELINE_INTRA_THREADS)).max(1));
    hotpath::gauge!("semantic_embedding_sessions_resident_safe").set(resident_session_limit.max(1));
    hotpath::gauge!("semantic_embedding_intra_threads").set(plan.intra_threads);
    hotpath::gauge!("semantic_embedding_sessions_limiting_reason").set(
        match plan.limiting_reason {
            EmbeddingSessionLimitingReasonV1::SharedCodeIndexCpuBudget => 1,
            EmbeddingSessionLimitingReasonV1::EnvironmentOverride => 2,
            EmbeddingSessionLimitingReasonV1::ConfiguredMaximum => 3,
            EmbeddingSessionLimitingReasonV1::ResidentSessionLimit => 4,
        },
    );
}

/// Session-pool sizing that lets the derived concurrency actually be used.
///
/// The pool's own memory ceiling still applies; this only stops the pool from
/// becoming the binding constraint before the reservation is. The extra slot
/// keeps an interactive query session warm while a rebuild holds the
/// projection sessions.
#[must_use]
pub fn embedding_pool_sessions(intra_threads: u32, configured_max_sessions: u32) -> usize {
    let _ = intra_threads;
    (configured_max_sessions as usize).max(1).saturating_add(1)
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
    let width = embedding_session_width_for(total_cores, BASELINE_INTRA_THREADS as u32, u32::MAX);
    u32::try_from(width.max(1)).unwrap_or(1)
}

/// Host-derived semantic resident ceiling for an unconfigured runtime.
#[must_use]
pub fn default_resident_ceiling_for(admitted_process_bytes: u64) -> u64 {
    let admitted_process_bytes = admitted_process_bytes.max(1);
    (admitted_process_bytes / DEFAULT_RESIDENT_FRACTION_DENOMINATOR)
        .max(DEFAULT_SEMANTIC_RESIDENT_BYTES.min(admitted_process_bytes))
        .min(MAX_SEMANTIC_RESIDENT_BYTES)
}

/// Where the resident ceiling in force came from, reported alongside it so an
/// operator can tell a pin from a derivation, and a derivation from a
/// derivation the model ceilings had to widen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticResidentCeilingSourceV1 {
    /// Derived from the process resident-memory admission authority.
    HostDerived,
    /// Derived, then raised to `max_model_bytes` / `max_tokenizer_bytes` so
    /// the cataloged model still fits. A host this small cannot honour its own
    /// memory share and hold the model at once; the model wins, because a
    /// ceiling under it admits nothing at all.
    HostDerivedClampedToModel,
    /// Pinned by the operator in `semantic.runtime.v1`, already validated.
    OperatorPinned,
}

/// The resident ceiling in force, and where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticResidentCeilingV1 {
    pub bytes: u64,
    pub source: SemanticResidentCeilingSourceV1,
}

/// Preserve an explicit `semantic.runtime.v1` ceiling; otherwise derive it
/// from the process resident-memory authority.
///
/// A derived ceiling must satisfy the same invariant the validator enforces on
/// a pinned one — `max_model_bytes <= max_resident_bytes` — because the
/// artifact admission path checks it again and reports a violation as
/// `RuntimeFailureKindV1::OutOfMemory` long after the derivation. Deriving
/// below the model and letting that surface as a load failure would make a
/// small host look like a corrupt catalog, so the derivation is clamped up and
/// says so.
#[must_use]
pub fn effective_resident_ceiling(
    admitted_process_bytes: u64,
    ceilings: SemanticResourceCeilings,
) -> SemanticResidentCeilingV1 {
    let resolved = match ceilings.max_resident_bytes {
        Some(pinned) => SemanticResidentCeilingV1 {
            bytes: pinned,
            source: SemanticResidentCeilingSourceV1::OperatorPinned,
        },
        None => {
            let derived = default_resident_ceiling_for(admitted_process_bytes);
            let required = ceilings.max_model_bytes.max(ceilings.max_tokenizer_bytes);
            if derived < required {
                SemanticResidentCeilingV1 {
                    bytes: required.min(MAX_SEMANTIC_RESIDENT_BYTES),
                    source: SemanticResidentCeilingSourceV1::HostDerivedClampedToModel,
                }
            } else {
                SemanticResidentCeilingV1 {
                    bytes: derived,
                    source: SemanticResidentCeilingSourceV1::HostDerived,
                }
            }
        }
    };
    hotpath::gauge!("semantic_embedding_resident_admitted_bytes").set(admitted_process_bytes);
    hotpath::gauge!("semantic_embedding_resident_ceiling_bytes").set(resolved.bytes);
    hotpath::gauge!("semantic_embedding_resident_ceiling_source").set(match resolved.source {
        SemanticResidentCeilingSourceV1::HostDerived => 1_u8,
        SemanticResidentCeilingSourceV1::OperatorPinned => 2_u8,
        SemanticResidentCeilingSourceV1::HostDerivedClampedToModel => 3_u8,
    });
    resolved
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
    use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;

    use super::*;

    #[test]
    fn session_width_uses_the_shared_code_index_cpu_budget() {
        assert_eq!(embedding_session_width_for(96, 4, 64), 12);
        assert_eq!(embedding_session_width_for(16, 4, 64), 2);
        assert_eq!(embedding_session_width_for(4, 4, 64), 1);
        assert_eq!(embedding_session_width_for(1, 4, 64), 1);
    }

    #[test]
    fn forced_sessions_are_clamped_to_the_shared_cpu_budget() {
        assert_eq!(
            embedding_execution_plan_for(8, 4, 64, 64, Some(12)),
            EmbeddingExecutionPlanV1 {
                intra_threads: 4,
                sessions: 2,
                limiting_reason: EmbeddingSessionLimitingReasonV1::SharedCodeIndexCpuBudget,
            }
        );
        assert_eq!(
            embedding_execution_plan_for(64, 4, 1, 64, Some(12)),
            EmbeddingExecutionPlanV1 {
                intra_threads: 4,
                sessions: 1,
                limiting_reason: EmbeddingSessionLimitingReasonV1::ConfiguredMaximum,
            }
        );
    }

    #[test]
    fn resident_capacity_reassigns_idle_cpu_to_the_sessions_that_fit() {
        assert_eq!(
            embedding_execution_plan_for(48, 12, 12, 4, None),
            EmbeddingExecutionPlanV1 {
                intra_threads: 12,
                sessions: 4,
                limiting_reason: EmbeddingSessionLimitingReasonV1::ResidentSessionLimit,
            }
        );
        assert_eq!(
            embedding_execution_plan_for(8, 12, 12, 2, None),
            EmbeddingExecutionPlanV1 {
                intra_threads: 4,
                sessions: 2,
                limiting_reason: EmbeddingSessionLimitingReasonV1::ResidentSessionLimit,
            }
        );
        assert_eq!(
            embedding_execution_plan_for(4, 12, 12, 1, None),
            EmbeddingExecutionPlanV1 {
                intra_threads: 4,
                sessions: 1,
                limiting_reason: EmbeddingSessionLimitingReasonV1::ResidentSessionLimit,
            }
        );
    }

    #[test]
    fn default_resident_ceiling_scales_with_admitted_host_memory() {
        const GIB: u64 = 1024 * 1024 * 1024;

        assert_eq!(default_resident_ceiling_for(6 * GIB), 2 * GIB);
        assert_eq!(default_resident_ceiling_for(96 * GIB), 12 * GIB);
        assert_eq!(default_resident_ceiling_for(256 * GIB), 16 * GIB);
    }

    #[test]
    fn configured_resident_ceiling_wins_over_host_derivation() {
        const GIB: u64 = 1024 * 1024 * 1024;

        let pinned = SemanticResourceCeilings {
            max_resident_bytes: Some(3 * GIB),
            ..SemanticResourceCeilings::default()
        };
        assert_eq!(
            effective_resident_ceiling(96 * GIB, pinned),
            SemanticResidentCeilingV1 {
                bytes: 3 * GIB,
                source: SemanticResidentCeilingSourceV1::OperatorPinned,
            }
        );
    }

    #[test]
    fn an_unpinned_ceiling_is_derived_from_the_admitted_process_memory() {
        const GIB: u64 = 1024 * 1024 * 1024;

        assert_eq!(
            effective_resident_ceiling(96 * GIB, SemanticResourceCeilings::default()),
            SemanticResidentCeilingV1 {
                bytes: 12 * GIB,
                source: SemanticResidentCeilingSourceV1::HostDerived,
            }
        );
    }

    /// A derived ceiling has to satisfy the same invariant the settings
    /// validator enforces on a pinned one, or artifact admission later reports
    /// a host too small to hold the model as `OutOfMemory` against the
    /// catalog. The clamp is reported so a host running above its own memory
    /// share is distinguishable from one sized normally.
    #[test]
    fn a_derived_ceiling_below_the_model_ceiling_is_clamped_and_reported() {
        let ceilings = SemanticResourceCeilings {
            max_model_bytes: 700 * 1024 * 1024,
            max_tokenizer_bytes: 64 * 1024 * 1024,
            max_resident_bytes: None,
            ..SemanticResourceCeilings::default()
        };
        let admitted = 512 * 1024 * 1024;
        assert!(
            default_resident_ceiling_for(admitted) < ceilings.max_model_bytes,
            "this host must be small enough to derive below the model ceiling"
        );

        let resolved = effective_resident_ceiling(admitted, ceilings);

        assert_eq!(
            resolved,
            SemanticResidentCeilingV1 {
                bytes: ceilings.max_model_bytes,
                source: SemanticResidentCeilingSourceV1::HostDerivedClampedToModel,
            }
        );
        assert!(
            tracedecay_semantic_contracts::semantic_resident_ceiling_is_valid(
                ceilings,
                resolved.bytes
            ),
            "a derived ceiling must satisfy the validator a pinned one must satisfy"
        );
    }

    #[test]
    fn low_width_native_thread_demand_fits_the_shared_cpu_authority() {
        for shared_cpu_budget in 1..=3usize {
            let plan = embedding_execution_plan_for(
                shared_cpu_budget,
                DEFAULT_INTRA_THREADS,
                u32::MAX,
                usize::MAX,
                None,
            );

            assert_eq!(plan.intra_threads, shared_cpu_budget);
            assert!(plan.sessions * plan.intra_threads <= shared_cpu_budget);
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

    /// Weighted semantic units and one-unit index work meter against one
    /// authority from inside their pools. The authority is local to this
    /// test, so nothing process-wide is installed.
    #[test]
    fn concurrent_index_and_semantic_units_share_width_and_both_progress() {
        let authority = Arc::new(ProcessBackgroundCpuV1::new(
            NonZeroUsize::new(4).expect("nonzero background width"),
        ));
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
                        authority.with_permits(DEFAULT_INTRA_THREADS as usize, operation);
                    })
                    .expect("semantic shared operation");
                } else {
                    tracedecay_code_index::parallelism::install(|| {
                        authority.with_permit(operation);
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
