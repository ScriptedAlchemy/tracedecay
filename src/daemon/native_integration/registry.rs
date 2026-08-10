//! One retained native-integration transaction authority per daemon-owned
//! project store.
//!
//! The registry composes the four coordinator inputs the Plan 36 deferral
//! named — the durable store actor, the exact-pair topology resolver, the
//! native Gix mechanics, and the pinned-policy authorization — completes
//! durable startup recovery, and only then exposes the owner to invocation
//! routing. A project without a mounted owner keeps answering the typed
//! unavailable result; nothing here guesses or falls back to local mutation.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_application::git::{
    NativeWorktreeService, WorktreeCleanupReconcileRequestV1, WorktreeCleanupReconciliationV1,
    WorktreeContractError,
};
use tracedecay_application::{
    AuthorizedScopeSet, CancellationSignal, NativeIntegrationContractError, NativeIntegrationPort,
    NativeIntegrationPortError, NativeIntegrationRecoveryRequestV1, NativeIntegrationService,
    NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationStackResolutionPort,
    NativeIntegrationStackResolutionRequestV1, NativeIntegrationStackSnapshotService,
    ResolvedScope,
};
use tracedecay_domain::{
    ManifestDigest, ProjectId, RepositoryId, ScopeSetId, ScopeSetRevision, UtcMicros,
};
use tracedecay_store::{NativeIntegrationStore, NativeIntegrationStoreResult};
use tracedecay_usecases::native_integration::{
    DaemonNativeIntegrationAuthorization, ExactPairNativeIntegrationTopology,
    GixNativeIntegrationAdapter, NativeIntegrationTransactionCoordinator,
};
use tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_usecases::stack_coordinator::{
    DaemonGitHubStackCoordinatorV1, StackCoordinatorErrorV1,
};

#[cfg(test)]
use crate::db::engine::TestConnection;
use crate::global_db::{ProjectGraphRuntimePortV1, RegisteredGlobalDb};
use tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage;

use super::stack_runtime::DaemonGitHubStackRuntimeV1;
use super::store::{DaemonNativeIntegrationStore, SharedDaemonNativeIntegrationStore};
use super::worktree::{DaemonAuthorizedScopeSetReader, DaemonNativeWorktreeAuthority};

const MAX_PENDING_WORKTREE_CLEANUPS: u32 = 4_096;

/// The one exact composition served to invocation routing.
pub(crate) type DaemonProjectNativeIntegrationCoordinator = NativeIntegrationTransactionCoordinator<
    SharedDaemonNativeIntegrationStore,
    SharedProjectNativeIntegrationTopology,
    GixNativeIntegrationAdapter,
    DaemonNativeIntegrationAuthorization,
>;

pub(crate) type DaemonProjectNativeIntegrationService =
    NativeIntegrationService<DaemonProjectNativeIntegrationCoordinator>;

pub(crate) type DaemonProjectNativeWorktreeService =
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

/// Shares one enrolled topology resolver between the transaction coordinator
/// and the stack-snapshot service without a second repository handle.
#[derive(Clone)]
pub(crate) struct SharedProjectNativeIntegrationTopology {
    inner: Arc<ExactPairNativeIntegrationTopology>,
}

impl NativeIntegrationStackResolutionPort for SharedProjectNativeIntegrationTopology {
    fn resolve(
        &self,
        request: &NativeIntegrationStackResolutionRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationPortError> {
        self.inner.resolve(request, cancellation)
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
        database: Arc<RegisteredGlobalDb>,
    ) -> NativeIntegrationStoreResult<SharedDaemonNativeIntegrationStore> {
        // The registered runtime authority already supplies the canonical
        // database identity; a fresh SQLite shard may not have materialized
        // its path yet, so no filesystem lookup happens here.
        let path = database.db_path().to_path_buf();
        self.ensure_with(path, || DaemonNativeIntegrationStore::open(database))
    }

    #[cfg(test)]
    fn ensure_engine_test(
        &self,
        path: PathBuf,
    ) -> NativeIntegrationStoreResult<SharedDaemonNativeIntegrationStore> {
        let path = path
            .canonicalize()
            .map_err(|_| tracedecay_store::NativeIntegrationStoreError::Unavailable)?;
        let store_path = path.clone();
        self.ensure_with(path, || {
            DaemonNativeIntegrationStore::open_engine_test(TestConnection::open(&store_path))
        })
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
            return Err(tracedecay_store::NativeIntegrationStoreError::Unavailable);
        }
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| tracedecay_store::NativeIntegrationStoreError::Unavailable)?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(tracedecay_store::NativeIntegrationStoreError::Unavailable);
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
                .map_err(|_| tracedecay_store::NativeIntegrationStoreError::Unavailable)?;
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
        .map_err(|_| tracedecay_store::NativeIntegrationStoreError::Unavailable)?
    }
}

