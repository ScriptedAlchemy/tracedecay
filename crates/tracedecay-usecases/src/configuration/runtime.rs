//! Retained daemon composition for the configuration control plane.
//!
//! This module owns only lifetime and delegation. Resolution, validation,
//! authorization, mutation, audit, and credential semantics remain in the
//! existing application operations and Plan20 store.

use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1,
    CredentialReferenceMetadataV1, ProtectedApplyRequest, ProtectedChange, ProtectedChangePlan,
    SettingKey,
};

use crate::config::{
    OpenedRuntimeConfiguration, PinnedRuntimeConfiguration, RuntimeConfigurationTarget,
};
use crate::semantic_runtime::{
    ProductionSemanticActivationCoordinatorV1, SemanticConfigurationSnapshotSourceV1,
};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

use super::operations::{ConfigurationControlPlane, ConfigurationControlPlaneOperations};
use super::ports::{
    ConfigurationClock, ConfigurationMutationAuthorizationPort, ConfigurationOperationFuture,
    ScopeResolutionPort, ScopeRevalidationEvidenceV1,
};
use super::types::{
    AuthorizedActor, ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationError, ConfigurationMutationAuthority, ConfigurationMutationReceipt,
    ConfigurationRollbackRequest, DirectConfigurationMutation, ResolvedSetting, SettingSummary,
    WriteOnlyCredentialMutation,
};
use super::user_settings::{ProductionUserSettingsDaemonClient, UserSettingsDaemonClient};

type SharedConfigurationControlPlane = Arc<dyn ConfigurationControlPlane + Send + Sync>;

pub(crate) const RUNTIME_CONFIGURATION_COMPONENT: &str = "configuration.runtime-cache";

/// Retained project-level control-plane runtime. It owns the one opened
/// Plan20 store handle and the one application operation facade used by every
/// local transport.
pub struct ProjectConfigurationRuntime {
    target: RuntimeConfigurationTarget,
    configuration_database: RegisteredGlobalDbLeaseV1,
    authorities: Arc<ConfigurationAuthoritySlots>,
    client: Arc<ProductionConfigurationDaemonClient>,
    semantic_runtime: OnceLock<Arc<ProductionSemanticActivationCoordinatorV1>>,
    user_settings: Arc<ProductionUserSettingsDaemonClient>,
}

