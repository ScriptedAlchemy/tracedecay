//! Durable configuration effect rendering and runtime reconciliation.

use super::*;

pub(super) async fn reconcile_configuration_runtime(
    registered: &RegisteredConfigurationRuntime,
    receipt: &crate::application::configuration::ConfigurationMutationReceipt,
    now: UtcMicros,
) {
    let current = match registered.runtime.client().current().await {
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
    let installation = crate::config::root_runtime_configuration(&current)
        .map_err(|error| error.to_string())
        .and_then(|root| {
            crate::config::install_pinned_runtime_configuration(root)
                .map_err(|error| error.to_string())
        });
    let (observed_revision_id, activation_error_code) = match installation {
        Ok(()) => (Some(current.revision_id), None),
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
pub(super) fn configuration_effect(
    payload: serde_json::Value,
    mut authority: AuthorityReceipt,
    actor: &ActorId,
    scope: &ResolvedScope,
    operation: crate::application_surface::ApplicationSurfaceOperation,
    caller_idempotency_key: &ConfigurationIdempotencyKey,
    expected_revision: &ConfigurationRevisionId,
    operation_digest: ManifestDigest,
    settlement_authority: tracedecay_domain::configuration::ConfigurationSettlementAuthorityV1,
    committed_at: UtcMicros,
    effective_deadline_at: UtcMicros,
) -> Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> {
    let application_operation =
        tracedecay_application::configuration::configuration_surface_operation(operation.as_str())
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
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet,
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
