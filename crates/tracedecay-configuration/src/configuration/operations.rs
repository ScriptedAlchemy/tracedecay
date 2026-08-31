//! Application orchestration for revisioned configuration operations.

use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, ChangePlanId, ConfigurationMutationEffectV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationRevisionId,
    ConfigurationValueV1, ProtectedApplyRequest, ProtectedChange, ProtectedChangePlan,
    RollbackModeV1, SOURCE_BINDINGS_SETTING_KEY, SettingKey, WORK_TOPOLOGY_POLICY_SETTING_KEY,
};
use tracedecay_domain::{UtcMicros, canonical_sha256};

use crate::config::registry::ConfigurationRegistry;
use crate::config::scope_control::{
    ProtectedChangePlanDraftV1, plan_protected_change, validate_apply_binding,
};

use super::ports::{
    ConfigurationClock, ConfigurationControlStore, ConfigurationMutationAuthorizationPort,
    ConfigurationOperationFuture, CredentialWritePort, ScopeResolutionPort,
    ScopeRevalidationEvidenceV1,
};
use super::types::{
    AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT, ComponentConfigurationState,
    ConfigurationAuditPage, ConfigurationAuditQuery, ConfigurationError,
    ConfigurationMutationAuthority, ConfigurationMutationReceipt, ConfigurationRollbackRequest,
    DirectConfigurationMutation, ResolvedSetting, SettingSummary, WriteOnlyCredentialMutation,
};

/// One transport-neutral control-plane contract. CLI, MCP, HTTP, dashboard,
/// and Doctor call this shape rather than rebuilding mutation semantics.
pub trait ConfigurationControlPlane: Sync {
    fn list(&self, actor: AuthorizedActor)
    -> ConfigurationOperationFuture<'_, Vec<SettingSummary>>;