impl ProjectConfigurationRuntime {
    pub fn open(opened: OpenedRuntimeConfiguration) -> Result<(Self, PinnedRuntimeConfiguration)> {
        let OpenedRuntimeConfiguration {
            configuration,
            registered_database,
        } = opened;
        let target = configuration.target.clone();
        let profile_id = registered_database.binding().shard_id.profile_id.clone();
        let registry = crate::config::registry::ConfigurationRegistry::core().map_err(|error| {
            TraceDecayError::Config {
                message: format!("configuration registry unavailable: {error}"),
            }
        })?;
        let registry = Arc::new(registry);
        let store = OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
            registered_database.clone(),
        );
        let authorities = Arc::new(ConfigurationAuthoritySlots::new());
        let control_plane: SharedConfigurationControlPlane =
            Arc::new(RetainedConfigurationControlPlane {
                registry,
                store: store.clone(),
                scopes: SharedScopeResolution(Arc::clone(&authorities)),
                authorization: SharedMutationAuthorization(Arc::clone(&authorities)),
                clock: SystemConfigurationClock,
            });
        let client = Arc::new(ProductionConfigurationDaemonClient {
            target: configuration.target.clone(),
            store,
            control_plane: Arc::clone(&control_plane),
        });
        let user_settings = Arc::new(ProductionUserSettingsDaemonClient::new(
            Arc::clone(&client),
            profile_id,
        ));
        Ok((
            Self {
                target,
                configuration_database: registered_database,
                authorities,
                client,
                semantic_runtime: OnceLock::new(),
                user_settings,
            },
            configuration,
        ))
    }

    /// Immutable routing identity only. Effective values and revisions must be
    /// read from [`Self::client`] so the retained store remains the sole
    /// runtime configuration authority.
    pub fn configuration_target(&self) -> &RuntimeConfigurationTarget {
        &self.target
    }

    pub fn registered_database(&self) -> RegisteredGlobalDbLeaseV1 {
        self.configuration_database.clone()
    }

    pub fn install_authorities(
        &self,
        scopes: Arc<dyn ScopeResolutionPort + Send + Sync>,
        authorization: Arc<dyn ConfigurationMutationAuthorizationPort + Send + Sync>,
    ) -> Result<()> {
        self.authorities.install(scopes, authorization)
    }

    pub fn client(&self) -> Arc<ProductionConfigurationDaemonClient> {
        Arc::clone(&self.client)
    }

    /// Daemon-owned user-profile settings authority. Dashboard and other
    /// adapters receive this narrow client rather than loading `config.toml`.
    pub fn user_settings_client(&self) -> Arc<dyn UserSettingsDaemonClient> {
        Arc::clone(&self.user_settings) as Arc<dyn UserSettingsDaemonClient>
    }

    pub fn configuration_store(&self) -> OwnedGlobalDbConfigurationControlStore {
        self.client.store.clone()
    }

    pub fn record_runtime_activation(
        &self,
        observed_revision_id: Option<ConfigurationRevisionId>,
        activation_error_code: Option<String>,
        occurred_at: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ()> {
        self.client.store.record_component_activation(
            RUNTIME_CONFIGURATION_COMPONENT.to_owned(),
            observed_revision_id,
            activation_error_code,
            occurred_at,
        )
    }

    pub fn install_semantic_runtime(
        &self,
        runtime: Arc<ProductionSemanticActivationCoordinatorV1>,
    ) -> Result<()> {
        let _ = self.semantic_runtime.set(runtime);
        Ok(())
    }

    pub fn semantic_configuration_inventory_authority(
        &self,
    ) -> Option<crate::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1> {
        self.semantic_runtime
            .get()
            .map(|runtime| runtime.configuration_inventory_authority())
    }

    pub(crate) fn semantic_activation_coordinator(
        &self,
    ) -> Option<Arc<ProductionSemanticActivationCoordinatorV1>> {
        self.semantic_runtime.get().cloned()
    }

    pub(crate) async fn authorize_semantic_configuration_mutation(
        &self,
        authority: ConfigurationMutationAuthority,
        expected_revision: &ConfigurationRevisionId,
        now: UtcMicros,
    ) -> std::result::Result<(), crate::semantic_runtime::SemanticActivationCoordinationErrorV1>
    {
        self.retrieval_profile_mutation_capability(authority, expected_revision, now)
            .await
            .map(|_| ())
    }

    pub(crate) async fn bootstrap_query_retrieval_profile(
        &self,
        configuration: super::ports::ConfigurationCurrentStateV1,
        accepted_query: crate::config::retrieval::AcceptedRetrievalProfileV1,
        runtime: &crate::config::retrieval::RetrievalRuntimeCompatibilityV1,
    ) -> std::result::Result<(), crate::semantic_runtime::SemanticActivationCoordinationErrorV1>
    {
        self.semantic_runtime
            .get()
            .ok_or(crate::semantic_runtime::SemanticActivationCoordinationErrorV1::Unavailable)?
            .bootstrap_query_profile(configuration, accepted_query, runtime)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn stage_and_activate_semantic(
        &self,
        base_configuration: crate::semantic_runtime::SemanticConfigurationPinV1,
        result_configuration: super::ports::ConfigurationCurrentStateV1,
        authority: ConfigurationMutationAuthority,
        expected: crate::config::retrieval::RetrievalProfileCasV1,
        candidate: crate::config::retrieval::AcceptedRetrievalProfileV1,
        current_runtime: &crate::config::retrieval::RetrievalRuntimeCompatibilityV1,
        candidate_runtime: &crate::config::retrieval::RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        freshness_vector_digest: tracedecay_domain::ManifestDigest,
        now: UtcMicros,
    ) -> std::result::Result<
        crate::semantic_runtime::SemanticActivationReceiptV1,
        crate::semantic_runtime::SemanticActivationCoordinationErrorV1,
    > {
        let capability = self
            .retrieval_profile_mutation_capability(
                authority,
                &expected.expected_configuration_revision,
                now,
            )
            .await?;
        self.semantic_runtime
            .get()
            .ok_or(crate::semantic_runtime::SemanticActivationCoordinationErrorV1::Unavailable)?
            .stage_and_activate(
                base_configuration,
                result_configuration,
                &capability,
                expected,
                candidate,
                current_runtime,
                candidate_runtime,
                central_mutation,
                freshness_vector_digest,
                now,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn stage_and_rollback_semantic(
        &self,
        base_configuration: crate::semantic_runtime::SemanticConfigurationPinV1,
        result_configuration: super::ports::ConfigurationCurrentStateV1,
        authority: ConfigurationMutationAuthority,
        expected: crate::config::retrieval::RetrievalProfileCasV1,
        restored_runtime: &crate::config::retrieval::RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        trigger: String,
        freshness_vector_digest: tracedecay_domain::ManifestDigest,
        now: UtcMicros,
    ) -> std::result::Result<
        crate::semantic_runtime::SemanticRollbackReceiptV1,
        crate::semantic_runtime::SemanticActivationCoordinationErrorV1,
    > {
        let capability = self
            .retrieval_profile_mutation_capability(
                authority,
                &expected.expected_configuration_revision,
                now,
            )
            .await?;
        self.semantic_runtime
            .get()
            .ok_or(crate::semantic_runtime::SemanticActivationCoordinationErrorV1::Unavailable)?
            .stage_and_rollback(
                base_configuration,
                result_configuration,
                &capability,
                expected,
                restored_runtime,
                central_mutation,
                trigger,
                freshness_vector_digest,
                now,
            )
            .await
    }

    async fn retrieval_profile_mutation_capability(
        &self,
        authority: ConfigurationMutationAuthority,
        expected_revision: &ConfigurationRevisionId,
        now: UtcMicros,
    ) -> std::result::Result<
        crate::config::retrieval::RetrievalProfileMutationCapabilityV1,
        crate::semantic_runtime::SemanticActivationCoordinationErrorV1,
    > {
        let current = self
            .authorities
            .installed_mutation_authorization()
            .map_err(|_| {
                crate::semantic_runtime::SemanticActivationCoordinationErrorV1::Unavailable
            })?
            .recheck(
                &authority.receipt,
                tracedecay_domain::configuration::ConfigurationMutationOperationV1::DirectMutation,
                expected_revision,
                tracedecay_domain::configuration::ConfigurationMutationSinkV1::ConfigurationStore,
                tracedecay_domain::configuration::ConfigurationMutationEffectV1::CommitConfigurationRevision,
                now,
            )
            .await
            .map_err(|error| match error {
                ConfigurationError::Unavailable => {
                    crate::semantic_runtime::SemanticActivationCoordinationErrorV1::Unavailable
                }
                _ => {
                    crate::semantic_runtime::SemanticActivationCoordinationErrorV1::Rejected
                }
            })?;
        crate::config::retrieval::RetrievalProfileMutationCapabilityV1::from_current_authorization(
            authority, current,
        )
        .map_err(|_| crate::semantic_runtime::SemanticActivationCoordinationErrorV1::Rejected)
    }
}

/// Production daemon client for the retained project configuration runtime.
///
/// Reads and daemon-authorized mutations share the same retained application
/// operations and transactional store.
pub struct ProductionConfigurationDaemonClient {
    target: RuntimeConfigurationTarget,
    store: OwnedGlobalDbConfigurationControlStore,
    control_plane: SharedConfigurationControlPlane,
}

impl ProductionConfigurationDaemonClient {
    pub fn list(
        &self,
        actor: AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<SettingSummary>> {
        self.control_plane.list(actor)
    }

    pub fn explain(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> ConfigurationOperationFuture<'_, ResolvedSetting> {
        self.control_plane.explain(actor, key)
    }

    pub fn get(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> ConfigurationOperationFuture<'_, ResolvedSetting> {
        self.control_plane.get(actor, key)
    }

    pub fn mutate_direct(
        &self,
        authority: ConfigurationMutationAuthority,
        mutation: DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        self.control_plane
            .mutate_direct(authority, mutation, expected_revision)
    }

    pub fn set(
        &self,
        authority: ConfigurationMutationAuthority,
        layer: ConfigurationLayerIdV1,
        key: SettingKey,
        value: ConfigurationValueV1,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        self.mutate_direct(
            authority,
            DirectConfigurationMutation::Set { layer, key, value },
            expected_revision,
        )
    }

    pub fn unset(
        &self,
        authority: ConfigurationMutationAuthority,
        layer: ConfigurationLayerIdV1,
        key: SettingKey,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        self.mutate_direct(
            authority,
            DirectConfigurationMutation::Unset { layer, key },
            expected_revision,
        )
    }

    pub fn batch(
        &self,
        authority: ConfigurationMutationAuthority,
        mutations: Vec<DirectConfigurationMutation>,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        self.mutate_direct(
            authority,
            DirectConfigurationMutation::Batch { mutations },
            expected_revision,
        )
    }

    pub fn write_credential(
        &self,
        authority: ConfigurationMutationAuthority,
        write: WriteOnlyCredentialMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, CredentialReferenceMetadataV1> {
        self.control_plane
            .write_credential(authority, write, expected_revision)
    }

    pub fn observed_state(
        &self,
        actor: AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>> {
        self.control_plane.observed_state(actor)
    }

    pub fn dry_run_protected_change(
        &self,
        authority: ConfigurationMutationAuthority,
        change: ProtectedChange,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        self.control_plane
            .dry_run_protected_change(authority, change, expected_revision)
    }

    pub fn apply_protected_change(
        &self,
        authority: ConfigurationMutationAuthority,
        request: ProtectedApplyRequest,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        self.control_plane
            .apply_protected_change(authority, request)
    }

    pub fn dry_run_rollback(
        &self,
        authority: ConfigurationMutationAuthority,
        rollback: ConfigurationRollbackRequest,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        self.control_plane.dry_run_rollback(authority, rollback)
    }

    pub fn apply_rollback(
        &self,
        authority: ConfigurationMutationAuthority,
        request: ProtectedApplyRequest,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        self.control_plane.apply_rollback(authority, request)
    }

    pub fn audit(
        &self,
        actor: AuthorizedActor,
        query: ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage> {
        self.control_plane.audit(actor, query)
    }

    pub fn current(&self) -> ConfigurationOperationFuture<'_, PinnedRuntimeConfiguration> {
        let store = self.store.clone();
        let target = self.target.clone();
        Box::pin(async move {
            let current = super::ports::ConfigurationControlStore::current(&store).await?;
            PinnedRuntimeConfiguration::new(target, current.revision_id, current.snapshot)
                .map_err(|_| ConfigurationError::Unavailable)
        })
    }
}

impl SemanticConfigurationSnapshotSourceV1 for ProductionConfigurationDaemonClient {
    fn current_configuration(
        &self,
    ) -> ConfigurationOperationFuture<'_, super::ports::ConfigurationCurrentStateV1> {
        let store = self.store.clone();
        Box::pin(async move { super::ports::ConfigurationControlStore::current(&store).await })
    }
}

struct RetainedConfigurationControlPlane {
    registry: Arc<crate::config::registry::ConfigurationRegistry>,
    store: OwnedGlobalDbConfigurationControlStore,
    scopes: SharedScopeResolution,
    authorization: SharedMutationAuthorization,
    clock: SystemConfigurationClock,
}

impl ConfigurationControlPlane for RetainedConfigurationControlPlane {
    fn list(
        &self,
        actor: AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<SettingSummary>> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .list(actor)
            .await
        })
    }

    fn explain(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> ConfigurationOperationFuture<'_, ResolvedSetting> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .explain(actor, key)
            .await
        })
    }

    fn get(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> ConfigurationOperationFuture<'_, ResolvedSetting> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .get(actor, key)
            .await
        })
    }

    fn mutate_direct(
        &self,
        authority: ConfigurationMutationAuthority,
        mutation: DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .mutate_direct(authority, mutation, expected_revision)
            .await
        })
    }

    fn write_credential(
        &self,
        authority: ConfigurationMutationAuthority,
        write: WriteOnlyCredentialMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, CredentialReferenceMetadataV1> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .write_credential(authority, write, expected_revision)
            .await
        })
    }

    fn observed_state(
        &self,
        actor: AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .observed_state(actor)
            .await
        })
    }

    fn dry_run_protected_change(
        &self,
        authority: ConfigurationMutationAuthority,
        change: ProtectedChange,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .dry_run_protected_change(authority, change, expected_revision)
            .await
        })
    }

    fn apply_protected_change(
        &self,
        authority: ConfigurationMutationAuthority,
        request: ProtectedApplyRequest,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .apply_protected_change(authority, request)
            .await
        })
    }

    fn dry_run_rollback(
        &self,
        authority: ConfigurationMutationAuthority,
        rollback: ConfigurationRollbackRequest,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .dry_run_rollback(authority, rollback)
            .await
        })
    }

    fn apply_rollback(
        &self,
        authority: ConfigurationMutationAuthority,
        request: ProtectedApplyRequest,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .apply_rollback(authority, request)
            .await
        })
    }

    fn audit(
        &self,
        actor: AuthorizedActor,
        query: ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage> {
        Box::pin(async move {
            ConfigurationControlPlaneOperations::new(
                self.registry.as_ref(),
                &self.store,
                &self.scopes,
                &self.store,
                &self.authorization,
                &self.clock,
            )
            .audit(actor, query)
            .await
        })
    }
}

