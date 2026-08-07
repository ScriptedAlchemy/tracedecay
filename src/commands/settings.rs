use std::path::Path;

use tracedecay::application_surface::{
    ApplicationSurfaceOperation, ApplicationSurfaceRequest, ConfigurationBatchSurfaceRequest,
    ConfigurationDirectMutationSurfaceRequest, ConfigurationKeySurfaceRequest,
    ConfigurationObservedStateSurfaceRequest, ConfigurationSetSurfaceRequest,
    ConfigurationSurfaceRequest, ConfigurationUnsetSurfaceRequest,
};
use tracedecay::daemon_client::{
    DaemonInvocationClient, RequestedOutputFormat, invocation_now_micros,
};
use tracedecay::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, CancellationSignal, ComponentConfigurationState,
    Deadline, EffectReceipt, ResolvedSetting,
};
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationValueV1, SettingKey, USER_UPLOAD_ENABLED_SETTING_KEY, UserProfileId,
};
use tracedecay_domain::{ProjectId, UtcMicros, canonical_sha256};

use super::daemon::daemon_tool_json;

fn configuration_error(message: impl Into<String>) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: message.into(),
    }
}

fn cli_configuration_idempotency_key(
    project_id: &ProjectId,
    expected_revision: &ConfigurationRevisionId,
    mutations: &[ConfigurationDirectMutationSurfaceRequest],
) -> tracedecay::errors::Result<ConfigurationIdempotencyKey> {
    let digest = canonical_sha256(&(
        "tracedecay.cli.configuration-mutation.v1",
        project_id,
        expected_revision,
        mutations,
    ))
    .map_err(|error| configuration_error(format!("invalid configuration mutation: {error}")))?;
    let suffix = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| configuration_error("configuration mutation digest is malformed"))?;
    ConfigurationIdempotencyKey::new(format!("configuration.idempotency.cli.{suffix}"))
        .map_err(|error| configuration_error(format!("invalid configuration request key: {error}")))
}

fn cli_user_configuration_idempotency_key(
    profile_id: &UserProfileId,
    expected_revision: &ConfigurationRevisionId,
    mutations: &[ConfigurationDirectMutationSurfaceRequest],
) -> tracedecay::errors::Result<ConfigurationIdempotencyKey> {
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
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| configuration_error("user configuration mutation digest is malformed"))?;
    ConfigurationIdempotencyKey::new(format!("configuration.idempotency.cli.user.{suffix}"))
        .map_err(|error| {
            configuration_error(format!("invalid user configuration request key: {error}"))
        })
}

fn configuration_deadline(
    operation: ApplicationSurfaceOperation,
    observed_at: UtcMicros,
) -> tracedecay::errors::Result<Deadline> {
    let application_operation =
        tracedecay_application::configuration::configuration_surface_operation(operation.as_str())
            .map_err(|error| configuration_error(error.to_string()))?
            .ok_or_else(|| configuration_error("configuration operation is not cataloged"))?;
    let catalog = tracedecay::application_surface::application_surface_catalog()
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

async fn invoke_configuration_surface(
    project_path: &Path,
    operation: ApplicationSurfaceOperation,
    request: ConfigurationSurfaceRequest,
) -> tracedecay::errors::Result<ApplicationEnvelope<serde_json::Value>> {
    let request_id = mint_global_request_id(GlobalRequestSurface::Cli)
        .map_err(|error| configuration_error(error.to_string()))?;
    let observed_at = invocation_now_micros();
    let deadline = configuration_deadline(operation, observed_at)?;
    let cancellation =
        CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str()))
            .map_err(|error| configuration_error(error.to_string()))?;
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    )?;
    let client = DaemonInvocationClient::for_current(handshake)?;
    let result = crate::cli::dispatch::resolve_cli_application_surface(
        operation,
        request_id,
        ApplicationSurfaceRequest::Configuration(request),
        RequestedOutputFormat::Json,
        deadline,
        cancellation,
        Some(&client),
    )
    .await
    .map_err(|error| configuration_error(error.to_string()))?;
    result.result.map_err(|problem| {
        configuration_error(format!(
            "{}: {}",
            problem.problem.code, problem.problem.message
        ))
    })
}

