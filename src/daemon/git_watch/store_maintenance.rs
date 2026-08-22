//! Retention, compaction, and garbage-collection operations run by the daemon
//! maintenance owner.
//!
//! Every operation that opens or garbage-collects a store lives here so its
//! [`StoreAdministration`] lifetime is kept separate from the watcher state
//! machine. The git watcher itself never opens or mutates a store: it routes
//! exact-frontier freshness requests to the code-index scheduler and wakes the
//! maintenance owner.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::branch::BranchAdminAction;
use crate::config::{CompactionThresholdConfig, RetentionConfig};
use crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use crate::tracedecay::TraceDecay;

use super::branch_admin::StoreAdministration;
use super::log_daemon_event;

#[path = "store_maintenance/graph_replay.rs"]
mod graph_replay;
#[cfg(test)]
#[path = "store_maintenance/vector_retention_tests.rs"]
mod vector_retention_tests;
use graph_replay::log_code_generation_retention_degraded;

const MAX_GIT_WORKTREES_PER_SCOPE_INVENTORY: usize = 256;

struct ScopeRootProofInputsV1 {
    live_roots: std::collections::BTreeSet<PathBuf>,
    registered_roots: crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1,
    git_worktrees: crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1,
    mounted_leases: crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1,
    configuration_roots: crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1,
    vector_census: crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1,
    vector_dependencies: crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1,
    vector_sources: std::collections::BTreeSet<tracedecay_domain::CodeGenerationId>,
}

impl ScopeRootProofInputsV1 {
    fn bind_candidate(
        &self,
        scope_hash: String,
        source_scope: tracedecay_store::StoreShardIdV1,
        vector_revision: tracedecay_store::SemanticVectorStageCensusRevision,
    ) -> Result<crate::retention::code_index_generations::ScopeRootLivenessProofV1, &'static str>
    {
        let live_scope_hashes = self
            .live_roots
            .iter()
            .map(|root| crate::retention::code_index_generations::code_index_scope_hash(root))
            .collect();
        crate::retention::code_index_generations::ScopeRootLivenessProofV1::new(
            live_scope_hashes,
            self.registered_roots.clone(),
            self.git_worktrees.clone(),
            self.mounted_leases.clone(),
            self.configuration_roots.clone(),
            self.vector_census.clone(),
            self.vector_dependencies.clone(),
            crate::retention::code_index_generations::ScopeRootCandidateBindingV1 {
                scope_hash,
                source_scope,
                vector_census_revision: vector_revision.get().to_string(),
                live: false,
            },
        )
        .map_err(|_| "scope_liveness_proof_invalid")
    }
}

/// Runs branch-store GC for a project through the daemon administration
/// coordinator, logging what it removed. Returns `false` when layout resolution
/// or administration fails so the maintenance owner keeps the GC cadence
/// eligible for a retry.
pub(super) async fn run_gc(
    administration: &StoreAdministration,
    schedulers: &CodeIndexSchedulerRegistryV1,
    branch_gc_days: u64,
    orphan_db_gc_days: u64,
    cg: &TraceDecay,
) -> bool {
    let root = cg.project_root();
    let data_root = &cg.store_layout().data_root;

    // The coordinator owns the writer gate and its process/store-holder
    // safety checks; GC never runs beside a content writer on the same store.
    let report = administration
        .execute_branch_admin_in_layout(
            schedulers,
            root,
            data_root,
            BranchAdminAction::Gc,
            branch_gc_days,
            orphan_db_gc_days,
        )
        .await;
    let report = match report {
        Ok(report) => report,
        Err(_) => {
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "branch_gc".to_string()),
                    ("project", root.display().to_string()),
                    ("failure", "branch_administration_failed".to_string()),
                ],
            );
            return false;
        }
    };

    if !report.removed_branches.is_empty() || !report.removed_orphan_dbs.is_empty() {
        log_daemon_event(
            "retention_branch_gc",
            &[
                ("project", root.display().to_string()),
                ("removed_tracked", report.removed_branches.len().to_string()),
                (
                    "removed_orphans",
                    report.removed_orphan_dbs.len().to_string(),
                ),
            ],
        );
    }
    true
}

/// Current unix time in whole seconds, as the `i64` the retention engines
/// compare row timestamps against.
fn now_secs_i64() -> Result<i64, &'static str> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system_clock_before_unix_epoch")?
        .as_secs();
    i64::try_from(seconds).map_err(|_| "system_clock_out_of_range")
}

/// Advance one bounded project-wide semantic-vector retention page.
///
/// The maintenance observation registry carries the stage cursor across ticks.
/// A mutating action resets the cursor because the returned census described
/// pre-action state; a no-action page advances it, and end-of-census publishes
/// only fixed-size aggregate counts for Doctor.
pub(super) async fn run_semantic_vector_generation_retention(
    graph: &TraceDecay,
    schedulers: &CodeIndexSchedulerRegistryV1,
    observations: &crate::daemon::maintenance::StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_usecases::context::CancellationToken,
) -> bool {
    if cancellation.is_cancelled() {
        observations.record_semantic_vector_retention_failure(graph.project_root());
        log_semantic_vector_retention_degraded("retention_cancelled");
        return false;
    }
    let root = graph.project_root();
    let Some(configuration) = graph
        .configuration_runtime()
        .semantic_configuration_inventory_authority()
    else {
        // The activation coordinator is not seated. Whether that is the
        // ordinary default-off state or a project-open overlap is decided by
        // the durable semantic configuration, never by mount timing: a
        // committed retrieval profile means a coordinator is expected
        // imminently, so the pass stays retryable on the short cadence
        // instead of pinning quiet and making the first census wait a full
        // maintenance interval.
        return match graph.configuration_runtime().client().current().await {
            Ok(runtime_configuration)
                if semantic_retrieval_profiles_disabled(&runtime_configuration.config.semantic) =>
            {
                // Plan 20 default off: no committed active or rollback
                // retrieval profile, so no census will ever complete. Pin the
                // typed unseated read for the code-generation pass and
                // succeed quietly instead of resetting to Unknown and
                // re-logging a degraded retry loop every tick.
                observations.record_semantic_vector_retention_unseated(root);
                true
            }
            Ok(_) => {
                observations.record_semantic_vector_retention_failure(root);
                log_semantic_vector_retention_degraded("configuration_inventory_unavailable");
                false
            }
            Err(_) => {
                observations.record_semantic_vector_retention_failure(root);
                log_semantic_vector_retention_degraded("runtime_configuration_unavailable");
                false
            }
        };
    };
    let after = observations.semantic_vector_retention_cursor(root);
    match crate::daemon::code_index_scheduler::semantic_vector_graph::retire_one_project_vector_generation(
        schedulers,
        root,
        &configuration,
        after,
    )
    .await
    {
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorRetentionStep::Ready(
            census,
        ) => {
            let convergence_pending = census.continuation.is_some()
                || matches!(
                    census.action,
                    tracedecay_graph_db::SemanticVectorRetentionAction::Retired(_)
                        | tracedecay_graph_db::SemanticVectorRetentionAction::Finalized(_)
                        | tracedecay_graph_db::SemanticVectorRetentionAction::CancelledRemoved(_)
                );
            if !observations.record_semantic_vector_retention_census(root, &census) {
                log_semantic_vector_retention_degraded("census_count_overflow");
                return false;
            }
            if !matches!(
                census.action,
                tracedecay_graph_db::SemanticVectorRetentionAction::None
            ) {
                log_daemon_event(
                    "retention_semantic_vector_generations",
                    &[
                        ("project", root.display().to_string()),
                        ("action", format!("{:?}", census.action)),
                    ],
                );
            }
            // A bounded page/action deliberately requests the short retry
            // cadence until a post-mutation census reaches its end.
            !convergence_pending
        }
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorRetentionStep::ResetRequired(
            reason,
        ) => {
            observations.record_semantic_vector_retention_failure(root);
            log_semantic_vector_retention_degraded(&format!("reset_required:{reason}"));
            false
        }
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorRetentionStep::Corrupt(
            reason,
        ) => {
            observations.record_semantic_vector_retention_failure(root);
            log_semantic_vector_retention_degraded(&format!("corrupt:{reason}"));
            false
        }
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorRetentionStep::Unavailable(
            reason,
        ) => {
            observations.record_semantic_vector_retention_failure(root);
            log_semantic_vector_retention_degraded(&format!("unavailable:{reason}"));
            false
        }
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorRetentionStep::Denied(
            reason,
        ) => {
            observations.record_semantic_vector_retention_failure(root);
            log_semantic_vector_retention_degraded(&format!("denied:{reason}"));
            false
        }
    }
}

