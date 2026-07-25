use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use tracedecay::agents::host_registration::{
    CompatibilityAgentRegistrationDelegate, project_local_registration_path,
};
use tracedecay::automation::config::{
    AutomationBackend, AutomationConfigPatch, AutomationHostMode, AutomationTaskPatch,
    apply_project_config_patch, load_project_config, project_config_path,
};

/// How `install --agent codex --automation` should configure the daemon loop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodexAutomationInstall {
    /// Apply accepted memory-curation ops without dashboard approval
    /// (`--auto-apply`).
    pub(crate) auto_apply: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostBundleCliOperation {
    Install,
    Update,
    Repair,
    Uninstall,
}

pub(crate) async fn handle_host_bundle_component_command(
    agent: Option<String>,
    operation: HostBundleCliOperation,
    options: crate::cli::HostBundleCliOptions,
) -> tracedecay::errors::Result<()> {
    if options.component.is_some() && !options.dry_run && !options.yes {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "host component mutation requires --yes; use --dry-run first".to_string(),
        });
    }
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    let mut user_config = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_config);
    let agent_ids = match agent {
        Some(agent) => vec![agent],
        None if operation == HostBundleCliOperation::Install => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "component install requires --agent".to_string(),
            });
        }
        None => user_config.installed_agents.clone(),
    };
    if agent_ids.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "no installed agents are tracked for component lifecycle".to_string(),
        });
    }
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    for agent_id in &agent_ids {
        let component_set = canonical_host_component_set(agent_id, options.component, now_unix)?
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "agent {agent_id:?} has no canonical first-party host component set"
                ),
            })?;
        if options.dry_run {
            dry_run_canonical_component_set(
                agent_id,
                operation,
                &component_set,
                &options,
                &home,
                &lifecycle_root,
            )?;
        } else {
            apply_canonical_component_set(
                agent_id,
                operation,
                &component_set,
                &options,
                &home,
                &lifecycle_root,
            )?;
        }
    }
    if operation == HostBundleCliOperation::Install && !options.dry_run {
        for agent_id in agent_ids {
            if !user_config.installed_agents.contains(&agent_id) {
                user_config.installed_agents.push(agent_id);
            }
        }
        user_config
            .save()
            .map_err(|error| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {error}"),
            })?;
    }
    Ok(())
}

fn canonical_host_component_set(
    agent: &str,
    component: Option<crate::cli::HostBundleComponentArg>,
    now_unix: u64,
) -> tracedecay::errors::Result<
    Option<tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1>,
> {
    let host = match host_kind_for_agent(agent) {
        Ok(host) => host,
        Err(_) if component.is_none() => return Ok(None),
        Err(error) => return Err(error),
    };
    let requested = component.map(host_bundle_component).map_or_else(
        || tracedecay::agents::host_bundle_registry::default_components(host),
        |component| vec![component],
    );
    if requested.is_empty() {
        return Ok(None);
    }
    tracedecay::agents::host_bundle_registry::verified_embedded_host_component_set(
        host, &requested, now_unix,
    )
    .map(Some)
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("first-party {agent:?} component set is unavailable: {error}"),
    })
}

fn lifecycle_operation(
    operation: HostBundleCliOperation,
) -> tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1 {
    match operation {
        HostBundleCliOperation::Install => {
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install
        }
        HostBundleCliOperation::Update => {
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
        }
        HostBundleCliOperation::Repair => {
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair
        }
        HostBundleCliOperation::Uninstall => {
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall
        }
    }
}

fn component_set_request(
    component_set: &tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    operation: HostBundleCliOperation,
    explicit_confirmation: bool,
) -> tracedecay::errors::Result<
    tracedecay::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
> {
    let mut operation_id = [0_u8; 16];
    getrandom::getrandom(&mut operation_id).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("could not generate host lifecycle operation id: {error}"),
        }
    })?;
    let host = component_set.component_set.host;
    Ok(
        tracedecay::agents::host_bundle_v2::HostComponentSetExecutionRequestV1 {
            lifecycle: tracedecay::agents::host_bundle_v2::HostComponentSetLifecycleRequestV1 {
                operation: lifecycle_operation(operation),
                expected_host: host,
                expected_components: component_set
                    .component_set
                    .components
                    .iter()
                    .map(|component| component.manifest.component)
                    .collect(),
                explicit_confirmation,
                hermes_profile_bindings: u8::from(
                    host == tracedecay::agents::host_bundle_v2::HostKindV1::Hermes,
                ),
            },
            operation_id,
        },
    )
}

fn dry_run_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    component_set: &tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
) -> tracedecay::errors::Result<()> {
    let request = component_set_request(component_set, operation, options.yes)?;
    let mut registration = CompatibilityAgentRegistrationDelegate::new(
        agent_id,
        home,
        lifecycle_root,
        request.lifecycle.operation,
    )?;
    let preview = tracedecay::agents::host_bundle_v2::dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
            home,
            lifecycle_root,
            &component_set.component_set,
            &request,
            component_set,
            &mut registration,
        )
        .map_err(host_bundle_error)?;
    eprintln!(
        "{} {:?}: plan={}, registration_base={}, registration_current={}, artifacts={}, confirmation={}",
        agent_id,
        request.lifecycle.operation,
        hex::encode(preview.plan_digest),
        hex::encode(preview.base_registration_revision),
        hex::encode(preview.current_registration_revision),
        hex::encode(preview.artifact_state_revision),
        preview.confirmation_required
    );
    for plan in preview.component_plans {
        eprintln!(
            "  {:?}: {} mutation(s), rollback={}",
            plan.component,
            plan.mutations.len(),
            plan.rollback_required
        );
        for mutation in plan.mutations {
            eprintln!("  {:?} {}", mutation.action, mutation.relative_path);
        }
    }
    Ok(())
}

fn apply_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    component_set: &tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
) -> tracedecay::errors::Result<()> {
    let request = component_set_request(
        component_set,
        operation,
        options.component.is_none() || options.yes,
    )?;
    let mut writer =
        tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
            home,
            lifecycle_root,
        )
        .map_err(host_bundle_error)?;
    let mut transaction =
        tracedecay::agents::host_bundle_v2::HostComponentSetTransactionV1::new(&mut writer);
    let mut registration = CompatibilityAgentRegistrationDelegate::new(
        agent_id,
        home,
        lifecycle_root,
        request.lifecycle.operation,
    )?;
    let preview = transaction
        .preview(
            &component_set.component_set,
            &request,
            component_set,
            &mut registration,
        )
        .map_err(host_bundle_error)?;
    let receipt = transaction
        .execute_confirmed(
            &component_set.component_set,
            &request,
            &preview,
            component_set,
            &mut registration,
        )
        .map_err(host_bundle_error)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m {} {:?}: {} component(s), receipt {}",
        agent_id,
        request.lifecycle.operation,
        receipt.component_receipts.len(),
        hex::encode(receipt.operation_id)
    );
    Ok(())
}

fn apply_default_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    home: &Path,
) -> tracedecay::errors::Result<bool> {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    let Some(component_set) = canonical_host_component_set(agent_id, None, now_unix)? else {
        return Ok(false);
    };
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    apply_canonical_component_set(
        agent_id,
        operation,
        &component_set,
        &crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
        },
        home,
        &lifecycle_root,
    )?;
    Ok(true)
}

