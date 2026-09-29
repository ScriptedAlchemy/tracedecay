use std::path::Path;
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_contracts::now_micros;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{
    ApplicationEnvelope, ApplicationOutcome, CancellationSignal, ComponentConfigurationState,
    Deadline, EffectReceipt, ResolvedSetting,
};
use tracedecay_contracts::{
    ConfigurationBatchRequestV1, ConfigurationDirectMutationRequestV1, ConfigurationGetRequestV1,
    ConfigurationObservedStateRequestV1, ConfigurationSetRequestV1, ConfigurationUnsetRequestV1,
    ConfigurationWireRequestV1,
};
use tracedecay_daemon_protocol::ApplicationSurfaceRequest;
use tracedecay_daemon_protocol::RequestedOutputFormat;
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationValueV1, SettingKey, USER_UPLOAD_ENABLED_SETTING_KEY, UserProfileId,
};
use tracedecay_domain::{ProjectId, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

fn configuration_error(message: impl Into<String>) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: message.into(),
    }
}

fn cli_configuration_idempotency_key(
    project_id: &ProjectId,
    expected_revision: &ConfigurationRevisionId,
    mutations: &[ConfigurationDirectMutationRequestV1],
) -> tracedecay_domain::errors::Result<ConfigurationIdempotencyKey> {
    let digest = canonical_sha256(&(
        "tracedecay.cli.configuration-mutation.v1",
        project_id,
        expected_revision,
        mutations,
    ))
    .map_err(|error| configuration_error(format!("invalid configuration mutation: {error}")))?;
    let suffix = digest
        .hex_suffix()
        .ok_or_else(|| configuration_error("configuration mutation digest is malformed"))?;
    ConfigurationIdempotencyKey::new(format!("configuration.idempotency.cli.{suffix}"))
        .map_err(|error| configuration_error(format!("invalid configuration request key: {error}")))
}

fn cli_user_configuration_idempotency_key(
    profile_id: &UserProfileId,
    expected_revision: &ConfigurationRevisionId,
    mutations: &[ConfigurationDirectMutationRequestV1],
) -> tracedecay_domain::errors::Result<ConfigurationIdempotencyKey> {
    let digest = canonical_sha256(&(
        "tracedecay.cli.user-configuration-mutation.v1",
        profile_id,
        expected_revision,
        mutations,
    ))
    .map_err(|error| {
        configuration_error(format!("invalid user configuration mutation: {error}"))
    })?;
    let suffix = digest
        .hex_suffix()
        .ok_or_else(|| configuration_error("user configuration mutation digest is malformed"))?;
    ConfigurationIdempotencyKey::new(format!("configuration.idempotency.cli.user.{suffix}"))
        .map_err(|error| {
            configuration_error(format!("invalid user configuration request key: {error}"))
        })
}

fn configuration_deadline(
    operation: ApplicationSurfaceOperation,
    observed_at: UtcMicros,
) -> tracedecay_domain::errors::Result<Deadline> {
    let application_operation =
        tracedecay_contracts::configuration::configuration_surface_operation(operation.as_str())
            .map_err(|error| configuration_error(error.to_string()))?
            .ok_or_else(|| configuration_error("configuration operation is not cataloged"))?;
    let catalog = tracedecay_daemon_service::application_surface::application_surface_catalog()
        .map_err(|error| configuration_error(error.to_string()))?;
    let maximum_millis = catalog
        .capability(application_operation.capability_id())
        .ok_or_else(|| configuration_error("configuration capability is not cataloged"))?
        .deadline()
        .maximum_millis();
    let maximum_micros = i64::try_from(maximum_millis)
        .ok()
        .and_then(|millis| millis.checked_mul(1_000))
        .ok_or_else(|| configuration_error("configuration deadline exceeds the domain clock"))?;
    Deadline::new(UtcMicros(observed_at.0.saturating_add(maximum_micros)))
        .map_err(|error| configuration_error(error.to_string()))
}