    fn explain(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> ConfigurationOperationFuture<'_, ResolvedSetting>;

    fn get(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> ConfigurationOperationFuture<'_, ResolvedSetting>;

    fn mutate_direct(
        &self,
        authority: ConfigurationMutationAuthority,
        mutation: DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt>;

    fn write_credential(
        &self,
        authority: ConfigurationMutationAuthority,
        write: WriteOnlyCredentialMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<
        '_,
        tracedecay_domain::configuration::CredentialReferenceMetadataV1,
    >;

    fn observed_state(
        &self,
        actor: AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>>;

    fn dry_run_protected_change(
        &self,
        authority: ConfigurationMutationAuthority,
        change: ProtectedChange,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan>;

    fn apply_protected_change(
        &self,
        authority: ConfigurationMutationAuthority,
        request: ProtectedApplyRequest,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt>;

    fn dry_run_rollback(
        &self,
        authority: ConfigurationMutationAuthority,
        rollback: ConfigurationRollbackRequest,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan>;

    fn apply_rollback(
        &self,
        authority: ConfigurationMutationAuthority,
        request: ProtectedApplyRequest,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt>;

    fn audit(
        &self,
        actor: AuthorizedActor,
        query: ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage>;
}

pub struct ConfigurationControlPlaneOperations<'a, Store, Scopes, Credentials, Authorization, Clock>
{
    registry: &'a ConfigurationRegistry,
    store: &'a Store,
    scopes: &'a Scopes,
    credentials: &'a Credentials,
    authorization: &'a Authorization,
    clock: &'a Clock,
}

impl<'a, Store, Scopes, Credentials, Authorization, Clock>
    ConfigurationControlPlaneOperations<'a, Store, Scopes, Credentials, Authorization, Clock>
{
    pub fn new(
        registry: &'a ConfigurationRegistry,
        store: &'a Store,
        scopes: &'a Scopes,
        credentials: &'a Credentials,
        authorization: &'a Authorization,
        clock: &'a Clock,
    ) -> Self {
        Self {
            registry,
            store,
            scopes,
            credentials,
            authorization,
            clock,
        }
    }
}

impl<Store, Scopes, Credentials, Authorization, Clock> ConfigurationControlPlane
    for ConfigurationControlPlaneOperations<'_, Store, Scopes, Credentials, Authorization, Clock>
where
    Store: ConfigurationControlStore,
    Scopes: ScopeResolutionPort,
    Credentials: CredentialWritePort,
    Authorization: ConfigurationMutationAuthorizationPort,
    Clock: ConfigurationClock,
{
    fn list(
        &self,
        actor: AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<SettingSummary>> {
        Box::pin(async move {
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
        })
    }

    fn explain(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> ConfigurationOperationFuture<'_, ResolvedSetting> {
        Box::pin(async move { self.get(actor, key).await })
    }

    fn get(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> ConfigurationOperationFuture<'_, ResolvedSetting> {
        Box::pin(async move {
            actor.validate()?;
            self.registry
                .definition(&key)
                .map_err(ConfigurationError::validation)?;
            let current = self.store.current().await?;
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
        })
    }

    fn mutate_direct(
        &self,
        authority: ConfigurationMutationAuthority,
        mutation: DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async move {
            expected_revision
                .validate()
                .map_err(ConfigurationError::validation)?;
            validate_direct_mutation(self.registry, &mutation)?;
            if authority.receipt.scope_digest != mutation.target_scope_digest()? {
                return Err(ConfigurationError::MutationAuthorityRejected);
            }
            // The transactional store checks exact replay before its CAS. A
            // preflight current-revision read here would turn a successful
            // retry into `revision_conflict` after the first commit.
            self.authorize_mutation(
                &authority,
                ConfigurationMutationOperationV1::DirectMutation,
                &expected_revision,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
            )
            .await?;
            self.store
                .commit_direct(&authority, &mutation, &expected_revision)
                .await
        })
    }

    fn write_credential(
        &self,
        authority: ConfigurationMutationAuthority,
        write: WriteOnlyCredentialMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<
        '_,
        tracedecay_domain::configuration::CredentialReferenceMetadataV1,
    > {
        Box::pin(async move {
            expected_revision
                .validate()
                .map_err(ConfigurationError::validation)?;
            // The credential store checks exact replay before its revision
            // CAS, matching direct configuration mutation semantics.
            self.authorize_mutation(
                &authority,
                ConfigurationMutationOperationV1::CredentialWrite,
                &expected_revision,
                ConfigurationMutationSinkV1::CredentialStore,
                ConfigurationMutationEffectV1::WriteCredentialReference,
            )
            .await?;
            self.credentials
                .write_reference(&authority, &write, &expected_revision)
                .await
        })
    }

    fn observed_state(
        &self,
        actor: AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>> {
        Box::pin(async move {
            actor.validate()?;
            self.store.observed_state(&actor).await
        })
    }

    fn dry_run_protected_change(
        &self,
        authority: ConfigurationMutationAuthority,
        change: ProtectedChange,
        expected_revision: ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        Box::pin(async move {
            expected_revision
                .validate()
                .map_err(ConfigurationError::validation)?;
            change.validate().map_err(ConfigurationError::validation)?;
            let current = self.store.current().await?;
            if current.revision_id != expected_revision {
                return Err(ConfigurationError::RevisionConflict);
            }
            let current_authorization = self
                .authorize_mutation(
                    &authority,
                    ConfigurationMutationOperationV1::ProtectedDryRun,
                    &expected_revision,
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CreateProtectedChangePlan,
                )
                .await?;
            let actor = authority.actor();
            let evidence = self
                .scopes
                .resolve_protected_change(&actor, &change)
                .await?;
            validate_authorization_evidence(&current_authorization, &evidence)?;
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
                change.clone(),
            )
            .map_err(ConfigurationError::validation)?;
            self.store.save_plan(&plan, &change).await?;
            Ok(plan)
        })
    }

    fn apply_protected_change(
        &self,
        authority: ConfigurationMutationAuthority,
        request: ProtectedApplyRequest,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async move {
            self.authorize_mutation(
                &authority,
                ConfigurationMutationOperationV1::ProtectedApply,
                &request.expected_base_revision_id,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
            )
            .await?;
            if request.actor_id != authority.receipt.actor_id {
                return Err(ConfigurationError::MutationAuthorityRejected);
            }
            if let Some(receipt) = self
                .store
                .replay_apply(
                    &authority,
                    &request,
                    ConfigurationMutationOperationV1::ProtectedApply,
                )
                .await?
            {
                return Ok(receipt);
            }
            let plan = self
                .store
                .load_plan(&request.plan_id)
                .await?
                .ok_or(ConfigurationError::PlanStale)?;
            self.apply_plan(authority, request, plan, false).await
        })
    }

    fn dry_run_rollback(
        &self,
        authority: ConfigurationMutationAuthority,
        rollback: ConfigurationRollbackRequest,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        Box::pin(async move {
            if rollback.mode == RollbackModeV1::Partial {
                return Err(ConfigurationError::Unavailable);
            }
            rollback
                .target_revision_id
                .validate()
                .map_err(ConfigurationError::validation)?;
            let expected_revision = authority.receipt.expected_configuration_revision.clone();
            let current = self.store.current().await?;
            if current.revision_id != expected_revision {
                return Err(ConfigurationError::RevisionConflict);
            }
            self.authorize_mutation(
                &authority,
                ConfigurationMutationOperationV1::RollbackDryRun,
                &expected_revision,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CreateProtectedChangePlan,
            )
            .await?;
            self.store
                .dry_run_rollback(&authority, &rollback, self.clock.now())
                .await
        })
    }

    fn apply_rollback(
        &self,
        authority: ConfigurationMutationAuthority,
        request: ProtectedApplyRequest,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async move {
            self.authorize_mutation(
                &authority,
                ConfigurationMutationOperationV1::RollbackApply,
                &request.expected_base_revision_id,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
            )
            .await?;
            if request.actor_id != authority.receipt.actor_id {
                return Err(ConfigurationError::MutationAuthorityRejected);
            }
            if let Some(receipt) = self
                .store
                .replay_apply(
                    &authority,
                    &request,
                    ConfigurationMutationOperationV1::RollbackApply,
                )
                .await?
            {
                return Ok(receipt);
            }
            let plan = self
                .store
                .load_plan(&request.plan_id)
                .await?
                .ok_or(ConfigurationError::PlanStale)?;
            self.apply_plan(authority, request, plan, true).await
        })
    }

    fn audit(
        &self,
        actor: AuthorizedActor,
        query: ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage> {
        Box::pin(async move {
            actor.validate()?;
            if query.limit == 0 || query.limit > CONFIGURATION_AUDIT_PAGE_LIMIT {
                return Err(ConfigurationError::validation_message(
                    "configuration audit limit must be between 1 and 1000",
                ));
            }
            self.store.audit(&actor, &query).await
        })
    }
}

impl<Store, Scopes, Credentials, Authorization, Clock>
    ConfigurationControlPlaneOperations<'_, Store, Scopes, Credentials, Authorization, Clock>
where
    Store: ConfigurationControlStore,
    Scopes: ScopeResolutionPort,
    Credentials: CredentialWritePort,
    Authorization: ConfigurationMutationAuthorizationPort,
    Clock: ConfigurationClock,
{
    fn apply_plan(
        &self,
        authority: ConfigurationMutationAuthority,
        request: ProtectedApplyRequest,
        plan: ProtectedChangePlan,
        rollback: bool,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        Box::pin(async move {
            let operation = if rollback {
                ConfigurationMutationOperationV1::RollbackApply
            } else {
                ConfigurationMutationOperationV1::ProtectedApply
            };
            let current_authorization = self
                .authorize_mutation(
                    &authority,
                    operation,
                    &request.expected_base_revision_id,
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CommitConfigurationRevision,
                )
                .await?;
            let actor = authority.actor();
            if request.actor_id != actor.actor_id {
                return Err(ConfigurationError::MutationAuthorityRejected);
            }
            if let Some(receipt) = self
                .store
                .replay_apply(&authority, &request, operation)
                .await?
            {
                return Ok(receipt);
            }
            let now = self.clock.now();
            if plan.is_expired_at(now) {
                return Err(ConfigurationError::PlanExpired);
            }
            validate_apply_binding(&plan, &request, now)
                .map_err(|_| ConfigurationError::PlanStale)?;
            // Do not reject against an ambient current revision before the
            // store can return an exact idempotent replay. The store performs
            // replay-first CAS in the same write transaction.
            let evidence = self.scopes.revalidate_plan(&actor, &plan).await?;
            validate_frozen_evidence(&plan, &evidence)?;
            validate_authorization_evidence(&current_authorization, &evidence)?;
            if rollback {
                self.store
                    .apply_rollback(&authority, &request, &plan, &evidence)
                    .await
            } else {
                self.store
                    .commit_protected(&authority, &request, &plan, &evidence)
                    .await
            }
        })
    }

    async fn authorize_mutation(
        &self,
        authority: &ConfigurationMutationAuthority,
        operation: ConfigurationMutationOperationV1,
        expected_revision: &ConfigurationRevisionId,
        sink: ConfigurationMutationSinkV1,
        effect: ConfigurationMutationEffectV1,
    ) -> Result<super::ports::CurrentConfigurationMutationAuthorizationV1, ConfigurationError> {
        authority.validate_integrity()?;
        let now = self.clock.now();
        let current = self
            .authorization
            .recheck(
                &authority.receipt,
                operation,
                expected_revision,
                sink,
                effect,
                now,
            )
            .await?;
        authority
            .receipt
            .validate_for(
                &authority.receipt.actor_id,
                operation,
                &current.scope_digest,
                expected_revision,
                sink,
                effect,
                now,
            )
            .map_err(|_| ConfigurationError::MutationAuthorityRejected)?;
        if authority.receipt.policy_epoch != current.policy_epoch
            || authority.receipt.policy_digest != current.policy_digest
        {
            return Err(ConfigurationError::MutationAuthorityRejected);
        }
        Ok(current)
    }
}

fn validate_direct_mutation(
    registry: &ConfigurationRegistry,
    mutation: &DirectConfigurationMutation,
) -> Result<(), ConfigurationError> {
    match mutation {
        DirectConfigurationMutation::Set { layer, key, value } => {
            reject_protected_key(key)?;
            if matches!(value.as_ref(), ConfigurationValueV1::CredentialReference(_)) {
                return Err(ConfigurationError::validation_message(
                    "credential references require the write-only credential operation",
                ));
            }
            registry
                .validate_layer(key, layer)
                .map_err(ConfigurationError::validation)?;
            registry
                .validate_value(key, value)
                .map_err(ConfigurationError::validation)
        }
        DirectConfigurationMutation::Unset { layer, key } => {
            reject_protected_key(key)?;
            registry
                .validate_layer(key, layer)
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

fn validate_authorization_evidence(
    authorization: &super::ports::CurrentConfigurationMutationAuthorizationV1,
    evidence: &ScopeRevalidationEvidenceV1,
) -> Result<(), ConfigurationError> {
    if authorization.scope_digest != evidence.resolved_scope_digest
        || authorization.policy_epoch != evidence.policy_epoch
        || authorization.policy_digest != evidence.authorization_policy_digest
    {
        return Err(ConfigurationError::MutationAuthorityRejected);
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
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::config::registry::ConfigurationRegistry;
    use crate::configuration::types::ConfigurationSettlementAuthorityV1;
    use tracedecay_domain::configuration::{
        AnalyzerSettingsV1, AuthorityRef, ConfigurationGrantId, ConfigurationGrantReceiptId,
        ConfigurationLayerIdV1, ConfigurationMutationGrantReceiptV1, ConfigurationSnapshotV1,
        ConfigurationValueV1, CredentialReferenceMetadataV1, ProtectedChange, ScopeSourceBinding,
        SettingKey, SourceBindingId, SourceKindV1,
    };
    use tracedecay_domain::{
        AccessPolicyDigest, ActorId, LocatorDigest, ManifestDigest, ProjectId,
    };

    use super::super::ports::{
        ConfigurationControlStore, ConfigurationCurrentStateV1, ConfigurationOperationFuture,
        CredentialWritePort,
    };

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn policy_digest(byte: char) -> AccessPolicyDigest {
        AccessPolicyDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    struct Store {
        current: ConfigurationCurrentStateV1,
        saved: Mutex<Option<(ProtectedChangePlan, ProtectedChange)>>,
        replay: Mutex<
            Option<(
                ProtectedApplyRequest,
                ConfigurationMutationOperationV1,
                ConfigurationMutationReceipt,
            )>,
        >,
    }

    impl ConfigurationControlStore for Store {
        fn current(&self) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1> {
            let current = self.current.clone();
            Box::pin(async move { Ok(current) })
        }

        fn save_plan(
            &self,
            plan: &ProtectedChangePlan,
            operation: &ProtectedChange,
        ) -> ConfigurationOperationFuture<'_, ()> {
            let plan = plan.clone();
            let operation = operation.clone();
            Box::pin(async move {
                *self.saved.lock().unwrap() = Some((plan, operation));
                Ok(())
            })
        }

        fn load_plan(
            &self,
            plan_id: &ChangePlanId,
        ) -> ConfigurationOperationFuture<'_, Option<ProtectedChangePlan>> {
            let plan = self
                .saved
                .lock()
                .unwrap()
                .as_ref()
                .map(|(plan, _)| plan.clone())
                .filter(|plan| &plan.plan_id == plan_id);
            Box::pin(async move { Ok(plan) })
        }

        fn replay_apply(
            &self,
            authority: &ConfigurationMutationAuthority,
            request: &ProtectedApplyRequest,
            operation: ConfigurationMutationOperationV1,
        ) -> ConfigurationOperationFuture<'_, Option<ConfigurationMutationReceipt>> {
            let replay = self.replay.lock().unwrap().clone();
            let actor_id = authority.receipt.actor_id.clone();
            let request = request.clone();
            Box::pin(async move {
                let Some((original_request, original_operation, receipt)) = replay else {
                    return Ok(None);
                };
                if original_request.actor_id != actor_id
                    || original_request.idempotency_key != request.idempotency_key
                {
                    return Ok(None);
                }
                if original_request != request || original_operation != operation {
                    return Err(ConfigurationError::IdempotencyConflict);
                }
                Ok(Some(receipt))
            })
        }

        fn commit_direct(
            &self,
            _authority: &ConfigurationMutationAuthority,
            _mutation: &DirectConfigurationMutation,
            _expected_revision: &ConfigurationRevisionId,
        ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
            Box::pin(async { Err(ConfigurationError::Unavailable) })
        }

        fn commit_protected(
            &self,
            _authority: &ConfigurationMutationAuthority,
            _request: &ProtectedApplyRequest,
            _plan: &ProtectedChangePlan,
            _evidence: &ScopeRevalidationEvidenceV1,
        ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
            Box::pin(async { Err(ConfigurationError::Unavailable) })
        }

        fn dry_run_rollback(
            &self,
            _authority: &ConfigurationMutationAuthority,
            _rollback: &ConfigurationRollbackRequest,
            _now: UtcMicros,
        ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
            Box::pin(async { Err(ConfigurationError::Unavailable) })
        }

        fn apply_rollback(
            &self,
            _authority: &ConfigurationMutationAuthority,
            _request: &ProtectedApplyRequest,
            _plan: &ProtectedChangePlan,
            _evidence: &ScopeRevalidationEvidenceV1,
        ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
            Box::pin(async { Err(ConfigurationError::Unavailable) })
        }

        fn audit(
            &self,
            _actor: &AuthorizedActor,
            _query: &ConfigurationAuditQuery,
        ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage> {
            Box::pin(async { Err(ConfigurationError::Unavailable) })
        }

        fn observed_state(
            &self,
            _actor: &AuthorizedActor,
        ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    struct Scope {
        evidence: ScopeRevalidationEvidenceV1,
    }

    impl ScopeResolutionPort for Scope {
        fn resolve_protected_change(
            &self,
            _actor: &AuthorizedActor,
            _change: &ProtectedChange,
        ) -> ConfigurationOperationFuture<'_, ScopeRevalidationEvidenceV1> {
            let evidence = self.evidence.clone();
            Box::pin(async move { Ok(evidence) })
        }

        fn revalidate_plan(
            &self,
            _actor: &AuthorizedActor,
            _plan: &ProtectedChangePlan,
        ) -> ConfigurationOperationFuture<'_, ScopeRevalidationEvidenceV1> {
            let evidence = self.evidence.clone();
            Box::pin(async move { Ok(evidence) })
        }
    }

    struct Credentials;

    impl CredentialWritePort for Credentials {
        fn write_reference(
            &self,
            _authority: &ConfigurationMutationAuthority,
            _write: &WriteOnlyCredentialMutation,
            _expected_revision: &ConfigurationRevisionId,
        ) -> ConfigurationOperationFuture<'_, CredentialReferenceMetadataV1> {
            Box::pin(async { Err(ConfigurationError::Unavailable) })
        }
    }

    struct Authorization {
        current: super::super::ports::CurrentConfigurationMutationAuthorizationV1,
    }

    impl ConfigurationMutationAuthorizationPort for Authorization {
        fn recheck(
            &self,
            _receipt: &ConfigurationMutationGrantReceiptV1,
            _operation: ConfigurationMutationOperationV1,
            _expected_revision: &ConfigurationRevisionId,
            _sink: ConfigurationMutationSinkV1,
            _effect: ConfigurationMutationEffectV1,
            _now: UtcMicros,
        ) -> ConfigurationOperationFuture<
            '_,
            super::super::ports::CurrentConfigurationMutationAuthorizationV1,
        > {
            let current = self.current.clone();
            Box::pin(async move { Ok(current) })
        }
    }

    struct Clock;

    impl ConfigurationClock for Clock {
        fn now(&self) -> UtcMicros {
            UtcMicros(10)
        }
    }

    struct AdvancedClock(UtcMicros);

    impl ConfigurationClock for AdvancedClock {
        fn now(&self) -> UtcMicros {
            self.0
        }
    }

    #[test]
    fn direct_mutation_rejects_protected_control_settings() {
        let registry = ConfigurationRegistry::core().unwrap();
        let result = validate_direct_mutation(
            &registry,
            &DirectConfigurationMutation::Set {
                layer: ConfigurationLayerIdV1::Project {
                    project_id: id::<ProjectId>("project.fixture"),
                },
                key: SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap(),
                value: Box::new(ConfigurationValueV1::SourceBindings(Vec::new())),
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
                layer: ConfigurationLayerIdV1::Project {
                    project_id: id::<ProjectId>("project.fixture"),
                },
                key: SettingKey::new("analyzer.settings.v1").unwrap(),
                value: Box::new(ConfigurationValueV1::AnalyzerSettings(
                    AnalyzerSettingsV1::empty(),
                )),
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn current_authorization_must_match_scope_and_policy_evidence() {
        let authorization = super::super::ports::CurrentConfigurationMutationAuthorizationV1 {
            grant_revision: 1,
            grant_digest: digest('c'),
            scope_digest: digest('a'),
            policy_epoch: 7,
            policy_digest: policy_digest('b'),
        };
        let evidence = ScopeRevalidationEvidenceV1 {
            resolved_scope_digest: digest('a'),
            membership_digest: None,
            authorization_policy_digest: policy_digest('b'),
            policy_epoch: 7,
        };
        assert!(validate_authorization_evidence(&authorization, &evidence).is_ok());

        let stale = ScopeRevalidationEvidenceV1 {
            policy_epoch: 8,
            ..evidence
        };
        assert_eq!(
            validate_authorization_evidence(&authorization, &stale),
            Err(ConfigurationError::MutationAuthorityRejected)
        );
    }

    #[tokio::test]
    async fn protected_dry_run_persists_the_exact_operation_beside_its_redacted_plan() {
        let revision_id: ConfigurationRevisionId = id("configuration.revision.fixture");
        let scope_digest = digest('a');
        let policy_digest = policy_digest('b');
        let evidence = ScopeRevalidationEvidenceV1 {
            resolved_scope_digest: scope_digest.clone(),
            membership_digest: None,
            authorization_policy_digest: policy_digest.clone(),
            policy_epoch: 7,
        };
        let store = Store {
            current: ConfigurationCurrentStateV1 {
                revision_id: revision_id.clone(),
                snapshot: ConfigurationSnapshotV1::new(BTreeMap::default(), BTreeMap::default())
                    .unwrap(),
            },
            saved: Mutex::new(None),
            replay: Mutex::new(None),
        };
        let authorization = Authorization {
            current: super::super::ports::CurrentConfigurationMutationAuthorizationV1 {
                grant_revision: 1,
                grant_digest: digest('c'),
                scope_digest: scope_digest.clone(),
                policy_epoch: 7,
                policy_digest: policy_digest.clone(),
            },
        };
        let authority = ConfigurationMutationAuthority {
            receipt: ConfigurationMutationGrantReceiptV1::issue(
                id::<ConfigurationGrantReceiptId>("configuration.grant-receipt.fixture"),
                id::<ConfigurationGrantId>("configuration.grant.fixture"),
                id::<ActorId>("actor.configuration.fixture"),
                ConfigurationMutationOperationV1::ProtectedDryRun,
                scope_digest,
                revision_id.clone(),
                7,
                policy_digest,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CreateProtectedChangePlan,
                None,
                UtcMicros(1),
                UtcMicros(100),
            )
            .unwrap(),
        };
        let change = ProtectedChange::BindSource(
            ScopeSourceBinding::new(
                id::<SourceBindingId>("binding.configuration.fixture"),
                SourceKindV1::Cursor,
                LocatorDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
                AuthorityRef::Project(id::<ProjectId>("project.configuration.fixture")),
            )
            .unwrap(),
        );
        let registry = ConfigurationRegistry::core().unwrap();
        let scope = Scope { evidence };
        let credentials = Credentials;
        let clock = Clock;
        let operations = ConfigurationControlPlaneOperations::new(
            &registry,
            &store,
            &scope,
            &credentials,
            &authorization,
            &clock,
        );

        let plan = operations
            .dry_run_protected_change(authority, change.clone(), revision_id)
            .await
            .unwrap();
        let (saved_plan, saved_operation) = store.saved.lock().unwrap().clone().unwrap();
        assert_eq!(saved_plan, plan);
        assert_eq!(saved_operation, change);
        assert_eq!(
            saved_plan.operation_digest,
            saved_operation.compute_digest().unwrap()
        );
    }

    #[tokio::test]
    async fn protected_apply_restart_replays_original_receipt_after_plan_expiry() {
        let actor_id: ActorId = id("actor.configuration.replay");
        let revision_id: ConfigurationRevisionId = id("configuration.revision.replay.base");
        let result_revision_id: ConfigurationRevisionId =
            id("configuration.revision.replay.result");
        let idempotency_key: tracedecay_domain::configuration::ConfigurationIdempotencyKey =
            id("configuration.idempotency.replay.protected");
        let scope_digest = digest('a');
        let policy_digest = policy_digest('b');
        let evidence = ScopeRevalidationEvidenceV1 {
            resolved_scope_digest: scope_digest.clone(),
            membership_digest: None,
            authorization_policy_digest: policy_digest.clone(),
            policy_epoch: 7,
        };
        let snapshot =
            ConfigurationSnapshotV1::new(BTreeMap::default(), BTreeMap::default()).unwrap();
        let change = ProtectedChange::BindSource(
            ScopeSourceBinding::new(
                id::<SourceBindingId>("binding.configuration.replay"),
                SourceKindV1::Cursor,
                LocatorDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
                AuthorityRef::Project(id::<ProjectId>("project.configuration.replay")),
            )
            .unwrap(),
        );
        let operation_digest = change.compute_digest().unwrap();
        let plan = plan_protected_change(
            ProtectedChangePlanDraftV1 {
                plan_id: id("configuration.plan.replay.protected"),
                actor_id: actor_id.clone(),
                base_revision_id: revision_id.clone(),
                resolved_scope_digest: scope_digest.clone(),
                membership_digest: None,
                authorization_policy_digest: policy_digest.clone(),
                policy_epoch: 7,
                created_at: UtcMicros(1),
                expires_at: UtcMicros(5),
                before_digest: Some(digest('d')),
                after_digest: Some(operation_digest.clone()),
            },
            change.clone(),
        )
        .unwrap();
        let request = ProtectedApplyRequest {
            plan_id: plan.plan_id.clone(),
            actor_id: actor_id.clone(),
            expected_base_revision_id: revision_id.clone(),
            operation_digest: operation_digest.clone(),
            idempotency_key: idempotency_key.clone(),
        };
        let receipt = ConfigurationMutationReceipt {
            receipt_id: id("configuration.receipt.replay.protected"),
            base_revision_id: revision_id.clone(),
            result_revision_id,
            snapshot_id: snapshot.snapshot_id.clone(),
            operation_digest,
            settlement_authority: ConfigurationSettlementAuthorityV1 {
                policy_epoch: 7,
                policy_digest: policy_digest.clone(),
                revalidated_at: UtcMicros(2),
            },
            created_at: UtcMicros(2),
            effective_deadline_at: UtcMicros(100),
        };
        let store = Store {
            current: ConfigurationCurrentStateV1 {
                revision_id: revision_id.clone(),
                snapshot,
            },
            saved: Mutex::new(Some((plan, change))),
            replay: Mutex::new(Some((
                request.clone(),
                ConfigurationMutationOperationV1::ProtectedApply,
                receipt.clone(),
            ))),
        };
        let authorization = Authorization {
            current: super::super::ports::CurrentConfigurationMutationAuthorizationV1 {
                grant_revision: 2,
                grant_digest: digest('e'),
                scope_digest: scope_digest.clone(),
                policy_epoch: 7,
                policy_digest: policy_digest.clone(),
            },
        };
        let authority = ConfigurationMutationAuthority {
            receipt: ConfigurationMutationGrantReceiptV1::issue(
                id::<ConfigurationGrantReceiptId>("configuration.grant-receipt.replay"),
                id::<ConfigurationGrantId>("configuration.grant.replay"),
                actor_id,
                ConfigurationMutationOperationV1::ProtectedApply,
                scope_digest,
                revision_id,
                7,
                policy_digest,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
                Some(idempotency_key),
                UtcMicros(9),
                UtcMicros(100),
            )
            .unwrap(),
        };
        let registry = ConfigurationRegistry::core().unwrap();
        let scope = Scope { evidence };
        let credentials = Credentials;
        let clock = AdvancedClock(UtcMicros(10));

        let restarted = ConfigurationControlPlaneOperations::new(
            &registry,
            &store,
            &scope,
            &credentials,
            &authorization,
            &clock,
        );

        assert_eq!(
            restarted
                .apply_protected_change(authority, request)
                .await
                .unwrap(),
            receipt
        );
    }
}