/// Plan 20 default-off: semantic retrieval is genuinely disabled only when
/// the durable configuration commits neither an active nor a rollback
/// retrieval profile. A committed profile with an unseated activation
/// coordinator is a transient (or genuinely degraded) state that must stay
/// retryable, not a quiet pin.
fn semantic_retrieval_profiles_disabled(semantic: &crate::config::SemanticConfig) -> bool {
    semantic.active_profile.is_none() && semantic.rollback_profile.is_none()
}

fn log_semantic_vector_retention_degraded(failure: &str) {
    log_daemon_event(
        "retention_degraded",
        &[
            ("pass", "semantic_vector_generations".to_owned()),
            ("failure", failure.to_owned()),
        ],
    );
}

/// Vector protection inventory for one code-generation retention pass.
///
/// `Online` carries the exact vector pin set read from the mounted code
/// graph plus the authorities needed to re-verify it under the writer freeze.
/// `SemanticUnseated` is the ordinary default-off state: no semantic runtime
/// is seated, no census will ever exist, and the pass sweeps under the
/// offline protection set without reporting a degradation. `CensusScanning`
/// is in-progress: the bounded census is still paging toward its exact pin
/// set, so the pass defers instead of planning against a mid-scan inventory.
/// `Offline` is a typed degradation for an unavailable vector runtime; the
/// pass then plans against the offline protection set. `Refused` is
/// fail-closed: the vector authority reported reset/corrupt/denied and no
/// sweep may run.
pub(super) enum VectorRetentionInventoryV1 {
    Online {
        sources: std::collections::BTreeSet<tracedecay_domain::CodeGenerationId>,
        configuration:
            tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
        expected_vector_revision: tracedecay_store::SemanticVectorStageCensusRevision,
    },
    SemanticUnseated,
    CensusScanning,
    Offline {
        reason: String,
    },
    Refused {
        reason: String,
    },
}

impl VectorRetentionInventoryV1 {
    /// The `retention_degraded` failure the code-generation pass reports for
    /// this inventory, or `None` for states that are ordinary journeys and
    /// must stay quiet on every pass: an online inventory, a daemon whose
    /// semantic runtime is not seated (the default-off state), and a census
    /// still paging toward its exact pin set.
    pub(super) fn degraded_reason(&self) -> Option<String> {
        match self {
            Self::Online { .. } | Self::SemanticUnseated | Self::CensusScanning => None,
            Self::Offline { reason } => Some(format!("vector_inventory_offline:{reason}")),
            Self::Refused { reason } => Some(reason.clone()),
        }
    }
}

pub(super) async fn resolve_vector_retention_inventory(
    graph: &TraceDecay,
    schedulers: &CodeIndexSchedulerRegistryV1,
    observations: &crate::daemon::maintenance::StoreTelemetrySamplingRegistry,
) -> VectorRetentionInventoryV1 {
    let expected_vector_revision =
        match observations.semantic_vector_retention_read(graph.project_root()) {
            crate::daemon::maintenance::SemanticVectorRetentionReadV1::Observed { receipt } => {
                receipt.revision
            }
            crate::daemon::maintenance::SemanticVectorRetentionReadV1::SemanticUnseated => {
                return VectorRetentionInventoryV1::SemanticUnseated;
            }
            crate::daemon::maintenance::SemanticVectorRetentionReadV1::Scanning => {
                return VectorRetentionInventoryV1::CensusScanning;
            }
            crate::daemon::maintenance::SemanticVectorRetentionReadV1::Unknown => {
                return VectorRetentionInventoryV1::Offline {
                    reason: "vector_census_incomplete".to_owned(),
                };
            }
        };
    let Some(configuration) = graph
        .configuration_runtime()
        .semantic_configuration_inventory_authority()
    else {
        return VectorRetentionInventoryV1::Offline {
            reason: "configuration_inventory_unavailable".to_owned(),
        };
    };
    let project_root = graph.hook_store_layout().project_root.clone();
    let sources = crate::daemon::code_index_scheduler::semantic_vector_graph::project_vector_readable_sources(
        schedulers,
        &project_root,
        &configuration,
        expected_vector_revision,
    )
    .await;
    classify_vector_readable_sources(sources, configuration, expected_vector_revision)
}

/// Map the mounted graph's readable-source read onto the retention inventory:
/// unavailable degrades to the offline protection set, while reset, corrupt,
/// and denied stay fail-closed and refuse the sweep.
fn classify_vector_readable_sources(
    sources: crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources,
    configuration: tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
    expected_vector_revision: tracedecay_store::SemanticVectorStageCensusRevision,
) -> VectorRetentionInventoryV1 {
    match sources {
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Ready {
            sources,
            ..
        } => VectorRetentionInventoryV1::Online {
            sources,
            configuration,
            expected_vector_revision,
        },
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Unavailable(
            reason,
        ) => VectorRetentionInventoryV1::Offline {
            reason: format!("vector_graph_unavailable:{reason}"),
        },
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::ResetRequired(
            reason,
        ) => VectorRetentionInventoryV1::Refused {
            reason: format!("vector_graph_reset_required:{reason}"),
        },
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Corrupt(
            reason,
        ) => VectorRetentionInventoryV1::Refused {
            reason: format!("vector_graph_corrupt:{reason}"),
        },
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Denied(
            reason,
        ) => VectorRetentionInventoryV1::Refused {
            reason: format!("vector_graph_denied:{reason}"),
        },
    }
}

/// Collect superseded code-index generations for one mounted project.
///
/// Sealed generations are ordinary files, so no database retention or
/// compaction pass reclaims them. This runs on the ordinary maintenance cadence
/// and is independent of the semantic projection lane: the only previous caller
/// sat inside legacy vector migration, so a profile with semantic search
/// disabled never collected anything and grew without bound.
///
/// Vector-readable source generations are pinned through the mounted code
/// graph when it is resolvable. A daemon without a seated semantic runtime
/// (the default-off state) sweeps under the offline protection set as its
/// ordinary quiet journey, and an in-progress census defers the sweep until
/// its exact pin set is complete. When the graph runtime is unavailable —
/// saturated capacity, failed activation, or nothing serving — the pass
/// degrades to the offline protection set (active pointer head, durable
/// pointer index, rollback floor, and the serving generation) so sealed files
/// cannot grow without bound while the graph is dark. Reset, corrupt, and
/// denied vector authorities stay fail-closed and collect nothing.
pub(super) async fn run_code_generation_retention(
    graph: &TraceDecay,
    schedulers: &CodeIndexSchedulerRegistryV1,
    observations: &crate::daemon::maintenance::StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_usecases::context::CancellationToken,
) -> bool {
    if cancellation.is_cancelled() {
        log_code_generation_retention_degraded("retention_cancelled");
        return false;
    }
    let layout = graph.hook_store_layout();
    let store_root = code_index_store_root(&layout.data_root, &layout.project_root);
    // No published generation means nothing has been sealed for this project.
    if !store_root.join("active-code-generation-v1.json").is_file() {
        return true;
    }
    let vector_inventory =
        resolve_vector_retention_inventory(graph, schedulers, observations).await;
    apply_code_generation_retention(graph, schedulers, vector_inventory, cancellation).await
}