fn apply_project_local_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    project_path: &Path,
    home: &Path,
) -> tracedecay::errors::Result<bool> {
    if project_local_registration_path(agent_id, home, project_path).is_none() {
        return Ok(false);
    }
    let host = host_kind_for_agent(agent_id)?;
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    let component_set =
        tracedecay::agents::host_bundle_registry::verified_embedded_project_host_component_set(
            host, agent_id, now_unix,
        )
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("project-local {agent_id:?} component set is unavailable: {error}"),
        })?;
    let lifecycle_root = project_path.join(".tracedecay/host-lifecycle");
    let request = component_set_request(&component_set, operation, true)?;
    let mut writer =
        tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
            project_path,
            &lifecycle_root,
        )
        .map_err(host_bundle_error)?;
    let mut registration = CompatibilityAgentRegistrationDelegate::new_project_local(
        agent_id,
        home,
        project_path,
        &lifecycle_root,
        request.lifecycle.operation,
    )?;
    let mut transaction =
        tracedecay::agents::host_bundle_v2::HostComponentSetTransactionV1::new(&mut writer);
    let preview = transaction
        .preview(
            &component_set.component_set,
            &request,
            &component_set,
            &mut registration,
        )
        .map_err(host_bundle_error)?;
    let receipt = transaction
        .execute_confirmed(
            &component_set.component_set,
            &request,
            &preview,
            &component_set,
            &mut registration,
        )
        .map_err(host_bundle_error)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m {} {:?} project-local: {} component(s), receipt {}",
        agent_id,
        request.lifecycle.operation,
        receipt.component_receipts.len(),
        hex::encode(receipt.operation_id)
    );
    Ok(true)
}

pub(crate) async fn handle_project_local_lifecycle_command(
    agent_id: String,
    operation: HostBundleCliOperation,
) -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let project_path =
        std::env::current_dir().map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not determine current project directory: {error}"),
        })?;
    if !apply_project_local_component_set(&agent_id, operation, &project_path, &home)? {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("agent {agent_id:?} has no atomic project-local lifecycle route"),
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FeedbackRollbackCliStatus {
    Prepared,
    Applied,
    Restored,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeedbackRollbackCliState {
    schema_version: u16,
    agent_id: String,
    host: tracedecay::agents::host_bundle_v2::HostKindV1,
    status: FeedbackRollbackCliStatus,
    previous_manifest: tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    previous_contents: Vec<tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1>,
    target_manifest: tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    switch_receipt: Option<tracedecay::agents::host_bundle_v2::FeedbackPathRollbackReceiptV1>,
}

#[derive(Clone)]
struct FeedbackPairVerifier {
    digests: [[u8; 32]; 2],
}

impl tracedecay::agents::host_bundle_v2::HostBundleVerificationAdapterV1 for FeedbackPairVerifier {
    fn verify_manifest(
        &self,
        manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    ) -> Result<(), tracedecay::agents::host_bundle_v2::HostBundleError> {
        manifest.validate_structure()?;
        self.digests
            .contains(&manifest.canonical_digest()?)
            .then_some(())
            .ok_or(tracedecay::agents::host_bundle_v2::HostBundleError::CatalogMismatch)
    }
}

struct FeedbackPreviewStorage;

impl tracedecay::agents::host_bundle_v2::HostBundleLifecycleStorageV1 for FeedbackPreviewStorage {
    fn recover_lifecycle(
        &mut self,
    ) -> Result<(), tracedecay::agents::host_bundle_v2::HostBundleError> {
        Ok(())
    }

    fn execute_lifecycle<V: tracedecay::agents::host_bundle_v2::HostBundleVerificationAdapterV1>(
        &mut self,
        _manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
        _request: &tracedecay::agents::host_bundle_v2::HostBundleExecutionRequestV1,
        _contents: &[tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1],
        _verifier: &V,
    ) -> Result<
        tracedecay::agents::host_bundle_v2::HostBundleInstallReceiptV1,
        tracedecay::agents::host_bundle_v2::HostBundleError,
    > {
        Err(tracedecay::agents::host_bundle_v2::HostBundleError::StorageFailure)
    }
}

pub(crate) async fn handle_feedback_rollback_command(
    action: crate::cli::FeedbackRollbackAction,
) -> tracedecay::errors::Result<()> {
    match action {
        crate::cli::FeedbackRollbackAction::DryRun { agent } => feedback_rollback_dry_run(&agent),
        crate::cli::FeedbackRollbackAction::Apply { agent, state, yes } => {
            if !yes {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "feedback rollback apply requires --yes".to_string(),
                });
            }
            feedback_rollback_apply(&agent, Path::new(&state))
        }
        crate::cli::FeedbackRollbackAction::Restore { state, yes } => {
            if !yes {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "feedback rollback restore requires --yes".to_string(),
                });
            }
            feedback_rollback_restore(Path::new(&state))
        }
    }
}

fn feedback_rollback_inputs(
    agent_id: &str,
) -> tracedecay::errors::Result<(
    PathBuf,
    PathBuf,
    tracedecay::agents::host_bundle_v2::HostComponentSetReceiptV1,
    tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostBundleV1,
)> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    let host = host_kind_for_agent(agent_id)?;
    let previous = tracedecay::agents::host_bundle_v2::latest_host_component_set_receipt_at(
        &lifecycle_root,
        host,
    )
    .map_err(host_bundle_error)?
    .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
        message: format!("no aggregate host receipt exists for {agent_id}"),
    })?;
    let target = tracedecay::agents::host_bundle_registry::verified_embedded_host_bundle(
        host,
        tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core,
        0,
    )
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("compiled feedback route is unavailable for {agent_id}: {error}"),
    })?;
    Ok((home, lifecycle_root, previous, target))
}

fn feedback_core_receipt(
    aggregate: &tracedecay::agents::host_bundle_v2::HostComponentSetReceiptV1,
) -> tracedecay::errors::Result<(
    tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    tracedecay::agents::host_bundle_v2::HostBundleInstallReceiptV1,
)> {
    let component = tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core;
    let manifest = aggregate
        .component_manifests
        .iter()
        .find(|manifest| manifest.component == component)
        .cloned()
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "aggregate receipt has no Core feedback manifest".to_string(),
        })?;
    let receipt = aggregate
        .component_receipts
        .iter()
        .find(|receipt| receipt.component == component)
        .cloned()
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "aggregate receipt has no Core component receipt".to_string(),
        })?;
    Ok((manifest, receipt))
}

fn feedback_pair_verifier(
    previous: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    target: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
) -> tracedecay::errors::Result<FeedbackPairVerifier> {
    Ok(FeedbackPairVerifier {
        digests: [
            previous.canonical_digest().map_err(host_bundle_error)?,
            target.canonical_digest().map_err(host_bundle_error)?,
        ],
    })
}

fn feedback_request(
    manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    operation: tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    confirmed: bool,
) -> tracedecay::errors::Result<tracedecay::agents::host_bundle_v2::HostBundleExecutionRequestV1> {
    let mut operation_id = [0; 16];
    getrandom::getrandom(&mut operation_id).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("could not generate feedback rollback operation id: {error}"),
        }
    })?;
    Ok(
        tracedecay::agents::host_bundle_v2::HostBundleExecutionRequestV1 {
            lifecycle: tracedecay::agents::host_bundle_v2::HostBundleLifecycleRequestV1 {
                operation,
                expected_host: manifest.host,
                expected_component: manifest.component,
                explicit_confirmation: confirmed,
                hermes_profile_bindings: u8::from(
                    manifest.host == tracedecay::agents::host_bundle_v2::HostKindV1::Hermes,
                ),
            },
            operation_id,
        },
    )
}

