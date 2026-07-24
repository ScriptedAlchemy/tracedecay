//! Retained daemon composition for the configuration control plane.
//!
//! This module owns only lifetime and delegation. Resolution, validation,
//! authorization, mutation, audit, and credential semantics remain in the
//! existing application operations and Plan20 store.

use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueKindV1,
    ConfigurationValueV1, CredentialReferenceMetadataV1, DeprecationStateV1, ProtectedApplyRequest,
    ProtectedChange, ProtectedChangePlan, RestartRequirementV1, SettingDefinitionV1, SettingKey,
    SettingScopeV1, SettingSensitivityV1,
};

use crate::application::semantic_runtime::SemanticConfigurationSnapshotSourceV1;
use crate::config::{
    ConfigurationDaemonClient, OpenedRuntimeConfiguration, PinnedRuntimeConfiguration,
    RuntimeConfigurationFuture, RuntimeConfigurationTarget, SEMANTIC_RUNTIME_SETTING_KEY,
    SemanticConfig,
};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::global_db::configuration::OwnedGlobalDbConfigurationControlStore;

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

type SharedConfigurationControlPlane = Arc<dyn ConfigurationControlPlane + Send + Sync>;

/// Retained project-level control-plane runtime. It owns the one opened
/// Plan20 store handle and the one application operation facade used by every
/// local transport.
pub struct ProjectConfigurationRuntime {
    configuration: PinnedRuntimeConfiguration,
    configuration_database: Arc<RegisteredGlobalDb>,
    authorities: Arc<ConfigurationAuthoritySlots>,
    #[allow(dead_code)] // Plan 20 config control-plane — staged
    control_plane: SharedConfigurationControlPlane,
    client: Arc<ProductionConfigurationDaemonClient>,
}

impl ProjectConfigurationRuntime {
    pub(crate) fn open(opened: OpenedRuntimeConfiguration) -> Result<Self> {
        let OpenedRuntimeConfiguration {
            configuration,
            registered_database,
        } = opened;
        let mut registry =
            crate::config::registry::ConfigurationRegistry::core().map_err(|error| {
                TraceDecayError::Config {
                    message: format!("configuration registry unavailable: {error}"),
                }
            })?;
        register_semantic_runtime_configuration(&mut registry)?;
        let registry = Arc::new(registry);
        let store = OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
            Arc::clone(&registered_database),
        );
        let authorities = Arc::new(ConfigurationAuthoritySlots::default());
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
        Ok(Self {
            configuration,
            configuration_database: registered_database,
            authorities,
            control_plane,
            client,
        })
    }

    pub(crate) fn configuration(&self) -> &PinnedRuntimeConfiguration {
        &self.configuration
    }

    pub(crate) fn registered_database(&self) -> Arc<RegisteredGlobalDb> {
        Arc::clone(&self.configuration_database)
    }

    pub(crate) fn install_authorities(
        &self,
        scopes: Arc<dyn ScopeResolutionPort + Send + Sync>,
        authorization: Arc<dyn ConfigurationMutationAuthorizationPort + Send + Sync>,
    ) -> Result<()> {
        self.authorities.install(scopes, authorization)
    }

    #[allow(dead_code)] // Plan 20 config control-plane — staged
    pub(crate) fn control_plane(&self) -> SharedConfigurationControlPlane {
        Arc::clone(&self.control_plane)
    }

    pub(crate) fn client(&self) -> Arc<ProductionConfigurationDaemonClient> {
        Arc::clone(&self.client)
    }

    pub(crate) fn dyn_client(&self) -> Arc<dyn crate::config::ConfigurationDaemonClient> {
        Arc::clone(&self.client) as Arc<dyn crate::config::ConfigurationDaemonClient>
    }
}

// Release this runtime's process-global daemon-client registration when the
// last handle drops. The uninstall is `Arc::ptr_eq`-guarded, so a newer client
// installed by a live handle for the same project is never removed.
impl Drop for ProjectConfigurationRuntime {
    fn drop(&mut self) {
        crate::config::uninstall_configuration_daemon_client_for_project(
            &self.configuration.target,
            &self.dyn_client(),
        );
    }
}

