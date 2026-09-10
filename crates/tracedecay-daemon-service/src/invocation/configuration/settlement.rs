//! Durable configuration effect rendering and runtime reconciliation.

use super::*;
use tracedecay_domain::configuration::RestartRequirementV1;
use tracedecay_global_db::configuration::registry::ConfigurationRegistry;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

fn requires_daemon_restart(
    observed: &ConfigurationSnapshotV1,
    desired: &ConfigurationSnapshotV1,
) -> Result<bool, ConfigurationError> {
    observed
        .validate()
        .map_err(ConfigurationError::validation)?;
    desired.validate().map_err(ConfigurationError::validation)?;
    let registry = ConfigurationRegistry::core().map_err(ConfigurationError::validation)?;
    Ok(registry.definitions().any(|definition| {
        definition.restart_requirement == RestartRequirementV1::DaemonRestart
            && observed.effective_values.get(&definition.key)
                != desired.effective_values.get(&definition.key)
    }))
}

#[hotpath::measure(label = "daemon.service.configuration.reconcile", future = true)]
pub(super) async fn reconcile_configuration_runtime(
    registered: &RegisteredConfigurationRuntime,
    receipt: &tracedecay_configuration::ConfigurationMutationReceipt,
    now: UtcMicros,
) {
    let current = match hotpath::future!(
        registered.runtime.client().current(),
        label = "daemon.service.configuration.reconcile_read"
    )
    .await
    {
        Ok(current) => current,
        Err(error) => {
            tracing::warn!(
                receipt_id = %receipt.receipt_id,
                error = %error,
                "configuration committed; runtime reconciliation could not read desired state"
            );
            return;
        }
    };
    let observed = match hotpath::future!(
        registered.runtime.observed_runtime_configuration(),
        label = "daemon.service.configuration.reconcile_observed"
    )
    .await
    {
        Ok(Some(observed)) => observed,
        Ok(None) => {
            tracing::warn!(
                receipt_id = %receipt.receipt_id,
                "configuration committed before runtime activation was observed"
            );
            return;
        }
        Err(error) => {
            tracing::warn!(
                receipt_id = %receipt.receipt_id,
                error = %error,
                "configuration committed; observed runtime configuration is unavailable"
            );
            return;
        }
    };
    let restart_required = match requires_daemon_restart(&observed.snapshot, current.snapshot()) {
        Ok(restart_required) => restart_required,
        Err(error) => {
            tracing::warn!(
                receipt_id = %receipt.receipt_id,
                error = %error,
                "configuration committed; restart requirement could not be resolved"
            );
            return;
        }
    };
    if restart_required {
        tracing::info!(
            receipt_id = %receipt.receipt_id,
            desired_revision_id = %current.revision_id(),
            observed_revision_id = %observed.revision_id,
            "configuration committed; daemon restart is required before activation"
        );
    }
    let revision_id = current.revision_id().clone();
    let successful_observed_revision_id = if restart_required {
        observed.revision_id
    } else {
        revision_id.clone()
    };
    let installation = hotpath::measure_block!("daemon.service.configuration.activate", {
        tracedecay_configuration::config::publish_pinned_runtime_configuration(current)
            .map_err(|error| error.to_string())
    });
    let (observed_revision_id, activation_error_code) = match installation {
        Ok(()) => (Some(successful_observed_revision_id), None),
        Err(error) => {
            tracing::warn!(
                receipt_id = %receipt.receipt_id,
                error,
                "configuration committed; runtime activation remains pending"
            );
            (
                None,
                Some("runtime_configuration_activation_failed".to_owned()),
            )
        }
    };
    if let Err(error) = registered
        .runtime
        .record_runtime_activation(observed_revision_id, activation_error_code, now)
        .await
    {
        tracing::warn!(
            receipt_id = %receipt.receipt_id,
            error = %error,
            "configuration committed; activation observation remains pending"
        );
    }
}