/// The offline protection pin: the generation the mounted scheduler is
/// currently serving, when one is mounted at all.
async fn serving_generation_pins(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> std::collections::BTreeSet<tracedecay_domain::CodeGenerationId> {
    let mut pins = std::collections::BTreeSet::new();
    if let Some(scope) = schedulers.serving_code_scope(project_root).await
        && let Some(serving) = scope.serving_generation
    {
        pins.insert(serving.manifest().generation_id.clone());
    }
    pins
}

/// Execute one code-generation retention pass against a resolved vector
/// inventory. Emitting `retention_degraded` is decided exclusively by
/// [`VectorRetentionInventoryV1::degraded_reason`], so quiet states cannot be
/// reintroduced into the degraded log by a divergent match arm.
async fn apply_code_generation_retention(
    graph: &TraceDecay,
    schedulers: &CodeIndexSchedulerRegistryV1,
    vector_inventory: VectorRetentionInventoryV1,
    cancellation: &tracedecay_usecases::context::CancellationToken,
) -> bool {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionErrorV1, CodeGenerationRetentionModeV1,
        DEFAULT_SUPERSEDED_GENERATION_FLOOR, execute_code_generation_retention_cancellable,
        prepare_next_code_generation_retention_cancellable,
    };
    let layout = graph.hook_store_layout();
    let store_root = code_index_store_root(&layout.data_root, &layout.project_root);
    // Retired generations stay reachable for graph replay through the replay
    // pool; retention hard-links each one there before its release event
    // becomes durable, and the replay reconciler deletes pool entries once
    // the graph confirms it no longer needs them.
    let graph_replay_pool_root = graph.db().database_path().with_extension("graph-replay");
    if let Some(failure) = vector_inventory.degraded_reason() {
        log_code_generation_retention_degraded(&failure);
    }
    // Published vectors live in the mounted code graph. When the graph is
    // resolvable, its inventory is the exact vector pin set. Without a seated
    // semantic runtime, and while the vector runtime is unavailable, the pass
    // plans against the offline protection set (active pointer head, durable
    // pointer index, rollback floor, plus the serving generation) instead of
    // letting sealed files grow without bound while the graph is dark. A
    // paging census defers: its exact pin set arrives when the scan
    // completes, and the vector retention pass already keeps the retry
    // cadence short while paging. Reset, corrupt, and denied vector
    // authorities stay fail-closed.
    let (vector_readable_sources, inventory_mode) = match &vector_inventory {
        VectorRetentionInventoryV1::Online { sources, .. } => (sources.clone(), "online"),
        VectorRetentionInventoryV1::SemanticUnseated => (
            serving_generation_pins(schedulers, &layout.project_root).await,
            "semantic_unseated",
        ),
        VectorRetentionInventoryV1::CensusScanning => return true,
        VectorRetentionInventoryV1::Offline { .. } => (
            serving_generation_pins(schedulers, &layout.project_root).await,
            "offline",
        ),
        VectorRetentionInventoryV1::Refused { .. } => return false,
    };
    // Full digest verification routinely reads several GiB. Run it before
    // entering the graph transaction and preserve the daemon shutdown token
    // through the blocking boundary; the planner checks it after every bounded
    // read chunk and creates no journal before verification completes.
    let plan_root = store_root.clone();
    let plan_sources = vector_readable_sources.clone();
    let plan_cancellation = cancellation.clone();
    let plan_pool_root = graph_replay_pool_root.clone();
    let plan = tokio::task::spawn_blocking(move || {
        prepare_next_code_generation_retention_cancellable(
            &plan_root,
            &plan_sources,
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
            &|| plan_cancellation.is_cancelled(),
            Some(&plan_pool_root),
        )
    })
    .await;
    let plan = match plan {
        Ok(Ok(plan)) => plan,
        Ok(Err(
            crate::retention::code_index_generations::CodeGenerationRetentionErrorV1::Cancelled,
        )) => {
            log_code_generation_retention_degraded("retention_cancelled");
            return false;
        }
        Ok(Err(_)) => {
            log_code_generation_retention_degraded("retention_plan_failed");
            return false;
        }
        Err(_) => {
            log_code_generation_retention_degraded("retention_task_panicked");
            return false;
        }
    };
    // A failed replay reconcile in offline mode keeps its journaled release
    // evidence for a later graph-available pass; deletion of newly planned
    // files stays safe, but the pass reports degraded so the retry cadence
    // stays short.
    let mut offline_replay_reconcile_failed = false;
    match graph_replay::reconcile_graph_replay_releases(graph, &store_root, cancellation).await {
        graph_replay::ReconcileOutcome::Complete => {}
        graph_replay::ReconcileOutcome::Retained => return true,
        graph_replay::ReconcileOutcome::Failed => {
            if matches!(&vector_inventory, VectorRetentionInventoryV1::Online { .. }) {
                return false;
            }
            offline_replay_reconcile_failed = true;
        }
    }
    if !plan.has_collectable_work() {
        return !offline_replay_reconcile_failed;
    }
    if cancellation.is_cancelled() {
        log_code_generation_retention_degraded("retention_cancelled");
        return false;
    }

    // Freeze the vector writer, then re-read the committed active+rollback
    // identities and their exact source generations. Graph head order is not
    // retention authority: a newer unactivated candidate must not displace
    // the configured generation from this fence. The offline protection set
    // has no vector inventory to fence, so no freeze is taken there.
    let vector_writer_freeze = if let VectorRetentionInventoryV1::Online {
        configuration,
        expected_vector_revision,
        ..
    } = &vector_inventory
    {
        let Some(vector_runtime) =
            tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
                &layout.project_root,
            )
        else {
            log_code_generation_retention_degraded("vector_writer_unavailable");
            return false;
        };
        let vector_writer_freeze = vector_runtime.freeze_vector_mutations().await;
        let pinned_vector_sources =
            match crate::daemon::code_index_scheduler::semantic_vector_graph::project_vector_readable_sources(
                schedulers,
                &layout.project_root,
                configuration,
                *expected_vector_revision,
            )
            .await
            {
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Ready {
                    sources,
                    ..
                } => sources,
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::ResetRequired(
                    reason,
                ) => {
                    log_code_generation_retention_degraded(&format!(
                        "vector_inventory_reset_required:{reason}"
                    ));
                    return false;
                }
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Corrupt(
                    reason,
                ) => {
                    log_code_generation_retention_degraded(&format!(
                        "vector_inventory_corrupt:{reason}"
                    ));
                    return false;
                }
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Unavailable(
                    reason,
                ) => {
                    log_code_generation_retention_degraded(&format!(
                        "vector_inventory_unavailable:{reason}"
                    ));
                    return false;
                }
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Denied(
                    reason,
                ) => {
                    log_code_generation_retention_degraded(&format!(
                        "vector_inventory_denied:{reason}"
                    ));
                    return false;
                }
            };
        if pinned_vector_sources != vector_readable_sources {
            log_code_generation_retention_degraded("vector_inventory_changed");
            return false;
        }
        tracedecay_usecases::semantic_runtime::retain_project_semantic_code_sources(
            &layout.project_root,
            &pinned_vector_sources,
        );
        for generation in &plan.collectable_generations {
            match crate::daemon::code_index_scheduler::semantic_vector_graph::project_vector_source_generation_is_live(
                schedulers,
                &layout.project_root,
                &generation.generation_id,
                *expected_vector_revision,
            )
            .await
            {
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Ready(
                    true,
                ) => {
                    // A pending, ready, published, or base-linked vector stage still
                    // reads this exact source. It was absent from the root-only
                    // planning inventory, so retain it and let vector convergence
                    // make the next maintenance tick eligible.
                    return true;
                }
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Ready(
                    false,
                ) => {}
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Unavailable(
                    reason,
                ) => {
                    log_code_generation_retention_degraded(&format!(
                        "vector_source_liveness_unavailable:{reason}"
                    ));
                    return false;
                }
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Denied(
                    reason,
                ) => {
                    log_code_generation_retention_degraded(&format!(
                        "vector_source_liveness_denied:{reason}"
                    ));
                    return false;
                }
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::ResetRequired(
                    reason,
                ) => {
                    log_code_generation_retention_degraded(&format!(
                        "vector_source_liveness_reset_required:{reason}"
                    ));
                    return false;
                }
                crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Corrupt(
                    reason,
                ) => {
                    log_code_generation_retention_degraded(&format!(
                        "vector_source_liveness_corrupt:{reason}"
                    ));
                    return false;
                }
            }
        }
        Some(vector_writer_freeze)
    } else {
        None
    };
    if cancellation.is_cancelled() {
        log_code_generation_retention_degraded("retention_cancelled");
        return false;
    }
    let completed_at = tracedecay_domain::UtcMicros(crate::tracedecay::current_timestamp());
    let execution_root = store_root.clone();
    let execution_pool_root = graph_replay_pool_root.clone();
    let execution_cancellation = cancellation.clone();
    let report = tokio::task::spawn_blocking(move || {
        execute_code_generation_retention_cancellable(
            &execution_root,
            plan,
            CodeGenerationRetentionModeV1::Apply,
            completed_at,
            Some(&execution_pool_root),
            &|| execution_cancellation.is_cancelled(),
        )
    })
    .await;
    drop(vector_writer_freeze);

    match report {
        Ok(Ok(report)) => {
            let generation_reclaimed = report.receipt.as_ref().map_or_else(
                || {
                    report
                        .deleted_generations
                        .iter()
                        .map(|generation| generation.size_bytes)
                        .sum()
                },
                |receipt| receipt.reclaimed_bytes,
            );
            let text_artifact_reclaimed = report.text_artifact_receipt.as_ref().map_or_else(
                || {
                    report
                        .deleted_text_artifacts
                        .iter()
                        .map(|artifact| artifact.size_bytes)
                        .sum()
                },
                |receipt| receipt.reclaimed_bytes,
            );
            let reclaimed = generation_reclaimed.saturating_add(text_artifact_reclaimed);
            if reclaimed > 0 {
                log_daemon_event(
                    "retention_code_generations",
                    &[
                        ("store", "code-index-v1".to_string()),
                        ("mode", inventory_mode.to_string()),
                        ("bytes_reclaimed", reclaimed.to_string()),
                        (
                            "generations_collected",
                            report.deleted_generations.len().to_string(),
                        ),
                        (
                            "text_artifacts_collected",
                            report.deleted_text_artifacts.len().to_string(),
                        ),
                    ],
                );
            }
            graph_replay::reconcile_graph_replay_releases(graph, &store_root, cancellation)
                .await
                .succeeded()
                && !offline_replay_reconcile_failed
        }
        Ok(Err(CodeGenerationRetentionErrorV1::Cancelled)) => {
            log_code_generation_retention_degraded("retention_cancelled");
            false
        }
        Ok(Err(_)) => {
            log_code_generation_retention_degraded("retention_pass_failed");
            false
        }
        Err(_) => {
            log_code_generation_retention_degraded("retention_task_panicked");
            false
        }
    }
}

