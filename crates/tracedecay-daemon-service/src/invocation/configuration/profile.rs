//! Profile configuration: reads and direct writes of the settings the
//! profile's own `ProfileSessions` store owns. No project is opened or named.

use super::*;
use tracedecay_configuration::config::setting_findings;
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, SettingKey, USER_CODE_INDEX_WORKERS_SETTING_KEY, UserProfileId,
};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::configuration::ProfileConfigurationStore;
use tracedecay_global_db::configuration::contracts::types::ResolvedSetting;
use tracedecay_global_db::configuration::registry::ConfigurationRegistry;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

/// The authenticated profile's configuration route: its registered
/// `ProfileSessions` store and the scope and actor its requests settle under.
pub struct ProfileConfigurationAuthorityV1 {
    pub database: RegisteredGlobalDbLeaseV1,
    pub profile_id: UserProfileId,
    pub scope: ResolvedScope,
    pub actor: ActorId,
}

#[hotpath::measure(label = "daemon.service.configuration.profile", future = true)]
pub async fn execute_profile_configuration(
    wire_request_id: String,
    profile: ProfileConfigurationAuthorityV1,
    surface_operation: ApplicationSurfaceOperation,
    request: ConfigurationWireRequestV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    if cancellation.is_cancelled() {
        return application_problem(
            wire_request_id,
            ApplicationProblem::cancelled_before_admission(),
        );
    }
    if deadline.is_elapsed_at(observed_at) || deadline.is_elapsed_at(now_micros()) {
        return application_problem(
            wire_request_id,
            ApplicationProblem::timed_out_before_admission(),
        );
    }
    let policy_digest = match profile_policy_digest(&profile.profile_id) {
        Ok(digest) => digest,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let authority = match configuration_route_authority(
        ConfigurationRoutePolicyV1 {
            actor: &profile.actor,
            scope: &profile.scope,
            grant_expires_at: deadline.expires_at,
            policy_epoch: 1,
            policy_digest: &policy_digest,
        },
        &wire_request_id,
        surface_operation,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(authority) => authority,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let result = Box::pin(profile_configuration_outcome(
        &wire_request_id,
        &profile,
        policy_digest,
        authority,
        surface_operation,
        request,
        observed_at,
        deadline,
    ))
    .await;
    match result {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::Configuration {
                scope: profile.scope,
                outcome,
            },
        ),
        Err(error) => application_problem(wire_request_id, configuration_problem(error)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn profile_configuration_outcome(
    wire_request_id: &str,
    profile: &ProfileConfigurationAuthorityV1,
    policy_digest: AccessPolicyDigest,
    authority: AuthorityReceipt,
    surface_operation: ApplicationSurfaceOperation,
    request: ConfigurationWireRequestV1,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> {
    let store = ProfileConfigurationStore::new_registered(&profile.database, &profile.profile_id)?;
    let (idempotency_key, expected_revision, mutation) = match (surface_operation, request) {
        (
            ApplicationSurfaceOperation::ConfigurationGet,
            ConfigurationWireRequestV1::Get(request),
        ) => {
            let registry =
                ConfigurationRegistry::profile().map_err(ConfigurationError::validation)?;
            registry
                .definition(&request.key)
                .map_err(ConfigurationError::validation)?;
            let current = store.read_or_initialize(observed_at).await?;
            let effective_value = current
                .snapshot
                .effective_values
                .get(&request.key)
                .cloned()
                .ok_or(ConfigurationError::Unavailable)?;
            let setting = ResolvedSetting {
                findings: setting_findings(&request.key, &effective_value),
                candidates: current
                    .snapshot
                    .provenance
                    .get(&request.key)
                    .cloned()
                    .unwrap_or_default(),
                key: request.key,
                effective_value,
                revision_id: current.revision_id,
                snapshot_id: current.snapshot.snapshot_id,
                effective_behavior_digest: current.snapshot.effective_behavior_digest,
                resolution_provenance_digest: current.snapshot.resolution_provenance_digest,
            };
            return configuration_evidence(
                serde_json::to_value(setting).map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            );
        }
        (
            ApplicationSurfaceOperation::ConfigurationSet,
            ConfigurationWireRequestV1::Set(request),
        ) => (
            request.idempotency_key,
            request.expected_revision,
            DirectConfigurationMutation::Set {
                layer: request.layer,
                key: request.key,
                value: Box::new(request.value),
            },
        ),
        (
            ApplicationSurfaceOperation::ConfigurationUnset,
            ConfigurationWireRequestV1::Unset(request),
        ) => (
            request.idempotency_key,
            request.expected_revision,
            DirectConfigurationMutation::Unset {
                layer: request.layer,
                key: request.key,
            },
        ),
        (
            ApplicationSurfaceOperation::ConfigurationBatch,
            ConfigurationWireRequestV1::Batch(request),
        ) => (
            request.idempotency_key,
            request.expected_revision,
            DirectConfigurationMutation::Batch {
                mutations: request
                    .mutations
                    .into_iter()
                    .map(|mutation| match mutation {
                        tracedecay_contracts::ConfigurationDirectMutationRequestV1::Set {
                            layer,
                            key,
                            value,
                        } => DirectConfigurationMutation::Set { layer, key, value },
                        tracedecay_contracts::ConfigurationDirectMutationRequestV1::Unset {
                            layer,
                            key,
                        } => DirectConfigurationMutation::Unset { layer, key },
                    })
                    .collect(),
            },
        ),
        _ => {
            return Err(ConfigurationError::validation_message(
                "profile configuration serves only get, set, unset, and batch",
            ));
        }
    };
    // Worker counts are admitted against the installed worker plan, which
    // only the dashboard's worker settings route checks.
    let workers = SettingKey::new(USER_CODE_INDEX_WORKERS_SETTING_KEY)
        .map_err(ConfigurationError::validation)?;
    if mutation.touched_keys()?.contains(&workers) {
        return Err(ConfigurationError::validation_message(
            "user.code_index_workers.v1 is changed through the dashboard worker settings",
        ));
    }
    let grants = DaemonConfigurationGrantAuthority::for_layer(
        profile.actor.clone(),
        policy_digest,
        deadline.expires_at,
        ConfigurationLayerIdV1::UserProfile {
            profile_id: profile.profile_id.clone(),
        },
    )
    .map_err(|_| ConfigurationError::Unavailable)?;
    let mutation_authority = grants
        .issue_direct(
            wire_request_id,
            idempotency_key.clone(),
            &mutation,
            expected_revision.clone(),
            deadline.expires_at,
            observed_at,
        )
        .map_err(|problem| match problem {
            DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                ConfigurationError::MutationAuthorityRejected
            }
            DaemonInvocationProblem::InvalidRequest => {
                ConfigurationError::validation_message("invalid configuration mutation target")
            }
            _ => ConfigurationError::Unavailable,
        })?;
    let committed = store
        .commit_direct(&mutation_authority, &mutation, &expected_revision)
        .await?;
    let receipt = committed.receipt;
    configuration_effect(
        serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
        authority,
        &profile.actor,
        &profile.scope,
        surface_operation,
        &idempotency_key,
        &expected_revision,
        receipt.operation_digest,
        receipt.settlement_authority,
        receipt.created_at,
        receipt.effective_deadline_at,
    )
}

fn profile_policy_digest(
    profile_id: &UserProfileId,
) -> Result<AccessPolicyDigest, ApplicationProblem> {
    let digest = canonical_sha256(&(
        "tracedecay.daemon.profile-configuration-policy.v1",
        profile_id,
    ))
    .map_err(|_| invalid_configuration_request())?;
    AccessPolicyDigest::new(digest.as_str().to_owned()).map_err(|_| invalid_configuration_request())
}