fn feedback_observed(
    home: &Path,
    target: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    previous_receipt: &tracedecay::agents::host_bundle_v2::HostBundleInstallReceiptV1,
) -> tracedecay::errors::Result<(
    Vec<tracedecay::agents::host_bundle_v2::ObservedHostArtifactV1>,
    Vec<tracedecay::agents::host_bundle_v2::ObservedHostArtifactV1>,
)> {
    use tracedecay::agents::host_bundle_v2::{ObservedArtifactKindV1, ObservedHostArtifactV1};

    let observe =
        |relative_path: &str,
         owned: Option<&tracedecay::agents::host_bundle_v2::HostBundleReceiptArtifactV1>,
         cataloged_ownership_marker: Option<String>|
         -> tracedecay::errors::Result<ObservedHostArtifactV1> {
            let path = tracedecay::agents::host_bundle_v2::inspect_install_target(
                home,
                Path::new(relative_path),
            )
            .map_err(host_bundle_error)?;
            let (kind, artifact_digest) = match fs::read(&path) {
                Ok(bytes) => (
                    ObservedArtifactKindV1::RegularFile,
                    Some(Sha256::digest(bytes).into()),
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    (ObservedArtifactKindV1::Missing, None)
                }
                Err(_) => {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: format!("could not inspect feedback artifact {}", path.display()),
                    });
                }
            };
            Ok(ObservedHostArtifactV1 {
                relative_path: relative_path.to_string(),
                kind,
                artifact_digest,
                ownership_marker: owned.map(|owned| owned.ownership_marker.clone()),
                owned_artifact_digest: owned.map(|owned| owned.artifact_digest),
                cataloged_ownership_marker,
            })
        };

    let manifest_observed = target
        .artifacts
        .iter()
        .map(|artifact| {
            let owned = previous_receipt
                .artifacts
                .iter()
                .find(|owned| owned.relative_path == artifact.relative_path);
            observe(
                &artifact.relative_path,
                owned,
                Some(artifact.ownership_marker.clone()),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let orphan_observed = previous_receipt
        .artifacts
        .iter()
        .filter(|owned| {
            !target
                .artifacts
                .iter()
                .any(|artifact| artifact.relative_path == owned.relative_path)
        })
        .map(|owned| observe(&owned.relative_path, Some(owned), None))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((manifest_observed, orphan_observed))
}

fn read_feedback_contents(
    home: &Path,
    manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
) -> tracedecay::errors::Result<Vec<tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1>>
{
    manifest
        .artifacts
        .iter()
        .map(|artifact| {
            let path = tracedecay::agents::host_bundle_v2::inspect_install_target(
                home,
                Path::new(&artifact.relative_path),
            )
            .map_err(host_bundle_error)?;
            let bytes =
                fs::read(&path).map_err(|error| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "could not snapshot feedback artifact {}: {error}",
                        path.display()
                    ),
                })?;
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            if digest != artifact.artifact_digest {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "feedback artifact {} no longer matches its ownership receipt",
                        path.display()
                    ),
                });
            }
            Ok(
                tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1 {
                    relative_path: artifact.relative_path.clone(),
                    bytes,
                },
            )
        })
        .collect()
}

fn write_feedback_state(
    path: &Path,
    state: &FeedbackRollbackCliState,
) -> tracedecay::errors::Result<()> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("could not serialize feedback rollback state: {error}"),
        }
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not create feedback state directory: {error}"),
    })?;
    let temporary = path.with_extension("json.new");
    fs::write(&temporary, bytes).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not stage feedback rollback state: {error}"),
    })?;
    fs::rename(&temporary, path).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not publish feedback rollback state: {error}"),
    })
}

fn feedback_doctor_state_path(lifecycle_root: &Path, agent_id: &str) -> PathBuf {
    lifecycle_root
        .join(".tracedecay-host-bundle-v1")
        .join(format!("feedback-rollback.{agent_id}.v1.json"))
}

fn persist_feedback_state(
    state_path: &Path,
    lifecycle_root: &Path,
    state: &FeedbackRollbackCliState,
) -> tracedecay::errors::Result<()> {
    write_feedback_state(state_path, state)?;
    let doctor_path = feedback_doctor_state_path(lifecycle_root, &state.agent_id);
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1,
        "agent_id": state.agent_id,
        "host": state.host,
        "status": state.status,
        "state_path": state_path,
    }))
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not serialize feedback Doctor state: {error}"),
    })?;
    let parent = doctor_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not create feedback Doctor state directory: {error}"),
    })?;
    let temporary = doctor_path.with_extension("json.new");
    fs::write(&temporary, bytes).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not stage feedback Doctor state: {error}"),
    })?;
    fs::rename(&temporary, doctor_path).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("could not publish feedback Doctor state: {error}"),
        }
    })
}

fn feedback_rollback_dry_run(agent_id: &str) -> tracedecay::errors::Result<()> {
    let (home, _lifecycle_root, aggregate, target) = feedback_rollback_inputs(agent_id)?;
    let (previous, previous_receipt) = feedback_core_receipt(&aggregate)?;
    let verifier = feedback_pair_verifier(&previous, &target.manifest)?;
    let request = feedback_request(
        &target.manifest,
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update,
        false,
    )?;
    let (manifest_observed, orphan_observed) =
        feedback_observed(&home, &target.manifest, &previous_receipt)?;
    let lifecycle = tracedecay::agents::host_bundle_v2::HostBundleLifecycleRuntimeV1::new(
        verifier,
        FeedbackPreviewStorage,
    );
    let rollback = tracedecay::agents::host_bundle_v2::FeedbackPathRollbackSwitchV1::new(lifecycle);
    let preview = rollback
        .feedback_rollback_switch_dry_run(
            &previous,
            &target.manifest,
            &request,
            &manifest_observed,
            Some(&previous_receipt),
            &orphan_observed,
            &[],
        )
        .map_err(host_bundle_error)?;
    println!(
        "{agent_id} feedback rollback: {} mutation(s), rollback={}, confirmation={}",
        preview.plan.mutations.len(),
        preview.plan.rollback_required,
        preview.confirmation_required
    );
    for mutation in preview.plan.mutations {
        println!("  {:?} {}", mutation.action, mutation.relative_path);
    }
    Ok(())
}

fn feedback_rollback_apply(agent_id: &str, state_path: &Path) -> tracedecay::errors::Result<()> {
    let (home, lifecycle_root, aggregate, target) = feedback_rollback_inputs(agent_id)?;
    let (previous, _previous_receipt) = feedback_core_receipt(&aggregate)?;
    let previous_contents = read_feedback_contents(&home, &previous)?;
    let verifier = feedback_pair_verifier(&previous, &target.manifest)?;
    let request = feedback_request(
        &target.manifest,
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update,
        true,
    )?;
    let mut state = FeedbackRollbackCliState {
        schema_version: 1,
        agent_id: agent_id.to_string(),
        host: target.manifest.host,
        status: FeedbackRollbackCliStatus::Prepared,
        previous_manifest: previous.clone(),
        previous_contents,
        target_manifest: target.manifest.clone(),
        switch_receipt: None,
    };
    persist_feedback_state(state_path, &lifecycle_root, &state)?;

    let writer = tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
        &home,
        &lifecycle_root,
    )
    .map_err(host_bundle_error)?;
    let lifecycle =
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleRuntimeV1::new(verifier, writer);
    let mut rollback =
        tracedecay::agents::host_bundle_v2::FeedbackPathRollbackSwitchV1::new(lifecycle);
    let switch_receipt = rollback
        .feedback_rollback_switch_apply(
            &previous,
            &target.manifest,
            &request,
            &target.contents,
            &[],
        )
        .map_err(host_bundle_error)?;
    state.switch_receipt = Some(switch_receipt.clone());
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    let lifecycle = rollback.into_lifecycle();
    let writer = lifecycle.into_storage();

    let integration = tracedecay::agents::get_integration(agent_id)?;
    let context = tracedecay::agents::InstallContext {
        home: home.clone(),
        tracedecay_bin: tracedecay::agents::which_tracedecay()
            .unwrap_or_else(|| "tracedecay".to_string()),
        tool_permissions: tracedecay::agents::expected_tool_perms(),
        project_root: None,
        dashboard: true,
    };
    let registration_result = integration
        .activate_deployed_host_registration(&context)
        .and_then(|()| {
            let health = tracedecay::agents::HealthcheckContext {
                home: home.clone(),
                project_path: std::env::current_dir().unwrap_or_else(|_| home.clone()),
            };
            (integration.host_component_registration(
                tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core,
                &health,
            ) == tracedecay::agents::host_bundle_v2::HostBundleRegistrationStateV1::Current)
                .then_some(())
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "{agent_id} did not verify its activated Core feedback registration"
                    ),
                })
        });
    if let Err(registration_error) = registration_result {
        let verifier = feedback_pair_verifier(&previous, &target.manifest)?;
        let restore_request = feedback_request(
            &previous,
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair,
            true,
        )?;
        let lifecycle =
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleRuntimeV1::new(verifier, writer);
        let mut rollback =
            tracedecay::agents::host_bundle_v2::FeedbackPathRollbackSwitchV1::new(lifecycle);
        let restore = rollback
            .feedback_rollback_switch_restore(
                &switch_receipt,
                &previous,
                &restore_request,
                &state.previous_contents,
                &[],
            )
            .map_err(host_bundle_error)?;
        let writer = rollback.into_lifecycle().into_storage();
        writer
            .publish_feedback_component_set_receipt(&previous, &restore.restore_receipt)
            .map_err(host_bundle_error)?;
        state.status = FeedbackRollbackCliStatus::Restored;
        persist_feedback_state(state_path, &lifecycle_root, &state)?;
        return Err(registration_error);
    }
    writer
        .publish_feedback_component_set_receipt(&target.manifest, &switch_receipt.apply_receipt)
        .map_err(host_bundle_error)?;

    state.status = FeedbackRollbackCliStatus::Applied;
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    println!(
        "\x1b[32m✔\x1b[0m {agent_id} feedback rollback applied; state {}",
        state_path.display()
    );
    Ok(())
}

