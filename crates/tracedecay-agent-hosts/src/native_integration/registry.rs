//! One retained native-integration transaction authority per daemon-owned
//! project store.
//!
//! The registry composes the four coordinator inputs — the durable store
//! actor, the exact-pair topology resolver, the native Gix mechanics, and the
//! pinned-policy authorization — completes durable startup recovery, and only
//! then exposes the owner to invocation routing. A project without a mounted
//! owner keeps answering the typed unavailable result; nothing here guesses
//! or falls back to local mutation.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_application::native_integration::{
    DaemonNativeIntegrationAuthorization, ExactPairNativeIntegrationTopology,
    GixNativeIntegrationAdapter, NativeIntegrationAnalysisPort,
    NativeIntegrationGraphRuntimeProviderV1, NativeIntegrationTransactionCoordinator,
};
use tracedecay_application::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_application::stack_coordinator::{
    DaemonGitHubStackCoordinatorV1, StackCoordinatorErrorV1,
};
use tracedecay_contracts::git::{
    NativeWorktreeService, WorktreeCleanupReconcileRequestV1, WorktreeCleanupReconciliationV1,
    WorktreeContractError,
};
use tracedecay_contracts::{
    AuthorizedScopeSet, CancellationSignal, NativeIntegrationContractError, NativeIntegrationPort,
    NativeIntegrationPortError, NativeIntegrationRecoveryRequestV1, NativeIntegrationService,
    NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationStackResolutionRequestV1,
    NativeIntegrationStackSnapshotService, ResolvedScope,
};
use tracedecay_domain::{
    ManifestDigest, ProjectId, RepositoryId, ScopeSetId, ScopeSetRevision, UtcMicros,
};
use tracedecay_store::{
    NativeIntegrationStore, NativeIntegrationStoreResult, StoreShardIdV1, StoreShardScopeV1,
};

use tracedecay_global_db::{RegisteredGlobalDbLeaseV1, VerifiedGraphRuntimePortV1};
use tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage;

use super::stack_runtime::DaemonGitHubStackRuntimeV1;
use super::store::{DaemonNativeIntegrationStore, SharedDaemonNativeIntegrationStore};
use super::worktree::{DaemonAuthorizedScopeSetReader, DaemonNativeWorktreeAuthority};

const MAX_PENDING_WORKTREE_CLEANUPS: u32 = 4_096;

/// The one exact composition served to invocation routing.
pub type DaemonProjectNativeIntegrationCoordinator = NativeIntegrationTransactionCoordinator<
    SharedDaemonNativeIntegrationStore,
    ExactPairNativeIntegrationTopology,
    GixNativeIntegrationAdapter,
    DaemonNativeIntegrationAuthorization,
>;

pub type DaemonProjectNativeIntegrationService =
    NativeIntegrationService<DaemonProjectNativeIntegrationCoordinator>;

pub type DaemonProjectNativeWorktreeService =
    NativeWorktreeService<DaemonAuthorizedScopeSetReader, DaemonNativeWorktreeAuthority>;

fn worktree_recovery_error(error: WorktreeContractError) -> NativeIntegrationPortError {
    match error {
        WorktreeContractError::Denied | WorktreeContractError::ScopeSetDenied => {
            NativeIntegrationPortError::Denied
        }
        WorktreeContractError::Stale => NativeIntegrationPortError::Stale,
        WorktreeContractError::DurabilityUncertain => {
            NativeIntegrationPortError::DurabilityUncertain
        }
        WorktreeContractError::Domain(_)
        | WorktreeContractError::Inconsistent { .. }
        | WorktreeContractError::ScopeSetUnavailable
        | WorktreeContractError::AuthorityUnavailable
        | WorktreeContractError::Native(_) => NativeIntegrationPortError::Unavailable,
    }
}

/// Retains the one `DaemonNativeIntegrationStore` actor for each daemon-owned
/// project database. Dropping the registry closes every actor when the daemon
/// store administration shuts down.
#[derive(Default)]
struct NativeIntegrationStoreRegistry {
    stores: Mutex<HashMap<PathBuf, SharedDaemonNativeIntegrationStore>>,
    closed: AtomicBool,
}

impl NativeIntegrationStoreRegistry {
    fn ensure(
        &self,
        database: RegisteredGlobalDbLeaseV1,
    ) -> NativeIntegrationStoreResult<SharedDaemonNativeIntegrationStore> {
        // The registered runtime authority already supplies the canonical
        // database identity; a fresh SQLite shard may not have materialized
        // its path yet, so no filesystem lookup happens here.
        let path = database.db_path().to_path_buf();
        self.ensure_with(path, || DaemonNativeIntegrationStore::open(database))
    }

