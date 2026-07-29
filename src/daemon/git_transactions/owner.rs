//! One retained Git index transaction service per daemon-owned project store.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_application::{
    GitIndexApplyRequestV1, GitIndexOperationBindingV1, GitIndexTransactionPortError, ResolvedScope,
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
use tracedecay_tool_catalog::CapabilityId;

use crate::application::ProjectSourceAccessSnapshot;
use crate::application::configuration::ConfigurationControlStore;
use crate::catalog_composition::build_application_catalog_snapshot;
use crate::global_db::RegisteredGlobalDb;
use crate::global_db::configuration::OwnedGlobalDbConfigurationControlStore;

use super::{
    CurrentGitIndexPolicyStateV1, DaemonGitIndexTransactionService,
    DaemonProjectGitIndexPreviewAssembler, FixedDaemonGitIndexExecutor, GitIndexPolicyRecheckPort,
    GitIndexTransactionStoreRegistry, SharedDaemonGitIndexTransactionStore,
};

const GIT_POLICY_REVISION: u64 = 2;

#[derive(Clone, Debug)]
pub(crate) struct DaemonGitAuthorityStateV1 {
    pub(crate) scope: ResolvedScope,
    pub(crate) requester: ActorId,
    pub(crate) effective_capabilities: BTreeSet<CapabilityId>,
    pub(crate) grant_expires_at: UtcMicros,
    pub(crate) policy_revision: u64,
    pub(crate) policy_digest: ManifestDigest,
    pub(crate) configuration_digest: ManifestDigest,
    pub(crate) catalog_digest: ManifestDigest,
    pub(crate) privacy_digest: ManifestDigest,
    pub(crate) evaluated_at: UtcMicros,
}

pub(crate) trait DaemonGitAuthoritySource: Send + Sync {
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
    runtime: tokio::runtime::Handle,
}