struct InstalledConfigurationAuthorities {
    scopes: Arc<dyn ScopeResolutionPort + Send + Sync>,
    authorization: Arc<dyn ConfigurationMutationAuthorizationPort + Send + Sync>,
}

struct ConfigurationAuthoritySlots {
    installed: OnceLock<InstalledConfigurationAuthorities>,
}

impl ConfigurationAuthoritySlots {
    fn new() -> Self {
        Self {
            installed: OnceLock::new(),
        }
    }

    fn install(
        &self,
        scopes: Arc<dyn ScopeResolutionPort + Send + Sync>,
        authorization: Arc<dyn ConfigurationMutationAuthorizationPort + Send + Sync>,
    ) -> Result<()> {
        self.installed
            .set(InstalledConfigurationAuthorities {
                scopes,
                authorization,
            })
            .map_err(|_| TraceDecayError::Config {
                message: "configuration runtime authorities are already installed".to_owned(),
            })
    }

    fn scope_resolution(
        &self,
    ) -> std::result::Result<&Arc<dyn ScopeResolutionPort + Send + Sync>, ConfigurationError> {
        self.installed
            .get()
            .map(|authorities| &authorities.scopes)
            .ok_or(ConfigurationError::Unavailable)
    }

    fn installed_mutation_authorization(
        &self,
    ) -> std::result::Result<
        &Arc<dyn ConfigurationMutationAuthorizationPort + Send + Sync>,
        ConfigurationError,
    > {
        self.installed
            .get()
            .map(|authorities| &authorities.authorization)
            .ok_or(ConfigurationError::Unavailable)
    }
}

