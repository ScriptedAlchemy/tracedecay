use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tracedecay_store::{
    CodeShardScopeV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1, StoreShardScopeV1,
    VerifiedStoreLocatorV1,
};

use crate::daemon::store_runtime::registry::{
    ClosedStoreRuntime, LifecycleShardRuntimePublisher, ProfileAuthorityPin,
    ProfileAuthorityPinResult, ResolvedStoreLocator, StoreRuntimeHandle, StoreRuntimeKey,
    StoreRuntimeOpenMode, StoreRuntimeOpenRequest, StoreRuntimeOpenResult, StoreRuntimeRegistry,
    StoreRuntimeRegistryConfig, StoreRuntimeRegistryFailure, StoreRuntimeRegistryFuture,
    StoreRuntimeResolver,
};
use crate::db::{Database, DatabaseAccessMode, DatabaseAuthority, MaintenanceDatabaseScope};
use crate::errors::{Result, TraceDecayError};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ConsolidationArtifactRoleV1 {
    DestinationCodeGraph,
    SourceCodeGraphInput,
    DestinationSessions,
    SourceSessionsInput,
    TargetSessionsInput,
    FrozenInputCodeGraph,
    FrozenInputSessions,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConsolidationArtifactAuthorityV1 {
    pub(super) role: ConsolidationArtifactRoleV1,
    pub(super) shard_id: StoreShardIdV1,
    pub(super) incarnation: StoreIncarnationV1,
    pub(super) relative_locator: PathBuf,
}

impl ConsolidationArtifactAuthorityV1 {
    pub(super) fn new(
        role: ConsolidationArtifactRoleV1,
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        relative_locator: PathBuf,
    ) -> Result<Self> {
        let authority = Self {
            role,
            shard_id,
            incarnation,
            relative_locator,
        };
        authority.validate()?;
        Ok(authority)
    }

    pub(super) fn validate(&self) -> Result<()> {
        validate_relative_locator(&self.relative_locator)?;
        let valid = matches!(
            (self.role, &self.shard_id.scope),
            (
                ConsolidationArtifactRoleV1::DestinationCodeGraph,
                StoreShardScopeV1::Code {
                    scope: CodeShardScopeV1::Worktree { .. },
                    ..
                }
            ) | (
                ConsolidationArtifactRoleV1::SourceCodeGraphInput,
                StoreShardScopeV1::Code {
                    scope: CodeShardScopeV1::Branch { .. },
                    ..
                }
            ) | (
                ConsolidationArtifactRoleV1::DestinationSessions
                    | ConsolidationArtifactRoleV1::SourceSessionsInput
                    | ConsolidationArtifactRoleV1::TargetSessionsInput
                    | ConsolidationArtifactRoleV1::FrozenInputSessions,
                StoreShardScopeV1::ProjectSessions { .. }
            ) | (
                ConsolidationArtifactRoleV1::FrozenInputCodeGraph,
                StoreShardScopeV1::Code {
                    scope: CodeShardScopeV1::Worktree { .. } | CodeShardScopeV1::Branch { .. },
                    ..
                }
            )
        );
        if !valid {
            return Err(runtime_error(
                "consolidation artifact role does not match its typed shard",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConsolidationArtifactRecordV1 {
    pub(super) authority: ConsolidationArtifactAuthorityV1,
    pub(super) file_identity: u64,
}

impl ConsolidationArtifactRecordV1 {
    pub(super) fn capture(
        root: &Path,
        authority: ConsolidationArtifactAuthorityV1,
    ) -> Result<Self> {
        authority.validate()?;
        let path = resolve_relative(root, &authority.relative_locator)?;
        let file_identity = current_file_identity(&path)?;
        Ok(Self {
            authority,
            file_identity,
        })
    }

    fn validate_expected(
        &self,
        root: &Path,
        expected: &ConsolidationArtifactAuthorityV1,
    ) -> Result<PathBuf> {
        expected.validate()?;
        self.authority.validate()?;
        if &self.authority != expected {
            return Err(runtime_error(
                "consolidation artifact ledger authority does not match the requested artifact",
            ));
        }
        let path = resolve_relative(root, &expected.relative_locator)?;
        if current_file_identity(&path)? != self.file_identity {
            return Err(runtime_error(
                "consolidation artifact file identity changed since DestinationReady",
            ));
        }
        Ok(path)
    }
}

#[derive(Clone)]
struct ExactArtifactLocatorV1 {
    verified: VerifiedStoreLocatorV1,
    path: PathBuf,
    file_identity: u64,
}

struct ExactArtifactResolverV1 {
    locators: BTreeMap<StoreRuntimeKey, ExactArtifactLocatorV1>,
}

impl StoreRuntimeResolver for ExactArtifactResolverV1 {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        mode: StoreRuntimeOpenMode,
        authority: Option<&'a DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<
        'a,
        std::result::Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            if mode != StoreRuntimeOpenMode::Existing {
                return Err(resolver_failure(
                    "consolidation artifacts must exist before runtime publication",
                ));
            }
            let locator = self.locators.get(key).ok_or_else(|| {
                resolver_failure("consolidation runtime received an unledgered typed shard")
            })?;
            let authority = authority.ok_or_else(|| {
                resolver_failure("consolidation runtime requires exact maintenance authority")
            })?;
            authority
                .require_active_write_scope("resolve consolidation artifact runtime")
                .map_err(|error| resolver_failure(error.to_string()))?;
            if authority.canonical_database_path() != locator.path {
                return Err(resolver_failure(
                    "consolidation runtime authority does not match its ledger locator",
                ));
            }
            let current = current_file_identity(&locator.path)
                .map_err(|error| resolver_failure(error.to_string()))?;
            if current != locator.file_identity {
                return Err(resolver_failure(
                    "consolidation artifact identity changed before runtime publication",
                ));
            }
            Ok(ResolvedStoreLocator::new(
                locator.verified.clone(),
                locator.path.clone(),
            ))
        })
    }
}

pub(super) struct ConsolidationRuntimeOwnerV1 {
    root: PathBuf,
    registry: StoreRuntimeRegistry,
    profile_pin: ProfileAuthorityPin,
    profile_runtime: Option<StoreRuntimeHandle>,
    profile_binding: StoreRuntimeBindingV1,
    profile_authority: DatabaseAuthority,
    authorities: BTreeMap<StoreRuntimeKey, DatabaseAuthority>,
    active_artifact: Arc<AtomicBool>,
    revocation_epoch: Arc<AtomicU64>,
}

impl ConsolidationRuntimeOwnerV1 {
    pub(super) async fn new(
        profile_root: &Path,
        root: &Path,
        lifecycle: &crate::lifecycle_lease::LifecycleLease,
        maintenance: &MaintenanceDatabaseScope<'_>,
        profile_shard: StoreShardIdV1,
        records: &[ConsolidationArtifactRecordV1],
    ) -> Result<Self> {
        if !lifecycle.is_exclusive() || !lifecycle.guards_profile(profile_root) {
            return Err(runtime_error(
                "consolidation runtime owner requires the exact exclusive profile lifecycle",
            ));
        }
        if !matches!(profile_shard.scope, StoreShardScopeV1::Profile) {
            return Err(runtime_error(
                "consolidation runtime owner requires a typed profile shard",
            ));
        }
        let root = root.canonicalize().map_err(|error| {
            runtime_error(format!(
                "could not resolve consolidation destination root '{}': {error}",
                root.display()
            ))
        })?;
        let profile_incarnation =
            StoreIncarnationV1::new(1).map_err(|error| runtime_error(error.to_string()))?;
        let profile_path = profile_root.join("global.db");
        let mut locators = BTreeMap::new();
        insert_locator(
            &mut locators,
            profile_shard.clone(),
            profile_incarnation,
            profile_path.clone(),
            current_file_identity(&profile_path)?,
        )?;
        let mut authorities = BTreeMap::new();
        for record in records {
            record.authority.validate()?;
            let path = record.validate_expected(&root, &record.authority)?;
            let key = StoreRuntimeKey::new(
                record.authority.shard_id.clone(),
                record.authority.incarnation,
            );
            let authority =
                maintenance.database_authority(&path, "mount consolidation artifact")?;
            insert_locator(
                &mut locators,
                record.authority.shard_id.clone(),
                record.authority.incarnation,
                path,
                record.file_identity,
            )?;
            if authorities.insert(key, authority).is_some() {
                return Err(runtime_error(
                    "duplicate typed consolidation artifact authority",
                ));
            }
        }
        let registry = StoreRuntimeRegistry::new(
            Arc::new(ExactArtifactResolverV1 { locators }),
            Arc::new(LifecycleShardRuntimePublisher),
        );
        let profile_authority = maintenance
            .database_authority(&profile_path, "mount consolidation profile authority")?;
        let profile_runtime = match registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                profile_shard.clone(),
                profile_incarnation,
                None,
                profile_authority.clone(),
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(runtime) => runtime,
            StoreRuntimeOpenResult::Failed(failure) => {
                return Err(registry_error(
                    "mount consolidation profile authority",
                    failure,
                ));
            }
        };
        let profile_binding = profile_runtime.binding().clone();
        let profile_pin = match registry.profile_authority_pin(&profile_shard) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            outcome => {
                drop(profile_runtime);
                registry
                    .close_exact(&profile_binding, &profile_authority)
                    .await
                    .map_err(|failure| {
                        registry_error("close unpinned consolidation profile", failure)
                    })?;
                return Err(runtime_error(format!(
                    "could not pin consolidation profile authority: {outcome:?}"
                )));
            }
        };
        Ok(Self {
            root,
            registry,
            profile_pin,
            profile_runtime: Some(profile_runtime),
            profile_binding,
            profile_authority,
            authorities,
            active_artifact: Arc::new(AtomicBool::new(false)),
            revocation_epoch: Arc::new(AtomicU64::new(1)),
        })
    }

    pub(super) async fn close(mut self) -> Result<()> {
        if self.active_artifact.load(Ordering::Acquire) {
            return Err(runtime_error(
                "cannot close consolidation runtime owner with an active artifact",
            ));
        }
        drop(self.profile_runtime.take());
        self.registry
            .close_exact(&self.profile_binding, &self.profile_authority)
            .await
            .map_err(|failure| registry_error("close consolidation profile runtime", failure))?;
        Ok(())
    }

    pub(super) async fn mount(
        &self,
        expected: &ConsolidationArtifactAuthorityV1,
        record: &ConsolidationArtifactRecordV1,
    ) -> Result<MountedConsolidationArtifactV1> {
        let path = record.validate_expected(&self.root, expected)?;
        if self
            .active_artifact
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(runtime_error(
                "consolidation runtime already has an active artifact writer",
            ));
        }
        let key = StoreRuntimeKey::new(expected.shard_id.clone(), expected.incarnation);
        let authority = self.authorities.get(&key).cloned().ok_or_else(|| {
            self.active_artifact.store(false, Ordering::Release);
            runtime_error("consolidation artifact has no retained exact authority")
        })?;
        if let Err(error) = authority.require_active_write_scope("mount consolidation artifact") {
            self.active_artifact.store(false, Ordering::Release);
            return Err(error);
        }
        if authority.canonical_database_path() != path {
            self.active_artifact.store(false, Ordering::Release);
            return Err(runtime_error(
                "consolidation artifact authority does not match its ledger locator",
            ));
        }
        let runtime = match self
            .registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                expected.shard_id.clone(),
                expected.incarnation,
                Some(self.profile_pin.clone()),
                authority.clone(),
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(runtime) => runtime,
            StoreRuntimeOpenResult::Failed(failure) => {
                self.active_artifact.store(false, Ordering::Release);
                return Err(registry_error("mount consolidation artifact", failure));
            }
        };
        let binding = runtime.binding().clone();
        if runtime.opened_file_identity() != Some(record.file_identity) {
            drop(runtime);
            if let Err(failure) = self.registry.close_exact(&binding, &authority).await {
                self.active_artifact.store(false, Ordering::Release);
                return Err(registry_error(
                    "close identity-mismatched consolidation artifact",
                    failure,
                ));
            }
            self.active_artifact.store(false, Ordering::Release);
            return Err(runtime_error(
                "mounted consolidation artifact identity differs from its ledger record",
            ));
        }
        let database = match Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await
        {
            Ok(database) => database,
            Err(error) => {
                if let Err(failure) = self.registry.close_exact(&binding, &authority).await {
                    self.active_artifact.store(false, Ordering::Release);
                    return Err(registry_error(
                        "close unpublished consolidation database facade",
                        failure,
                    ));
                }
                self.active_artifact.store(false, Ordering::Release);
                return Err(error);
            }
        };
        Ok(MountedConsolidationArtifactV1 {
            registry: self.registry.clone(),
            active_artifact: Arc::clone(&self.active_artifact),
            revocation_epoch: Arc::clone(&self.revocation_epoch),
            record: record.clone(),
            binding,
            authority,
            database: Some(database),
            closed_successfully: false,
        })
    }
}