fn register_semantic_runtime_configuration(
    registry: &mut crate::config::registry::ConfigurationRegistry,
) -> Result<()> {
    let key =
        SettingKey::new(SEMANTIC_RUNTIME_SETTING_KEY).map_err(|error| TraceDecayError::Config {
            message: format!("semantic runtime setting key is invalid: {error}"),
        })?;
    if registry.definition(&key).is_ok() {
        return Ok(());
    }
    let default = SemanticConfig::default();
    default.validate()?;
    let default = serde_json::to_string(&default).map_err(|error| TraceDecayError::Config {
        message: format!("semantic runtime default is invalid: {error}"),
    })?;
    registry
        .register(SettingDefinitionV1 {
            key,
            schema_revision: crate::config::registry::CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::Text,
            default_value: ConfigurationValueV1::Text(default),
            sensitivity: SettingSensitivityV1::Sensitive,
            scope: SettingScopeV1::Project,
            restart_requirement: RestartRequirementV1::AnalyzerRestart,
            deprecation: DeprecationStateV1::Active,
        })
        .map_err(|error| TraceDecayError::Config {
            message: format!("semantic runtime setting registration failed: {error}"),
        })
}

/// Production daemon client for the retained project configuration runtime.
///
/// Reads and caller-authorized mutations share the same retained application
/// operations. The legacy runtime-diff seam intentionally has no synthetic
/// mutation grant; it fails closed until the daemon has an authenticated
/// authority to pass to the typed mutation operation.
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

impl ConfigurationDaemonClient for ProductionConfigurationDaemonClient {
    fn mutate_direct(
        &self,
        target: RuntimeConfigurationTarget,
        _mutation: DirectConfigurationMutation,
        _expected_revision: ConfigurationRevisionId,
    ) -> RuntimeConfigurationFuture<'_, PinnedRuntimeConfiguration> {
        let expected_project = self.target.project_id.clone();
        Box::pin(async move {
            if target.project_id != expected_project {
                return Err(TraceDecayError::Config {
                    message: "configuration daemon target does not match the retained project"
                        .to_owned(),
                });
            }
            Err(TraceDecayError::Config {
                message: "configuration mutation authority unavailable: runtime diff requests require an authenticated configuration grant"
                    .to_owned(),
            })
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

#[derive(Default)]
struct ConfigurationAuthoritySlots {
    installed: OnceLock<InstalledConfigurationAuthorities>,
}

impl ConfigurationAuthoritySlots {
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

    fn mutation_authorization(
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
        let Ok(authorization) = self.0.mutation_authorization() else {
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
    use crate::application::semantic_runtime::SemanticConfigurationSnapshotSourceV1;

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
        let authorities = ConfigurationAuthoritySlots::default();

        assert!(matches!(
            authorities.scope_resolution(),
            Err(ConfigurationError::Unavailable)
        ));
        assert!(matches!(
            authorities.mutation_authorization(),
            Err(ConfigurationError::Unavailable)
        ));
    }

    #[test]
    fn configuration_authorities_bind_atomically_once() {
        let authorities = ConfigurationAuthoritySlots::default();
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
                .mutation_authorization()
                .expect("installed authorization"),
            &authorization
        ));

        let error = authorities
            .install(scopes, authorization)
            .expect_err("second authority installation must fail");
        assert!(matches!(error, TraceDecayError::Config { .. }));
    }

    #[test]
    fn client_exposes_typed_direct_mutation_operations() {
        let _ = ProductionConfigurationDaemonClient::set;
        let _ = ProductionConfigurationDaemonClient::unset;
        let _ = ProductionConfigurationDaemonClient::batch;
    }

    #[test]
    fn production_client_is_the_semantic_configuration_source() {
        fn assert_source<T: SemanticConfigurationSnapshotSourceV1>() {}
        assert_source::<ProductionConfigurationDaemonClient>();
    }

    #[test]
    fn production_registry_accepts_atomic_semantic_configuration() {
        let mut registry =
            crate::config::registry::ConfigurationRegistry::core().expect("core registry");
        register_semantic_runtime_configuration(&mut registry).expect("semantic definition");
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
