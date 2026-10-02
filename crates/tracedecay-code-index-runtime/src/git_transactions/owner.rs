//! One retained Git index transaction service per daemon-owned project store.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_contracts::{
    GitIndexApplyRequestV1, GitIndexOperationBindingV1, GitIndexTransactionPortError,
    ResolvedScope, now_micros,
};
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, CapabilityResolutionContextV1, ConfigurationValueV1,
    SOURCE_BINDINGS_SETTING_KEY, SettingKey, resolve_restrictive_capabilities,
};
use tracedecay_domain::{
    ActorId, CapabilityId as DomainCapabilityId, GitHeadStateV1, GitIndexPreviewDispositionV1,
    GitIndexPreviewV1, GitIndexTransactionOperationV1, GitIndexUnsupportedStateV1, ManifestDigest,
    ProjectId, RepositoryIndexStateV1, RepositoryWorkingTreeStateV1, UtcMicros, canonical_sha256,
};
use tracedecay_policy::{GitConflictRiskV1, GitEffectAuthorizationV1, GitEffectClassifierV1};
use tracedecay_tool_catalog::{CapabilityId, CatalogSnapshotV1};

use super::{
    CurrentGitIndexPolicyStateV1, DaemonGitIndexTransactionService,
    DaemonProjectGitIndexPreviewAssembler, FixedDaemonGitIndexExecutor, GitIndexPolicyRecheckPort,
    GitIndexTransactionStoreRegistry, RepositoryMutationQueue,
    SharedDaemonGitIndexTransactionStore, canonicalize_repository_root,
};
use tracedecay_application::ProjectSourceAccessSnapshot;
use tracedecay_contracts::catalog_composition::CatalogCompositionError;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore;
use tracedecay_global_db::configuration::contracts::ConfigurationControlStore;

const GIT_POLICY_REVISION: u64 = 2;

type ProfiledStdRwLock<T> = std::sync::RwLock<T>;
type ProfiledTokioMutex<T> = tokio::sync::Mutex<T>;
type ApplicationCatalogComposer =
    Arc<dyn Fn() -> Result<CatalogSnapshotV1, CatalogCompositionError> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct DaemonGitAuthorityStateV1 {
    pub scope: ResolvedScope,
    pub requester: ActorId,
    pub effective_capabilities: BTreeSet<CapabilityId>,
    pub grant_expires_at: UtcMicros,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub catalog_digest: ManifestDigest,
    pub privacy_digest: ManifestDigest,
    pub evaluated_at: UtcMicros,
}