impl Drop for ConsolidationRuntimeOwnerV1 {
    fn drop(&mut self) {
        self.revocation_epoch.fetch_add(1, Ordering::AcqRel);
    }
}

pub(super) struct MountedConsolidationArtifactV1 {
    registry: StoreRuntimeRegistry,
    active_artifact: Arc<AtomicBool>,
    revocation_epoch: Arc<AtomicU64>,
    record: ConsolidationArtifactRecordV1,
    binding: StoreRuntimeBindingV1,
    authority: DatabaseAuthority,
    database: Option<Database>,
    closed_successfully: bool,
}

impl MountedConsolidationArtifactV1 {
    pub(super) fn database(&self) -> Result<&Database> {
        self.database.as_ref().ok_or_else(|| {
            runtime_error("mounted consolidation database is retained until exact close")
        })
    }

    pub(super) async fn checkpoint_and_close_exact(mut self) -> Result<ConsolidationAttachTokenV1> {
        if let Err(error) = self
            .database()?
            .truncate_wal_for_offline_maintenance()
            .await
        {
            drop(self.database.take());
            if let Err(failure) = self
                .registry
                .close_exact(&self.binding, &self.authority)
                .await
            {
                return Err(registry_error(
                    "close consolidation artifact after checkpoint failure",
                    failure,
                ));
            }
            return Err(error);
        }
        drop(self.database.take());
        let closed = self
            .registry
            .close_exact(&self.binding, &self.authority)
            .await
            .map_err(|failure| registry_error("close consolidation artifact", failure))?;
        validate_close_proof(&closed, &self.record)?;
        self.active_artifact.store(false, Ordering::Release);
        self.closed_successfully = true;
        let token = ConsolidationAttachTokenV1 {
            closed,
            expected: self.record.authority.clone(),
            file_identity: self.record.file_identity,
            revocation_epoch: Arc::clone(&self.revocation_epoch),
            issued_epoch: self.revocation_epoch.load(Ordering::Acquire),
        };
        Ok(token)
    }