#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "daemon.service.configuration.effect")]
pub(super) fn configuration_effect(
    payload: serde_json::Value,
    mut authority: AuthorityReceipt,
    actor: &ActorId,
    scope: &ResolvedScope,
    operation: ApplicationSurfaceOperation,
    caller_idempotency_key: &ConfigurationIdempotencyKey,
    expected_revision: &ConfigurationRevisionId,
    operation_digest: ManifestDigest,
    settlement_authority: tracedecay_domain::configuration::ConfigurationSettlementAuthorityV1,
    committed_at: UtcMicros,
    effective_deadline_at: UtcMicros,
) -> Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> {
    let application_operation =
        tracedecay_contracts::configuration::configuration_surface_operation(operation.as_str())
            .map_err(ConfigurationError::validation)?
            .ok_or_else(|| {
                ConfigurationError::validation_message("unknown configuration operation")
            })?;
    let idempotency_key = IdempotencyKey::new(caller_idempotency_key.as_str().to_owned())
        .map_err(ConfigurationError::validation)?;
    let effect_identity_digest = derive_logical_effect_idempotency(
        LogicalEffectIdempotencyDomain::ConfigurationEffect,
        &(actor, scope, operation.as_str(), &idempotency_key),
    )
    .map_err(|error| ConfigurationError::validation_message(error.to_string()))?;
    let effect_identity_suffix = effect_identity_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| {
            ConfigurationError::validation_message(
                "configuration effect identity digest is malformed",
            )
        })?;
    let canonical_request_id =
        RequestId::new(format!("request.configuration.{effect_identity_suffix}"))
            .map_err(ConfigurationError::validation)?;
    authority.policy.revision = settlement_authority.policy_epoch;
    authority.policy.digest =
        ManifestDigest::new(settlement_authority.policy_digest.as_str().to_owned())
            .map_err(ConfigurationError::validation)?;
    authority.grant_id = CapabilityGrantId::new(format!(
        "grant.daemon.configuration.{effect_identity_suffix}"
    ))
    .map_err(ConfigurationError::validation)?;
    authority.grant_digest = stable_digest(&(
        "tracedecay.daemon.configuration-effect-grant.v1",
        actor,
        scope,
        operation.as_str(),
        &idempotency_key,
        &authority.policy,
    ))
    .map_err(|_| ConfigurationError::Unavailable)?;
    authority.revalidated_at = settlement_authority.revalidated_at;
    let expected_state = canonical_sha256(&(
        "tracedecay.configuration.expected-revision.v1",
        expected_revision,
    ))
    .map_err(ConfigurationError::validation)?;
    let committed_state = canonical_sha256(&(
        "tracedecay.configuration.committed-effect.v1",
        &operation_digest,
        &payload,
    ))
    .map_err(ConfigurationError::validation)?;
    let execution = OperationReceipt::completed(
        committed_at,
        committed_at,
        Deadline::new(effective_deadline_at).map_err(ConfigurationError::validation)?,
        OperationBudgetUsage::default(),
    )
    .map_err(ConfigurationError::validation)?;
    let receipt = EffectReceipt {
        operation: application_operation.use_case_id().clone(),
        request_id: canonical_request_id,
        actor: actor.clone(),
        scope: scope.clone(),
        effect_class: EffectClass::ConfigurationWrite,
        idempotency_key: idempotency_key.clone(),
        input_digest: operation_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: committed_state.clone(),
        catalog_digest: stable_digest(&"tracedecay.application.catalog.v1")
            .map_err(|_| ConfigurationError::Unavailable)?,
        privacy_digest: stable_digest(&"tracedecay.application.privacy.v1")
            .map_err(|_| ConfigurationError::Unavailable)?,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    let effect = EffectResult::new(
        EffectId::new(format!("effect.configuration.{effect_identity_suffix}"))
            .map_err(ConfigurationError::validation)?,
        EffectClass::ConfigurationWrite,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Pending,
        receipt,
        Some(payload),
    )
    .map_err(ConfigurationError::validation)?;
    Ok(ApplicationOutcome::Effect(effect))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved_snapshot(
        key: &str,
        value: tracedecay_domain::configuration::ConfigurationValueV1,
    ) -> ConfigurationSnapshotV1 {
        resolved_snapshot_entries(vec![(key, value)])
    }

    fn resolved_snapshot_entries(
        entries: Vec<(&str, tracedecay_domain::configuration::ConfigurationValueV1)>,
    ) -> ConfigurationSnapshotV1 {
        let registry = ConfigurationRegistry::core().unwrap();
        let project_id = ProjectId::new("project.configuration.restart.fixture").unwrap();
        let entries = entries
            .into_iter()
            .map(|(key, value)| {
                (
                    tracedecay_domain::configuration::SettingKey::new(key).unwrap(),
                    value,
                )
            })
            .collect();
        tracedecay_global_db::configuration::resolver::resolve_configuration(
            &registry,
            &[
                tracedecay_global_db::configuration::resolver::ConfigurationLayerV1 {
                    layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                        project_id,
                    },
                    revision_id: ConfigurationRevisionId::new(
                        "configuration.revision.restart.fixture",
                    )
                    .unwrap(),
                    entries,
                },
            ],
        )
        .unwrap()
        .snapshot
    }

    #[test]
    fn executable_binding_transition_requires_daemon_restart() {
        use tracedecay_domain::WorkExecutableReference;
        use tracedecay_domain::configuration::{
            WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1,
            WorkExecutableCapabilityV1,
        };

        let registry = ConfigurationRegistry::core().unwrap();
        let observed =
            tracedecay_global_db::configuration::resolver::resolve_configuration(&registry, &[])
                .unwrap()
                .snapshot;
        let executable = WorkExecutableReference::new(
            "provider.configuration.restart.fixture".to_owned(),
            digest('e'),
        )
        .unwrap();
        let binding = WorkExecutableBindingV1::new(
            executable,
            std::path::PathBuf::from("/tmp/provider-configuration-restart-fixture"),
            vec![WorkExecutableCapabilityV1::CodexCliExecJson],
            Vec::new(),
        )
        .unwrap();
        let desired = resolved_snapshot(
            WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
            tracedecay_domain::configuration::ConfigurationValueV1::WorkExecutableBindings(vec![
                binding,
            ]),
        );

        assert!(requires_daemon_restart(&observed, &desired).unwrap());
    }

    #[test]
    fn live_setting_transition_does_not_require_daemon_restart() {
        use tracedecay_domain::configuration::{
            ConfigurationValueV1, DIAGNOSTICS_PREWARM_SETTING_KEY,
        };

        let observed = resolved_snapshot(
            DIAGNOSTICS_PREWARM_SETTING_KEY,
            ConfigurationValueV1::Boolean(false),
        );
        let desired = resolved_snapshot(
            DIAGNOSTICS_PREWARM_SETTING_KEY,
            ConfigurationValueV1::Boolean(true),
        );

        assert!(!requires_daemon_restart(&observed, &desired).unwrap());
    }

    #[test]
    fn reverted_restart_setting_can_settle_current_without_restart() {
        use tracedecay_domain::configuration::{
            ConfigurationValueV1, WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
        };

        let registry = ConfigurationRegistry::core().unwrap();
        let observed =
            tracedecay_global_db::configuration::resolver::resolve_configuration(&registry, &[])
                .unwrap()
                .snapshot;
        let reverted = resolved_snapshot(
            WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
            ConfigurationValueV1::WorkExecutableBindings(Vec::new()),
        );

        assert_ne!(observed.snapshot_id, reverted.snapshot_id);
        assert!(!requires_daemon_restart(&observed, &reverted).unwrap());
    }

    #[test]
    fn live_change_does_not_erase_pending_restart_against_durable_observation() {
        use tracedecay_domain::WorkExecutableReference;
        use tracedecay_domain::configuration::{
            ConfigurationValueV1, DIAGNOSTICS_PREWARM_SETTING_KEY,
            WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1,
            WorkExecutableCapabilityV1,
        };

        let registry = ConfigurationRegistry::core().unwrap();
        let observed =
            tracedecay_global_db::configuration::resolver::resolve_configuration(&registry, &[])
                .unwrap()
                .snapshot;
        let binding = WorkExecutableBindingV1::new(
            WorkExecutableReference::new(
                "provider.configuration.mixed.fixture".to_owned(),
                digest('f'),
            )
            .unwrap(),
            std::path::PathBuf::from("/tmp/provider-configuration-mixed-fixture"),
            vec![WorkExecutableCapabilityV1::CodexCliExecJson],
        )
        .unwrap();
        let pending = resolved_snapshot_entries(vec![
            (
                WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
                ConfigurationValueV1::WorkExecutableBindings(vec![binding.clone()]),
            ),
            (
                DIAGNOSTICS_PREWARM_SETTING_KEY,
                ConfigurationValueV1::Boolean(false),
            ),
        ]);
        let advanced = resolved_snapshot_entries(vec![
            (
                WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
                ConfigurationValueV1::WorkExecutableBindings(vec![binding]),
            ),
            (
                DIAGNOSTICS_PREWARM_SETTING_KEY,
                ConfigurationValueV1::Boolean(true),
            ),
        ]);

        assert!(requires_daemon_restart(&observed, &pending).unwrap());
        assert!(requires_daemon_restart(&observed, &advanced).unwrap());
        assert!(!requires_daemon_restart(&pending, &advanced).unwrap());
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn authority(
        scope: &ResolvedScope,
        policy_revision: u64,
        observed_at: UtcMicros,
    ) -> AuthorityReceipt {
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new("grant.configuration.retry.fixture").unwrap(),
            grant_revision: 1,
            grant_digest: digest('a'),
            authorized_scope_digest: scope.scope_digest.clone(),
            disclosure: DisclosureClass::Sensitive,
            policy: PolicyDecisionRef::new(
                "policy.daemon.configuration.v1",
                policy_revision,
                digest('b'),
                ComponentVersion::new("tracedecay.daemon.configuration-policy.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: observed_at,
        }
    }

    #[test]
    fn replay_effect_uses_persisted_policy_evidence_not_retry_policy() {
        let actor = ActorId::new("actor.configuration.replay").unwrap();
        let scope = ResolvedScope::new(
            ProjectId::new("project.configuration.replay").unwrap(),
            tracedecay_domain::RepositoryId::new("repository.configuration.replay").unwrap(),
            tracedecay_domain::WorktreeId::new("worktree.configuration.replay").unwrap(),
            None,
        )
        .unwrap();
        let settlement = tracedecay_domain::configuration::ConfigurationSettlementAuthorityV1 {
            policy_epoch: 7,
            policy_digest: AccessPolicyDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
            revalidated_at: UtcMicros(10),
        };
        let render = |authority| {
            configuration_effect(
                serde_json::json!({"receipt_id": "configuration.receipt.replay"}),
                authority,
                &actor,
                &scope,
                ApplicationSurfaceOperation::ConfigurationSet,
                &ConfigurationIdempotencyKey::new("configuration.idempotency.effect-replay")
                    .unwrap(),
                &ConfigurationRevisionId::new("configuration.revision.base").unwrap(),
                digest('d'),
                settlement.clone(),
                UtcMicros(10),
                UtcMicros(20),
            )
            .unwrap()
        };

        let original = render(authority(&scope, 7, UtcMicros(10)));
        let replay_after_policy_change = render(authority(&scope, 9, UtcMicros(19)));

        assert_eq!(replay_after_policy_change, original);
    }
}