fn feedback_rollback_restore(state_path: &Path) -> tracedecay::errors::Result<()> {
    let bytes =
        fs::read(state_path).map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "could not read feedback rollback state {}: {error}",
                state_path.display()
            ),
        })?;
    let mut state: FeedbackRollbackCliState = serde_json::from_slice(&bytes).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("invalid feedback rollback state: {error}"),
        }
    })?;
    if state.schema_version != 1 {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "unsupported feedback rollback state version".to_string(),
        });
    }
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    let writer = tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
        &home,
        &lifecycle_root,
    )
    .map_err(host_bundle_error)?;
    let Some(switch_receipt) = state.switch_receipt.clone() else {
        state.status = FeedbackRollbackCliStatus::Restored;
        persist_feedback_state(state_path, &lifecycle_root, &state)?;
        return Ok(());
    };
    let verifier = feedback_pair_verifier(&state.previous_manifest, &state.target_manifest)?;
    let request = feedback_request(
        &state.previous_manifest,
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair,
        true,
    )?;
    let lifecycle =
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleRuntimeV1::new(verifier, writer);
    let mut rollback =
        tracedecay::agents::host_bundle_v2::FeedbackPathRollbackSwitchV1::new(lifecycle);
    let restore = rollback
        .feedback_rollback_switch_restore(
            &switch_receipt,
            &state.previous_manifest,
            &request,
            &state.previous_contents,
            &[],
        )
        .map_err(host_bundle_error)?;
    let lifecycle = rollback.into_lifecycle();
    let writer = lifecycle.into_storage();
    let integration = tracedecay::agents::get_integration(&state.agent_id)?;
    let context = tracedecay::agents::InstallContext {
        home,
        tracedecay_bin: tracedecay::agents::which_tracedecay()
            .unwrap_or_else(|| "tracedecay".to_string()),
        tool_permissions: tracedecay::agents::expected_tool_perms(),
        project_root: None,
        dashboard: true,
    };
    integration.activate_deployed_host_registration(&context)?;
    let health = tracedecay::agents::HealthcheckContext {
        home: context.home.clone(),
        project_path: std::env::current_dir().unwrap_or_else(|_| context.home.clone()),
    };
    if integration.host_component_registration(
        tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core,
        &health,
    ) != tracedecay::agents::host_bundle_v2::HostBundleRegistrationStateV1::Current
    {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "{} did not verify its restored Core feedback registration",
                state.agent_id
            ),
        });
    }
    writer
        .publish_feedback_component_set_receipt(&state.previous_manifest, &restore.restore_receipt)
        .map_err(host_bundle_error)?;
    state.status = FeedbackRollbackCliStatus::Restored;
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    println!(
        "\x1b[32m✔\x1b[0m {} feedback route restored; state {}",
        state.agent_id,
        state_path.display()
    );
    Ok(())
}

fn host_bundle_component(
    component: crate::cli::HostBundleComponentArg,
) -> tracedecay::agents::host_bundle_v2::HostBundleComponentV1 {
    match component {
        crate::cli::HostBundleComponentArg::Core => {
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core
        }
        crate::cli::HostBundleComponentArg::Agent => {
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Agent
        }
        crate::cli::HostBundleComponentArg::ContextMcp => {
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp
        }
        crate::cli::HostBundleComponentArg::OperatorMcp => {
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::OperatorMcp
        }
    }
}

/// Inspect or recover an interrupted first-party host component transaction.
///
/// This is the supported replacement for hand-deleting
/// `~/.tracedecay/host-components/.tracedecay-host-bundle-v1/component-set-journal.*.json`,
/// which used to be the only way out of a wedged host lifecycle.
pub(crate) async fn handle_host_bundle_recovery_command(
    action: crate::cli::HostBundleAction,
    dry_run: bool,
    yes: bool,
) -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    let mut writer =
        tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
            &home,
            &lifecycle_root,
        )
        .map_err(host_bundle_error)?;

    let (selected_agent, quarantine, status_only) = match action {
        crate::cli::HostBundleAction::Status => (None, false, true),
        crate::cli::HostBundleAction::Recover { agent, quarantine } => (agent, quarantine, dry_run),
    };

    let mut pending = writer
        .pending_component_set_journal_hosts()
        .map_err(host_bundle_error)?;
    if let Some(agent) = selected_agent.as_deref() {
        let host = host_kind_for_agent(agent)?;
        pending.retain(|pending_host| *pending_host == host);
    }
    if pending.is_empty() {
        eprintln!("\x1b[32m✔\x1b[0m no host component lifecycle journal is awaiting recovery");
        return Ok(());
    }
    for host in &pending {
        eprintln!(
            "  pending: {} ({:?})",
            tracedecay::agents::integration_id_for_host(*host),
            host
        );
    }
    if status_only {
        return Ok(());
    }
    if !yes {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "host component recovery mutates deployed files; re-run with --yes"
                .to_string(),
        });
    }

    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    for host in pending {
        let agent_id = tracedecay::agents::integration_id_for_host(host);
        let mut registration = CompatibilityAgentRegistrationDelegate::new(
            agent_id,
            &home,
            &lifecycle_root,
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair,
        )?;
        let outcome =
            tracedecay::agents::host_bundle_v2::HostComponentSetTransactionV1::new(&mut writer)
                .recover_host(host, &mut registration);
        match outcome {
            Ok(()) => eprintln!("\x1b[32m✔\x1b[0m {agent_id}: lifecycle journal recovered"),
            Err(error) if quarantine => {
                let quarantined = writer
                    .quarantine_component_set_journal(host, now_unix)
                    .map_err(host_bundle_error)?;
                match quarantined {
                    Some(path) => eprintln!(
                        "\x1b[33m!\x1b[0m {agent_id}: {error}; journal quarantined at {} (rollback backups preserved)",
                        path.display()
                    ),
                    None => eprintln!("\x1b[32m✔\x1b[0m {agent_id}: lifecycle journal cleared"),
                }
            }
            Err(error) => return Err(host_bundle_error(error)),
        }
    }
    Ok(())
}