    fn ensure_with<F>(
        &self,
        path: PathBuf,
        open: F,
    ) -> NativeIntegrationStoreResult<SharedDaemonNativeIntegrationStore>
    where
        F: FnOnce() -> NativeIntegrationStoreResult<DaemonNativeIntegrationStore>,
    {
        if self.closed.load(Ordering::SeqCst) {
            return Err(tracedecay_store::NativeIntegrationStoreError::unavailable(
                "native integration store registry is shut down",
            ));
        }
        let mut stores = self
            .stores
            .lock()
            .map_err(tracedecay_store::NativeIntegrationStoreError::unavailable)?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(tracedecay_store::NativeIntegrationStoreError::unavailable(
                "native integration store registry is shut down",
            ));
        }
        if let Some(existing) = stores.get(&path) {
            return Ok(existing.clone());
        }
        let store = SharedDaemonNativeIntegrationStore::from_arc(Arc::new(open()?));
        stores.insert(path, store.clone());
        Ok(store)
    }

    async fn shutdown_all(&self) -> NativeIntegrationStoreResult<usize> {
        self.closed.store(true, Ordering::SeqCst);
        let stores = {
            let mut retained = self
                .stores
                .lock()
                .map_err(tracedecay_store::NativeIntegrationStoreError::unavailable)?;
            retained.drain().map(|(_, store)| store).collect::<Vec<_>>()
        };
        tokio::task::spawn_blocking(move || {
            let mut joined = 0usize;
            for store in stores {
                joined = joined.saturating_add(usize::from(store.shutdown()?));
            }
            Ok(joined)
        })
        .await
        .map_err(tracedecay_store::NativeIntegrationStoreError::unavailable)?
    }
}

/// The per-project invocation owner handed to the daemon handler.
#[derive(Clone)]
pub struct DaemonNativeIntegrationOwner {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    service: Arc<DaemonProjectNativeIntegrationService>,
    snapshots: Arc<NativeIntegrationStackSnapshotService<Arc<ExactPairNativeIntegrationTopology>>>,
    worktrees: Option<Arc<DaemonProjectNativeWorktreeService>>,
    store: SharedDaemonNativeIntegrationStore,
    scope_sets: Option<AuthorizedScopeSetSqliteStorage>,
    stack_runtimes: Arc<Mutex<BTreeMap<ManifestDigest, Arc<DaemonGitHubStackRuntimeV1>>>>,
}

impl DaemonNativeIntegrationOwner {
    pub fn service(&self) -> &DaemonProjectNativeIntegrationService {
        &self.service
    }

    pub fn service_arc(&self) -> Arc<DaemonProjectNativeIntegrationService> {
        Arc::clone(&self.service)
    }

    pub fn worktree_service_arc(&self) -> Option<Arc<DaemonProjectNativeWorktreeService>> {
        self.worktrees.as_ref().map(Arc::clone)
    }