fn git_worktree_root_inventory(
    project_root: &Path,
) -> Result<
    (
        std::collections::BTreeSet<PathBuf>,
        crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1,
    ),
    &'static str,
> {
    let repository = gix::open(project_root).map_err(|_| "git_repository_unavailable")?;
    let linked = repository
        .worktrees()
        .map_err(|_| "git_worktree_inventory_unavailable")?;
    if linked.len() >= MAX_GIT_WORKTREES_PER_SCOPE_INVENTORY {
        return Err("git_worktree_inventory_exceeds_bound");
    }

    let mut exact_roots = std::collections::BTreeSet::from([project_root.to_path_buf()]);
    if let Ok(main) = repository.main_repo()
        && let Some(worktree) = main.worktree()
    {
        exact_roots.insert(worktree.base().to_path_buf());
    }
    let mut linked_material = Vec::with_capacity(linked.len());
    for worktree in linked {
        let base = worktree
            .base()
            .map_err(|_| "git_worktree_root_unavailable")?;
        linked_material.push((
            worktree.git_dir().to_string_lossy().into_owned(),
            base.to_string_lossy().into_owned(),
        ));
        exact_roots.insert(base);
    }
    if exact_roots.is_empty() || exact_roots.len() > MAX_GIT_WORKTREES_PER_SCOPE_INVENTORY {
        return Err("git_worktree_inventory_invalid");
    }
    let terminal_count =
        u64::try_from(exact_roots.len()).map_err(|_| "git_worktree_count_overflow")?;
    let material = (
        "tracedecay.git-worktree-root-inventory.v1",
        repository.common_dir().to_string_lossy().into_owned(),
        linked_material,
        exact_roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    );
    let digest = tracedecay_domain::canonical_sha256(&material)
        .map_err(|_| "git_worktree_inventory_digest_failed")?;
    let receipt = crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1 {
        revision: digest.as_str().to_owned(),
        terminal_count,
        digest: digest.as_str().to_owned(),
    };
    let mut roots = std::collections::BTreeSet::new();
    for root in exact_roots {
        insert_live_root_variants(&mut roots, &root);
    }
    Ok((roots, receipt))
}

async fn collect_scope_root_proof_inputs(
    graph: &TraceDecay,
    schedulers: &CodeIndexSchedulerRegistryV1,
    vector_receipt: &tracedecay_store::SemanticVectorProjectCensusReceipt,
) -> Result<ScopeRootProofInputsV1, &'static str> {
    let layout = graph.hook_store_layout();
    let project_id = layout
        .identity
        .project_id
        .as_deref()
        .ok_or("registered_project_identity_missing")?;
    let registered = graph
        .profile_database()
        .registered_project_root_inventory(project_id)
        .await
        .map_err(|_| "registered_root_inventory_unavailable")?
        .ok_or("registered_root_inventory_missing")?;
    let registered_candidates = registered
        .roots
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned())
        .map_err(|_| "registered_project_identity_invalid")?;
    let enrolled_roots = TraceDecay::enrolled_project_roots(registered_candidates, &project_id)
        .map_err(|_| "registered_enrollment_inventory_unavailable")?;
    if enrolled_roots.is_empty() {
        return Err("registered_enrollment_inventory_empty");
    }
    let enrolled_material = enrolled_roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let enrolled_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.registered-enrollment-root-inventory.v1",
        registered.inventory_digest.as_str(),
        &enrolled_material,
    ))
    .map_err(|_| "registered_enrollment_inventory_digest_failed")?;
    let registered_receipt =
        crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1 {
            revision: registered.inventory_digest.as_str().to_owned(),
            terminal_count: u64::try_from(enrolled_roots.len())
                .map_err(|_| "registered_enrollment_count_overflow")?,
            digest: enrolled_digest.as_str().to_owned(),
        };
    let mut live_roots = std::collections::BTreeSet::new();
    for root in enrolled_roots {
        insert_live_root_variants(&mut live_roots, &root);
    }

    let project_root = graph.project_root().to_path_buf();
    let (git_roots, git_receipt) =
        tokio::task::spawn_blocking(move || git_worktree_root_inventory(&project_root))
            .await
            .map_err(|_| "git_worktree_inventory_task_panicked")??;
    live_roots.extend(git_roots);

    let mounted = schedulers.scope_retention_mounted_roots().await?;
    let mounted_count = u64::try_from(mounted.len()).map_err(|_| "mounted_root_count_overflow")?;
    let mounted_material = mounted
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mounted_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.mounted-code-index-root-inventory.v1",
        &mounted_material,
    ))
    .map_err(|_| "mounted_root_inventory_digest_failed")?;
    let mounted_receipt = crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1 {
        revision: mounted_digest.as_str().to_owned(),
        terminal_count: mounted_count,
        digest: mounted_digest.as_str().to_owned(),
    };
    for root in mounted {
        insert_live_root_variants(&mut live_roots, &root);
    }
    if live_roots.is_empty() {
        return Err("scope_live_root_inventory_empty");
    }

    let configuration = graph
        .configuration_runtime()
        .semantic_configuration_inventory_authority()
        .ok_or("configuration_inventory_unavailable")?;
    let (
        vector_sources,
        configuration_receipt,
        configured_root_receipt,
    ) = match crate::daemon::code_index_scheduler::semantic_vector_graph::project_vector_readable_sources(
        schedulers,
        graph.project_root(),
        &configuration,
        vector_receipt.revision,
    )
    .await
    {
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Ready {
            sources,
            configuration_receipt,
            configured_root_receipt,
        } => (sources, configuration_receipt, configured_root_receipt),
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::ResetRequired(
            _,
        ) => return Err("scope_vector_inventory_reset_required"),
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Corrupt(
            _,
        ) => return Err("scope_vector_inventory_corrupt"),
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Unavailable(
            _,
        ) => return Err("scope_vector_inventory_unavailable"),
        crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Denied(
            _,
        ) => return Err("scope_vector_inventory_denied"),
    };
    let configuration_roots =
        crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1 {
            revision: configuration_receipt.revision().to_string(),
            terminal_count: configuration_receipt.root_binding_count(),
            digest: configuration_receipt.inventory_digest().as_str().to_owned(),
        };
    let vector_dependency_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.configured-vector-dependency-inventory.v1",
        configured_root_receipt.root_digest().as_str(),
        &vector_sources,
    ))
    .map_err(|_| "vector_dependency_inventory_digest_failed")?;
    let vector_dependencies =
        crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1 {
            revision: configured_root_receipt.revision().to_string(),
            terminal_count: configured_root_receipt.root_count(),
            digest: vector_dependency_digest.as_str().to_owned(),
        };
    let vector_count = vector_receipt
        .counts
        .pending
        .checked_add(vector_receipt.counts.ready)
        .and_then(|count| count.checked_add(vector_receipt.counts.published))
        .and_then(|count| count.checked_add(vector_receipt.counts.cancelled))
        .ok_or("vector_census_count_overflow")?;
    let vector_census = crate::retention::code_index_generations::ScopeRootAuthorityReceiptV1 {
        revision: vector_receipt.revision.get().to_string(),
        terminal_count: vector_count,
        digest: vector_receipt.record_digest.as_str().to_owned(),
    };

    Ok(ScopeRootProofInputsV1 {
        live_roots,
        registered_roots: registered_receipt,
        git_worktrees: git_receipt,
        mounted_leases: mounted_receipt,
        configuration_roots,
        vector_census,
        vector_dependencies,
        vector_sources,
    })
}