fn host_kind_for_agent(
    agent: &str,
) -> tracedecay::errors::Result<tracedecay::agents::host_bundle_v2::HostKindV1> {
    use tracedecay::agents::host_bundle_v2::HostKindV1;

    match agent {
        "claude" => Ok(HostKindV1::ClaudeCode),
        "cursor" => Ok(HostKindV1::CursorDesktop),
        "codex" => Ok(HostKindV1::Codex),
        "hermes" => Ok(HostKindV1::Hermes),
        "kiro" => Ok(HostKindV1::Kiro),
        "cline" => Ok(HostKindV1::Cline),
        "roo-code" => Ok(HostKindV1::RooCode),
        "kilo" => Ok(HostKindV1::Kilo),
        "kimi" => Ok(HostKindV1::KimiCode),
        "opencode" => Ok(HostKindV1::OpenCode),
        _ => Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("agent {agent:?} has no embedded first-party host component"),
        }),
    }
}

fn host_bundle_error(
    error: tracedecay::agents::host_bundle_v2::HostBundleError,
) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: format!("host bundle lifecycle failed: {error}"),
    }
}

fn validate_codex_automation_flags(
    agent: Option<&str>,
    automation: Option<CodexAutomationInstall>,
) -> tracedecay::errors::Result<()> {
    if automation.is_none() {
        return Ok(());
    }
    if agent != Some("codex") {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "`--automation` is only supported with `--agent codex`".to_string(),
        });
    }
    Ok(())
}

fn validate_codex_automation_project_path() -> tracedecay::errors::Result<PathBuf> {
    let project_path =
        std::env::current_dir().map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not determine current project directory: {e}"),
        })?;
    std::fs::canonicalize(&project_path).map_err(|e| tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "could not canonicalize project directory {}: {e}",
            project_path.display()
        ),
    })
}

async fn install_codex_daemon_automation(
    project_path: &Path,
    home: &Path,
    options: CodexAutomationInstall,
) -> tracedecay::errors::Result<PathBuf> {
    let auto_apply = options.auto_apply;
    if tracedecay::agents::codex::remove_legacy_codex_native_automation(home)? {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed the legacy Codex-native scheduled automation; the TraceDecay daemon loop replaces it."
        );
    }

    let dashboard_root = open_or_init_codex_daemon_automation_project(project_path).await?;
    let patch = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        host_mode: Some(AutomationHostMode::Standalone),
        // Unattended memory-op apply is opt-in: without --auto-apply these
        // stays unset, and re-running the installer never weakens stricter
        // settings a user already chose.
        auto_apply_memory_ops: auto_apply.then_some(true),
        memory_curator: codex_daemon_interval_task(15 * 60),
        session_reflector: codex_daemon_interval_task(15 * 60),
        skill_writer: AutomationTaskPatch {
            min_idle_secs: Some(Some(15 * 60)),
            ..codex_daemon_interval_task(60 * 60)
        },
        ..AutomationConfigPatch::default()
    };

    let global = tracedecay::user_config::UserConfig::load().automation;
    let current = load_project_config(&dashboard_root).await?;
    let (updated, _) = apply_project_config_patch(&dashboard_root, &global, patch).await?;
    if crate::automation_cli::config::automation_config_changed(current.as_ref(), &updated) {
        crate::automation_cli::config::notify_project_automation_scheduler(project_path).await?;
    }
    let path = project_config_path(&dashboard_root);
    eprintln!(
        "\x1b[32m✔\x1b[0m Enabled TraceDecay daemon automation loop at {}",
        path.display()
    );
    eprintln!(
        "  The daemon scheduler will run memory_curator, session_reflector, and skill_writer via the Codex app-server backend."
    );
    if auto_apply {
        eprintln!(
            "\x1b[33m⚠\x1b[0m --auto-apply: accepted memory-curation ops (deletes and merges) will be applied without dashboard approval. There is no archive; removals are permanent."
        );
    }
    if !tracedecay::daemon::daemon_reachable() {
        eprintln!(
            "\x1b[33m⚠\x1b[0m The TraceDecay daemon is not running, so the automation loop will stay idle. Enable it with `tracedecay daemon install-service`."
        );
    }
    Ok(path)
}

async fn open_or_init_codex_daemon_automation_project(
    project_path: &Path,
) -> tracedecay::errors::Result<PathBuf> {
    broker_codex_daemon_automation_project(
        project_path,
        |handshake| async move {
            tracedecay::daemon::call_default_tool(
                &handshake,
                "tracedecay_admin_project",
                serde_json::json!({"action": "counter_get"}),
            )
            .await
            .map(|_| ())
        },
        |project_path| {
            tracedecay::storage::resolve_layout_for_current_profile(project_path)
                .map(|layout| layout.dashboard_root)
        },
    )
    .await
}

async fn broker_codex_daemon_automation_project<I, IFut, R>(
    project_path: &Path,
    initialize: I,
    resolve_dashboard_root: R,
) -> tracedecay::errors::Result<PathBuf>
where
    I: FnOnce(tracedecay::daemon::DaemonHandshake) -> IFut,
    IFut: std::future::Future<Output = tracedecay::errors::Result<()>>,
    R: FnOnce(&Path) -> tracedecay::errors::Result<PathBuf>,
{
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(project_path.to_path_buf()),
        None,
        false,
        true,
    )?;
    initialize(handshake).await?;
    resolve_dashboard_root(project_path)
}

fn codex_daemon_interval_task(interval_secs: u64) -> AutomationTaskPatch {
    AutomationTaskPatch {
        enabled: Some(true),
        schedule: Some(Some("interval".to_string())),
        interval_secs: Some(Some(interval_secs)),
        cooldown_secs: Some(Some(5 * 60)),
        ..AutomationTaskPatch::default()
    }
}

/// Moves provable historical Hermes-local session data before any install can
/// remove its legacy project pin. Unresolved sources remain untouched and are
/// reported without blocking the projectless user-profile integration. Read,
/// integrity, or copy failures still block the cutover.
pub(crate) async fn migrate_legacy_hermes_data(home: &Path) -> tracedecay::errors::Result<()> {
    let report = tracedecay::migrate::hermes::migrate_legacy_hermes_stores(home).await;
    finish_legacy_hermes_migration(report)
}

/// Upgrade reinstalls share the same preservation policy while reusing any
/// lifecycle authority already held by post-update maintenance.
async fn migrate_legacy_hermes_data_for_reinstall(
    home: &Path,
    lifecycle: Option<&tracedecay::lifecycle_lease::LifecycleLease>,
) -> tracedecay::errors::Result<()> {
    let report = if let Some(lifecycle) = lifecycle {
        tracedecay::migrate::hermes::migrate_legacy_hermes_stores_under_lease(home, lifecycle).await
    } else {
        tracedecay::migrate::hermes::migrate_legacy_hermes_stores(home).await
    };
    finish_legacy_hermes_migration(report)
}

fn finish_legacy_hermes_migration(
    report: tracedecay::migrate::hermes::LegacyHermesMigrationReport,
) -> tracedecay::errors::Result<()> {
    for migration in report.migrated {
        eprintln!(
            "  \x1b[32m✔\x1b[0m Migrated legacy Hermes session store {} -> {} ({} rows)",
            migration.source_db.display(),
            migration.target_project.display(),
            migration.rows_copied
        );
    }
    for issue in report.unresolved {
        eprintln!(
            "  \x1b[33mwarning:\x1b[0m preserving unresolved legacy Hermes session store {}: {}",
            issue.source_db.display(),
            issue.reason
        );
    }
    if report.failed.is_empty() {
        return Ok(());
    }
    let issues = report
        .failed
        .into_iter()
        .map(|issue| format!("{}: {}", issue.source_db.display(), issue.reason))
        .collect::<Vec<_>>()
        .join("; ");
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "legacy Hermes session data migration failed; source data and project pins were preserved: {issues}"
        ),
    })
}