    pub fn stack_snapshot(
        &self,
        request: NativeIntegrationStackResolutionRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationContractError> {
        self.snapshots.snapshot(request, cancellation)
    }

    pub fn store(&self) -> &SharedDaemonNativeIntegrationStore {
        &self.store
    }

    pub fn cleanup_recovery_roots(&self) -> Result<Vec<PathBuf>, NativeIntegrationPortError> {
        let mut roots = self
            .store
            .pending_worktree_cleanups(&self.repository_id, MAX_PENDING_WORKTREE_CLEANUPS)
            .map_err(|_| NativeIntegrationPortError::Unavailable)?
            .into_iter()
            .map(|transaction| transaction.command.worktree_root)
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        Ok(roots)
    }

    /// Reconciles every cleanup fenced during project-open before holder
    /// runtimes are published. Any unresolved journal keeps its exact-root
    /// fence and fails project-open closed.
    #[hotpath::measure(label = "daemon.native_integration.worktree_recover", future = true)]
    pub async fn recover_worktree_cleanups(&self) -> Result<usize, NativeIntegrationPortError> {
        let service = self
            .worktree_service_arc()
            .ok_or(NativeIntegrationPortError::Unavailable)?;
        let store = self.store.clone();
        let repository_id = self.repository_id.clone();
        tokio::task::spawn_blocking(move || {
            let pending = store
                .pending_worktree_cleanups(&repository_id, MAX_PENDING_WORKTREE_CLEANUPS)
                .map_err(|_| NativeIntegrationPortError::Unavailable)?;
            let mut reconciled = 0usize;
            for (index, transaction) in pending.into_iter().enumerate() {
                let signal = CancellationSignal::active(format!(
                    "cancel.native-worktree-startup-recovery.{index}"
                ))
                .map_err(|_| NativeIntegrationPortError::Unavailable)?;
                let outcome = service
                    .reconcile(
                        &WorktreeCleanupReconcileRequestV1 {
                            scope_set_id: transaction.scope_set_id,
                            scope_set_revision: transaction.scope_set_revision,
                            scope_set_digest: transaction.scope_set_digest,
                            target: tracedecay_contracts::git::NativeWorktreeTargetV1::Worktree {
                                project_id: transaction.command.project_id,
                                repository_id: transaction.command.repository_id,
                                worktree_id: transaction.command.worktree_id,
                            },
                            confirmation_digest: transaction.confirmation_digest,
                        },
                        &signal,
                    )
                    .map_err(worktree_recovery_error)?;
                match outcome {
                    WorktreeCleanupReconciliationV1::Removed { .. }
                    | WorktreeCleanupReconciliationV1::StillPresent
                    | WorktreeCleanupReconciliationV1::Denied => {
                        reconciled = reconciled.saturating_add(1);
                    }
                    WorktreeCleanupReconciliationV1::Stale => {
                        return Err(NativeIntegrationPortError::Stale);
                    }
                    WorktreeCleanupReconciliationV1::DurabilityUncertain => {
                        return Err(NativeIntegrationPortError::DurabilityUncertain);
                    }
                    WorktreeCleanupReconciliationV1::Unavailable => {
                        return Err(NativeIntegrationPortError::Unavailable);
                    }
                }
            }
            Ok(reconciled)
        })
        .await
        .map_err(|_| NativeIntegrationPortError::Unavailable)?
        .inspect(|reconciled| {
            hotpath::gauge!("daemon.native_integration.worktree_recovered").inc(*reconciled as f64);
        })
    }

    pub fn authorized_scope_set(
        &self,
        scope_set_id: &ScopeSetId,
        revision: ScopeSetRevision,
        digest: &ManifestDigest,
    ) -> Result<AuthorizedScopeSet, NativeIntegrationPortError> {
        let storage = self
            .scope_sets
            .as_ref()
            .ok_or(NativeIntegrationPortError::Unavailable)?;
        let scope_set = storage
            .read(scope_set_id)
            .map_err(|_| NativeIntegrationPortError::Unavailable)?
            .ok_or(NativeIntegrationPortError::Unavailable)?;
        if scope_set.revision() != revision || scope_set.digest() != digest {
            return Err(NativeIntegrationPortError::Stale);
        }
        Ok(scope_set)
    }

    /// Mounts exactly one stack-delivery runtime for one exact project scope.
    /// Re-opening the project refreshes its source-access expiry but never
    /// creates a second queue actor or a second background drain task.
    pub fn mount_github_stack_runtime(
        &self,
        database: RegisteredGlobalDbLeaseV1,
        scope: ResolvedScope,
        access: ProjectSourceAccessSnapshot,
        coordinator: Arc<DaemonGitHubStackCoordinatorV1>,
    ) -> Result<Arc<DaemonGitHubStackRuntimeV1>, StackCoordinatorErrorV1> {
        if self.project_id != scope.project_id || self.repository_id != scope.repository_id {
            return Err(StackCoordinatorErrorV1::Stale);
        }
        let mut runtimes = self
            .stack_runtimes
            .lock()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?;
        if let Some(existing) = runtimes.get(&scope.scope_digest) {
            existing.refresh_access(access)?;
            return Ok(Arc::clone(existing));
        }
        let runtime = DaemonGitHubStackRuntimeV1::mount(
            self.project_id.clone(),
            scope.clone(),
            access,
            database,
            coordinator,
            self.service_arc(),
            self.store.clone(),
        )?;
        runtimes.insert(scope.scope_digest.clone(), Arc::clone(&runtime));
        Ok(runtime)
    }

    /// Returns the runtime for a scope already admitted by project-open. A
    /// missing runtime is intentionally indistinguishable from an unmounted
    /// owner to callers that need to conceal stack signal existence.
    pub fn github_stack_runtime(
        &self,
        scope: &ResolvedScope,
    ) -> Result<Option<Arc<DaemonGitHubStackRuntimeV1>>, NativeIntegrationPortError> {
        if self.project_id != scope.project_id || self.repository_id != scope.repository_id {
            return Ok(None);
        }
        self.stack_runtimes
            .lock()
            .map_err(|_| NativeIntegrationPortError::Unavailable)
            .map(|runtimes| runtimes.get(&scope.scope_digest).cloned())
    }
}

struct OwnerEntry {
    repository_root: PathBuf,
    owner: DaemonNativeIntegrationOwner,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct OwnerKey {
    database_path: PathBuf,
    project_id: ProjectId,
    repository_root: PathBuf,
}

/// Owns the store actor, topology resolver, native mechanics, authorization,
/// and coordinator for each exact project/repository identity.
#[derive(Default)]
pub struct DaemonNativeIntegrationServiceRegistry {
    stores: NativeIntegrationStoreRegistry,
    owners: tokio::sync::Mutex<HashMap<OwnerKey, OwnerEntry>>,
    creation_gate: tokio::sync::Mutex<()>,
    shutdown_fenced: AtomicBool,
}

impl DaemonNativeIntegrationServiceRegistry {
    /// Returns the retained owner for this exact identity, or composes exactly
    /// one: store actor, topology, mechanics, pinned-policy authorization,
    /// then durable startup recovery. A failed recovery mounts nothing.
    #[hotpath::measure(label = "daemon.native_integration.native_ensure", future = true)]
    pub async fn ensure(
        &self,
        database: RegisteredGlobalDbLeaseV1,
        repository_root: PathBuf,
        project_id: ProjectId,
        repository_id: RepositoryId,
        policy_digest: ManifestDigest,
        observed_at: UtcMicros,
        analysis: Arc<dyn NativeIntegrationAnalysisPort>,
    ) -> Result<DaemonNativeIntegrationOwner, NativeIntegrationPortError> {
        let owner = self
            .ensure_registered(
                database,
                repository_root,
                project_id,
                repository_id,
                policy_digest,
                observed_at,
                analysis,
            )
            .await;
        if owner.is_err() {
            hotpath::gauge!("daemon.native_integration.ensure.failed").inc(1.0);
        }
        owner
    }