    pub(super) async fn finish_operation<T>(
        self,
        operation: Result<T>,
    ) -> Result<(T, ConsolidationAttachTokenV1)> {
        let token = self.checkpoint_and_close_exact().await?;
        operation.map(|value| (value, token))
    }
}

impl Drop for MountedConsolidationArtifactV1 {
    fn drop(&mut self) {
        self.active_artifact.store(false, Ordering::Release);
        if !self.closed_successfully {
            self.revocation_epoch.fetch_add(1, Ordering::AcqRel);
        }
    }
}

pub(super) struct ConsolidationAttachTokenV1 {
    closed: ClosedStoreRuntime,
    expected: ConsolidationArtifactAuthorityV1,
    file_identity: u64,
    revocation_epoch: Arc<AtomicU64>,
    issued_epoch: u64,
}

impl ConsolidationAttachTokenV1 {
    pub(super) fn into_verified_path(self) -> Result<PathBuf> {
        self.expected.validate()?;
        if self.closed.binding().shard_id != self.expected.shard_id
            || self.closed.binding().incarnation != self.expected.incarnation
            || self.closed.opened_file_identity() != self.file_identity
            || self.revocation_epoch.load(Ordering::Acquire) != self.issued_epoch
        {
            return Err(runtime_error(
                "consolidation attach token is stale or has the wrong exact authority",
            ));
        }
        if current_file_identity(self.closed.path())? != self.file_identity {
            return Err(runtime_error(
                "consolidation attach token names a replaced SQLite file",
            ));
        }
        let wal = super::files::sqlite_sidecar(self.closed.path(), "-wal");
        if wal.exists() {
            let bytes = std::fs::metadata(&wal)
                .map_err(|error| runtime_error(format!("inspect truncated WAL: {error}")))?
                .len();
            if bytes != 0 {
                return Err(runtime_error(
                    "consolidation attach token refuses a non-empty WAL",
                ));
            }
            std::fs::remove_file(&wal)
                .map_err(|error| runtime_error(format!("remove truncated WAL: {error}")))?;
        }
        let shm = super::files::sqlite_sidecar(self.closed.path(), "-shm");
        if shm.exists() {
            std::fs::remove_file(&shm)
                .map_err(|error| runtime_error(format!("remove closed WAL index: {error}")))?;
        }
        if wal.exists() || shm.exists() {
            return Err(runtime_error(
                "consolidation attach token could not clear closed WAL/SHM sidecars",
            ));
        }
        Ok(self.closed.path().to_path_buf())
    }
}