struct SharedScopeResolution(Arc<ConfigurationAuthoritySlots>);

impl ScopeResolutionPort for SharedScopeResolution {
    fn resolve_protected_change<'a>(
        &'a self,
        actor: &'a AuthorizedActor,
        change: &'a ProtectedChange,
    ) -> ConfigurationOperationFuture<'a, ScopeRevalidationEvidenceV1> {
        let Ok(scopes) = self.0.scope_resolution() else {
            return Box::pin(async { Err(ConfigurationError::Unavailable) });
        };
        scopes.resolve_protected_change(actor, change)
    }

    fn revalidate_plan<'a>(
        &'a self,
        actor: &'a AuthorizedActor,
        plan: &'a ProtectedChangePlan,
    ) -> ConfigurationOperationFuture<'a, ScopeRevalidationEvidenceV1> {
        let Ok(scopes) = self.0.scope_resolution() else {
            return Box::pin(async { Err(ConfigurationError::Unavailable) });
        };
        scopes.revalidate_plan(actor, plan)
    }
}

struct SharedMutationAuthorization(Arc<ConfigurationAuthoritySlots>);

impl ConfigurationMutationAuthorizationPort for SharedMutationAuthorization {
    fn recheck<'a>(
        &'a self,
        receipt: &'a tracedecay_domain::configuration::ConfigurationMutationGrantReceiptV1,
        operation: tracedecay_domain::configuration::ConfigurationMutationOperationV1,
        expected_revision: &'a ConfigurationRevisionId,
        sink: tracedecay_domain::configuration::ConfigurationMutationSinkV1,
        effect: tracedecay_domain::configuration::ConfigurationMutationEffectV1,
        now: UtcMicros,
    ) -> ConfigurationOperationFuture<'a, super::ports::CurrentConfigurationMutationAuthorizationV1>
    {
        let Ok(authorization) = self.0.installed_mutation_authorization() else {
            return Box::pin(async { Err(ConfigurationError::Unavailable) });
        };
        authorization.recheck(receipt, operation, expected_revision, sink, effect, now)
    }
}