/// Invoke one configuration request. A request of profile settings only is
/// served by the profile's configuration store, so it names no project.
async fn invoke_configuration_surface(
    profile: &ProfileRoot,
    project_path: Option<&Path>,
    operation: ApplicationSurfaceOperation,
    request: ConfigurationWireRequestV1,
) -> tracedecay_domain::errors::Result<ApplicationEnvelope<serde_json::Value>> {
    let request_id = mint_global_request_id(GlobalRequestSurface::Cli)
        .map_err(|error| configuration_error(error.to_string()))?;
    let observed_at = now_micros();
    let deadline = configuration_deadline(operation, observed_at)?;
    let cancellation =
        CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str()))
            .map_err(|error| configuration_error(error.to_string()))?;
    let handshake = super::daemon::client_handshake(profile, project_path)?;
    let client = tracedecay::daemon::invocation_client_for_current(profile, handshake)?;
    loop {
        let result = crate::cli::dispatch::resolve_cli_application_surface(
            operation,
            request_id.clone(),
            ApplicationSurfaceRequest::Configuration(request.clone()),
            RequestedOutputFormat::Json,
            deadline.clone(),
            cancellation.clone(),
            Some(&client),
        )
        .await
        .map_err(|error| configuration_error(error.to_string()))?;
        if let Some(delay) = crate::cli::dispatch::surface_retry_delay(&result) {
            let now = now_micros();
            let remaining_micros = deadline.expires_at.0.saturating_sub(now.0);
            let remaining_micros = u64::try_from(remaining_micros)
                .map_err(|_| configuration_error("configuration deadline elapsed"))?;
            if delay <= std::time::Duration::from_micros(remaining_micros) {
                tokio::time::sleep(delay).await;
                continue;
            }
        }
        return result.result.map_err(|problem| {
            configuration_error(format!(
                "{}: {}",
                problem.problem.code, problem.problem.message
            ))
        });
    }
}

pub(crate) async fn current_configuration_revision(
    profile: &ProfileRoot,
    project_path: &Path,
) -> tracedecay_domain::errors::Result<ConfigurationRevisionId> {
    let envelope = invoke_configuration_surface(
        profile,
        Some(project_path),
        ApplicationSurfaceOperation::ConfigurationObservedState,
        ConfigurationWireRequestV1::ObservedState(ConfigurationObservedStateRequestV1 {}),
    )
    .await?;
    let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
        return Err(configuration_error(
            "configuration state returned a non-evidence outcome",
        ));
    };
    let states: Vec<ComponentConfigurationState> = serde_json::from_value(
        evidence
            .payload
            .ok_or_else(|| configuration_error("configuration state omitted its payload"))?,
    )
    .map_err(|error| configuration_error(format!("invalid configuration state: {error}")))?;
    let revision = states
        .first()
        .map(|state| state.desired_revision_id.clone())
        .ok_or_else(|| configuration_error("configuration state has no runtime component"))?;
    if states
        .iter()
        .any(|state| state.desired_revision_id != revision)
    {
        return Err(configuration_error(
            "configuration components disagree on the desired revision",
        ));
    }
    Ok(revision)
}

pub(crate) async fn current_project_setting(
    profile: &ProfileRoot,
    project_path: &Path,
    key: &str,
) -> tracedecay_domain::errors::Result<ConfigurationValueV1> {
    resolved_setting(profile, Some(project_path), key)
        .await
        .map(|setting| setting.effective_value)
}

async fn resolved_setting(
    profile: &ProfileRoot,
    project_path: Option<&Path>,
    key: &str,
) -> tracedecay_domain::errors::Result<ResolvedSetting> {
    let key = SettingKey::new(key).map_err(|error| configuration_error(error.to_string()))?;
    let envelope = invoke_configuration_surface(
        profile,
        project_path,
        ApplicationSurfaceOperation::ConfigurationGet,
        ConfigurationWireRequestV1::Get(ConfigurationGetRequestV1 { key }),
    )
    .await?;
    let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
        return Err(configuration_error(
            "configuration read returned a non-evidence outcome",
        ));
    };
    serde_json::from_value(
        evidence
            .payload
            .ok_or_else(|| configuration_error("configuration read omitted its payload"))?,
    )
    .map_err(|error| configuration_error(format!("invalid configuration setting: {error}")))
}

/// The profile's worldwide-counter upload setting, wherever the command runs.
pub(crate) async fn canonical_upload_enabled(
    profile: &ProfileRoot,
) -> tracedecay_domain::errors::Result<bool> {
    upload_enabled(&resolved_setting(profile, None, USER_UPLOAD_ENABLED_SETTING_KEY).await?)
}

fn upload_enabled(setting: &ResolvedSetting) -> tracedecay_domain::errors::Result<bool> {
    match setting.effective_value {
        ConfigurationValueV1::Boolean(enabled) => Ok(enabled),
        _ => Err(configuration_error(
            "worldwide counter upload setting is not boolean",
        )),
    }
}