fn print_legacy_install_guidance(agent_id: &str) {
    if agent_id != "hermes" {
        return;
    }
    eprintln!();
    eprintln!("Setup complete. Next steps:");
    eprintln!("  1. cd into your project and run: tracedecay init");
    eprintln!("  2. Start Hermes — tracedecay plugin tools are now available");
}

fn print_legacy_uninstall_guidance(agent_id: &str) {
    if agent_id != "hermes" {
        return;
    }
    eprintln!();
    eprintln!("Uninstall complete. Tracedecay has been removed from Hermes.");
    eprintln!("Restart Hermes for changes to take effect.");
}

pub(crate) async fn handle_install_command(
    agent: Option<String>,
    local: bool,
    no_dashboard: bool,
    automation: Option<CodexAutomationInstall>,
) -> tracedecay::errors::Result<()> {
    validate_codex_automation_flags(agent.as_deref(), automation)?;
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH. Install it from this repo first:\n  \
                          cargo binstall --git https://github.com/ScriptedAlchemy/tracedecay tracedecay\n  \
                          cargo install --git https://github.com/ScriptedAlchemy/tracedecay tracedecay"
                .to_string(),
        }
    })?;
    if local {
        let project_path =
            std::env::current_dir().map_err(|e| tracedecay::errors::TraceDecayError::Config {
                message: format!("could not determine current project directory: {e}"),
            })?;
        let ctx = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: !no_dashboard,
        };
        let mut installed_names: Vec<String> = Vec::new();

        if let Some(id) = agent {
            let ag = tracedecay::agents::get_integration(&id)?;
            // Agents with an atomic project-local lifecycle route install
            // through the receipt-backed component-set transaction; the rest
            // keep the direct integration path (which itself rejects agents
            // without project-local support).
            if !apply_project_local_component_set(
                &id,
                HostBundleCliOperation::Install,
                &project_path,
                &home,
            )? {
                ag.install_local(&ctx, &project_path)?;
            }
            ag.post_install(Some(&project_path)).await;
            if let Some(options) = automation.filter(|_| id == "codex") {
                let scoped_project_path = validate_codex_automation_project_path()?;
                install_codex_daemon_automation(&scoped_project_path, &home, options).await?;
            }
            installed_names.push(ag.name().to_string());
        } else {
            let (to_install, _) = tracedecay::agents::pick_integrations_interactive(&home, &[])?;
            for id in &to_install {
                let ag = tracedecay::agents::get_integration(id)?;
                if ag.supports_local_install() {
                    if !apply_project_local_component_set(
                        id,
                        HostBundleCliOperation::Install,
                        &project_path,
                        &home,
                    )? {
                        ag.install_local(&ctx, &project_path)?;
                    }
                    ag.post_install(Some(&project_path)).await;
                    installed_names.push(ag.name().to_string());
                } else {
                    eprintln!(
                        "Skipping {}: project-local install is not supported",
                        ag.name()
                    );
                }
            }
        }

        eprintln!();
        if installed_names.is_empty() {
            eprintln!("No local changes.");
        } else {
            for name in &installed_names {
                eprintln!("\x1b[32m+\x1b[0m {name} (local)");
            }
        }
        return Ok(());
    }

    if agent.as_deref() == Some("hermes") {
        migrate_legacy_hermes_data(&home).await?;
    }

    let mut user_cfg = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

    let mut installed_names: Vec<String> = Vec::new();
    let mut removed_names: Vec<String> = Vec::new();
    let project_path = std::env::current_dir().ok();

    if let Some(id) = agent {
        let ag = tracedecay::agents::get_integration(&id)?;
        let name = ag.name().to_string();
        let ctx = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: !no_dashboard,
        };
        if !apply_default_canonical_component_set(&id, HostBundleCliOperation::Install, &home)? {
            ag.install(&ctx)?;
            print_legacy_install_guidance(&id);
        }
        ag.post_install(project_path.as_deref()).await;
        if let Some(options) = automation.filter(|_| id == "codex") {
            let scoped_project_path = validate_codex_automation_project_path()?;
            install_codex_daemon_automation(&scoped_project_path, &home, options).await?;
        }
        if !user_cfg.installed_agents.contains(&id) {
            user_cfg.installed_agents.push(id);
            installed_names.push(name);
        }
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    } else {
        let (to_install, to_uninstall) =
            tracedecay::agents::pick_integrations_interactive(&home, &user_cfg.installed_agents)?;

        if to_install.iter().any(|id| id == "hermes") {
            migrate_legacy_hermes_data(&home).await?;
        }

        for id in &to_uninstall {
            let ag = tracedecay::agents::get_integration(id)?;
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: tracedecay_bin.clone(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                project_root: None,
                dashboard: !no_dashboard,
            };
            if !apply_default_canonical_component_set(id, HostBundleCliOperation::Uninstall, &home)?
            {
                ag.uninstall(&ctx)?;
                print_legacy_uninstall_guidance(id);
            }
            removed_names.push(ag.name().to_string());
            user_cfg.installed_agents.retain(|a| a != id);
        }
        for id in &to_install {
            let ag = tracedecay::agents::get_integration(id)?;
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: tracedecay_bin.clone(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                project_root: None,
                dashboard: !no_dashboard,
            };
            if !apply_default_canonical_component_set(id, HostBundleCliOperation::Install, &home)? {
                ag.install(&ctx)?;
                print_legacy_install_guidance(id);
            }
            ag.post_install(project_path.as_deref()).await;
            installed_names.push(ag.name().to_string());
            if !user_cfg.installed_agents.contains(id) {
                user_cfg.installed_agents.push(id.clone());
            }
        }
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    }

    eprintln!();
    if installed_names.is_empty() && removed_names.is_empty() {
        eprintln!("No changes.");
    } else {
        for name in &installed_names {
            eprintln!("\x1b[32m+\x1b[0m {name}");
        }
        for name in &removed_names {
            eprintln!("\x1b[31m-\x1b[0m {name}");
        }
    }

    user_cfg.last_installed_version = env!("CARGO_PKG_VERSION").to_string();
    user_cfg
        .save()
        .map_err(|err| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to save user config: {err}"),
        })?;

    tracedecay::agents::offer_git_post_commit_hook(&tracedecay_bin);
    Ok(())
}

pub(crate) async fn handle_reinstall_command() -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })?;
    let mut user_cfg = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

    if user_cfg.installed_agents.is_empty() {
        eprintln!("No installed agents found. Run `tracedecay install` first.");
    } else {
        let agents = user_cfg.installed_agents.clone();
        eprintln!(
            "Reinstalling {} agent(s): {}",
            agents.len(),
            agents.join(", ")
        );
        let results = reinstall_agent_integrations(&agents, &home, &tracedecay_bin).await;
        // Keep the reason with the name — a bare id list left "failed for:
        // claude, cursor, hermes, kimi" undiagnosable from the output.
        let failed: Vec<String> = results
            .iter()
            .filter_map(|(id, result)| result.as_ref().err().map(|error| format!("{id}: {error}")))
            .collect();
        if !failed.is_empty() {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to reinstall agent(s): {}", failed.join("; ")),
            });
        }
        eprintln!("\x1b[32m✔\x1b[0m All agents reinstalled");
        user_cfg.last_installed_version = env!("CARGO_PKG_VERSION").to_string();
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    }
    Ok(())
}