pub(super) async fn current_configuration_revision(
    project_path: &Path,
) -> tracedecay::errors::Result<ConfigurationRevisionId> {
    let envelope = invoke_configuration_surface(
        project_path,
        ApplicationSurfaceOperation::ConfigurationObservedState,
        ConfigurationSurfaceRequest::ObservedState(ConfigurationObservedStateSurfaceRequest {}),
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

pub(super) async fn current_project_setting(
    project_path: &Path,
    key: &str,
) -> tracedecay::errors::Result<ConfigurationValueV1> {
    let key = SettingKey::new(key).map_err(|error| configuration_error(error.to_string()))?;
    let envelope = invoke_configuration_surface(
        project_path,
        ApplicationSurfaceOperation::ConfigurationGet,
        ConfigurationSurfaceRequest::Get(ConfigurationKeySurfaceRequest { key }),
    )
    .await?;
    let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
        return Err(configuration_error(
            "configuration read returned a non-evidence outcome",
        ));
    };
    let setting: ResolvedSetting = serde_json::from_value(
        evidence
            .payload
            .ok_or_else(|| configuration_error("configuration read omitted its payload"))?,
    )
    .map_err(|error| configuration_error(format!("invalid configuration setting: {error}")))?;
    Ok(setting.effective_value)
}

pub(crate) async fn canonical_upload_enabled(
    project_path: &Path,
) -> tracedecay::errors::Result<bool> {
    match current_project_setting(project_path, USER_UPLOAD_ENABLED_SETTING_KEY).await? {
        ConfigurationValueV1::Boolean(enabled) => Ok(enabled),
        _ => Err(configuration_error(
            "worldwide counter upload setting is not boolean",
        )),
    }
}

pub(super) async fn mutate_project_configuration(
    project_path: &Path,
    project_id: &ProjectId,
    expected_revision: ConfigurationRevisionId,
    mutations: Vec<ConfigurationDirectMutationSurfaceRequest>,
) -> tracedecay::errors::Result<Option<EffectReceipt>> {
    if mutations.is_empty() {
        return Ok(None);
    }
    let idempotency_key =
        cli_configuration_idempotency_key(project_id, &expected_revision, &mutations)?;
    let (operation, request) = match mutations.as_slice() {
        [ConfigurationDirectMutationSurfaceRequest::Set { layer, key, value }] => (
            ApplicationSurfaceOperation::ConfigurationSet,
            ConfigurationSurfaceRequest::Set(ConfigurationSetSurfaceRequest {
                layer: layer.clone(),
                key: key.clone(),
                value: value.clone(),
                expected_revision,
                idempotency_key: idempotency_key.clone(),
            }),
        ),
        [ConfigurationDirectMutationSurfaceRequest::Unset { layer, key }] => (
            ApplicationSurfaceOperation::ConfigurationUnset,
            ConfigurationSurfaceRequest::Unset(ConfigurationUnsetSurfaceRequest {
                layer: layer.clone(),
                key: key.clone(),
                expected_revision,
                idempotency_key: idempotency_key.clone(),
            }),
        ),
        _ => (
            ApplicationSurfaceOperation::ConfigurationBatch,
            ConfigurationSurfaceRequest::Batch(ConfigurationBatchSurfaceRequest {
                mutations,
                expected_revision,
                idempotency_key: idempotency_key.clone(),
            }),
        ),
    };
    let envelope = invoke_configuration_surface(project_path, operation, request).await?;
    configuration_effect_receipt(envelope, &idempotency_key).map(Some)
}

async fn mutate_user_configuration(
    project_path: &Path,
    profile_id: &UserProfileId,
    expected_revision: ConfigurationRevisionId,
    mutations: Vec<ConfigurationDirectMutationSurfaceRequest>,
) -> tracedecay::errors::Result<Option<EffectReceipt>> {
    if mutations.is_empty() {
        return Ok(None);
    }
    let idempotency_key =
        cli_user_configuration_idempotency_key(profile_id, &expected_revision, &mutations)?;
    let envelope = invoke_configuration_surface(
        project_path,
        ApplicationSurfaceOperation::ConfigurationBatch,
        ConfigurationSurfaceRequest::Batch(ConfigurationBatchSurfaceRequest {
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
) -> tracedecay::errors::Result<EffectReceipt> {
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

pub(super) fn project_configuration_set(
    project_id: &ProjectId,
    key: &str,
    value: ConfigurationValueV1,
) -> tracedecay::errors::Result<ConfigurationDirectMutationSurfaceRequest> {
    Ok(ConfigurationDirectMutationSurfaceRequest::Set {
        layer: ConfigurationLayerIdV1::Project {
            project_id: project_id.clone(),
        },
        key: SettingKey::new(key).map_err(|error| configuration_error(error.to_string()))?,
        value,
    })
}

pub(super) fn report_configuration_receipt(receipt: Option<&EffectReceipt>) {
    if let Some(receipt) = receipt {
        eprintln!("Receipt: {}", receipt.request_id.as_str());
    }
}

pub(crate) async fn handle_upload_counter(enable: bool) -> tracedecay::errors::Result<()> {
    let resolved =
        super::scope::resolve_project_scope(tracedecay::config::resolve_path_with_discovery(None))
            .await?;
    let expected_revision = current_configuration_revision(&resolved.project_path).await?;
    let current = canonical_upload_enabled(&resolved.project_path).await?;
    let mutations = if current != enable {
        vec![ConfigurationDirectMutationSurfaceRequest::Set {
            layer: ConfigurationLayerIdV1::UserProfile {
                profile_id: resolved.profile_id.clone(),
            },
            key: SettingKey::new(USER_UPLOAD_ENABLED_SETTING_KEY)
                .map_err(|error| configuration_error(error.to_string()))?,
            value: ConfigurationValueV1::Boolean(enable),
        }]
    } else {
        Vec::new()
    };
    let receipt = mutate_user_configuration(
        &resolved.project_path,
        &resolved.profile_id,
        expected_revision,
        mutations,
    )
    .await?;
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

pub(crate) async fn handle_gitignore(
    path: Option<String>,
    action: Option<String>,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(path);
    match action.as_deref() {
        Some("on") => {
            let resolved = super::scope::resolve_project_scope(project_path).await?;
            let expected_revision = current_configuration_revision(&resolved.project_path).await?;
            let current = current_project_setting(
                &resolved.project_path,
                tracedecay_domain::configuration::INDEX_GIT_IGNORE_SETTING_KEY,
            )
            .await?;
            let mutations = (current != ConfigurationValueV1::Boolean(true))
                .then(|| {
                    project_configuration_set(
                        &resolved.project_id,
                        tracedecay_domain::configuration::INDEX_GIT_IGNORE_SETTING_KEY,
                        ConfigurationValueV1::Boolean(true),
                    )
                })
                .transpose()?
                .into_iter()
                .collect();
            let receipt = mutate_project_configuration(
                &resolved.project_path,
                &resolved.project_id,
                expected_revision,
                mutations,
            )
            .await?;
            eprintln!("gitignore enabled — .gitignore rules will be respected during indexing.");
            eprintln!("Run `tracedecay sync` to re-index with the new setting.");
            report_configuration_receipt(receipt.as_ref());
        }
        Some("off") => {
            let resolved = super::scope::resolve_project_scope(project_path).await?;
            let expected_revision = current_configuration_revision(&resolved.project_path).await?;
            let current = current_project_setting(
                &resolved.project_path,
                tracedecay_domain::configuration::INDEX_GIT_IGNORE_SETTING_KEY,
            )
            .await?;
            let mutations = (current != ConfigurationValueV1::Boolean(false))
                .then(|| {
                    project_configuration_set(
                        &resolved.project_id,
                        tracedecay_domain::configuration::INDEX_GIT_IGNORE_SETTING_KEY,
                        ConfigurationValueV1::Boolean(false),
                    )
                })
                .transpose()?
                .into_iter()
                .collect();
            let receipt = mutate_project_configuration(
                &resolved.project_path,
                &resolved.project_id,
                expected_revision,
                mutations,
            )
            .await?;
            eprintln!("gitignore disabled — .gitignore rules will be ignored during indexing.");
            eprintln!("Run `tracedecay sync` to re-index with the new setting.");
            report_configuration_receipt(receipt.as_ref());
        }
        Some(other) => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!("unknown action '{other}': expected 'on' or 'off'"),
            });
        }
        None => {
            let resolved = super::scope::resolve_project_scope(project_path).await?;
            let response = daemon_tool_json(
                Some(&resolved.project_path),
                "tracedecay_admin_project",
                serde_json::json!({ "action": "gitignore_status" }),
            )
            .await?;
            let enabled = response
                .get("git_ignore")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: "daemon gitignore status omitted git_ignore".to_string(),
                })?;
            let status = if enabled { "on" } else { "off" };
            eprintln!("gitignore: {status}");
        }
    }
    Ok(())
}
