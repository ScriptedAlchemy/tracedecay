//! Application orchestration for revisioned configuration operations.

use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, ChangePlanId, ConfigurationRevisionId, ConfigurationValueV1,
    ProtectedApplyRequest, ProtectedChange, ProtectedChangePlan, SOURCE_BINDINGS_SETTING_KEY,
    SettingKey, WORK_TOPOLOGY_POLICY_SETTING_KEY,
};
use tracedecay_domain::{UtcMicros, canonical_sha256};

use crate::config::registry::ConfigurationRegistry;
use crate::config::scope_control::{
    ProtectedChangePlanDraftV1, plan_protected_change, validate_apply_binding,
};

use super::ports::{
    ConfigurationClock, ConfigurationControlStore, CredentialWritePort, ScopeResolutionPort,
    ScopeRevalidationEvidenceV1,
};
use super::types::{
    AuthorizedActor, ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationError, ConfigurationMutationReceipt, ConfigurationRollbackRequest,
    DirectConfigurationMutation, ResolvedSetting, SettingSummary, WriteOnlyCredentialMutation,
};

/// One transport-neutral control-plane contract. CLI, MCP, HTTP, dashboard,
/// and Doctor call this shape rather than rebuilding mutation semantics.
pub trait ConfigurationControlPlane {
    fn list(&self, actor: AuthorizedActor) -> Result<Vec<SettingSummary>, ConfigurationError>;