impl DaemonGitAuthoritySource for ProductionDaemonGitAuthoritySource {
    fn current_capability(
        &self,
        capability_id: &CapabilityId,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError> {
        let evaluated_at = current_micros();
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
        let catalog = build_application_catalog_snapshot()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
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

#[derive(Default)]
struct DaemonGitAuthoritySlot {
    source: RwLock<Option<Arc<dyn DaemonGitAuthoritySource>>>,
}

impl DaemonGitAuthoritySlot {
    fn install(
        &self,
        source: Arc<dyn DaemonGitAuthoritySource>,
    ) -> Result<(), GitIndexTransactionPortError> {
        *self
            .source
            .write()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)? = Some(source);
        Ok(())
    }
}

impl DaemonGitAuthoritySource for DaemonGitAuthoritySlot {
    fn current_capability(
        &self,
        capability_id: &CapabilityId,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError> {
        let source = self
            .source
            .read()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?
            .as_ref()
            .ok_or(GitIndexTransactionPortError::PolicyDenied)?
            .clone();
        source.current_capability(capability_id)
    }
}

/// Rechecks current daemon-authenticated capability, configuration, catalog,
/// privacy, and exact preview scope immediately before native mutation.
pub(crate) struct DaemonGitIndexPolicyRecheck {
    authority: Arc<dyn DaemonGitAuthoritySource>,
}

impl DaemonGitIndexPolicyRecheck {
    pub(crate) fn new(authority: Arc<dyn DaemonGitAuthoritySource>) -> Self {
        Self { authority }
    }
}

impl GitIndexPolicyRecheckPort for DaemonGitIndexPolicyRecheck {
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

pub(super) fn preview_conflict_risk(preview: &GitIndexPreviewV1) -> GitConflictRiskV1 {
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

fn current_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
        )
        .unwrap_or(i64::MAX),
    )
}

pub(crate) type DaemonProjectGitIndexTransactionService = DaemonGitIndexTransactionService<
    SharedDaemonGitIndexTransactionStore,
    FixedDaemonGitIndexExecutor<DaemonProjectGitIndexPreviewAssembler>,
    GitEffectClassifierV1,
    DaemonGitIndexPolicyRecheck,
>;

#[derive(Clone)]
pub(crate) struct DaemonGitInvocationOwner {
    pub(crate) project_id: ProjectId,
    pub(crate) service: Arc<DaemonProjectGitIndexTransactionService>,
    authority: Arc<DaemonGitAuthoritySlot>,
}

impl DaemonGitInvocationOwner {
    pub(crate) fn current_authority(
        &self,
        operation: GitIndexTransactionOperationV1,
    ) -> Result<DaemonGitAuthorityStateV1, GitIndexTransactionPortError> {
        self.authority.current(operation)
    }

    pub(crate) fn current_read_authority(
        &self,
        request: &crate::application::git_reads::GitReadRequestV1,
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ServiceKey {
    database_path: PathBuf,
    project_id: ProjectId,
    repository_root: PathBuf,
}

impl ServiceKey {
    fn new(database_path: &PathBuf, project_id: &ProjectId, repository_root: &PathBuf) -> Self {
        Self {
            database_path: database_path.clone(),
            project_id: project_id.clone(),
            repository_root: repository_root.clone(),
        }
    }
}

/// Owns the store actor, native executor, classifier, policy recheck, and
/// repository queue for each exact project/worktree identity. Linked
/// worktrees share one session store actor without sharing native executors or
/// mutation authority.
#[derive(Default)]
pub(crate) struct DaemonGitIndexTransactionServiceRegistry {
    stores: GitIndexTransactionStoreRegistry,
    services: tokio::sync::Mutex<HashMap<ServiceKey, ServiceEntry>>,
    creation_gate: tokio::sync::Mutex<()>,
}

impl DaemonGitIndexTransactionServiceRegistry {
    pub(crate) async fn ensure(
        &self,
        database: Arc<RegisteredGlobalDb>,
        repository_root: PathBuf,
        project_id: ProjectId,
        observed_at: UtcMicros,
    ) -> Result<Arc<DaemonProjectGitIndexTransactionService>, GitIndexTransactionPortError> {
        // `RegisteredGlobalDb` already carries the canonical path admitted by
        // its retained runtime authority. Do not rediscover identity through
        // the filesystem: a newly opened SQLite shard may not have
        // materialized its directory entry yet.
        let database_path = database.db_path().to_path_buf();
        self.ensure_with(
            database_path,
            repository_root,
            project_id,
            observed_at,
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
        observed_at: UtcMicros,
    ) -> Result<Arc<DaemonProjectGitIndexTransactionService>, GitIndexTransactionPortError> {
        let database_path = database_path
            .canonicalize()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let store_path = database_path.clone();
        self.ensure_with(
            database_path,
            repository_root,
            project_id,
            observed_at,
            || self.stores.ensure_engine_test(store_path),
        )
        .await
    }

    async fn ensure_with<F>(
        &self,
        database_path: PathBuf,
        repository_root: PathBuf,
        project_id: ProjectId,
        observed_at: UtcMicros,
        open_store: F,
    ) -> Result<Arc<DaemonProjectGitIndexTransactionService>, GitIndexTransactionPortError>
    where
        F: FnOnce() -> tracedecay_store::GitIndexTransactionStoreResult<
            super::SharedDaemonGitIndexTransactionStore,
        >,
    {
        let repository_root = repository_root
            .canonicalize()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        if let Some(service) = self
            .existing(&database_path, &repository_root, &project_id)
            .await?
        {
            return Ok(service);
        }

        let _creation = self.creation_gate.lock().await;
        if let Some(service) = self
            .existing(&database_path, &repository_root, &project_id)
            .await?
        {
            return Ok(service);
        }

        // Open/retain the store actor under the creation gate before native
        // recovery runs on a blocking thread. Later ensures reuse this actor.
        let store = open_store().map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let native_root = repository_root.clone();
        let authority = Arc::new(DaemonGitAuthoritySlot::default());
        let service_authority = Arc::clone(&authority);
        let (project_id, service) = tokio::task::spawn_blocking(move || {
            let native = FixedDaemonGitIndexExecutor::new(
                DaemonProjectGitIndexPreviewAssembler::new(native_root, project_id.clone()),
            );
            DaemonGitIndexTransactionService::start(
                store,
                native,
                GitEffectClassifierV1::default(),
                DaemonGitIndexPolicyRecheck::new(service_authority),
                observed_at,
            )
            .map(|service| (project_id, service))
        })
        .await
        .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)??;
        let service = Arc::new(service);
        let service_key = ServiceKey::new(&database_path, &project_id, &repository_root);
        self.services.lock().await.insert(
            service_key,
            ServiceEntry {
                project_id,
                repository_root,
                service: Arc::clone(&service),
                authority,
            },
        );
        Ok(service)
    }

    async fn existing(
        &self,
        database_path: &PathBuf,
        repository_root: &PathBuf,
        project_id: &ProjectId,
    ) -> Result<Option<Arc<DaemonProjectGitIndexTransactionService>>, GitIndexTransactionPortError>
    {
        let services = self.services.lock().await;
        Ok(services
            .get(&ServiceKey::new(database_path, project_id, repository_root))
            .map(|entry| Arc::clone(&entry.service)))
    }

    pub(crate) async fn install_authority(
        &self,
        repository_root: &std::path::Path,
        access: ProjectSourceAccessSnapshot,
        configuration_database: Arc<RegisteredGlobalDb>,
        runtime: tokio::runtime::Handle,
    ) -> Result<(), GitIndexTransactionPortError> {
        let repository_root = repository_root
            .canonicalize()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let services = self.services.lock().await;
        let mut matches = services
            .values()
            .filter(|entry| entry.repository_root == repository_root);
        let Some(entry) = matches.next() else {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        };
        if matches.next().is_some()
            || access.scope.project_id != entry.project_id
            || access.binding.authority != AuthorityRef::Project(entry.project_id.clone())
        {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        entry
            .authority
            .install(Arc::new(ProductionDaemonGitAuthoritySource {
                access,
                configuration:
                    OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
                        configuration_database,
                    ),
                runtime,
            }))?;
        Ok(())
    }

    /// Resolve only an owner already mounted by project-open admission.
    /// Missing and ambiguous roots deliberately share the same outcome.
    pub(crate) async fn for_repository_root(
        &self,
        repository_root: &std::path::Path,
    ) -> Result<Option<DaemonGitInvocationOwner>, GitIndexTransactionPortError> {
        let repository_root = repository_root
            .canonicalize()
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
        Ok(Some(DaemonGitInvocationOwner {
            project_id: entry.project_id.clone(),
            service: Arc::clone(&entry.service),
            authority: Arc::clone(&entry.authority),
        }))
    }

    #[cfg(test)]
    #[cfg_attr(not(unix), allow(dead_code))] // exercised only by unix-only daemon tests
    pub(crate) async fn quarantine_preview_for_test(
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
