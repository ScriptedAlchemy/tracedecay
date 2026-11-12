//! Retention, compaction, and garbage-collection operations run by the daemon
//! maintenance owner.
//!
//! Every operation that opens or garbage-collects a store lives here so its
//! `StoreAdministration` lifetime is kept separate from the watcher state
//! machine. The git watcher itself never opens or mutates a store: it routes
//! exact-frontier freshness requests to the code-index scheduler and wakes the
//! maintenance owner.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::lease::ProjectStoreMaintenanceLeaseV1;
use crate::telemetry::StoreTelemetrySamplingRegistry;
use tracedecay_code_index_retention::code_index_generations::{
    CodeIndexScopeV1, code_index_scopes, code_index_store_root,
};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_domain::CodeGenerationId;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::logging::log_daemon_event;
use tracedecay_runtime_core::native_runtime;

mod graph_replay;
mod scope_reconciliation;
use graph_replay::log_code_generation_retention_degraded;
#[cfg(test)]
pub(crate) mod registered_tests;
pub use scope_reconciliation::{run_code_index_scope_reconciliation, scheduler_owner_roots};

/// Outcome of one bounded code-generation retention pass.
///
/// `MoreWork` reports bounded progress with a remaining backlog, another
/// collectable superseded generation, superseded bytes a transient holder
/// (serving seat, in-flight text replacement) is about to release,
/// unconsumed graph-replay release evidence, or a consumed release whose
/// freed segments the pass's earlier census missed, so the maintenance owner
/// keeps the short cadence until the store converges instead of parking
/// multi-GiB debris behind the full maintenance interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeGenerationRetentionOutcomeV1 {
    Complete,
    MoreWork,
    Failed,
}

impl CodeGenerationRetentionOutcomeV1 {
    /// The outcome of two independent passes over one tick: a failure keeps
    /// the retry cadence, and any remaining work keeps the short cadence.
    #[must_use]
    pub const fn with_pass(self, other: Self) -> Self {
        match (self, other) {
            (Self::Failed, _) | (_, Self::Failed) => Self::Failed,
            (Self::MoreWork, _) | (_, Self::MoreWork) => Self::MoreWork,
            (Self::Complete, Self::Complete) => Self::Complete,
        }
    }
}

/// Exact liveness roots for one code-index scope beyond the active pointer,
/// which the planner marks itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeGenerationProtectionV1 {
    /// The generations a mounted scheduler serves for the root, sealed and
    /// text slots. Empty when the root is not mounted in this daemon.
    pub serving: BTreeSet<CodeGenerationId>,
    /// `serving` plus every live native-preview candidate binding of the
    /// root's repository.
    pub sources: BTreeSet<CodeGenerationId>,
}

/// Why the protection set of a root could not be resolved. Retention and the
/// storage report refuse to plan without it rather than guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeGenerationProtectionUnavailableV1 {
    /// The root yields no repository identity.
    RepositoryIdentity,
    /// The native-integration binding inventory could not be read.
    NativeBindings,
}

impl CodeGenerationProtectionUnavailableV1 {
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::RepositoryIdentity => "repository_identity_unavailable",
            Self::NativeBindings => "native_bindings_unavailable",
        }
    }
}

/// Resolve the protection set daemon retention plans against for one root.
///
/// The planner marks only the active pointer head itself. This adds the
/// generation the mounted scheduler is serving and every live native-preview
/// candidate binding, because both can bind a generation the active pointer
/// alone cannot name. Native previews bind retained-only candidate generations
/// between preflight and terminal apply; their durable commitments are
/// liveness roots. The bindings are keyed by repository, a pure function of
/// the checkout's git common dir, so a root that is not mounted in this daemon
/// still resolves them.
#[tracing::instrument(
    name = "daemon.git.maintenance.code_generation_protection",
    level = "trace",
    skip_all
)]
pub async fn code_generation_protection(
    schedulers: &CodeIndexSchedulerRegistryV1,
    profile_database: &RegisteredGlobalDb,
    project_root: &Path,
) -> Result<CodeGenerationProtectionV1, CodeGenerationProtectionUnavailableV1> {
    let serving = serving_generation_pins(schedulers, project_root).await;
    let repository_id = match schedulers.serving_code_scope(project_root).await {
        Some(serving_scope) => serving_scope.repository_id,
        None => tracedecay_code_index_runtime::code_index_scheduler::identity::repository_id_for(
            project_root,
        )
        .map_err(|_| CodeGenerationProtectionUnavailableV1::RepositoryIdentity)?,
    };
    let native_pins = tracedecay_global_db::GlobalDbNativeIntegrationStore::new(profile_database)
        .live_candidate_generation_bindings(
            &repository_id,
            tracedecay_contracts::clock::now_micros(),
        )
        .await
        .map_err(|_| CodeGenerationProtectionUnavailableV1::NativeBindings)?;
    let mut sources = serving.clone();
    sources.extend(native_pins);
    Ok(CodeGenerationProtectionV1 { serving, sources })
}