pub(crate) async fn handle_update_plugin_command() -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })?;
    let mut user_cfg = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);
    let project_path = std::env::current_dir().ok();

    for id in &user_cfg.installed_agents {
        if apply_default_canonical_component_set(id, HostBundleCliOperation::Update, &home)? {
            continue;
        }
        let integration = tracedecay::agents::get_integration(id)?;
        let context = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: project_path.clone(),
            dashboard: true,
        };
        match integration.update_plugin(&context)? {
            tracedecay::agents::UpdatePluginOutcome::Refreshed(paths) => {
                for path in paths {
                    eprintln!(
                        "\x1b[32m✔\x1b[0m refreshed {} at {}",
                        integration.name(),
                        path.display()
                    );
                }
            }
            tracedecay::agents::UpdatePluginOutcome::NotInstalled => {
                eprintln!(
                    "{} is not installed; skipping generated artifact refresh",
                    integration.name()
                );
            }
            tracedecay::agents::UpdatePluginOutcome::ConfigOnly => {
                eprintln!(
                    "{} has no generated artifacts to refresh",
                    integration.name()
                );
            }
        }
    }
    Ok(())
}

/// Re-runs `install()` + `post_install()` for each tracked agent id, returning
/// only the ids that resolve to a real integration paired with their install
/// result.
///
/// An id that does NOT resolve to an integration (a later release renamed or
/// removed it, or a typo landed in `installed_agents`) is SKIPPED, not failed:
/// it is logged as a warning and left out of the returned results entirely.
/// Gating version-marker advancement on such an id would wedge the reinstall
/// loop forever — `migrate_installed_agents` only ever adds ids, never prunes,
/// so a stale id would never resolve and the markers would never advance. Only
/// genuine `install()` failures are reported as `Err` so they still gate
/// markers. Unresolved legacy Hermes project evidence is preserved and warned
/// during automated reinstall; actual source-integrity or copy failures still
/// gate Hermes so corrupted or partially copied data cannot be hidden.
pub(crate) async fn reinstall_agent_integrations(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
) -> Vec<(String, tracedecay::errors::Result<()>)> {
    reinstall_agent_integrations_with_lease(agent_ids, home, tracedecay_bin, None).await
}

/// Reinstalls tracked integrations while reusing lifecycle authority already
/// held by post-update maintenance.
pub(crate) async fn reinstall_agent_integrations_under_lease(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
    lifecycle: &tracedecay::lifecycle_lease::LifecycleLease,
) -> Vec<(String, tracedecay::errors::Result<()>)> {
    reinstall_agent_integrations_with_lease(agent_ids, home, tracedecay_bin, Some(lifecycle)).await
}

async fn reinstall_agent_integrations_with_lease(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
    lifecycle: Option<&tracedecay::lifecycle_lease::LifecycleLease>,
) -> Vec<(String, tracedecay::errors::Result<()>)> {
    let project_path = std::env::current_dir().ok();
    let mut results = Vec::new();
    let hermes_migration_error = if agent_ids.iter().any(|id| id == "hermes") {
        migrate_legacy_hermes_data_for_reinstall(home, lifecycle)
            .await
            .err()
            .map(|error| error.to_string())
    } else {
        None
    };
    for id in agent_ids {
        if id == "hermes"
            && let Some(message) = hermes_migration_error.as_ref()
        {
            results.push((
                id.clone(),
                Err(tracedecay::errors::TraceDecayError::Config {
                    message: message.clone(),
                }),
            ));
            continue;
        }
        match apply_default_canonical_component_set(id, HostBundleCliOperation::Repair, home) {
            Ok(true) => {
                if let Ok(integration) = tracedecay::agents::get_integration(id) {
                    integration.post_install(project_path.as_deref()).await;
                }
                results.push((id.clone(), Ok(())));
                continue;
            }
            Ok(false) => {}
            Err(error) => {
                results.push((id.clone(), Err(error)));
                continue;
            }
        }
        let ag = match tracedecay::agents::get_integration(id) {
            Ok(ag) => ag,
            Err(_) => {
                tracing::warn!(
                    agent_id = id,
                    "skipping unknown tracked agent id; it will not gate the version-marker refresh"
                );
                continue;
            }
        };
        let ctx = tracedecay::agents::InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: tracedecay_bin.to_string(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: true,
        };
        let result = match ag.install(&ctx) {
            Ok(()) => {
                ag.post_install(project_path.as_deref()).await;
                Ok(())
            }
            Err(e) => Err(e),
        };
        results.push((id.clone(), result));
    }
    results
}

