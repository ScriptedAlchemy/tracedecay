//! Retained daemon composition for the configuration control plane.
//!
//! This module owns only lifetime and delegation. Resolution, validation,
//! authorization, mutation, audit, and credential semantics remain in the
//! existing application operations and transactional store.

use std::any::Any;
use std::sync::{Arc, OnceLock};

use tracedecay_application::now_micros;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1,
    CredentialReferenceMetadataV1, ProtectedApplyRequest, ProtectedChange, ProtectedChangePlan,
    SettingKey,
};

use crate::config::{
    OpenedRuntimeConfiguration, PinnedRuntimeConfiguration, RuntimeConfigurationTarget,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore;

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
/// transactional store handle and the one application operation facade used by every
/// local transport.
pub struct ProjectConfigurationRuntime {
    target: RuntimeConfigurationTarget,
    configuration_database: RegisteredGlobalDbLeaseV1,
    authorities: Arc<ConfigurationAuthoritySlots>,
    client: Arc<ProductionConfigurationDaemonClient>,
    semantic_activation: OnceLock<Arc<dyn Any + Send + Sync>>,
    semantic_inventory: OnceLock<Arc<dyn Any + Send + Sync>>,
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
                semantic_activation: OnceLock::new(),
                semantic_inventory: OnceLock::new(),
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

    /// First-wins type-erased semantic activation payload. Callers in
    /// `tracedecay-usecases` downcast to the production coordinator.
    pub fn install_semantic_activation<T: Send + Sync + 'static>(&self, value: Arc<T>) {
        let _ = self
            .semantic_activation
            .set(value as Arc<dyn Any + Send + Sync>);
    }

    pub fn semantic_activation<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.semantic_activation
            .get()
            .and_then(|value| Arc::clone(value).downcast::<T>().ok())
    }

    /// First-wins type-erased inventory payload. Callers downcast to the
    /// production retrieval configuration store.
    pub fn install_semantic_inventory<T: Send + Sync + 'static>(&self, value: T) {
        let _ = self
            .semantic_inventory
            .set(Arc::new(value) as Arc<dyn Any + Send + Sync>);
    }

    pub fn semantic_inventory<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.semantic_inventory
            .get()
            .and_then(|value| value.downcast_ref::<T>().cloned())
    }

    pub fn installed_mutation_authorization(
        &self,
    ) -> std::result::Result<
        &Arc<dyn ConfigurationMutationAuthorizationPort + Send + Sync>,
        ConfigurationError,
    > {
        self.authorities.installed_mutation_authorization()
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
            DirectConfigurationMutation::Set {
                layer,
                key,
                value: Box::new(value),
            },
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
        Box::pin(hotpath::future!(
            async move {
                let current = super::ports::ConfigurationControlStore::current(&store).await?;
                PinnedRuntimeConfiguration::new(target, current.revision_id, current.snapshot)
                    .map_err(|_| ConfigurationError::Unavailable)
            },
            label = "usecases.configuration.current"
        ))
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
        now_micros()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::configuration::ConfigurationValueKindV1;
    use tracedecay_semantic_contracts::SemanticConfig;

    use crate::config::SEMANTIC_RUNTIME_SETTING_KEY;

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