/// Reconcile whole code-index *scope roots* for one mounted repository.
///
/// Generation retention above is scoped to a single
/// `code-index-v1/<sha256(canonical_project_root)>/` directory, and every caller
/// derives exactly one such scope from the root it was handed. Nothing has ever
/// enumerated the siblings, so a scope whose project root is gone — a deleted
/// agent worktree is the ordinary cause — is unreachable by any retention pass
/// and uncounted by any report. One large repository carried three scope
/// directories, two of them orphaned, holding 7.2 GiB nothing could see.
///
/// The pass is fail-closed by construction. Git proves the complete registered
/// worktree set, while the revision-pinned semantic staging authority binds
/// every candidate physical scope hash to its exact logical source shard. A
/// candidate is collected only when both authorities say it is unreferenced;
/// missing, conflicting, or stale vector evidence collects nothing.
pub(super) async fn run_code_index_scope_reconciliation(
    graph: &TraceDecay,
    schedulers: &CodeIndexSchedulerRegistryV1,
    observations: &crate::daemon::maintenance::StoreTelemetrySamplingRegistry,
) -> bool {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        complete_scope_root_binding_cleanup, execute_scope_root_retention,
        plan_scope_root_retention, plan_scope_root_retention_with_liveness_proof,
        prepare_scope_root_binding_cleanup, recover_scope_root_binding_cleanup,
        recover_scope_root_retention,
    };

    let layout = graph.hook_store_layout();
    let store_root = code_index_scope_store_root(&layout.data_root);
    if !store_root.is_dir() {
        return true;
    }

    let recovery_root = store_root.clone();
    let pending_binding_cleanup = tokio::task::spawn_blocking(move || {
        recover_scope_root_retention(&recovery_root)
            .map_err(|_| "scope_reconciliation_recovery_failed")?;
        recover_scope_root_binding_cleanup(&recovery_root)
            .map_err(|_| "scope_binding_cleanup_recovery_failed")
    })
    .await;
    let pending_binding_cleanup = match pending_binding_cleanup {
        Ok(Ok(pending)) => pending,
        Ok(Err(failure)) => {
            log_code_index_scope_reconciliation_degraded(failure);
            return false;
        }
        Err(_) => {
            log_code_index_scope_reconciliation_degraded("scope_reconciliation_task_panicked");
            return false;
        }
    };
    let vector_receipt = match observations.semantic_vector_retention_read(graph.project_root()) {
        crate::daemon::maintenance::SemanticVectorRetentionReadV1::Observed { receipt } => receipt,
        crate::daemon::maintenance::SemanticVectorRetentionReadV1::Unknown
        | crate::daemon::maintenance::SemanticVectorRetentionReadV1::Scanning => {
            log_code_index_scope_reconciliation_degraded("vector_census_incomplete");
            return false;
        }
        // Scope collection is gated on a complete post-convergence census, so
        // an unseated semantic runtime can never reach this pass through the
        // maintenance journey; refuse fail-closed with its own reason if a
        // future caller ever does.
        crate::daemon::maintenance::SemanticVectorRetentionReadV1::SemanticUnseated => {
            log_code_index_scope_reconciliation_degraded("semantic_configuration_unseated");
            return false;
        }
    };
    if let Some(replay) = pending_binding_cleanup {
        let Some(vector_runtime) =
            tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
                graph.project_root(),
            )
        else {
            log_code_index_scope_reconciliation_degraded("vector_writer_unavailable");
            return false;
        };
        let _vector_writer = vector_runtime.freeze_vector_mutations().await;
        let current_inputs =
            match collect_scope_root_proof_inputs(graph, schedulers, &vector_receipt).await {
                Ok(inputs) => inputs,
                Err(failure) => {
                    log_code_index_scope_reconciliation_degraded(failure);
                    return false;
                }
            };
        match crate::daemon::code_index_scheduler::semantic_vector_graph::project_vector_code_scope_is_live(
            schedulers,
            graph.project_root(),
            &replay.scope_hash,
            vector_receipt.revision,
        )
        .await
        {
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Ready {
                source_scope,
                live: false,
            } => {
                if source_scope != replay.source_scope {
                    log_code_index_scope_reconciliation_degraded(
                        "vector_scope_binding_replay_mismatch",
                    );
                    return false;
                }
                let current_proof = match current_inputs.bind_candidate(
                    replay.scope_hash.clone(),
                    source_scope.clone(),
                    vector_receipt.revision,
                ) {
                    Ok(proof) => proof,
                    Err(failure) => {
                        log_code_index_scope_reconciliation_degraded(failure);
                        return false;
                    }
                };
                if current_proof != replay.liveness_proof {
                    log_code_index_scope_reconciliation_degraded(
                        "scope_binding_cleanup_authority_changed",
                    );
                    return false;
                }
                match crate::daemon::code_index_scheduler::semantic_vector_graph::remove_project_vector_code_scope_binding(
                    schedulers,
                    graph.project_root(),
                    &replay.scope_hash,
                    &source_scope,
                    vector_receipt.revision,
                )
                .await
                {
                    crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Ready(true) => {}
                    crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Ready(false) => {
                        log_code_index_scope_reconciliation_degraded(
                            "vector_scope_binding_not_removed",
                        );
                        return false;
                    }
                    crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Unavailable(reason) => {
                        log_code_index_scope_reconciliation_degraded(&format!(
                            "vector_scope_binding_unavailable:{reason}"
                        ));
                        return false;
                    }
                    crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Denied(reason) => {
                        log_code_index_scope_reconciliation_degraded(&format!(
                            "vector_scope_binding_denied:{reason}"
                        ));
                        return false;
                    }
                    crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::ResetRequired(reason) => {
                        log_code_index_scope_reconciliation_degraded(&format!(
                            "vector_scope_binding_reset_required:{reason}"
                        ));
                        return false;
                    }
                    crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorSourceLiveness::Corrupt(reason) => {
                        log_code_index_scope_reconciliation_degraded(&format!(
                            "vector_scope_binding_corrupt:{reason}"
                        ));
                        return false;
                    }
                }
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Missing => {}
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Ready {
                live: true,
                ..
            } => {
                log_code_index_scope_reconciliation_degraded("vector_scope_binding_still_live");
                return false;
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Unavailable(
                reason,
            ) => {
                log_code_index_scope_reconciliation_degraded(&format!(
                    "vector_scope_unavailable:{reason}"
                ));
                return false;
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Denied(
                reason,
            ) => {
                log_code_index_scope_reconciliation_degraded(&format!(
                    "vector_scope_denied:{reason}"
                ));
                return false;
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::ResetRequired(
                reason,
            ) => {
                log_code_index_scope_reconciliation_degraded(&format!(
                    "vector_scope_reset_required:{reason}"
                ));
                return false;
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Corrupt(
                reason,
            ) => {
                log_code_index_scope_reconciliation_degraded(&format!(
                    "vector_scope_corrupt:{reason}"
                ));
                return false;
            }
        }
        let completion_root = store_root.clone();
        let completion_replay = replay.clone();
        match tokio::task::spawn_blocking(move || {
            complete_scope_root_binding_cleanup(&completion_root, &completion_replay)
                .map_err(|_| "scope_binding_cleanup_completion_failed")
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(failure)) => {
                log_code_index_scope_reconciliation_degraded(failure);
                return false;
            }
            Err(_) => {
                log_code_index_scope_reconciliation_degraded("scope_reconciliation_task_panicked");
                return false;
            }
        }
        // Removing a binding advances the vector census revision. Defer the
        // next filesystem plan until maintenance has observed that revision.
        return false;
    }

    let now_secs = match now_secs_i64() {
        Ok(now) => now,
        Err(failure) => {
            log_code_index_scope_reconciliation_degraded(failure);
            return false;
        }
    };
    let completed_at = tracedecay_domain::UtcMicros(crate::tracedecay::current_timestamp());
    let Some(vector_runtime) =
        tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
            graph.project_root(),
        )
    else {
        log_code_index_scope_reconciliation_degraded("vector_writer_unavailable");
        return false;
    };
    // Configuration activation, vector publication, and source-scope
    // collection share this mutation fence. Root inventories are read twice
    // under it and compared exactly before quarantine.
    let _vector_writer = vector_runtime.freeze_vector_mutations().await;
    let initial_inputs =
        match collect_scope_root_proof_inputs(graph, schedulers, &vector_receipt).await {
            Ok(inputs) => inputs,
            Err(failure) => {
                log_code_index_scope_reconciliation_degraded(failure);
                return false;
            }
        };
    let plan_root = store_root.clone();
    let plan_live_roots = initial_inputs.live_roots.clone();
    let plan = tokio::task::spawn_blocking(move || {
        recover_scope_root_retention(&plan_root)
            .map_err(|_| "scope_reconciliation_recovery_failed")?;
        plan_scope_root_retention(
            &plan_root,
            &plan_live_roots,
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            now_secs,
        )
        .map_err(|_| "scope_reconciliation_pass_failed")
    })
    .await;
    let plan = match plan {
        Ok(Ok(plan)) => plan,
        Ok(Err(failure)) => {
            log_code_index_scope_reconciliation_degraded(failure);
            return false;
        }
        Err(_) => {
            log_code_index_scope_reconciliation_degraded("scope_reconciliation_task_panicked");
            return false;
        }
    };
    if plan.collectable_scopes.is_empty() {
        return true;
    }

    let candidates = plan.collectable_scopes.clone();
    let start = usize::try_from(now_secs)
        .ok()
        .map_or(0, |now| now % candidates.len());
    let mut selected = None;
    const MAX_SCOPE_LIVENESS_CHECKS_PER_PASS: usize = 32;
    for offset in 0..candidates.len().min(MAX_SCOPE_LIVENESS_CHECKS_PER_PASS) {
        let candidate = &candidates[(start + offset) % candidates.len()];
        match crate::daemon::code_index_scheduler::semantic_vector_graph::project_vector_code_scope_is_live(
            schedulers,
            graph.project_root(),
            &candidate.scope_hash,
            vector_receipt.revision,
        )
        .await
        {
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Ready {
                source_scope,
                live: false,
            } => {
                selected = Some((candidate.clone(), source_scope));
                break;
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Ready {
                live: true,
                ..
            } => {}
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Missing => {
                log_code_index_scope_reconciliation_degraded("vector_scope_binding_missing");
                return false;
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Unavailable(
                reason,
            ) => {
                log_code_index_scope_reconciliation_degraded(&format!(
                    "vector_scope_unavailable:{reason}"
                ));
                return false;
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Denied(
                reason,
            ) => {
                log_code_index_scope_reconciliation_degraded(&format!(
                    "vector_scope_denied:{reason}"
                ));
                return false;
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::ResetRequired(
                reason,
            ) => {
                log_code_index_scope_reconciliation_degraded(&format!(
                    "vector_scope_reset_required:{reason}"
                ));
                return false;
            }
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Corrupt(
                reason,
            ) => {
                log_code_index_scope_reconciliation_degraded(&format!(
                    "vector_scope_corrupt:{reason}"
                ));
                return false;
            }
        }
    }
    let Some((candidate, source_scope)) = selected else {
        return true;
    };
    let planned_proof = match initial_inputs.bind_candidate(
        candidate.scope_hash.clone(),
        source_scope.clone(),
        vector_receipt.revision,
    ) {
        Ok(proof) => proof,
        Err(failure) => {
            log_code_index_scope_reconciliation_degraded(failure);
            return false;
        }
    };
    let proof_root = store_root.clone();
    let proof_for_plan = planned_proof.clone();
    let plan = match tokio::task::spawn_blocking(move || {
        plan_scope_root_retention_with_liveness_proof(
            &proof_root,
            proof_for_plan,
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            now_secs,
        )
        .map_err(|_| "scope_proof_bound_plan_failed")
    })
    .await
    {
        Ok(Ok(plan)) => plan,
        Ok(Err(failure)) => {
            log_code_index_scope_reconciliation_degraded(failure);
            return false;
        }
        Err(_) => {
            log_code_index_scope_reconciliation_degraded("scope_reconciliation_task_panicked");
            return false;
        }
    };

    // Re-read every terminal authority and the exact source binding after
    // planning. This is the compare-and-swap immediately preceding quarantine.
    let revalidated_inputs =
        match collect_scope_root_proof_inputs(graph, schedulers, &vector_receipt).await {
            Ok(inputs) => inputs,
            Err(failure) => {
                log_code_index_scope_reconciliation_degraded(failure);
                return false;
            }
        };
    let revalidated_source_scope =
        match crate::daemon::code_index_scheduler::semantic_vector_graph::project_vector_code_scope_is_live(
            schedulers,
            graph.project_root(),
            &candidate.scope_hash,
            vector_receipt.revision,
        )
        .await
        {
            crate::daemon::code_index_scheduler::semantic_vector_graph::ProjectSemanticVectorCodeScopeLiveness::Ready {
                source_scope,
                live: false,
            } => source_scope,
            _ => {
                log_code_index_scope_reconciliation_degraded(
                    "scope_candidate_changed_before_quarantine",
                );
                return false;
            }
        };
    if revalidated_source_scope != source_scope {
        log_code_index_scope_reconciliation_degraded(
            "scope_candidate_binding_changed_before_quarantine",
        );
        return false;
    }
    let revalidated_proof = match revalidated_inputs.bind_candidate(
        candidate.scope_hash.clone(),
        revalidated_source_scope,
        vector_receipt.revision,
    ) {
        Ok(proof) => proof,
        Err(failure) => {
            log_code_index_scope_reconciliation_degraded(failure);
            return false;
        }
    };
    if revalidated_proof != planned_proof {
        log_code_index_scope_reconciliation_degraded(
            "scope_liveness_authority_changed_before_quarantine",
        );
        return false;
    }
    tracedecay_usecases::semantic_runtime::retain_project_semantic_code_sources(
        graph.project_root(),
        &revalidated_inputs.vector_sources,
    );

    let execute_root = code_index_scope_store_root(&layout.data_root);
    let intent_scope = candidate.scope_hash.clone();
    let intent_source_scope = source_scope.clone();
    let proof_for_execute = revalidated_proof.clone();
    let report = tokio::task::spawn_blocking(move || {
        prepare_scope_root_binding_cleanup(
            &execute_root,
            &plan,
            &intent_scope,
            &intent_source_scope,
            &proof_for_execute,
            completed_at,
        )
        .map_err(|_| "scope_binding_cleanup_prepare_failed")?;
        execute_scope_root_retention(
            &execute_root,
            plan,
            &proof_for_execute,
            CodeGenerationRetentionModeV1::Apply,
            now_secs,
            completed_at,
        )
        .map_err(|_| "scope_reconciliation_pass_failed")
    })
    .await;

    match report {
        Ok(Ok(report)) => {
            let reclaimed = report
                .receipt
                .as_ref()
                .map_or(0, |receipt| receipt.reclaimed_bytes);
            if reclaimed > 0 || report.plan.stranded_scope_count() > 0 {
                log_daemon_event(
                    "retention_code_index_scopes",
                    &[
                        ("store", "code-index-v1".to_string()),
                        ("live_scopes", report.plan.live_scope_count.to_string()),
                        (
                            "stranded_scopes",
                            report.plan.stranded_scope_count().to_string(),
                        ),
                        (
                            "stranded_bytes",
                            report.plan.stranded_scope_bytes().to_string(),
                        ),
                        (
                            "retained_immature_scopes",
                            report.plan.retained_immature_scopes.len().to_string(),
                        ),
                        (
                            "refused_scopes",
                            report.plan.refused_scopes.len().to_string(),
                        ),
                        (
                            "collected_scopes",
                            report.collected_scopes.len().to_string(),
                        ),
                        ("bytes_reclaimed", reclaimed.to_string()),
                    ],
                );
            }
            // The durable intent is deliberately completed on the next cadence.
            // A daemon restart at this boundary exercises exactly the same
            // replay path as an ordinary subsequent tick.
            report.collected_scopes.is_empty()
        }
        Ok(Err(failure)) => {
            log_code_index_scope_reconciliation_degraded(failure);
            false
        }
        Err(_) => {
            log_code_index_scope_reconciliation_degraded("scope_reconciliation_task_panicked");
            false
        }
    }
}

/// Bounded read-only projection of gix's registered worktree roots.
///
/// Scope directories under one `data_root` all belong to one repository: linked
/// worktrees share a git common directory and therefore one project store, and
/// differ only by the per-worktree canonical root the scope hash is derived
/// Apply never uses this projection by itself: scope collection combines it
/// with durable project enrollment, mounted leases, configuration roots,
/// vector dependencies, and the exact source binding in one proof receipt.
///
/// Every failure is an `Err`, never a smaller set: a truncated live set is
/// indistinguishable from stranding and would authorize deletion.
pub(super) fn resolve_live_code_index_roots(
    project_root: &Path,
) -> Result<std::collections::BTreeSet<PathBuf>, &'static str> {
    git_worktree_root_inventory(project_root).map(|(roots, _)| roots)
}

/// Record both the literal path and its symlink-resolved form. The scope hash
/// is taken over the canonical root string recorded at publication time, and a
/// live root spelled differently must never be mistaken for a dead one.
fn insert_live_root_variants(roots: &mut std::collections::BTreeSet<PathBuf>, root: &Path) {
    roots.insert(root.to_path_buf());
    if let Ok(resolved) = std::fs::canonicalize(root) {
        roots.insert(resolved);
    }
}

/// The shared `code-index-v1/` parent that holds every scope root for one
/// repository. Scope reconciliation operates here; generation retention
/// operates one level down.
pub(super) fn code_index_scope_store_root(data_root: &Path) -> PathBuf {
    data_root.join("code-index-v1")
}

/// Durable failure visibility for scope reconciliation. Every refusal names why
/// so a fail-closed pass is never mistaken for "nothing was stranded".
fn log_code_index_scope_reconciliation_degraded(failure: &str) {
    log_daemon_event(
        "retention_degraded",
        &[
            ("pass", "code_index_scopes".to_string()),
            ("failure", failure.to_string()),
        ],
    );
}

/// The exact per-project code-index store root this cadence sweeps.
///
/// This must stay the scoped root the scheduler publishes into and Doctor
/// reports on. A cadence pointed at any other directory would find no sealed
/// generations and silently reclaim nothing, which is the failure this pass
/// exists to end.
pub(super) fn code_index_store_root(data_root: &Path, project_root: &Path) -> PathBuf {
    crate::retention::code_index_generations::scoped_code_index_store_root(
        &code_index_scope_store_root(data_root),
        project_root,
    )
}

pub(super) async fn run_session_retention(
    database: &crate::global_db::RegisteredGlobalDb,
    config: &RetentionConfig,
) -> bool {
    let now = match now_secs_i64() {
        Ok(now) => now,
        Err(failure) => {
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "session_retention".to_owned()),
                    ("failure", failure.to_owned()),
                ],
            );
            return false;
        }
    };
    let mut succeeded = true;

    if config.session_lcm.enabled {
        match database
            .run_session_lcm_retention(
                "all",
                None,
                &config.session_lcm,
                tracedecay_sessions::runtime::lcm::RetentionMode::Apply,
                now,
            )
            .await
        {
            Ok(report) => {
                let reclaimed = report.bytes_reclaimed();
                succeeded &= report.errors.is_empty();
                if reclaimed > 0 || !report.errors.is_empty() {
                    log_daemon_event(
                        "retention_session_lcm",
                        &[
                            ("store", "mounted_sessions".to_string()),
                            ("bytes_reclaimed", reclaimed.to_string()),
                            ("errors", report.errors.len().to_string()),
                        ],
                    );
                }
            }
            Err(_) => {
                succeeded = false;
                log_daemon_event(
                    "retention_degraded",
                    &[
                        ("pass", "session_lcm".to_string()),
                        ("failure", "retention_pass_failed".to_string()),
                    ],
                );
            }
        }
    }

    if config.observation.enabled {
        match database
            .run_observation_retention(
                None,
                &config.observation,
                crate::global_db::observation::retention::RetentionMode::Apply,
                now,
            )
            .await
        {
            Ok(report) => {
                let reclaimed = report.bytes_reclaimed();
                succeeded &= report.errors.is_empty();
                if reclaimed > 0 || !report.errors.is_empty() {
                    log_daemon_event(
                        "retention_observation",
                        &[
                            ("store", "mounted_sessions".to_string()),
                            ("bytes_reclaimed", reclaimed.to_string()),
                            ("errors", report.errors.len().to_string()),
                        ],
                    );
                }
                // Per-pass admission-work receipts accumulated on the refusal
                // markers: the in-product signal for collision re-admission
                // churn that previously required perf(1) to diagnose.
                let admission_work = &report.admission_work;
                if admission_work.refusal_markers > 0 {
                    log_daemon_event(
                        "observation_admission_work",
                        &[
                            ("store", "mounted_sessions".to_string()),
                            (
                                "refusal_markers",
                                admission_work.refusal_markers.to_string(),
                            ),
                            (
                                "stored_rows_decoded",
                                admission_work.stored_rows_decoded.to_string(),
                            ),
                            (
                                "identity_derivations",
                                admission_work.identity_derivations.to_string(),
                            ),
                            (
                                "payload_digests",
                                admission_work.payload_digests.to_string(),
                            ),
                            (
                                "runtime_commands",
                                admission_work.runtime_commands.to_string(),
                            ),
                        ],
                    );
                }
            }
            Err(_) => {
                succeeded = false;
                log_daemon_event(
                    "retention_degraded",
                    &[
                        ("pass", "observation".to_string()),
                        ("failure", "retention_pass_failed".to_string()),
                    ],
                );
            }
        }
    }

    succeeded &= run_observability_analytics_retention(database, "mounted_sessions").await;

    if let Some(compaction) = &config.compaction {
        succeeded &= run_compaction(
            RetainedCompactionStore::Registered(database),
            "mounted_sessions",
            compaction,
        )
        .await;
    }
    succeeded
}

pub(super) async fn run_observability_analytics_retention(
    database: &crate::global_db::RegisteredGlobalDb,
    store: &'static str,
) -> bool {
    let now = match now_secs_i64() {
        Ok(now) => now,
        Err(failure) => {
            // An unreadable clock must defer the pass, never fabricate a
            // pruning horizon.
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "observability_analytics".to_string()),
                    ("failure", failure.to_string()),
                ],
            );
            return false;
        }
    };
    match database.prune_observability_events(now).await {
        Ok(receipt) => {
            if receipt.expired_detail > 0 || receipt.expired_rollup > 0 {
                log_daemon_event(
                    "retention_observability_analytics",
                    &[
                        ("store", store.to_owned()),
                        ("expired_detail", receipt.expired_detail.to_string()),
                        ("expired_rollup", receipt.expired_rollup.to_string()),
                    ],
                );
            }
            true
        }
        Err(_) => {
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "observability_analytics".to_owned()),
                    ("failure", "retention_pass_failed".to_owned()),
                ],
            );
            false
        }
    }
}