    async fn ensure_registered(
        &self,
        database: RegisteredGlobalDbLeaseV1,
        repository_root: PathBuf,
        project_id: ProjectId,
        repository_id: RepositoryId,
        policy_digest: ManifestDigest,
        observed_at: UtcMicros,
        analysis: Arc<dyn NativeIntegrationAnalysisPort>,
    ) -> Result<DaemonNativeIntegrationOwner, NativeIntegrationPortError> {
        let database_path = database.db_path().to_path_buf();
        let scope_sets = database
            .authorized_scope_set_storage()
            .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        let session_shard = &database.binding().shard_id;
        let StoreShardScopeV1::ProjectSessions {
            project_id: session_project,
        } = &session_shard.scope
        else {
            return Err(NativeIntegrationPortError::Unavailable);
        };
        if session_project != &project_id {
            return Err(NativeIntegrationPortError::Unavailable);
        }
        let expected_graph_shard = StoreShardIdV1::project(
            session_shard.brain_id.clone(),
            session_shard.profile_id.clone(),
            project_id.clone(),
        );
        let graph_database = database.clone();
        let graph_runtime: NativeIntegrationGraphRuntimeProviderV1 = Arc::new(move || {
            graph_database
                .project_graph_runtime()
                .map(|runtime| Arc::new(runtime.clone()) as Arc<dyn VerifiedGraphRuntimePortV1>)
        });
        self.ensure_with(
            database_path,
            repository_root,
            project_id,
            repository_id,
            policy_digest,
            observed_at,
            Some(scope_sets),
            Some(expected_graph_shard),
            Some(graph_runtime),
            analysis,
            || self.stores.ensure(database),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn ensure_with<F>(
        &self,
        database_path: PathBuf,
        repository_root: PathBuf,
        project_id: ProjectId,
        repository_id: RepositoryId,
        policy_digest: ManifestDigest,
        observed_at: UtcMicros,
        scope_sets: Option<AuthorizedScopeSetSqliteStorage>,
        expected_graph_shard: Option<StoreShardIdV1>,
        graph_runtime: Option<NativeIntegrationGraphRuntimeProviderV1>,
        analysis: Arc<dyn NativeIntegrationAnalysisPort>,
        open_store: F,
    ) -> Result<DaemonNativeIntegrationOwner, NativeIntegrationPortError>
    where
        F: FnOnce() -> NativeIntegrationStoreResult<SharedDaemonNativeIntegrationStore>,
    {
        if self.shutdown_fenced.load(Ordering::SeqCst) {
            return Err(NativeIntegrationPortError::Unavailable);
        }
        let repository_root = repository_root
            .canonicalize()
            .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        if let Some(owner) = self
            .existing(&database_path, &repository_root, &project_id)
            .await?
        {
            hotpath::gauge!("daemon.native_integration.ensure.reused").inc(1.0);
            return Ok(owner);
        }

        let _creation = self.creation_gate.lock().await;
        if self.shutdown_fenced.load(Ordering::SeqCst) {
            return Err(NativeIntegrationPortError::Unavailable);
        }
        if let Some(owner) = self
            .existing(&database_path, &repository_root, &project_id)
            .await?
        {
            hotpath::gauge!("daemon.native_integration.ensure.reused").inc(1.0);
            return Ok(owner);
        }

        // Open/retain the store actor under the creation gate before native
        // recovery runs on a blocking thread. Later ensures reuse this actor.
        let store = open_store().map_err(|_| NativeIntegrationPortError::Unavailable)?;
        let recovery_store = store.clone();
        let native_root = repository_root.clone();
        let topology_runtime = graph_runtime.clone();
        let topology_shard = expected_graph_shard.clone();
        let worktree_scope_sets = scope_sets.clone();
        let owner_project_id = project_id.clone();
        let owner_repository_id = repository_id.clone();
        let (owner_project_id, owner_repository_id, service, snapshots, worktrees) =
            tokio::task::spawn_blocking(move || {
                let topology = Arc::new(match (topology_shard, topology_runtime) {
                    (Some(expected_shard), Some(runtime)) => {
                        ExactPairNativeIntegrationTopology::open_with_graph_runtime_provider(
                            owner_project_id.clone(),
                            owner_repository_id.clone(),
                            &native_root,
                            expected_shard,
                            runtime,
                        )?
                    }
                    (None, None) => ExactPairNativeIntegrationTopology::open(
                        owner_project_id.clone(),
                        owner_repository_id.clone(),
                        &native_root,
                    )?,
                    _ => return Err(NativeIntegrationPortError::Unavailable),
                });
                let native = GixNativeIntegrationAdapter::open(
                    owner_project_id.clone(),
                    owner_repository_id.clone(),
                    &native_root,
                    analysis,
                )?;
                let authorization = DaemonNativeIntegrationAuthorization::new(policy_digest)
                    .map_err(|_| NativeIntegrationPortError::Unavailable)?;
                let coordinator = NativeIntegrationTransactionCoordinator::new(
                    Arc::new(recovery_store.clone()),
                    Arc::clone(&topology),
                    Arc::new(native),
                    Arc::new(authorization),
                );
                let worktrees = worktree_scope_sets
                    .map(|storage| {
                        DaemonNativeWorktreeAuthority::open(
                            owner_project_id.clone(),
                            owner_repository_id.clone(),
                            &native_root,
                            recovery_store.clone(),
                        )
                        .map(|authority| {
                            Arc::new(NativeWorktreeService::new(
                                DaemonAuthorizedScopeSetReader::new(storage),
                                authority,
                            ))
                        })
                        .map_err(worktree_recovery_error)
                    })
                    .transpose()?;
                // Durable startup recovery: every unfinished record reaches a
                // terminal receipt or a quarantine fence before this owner
                // serves a single request. Failing closed here mounts nothing.
                let pending = recovery_store
                    .pending_transactions(None)
                    .map_err(|_| NativeIntegrationPortError::Unavailable)?;
                for record in pending {
                    coordinator.recover(&NativeIntegrationRecoveryRequestV1 {
                        transaction_id: record.status.transaction_id.clone(),
                        observed_at,
                    })?;
                }
                Ok::<_, NativeIntegrationPortError>((
                    owner_project_id,
                    owner_repository_id,
                    Arc::new(NativeIntegrationService::new(coordinator)),
                    Arc::new(NativeIntegrationStackSnapshotService::new(Arc::clone(
                        &topology,
                    ))),
                    worktrees,
                ))
            })
            .await
            .map_err(|_| NativeIntegrationPortError::Unavailable)??;

        let owner = DaemonNativeIntegrationOwner {
            project_id: owner_project_id.clone(),
            repository_id: owner_repository_id.clone(),
            service,
            snapshots,
            worktrees,
            store,
            scope_sets,
            stack_runtimes: Arc::new(Mutex::new(BTreeMap::new())),
        };
        let key = OwnerKey {
            database_path,
            project_id: owner_project_id.clone(),
            repository_root: repository_root.clone(),
        };
        self.owners.lock().await.insert(
            key,
            OwnerEntry {
                repository_root,
                owner: owner.clone(),
            },
        );
        hotpath::gauge!("daemon.native_integration.ensure.mounted").inc(1.0);
        Ok(owner)
    }

    async fn existing(
        &self,
        database_path: &Path,
        repository_root: &Path,
        project_id: &ProjectId,
    ) -> Result<Option<DaemonNativeIntegrationOwner>, NativeIntegrationPortError> {
        let owners = self.owners.lock().await;
        Ok(owners
            .get(&OwnerKey {
                database_path: database_path.to_path_buf(),
                project_id: project_id.clone(),
                repository_root: repository_root.to_path_buf(),
            })
            .map(|entry| entry.owner.clone()))
    }

    /// Resolve only an owner already mounted by project-open admission.
    /// Missing and ambiguous roots deliberately share the same outcome.
    pub async fn for_repository_root(
        &self,
        repository_root: &Path,
    ) -> Result<Option<DaemonNativeIntegrationOwner>, NativeIntegrationPortError> {
        if self.shutdown_fenced.load(Ordering::SeqCst) {
            return Err(NativeIntegrationPortError::Unavailable);
        }
        let repository_root = repository_root
            .canonicalize()
            .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        let owners = self.owners.lock().await;
        let mut matches = owners
            .values()
            .filter(|entry| entry.repository_root == repository_root);
        let Some(entry) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Ok(None);
        }
        Ok(Some(entry.owner.clone()))
    }

    /// Retires every owner attached to one exact project-session database.
    pub async fn retire_project_database(
        &self,
        project_id: &ProjectId,
        database_path: &Path,
    ) -> Result<(), NativeIntegrationPortError> {
        self.owners
            .lock()
            .await
            .retain(|key, _| key.project_id != *project_id || key.database_path != database_path);
        let mut stores = self
            .stores
            .stores
            .lock()
            .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        stores.remove(database_path);
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<usize, NativeIntegrationPortError> {
        self.shutdown_fenced.store(true, Ordering::SeqCst);
        let _creation = self.creation_gate.lock().await;
        self.owners.lock().await.clear();
        self.stores
            .shutdown_all()
            .await
            .map_err(|_| NativeIntegrationPortError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use super::DaemonNativeIntegrationServiceRegistry;
    use tracedecay_contracts::{
        AuthorizedScopeSetAuthority, CancellationContext, CancellationSignal, CapabilityGrantId,
        CapabilityGrantSnapshot, Deadline, DisclosureClass, NativeIntegrationCancelDispositionV1,
        NativeIntegrationCancelRequestV1, NativeIntegrationSelectionBindingV1,
        NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationStackResolutionRequestV1,
        NativeIntegrationStatusRequestV1, RequestContext, RequestId, ResolvedScope,
        native_integration_surface_operation,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, NativeIntegrationTransactionId, ProjectId, RefId, RepositoryId,
        ScopeSetId, ScopeSetRevision, UtcMicros, WorktreeId, WorktreeInventoryEpoch,
        WorktreeInventorySnapshotId,
    };
    use tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;
    use tracedecay_runtime_core::git::try_git_program;
    use tracedecay_sessions::admission::HostAdmissionScope;

    struct UnexpectedAnalysis;

    impl tracedecay_application::native_integration::NativeIntegrationAnalysisPort
        for UnexpectedAnalysis
    {
        fn analyze(
            &self,
            _selection: &tracedecay_domain::NativeIntegrationSelectionV1,
            _native: &tracedecay_runtime_core::git_repository::GitNativePreflight,
            _candidate: &tracedecay_runtime_core::git_repository::GitNativeCandidateTreeV1<'_>,
            _deadline: &Deadline,
            _cancellation_signal: &CancellationSignal,
            _cancellation: &tracedecay_runtime_core::cancellation::CancellationToken,
        ) -> Result<
            tracedecay_domain::NativeIntegrationAnalysisReportV1,
            tracedecay_contracts::NativeIntegrationPortError,
        > {
            panic!("snapshot-only registry tests must not run semantic analysis")
        }

        fn revalidate(
            &self,
            _report: &tracedecay_domain::NativeIntegrationAnalysisReportV1,
            _deadline: &Deadline,
            _cancellation: &CancellationSignal,
        ) -> Result<
            tracedecay_application::native_integration::NativeIntegrationAnalysisRevalidationV1,
            tracedecay_contracts::NativeIntegrationPortError,
        > {
            panic!("snapshot-only registry tests must not revalidate semantic analysis")
        }
    }

    fn init_repository(root: &Path) {
        for arguments in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "fixture@example.com"],
            vec!["config", "user.name", "Fixture"],
            vec!["commit", "--allow-empty", "-m", "seed"],
        ] {
            let status = Command::new(try_git_program().expect("resolve the git program"))
                .args(&arguments)
                .current_dir(root)
                .status()
                .expect("run git fixture command");
            assert!(status.success(), "git {arguments:?} failed");
        }
    }

    fn policy_digest() -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", "5".repeat(64))).expect("policy digest")
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn git(root: &Path, arguments: &[&str]) {
        let status = Command::new(try_git_program().expect("resolve the git program"))
            .args(arguments)
            .current_dir(root)
            .status()
            .expect("run git fixture command");
        assert!(status.success(), "git {arguments:?} failed");
    }

    fn prepare_independent_pair(root: &Path) {
        init_repository(root);
        git(root, &["checkout", "-b", "destination"]);
        git(root, &["checkout", "main"]);
        git(root, &["checkout", "-b", "source"]);
        std::fs::write(root.join("source.txt"), "source\n").expect("write source");
        git(root, &["add", "source.txt"]);
        git(root, &["commit", "-m", "source"]);
        git(root, &["checkout", "main"]);
    }

    fn exact_pair_scopes(
        project: &ProjectId,
        repository: &RepositoryId,
    ) -> (ResolvedScope, ResolvedScope) {
        let source = ResolvedScope::new(
            project.clone(),
            repository.clone(),
            WorktreeId::new("worktree.native.snapshot.source").expect("source worktree id"),
            Some(RefId::new("refs/heads/source").expect("source ref")),
        )
        .expect("source scope");
        let destination = ResolvedScope::new(
            project.clone(),
            repository.clone(),
            WorktreeId::new("worktree.native.snapshot.destination")
                .expect("destination worktree id"),
            Some(RefId::new("refs/heads/destination").expect("destination ref")),
        )
        .expect("destination scope");
        (source, destination)
    }

    fn stack_snapshot_request(
        source: ResolvedScope,
        destination: ResolvedScope,
    ) -> NativeIntegrationStackResolutionRequestV1 {
        let (capability, use_case) = {
            let operation = native_integration_surface_operation(
                tracedecay_contracts::NATIVE_INTEGRATION_STACK_SNAPSHOT_OPERATION,
            )
            .expect("canonical operation")
            .expect("declared operation");
            (
                operation.capability_id().clone(),
                operation.use_case_id().clone(),
            )
        };
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.native.snapshot").expect("grant id"),
            1,
            digest('a'),
            ActorId::new("actor.native.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(10_000),
            destination.clone(),
            BTreeSet::from([capability.clone()]),
            BTreeSet::from([use_case.clone()]),
            DisclosureClass::Sensitive,
        )
        .expect("grant");
        let context = RequestContext::new(
            ActorId::new("actor.native.requester").expect("requester"),
            destination.clone(),
            grant,
            RequestId::new("request.native.snapshot").expect("request id"),
            Deadline::new(UtcMicros(10_000)).expect("deadline"),
            CancellationContext::active("cancel.native.snapshot").expect("cancellation"),
        )
        .expect("request context");
        let source_grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.native.snapshot.source").expect("grant id"),
            1,
            digest('a'),
            ActorId::new("actor.native.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(10_000),
            source.clone(),
            BTreeSet::from([capability.clone()]),
            BTreeSet::from([use_case.clone()]),
            DisclosureClass::Sensitive,
        )
        .expect("source grant");
        let authorized_scope_set = AuthorizedScopeSetAuthority::authorize(
            ScopeSetId::new("scope-set.native.snapshot").expect("scope set id"),
            ScopeSetRevision::new(1).expect("scope set revision"),
            vec![
                context,
                RequestContext::new(
                    ActorId::new("actor.native.requester").expect("requester"),
                    source.clone(),
                    source_grant,
                    RequestId::new("request.native.snapshot.source").expect("request id"),
                    Deadline::new(UtcMicros(10_000)).expect("deadline"),
                    CancellationContext::active("cancel.native.snapshot.source")
                        .expect("cancellation"),
                )
                .expect("source context"),
            ],
            &capability,
            &use_case,
            UtcMicros(100),
        )
        .expect("authorized scope set");
        let source_ref = source.reference.clone().expect("source ref");
        let destination_ref = destination.reference.clone().expect("destination ref");
        NativeIntegrationStackResolutionRequestV1 {
            source,
            destination,
            authorized_scope_set,
            inventory_snapshot_id: WorktreeInventorySnapshotId::new("inventory.native.snapshot")
                .expect("inventory snapshot"),
            inventory_epoch: WorktreeInventoryEpoch::new(1).expect("inventory epoch"),
            selection: NativeIntegrationSelectionBindingV1::IndependentBranch {
                proposal_digest: digest('c'),
                source_ref,
                destination_ref,
            },
            grant_digest: digest('a'),
            policy_digest: digest('d'),
            observed_at: UtcMicros(100),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_stack_snapshot_freezes_topology_and_honours_guardrails() {
        let directory = tempfile::tempdir().expect("temporary project directory");
        let repository_root = directory.path().join("repo");
        std::fs::create_dir_all(&repository_root).expect("repository root");
        prepare_independent_pair(&repository_root);
        let registry = DaemonNativeIntegrationServiceRegistry::default();
        let project_id = ProjectId::new("project.native.snapshot").expect("project id");
        let repository_id = RepositoryId::new("repository.native.snapshot").expect("repository id");
        let runtime = HostAdmissionTestRuntimeV1::project(
            directory.path().join("profile"),
            &repository_root,
            project_id.clone(),
        )
        .await
        .expect("canonical project test runtime");
        let database = runtime
            .registered_database_lease(HostAdmissionScope::Project)
            .expect("registered project database");
        let owner = registry
            .ensure(
                database,
                repository_root,
                project_id.clone(),
                repository_id.clone(),
                policy_digest(),
                UtcMicros(100),
                Arc::new(UnexpectedAnalysis),
            )
            .await
            .expect("mount native integration owner");

        let (source, destination) = exact_pair_scopes(&project_id, &repository_id);
        let request = stack_snapshot_request(source, destination);
        let topology_request = request.clone();
        let signal = CancellationSignal::active("cancel.native.snapshot.resolve").expect("signal");
        let snapshot_owner = owner.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            snapshot_owner.stack_snapshot(topology_request, &signal)
        })
        .await
        .expect("stack snapshot join")
        .expect("stack snapshot result");
        let NativeIntegrationStackResolutionOutcomeV1::Complete(frozen) = outcome else {
            panic!("ensure must expose stack snapshot through the retained topology: {outcome:?}");
        };
        let selection = frozen.as_ref();
        assert_eq!(selection.project_id().expect("project"), &project_id);
        assert_eq!(
            selection.repository_id().expect("repository"),
            &repository_id
        );
        match selection {
            tracedecay_domain::NativeIntegrationSelectionV1::IndependentBranch(branch) => {
                assert!(branch.source_worktree_id.is_none());
                assert!(branch.destination_worktree_id.is_none());
            }
            other => {
                panic!("independent pair fixture must freeze an independent branch: {other:?}")
            }
        }

        let second_owner = owner.clone();
        let second_request = request.clone();
        let second_signal =
            CancellationSignal::active("cancel.native.snapshot.second").expect("signal");
        let second_outcome = tokio::task::spawn_blocking(move || {
            second_owner.stack_snapshot(second_request, &second_signal)
        })
        .await
        .expect("second stack snapshot join")
        .expect("second stack snapshot result");
        let NativeIntegrationStackResolutionOutcomeV1::Complete(second_frozen) = second_outcome
        else {
            panic!("retained topology must resolve consistently: {second_outcome:?}");
        };
        assert_eq!(second_frozen.as_ref(), frozen.as_ref());

        let foreign_project = ProjectId::new("project.native.snapshot.foreign").expect("foreign");
        let (foreign_source, foreign_destination) =
            exact_pair_scopes(&foreign_project, &repository_id);
        let foreign_request = stack_snapshot_request(foreign_source, foreign_destination);
        let denied_owner = owner.clone();
        let denied_signal =
            CancellationSignal::active("cancel.native.snapshot.denied").expect("signal");
        let denied = tokio::task::spawn_blocking(move || {
            denied_owner.stack_snapshot(foreign_request, &denied_signal)
        })
        .await
        .expect("denied join")
        .expect("denied result");
        assert_eq!(
            denied,
            NativeIntegrationStackResolutionOutcomeV1::Denied,
            "foreign project identity must not resolve against the enrolled topology"
        );

        let cancelled_signal =
            CancellationSignal::active("cancel.native.snapshot.already").expect("signal");
        cancelled_signal.cancel(UtcMicros(99));
        let cancelled = owner
            .stack_snapshot(request, &cancelled_signal)
            .expect("cancelled stack snapshot");
        assert_eq!(
            cancelled,
            NativeIntegrationStackResolutionOutcomeV1::Unavailable,
            "cancellation must fail closed before topology resolution"
        );

        registry.shutdown().await.expect("shutdown");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mounts_one_owner_and_answers_typed_states_for_unknown_transactions() {
        let directory = tempfile::tempdir().expect("temporary project directory");
        let repository_root = directory.path().join("repo");
        std::fs::create_dir_all(&repository_root).expect("repository root");
        init_repository(&repository_root);
        let registry = DaemonNativeIntegrationServiceRegistry::default();
        let project_id = ProjectId::new("project.native-owner.fixture").expect("project id");
        let runtime = HostAdmissionTestRuntimeV1::project(
            directory.path().join("profile"),
            &repository_root,
            project_id.clone(),
        )
        .await
        .expect("canonical project test runtime");
        let database = runtime
            .registered_database_lease(HostAdmissionScope::Project)
            .expect("registered project database");
        let owner = registry
            .ensure(
                database.clone(),
                repository_root.clone(),
                project_id.clone(),
                RepositoryId::new("repository.native-owner.fixture").expect("repository id"),
                policy_digest(),
                UtcMicros(1),
                Arc::new(UnexpectedAnalysis),
            )
            .await
            .expect("owner mounts with an empty store");

        // The retained owner resolves by exact repository root; an unknown
        // root resolves to nothing rather than a neighbouring owner.
        assert!(
            registry
                .for_repository_root(&repository_root)
                .await
                .expect("owner lookup")
                .is_some()
        );
        assert!(
            registry
                .for_repository_root(directory.path())
                .await
                .expect("unmounted lookup")
                .is_none()
        );

        // Unknown transactions answer typed absence, never an empty success
        // or a fabricated status.
        let unknown =
            NativeIntegrationTransactionId::new("transaction.native.unknown").expect("id");
        let status_owner = owner.clone();
        let status = tokio::task::spawn_blocking(move || {
            status_owner
                .service()
                .status(NativeIntegrationStatusRequestV1 {
                    transaction_id: NativeIntegrationTransactionId::new(
                        "transaction.native.unknown",
                    )
                    .expect("id"),
                })
        })
        .await
        .expect("status join")
        .expect("status read");
        assert_eq!(status, None);
        let cancel_owner = owner.clone();
        let disposition = tokio::task::spawn_blocking(move || {
            cancel_owner
                .service()
                .cancel(NativeIntegrationCancelRequestV1 {
                    transaction_id: unknown,
                    requested_at: UtcMicros(2),
                })
        })
        .await
        .expect("cancel join")
        .expect("cancel disposition");
        assert_eq!(
            disposition,
            NativeIntegrationCancelDispositionV1::UnknownTransaction
        );

        // A second ensure for the same identity reuses the retained owner
        // instead of composing a second authority for the same database.
        let second = registry
            .ensure(
                database,
                repository_root.clone(),
                project_id,
                RepositoryId::new("repository.native-owner.fixture").expect("repository id"),
                policy_digest(),
                UtcMicros(3),
                Arc::new(UnexpectedAnalysis),
            )
            .await
            .expect("second ensure");
        assert!(Arc::ptr_eq(&owner.service_arc(), &second.service_arc()));

        // Shutdown fences composition and resolution fail closed.
        registry.shutdown().await.expect("shutdown");
        assert!(
            registry
                .for_repository_root(&repository_root)
                .await
                .is_err()
        );
    }
}