struct SystemConfigurationClock;

impl ConfigurationClock for SystemConfigurationClock {
    fn now(&self) -> UtcMicros {
        UtcMicros(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| {
                    duration.as_micros().min(i64::MAX as u128) as i64
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic_runtime::SemanticConfigurationSnapshotSourceV1;
    use tracedecay_domain::configuration::ConfigurationValueKindV1;

    use crate::config::{SEMANTIC_RUNTIME_SETTING_KEY, SemanticConfig};

    struct TestScopeResolution;

    impl ScopeResolutionPort for TestScopeResolution {
        fn resolve_protected_change<'a>(
            &'a self,
            _actor: &'a AuthorizedActor,
            _change: &'a ProtectedChange,
        ) -> ConfigurationOperationFuture<'a, ScopeRevalidationEvidenceV1> {
            unreachable!("authority installation test does not invoke the scope port")
        }

        fn revalidate_plan<'a>(
            &'a self,
            _actor: &'a AuthorizedActor,
            _plan: &'a ProtectedChangePlan,
        ) -> ConfigurationOperationFuture<'a, ScopeRevalidationEvidenceV1> {
            unreachable!("authority installation test does not invoke the scope port")
        }
    }

    struct TestMutationAuthorization;

    impl ConfigurationMutationAuthorizationPort for TestMutationAuthorization {
        fn recheck<'a>(
            &'a self,
            _receipt: &'a tracedecay_domain::configuration::ConfigurationMutationGrantReceiptV1,
            _operation: tracedecay_domain::configuration::ConfigurationMutationOperationV1,
            _expected_revision: &'a ConfigurationRevisionId,
            _sink: tracedecay_domain::configuration::ConfigurationMutationSinkV1,
            _effect: tracedecay_domain::configuration::ConfigurationMutationEffectV1,
            _now: UtcMicros,
        ) -> ConfigurationOperationFuture<
            'a,
            super::super::ports::CurrentConfigurationMutationAuthorizationV1,
        > {
            unreachable!("authority installation test does not invoke the authorization port")
        }
    }

    #[test]
    fn configuration_authorities_fail_closed_until_installed() {
        let authorities = ConfigurationAuthoritySlots::new();

        assert!(matches!(
            authorities.scope_resolution(),
            Err(ConfigurationError::Unavailable)
        ));
        assert!(matches!(
            authorities.installed_mutation_authorization(),
            Err(ConfigurationError::Unavailable)
        ));
    }

    #[test]
    fn configuration_authorities_bind_atomically_once() {
        let authorities = ConfigurationAuthoritySlots::new();
        let scopes: Arc<dyn ScopeResolutionPort + Send + Sync> = Arc::new(TestScopeResolution);
        let authorization: Arc<dyn ConfigurationMutationAuthorizationPort + Send + Sync> =
            Arc::new(TestMutationAuthorization);

        authorities
            .install(Arc::clone(&scopes), Arc::clone(&authorization))
            .expect("first authority installation");
        assert!(Arc::ptr_eq(
            authorities.scope_resolution().expect("installed scopes"),
            &scopes
        ));
        assert!(Arc::ptr_eq(
            authorities
                .installed_mutation_authorization()
                .expect("installed authorization"),
            &authorization
        ));

        let error = authorities
            .install(scopes, authorization)
            .expect_err("second authority installation must fail");
        assert!(matches!(error, TraceDecayError::Config { .. }));
    }

    #[test]
    fn production_client_is_the_semantic_configuration_source() {
        fn assert_source<T: SemanticConfigurationSnapshotSourceV1>() {}
        assert_source::<ProductionConfigurationDaemonClient>();
    }

    #[test]
    fn core_registry_owns_atomic_semantic_configuration() {
        let registry =
            crate::config::registry::ConfigurationRegistry::core().expect("core registry");
        let key = SettingKey::new(SEMANTIC_RUNTIME_SETTING_KEY).unwrap();
        let definition = registry.definition(&key).unwrap();
        assert_eq!(definition.value_kind, ConfigurationValueKindV1::Text);
        registry
            .validate_value(
                &key,
                &ConfigurationValueV1::Text(
                    serde_json::to_string(&SemanticConfig::default()).unwrap(),
                ),
            )
            .unwrap();
    }
}