pub(crate) async fn handle_uninstall_command(
    agent: Option<String>,
) -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let mut user_cfg = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

    if agent.as_deref() == Some("hermes")
        || (agent.is_none() && user_cfg.installed_agents.iter().any(|id| id == "hermes"))
    {
        migrate_legacy_hermes_data(&home).await?;
    }

    if let Some(id) = agent {
        if !apply_default_canonical_component_set(&id, HostBundleCliOperation::Uninstall, &home)? {
            let ag = tracedecay::agents::get_integration(&id)?;
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: String::new(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                project_root: None,
                dashboard: true,
            };
            ag.uninstall(&ctx)?;
            print_legacy_uninstall_guidance(&id);
        }
        user_cfg.installed_agents.retain(|a| a != &id);
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    } else {
        for id in user_cfg.installed_agents.clone() {
            if apply_default_canonical_component_set(&id, HostBundleCliOperation::Uninstall, &home)?
            {
                continue;
            }
            let ag = tracedecay::agents::get_integration(&id)?;
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: String::new(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                project_root: None,
                dashboard: true,
            };
            ag.uninstall(&ctx)?;
            print_legacy_uninstall_guidance(&id);
        }
        user_cfg.installed_agents.clear();
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
        eprintln!("All agent integrations removed.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use tracedecay::migrate::hermes::{LegacyHermesMigrationIssue, LegacyHermesMigrationReport};

    use super::{
        CompatibilityAgentRegistrationDelegate, HostBundleCliOperation,
        apply_canonical_component_set, broker_codex_daemon_automation_project,
        canonical_host_component_set, component_set_request, finish_legacy_hermes_migration,
    };

    fn seed_opencode_non_context_state(home: &std::path::Path) -> (PathBuf, PathBuf, PathBuf) {
        let config_path = home.join(".config/opencode/opencode.json");
        let core_path = home.join(".config/opencode/plugins/tracedecay.ts");
        let agent_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::Agent),
            0,
        )
        .unwrap()
        .unwrap();
        let agent_path =
            home.join(&agent_set.component_set.components[0].manifest.artifacts[0].relative_path);
        for path in [&config_path, &core_path, &agent_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        std::fs::write(&config_path, b"{\"unrelated\":{\"keep\":true}}\n").unwrap();
        std::fs::write(&core_path, b"core-sentinel\n").unwrap();
        std::fs::write(&agent_path, b"agent-sentinel\n").unwrap();
        (config_path, core_path, agent_path)
    }

    fn assert_opencode_non_context_state(paths: &(PathBuf, PathBuf, PathBuf)) {
        assert_eq!(
            std::fs::read(&paths.0).unwrap(),
            b"{\"unrelated\":{\"keep\":true}}\n"
        );
        assert_eq!(std::fs::read(&paths.1).unwrap(), b"core-sentinel\n");
        assert_eq!(std::fs::read(&paths.2).unwrap(), b"agent-sentinel\n");
    }

    #[tokio::test]
    async fn codex_automation_project_initializes_through_daemon() {
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_path_buf();
        let expected_project_path = project_path.clone();
        let expected_dashboard = project_path.join("dashboard");
        let actual = broker_codex_daemon_automation_project(
            &project_path,
            move |handshake| async move {
                assert_eq!(
                    handshake.project_path.as_deref(),
                    Some(expected_project_path.as_path())
                );
                assert!(handshake.allow_init);
                Ok(())
            },
            |_| Ok(expected_dashboard.clone()),
        )
        .await
        .unwrap();

        assert_eq!(actual, expected_dashboard);
    }

    #[tokio::test]
    async fn unavailable_daemon_does_not_resolve_or_open_local_project() {
        let project = tempfile::tempdir().unwrap();
        let resolved = Arc::new(AtomicBool::new(false));
        let resolver_called = Arc::clone(&resolved);
        let error = broker_codex_daemon_automation_project(
            project.path(),
            |_| async {
                Err(tracedecay::errors::TraceDecayError::Config {
                    message: "daemon unavailable".to_string(),
                })
            },
            move |_| {
                resolver_called.store(true, Ordering::SeqCst);
                Ok(PathBuf::from("unreachable"))
            },
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("daemon unavailable"));
        assert!(!resolved.load(Ordering::SeqCst));
        assert!(std::fs::read_dir(project.path()).unwrap().next().is_none());
    }

    #[test]
    fn unresolved_legacy_store_is_preserved_without_gating_cutover() {
        let report = LegacyHermesMigrationReport {
            unresolved: vec![LegacyHermesMigrationIssue {
                source_db: PathBuf::from("legacy-sessions.db"),
                reason: "project evidence is unresolved".to_string(),
            }],
            ..LegacyHermesMigrationReport::default()
        };

        assert!(finish_legacy_hermes_migration(report).is_ok());
    }

    #[test]
    fn legacy_store_failures_gate_cutover() {
        let report = LegacyHermesMigrationReport {
            failed: vec![LegacyHermesMigrationIssue {
                source_db: PathBuf::from("legacy-sessions.db"),
                reason: "integrity check failed".to_string(),
            }],
            ..LegacyHermesMigrationReport::default()
        };

        let error = finish_legacy_hermes_migration(report)
            .unwrap_err()
            .to_string();
        assert!(error.contains("integrity check failed"));
    }

    #[test]
    fn canonical_host_component_selection_uses_default_or_explicit_set() {
        let default = canonical_host_component_set("opencode", None, 0)
            .unwrap()
            .expect("OpenCode has a first-party default set");
        assert_eq!(default.component_set.components.len(), 3);

        let explicit = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::ContextMcp),
            0,
        )
        .unwrap()
        .expect("explicit component uses a one-element set");
        assert_eq!(explicit.component_set.components.len(), 1);
        assert_eq!(
            explicit.component_set.components[0].manifest.component,
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp
        );
        let hermes = canonical_host_component_set("hermes", None, 0)
            .unwrap()
            .expect("Hermes has a first-party default Core set");
        assert_eq!(hermes.component_set.components.len(), 1);
        assert_eq!(
            hermes.component_set.components[0].manifest.component,
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core
        );
        let kiro = canonical_host_component_set("kiro", None, 0)
            .unwrap()
            .expect("Kiro has a first-party default Core set");
        assert_eq!(kiro.component_set.components.len(), 1);
        assert_eq!(
            kiro.component_set.components[0].manifest.component,
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core
        );
    }

    #[test]
    fn explicit_context_component_lifecycle_preserves_other_opencode_state() {
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let preserved = seed_opencode_non_context_state(home.path());
        let component_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::ContextMcp),
            0,
        )
        .unwrap()
        .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: Some(crate::cli::HostBundleComponentArg::ContextMcp),
            dry_run: false,
            yes: true,
        };

        apply_canonical_component_set(
            "opencode",
            HostBundleCliOperation::Install,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
        )
        .unwrap();
        assert_opencode_non_context_state(&preserved);

        apply_canonical_component_set(
            "opencode",
            HostBundleCliOperation::Uninstall,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
        )
        .unwrap();
        assert_opencode_non_context_state(&preserved);
    }

    #[test]
    fn explicit_context_component_rollback_preserves_other_opencode_state() {
        use tracedecay::agents::host_bundle_v2::HostComponentSetRegistrationV1;

        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let preserved = seed_opencode_non_context_state(home.path());
        let component_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::ContextMcp),
            0,
        )
        .unwrap()
        .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true).unwrap();
        let mut registration = CompatibilityAgentRegistrationDelegate::new(
            "opencode",
            home.path(),
            lifecycle.path(),
            request.lifecycle.operation,
        )
        .unwrap();

        registration
            .preflight(&component_set.component_set, &request)
            .unwrap();
        registration
            .stage(&component_set.component_set, &request)
            .unwrap();
        registration
            .apply(&component_set.component_set, &request)
            .unwrap();
        registration
            .rollback(&component_set.component_set, &request)
            .unwrap();

        assert_opencode_non_context_state(&preserved);
    }

    #[test]
    fn kiro_canonical_component_set_runs_full_lifecycle() {
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let mcp_path = home.path().join(".kiro/settings/mcp.json");
        std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
        std::fs::write(
            &mcp_path,
            r#"{"mcpServers":{"other":{"command":"other"}},"unrelated":true}"#,
        )
        .unwrap();
        let component_set = canonical_host_component_set("kiro", None, 0)
            .unwrap()
            .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
        };

        for operation in [
            HostBundleCliOperation::Install,
            HostBundleCliOperation::Update,
        ] {
            apply_canonical_component_set(
                "kiro",
                operation,
                &component_set,
                &options,
                home.path(),
                lifecycle.path(),
            )
            .unwrap();
        }
        std::fs::write(
            home.path().join(".kiro/steering/tracedecay.md"),
            "stale tracedecay steering",
        )
        .unwrap();
        apply_canonical_component_set(
            "kiro",
            HostBundleCliOperation::Repair,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
        )
        .unwrap();

        let mcp: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(mcp["unrelated"], true);
        assert_eq!(mcp["mcpServers"]["other"]["command"], "other");
        assert!(mcp["mcpServers"]["tracedecay"].is_object());
        assert!(
            home.path()
                .join(".kiro/tracedecay/component.json")
                .is_file()
        );
        assert!(home.path().join(".kiro/agents/tracedecay.json").is_file());
        assert!(
            std::fs::read_to_string(home.path().join(".kiro/steering/tracedecay.md"))
                .unwrap()
                .contains("## TraceDecay: mandatory tool routing")
        );

        apply_canonical_component_set(
            "kiro",
            HostBundleCliOperation::Uninstall,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
        )
        .unwrap();
        let mcp: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(mcp["unrelated"], true);
        assert_eq!(mcp["mcpServers"]["other"]["command"], "other");
        assert!(mcp["mcpServers"].get("tracedecay").is_none());
        assert!(!home.path().join(".kiro/tracedecay/component.json").exists());
        assert!(!home.path().join(".kiro/agents/tracedecay.json").exists());
    }

    #[test]
    fn stale_registration_stage_does_not_run_inverse_rollback_edit() {
        use tracedecay::agents::host_bundle_v2::{
            HostBundleError, HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1,
        };

        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set = canonical_host_component_set("opencode", None, 0)
            .unwrap()
            .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true).unwrap();
        let mut registration = CompatibilityAgentRegistrationDelegate::new(
            "opencode",
            home.path(),
            lifecycle.path(),
            request.lifecycle.operation,
        )
        .unwrap();
        registration
            .preflight(&component_set.component_set, &request)
            .unwrap();
        let revision = registration
            .current_revision(&component_set.component_set, &request)
            .unwrap();
        let preview = HostComponentSetLifecyclePreviewV1 {
            operation_id: request.operation_id,
            plan_digest: [7; 32],
            base_registration_revision: revision,
            current_registration_revision: revision,
            artifact_state_revision: [8; 32],
            component_plans: Vec::new(),
            confirmation_required: false,
        };
        registration
            .confirm_preview(&component_set.component_set, &request, &preview)
            .unwrap();

        let registration_path = registration.registration_path().unwrap().to_path_buf();
        std::fs::create_dir_all(registration_path.parent().unwrap()).unwrap();
        std::fs::write(&registration_path, b"{\"external\":true}").unwrap();
        assert_eq!(
            registration.stage(&component_set.component_set, &request),
            Err(HostBundleError::StalePreview)
        );
        registration
            .rollback(&component_set.component_set, &request)
            .unwrap();
        assert_eq!(
            std::fs::read(registration_path).unwrap(),
            b"{\"external\":true}"
        );
    }
}