/// The per-project invocation owner handed to the daemon handler.
#[derive(Clone)]
pub(crate) struct DaemonNativeIntegrationOwner {
    pub(crate) project_id: ProjectId,
    pub(crate) repository_id: RepositoryId,
    service: Arc<DaemonProjectNativeIntegrationService>,
    snapshots: Arc<NativeIntegrationStackSnapshotService<SharedProjectNativeIntegrationTopology>>,
    worktrees: Option<Arc<DaemonProjectNativeWorktreeService>>,
    store: SharedDaemonNativeIntegrationStore,
    scope_sets: Option<AuthorizedScopeSetSqliteStorage>,
    stack_runtimes: Arc<Mutex<BTreeMap<ManifestDigest, Arc<DaemonGitHubStackRuntimeV1>>>>,
}

impl DaemonNativeIntegrationOwner {
    pub(crate) fn service(&self) -> &DaemonProjectNativeIntegrationService {
        &self.service
    }

    pub(crate) fn service_arc(&self) -> Arc<DaemonProjectNativeIntegrationService> {
        Arc::clone(&self.service)
    }

    pub(crate) fn worktree_service_arc(&self) -> Option<Arc<DaemonProjectNativeWorktreeService>> {
        self.worktrees.as_ref().map(Arc::clone)
    }