/// Collect superseded code-index generations for one mounted project.
///
/// Sealed generations are ordinary files, so no database retention or
/// compaction pass reclaims them; this runs on the ordinary maintenance
/// cadence against [`code_generation_protection`]. A root that yields no
/// repository identity or an unreadable binding inventory fails the pass
/// closed instead of sweeping blind.
#[tracing::instrument(
    name = "daemon.git.maintenance.code_generation_retention",
    level = "trace",
    skip_all
)]
pub async fn run_code_generation_retention(
    lease: &ProjectStoreMaintenanceLeaseV1,
    schedulers: &CodeIndexSchedulerRegistryV1,
    observations: &StoreTelemetrySamplingRegistry,
    cancellation: &CancellationToken,
) -> CodeGenerationRetentionOutcomeV1 {
    if cancellation.is_cancelled() {
        log_code_generation_retention_degraded(observations, "retention_cancelled");
        return CodeGenerationRetentionOutcomeV1::Failed;
    }
    let layout = lease.store_layout();
    let store_root = code_index_store_root(&layout.data_root, &layout.project_root);
    // A store directory that never materialized has nothing to sweep. A store
    // *without* an active pointer is different: it is crash debris from a
    // publish that never reached its pointer write (an OOM-killed rebuild is
    // the ordinary cause), and the planner collects it as a typed unpublished
    // store, before this, such orphaned partial generations were unreachable
    // by every retention pass while their worktree root stayed live.
    if !store_root.is_dir() {
        return CodeGenerationRetentionOutcomeV1::Complete;
    }
    let protection = match code_generation_protection(
        schedulers,
        lease.profile_database().as_ref(),
        &layout.project_root,
    )
    .await
    {
        Ok(protection) => protection,
        Err(unavailable) => {
            log_code_generation_retention_degraded(observations, unavailable.reason());
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
    };
    let project = match &lease.graph_db().registered_binding().shard_id.scope {
        tracedecay_store::StoreShardScopeV1::Project { project_id } => project_id.clone(),
        _ => {
            log_code_generation_retention_degraded(observations, "registered_project_authority_unavailable");
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
    };
    let storage = match lease.graph_db().project_code_index_storage(schedulers.process_resident_memory(), &project) {
        Ok(storage) => storage,
        Err(error) => {
            log_code_generation_retention_degraded(observations, &error.to_string());
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
    };
    let worktree = match tracedecay_runtime_core::scope_identity::worktree_id_for(&layout.project_root) {
        Ok(worktree) => worktree,
        Err(error) => {
            log_code_generation_retention_degraded(observations, &error.to_string());
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
    };
    let stage_cancellation = cancellation.clone();
    let graph_storage = storage.clone();
    let graph_worktree = worktree.clone();
    let graph_protected = protection.sources.clone();
    match native_runtime::spawn_blocking(move || {
        tracedecay_code_index_retention::code_index_generations::retain_registered_code_index(
            &storage, &worktree, &protection.sources, &|| stage_cancellation.is_cancelled(),
        )
    }).await {
        Ok(Ok(true)) => {
            match finish_registered_retirements(
                &graph_storage, &graph_worktree, &graph_protected,
                lease.store_runtime(), lease.graph_db(), &project, cancellation,
            ).await {
                Ok(()) => CodeGenerationRetentionOutcomeV1::MoreWork,
                Err(error) => {
                    log_code_generation_retention_degraded(observations, &error);
                    CodeGenerationRetentionOutcomeV1::Failed
                }
            }
        }
        Ok(Ok(false)) => CodeGenerationRetentionOutcomeV1::Complete,
        result => {
            log_code_generation_retention_degraded(observations, &format!("{result:?}"));
            CodeGenerationRetentionOutcomeV1::Failed
        }
    }
}

async fn finish_registered_retirements(
    storage: &tracedecay_runtime_core::db::CodeIndexSqliteStorage,
    worktree: &tracedecay_domain::WorktreeId,
    protected: &BTreeSet<CodeGenerationId>,
    runtime: &tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1,
    database: &tracedecay_runtime_core::db::Database,
    project: &tracedecay_domain::ProjectId,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let read_storage = storage.clone();
    let read_worktree = worktree.clone();
    let entries = native_runtime::spawn_blocking(move || {
        read_storage.generation_page(&read_worktree, None)
    }).await?.map_err(|error| error.to_string())?;
    for entry in entries.records() {
        if cancellation.is_cancelled() { return Err("retirement cancelled".to_owned()); }
        if protected.contains(&entry.key.generation_id) { continue; }
        let read_storage = storage.clone();
        let key = entry.key.clone();
        let pending = native_runtime::spawn_blocking(move || {
            read_storage.pending_retirement(&key)
        }).await?.map_err(|error| error.to_string())?;
        let Some((operation, _receipt)) = pending else { continue; };
        let digest = tracedecay_domain::sha256_hex_suffix(entry.manifest.digest.as_str())
            .ok_or_else(|| "retirement digest is not canonical".to_owned())?;
        if runtime.reconcile_deleted_code_generation_graph_replays(
            project.clone(), database, &entry.key.generation_id,
            &format!("generation-{digest}.json"), cancellation,
        ).await.map_err(|error| error.to_string())? {
            let storage = storage.clone();
            let key = entry.key.clone();
            native_runtime::spawn_blocking(move || storage.complete_retirement(&key, &operation))
                .await?.map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// The checkout an unmounted scope belongs to, or `None` when the scope is
/// mounted, no record proves its root (only the primary scope's root is known
/// without one), or the recorded checkout is gone.
fn unmounted_scope_root(
    store: &RegisteredProjectStoreV1,
    scope: &CodeIndexScopeV1,
    mounted_store_roots: &BTreeSet<PathBuf>,
) -> Option<PathBuf> {
    if mounted_store_roots.contains(&scope.store_root) {
        return None;
    }
    let root = scope.recorded_root.clone().or_else(|| {
        (scope.store_root == code_index_store_root(&store.data_root, &store.canonical_root))
            .then(|| store.canonical_root.clone())
    })?;
    root.exists().then_some(root)
}

/// One registered project's profile shard, for a retention pass that runs
/// whether or not any of its worktrees is mounted in this daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredProjectStoreV1 {
    pub project_id: tracedecay_domain::ProjectId,
    /// `projects/<project id>/` under the profile root.
    pub data_root: PathBuf,
    /// The registry's canonical root: the primary checkout.
    pub canonical_root: PathBuf,
}

/// Collect superseded generations in every scope of a registered project that
/// no mounted graph owns.
///
/// Mounted scopes are skipped: their pass runs with the mounted graph lease.
/// An unmounted scope has no graph lease, so the graph-replay release queue
/// the batch appends to waits for the project's next mount; collection is
/// still exact, because retention hard-links each retired generation into the
/// replay pool before its release event is durable, so the graph can always
/// finish that retirement later. Text artifacts, the largest superseded
/// family, are reclaimed by the batch or, once its index rewrite frees them,
/// by the continuation pass that follows.
///
/// The protection set is resolved per scope at pass time, so a scope mounted
/// after the tick selected it still protects what its scheduler serves, and
/// the executor's pointer compare-and-swap refuses a batch a concurrent
/// publication invalidated. A scope whose root no record proves, or whose
/// recorded checkout is gone, is left to stranded-scope reconciliation.
#[tracing::instrument(
    name = "daemon.git.maintenance.registered_code_generation_retention",
    level = "trace",
    skip_all
)]
pub async fn run_registered_code_generation_retention(
    store: &RegisteredProjectStoreV1,
    mounted_store_roots: &BTreeSet<PathBuf>,
    schedulers: &CodeIndexSchedulerRegistryV1,
    profile_database: &RegisteredGlobalDb,
    observations: &StoreTelemetrySamplingRegistry,
    cancellation: &CancellationToken,
    project_database: &tracedecay_runtime_core::db::Database,
    store_runtime: &tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1,
) -> CodeGenerationRetentionOutcomeV1 {
    let scopes = match code_index_scopes(&store.data_root) {
        Ok(scopes) => scopes,
        Err(error) => {
            observations.mark_loud_retention_log();
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "code_generations".to_string()),
                    ("failure", "scope_inventory_unavailable".to_string()),
                    ("error", error.to_string()),
                ],
            );
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
    };
    let storage = match project_database.project_code_index_storage(
        schedulers.process_resident_memory(), &store.project_id,
    ) {
        Ok(storage) => storage,
        Err(error) => {
            log_code_generation_retention_degraded(observations, &error.to_string());
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
    };
    let mut outcome = CodeGenerationRetentionOutcomeV1::Complete;
    for scope in scopes {
        if cancellation.is_cancelled() {
            log_code_generation_retention_degraded(observations, "retention_cancelled");
            return CodeGenerationRetentionOutcomeV1::Failed;
        }
        let Some(root) = unmounted_scope_root(store, &scope, mounted_store_roots) else {
            continue;
        };
        let protection = match code_generation_protection(schedulers, profile_database, &root).await
        {
            Ok(protection) => protection,
            Err(unavailable) => {
                log_code_generation_retention_degraded(observations, unavailable.reason());
                outcome = outcome.with_pass(CodeGenerationRetentionOutcomeV1::Failed);
                continue;
            }
        };
        let worktree = match tracedecay_runtime_core::scope_identity::worktree_id_for(&root) {
            Ok(worktree) => worktree,
            Err(error) => {
                log_code_generation_retention_degraded(observations, &error.to_string());
                outcome = outcome.with_pass(CodeGenerationRetentionOutcomeV1::Failed);
                continue;
            }
        };
        let graph_worktree = worktree.clone();
        let graph_protected = protection.sources.clone();
        let staged_storage = storage.clone();
        let stage_cancellation = cancellation.clone();
        match native_runtime::spawn_blocking(move || {
            tracedecay_code_index_retention::code_index_generations::retain_registered_code_index(
                &staged_storage, &worktree, &protection.sources, &|| stage_cancellation.is_cancelled(),
            )
        }).await {
            Ok(Ok(true)) => {
                match finish_registered_retirements(
                    &storage, &graph_worktree, &graph_protected, store_runtime,
                    project_database, &store.project_id, cancellation,
                ).await {
                    Ok(()) => outcome = outcome.with_pass(CodeGenerationRetentionOutcomeV1::MoreWork),
                    Err(error) => {
                        log_code_generation_retention_degraded(observations, &error);
                        outcome = outcome.with_pass(CodeGenerationRetentionOutcomeV1::Failed);
                    }
                }
            }
            Ok(Ok(false)) => {}
            result => {
                log_code_generation_retention_degraded(observations, &format!("{result:?}"));
                outcome = outcome.with_pass(CodeGenerationRetentionOutcomeV1::Failed);
            }
        }
    }
    outcome
}

/// The generation the mounted scheduler is currently serving, when one is
/// mounted at all.
#[tracing::instrument(
    name = "daemon.git.maintenance.serving_generation_pins",
    level = "trace",
    skip_all
)]
async fn serving_generation_pins(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> std::collections::BTreeSet<tracedecay_domain::CodeGenerationId> {
    let mut pins = std::collections::BTreeSet::new();
    if let Some(scope) = schedulers.serving_code_scope(project_root).await {
        if let Some(serving) = scope.serving_generation {
            pins.insert(serving.manifest().generation_id.clone());
        }
        // The seated graph outlives both serving slots across a refresh: the
        // successor's graph builds over its sealed store, so collecting it
        // first turns an incremental publication into a full rebuild.
        pins.extend(scope.graph_generation);
    }
    // A clean restart whose retained revision-7 head recovered serves through
    // the text projection and never seats a second copy of its sealed
    // generation, so the sealed slot alone under-reports what is live. Pin
    // the level that actually serves or retention collects it out from under
    // the route.
    if let Some(text) = schedulers.latest_text_serving_for_root(project_root).await {
        pins.insert(text.metadata().manifest().generation_id.clone());
    }
    pins
}