pub(super) async fn run_global_compaction(
    database: &crate::global_db::RegisteredGlobalDb,
    config: &CompactionThresholdConfig,
) -> bool {
    run_compaction(
        RetainedCompactionStore::Registered(database),
        "global.db",
        config,
    )
    .await
}

pub(super) async fn run_project_compaction(
    database: &crate::db::Database,
    config: &CompactionThresholdConfig,
) -> bool {
    run_compaction(
        RetainedCompactionStore::Project(database),
        crate::config::DB_FILENAME,
        config,
    )
    .await
}

enum RetainedCompactionStore<'a> {
    Registered(&'a crate::global_db::RegisteredGlobalDb),
    Project(&'a crate::db::Database),
}

impl RetainedCompactionStore<'_> {
    async fn storage_page_counts(&self) -> crate::errors::Result<(u64, u64, u64)> {
        match self {
            Self::Registered(database) => database.storage_page_counts().await,
            Self::Project(database) => database.storage_page_counts().await,
        }
    }

    async fn run_bounded_incremental_compaction(
        &self,
        max_pages: u64,
    ) -> crate::errors::Result<()> {
        match self {
            Self::Registered(database) => {
                database.run_bounded_incremental_compaction(max_pages).await
            }
            Self::Project(database) => database.run_incremental_vacuum(max_pages).await,
        }
    }
}