pub(crate) async fn mutate_project_configuration(
    profile: &ProfileRoot,
    project_path: &Path,
    project_id: &ProjectId,
    expected_revision: ConfigurationRevisionId,
    mutations: Vec<ConfigurationDirectMutationRequestV1>,
) -> tracedecay_domain::errors::Result<Option<EffectReceipt>> {
    if mutations.is_empty() {
        return Ok(None);
    }
    let idempotency_key =
        cli_configuration_idempotency_key(project_id, &expected_revision, &mutations)?;
    let (operation, request) = match mutations.as_slice() {
        [ConfigurationDirectMutationRequestV1::Set { layer, key, value }] => (
            ApplicationSurfaceOperation::ConfigurationSet,
            ConfigurationWireRequestV1::Set(ConfigurationSetRequestV1 {
                layer: layer.clone(),
                key: key.clone(),
                value: value.as_ref().clone(),
                expected_revision,
                idempotency_key: idempotency_key.clone(),
            }),
        ),
        [ConfigurationDirectMutationRequestV1::Unset { layer, key }] => (
            ApplicationSurfaceOperation::ConfigurationUnset,
            ConfigurationWireRequestV1::Unset(ConfigurationUnsetRequestV1 {
                layer: layer.clone(),
                key: key.clone(),
                expected_revision,
                idempotency_key: idempotency_key.clone(),
            }),
        ),
        _ => (
            ApplicationSurfaceOperation::ConfigurationBatch,
            ConfigurationWireRequestV1::Batch(ConfigurationBatchRequestV1 {
                mutations,
                expected_revision,
                idempotency_key: idempotency_key.clone(),
            }),
        ),
    };
    let envelope =
        invoke_configuration_surface(profile, Some(project_path), operation, request).await?;
    configuration_effect_receipt(envelope, &idempotency_key).map(Some)
}

async fn mutate_user_configuration(
    profile: &ProfileRoot,
    profile_id: &UserProfileId,
    expected_revision: ConfigurationRevisionId,
    mutations: Vec<ConfigurationDirectMutationRequestV1>,
) -> tracedecay_domain::errors::Result<Option<EffectReceipt>> {
    if mutations.is_empty() {
        return Ok(None);
    }
    let idempotency_key =
        cli_user_configuration_idempotency_key(profile_id, &expected_revision, &mutations)?;
    let envelope = invoke_configuration_surface(
        profile,
        None,
        ApplicationSurfaceOperation::ConfigurationBatch,
        ConfigurationWireRequestV1::Batch(ConfigurationBatchRequestV1 {
            mutations,
            expected_revision,
            idempotency_key: idempotency_key.clone(),
        }),
    )
    .await?;
    configuration_effect_receipt(envelope, &idempotency_key).map(Some)
}

fn configuration_effect_receipt(
    envelope: ApplicationEnvelope<serde_json::Value>,
    idempotency_key: &ConfigurationIdempotencyKey,
) -> tracedecay_domain::errors::Result<EffectReceipt> {
    let ApplicationOutcome::Effect(effect) = envelope.outcome else {
        return Err(configuration_error(
            "configuration mutation returned a non-effect outcome",
        ));
    };
    if effect.idempotency_key.as_str() != idempotency_key.as_str()
        || effect.receipt.idempotency_key.as_str() != idempotency_key.as_str()
    {
        return Err(configuration_error(
            "configuration mutation returned a receipt for another request",
        ));
    }
    effect
        .receipt
        .validate()
        .map_err(|error| configuration_error(error.to_string()))?;
    Ok(effect.receipt)
}

pub(crate) fn project_configuration_set(
    project_id: &ProjectId,
    key: &str,
    value: ConfigurationValueV1,
) -> tracedecay_domain::errors::Result<ConfigurationDirectMutationRequestV1> {
    Ok(ConfigurationDirectMutationRequestV1::Set {
        layer: ConfigurationLayerIdV1::Project {
            project_id: project_id.clone(),
        },
        key: SettingKey::new(key).map_err(|error| configuration_error(error.to_string()))?,
        value: Box::new(value),
    })
}

pub(crate) fn report_configuration_receipt(receipt: Option<&EffectReceipt>) {
    if let Some(receipt) = receipt {
        eprintln!("Receipt: {}", receipt.request_id.as_str());
    }
}

#[hotpath::measure(label = "cli.settings.upload_counter", future = true)]
pub(crate) async fn handle_upload_counter(
    profile: &ProfileRoot,
    enable: bool,
) -> tracedecay_domain::errors::Result<()> {
    let current = resolved_setting(profile, None, USER_UPLOAD_ENABLED_SETTING_KEY).await?;
    let profile_id =
        tracedecay_daemon_identity::profile_identity::load_existing(profile.data_dir())?
            .profile_id()
            .clone();
    let mutations = if upload_enabled(&current)? != enable {
        vec![ConfigurationDirectMutationRequestV1::Set {
            layer: ConfigurationLayerIdV1::UserProfile {
                profile_id: profile_id.clone(),
            },
            key: current.key,
            value: Box::new(ConfigurationValueV1::Boolean(enable)),
        }]
    } else {
        Vec::new()
    };
    let receipt =
        mutate_user_configuration(profile, &profile_id, current.revision_id, mutations).await?;
    if enable {
        eprintln!("Worldwide counter upload enabled.");
    } else {
        eprintln!(
            "Worldwide counter upload disabled. You can re-enable with `tracedecay enable-upload-counter`."
        );
    }
    report_configuration_receipt(receipt.as_ref());
    Ok(())
}