    fn explain(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> Result<ResolvedSetting, ConfigurationError>;

    fn get(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> Result<ResolvedSetting, ConfigurationError>;

    fn mutate_direct(
        &self,
        actor: AuthorizedActor,
        mutation: DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError>;

    fn write_credential(
        &self,
        actor: AuthorizedActor,
        write: WriteOnlyCredentialMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> Result<tracedecay_domain::configuration::CredentialReferenceMetadataV1, ConfigurationError>;

    fn observed_state(
        &self,
        actor: AuthorizedActor,
    ) -> Result<Vec<ComponentConfigurationState>, ConfigurationError>;

    fn dry_run_protected_change(
        &self,
        actor: AuthorizedActor,
        change: ProtectedChange,
        expected_revision: ConfigurationRevisionId,
    ) -> Result<ProtectedChangePlan, ConfigurationError>;

    fn apply_protected_change(
        &self,
        actor: AuthorizedActor,
        request: ProtectedApplyRequest,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError>;

    fn dry_run_rollback(
        &self,
        actor: AuthorizedActor,
        rollback: ConfigurationRollbackRequest,
    ) -> Result<ProtectedChangePlan, ConfigurationError>;

    fn apply_rollback(
        &self,
        actor: AuthorizedActor,
        request: ProtectedApplyRequest,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError>;

    fn audit(
        &self,
        actor: AuthorizedActor,
        query: ConfigurationAuditQuery,
    ) -> Result<ConfigurationAuditPage, ConfigurationError>;
}

pub struct ConfigurationControlPlaneOperations<'a, Store, Scopes, Credentials, Clock> {
    registry: &'a ConfigurationRegistry,
    store: &'a Store,
    scopes: &'a Scopes,
    credentials: &'a Credentials,
    clock: &'a Clock,
}

impl<'a, Store, Scopes, Credentials, Clock>
    ConfigurationControlPlaneOperations<'a, Store, Scopes, Credentials, Clock>
{
    pub fn new(
        registry: &'a ConfigurationRegistry,
        store: &'a Store,
        scopes: &'a Scopes,
        credentials: &'a Credentials,
        clock: &'a Clock,
    ) -> Self {
        Self {
            registry,
            store,
            scopes,
            credentials,
            clock,
        }
    }
}

impl<Store, Scopes, Credentials, Clock> ConfigurationControlPlane
    for ConfigurationControlPlaneOperations<'_, Store, Scopes, Credentials, Clock>
where
    Store: ConfigurationControlStore,
    Scopes: ScopeResolutionPort,
    Credentials: CredentialWritePort,
    Clock: ConfigurationClock,
{
    fn list(&self, actor: AuthorizedActor) -> Result<Vec<SettingSummary>, ConfigurationError> {
        actor.validate()?;
        Ok(self
            .registry
            .definitions()
            .map(|definition| SettingSummary {
                key: definition.key.clone(),
                sensitivity: definition.sensitivity,
                restart_requirement: definition.restart_requirement,
            })
            .collect())
    }

    fn explain(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> Result<ResolvedSetting, ConfigurationError> {
        self.get(actor, key)
    }

    fn get(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> Result<ResolvedSetting, ConfigurationError> {
        actor.validate()?;
        self.registry
            .definition(&key)
            .map_err(ConfigurationError::validation)?;
        let current = self.store.current()?;
        current
            .snapshot
            .validate()
            .map_err(ConfigurationError::validation)?;
        let effective_value = current
            .snapshot
            .effective_values
            .get(&key)
            .cloned()
            .ok_or(ConfigurationError::TargetUnavailable)?;
        Ok(ResolvedSetting {
            key: key.clone(),
            effective_value,
            snapshot_id: current.snapshot.snapshot_id,
            effective_behavior_digest: current.snapshot.effective_behavior_digest,
            resolution_provenance_digest: current.snapshot.resolution_provenance_digest,
            candidates: current
                .snapshot
                .provenance
                .get(&key)
                .cloned()
                .unwrap_or_default(),
        })
    }

    fn mutate_direct(
        &self,
        actor: AuthorizedActor,
        mutation: DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError> {
        actor.validate()?;
        expected_revision
            .validate()
            .map_err(ConfigurationError::validation)?;
        validate_direct_mutation(self.registry, &mutation)?;
        let current = self.store.current()?;
        if current.revision_id != expected_revision {
            return Err(ConfigurationError::RevisionConflict);
        }
        self.store
            .commit_direct(&actor, &mutation, &expected_revision)
    }

    fn write_credential(
        &self,
        actor: AuthorizedActor,
        write: WriteOnlyCredentialMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> Result<tracedecay_domain::configuration::CredentialReferenceMetadataV1, ConfigurationError>
    {
        actor.validate()?;
        expected_revision
            .validate()
            .map_err(ConfigurationError::validation)?;
        let current = self.store.current()?;
        if current.revision_id != expected_revision {
            return Err(ConfigurationError::RevisionConflict);
        }
        self.credentials
            .write_reference(&actor, &write, &expected_revision)
    }

    fn observed_state(
        &self,
        actor: AuthorizedActor,
    ) -> Result<Vec<ComponentConfigurationState>, ConfigurationError> {
        actor.validate()?;
        self.store.observed_state(&actor)
    }

    fn dry_run_protected_change(
        &self,
        actor: AuthorizedActor,
        change: ProtectedChange,
        expected_revision: ConfigurationRevisionId,
    ) -> Result<ProtectedChangePlan, ConfigurationError> {
        actor.validate()?;
        expected_revision
            .validate()
            .map_err(ConfigurationError::validation)?;
        change.validate().map_err(ConfigurationError::validation)?;
        let current = self.store.current()?;
        if current.revision_id != expected_revision {
            return Err(ConfigurationError::RevisionConflict);
        }
        let evidence = self.scopes.resolve_protected_change(&actor, &change)?;
        let now = self.clock.now();
        let operation_digest = change
            .compute_digest()
            .map_err(ConfigurationError::validation)?;
        let plan_id = derive_plan_id(
            &actor,
            &current.revision_id,
            &operation_digest,
            &evidence,
            now,
        )?;
        let plan = plan_protected_change(
            ProtectedChangePlanDraftV1 {
                plan_id,
                actor_id: actor.actor_id.clone(),
                base_revision_id: current.revision_id,
                resolved_scope_digest: evidence.resolved_scope_digest,
                membership_digest: evidence.membership_digest,
                authorization_policy_digest: evidence.authorization_policy_digest,
                policy_epoch: evidence.policy_epoch,
                created_at: now,
                expires_at: UtcMicros(now.0.saturating_add(300_000_000)),
                before_digest: Some(current.snapshot.effective_behavior_digest),
                after_digest: Some(operation_digest),
            },
            change,
        )
        .map_err(ConfigurationError::validation)?;
        self.store.save_plan(&plan)?;
        Ok(plan)
    }

    fn apply_protected_change(
        &self,
        actor: AuthorizedActor,
        request: ProtectedApplyRequest,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError> {
        actor.validate()?;
        let plan = self
            .store
            .load_plan(&request.plan_id)?
            .ok_or(ConfigurationError::PlanStale)?;
        self.apply_plan(&actor, &request, &plan, false)
    }

    fn dry_run_rollback(
        &self,
        actor: AuthorizedActor,
        rollback: ConfigurationRollbackRequest,
    ) -> Result<ProtectedChangePlan, ConfigurationError> {
        actor.validate()?;
        rollback
            .target_revision_id
            .validate()
            .map_err(ConfigurationError::validation)?;
        self.store.dry_run_rollback(&actor, &rollback)
    }

    fn apply_rollback(
        &self,
        actor: AuthorizedActor,
        request: ProtectedApplyRequest,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError> {
        actor.validate()?;
        let plan = self
            .store
            .load_plan(&request.plan_id)?
            .ok_or(ConfigurationError::PlanStale)?;
        self.apply_plan(&actor, &request, &plan, true)
    }

    fn audit(
        &self,
        actor: AuthorizedActor,
        query: ConfigurationAuditQuery,
    ) -> Result<ConfigurationAuditPage, ConfigurationError> {
        actor.validate()?;
        if query.limit == 0 {
            return Err(ConfigurationError::validation_message(
                "configuration audit limit must be non-zero",
            ));
        }
        self.store.audit(&actor, &query)
    }
}

impl<Store, Scopes, Credentials, Clock>
    ConfigurationControlPlaneOperations<'_, Store, Scopes, Credentials, Clock>
where
    Store: ConfigurationControlStore,
    Scopes: ScopeResolutionPort,
    Credentials: CredentialWritePort,
    Clock: ConfigurationClock,
{
    fn apply_plan(
        &self,
        actor: &AuthorizedActor,
        request: &ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        rollback: bool,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError> {
        let now = self.clock.now();
        if plan.is_expired_at(now) {
            return Err(ConfigurationError::PlanExpired);
        }
        validate_apply_binding(plan, request, now).map_err(|_| ConfigurationError::PlanStale)?;
        let current = self.store.current()?;
        if current.revision_id != plan.base_revision_id {
            return Err(ConfigurationError::PlanStale);
        }
        let evidence = self.scopes.revalidate_plan(actor, plan)?;
        validate_frozen_evidence(plan, &evidence)?;
        if rollback {
            self.store.apply_rollback(actor, request, plan, &evidence)
        } else {
            self.store.commit_protected(actor, request, plan, &evidence)
        }
    }
}

fn validate_direct_mutation(
    registry: &ConfigurationRegistry,
    mutation: &DirectConfigurationMutation,
) -> Result<(), ConfigurationError> {
    match mutation {
        DirectConfigurationMutation::Set { key, value } => {
            reject_protected_key(key)?;
            if matches!(value, ConfigurationValueV1::CredentialReference(_)) {
                return Err(ConfigurationError::validation_message(
                    "credential references require the write-only credential operation",
                ));
            }
            registry
                .validate_value(key, value)
                .map_err(ConfigurationError::validation)
        }
        DirectConfigurationMutation::Unset { key } => {
            reject_protected_key(key)?;
            registry
                .definition(key)
                .map(|_| ())
                .map_err(ConfigurationError::validation)
        }
        DirectConfigurationMutation::Batch { mutations } => {
            mutation.touched_keys()?;
            for mutation in mutations {
                validate_direct_mutation(registry, mutation)?;
            }
            Ok(())
        }
    }
}

fn reject_protected_key(key: &SettingKey) -> Result<(), ConfigurationError> {
    if [
        SOURCE_BINDINGS_SETTING_KEY,
        ACCESS_RULES_SETTING_KEY,
        WORK_TOPOLOGY_POLICY_SETTING_KEY,
    ]
    .contains(&key.as_str())
    {
        return Err(ConfigurationError::PolicyWideningForbidden);
    }
    Ok(())
}

fn validate_frozen_evidence(
    plan: &ProtectedChangePlan,
    evidence: &ScopeRevalidationEvidenceV1,
) -> Result<(), ConfigurationError> {
    if plan.resolved_scope_digest != evidence.resolved_scope_digest
        || plan.membership_digest != evidence.membership_digest
        || plan.authorization_policy_digest != evidence.authorization_policy_digest
        || plan.policy_epoch != evidence.policy_epoch
    {
        return Err(ConfigurationError::PlanStale);
    }
    Ok(())
}

fn derive_plan_id(
    actor: &AuthorizedActor,
    base_revision_id: &ConfigurationRevisionId,
    operation_digest: &tracedecay_domain::ManifestDigest,
    evidence: &ScopeRevalidationEvidenceV1,
    created_at: UtcMicros,
) -> Result<ChangePlanId, ConfigurationError> {
    let digest = canonical_sha256(&(
        "tracedecay.configuration.change-plan.v1",
        &actor.actor_id,
        base_revision_id,
        operation_digest,
        &evidence.resolved_scope_digest,
        &evidence.membership_digest,
        &evidence.authorization_policy_digest,
        evidence.policy_epoch,
        created_at,
    ))
    .map_err(ConfigurationError::validation)?;
    let encoded = digest.as_str().strip_prefix("sha256:").ok_or_else(|| {
        ConfigurationError::validation_message("configuration plan digest missing prefix")
    })?;
    ChangePlanId::new(format!("configuration.plan.v1.{encoded}"))
        .map_err(ConfigurationError::validation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::registry::ConfigurationRegistry;
    use tracedecay_domain::configuration::{AnalyzerSettingsV1, ConfigurationValueV1, SettingKey};

    #[test]
    fn direct_mutation_rejects_protected_control_settings() {
        let registry = ConfigurationRegistry::core().unwrap();
        let result = validate_direct_mutation(
            &registry,
            &DirectConfigurationMutation::Set {
                key: SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap(),
                value: ConfigurationValueV1::SourceBindings(Vec::new()),
            },
        );
        assert_eq!(result, Err(ConfigurationError::PolicyWideningForbidden));
    }

    #[test]
    fn direct_mutation_accepts_typed_analyzer_values() {
        let registry = ConfigurationRegistry::core().unwrap();
        let result = validate_direct_mutation(
            &registry,
            &DirectConfigurationMutation::Set {
                key: SettingKey::new("analyzer.settings.v1").unwrap(),
                value: ConfigurationValueV1::AnalyzerSettings(AnalyzerSettingsV1::empty()),
            },
        );
        assert!(result.is_ok());
    }
}