/// Samples the store's free-page ratio and, when the owner-configured threshold
/// is met, schedules a bounded incremental vacuum in the deferred background
/// lane (Plan 38 §6). The placement is structurally forbidden from competing
/// with foreground writes; the page cap keeps the reclaim off the hot path.
async fn run_compaction(
    store: RetainedCompactionStore<'_>,
    store_name: &'static str,
    config: &CompactionThresholdConfig,
) -> bool {
    let Ok((page_size, page_count, freelist)) = store.storage_page_counts().await else {
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "compaction".to_string()),
                ("failure", "store_size_sample_failed".to_string()),
            ],
        );
        return false;
    };
    let Ok(scheduled) = compaction_is_scheduled(page_size, page_count, freelist, config) else {
        return false;
    };
    if !scheduled {
        return true;
    }
    let pages = config.max_pages_per_tick.max(1);
    let freelist_before = freelist;
    if store
        .run_bounded_incremental_compaction(u64::from(pages))
        .await
        .is_err()
    {
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "compaction".to_string()),
                ("failure", "incremental_vacuum_failed".to_string()),
            ],
        );
        return false;
    }
    let Ok((_, _, freelist_after)) = store.storage_page_counts().await else {
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "compaction".to_string()),
                ("failure", "post_compaction_sample_failed".to_string()),
            ],
        );
        return false;
    };
    log_compaction(store_name, freelist_before, freelist_after);
    true
}