    pub(crate) fn stack_snapshot(
        &self,
        request: NativeIntegrationStackResolutionRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationContractError> {
        self.snapshots.snapshot(request, cancellation)
    }

    pub(crate) fn store(&self) -> &SharedDaemonNativeIntegrationStore {
        &self.store
    }

    pub(crate) fn cleanup_recovery_roots(
        &self,
    ) -> Result<Vec<PathBuf>, NativeIntegrationPortError> {
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
    pub(crate) async fn recover_worktree_cleanups(
        &self,
    ) -> Result<usize, NativeIntegrationPortError> {
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
                            target: tracedecay_application::git::NativeWorktreeTargetV1::Worktree {
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
    }

    pub(crate) fn authorized_scope_set(
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
    pub(crate) fn mount_github_stack_runtime(
        &self,
        database: Arc<RegisteredGlobalDb>,
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
        )?;
        runtimes.insert(scope.scope_digest.clone(), Arc::clone(&runtime));
        Ok(runtime)
    }

    /// Returns the runtime for a scope already admitted by project-open. A
    /// missing runtime is intentionally indistinguishable from an unmounted
    /// owner to callers that need to conceal stack signal existence.
    pub(crate) fn github_stack_runtime(
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
pub(crate) struct DaemonNativeIntegrationServiceRegistry {
    stores: NativeIntegrationStoreRegistry,
    owners: tokio::sync::Mutex<HashMap<OwnerKey, OwnerEntry>>,
    creation_gate: tokio::sync::Mutex<()>,
    shutdown_fenced: AtomicBool,
}

impl DaemonNativeIntegrationServiceRegistry {
    /// Returns the retained owner for this exact identity, or composes exactly
    /// one: store actor, topology, mechanics, pinned-policy authorization,
    /// then durable startup recovery. A failed recovery mounts nothing.
    pub(crate) async fn ensure(
        &self,
        database: Arc<RegisteredGlobalDb>,
        repository_root: PathBuf,
        project_id: ProjectId,
        repository_id: RepositoryId,
        policy_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<DaemonNativeIntegrationOwner, NativeIntegrationPortError> {
        let database_path = database.db_path().to_path_buf();
        let scope_sets = database
            .authorized_scope_set_storage()
            .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        let graph_runtime = database
            .project_graph_runtime()
            .cloned()
            .ok_or(NativeIntegrationPortError::Unavailable)?;
        self.ensure_with(
            database_path,
            repository_root,
            project_id,
            repository_id,
            policy_digest,
            observed_at,
            Some(scope_sets),
            Some(graph_runtime),
            || self.stores.ensure(database),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn ensure_engine_test(
        &self,
        database_path: PathBuf,
        repository_root: PathBuf,
        project_id: ProjectId,
        repository_id: RepositoryId,
        policy_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<DaemonNativeIntegrationOwner, NativeIntegrationPortError> {
        let database_path = database_path
            .canonicalize()
            .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        let database = TestConnection::open(&database_path);
        let transaction = database
            .transaction_with_behavior(crate::db::engine::TransactionBehavior::Immediate)
            .await
            .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        crate::global_db::ensure_native_integration_schema(&transaction)
            .await
            .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        drop(database);
        let store_path = database_path.clone();
        self.ensure_with(
            database_path,
            repository_root,
            project_id,
            repository_id,
            policy_digest,
            observed_at,
            None,
            None,
            || self.stores.ensure_engine_test(store_path),
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
        graph_runtime: Option<Arc<dyn ProjectGraphRuntimePortV1>>,
        open_store: F,
    ) -> Result<DaemonNativeIntegrationOwner, NativeIntegrationPortError>
    where
        F: FnOnce() -> NativeIntegrationStoreResult<SharedDaemonNativeIntegrationStore>,
    {
        if self.shutdown_fenced.load(Ordering::SeqCst) {
            return Err(NativeIntegrationPortError::Unavailable);
        }
        let repository_root =
            crate::daemon::git_transactions::canonicalize_repository_root(&repository_root)
                .map_err(|_| NativeIntegrationPortError::Unavailable)?;
        if let Some(owner) = self
            .existing(&database_path, &repository_root, &project_id)
            .await?
        {
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
            return Ok(owner);
        }

        // Open/retain the store actor under the creation gate before native
        // recovery runs on a blocking thread. Later ensures reuse this actor.
        let store = open_store().map_err(|_| NativeIntegrationPortError::Unavailable)?;
        let recovery_store = store.clone();
        let native_root = repository_root.clone();
        let topology_runtime = graph_runtime.clone();
        let worktree_scope_sets = scope_sets.clone();
        let owner_project_id = project_id.clone();
        let owner_repository_id = repository_id.clone();
        let (owner_project_id, owner_repository_id, service, snapshots, worktrees) =
            tokio::task::spawn_blocking(move || {
                let topology = SharedProjectNativeIntegrationTopology {
                    inner: Arc::new(match topology_runtime {
                        Some(runtime) => {
                            ExactPairNativeIntegrationTopology::open_with_graph_runtime(
                                owner_project_id.clone(),
                                owner_repository_id.clone(),
                                &native_root,
                                runtime,
                            )?
                        }
                        None => ExactPairNativeIntegrationTopology::open(
                            owner_project_id.clone(),
                            owner_repository_id.clone(),
                            &native_root,
                        )?,
                    }),
                };
                let native = GixNativeIntegrationAdapter::open(
                    owner_project_id.clone(),
                    owner_repository_id.clone(),
                    &native_root,
                )?;
                let authorization = DaemonNativeIntegrationAuthorization::new(policy_digest)
                    .map_err(|_| NativeIntegrationPortError::Unavailable)?;
                let coordinator = NativeIntegrationTransactionCoordinator::new(
                    Arc::new(recovery_store.clone()),
                    Arc::new(topology.clone()),
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
                    Arc::new(NativeIntegrationStackSnapshotService::new(topology)),
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
    pub(crate) async fn for_repository_root(
        &self,
        repository_root: &Path,
    ) -> Result<Option<DaemonNativeIntegrationOwner>, NativeIntegrationPortError> {
        if self.shutdown_fenced.load(Ordering::SeqCst) {
            return Err(NativeIntegrationPortError::Unavailable);
        }
        let repository_root =
            crate::daemon::git_transactions::canonicalize_repository_root(repository_root)
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
    pub(crate) async fn retire_project_database(
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

    pub(crate) async fn shutdown(&self) -> Result<usize, NativeIntegrationPortError> {
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
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use tracedecay_application::{
        NativeIntegrationCancelDispositionV1, NativeIntegrationCancelRequestV1,
        NativeIntegrationStatusRequestV1,
    };
    use tracedecay_domain::{
        ManifestDigest, NativeIntegrationTransactionId, ProjectId, RepositoryId, UtcMicros,
    };

    use super::DaemonNativeIntegrationServiceRegistry;

    fn init_repository(root: &Path) {
        for arguments in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "fixture@example.com"],
            vec!["config", "user.name", "Fixture"],
            vec!["commit", "--allow-empty", "-m", "seed"],
        ] {
            let status = Command::new("git")
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

    #[tokio::test(flavor = "multi_thread")]
    async fn mounts_one_owner_and_answers_typed_states_for_unknown_transactions() {
        let directory = tempfile::tempdir().expect("temporary project directory");
        let repository_root = directory.path().join("repo");
        std::fs::create_dir_all(&repository_root).expect("repository root");
        init_repository(&repository_root);
        let database_path = directory.path().join("project-sessions.db");
        drop(std::fs::File::create(&database_path).expect("database file"));

        let registry = DaemonNativeIntegrationServiceRegistry::default();
        let owner = registry
            .ensure_engine_test(
                database_path.clone(),
                repository_root.clone(),
                ProjectId::new("project.native-owner.fixture").expect("project id"),
                RepositoryId::new("repository.native-owner.fixture").expect("repository id"),
                policy_digest(),
                UtcMicros(1),
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
            .ensure_engine_test(
                database_path,
                repository_root.clone(),
                ProjectId::new("project.native-owner.fixture").expect("project id"),
                RepositoryId::new("repository.native-owner.fixture").expect("repository id"),
                policy_digest(),
                UtcMicros(3),
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