pub trait DaemonGitAuthoritySource: Send + Sync {
    fn current_capability(
        &self,
        capability_id: &CapabilityId,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError>;

    fn current(
        &self,
        operation: GitIndexTransactionOperationV1,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError> {
        let binding = GitIndexOperationBindingV1::for_operation(operation)
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        self.current_capability(&binding.capability_id)
    }
}

struct ProductionDaemonGitAuthoritySource {
    access: ProjectSourceAccessSnapshot,
    configuration: OwnedGlobalDbConfigurationControlStore,
    catalog: ApplicationCatalogComposer,
    runtime: tokio::runtime::Handle,
}

impl DaemonGitAuthoritySource for ProductionDaemonGitAuthoritySource {
    #[tracing::instrument(name = "daemon.git.tx.authority", level = "trace", skip_all)]
    fn current_capability(
        &self,
        capability_id: &CapabilityId,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError> {
        let evaluated_at = now_micros();
        if evaluated_at >= self.access.grant_expires_at {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        // The Git transaction port is intentionally synchronous while the
        // configuration authority is asynchronous. Production calls arrive
        // on Tokio workers, so yield this worker before waiting on the same
        // runtime; calling `Handle::block_on` directly here panics because it
        // attempts to enter a runtime that is already driving this task.
        let current =
            tokio::task::block_in_place(|| self.runtime.block_on(self.configuration.current()))
                .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        if current.snapshot.validate().is_err() {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }

        let bindings_key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let Some(ConfigurationValueV1::SourceBindings(bindings)) =
            current.snapshot.effective_values.get(&bindings_key)
        else {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        };
        let authority = AuthorityRef::Project(self.access.scope.project_id.clone());
        let configured_bindings = bindings
            .iter()
            .filter(|binding| {
                binding.source_kind == self.access.binding.source_kind
                    && binding.authority == authority
            })
            .collect::<Vec<_>>();
        if configured_bindings.len() > 1
            || configured_bindings.first().copied().is_some_and(|binding| {
                binding.source_locator_digest != self.access.binding.source_locator_digest
            })
        {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        let source_binding = configured_bindings.first().map_or_else(
            || self.access.binding.clone(),
            |binding| (**binding).clone(),
        );
        let access_rules_key = SettingKey::new(ACCESS_RULES_SETTING_KEY)
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let Some(ConfigurationValueV1::AccessRules(access_rules)) =
            current.snapshot.effective_values.get(&access_rules_key)
        else {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        };
        let granted_capabilities = self
            .access
            .effective_capabilities
            .iter()
            .map(|capability| DomainCapabilityId::new(capability.as_str().to_owned()))
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(|_| GitIndexTransactionPortError::PolicyDenied)?;
        let resolution = resolve_restrictive_capabilities(
            granted_capabilities,
            access_rules,
            &CapabilityResolutionContextV1 {
                actor: self.access.requester.clone(),
                operation: None,
                source_kind: self.access.binding.source_kind,
                authority,
                evaluated_at,
            },
        )
        .map_err(|_| GitIndexTransactionPortError::PolicyDenied)?;
        let effective_capabilities = resolution
            .effective
            .into_iter()
            .map(|capability| CapabilityId::new(capability.as_str().to_owned()))
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(|_| GitIndexTransactionPortError::PolicyDenied)?;
        if !effective_capabilities.contains(capability_id) {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        let catalog =
            (self.catalog)().map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let manifest = catalog
            .capability(capability_id)
            .ok_or(GitIndexTransactionPortError::PolicyDenied)?;
        let catalog_digest = ManifestDigest::new(catalog.digest().to_string())
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let privacy_digest = canonical_sha256(&(
            manifest.privacy(),
            manifest.denied_disclosure(),
            manifest.scope(),
            &source_binding,
            &current.snapshot.resolution_provenance_digest,
        ))
        .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let policy_digest = canonical_sha256(&(
            GIT_POLICY_REVISION,
            &self.access.scope,
            &self.access.requester,
            &source_binding,
            &current.revision_id,
            &current.snapshot.effective_behavior_digest,
            &current.snapshot.resolution_provenance_digest,
            manifest,
            &catalog_digest,
            &privacy_digest,
        ))
        .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;

        Ok(DaemonGitAuthorityStateV1 {
            scope: self.access.scope.clone(),
            requester: self.access.requester.clone(),
            effective_capabilities,
            grant_expires_at: self.access.grant_expires_at,
            policy_revision: GIT_POLICY_REVISION,
            policy_digest,
            configuration_digest: current.snapshot.effective_behavior_digest,
            catalog_digest,
            privacy_digest,
            evaluated_at,
        })
    }
}

enum DaemonGitAuthority {
    Installed(Arc<dyn DaemonGitAuthoritySource>),
    /// Daemon shutdown revoked the authority of an owner a request may
    /// already hold.
    Revoked,
}

/// A slot is only ever constructed around an installed source, so a mounted
/// owner always carries authority until shutdown revokes it.
struct DaemonGitAuthoritySlot {
    source: ProfiledStdRwLock<DaemonGitAuthority>,
}

impl DaemonGitAuthoritySlot {
    fn new(source: Arc<dyn DaemonGitAuthoritySource>) -> Self {
        Self {
            source: std::sync::RwLock::new(DaemonGitAuthority::Installed(source)),
        }
    }

    fn set(&self, authority: DaemonGitAuthority) -> Result<(), GitIndexTransactionPortError> {
        *self
            .source
            .write()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)? = authority;
        Ok(())
    }
}

impl DaemonGitAuthoritySource for DaemonGitAuthoritySlot {
    fn current_capability(
        &self,
        capability_id: &CapabilityId,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError> {
        let source = match &*self
            .source
            .read()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?
        {
            DaemonGitAuthority::Installed(source) => Arc::clone(source),
            DaemonGitAuthority::Revoked => {
                return Err(GitIndexTransactionPortError::DaemonUnavailable);
            }
        };
        source.current_capability(capability_id)
    }
}

/// Rechecks current daemon-authenticated capability, configuration, catalog,
/// privacy, and exact preview scope immediately before native mutation.
pub struct DaemonGitIndexPolicyRecheck {
    authority: Arc<dyn DaemonGitAuthoritySource>,
}

impl DaemonGitIndexPolicyRecheck {
    pub fn new(authority: Arc<dyn DaemonGitAuthoritySource>) -> Self {
        Self { authority }
    }
}

impl GitIndexPolicyRecheckPort for DaemonGitIndexPolicyRecheck {
    #[tracing::instrument(name = "daemon.git.tx.recheck", level = "trace", skip_all)]
    fn recheck(
        &self,
        request: &GitIndexApplyRequestV1,
        preview: &GitIndexPreviewV1,
    ) -> Result<CurrentGitIndexPolicyStateV1, GitIndexTransactionPortError> {
        let current = self.authority.current(request.binding.operation)?;
        let capability_granted = request
            .context
            .allows(&request.binding.capability_id, &request.binding.use_case_id)
            && current
                .effective_capabilities
                .contains(&request.binding.capability_id);
        let owner_scope_matches =
            scope_matches_snapshot(&current.scope, &preview.repository_snapshot);
        if request.context.scope() != &current.scope
            || request.context.actor() != &current.requester
            || current.evaluated_at >= current.grant_expires_at
            || current.policy_revision != request.authority.policy.revision
            || current.policy_digest != request.authority.policy.digest
            || current.policy_digest != request.proof.policy_digest
            || current.configuration_digest != request.proof.configuration_digest
            || current.catalog_digest != request.proof.catalog_digest
            || current.privacy_digest != request.proof.privacy_digest
            || request.proof.external_proof.is_some()
            || !capability_granted
            || !owner_scope_matches
        {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        Ok(CurrentGitIndexPolicyStateV1 {
            authorization: GitEffectAuthorizationV1 {
                capability_granted,
                owner_scope_matches,
            },
            conflict_risk: preview_conflict_risk(preview),
            policy_revision: current.policy_revision,
            policy_digest: current.policy_digest,
            configuration_digest: current.configuration_digest,
            evaluated_at: current.evaluated_at,
        })
    }
}

pub fn preview_conflict_risk(preview: &GitIndexPreviewV1) -> GitConflictRiskV1 {
    let snapshot = &preview.repository_snapshot;
    if snapshot.index.state == RepositoryIndexStateV1::Unmerged
        || snapshot.working_tree.state == RepositoryWorkingTreeStateV1::Conflicted
        || matches!(
            preview.disposition,
            GitIndexPreviewDispositionV1::Unsupported(
                GitIndexUnsupportedStateV1::UnmergedIndex
                    | GitIndexUnsupportedStateV1::ConflictedWorkingTree
            )
        )
    {
        return GitConflictRiskV1::Confirmed;
    }
    if snapshot.coverage.leaves_state_unread()
        || !matches!(
            snapshot.index.state,
            RepositoryIndexStateV1::Clean | RepositoryIndexStateV1::Staged
        )
        || snapshot.operation_state != tracedecay_domain::GitOperationStateV1::None
        || matches!(
            preview.disposition,
            GitIndexPreviewDispositionV1::Unsupported(
                GitIndexUnsupportedStateV1::UnreadableIndex
                    | GitIndexUnsupportedStateV1::UnreadableWorkingTree
                    | GitIndexUnsupportedStateV1::InProgressOperation
                    | GitIndexUnsupportedStateV1::SparseIndex
                    | GitIndexUnsupportedStateV1::SplitIndex
            )
        )
    {
        GitConflictRiskV1::Possible
    } else {
        GitConflictRiskV1::NoneKnown
    }
}

fn scope_matches_snapshot(
    scope: &ResolvedScope,
    snapshot: &tracedecay_domain::RepositoryStateSnapshotV1,
) -> bool {
    scope.project_id == snapshot.project_id
        && scope.repository_id == snapshot.repository_id
        && snapshot.worktree_id.as_ref() == Some(&scope.worktree_id)
        && match (&scope.reference, &snapshot.head) {
            (
                Some(reference),
                GitHeadStateV1::Attached { branch, .. } | GitHeadStateV1::Unborn { branch },
            ) => reference.as_str() == branch,
            (None, GitHeadStateV1::Detached { .. }) => true,
            (None, _) | (Some(_), GitHeadStateV1::Detached { .. }) => false,
        }
}

pub type DaemonProjectGitIndexTransactionService = DaemonGitIndexTransactionService<
    SharedDaemonGitIndexTransactionStore,
    FixedDaemonGitIndexExecutor<DaemonProjectGitIndexPreviewAssembler>,
    GitEffectClassifierV1,
    DaemonGitIndexPolicyRecheck,
>;

#[derive(Clone)]
pub struct DaemonGitInvocationOwner {
    pub project_id: ProjectId,
    pub repository_root: PathBuf,
    pub service: Arc<DaemonProjectGitIndexTransactionService>,
    authority: Arc<DaemonGitAuthoritySlot>,
}

impl DaemonGitInvocationOwner {
    pub fn current_authority(
        &self,
        operation: GitIndexTransactionOperationV1,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError> {
        self.authority.current(operation)
    }

    pub fn current_read_authority(
        &self,
        request: &tracedecay_contracts::git::GitReadRequestV1,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError> {
        let capability = CapabilityId::new(request.capability_id().to_owned())
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        self.authority.current_capability(&capability)
    }
}

struct ServiceEntry {
    project_id: ProjectId,
    repository_root: PathBuf,
    service: Arc<DaemonProjectGitIndexTransactionService>,
    authority: Arc<DaemonGitAuthoritySlot>,
}

impl ServiceEntry {
    fn owner(&self) -> DaemonGitInvocationOwner {
        DaemonGitInvocationOwner {
            project_id: self.project_id.clone(),
            repository_root: self.repository_root.clone(),
            service: Arc::clone(&self.service),
            authority: Arc::clone(&self.authority),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ServiceKey {
    database_path: PathBuf,
    project_id: ProjectId,
    repository_root: PathBuf,
}

impl ServiceKey {
    fn new(database_path: &Path, project_id: &ProjectId, repository_root: &Path) -> Self {
        Self {
            database_path: database_path.to_path_buf(),
            project_id: project_id.clone(),
            repository_root: repository_root.to_path_buf(),
        }
    }
}

/// Owns the store actor, native executor, classifier, policy recheck, and
/// repository queue for each exact project/worktree identity. Linked
/// worktrees share one session store actor without sharing native executors or
/// mutation authority.
pub struct DaemonGitIndexTransactionServiceRegistry {
    catalog: ApplicationCatalogComposer,
    stores: GitIndexTransactionStoreRegistry,
    mutation_queue: Arc<RepositoryMutationQueue>,
    services: ProfiledTokioMutex<HashMap<ServiceKey, ServiceEntry>>,
    creation_gate: ProfiledTokioMutex<()>,
    shutdown_fenced: AtomicBool,
    shutdown_receipt: ProfiledTokioMutex<Option<DaemonGitIndexShutdownReceiptV1>>,
}

impl DaemonGitIndexTransactionServiceRegistry {
    /// Root supplies the catalog composer here: every owner this registry
    /// mounts resolves capability manifests through it, so there is no window
    /// in which an owner exists without one.
    pub fn new(
        catalog: impl Fn() -> Result<CatalogSnapshotV1, CatalogCompositionError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            catalog: Arc::new(catalog),
            stores: GitIndexTransactionStoreRegistry::default(),
            mutation_queue: Arc::new(RepositoryMutationQueue::default()),
            services: tokio::sync::Mutex::new(HashMap::new()),
            creation_gate: tokio::sync::Mutex::new(()),
            shutdown_fenced: AtomicBool::new(false),
            shutdown_receipt: tokio::sync::Mutex::new(None),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DaemonGitIndexShutdownReceiptV1 {
    pub services_closed: usize,
    pub store_actors_joined: usize,
}

impl DaemonGitIndexTransactionServiceRegistry {
    /// Mounts the owner for one exact project/worktree with its authority in
    /// a single publication: the service starts around an already installed
    /// authority and becomes resolvable only once both exist. Remounting the
    /// same identity reuses the started service and replaces its authority.
    #[tracing::instrument(name = "daemon.git.tx.mount", level = "trace", skip_all)]
    pub async fn mount(
        &self,
        database: RegisteredGlobalDbLeaseV1,
        repository_root: PathBuf,
        access: ProjectSourceAccessSnapshot,
        observed_at: UtcMicros,
        runtime: tokio::runtime::Handle,
    ) -> Result<DaemonGitInvocationOwner, GitIndexTransactionPortError> {
        let project_id = access.scope.project_id.clone();
        if access.binding.authority != AuthorityRef::Project(project_id.clone()) {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        // `RegisteredGlobalDb` already carries the canonical path admitted by
        // its retained runtime authority. Do not rediscover identity through
        // the filesystem: a newly opened SQLite shard may not have
        // materialized its directory entry yet.
        let database_path = database.db_path().to_path_buf();
        let source = Arc::new(ProductionDaemonGitAuthoritySource {
            access,
            configuration:
                OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
                    database.clone(),
                ),
            catalog: self.catalog.clone(),
            runtime,
        });
        self.mount_with(
            database_path,
            repository_root,
            project_id,
            source,
            observed_at,
            || self.stores.ensure(database),
        )
        .await
    }

    pub(super) async fn mount_with<F>(
        &self,
        database_path: PathBuf,
        repository_root: PathBuf,
        project_id: ProjectId,
        source: Arc<dyn DaemonGitAuthoritySource>,
        observed_at: UtcMicros,
        open_store: F,
    ) -> Result<DaemonGitInvocationOwner, GitIndexTransactionPortError>
    where
        F: FnOnce() -> tracedecay_store::GitIndexTransactionStoreResult<
            super::SharedDaemonGitIndexTransactionStore,
        >,
    {
        if self.shutdown_fenced.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionPortError::DaemonUnavailable);
        }
        let repository_root = canonicalize_repository_root(&repository_root)
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let service_key = ServiceKey::new(&database_path, &project_id, &repository_root);

        let _creation = self.creation_gate.lock().await;
        if self.shutdown_fenced.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionPortError::DaemonUnavailable);
        }
        {
            let services = self.services.lock().await;
            // Another identity mounted at this root makes the root ambiguous.
            if services
                .iter()
                .any(|(key, entry)| *key != service_key && entry.repository_root == repository_root)
            {
                return Err(GitIndexTransactionPortError::PolicyDenied);
            }
            if let Some(entry) = services.get(&service_key) {
                entry.authority.set(DaemonGitAuthority::Installed(source))?;
                return Ok(entry.owner());
            }
        }

        // Open/retain the store actor under the creation gate before native
        // recovery runs on a blocking thread. Later mounts reuse this actor.
        let store = open_store().map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let native_root = repository_root.clone();
        let authority = Arc::new(DaemonGitAuthoritySlot::new(source));
        let service_authority = Arc::clone(&authority);
        let mutation_queue = Arc::clone(&self.mutation_queue);
        let (project_id, service) = tokio::task::spawn_blocking(move || {
            let native = FixedDaemonGitIndexExecutor::new(
                DaemonProjectGitIndexPreviewAssembler::new(native_root, project_id.clone()),
            );
            DaemonGitIndexTransactionService::start(
                store,
                native,
                GitEffectClassifierV1::default(),
                DaemonGitIndexPolicyRecheck::new(service_authority),
                mutation_queue,
                observed_at,
            )
            .map(|service| (project_id, service))
        })
        .await
        .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)??;
        let entry = ServiceEntry {
            project_id,
            repository_root,
            service: Arc::new(service),
            authority,
        };
        let owner = entry.owner();
        self.services.lock().await.insert(service_key, entry);
        Ok(owner)
    }

    /// Retires every invocation owner attached to one exact project-session
    /// database. The caller has already fenced admission with a durable
    /// tombstone; dropping these process-local owners prevents a stale actor
    /// from retaining the deleted database.
    #[tracing::instrument(name = "daemon.git.tx.retire", level = "trace", skip_all)]
    pub async fn retire_project_database(
        &self,
        project_id: &ProjectId,
        database_path: &Path,
    ) -> Result<(), GitIndexTransactionPortError> {
        self.services
            .lock()
            .await
            .retain(|key, _| key.project_id != *project_id || key.database_path != database_path);
        self.stores
            .remove(database_path)
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)
    }

    /// Resolve only an owner already mounted by project-open admission.
    /// Missing and ambiguous roots deliberately share the same outcome.
    pub async fn for_repository_root(
        &self,
        repository_root: &std::path::Path,
    ) -> Result<Option<DaemonGitInvocationOwner>, GitIndexTransactionPortError> {
        if self.shutdown_fenced.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionPortError::DaemonUnavailable);
        }
        let repository_root = canonicalize_repository_root(repository_root)
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let services = self.services.lock().await;
        let mut matches = services
            .values()
            .filter(|entry| entry.repository_root == repository_root);
        let Some(entry) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Ok(None);
        }
        Ok(Some(entry.owner()))
    }

    #[tracing::instrument(name = "daemon.git.tx.shutdown", level = "trace", skip_all)]
    pub async fn shutdown(
        &self,
    ) -> Result<DaemonGitIndexShutdownReceiptV1, GitIndexTransactionPortError> {
        self.shutdown_fenced.store(true, Ordering::SeqCst);
        let _creation = self.creation_gate.lock().await;
        if let Some(receipt) = *self.shutdown_receipt.lock().await {
            return Ok(receipt);
        }
        let services = {
            let mut retained = self.services.lock().await;
            for entry in retained.values() {
                entry.authority.set(DaemonGitAuthority::Revoked)?;
            }
            retained.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        let services_closed = services.len();
        drop(services);
        let store_actors_joined = self
            .stores
            .shutdown_all()
            .await
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let receipt = DaemonGitIndexShutdownReceiptV1 {
            services_closed,
            store_actors_joined,
        };
        *self.shutdown_receipt.lock().await = Some(receipt);
        Ok(receipt)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(unix), allow(dead_code))] // exercised only by unix-only daemon tests
    pub async fn quarantine_preview_for_test(
        &self,
        repository_root: &std::path::Path,
        preview: &GitIndexPreviewV1,
        observed_at: UtcMicros,
    ) -> Result<(), GitIndexTransactionPortError> {
        let owner = self
            .for_repository_root(repository_root)
            .await?
            .ok_or(GitIndexTransactionPortError::DaemonUnavailable)?;
        owner
            .service
            .quarantine_preview_for_test(preview, observed_at)
    }
}