fn compaction_is_scheduled(
    page_size: u64,
    page_count: u64,
    freelist: u64,
    config: &CompactionThresholdConfig,
) -> Result<bool, ()> {
    use tracedecay_application::storage::compaction::CompactionTriggerPolicyV1;
    use tracedecay_application::storage::identity::{
        FreePageRatioV1, StorageByteSizeV1, StoreKeyV1,
    };
    use tracedecay_application::storage::telemetry::StoreSizeSampleV1;
    use tracedecay_domain::UtcMicros;

    if page_size == 0 || page_count == 0 {
        return Ok(false);
    }
    let store_key = StoreKeyV1::new("store.db").map_err(|_| ())?;
    let page_size_bytes = u32::try_from(page_size).map_err(|_| ())?;
    let sample = StoreSizeSampleV1 {
        store: store_key,
        page_size_bytes,
        page_count,
        freelist_pages: freelist,
        observed_at: UtcMicros(
            now_secs_i64()
                .map_err(|_| ())?
                .checked_mul(1_000_000)
                .ok_or(())?,
        ),
    };
    let threshold = FreePageRatioV1::new(config.free_page_ratio_threshold).map_err(|_| ())?;
    let policy = CompactionTriggerPolicyV1 {
        free_page_ratio_threshold: threshold,
        minimum_reclaimable_bytes: StorageByteSizeV1(config.minimum_reclaimable_bytes),
    };
    policy
        .decide(&sample)
        .map(|decision| decision.is_scheduled())
        .map_err(|_| ())
}

fn log_compaction(store_name: &'static str, freelist_before: u64, freelist_after: u64) {
    log_daemon_event(
        "retention_compaction",
        &[
            ("store", store_name.to_string()),
            (
                "freed_pages",
                freelist_before.saturating_sub(freelist_after).to_string(),
            ),
        ],
    );
}

/// Runs bounded incremental-vacuum compaction over every tracked branch
/// database other than the one `cg` currently has mounted (that store already
/// goes through [`run_project_compaction`]). Best-effort and independent per
/// file: a busy or failing branch database never blocks the rest, but keeps
/// the maintenance cadence retry-eligible — see
/// `src/retention/branch_compaction.rs` for the compaction policy itself.
pub(super) async fn run_branch_compaction(
    cg: &TraceDecay,
    config: &CompactionThresholdConfig,
) -> bool {
    let layout = cg.store_layout();
    let Some(meta) = crate::branch_meta::load_branch_meta(&layout.data_root) else {
        return true;
    };
    let active_db_path = layout.graph_db_path.clone();
    let candidates = crate::retention::branch_compaction::select_branch_db_candidates(
        &layout.data_root,
        &meta,
        &active_db_path,
    );
    if candidates.is_empty() {
        return true;
    }
    let report = crate::retention::branch_compaction::compact_branch_databases(&candidates, config);
    if report.policy_invalid {
        // Never silent: an out-of-range threshold disables the pass entirely
        // and would otherwise be indistinguishable from "nothing to compact".
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "branch_compaction".to_string()),
                ("failure", "invalid_compaction_policy".to_string()),
                (
                    "free_page_ratio_threshold",
                    config.free_page_ratio_threshold.to_string(),
                ),
            ],
        );
        return false;
    }
    if report.compacted.is_empty() && report.skipped.is_empty() {
        return true;
    }
    let freed_pages: u64 = report
        .compacted
        .iter()
        .map(|outcome| outcome.freed_pages)
        .sum();
    let unreclaimable = report
        .skipped
        .iter()
        .filter(|skip| {
            skip.reason
                == crate::retention::branch_compaction::BranchCompactionSkipReason::IncrementalVacuumUnavailable
        })
        .count();
    log_daemon_event(
        "retention_branch_compaction",
        &[
            ("project", cg.project_root().display().to_string()),
            ("compacted", report.compacted.len().to_string()),
            ("freed_pages", freed_pages.to_string()),
            ("skipped", report.skipped.len().to_string()),
            // Branch databases predating `auto_vacuum = INCREMENTAL`: their
            // free pages need a full VACUUM this pass deliberately avoids.
            ("unreclaimable", unreclaimable.to_string()),
        ],
    );
    branch_compaction_succeeded(&report)
}

pub(super) fn branch_compaction_succeeded(
    report: &crate::retention::branch_compaction::BranchCompactionReport,
) -> bool {
    !report.policy_invalid && report.skipped.is_empty()
}

#[cfg(test)]
mod observability_retention_tests {
    use crate::global_db::{AnalyticsEventInsert, AnalyticsEventQuery};

    use super::*;

    #[tokio::test]
    async fn session_maintenance_calls_observability_analytics_retention() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let harness = crate::global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "daemon-observability-retention",
        )
        .await;
        harness
            .registered
            .append_observability_event(&AnalyticsEventInsert {
                provider: "tracedecay-observability".to_owned(),
                project_id: "scope:retention".to_owned(),
                session_id: None,
                timestamp: 0,
                event_kind: "retrieval.query.completed.v1".to_owned(),
                hook_name: None,
                tool_name: None,
                tool_category: None,
                skill_name: None,
                hint_category: None,
                hint_id: Some("retention:event:1".to_owned()),
                outcome: Some("succeeded".to_owned()),
                metadata_json: Some(
                    serde_json::json!({
                        "retention_class": "optional_local_detail30d"
                    })
                    .to_string(),
                ),
            })
            .await
            .expect("append old observability detail");
        let mut config = RetentionConfig::default();
        config.session_lcm.enabled = false;
        config.observation.enabled = false;
        config.compaction = None;

        assert!(run_session_retention(&harness.registered, &config).await);
        let rows = harness
            .registered
            .query_analytics_events(&AnalyticsEventQuery {
                provider: Some("tracedecay-observability".to_owned()),
                project_id: Some("scope:retention".to_owned()),
                limit: 10,
                ..AnalyticsEventQuery::default()
            })
            .await
            .expect("query retained observability detail");
        assert!(
            rows.is_empty(),
            "maintenance must invoke analytics retention"
        );
    }
}