struct FrozenInputTaskV1 {
    release: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<()>>,
    binding: StoreRuntimeBindingV1,
    authority: DatabaseAuthority,
}

/// Exclusive writer reservations over every original input database.
///
/// The reservations use the same registry/publisher path as live daemon
/// stores. They are released and joined before destination runtimes are
/// mounted, so `global.db` never has two physical writer owners.
pub(super) struct FrozenInputRuntimeSetV1 {
    registry: StoreRuntimeRegistry,
    tasks: Vec<FrozenInputTaskV1>,
    profile_runtime: Option<StoreRuntimeHandle>,
    profile_binding: StoreRuntimeBindingV1,
    profile_authority: DatabaseAuthority,
}

impl FrozenInputRuntimeSetV1 {
    pub(super) async fn acquire(
        profile_root: &Path,
        lifecycle: &crate::lifecycle_lease::LifecycleLease,
        maintenance: &MaintenanceDatabaseScope<'_>,
        profile_shard: StoreShardIdV1,
        records: &[ConsolidationArtifactRecordV1],
    ) -> Result<Self> {
        if !lifecycle.is_exclusive() || !lifecycle.guards_profile(profile_root) {
            return Err(runtime_error(
                "frozen input runtimes require the exact exclusive profile lifecycle",
            ));
        }
        if !matches!(profile_shard.scope, StoreShardScopeV1::Profile) {
            return Err(runtime_error(
                "frozen input runtimes require a typed profile shard",
            ));
        }
        let code_count = records
            .iter()
            .filter(|record| {
                record.authority.role == ConsolidationArtifactRoleV1::FrozenInputCodeGraph
            })
            .count();
        if code_count == 0 {
            return Err(runtime_error(
                "frozen input runtime inventory has no code databases",
            ));
        }
        let profile_incarnation =
            StoreIncarnationV1::new(1).map_err(|error| runtime_error(error.to_string()))?;
        let profile_path = profile_root.join("global.db");
        let mut locators = BTreeMap::new();
        insert_locator(
            &mut locators,
            profile_shard.clone(),
            profile_incarnation,
            profile_path.clone(),
            current_file_identity(&profile_path)?,
        )?;
        let mut authorities = BTreeMap::new();
        for record in records {
            record.authority.validate()?;
            if !matches!(
                record.authority.role,
                ConsolidationArtifactRoleV1::FrozenInputCodeGraph
                    | ConsolidationArtifactRoleV1::FrozenInputSessions
            ) {
                return Err(runtime_error(
                    "frozen input inventory contains a destination artifact",
                ));
            }
            let path = record.validate_expected(profile_root, &record.authority)?;
            let key = StoreRuntimeKey::new(
                record.authority.shard_id.clone(),
                record.authority.incarnation,
            );
            let authority =
                maintenance.database_authority(&path, "freeze consolidation input runtime")?;
            insert_locator(
                &mut locators,
                record.authority.shard_id.clone(),
                record.authority.incarnation,
                path,
                record.file_identity,
            )?;
            if authorities.insert(key, authority).is_some() {
                return Err(runtime_error(
                    "duplicate typed frozen-input runtime authority",
                ));
            }
        }
        let config = StoreRuntimeRegistryConfig::for_exclusive_maintenance(code_count)
            .map_err(|failure| registry_error("configure frozen input runtimes", failure))?;
        let registry = StoreRuntimeRegistry::with_config(
            Arc::new(ExactArtifactResolverV1 { locators }),
            Arc::new(LifecycleShardRuntimePublisher),
            config,
        )
        .map_err(|failure| registry_error("construct frozen input registry", failure))?;
        let profile_authority = maintenance
            .database_authority(&profile_path, "mount frozen-input profile authority")?;
        let profile_runtime = match registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                profile_shard.clone(),
                profile_incarnation,
                None,
                profile_authority.clone(),
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(runtime) => runtime,
            StoreRuntimeOpenResult::Failed(failure) => {
                return Err(registry_error(
                    "mount frozen-input profile authority",
                    failure,
                ));
            }
        };
        let profile_binding = profile_runtime.binding().clone();
        let profile_pin = match registry.profile_authority_pin(&profile_shard) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            outcome => {
                drop(profile_runtime);
                registry
                    .close_exact(&profile_binding, &profile_authority)
                    .await
                    .map_err(|failure| {
                        registry_error("close unpinned frozen-input profile", failure)
                    })?;
                return Err(runtime_error(format!(
                    "could not pin frozen-input profile authority: {outcome:?}"
                )));
            }
        };
        let mut frozen = Self {
            registry,
            tasks: Vec::with_capacity(records.len()),
            profile_runtime: Some(profile_runtime),
            profile_binding,
            profile_authority,
        };
        for record in records {
            if let Err(error) = frozen.acquire_one(&profile_pin, &authorities, record).await {
                let cleanup = frozen.release_and_join_inner().await;
                return match cleanup {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => Err(runtime_error(format!(
                        "{error}; frozen-input cleanup also failed: {cleanup_error}"
                    ))),
                };
            }
        }
        Ok(frozen)
    }

    async fn acquire_one(
        &mut self,
        profile_pin: &ProfileAuthorityPin,
        authorities: &BTreeMap<StoreRuntimeKey, DatabaseAuthority>,
        record: &ConsolidationArtifactRecordV1,
    ) -> Result<()> {
        let key = StoreRuntimeKey::new(
            record.authority.shard_id.clone(),
            record.authority.incarnation,
        );
        let authority = authorities
            .get(&key)
            .cloned()
            .ok_or_else(|| runtime_error("frozen input has no exact retained authority"))?;
        let runtime = match self
            .registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                record.authority.shard_id.clone(),
                record.authority.incarnation,
                Some(profile_pin.clone()),
                authority.clone(),
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(runtime) => runtime,
            StoreRuntimeOpenResult::Failed(failure) => {
                return Err(registry_error("mount frozen input runtime", failure));
            }
        };
        let binding = runtime.binding().clone();
        if runtime.opened_file_identity() != Some(record.file_identity) {
            drop(runtime);
            self.registry
                .close_exact(&binding, &authority)
                .await
                .map_err(|failure| {
                    registry_error("close identity-mismatched frozen input", failure)
                })?;
            return Err(runtime_error(
                "frozen input runtime identity differs from its exact record",
            ));
        }
        let database = match Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await
        {
            Ok(database) => database,
            Err(error) => {
                self.registry
                    .close_exact(&binding, &authority)
                    .await
                    .map_err(|failure| {
                        registry_error("close unpublished frozen input runtime", failure)
                    })?;
                return Err(error);
            }
        };
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let transaction = match database
                .begin_write_transaction("freeze consolidation input runtime")
                .await
            {
                Ok(transaction) => transaction,
                Err(error) => {
                    let message = error.to_string();
                    let _ = ready_tx.send(Err(message.clone()));
                    return Err(runtime_error(message));
                }
            };
            if ready_tx.send(Ok(())).is_err() {
                let _ = transaction.rollback().await;
                return Err(runtime_error("frozen input readiness receiver was dropped"));
            }
            let _ = release_rx.await;
            transaction.rollback().await
        });
        match ready_rx.await {
            Ok(Ok(())) => {
                self.tasks.push(FrozenInputTaskV1 {
                    release: Some(release_tx),
                    task,
                    binding,
                    authority,
                });
                Ok(())
            }
            Ok(Err(message)) => {
                drop(release_tx);
                let _ = task.await;
                self.registry
                    .close_exact(&binding, &authority)
                    .await
                    .map_err(|failure| registry_error("close failed frozen input", failure))?;
                Err(runtime_error(message))
            }
            Err(error) => {
                drop(release_tx);
                let _ = task.await;
                self.registry
                    .close_exact(&binding, &authority)
                    .await
                    .map_err(|failure| registry_error("close failed frozen input", failure))?;
                Err(runtime_error(format!(
                    "frozen input readiness failed: {error}"
                )))
            }
        }
    }

    pub(super) async fn release_and_join(mut self) -> Result<()> {
        self.release_and_join_inner().await
    }

    async fn release_and_join_inner(&mut self) -> Result<()> {
        for frozen in &mut self.tasks {
            if let Some(release) = frozen.release.take() {
                let _ = release.send(());
            }
        }
        let mut first_error = None;
        for frozen in self.tasks.drain(..) {
            let result = frozen
                .task
                .await
                .map_err(|error| runtime_error(format!("frozen input task failed: {error}")))
                .and_then(|result| result);
            if first_error.is_none() {
                first_error = result.err();
            }
            let closed = self
                .registry
                .close_exact(&frozen.binding, &frozen.authority)
                .await
                .map_err(|failure| registry_error("close frozen input runtime", failure));
            if first_error.is_none() {
                first_error = closed.err();
            }
        }
        drop(self.profile_runtime.take());
        let profile_closed = self
            .registry
            .close_exact(&self.profile_binding, &self.profile_authority)
            .await
            .map_err(|failure| registry_error("close frozen-input profile runtime", failure));
        if first_error.is_none() {
            first_error = profile_closed.err();
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for FrozenInputRuntimeSetV1 {
    fn drop(&mut self) {
        for frozen in &mut self.tasks {
            if let Some(release) = frozen.release.take() {
                let _ = release.send(());
            }
        }
    }
}

fn insert_locator(
    locators: &mut BTreeMap<StoreRuntimeKey, ExactArtifactLocatorV1>,
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    path: PathBuf,
    file_identity: u64,
) -> Result<()> {
    let path = path.canonicalize().map_err(|error| {
        runtime_error(format!(
            "could not canonicalize consolidation runtime locator '{}': {error}",
            path.display()
        ))
    })?;
    let key = StoreRuntimeKey::new(shard_id.clone(), incarnation);
    let verified = VerifiedStoreLocatorV1::new(
        shard_id,
        incarnation,
        crate::daemon::store_runtime::resolver::canonical_store_locator_digest(&path)
            .map_err(runtime_error)?,
    );
    if locators
        .insert(
            key,
            ExactArtifactLocatorV1 {
                verified,
                path,
                file_identity,
            },
        )
        .is_some()
    {
        return Err(runtime_error(
            "duplicate typed consolidation artifact authority",
        ));
    }
    Ok(())
}

fn resolve_relative(root: &Path, relative: &Path) -> Result<PathBuf> {
    validate_relative_locator(relative)?;
    let path = root.join(relative);
    let canonical = path.canonicalize().map_err(|error| {
        runtime_error(format!(
            "could not resolve consolidation artifact '{}': {error}",
            path.display()
        ))
    })?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(runtime_error(
            "consolidation artifact locator is not an exact file beneath its destination",
        ));
    }
    Ok(canonical)
}

fn validate_relative_locator(locator: &Path) -> Result<()> {
    if locator.as_os_str().is_empty()
        || locator.is_absolute()
        || locator
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(runtime_error(
            "consolidation artifact locator must be a normalized relative path",
        ));
    }
    Ok(())
}

fn current_file_identity(path: &Path) -> Result<u64> {
    crate::db::sqlite_generation_identity(path).map_err(|_| {
        runtime_error(format!(
            "could not verify consolidation artifact file identity '{}'",
            path.display()
        ))
    })
}

fn validate_close_proof(
    closed: &ClosedStoreRuntime,
    record: &ConsolidationArtifactRecordV1,
) -> Result<()> {
    if closed.binding().shard_id != record.authority.shard_id
        || closed.binding().incarnation != record.authority.incarnation
        || closed.opened_file_identity() != record.file_identity
    {
        return Err(runtime_error(
            "closed consolidation runtime proof does not match its ledger authority",
        ));
    }
    Ok(())
}

fn resolver_failure(message: impl Into<String>) -> StoreRuntimeRegistryFailure {
    StoreRuntimeRegistryFailure::ResolverFailed {
        message: message.into(),
    }
}

fn registry_error(
    operation: &'static str,
    failure: StoreRuntimeRegistryFailure,
) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: format!("{failure:?}"),
    }
}

fn runtime_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}
