//! Feedback-path rollback command and durable recovery state.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write, sync_parent_directory};

use super::feedback_component::{
    aggregate_with_feedback_component, companion_owned_live_paths, live_feedback_receipt,
    selected_feedback_component,
};
use super::{host_bundle_error, host_kind_for_agent, load_host_lifecycle_user_config};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FeedbackRollbackCliStatus {
    Prepared,
    Applied,
    Restored,
}

const FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION: u16 = 6;
const MIN_FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION: u16 = 6;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeedbackRollbackCliState {
    schema_version: u16,
    agent_id: String,
    host: tracedecay_agent_hosts::agents::host_bundle::HostKindV1,
    status: FeedbackRollbackCliStatus,
    previous_aggregate: tracedecay_agent_hosts::agents::host_bundle::HostComponentSetReceiptV1,
    previous_manifest: tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
    previous_contents:
        Vec<tracedecay_agent_hosts::agents::host_bundle::HostBundleArtifactContentV1>,
    target_manifest: tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
    dashboard_enabled: bool,
    switch_operation_id: [u8; 16],
    effect_started: bool,
    registration_effect_started: bool,
    registration_intent_root: PathBuf,
    compensation_preserves_registration: bool,
    switch_receipt:
        Option<tracedecay_agent_hosts::agents::host_bundle::FeedbackPathRollbackReceiptV1>,
    restore_operation_id: Option<[u8; 16]>,
    restore_effect_started: bool,
    restore_receipt:
        Option<tracedecay_agent_hosts::agents::host_bundle::FeedbackPathRestoreReceiptV1>,
    identity: FeedbackRollbackIdentityV2,
    registration_files: Vec<FeedbackRegistrationFileState>,
    artifact_permissions: Vec<FeedbackArtifactPermissionStateV4>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FeedbackRollbackIdentityV2 {
    canonical_home: PathBuf,
    canonical_lifecycle_root: PathBuf,
    canonical_project: PathBuf,
    integration_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeedbackRegistrationFileState {
    path_index: usize,
    path_digest: [u8; 32],
    contents: Option<Vec<u8>>,
    permissions: Option<FeedbackFilePermissionsV2>,
    #[serde(default)]
    metadata: Option<tracedecay_agent_hosts::agents::HostFileMetadataIdentityV1>,
    applied_state: Option<FeedbackFileObservedStateV2>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FeedbackFileObservedStateV2 {
    present: bool,
    digest: [u8; 32],
    #[serde(default)]
    metadata: Option<tracedecay_agent_hosts::agents::HostFileMetadataIdentityV1>,
}

#[derive(Deserialize)]
struct FeedbackHostConfigWriteIntentV2 {
    schema_version: u16,
    digest: [u8; 32],
    metadata: Option<tracedecay_agent_hosts::agents::HostFileMetadataIdentityV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FeedbackFilePermissionsV2 {
    readonly: bool,
    unix_mode: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeedbackArtifactPermissionStateV4 {
    relative_path: String,
    permissions: FeedbackFilePermissionsV2,
}

impl FeedbackRollbackIdentityV2 {
    fn current(
        integration_id: &str,
        home: &Path,
        lifecycle_root: &Path,
    ) -> tracedecay_domain::errors::Result<Self> {
        let project = std::env::current_dir().map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("could not determine feedback rollback project: {error}"),
            }
        })?;
        Ok(Self {
            canonical_home: canonical_feedback_path("home", home)?,
            canonical_lifecycle_root: canonical_feedback_path("lifecycle profile", lifecycle_root)?,
            canonical_project: canonical_feedback_path("project", &project)?,
            integration_id: integration_id.to_string(),
        })
    }

    fn validate(
        &self,
        integration_id: &str,
        home: &Path,
        lifecycle_root: &Path,
    ) -> tracedecay_domain::errors::Result<()> {
        let current = Self::current(integration_id, home, lifecycle_root)?;
        (self == &current)
            .then_some(())
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message:
                    "feedback rollback state belongs to a different home, profile, project, or integration"
                        .to_string(),
            })
    }
}

fn canonical_feedback_path(label: &str, path: &Path) -> tracedecay_domain::errors::Result<PathBuf> {
    fs::canonicalize(path).map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!(
            "could not canonicalize feedback rollback {label} {}: {error}",
            path.display()
        ),
    })
}

#[derive(Clone)]
struct FeedbackPairVerifier {
    digests: [[u8; 32]; 2],
}

impl tracedecay_agent_hosts::agents::host_bundle::HostBundleVerificationAdapterV1
    for FeedbackPairVerifier
{
    fn verify_manifest(
        &self,
        manifest: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
    ) -> Result<(), tracedecay_agent_hosts::agents::host_bundle::HostBundleError> {
        manifest.validate_structure()?;
        self.digests
            .contains(&manifest.canonical_digest()?)
            .then_some(())
            .ok_or(tracedecay_agent_hosts::agents::host_bundle::HostBundleError::CatalogMismatch)
    }
}

struct FeedbackPreviewStorage;

impl tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleStorageV1
    for FeedbackPreviewStorage
{
    fn recover_lifecycle(
        &mut self,
    ) -> Result<(), tracedecay_agent_hosts::agents::host_bundle::HostBundleError> {
        Ok(())
    }

    fn execute_lifecycle<
        V: tracedecay_agent_hosts::agents::host_bundle::HostBundleVerificationAdapterV1,
    >(
        &mut self,
        _manifest: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
        _request: &tracedecay_agent_hosts::agents::host_bundle::HostBundleExecutionRequestV1,
        _contents: &[tracedecay_agent_hosts::agents::host_bundle::HostBundleArtifactContentV1],
        _verifier: &V,
    ) -> Result<
        tracedecay_agent_hosts::agents::host_bundle::HostBundleInstallReceiptV1,
        tracedecay_agent_hosts::agents::host_bundle::HostBundleError,
    > {
        Err(tracedecay_host_integration::host_bundle_storage_failure!())
    }
}

pub(crate) async fn handle_feedback_rollback_command(
    action: crate::cli::FeedbackRollbackAction,
) -> tracedecay_domain::errors::Result<()> {
    match action {
        crate::cli::FeedbackRollbackAction::DryRun { agent } => feedback_rollback_dry_run(&agent),
        crate::cli::FeedbackRollbackAction::Apply { agent, state, yes } => {
            if !yes {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "feedback rollback apply requires --yes".to_string(),
                });
            }
            feedback_rollback_apply(&agent, Path::new(&state))
        }
        crate::cli::FeedbackRollbackAction::Restore { state, yes } => {
            if !yes {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "feedback rollback restore requires --yes".to_string(),
                });
            }
            feedback_rollback_restore(Path::new(&state))
        }
    }
}

fn feedback_rollback_inputs(
    agent_id: &str,
) -> tracedecay_domain::errors::Result<(
    PathBuf,
    PathBuf,
    tracedecay_agent_hosts::agents::host_bundle::HostComponentSetReceiptV1,
    tracedecay_agent_hosts::agents::host_bundle_registry::VerifiedEmbeddedHostBundleV1,
)> {
    let home = tracedecay_agent_hosts::agents::home_dir().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root =
        tracedecay_agent_hosts::agents::host_bundle::resolved_host_bundle_lifecycle_root()
            .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("could not resolve host lifecycle root: {error}"),
            })?;
    let host = host_kind_for_agent(agent_id)?;
    let previous =
        tracedecay_agent_hosts::agents::host_bundle::latest_host_component_set_receipt_at(
            &lifecycle_root,
            host,
        )
        .map_err(host_bundle_error)?
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("no aggregate host receipt exists for {agent_id}"),
        })?;
    let component = selected_feedback_component(&previous)?;
    let mut target =
        tracedecay_agent_hosts::agents::host_bundle_registry::verified_embedded_host_bundle(
            host,
            component,
            0,
            crate::product_runtime::PRODUCT_FULL_SHA,
        )
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("compiled feedback route is unavailable for {agent_id}: {error}"),
        })?;
    let companion_owned_paths = companion_owned_live_paths(&home, &previous)?;
    target
        .manifest
        .artifacts
        .retain(|artifact| !companion_owned_paths.contains(&artifact.relative_path));
    target.contents.retain(|content| {
        target
            .manifest
            .artifacts
            .iter()
            .any(|artifact| artifact.relative_path == content.relative_path)
    });
    #[cfg(feature = "test-transport")]
    if let Some(revision) = std::env::var_os("TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION") {
        let revision = revision.to_string_lossy();
        target.manifest.configuration_snapshot_id = revision.clone().into_owned();
        let content = target.contents.first_mut().ok_or_else(|| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: "compiled feedback route has no artifact bytes".to_string(),
            }
        })?;
        content
            .bytes
            .extend_from_slice(format!("\nfeedback-route:{revision}\n").as_bytes());
        let artifact = target
            .manifest
            .artifacts
            .iter_mut()
            .find(|artifact| artifact.relative_path == content.relative_path)
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "compiled feedback route content has no manifest artifact".to_string(),
            })?;
        artifact.artifact_digest = Sha256::digest(&content.bytes).into();
    }
    Ok((home, lifecycle_root, previous, target))
}

fn feedback_pair_verifier(
    previous: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
    target: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
) -> tracedecay_domain::errors::Result<FeedbackPairVerifier> {
    Ok(FeedbackPairVerifier {
        digests: [
            previous.canonical_digest().map_err(host_bundle_error)?,
            target.canonical_digest().map_err(host_bundle_error)?,
        ],
    })
}

fn feedback_request(
    manifest: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
    operation: tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1,
    confirmed: bool,
) -> tracedecay_domain::errors::Result<
    tracedecay_agent_hosts::agents::host_bundle::HostBundleExecutionRequestV1,
> {
    let operation_id = tracedecay_contracts::request_identity::mint_global_operation_id(
        tracedecay_contracts::request_identity::GlobalOperationIdentityKind::HostFeedbackRollback,
    )
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("could not generate feedback rollback operation id: {error}"),
    })?;
    Ok(
        tracedecay_agent_hosts::agents::host_bundle::HostBundleExecutionRequestV1 {
            lifecycle: tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleRequestV1 {
                operation,
                expected_host: manifest.host,
                expected_component: manifest.component,
                explicit_confirmation: confirmed,
                hermes_profile_bindings: u8::from(
                    manifest.host
                        == tracedecay_agent_hosts::agents::host_bundle::HostKindV1::Hermes,
                ),
                // Feedback rollback only ever moves between receipt-owned
                // Core deployments; it never claims receiptless files.
                adopt_receiptless: false,
            },
            operation_id,
        },
    )
}

fn feedback_observed(
    home: &Path,
    target: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
    previous_receipt: &tracedecay_agent_hosts::agents::host_bundle::HostBundleInstallReceiptV1,
) -> tracedecay_domain::errors::Result<(
    Vec<tracedecay_agent_hosts::agents::host_bundle::ObservedHostArtifactV1>,
    Vec<tracedecay_agent_hosts::agents::host_bundle::ObservedHostArtifactV1>,
)> {
    use tracedecay_agent_hosts::agents::host_bundle::{
        ObservedArtifactKindV1, ObservedHostArtifactV1,
    };

    let observe = |relative_path: &str,
                   owned: Option<
        &tracedecay_agent_hosts::agents::host_bundle::HostBundleReceiptArtifactV1,
    >,
                   cataloged_ownership_marker: Option<String>|
     -> tracedecay_domain::errors::Result<ObservedHostArtifactV1> {
        let path = tracedecay_agent_hosts::agents::host_bundle::inspect_install_target(
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
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("could not inspect feedback artifact {}", path.display()),
                });
            }
        };
        Ok(ObservedHostArtifactV1 {
            relative_path: relative_path.to_string(),
            kind,
            artifact_digest,
            // The exact prior receipt and digest carry ownership across a
            // catalog revision. Present the target marker to the planner
            // only for that receipt-bound path; foreign bytes still fail
            // the owned-digest comparison below.
            ownership_marker: owned.and_then(|_| cataloged_ownership_marker.clone()),
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
    manifest: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
) -> tracedecay_domain::errors::Result<
    Vec<tracedecay_agent_hosts::agents::host_bundle::HostBundleArtifactContentV1>,
> {
    manifest
        .artifacts
        .iter()
        .map(|artifact| {
            let path = tracedecay_agent_hosts::agents::host_bundle::inspect_install_target(
                home,
                Path::new(&artifact.relative_path),
            )
            .map_err(host_bundle_error)?;
            let bytes = fs::read(&path).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "could not snapshot feedback artifact {}: {error}",
                        path.display()
                    ),
                }
            })?;
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            if digest != artifact.artifact_digest {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "feedback artifact {} no longer matches its ownership receipt",
                        path.display()
                    ),
                });
            }
            Ok(
                tracedecay_agent_hosts::agents::host_bundle::HostBundleArtifactContentV1 {
                    relative_path: artifact.relative_path.clone(),
                    bytes,
                },
            )
        })
        .collect()
}

fn read_feedback_repair_contents(
    home: &Path,
    manifest: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
) -> tracedecay_domain::errors::Result<
    Vec<tracedecay_agent_hosts::agents::host_bundle::HostBundleArtifactContentV1>,
> {
    manifest
        .artifacts
        .iter()
        .map(|artifact| {
            let path = tracedecay_agent_hosts::agents::host_bundle::inspect_install_target(
                home,
                Path::new(&artifact.relative_path),
            )
            .map_err(host_bundle_error)?;
            let bytes = fs::read(&path).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "could not snapshot feedback repair artifact {}: {error}",
                        path.display()
                    ),
                }
            })?;
            Ok(
                tracedecay_agent_hosts::agents::host_bundle::HostBundleArtifactContentV1 {
                    relative_path: artifact.relative_path.clone(),
                    bytes,
                },
            )
        })
        .collect()
}

fn snapshot_feedback_registration(
    home: &Path,
    integration: &dyn tracedecay_agent_hosts::agents::AgentIntegration,
    component: tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1,
) -> tracedecay_domain::errors::Result<Vec<FeedbackRegistrationFileState>> {
    let paths = feedback_registration_paths(home, integration, component)?;
    paths
        .into_iter()
        .enumerate()
        .map(|(path_index, path)| {
            let contents = match fs::read(&path) {
                Ok(contents) => Some(contents),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!(
                            "could not snapshot feedback registration {}: {error}",
                            path.display()
                        ),
                    });
                }
            };
            let permissions = fs::metadata(&path)
                .ok()
                .map(|metadata| feedback_file_permissions(&metadata.permissions()));
            let metadata = match &contents {
                Some(_) => Some(
                    tracedecay_agent_hosts::agents::capture_host_file_metadata(&path).map_err(
                        |error| tracedecay_domain::errors::TraceDecayError::Config {
                            message: format!(
                                "could not snapshot feedback registration metadata {}: {error}",
                                path.display()
                            ),
                        },
                    )?,
                ),
                None => None,
            };
            Ok(FeedbackRegistrationFileState {
                path_index,
                path_digest: feedback_path_digest(&path)?,
                contents,
                permissions,
                metadata,
                applied_state: None,
            })
        })
        .collect()
}

/// Re-resolves the registration inventory and pins it to a recorded snapshot.
/// `inventory_changed` carries the caller's own wording for a stale inventory.
fn feedback_registration_paths_for_state(
    home: &Path,
    integration: &dyn tracedecay_agent_hosts::agents::AgentIntegration,
    component: tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1,
    registration_files: &[FeedbackRegistrationFileState],
    inventory_changed: &str,
) -> tracedecay_domain::errors::Result<Vec<PathBuf>> {
    let paths = feedback_registration_paths(home, integration, component)?;
    if paths.len() == registration_files.len() {
        Ok(paths)
    } else {
        Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: inventory_changed.to_string(),
        })
    }
}

fn feedback_registration_path<'a>(
    paths: &'a [PathBuf],
    file: &FeedbackRegistrationFileState,
) -> tracedecay_domain::errors::Result<&'a Path> {
    paths
        .get(file.path_index)
        .map(PathBuf::as_path)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "feedback registration path index is invalid".to_string(),
        })
}

fn capture_feedback_applied_registration(
    home: &Path,
    integration: &dyn tracedecay_agent_hosts::agents::AgentIntegration,
    component: tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1,
    registration_files: &mut [FeedbackRegistrationFileState],
) -> tracedecay_domain::errors::Result<()> {
    let paths = feedback_registration_paths_for_state(
        home,
        integration,
        component,
        registration_files,
        "feedback registration inventory changed during apply",
    )?;
    for file in registration_files {
        let path = feedback_registration_path(&paths, file)?;
        if feedback_path_digest(path)? != file.path_digest {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "feedback registration path identity changed during apply".to_string(),
            });
        }
        file.applied_state = Some(feedback_file_observed_state(path)?);
    }
    Ok(())
}

fn validate_feedback_registration_snapshot(
    home: &Path,
    integration: &dyn tracedecay_agent_hosts::agents::AgentIntegration,
    component: tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1,
    registration_files: &[FeedbackRegistrationFileState],
) -> tracedecay_domain::errors::Result<()> {
    let paths = feedback_registration_paths_for_state(
        home,
        integration,
        component,
        registration_files,
        "feedback registration inventory changed before apply",
    )?;
    for file in registration_files {
        let path = feedback_registration_path(&paths, file)?;
        if feedback_path_digest(path)? != file.path_digest
            || feedback_file_observed_state(path)?
                != feedback_observed_state_for_contents(
                    file.contents.as_deref(),
                    file.metadata.clone(),
                )
        {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "feedback registration {} changed before apply; refusing stale activation",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

fn validate_feedback_registration_restore(
    home: &Path,
    integration: &dyn tracedecay_agent_hosts::agents::AgentIntegration,
    component: tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1,
    registration_files: &[FeedbackRegistrationFileState],
    effect_started: bool,
    intent_root: Option<&Path>,
) -> tracedecay_domain::errors::Result<Vec<PathBuf>> {
    let paths = feedback_registration_paths_for_state(
        home,
        integration,
        component,
        registration_files,
        "feedback registration inventory no longer matches rollback state",
    )?;
    for file in registration_files {
        let path = feedback_registration_path(&paths, file)?;
        if feedback_path_digest(path)? != file.path_digest {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "feedback registration path identity no longer matches rollback state"
                    .to_string(),
            });
        }
        let original =
            feedback_observed_state_for_contents(file.contents.as_deref(), file.metadata.clone());
        let expected = if let Some(applied) = file.applied_state.clone() {
            applied
        } else if effect_started {
            let intent_path = tracedecay_agent_hosts::agents::host_config_write_intent_path(
                intent_root.ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                    message: "feedback registration effect has no write-intent root".to_string(),
                })?,
                path,
            )?;
            match fs::read(intent_path) {
                Ok(intent) if intent.len() == 33 && intent[0] == 1 => {
                    let mut digest = [0_u8; 32];
                    digest.copy_from_slice(&intent[1..]);
                    FeedbackFileObservedStateV2 {
                        present: true,
                        digest,
                        metadata: original.metadata.clone(),
                    }
                }
                Ok(intent) => {
                    let intent: FeedbackHostConfigWriteIntentV2 = serde_json::from_slice(&intent)
                        .map_err(|_| {
                        tracedecay_domain::errors::TraceDecayError::Config {
                            message: "invalid feedback registration write intent".to_string(),
                        }
                    })?;
                    if intent.schema_version != 2 {
                        return Err(tracedecay_domain::errors::TraceDecayError::Config {
                            message: "unsupported feedback registration write-intent version"
                                .to_string(),
                        });
                    }
                    FeedbackFileObservedStateV2 {
                        present: true,
                        digest: intent.digest,
                        metadata: intent.metadata,
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => original.clone(),
                _ => {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: "invalid feedback registration write intent".to_string(),
                    });
                }
            }
        } else {
            original.clone()
        };
        let observed = feedback_file_observed_state(path)?;
        if observed != expected && observed != original {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "feedback registration {} changed after apply; refusing stale restore",
                    path.display()
                ),
            });
        }
    }
    Ok(paths)
}

fn restore_feedback_registration(
    home: &Path,
    integration: &dyn tracedecay_agent_hosts::agents::AgentIntegration,
    component: tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1,
    registration_files: &[FeedbackRegistrationFileState],
    effect_started: bool,
    intent_root: Option<&Path>,
) -> tracedecay_domain::errors::Result<()> {
    let paths = validate_feedback_registration_restore(
        home,
        integration,
        component,
        registration_files,
        effect_started,
        intent_root,
    )?;
    for file in registration_files {
        let path = &paths[file.path_index];
        let mut removed = false;
        match &file.contents {
            Some(contents) => {
                tracedecay_agent_hosts::agents::safe_write_bytes_file_with_metadata(
                    path,
                    contents,
                    None,
                    file.metadata.as_ref(),
                )?;
                if file.metadata.is_none()
                    && let Some(permissions) = &file.permissions
                {
                    restore_feedback_file_permissions(path, permissions)?;
                }
            }
            None => match fs::remove_file(path) {
                Ok(()) => removed = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!(
                            "could not remove restored feedback registration {}: {error}",
                            path.display()
                        ),
                    });
                }
            },
        }
        if removed {
            sync_parent_directory(path, DirectorySyncPolicy::TolerateUnsupported).map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "could not durably remove feedback registration {}: {error}",
                        path.display()
                    ),
                },
            )?;
        }
    }
    Ok(())
}

fn validate_feedback_applied_artifacts(
    home: &Path,
    target_manifest: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
    applied_receipt: &tracedecay_agent_hosts::agents::host_bundle::HostBundleInstallReceiptV1,
) -> tracedecay_domain::errors::Result<()> {
    if target_manifest
        .canonical_digest()
        .map_err(host_bundle_error)?
        != applied_receipt.manifest_digest
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "feedback target manifest no longer matches its applied receipt".to_string(),
        });
    }
    let contents = read_feedback_contents(home, target_manifest)?;
    if contents.len() != applied_receipt.artifacts.len() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "feedback artifact inventory no longer matches its applied receipt"
                .to_string(),
        });
    }
    for artifact in &applied_receipt.artifacts {
        let Some(content) = contents
            .iter()
            .find(|content| content.relative_path == artifact.relative_path)
        else {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "feedback artifact {} is missing before restore",
                    artifact.relative_path
                ),
            });
        };
        if <[u8; 32]>::from(Sha256::digest(&content.bytes)) != artifact.artifact_digest {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "feedback artifact {} changed before restore; refusing stale mutation",
                    artifact.relative_path
                ),
            });
        }
    }
    Ok(())
}

fn validate_feedback_active_receipts(
    lifecycle_root: &Path,
    switch_receipt: &tracedecay_agent_hosts::agents::host_bundle::FeedbackPathRollbackReceiptV1,
    selected_component: tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1,
    expected_aggregates: &[tracedecay_agent_hosts::agents::host_bundle::HostComponentSetReceiptV1],
    previous_aggregate: Option<
        &tracedecay_agent_hosts::agents::host_bundle::HostComponentSetReceiptV1,
    >,
) -> tracedecay_domain::errors::Result<()> {
    use tracedecay_agent_hosts::agents::host_bundle::{
        latest_host_component_receipt_at, latest_host_component_set_receipt_at,
    };

    let component =
        latest_host_component_receipt_at(lifecycle_root, switch_receipt.host, selected_component)
            .map_err(host_bundle_error)?;
    let aggregate = latest_host_component_set_receipt_at(lifecycle_root, switch_receipt.host)
        .map_err(host_bundle_error)?;
    let aggregate_component = aggregate.as_ref().and_then(|receipt| {
        receipt
            .component_receipts
            .iter()
            .find(|receipt| receipt.component == selected_component)
    });
    let self_authored_partial_transition = previous_aggregate.is_some_and(|previous| {
        aggregate.as_ref() == Some(previous)
            && component.as_ref() == Some(&switch_receipt.apply_receipt)
    });
    if component.as_ref() != Some(&switch_receipt.apply_receipt)
        || aggregate
            .as_ref()
            .is_none_or(|current| !expected_aggregates.contains(current))
        || (aggregate_component != Some(&switch_receipt.apply_receipt)
            && !self_authored_partial_transition)
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "feedback ownership receipt changed before restore; refusing stale mutation"
                .to_string(),
        });
    }
    Ok(())
}

fn snapshot_feedback_artifact_permissions(
    home: &Path,
    manifest: &tracedecay_agent_hosts::agents::host_bundle::HostBundleManifestV1,
) -> tracedecay_domain::errors::Result<Vec<FeedbackArtifactPermissionStateV4>> {
    manifest
        .artifacts
        .iter()
        .map(|artifact| {
            let path = home.join(&artifact.relative_path);
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "could not capture feedback artifact permissions for {}: {error}",
                        path.display()
                    ),
                }
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("unsafe feedback artifact {}", path.display()),
                });
            }
            Ok(FeedbackArtifactPermissionStateV4 {
                relative_path: artifact.relative_path.clone(),
                permissions: feedback_file_permissions(&metadata.permissions()),
            })
        })
        .collect()
}

fn restore_feedback_artifact_permissions(
    home: &Path,
    permissions: &[FeedbackArtifactPermissionStateV4],
) -> tracedecay_domain::errors::Result<()> {
    for artifact in permissions {
        restore_feedback_file_permissions(
            &home.join(&artifact.relative_path),
            &artifact.permissions,
        )?;
    }
    Ok(())
}

fn feedback_registration_paths(
    home: &Path,
    integration: &dyn tracedecay_agent_hosts::agents::AgentIntegration,
    component: tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1,
) -> tracedecay_domain::errors::Result<Vec<PathBuf>> {
    let mut paths = integration.host_component_registration_paths_checked(&[component], home)?;
    if integration.id() == "claude" {
        let artifact_owned_manifest =
            home.join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/marketplace.json");
        paths.retain(|path| path != &artifact_owned_manifest);
    } else if integration.id() == "cursor" {
        let artifact_owned_manifest =
            home.join(".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json");
        paths.retain(|path| path != &artifact_owned_manifest);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn feedback_path_digest(path: &Path) -> tracedecay_domain::errors::Result<[u8; 32]> {
    let bytes = serde_json::to_vec(path).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("could not bind feedback registration path: {error}"),
        }
    })?;
    Ok(Sha256::digest(bytes).into())
}

fn feedback_file_observed_state(
    path: &Path,
) -> tracedecay_domain::errors::Result<FeedbackFileObservedStateV2> {
    match fs::read(path) {
        Ok(bytes) => Ok(FeedbackFileObservedStateV2 {
            present: true,
            digest: Sha256::digest(bytes).into(),
            metadata: Some(
                tracedecay_agent_hosts::agents::capture_host_file_metadata(path).map_err(
                    |error| tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!(
                            "could not inspect feedback registration metadata {}: {error}",
                            path.display()
                        ),
                    },
                )?,
            ),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(FeedbackFileObservedStateV2 {
                present: false,
                digest: [0; 32],
                metadata: None,
            })
        }
        Err(error) => Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "could not inspect feedback registration {}: {error}",
                path.display()
            ),
        }),
    }
}

fn feedback_observed_state_for_contents(
    contents: Option<&[u8]>,
    metadata: Option<tracedecay_agent_hosts::agents::HostFileMetadataIdentityV1>,
) -> FeedbackFileObservedStateV2 {
    contents.map_or(
        FeedbackFileObservedStateV2 {
            present: false,
            digest: [0; 32],
            metadata: None,
        },
        |bytes| FeedbackFileObservedStateV2 {
            present: true,
            digest: Sha256::digest(bytes).into(),
            metadata,
        },
    )
}

fn feedback_file_permissions(permissions: &fs::Permissions) -> FeedbackFilePermissionsV2 {
    #[cfg(unix)]
    let unix_mode = Some(permissions.mode());
    #[cfg(not(unix))]
    let unix_mode = None;
    FeedbackFilePermissionsV2 {
        readonly: permissions.readonly(),
        unix_mode,
    }
}

fn restore_feedback_file_permissions(
    path: &Path,
    state: &FeedbackFilePermissionsV2,
) -> tracedecay_domain::errors::Result<()> {
    let mut permissions = fs::metadata(path)
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "could not inspect feedback registration permissions {}: {error}",
                path.display()
            ),
        })?
        .permissions();
    #[cfg(unix)]
    if let Some(mode) = state.unix_mode {
        permissions.set_mode(mode);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(state.readonly);
    fs::set_permissions(path, permissions).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "could not restore feedback registration permissions {}: {error}",
                path.display()
            ),
        }
    })?;
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .and_then(|()| sync_parent_directory(path, DirectorySyncPolicy::TolerateUnsupported))
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "could not durably restore feedback registration {}: {error}",
                path.display()
            ),
        })
}

fn write_feedback_state(
    path: &Path,
    state: &FeedbackRollbackCliState,
) -> tracedecay_domain::errors::Result<()> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("could not serialize feedback rollback state: {error}"),
        }
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("could not create feedback state directory: {error}"),
        }
    })?;
    atomic_write(
        path,
        "feedback-rollback-state",
        &bytes,
        DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("could not durably publish feedback rollback state: {error}"),
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
) -> tracedecay_domain::errors::Result<()> {
    write_feedback_state(state_path, state)?;
    let doctor_path = feedback_doctor_state_path(lifecycle_root, &state.agent_id);
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION,
        "agent_id": state.agent_id,
        "host": state.host,
        "status": state.status,
        "state_path": state_path,
    }))
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("could not serialize feedback Doctor state: {error}"),
    })?;
    let parent = doctor_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("could not create feedback Doctor state directory: {error}"),
        }
    })?;
    let temporary = doctor_path.with_extension("json.new");
    fs::write(&temporary, bytes).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("could not stage feedback Doctor state: {error}"),
        }
    })?;
    fs::rename(&temporary, doctor_path).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("could not publish feedback Doctor state: {error}"),
        }
    })
}

fn feedback_rollback_dry_run(agent_id: &str) -> tracedecay_domain::errors::Result<()> {
    let (home, _lifecycle_root, aggregate, target) = feedback_rollback_inputs(agent_id)?;
    let (previous, previous_receipt) = live_feedback_receipt(&home, &aggregate)?;
    let verifier = feedback_pair_verifier(&previous, &target.manifest)?;
    let request = feedback_request(
        &target.manifest,
        tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1::Repair,
        false,
    )?;
    let (manifest_observed, orphan_observed) =
        feedback_observed(&home, &target.manifest, &previous_receipt)?;
    let lifecycle = tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleRuntimeV1::new(
        verifier,
        FeedbackPreviewStorage,
    );
    let rollback =
        tracedecay_agent_hosts::agents::host_bundle::FeedbackPathRollbackSwitchV1::new(lifecycle);
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

#[hotpath::measure(label = "cli.agent.feedback")]
fn feedback_rollback_apply(
    agent_id: &str,
    state_path: &Path,
) -> tracedecay_domain::errors::Result<()> {
    let dashboard_enabled =
        load_host_lifecycle_user_config()?.dashboard_enabled_for_agent(agent_id);
    let (home, lifecycle_root, aggregate, target) = feedback_rollback_inputs(agent_id)?;
    let (previous, _previous_receipt) = live_feedback_receipt(&home, &aggregate)?;
    let previous_contents = read_feedback_repair_contents(&home, &previous)?;
    let artifact_permissions = snapshot_feedback_artifact_permissions(&home, &previous)?;
    let integration = tracedecay_agent_hosts::agents::get_integration(agent_id)?;
    let registration_files =
        snapshot_feedback_registration(&home, integration.as_ref(), target.manifest.component)?;
    let verifier = feedback_pair_verifier(&previous, &target.manifest)?;
    let request = feedback_request(
        &target.manifest,
        tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1::Repair,
        true,
    )?;
    let identity = FeedbackRollbackIdentityV2::current(agent_id, &home, &lifecycle_root)?;
    let mut state = FeedbackRollbackCliState {
        schema_version: FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION,
        agent_id: agent_id.to_string(),
        host: target.manifest.host,
        status: FeedbackRollbackCliStatus::Prepared,
        previous_aggregate: aggregate,
        previous_manifest: previous.clone(),
        previous_contents,
        target_manifest: target.manifest.clone(),
        dashboard_enabled,
        switch_operation_id: request.operation_id,
        effect_started: false,
        registration_effect_started: false,
        registration_intent_root: lifecycle_root
            .join("feedback-registration-intents")
            .join(hex::encode(request.operation_id)),
        compensation_preserves_registration: false,
        switch_receipt: None,
        restore_operation_id: None,
        restore_effect_started: false,
        restore_receipt: None,
        identity,
        registration_files,
        artifact_permissions,
    };
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    validate_feedback_registration_snapshot(
        &home,
        integration.as_ref(),
        target.manifest.component,
        &state.registration_files,
    )?;

    let writer =
        tracedecay_agent_hosts::agents::host_bundle::HostBundleWriterV1::open_with_lifecycle_root(
            &home,
            &lifecycle_root,
        )
        .map_err(host_bundle_error)?;
    let lifecycle = tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleRuntimeV1::new(
        verifier, writer,
    );
    let mut rollback =
        tracedecay_agent_hosts::agents::host_bundle::FeedbackPathRollbackSwitchV1::new(lifecycle);
    state.effect_started = true;
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    let switch_receipt = rollback
        .feedback_rollback_switch_apply(
            &previous,
            &target.manifest,
            &request,
            &target.contents,
            &[],
        )
        .map_err(host_bundle_error)?;
    #[cfg(feature = "test-transport")]
    if std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_FEEDBACK_SWITCH").is_some() {
        std::process::abort();
    }
    state.switch_receipt = Some(switch_receipt.clone());
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    let lifecycle = rollback.into_lifecycle();
    let writer = lifecycle.into_storage();

    let context = tracedecay_agent_hosts::agents::InstallContext {
        home: home.clone(),
        tracedecay_bin: tracedecay_agent_hosts::agents::which_tracedecay()
            .unwrap_or_else(|| "tracedecay".to_string()),
        tool_permissions: tracedecay_agent_hosts::agents::expected_tool_perms()?,
        project_root: None,
        dashboard: state.dashboard_enabled,
    };
    let registration_snapshot = validate_feedback_registration_snapshot(
        &home,
        integration.as_ref(),
        target.manifest.component,
        &state.registration_files,
    );
    let registration_effect_attempted = registration_snapshot.is_ok();
    let registration_result = match registration_snapshot {
        Err(error) => Err(error),
        Ok(()) => {
            state.registration_effect_started = true;
            persist_feedback_state(state_path, &lifecycle_root, &state)?;
            let activation_result = tracedecay_agent_hosts::agents::with_host_config_write_intents(
                state.registration_intent_root.clone(),
                || {
                    integration.activate_deployed_host_component_registration(
                        &[target.manifest.component],
                        &context,
                    )
                },
            )
            .and_then(|()| {
                if integration.id() == "cursor" {
                    return Ok(());
                }
                let health = tracedecay_agent_hosts::agents::HealthcheckContext {
                    home: home.clone(),
                    project_path: std::env::current_dir().unwrap_or_else(|_| home.clone()),
                };
                (integration.host_component_registration(target.manifest.component, &health)
                    == tracedecay_agent_hosts::agents::host_bundle::HostBundleRegistrationStateV1::Current)
                    .then_some(())
                    .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!(
                            "{agent_id} did not verify its activated feedback registration"
                        ),
                    })
            });
            let capture_result = capture_feedback_applied_registration(
                &home,
                integration.as_ref(),
                target.manifest.component,
                &mut state.registration_files,
            );
            if capture_result.is_ok() {
                persist_feedback_state(state_path, &lifecycle_root, &state)?;
            }
            activation_result.and(capture_result)
        }
    };
    if let Err(registration_error) = registration_result {
        state.compensation_preserves_registration = !registration_effect_attempted;
        persist_feedback_state(state_path, &lifecycle_root, &state)?;
        drop(writer);
        feedback_rollback_restore(state_path)?;
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

#[hotpath::measure(label = "cli.agent.feedback")]
fn feedback_rollback_restore(state_path: &Path) -> tracedecay_domain::errors::Result<()> {
    let bytes = fs::read(state_path).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "could not read feedback rollback state {}: {error}",
                state_path.display()
            ),
        }
    })?;
    let mut state: FeedbackRollbackCliState = serde_json::from_slice(&bytes).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("invalid feedback rollback state: {error}"),
        }
    })?;
    if !(MIN_FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION..=FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION)
        .contains(&state.schema_version)
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "unsupported feedback rollback state version".to_string(),
        });
    }
    let home = tracedecay_agent_hosts::agents::home_dir().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root =
        tracedecay_agent_hosts::agents::host_bundle::resolved_host_bundle_lifecycle_root()
            .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("could not resolve host lifecycle root: {error}"),
            })?;
    if host_kind_for_agent(&state.agent_id)? != state.host {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "feedback rollback state host does not match its integration".to_string(),
        });
    }
    state
        .identity
        .validate(&state.agent_id, &home, &lifecycle_root)?;
    let feedback_component = selected_feedback_component(&state.previous_aggregate)?;
    if state.previous_manifest.component != feedback_component
        || state.target_manifest.component != feedback_component
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "feedback rollback state component does not match its aggregate".to_string(),
        });
    }
    let integration = tracedecay_agent_hosts::agents::get_integration(&state.agent_id)?;
    let dashboard_enabled =
        load_host_lifecycle_user_config()?.dashboard_enabled_for_agent(&state.agent_id);
    if dashboard_enabled != state.dashboard_enabled {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "feedback rollback dashboard policy changed for {:?}; restore the exact policy before resuming",
                state.agent_id
            ),
        });
    }
    if state.switch_receipt.is_none() && !state.effect_started {
        validate_feedback_registration_restore(
            &home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
            false,
            Some(&state.registration_intent_root),
        )?;
        read_feedback_contents(&home, &state.previous_manifest)?;
        state.status = FeedbackRollbackCliStatus::Restored;
        persist_feedback_state(state_path, &lifecycle_root, &state)?;
        return Ok(());
    }
    if !state.compensation_preserves_registration {
        validate_feedback_registration_restore(
            &home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
            state.registration_effect_started,
            Some(&state.registration_intent_root),
        )?;
    }
    if !state.restore_effect_started
        && let Some(switch_receipt) = state.switch_receipt.as_ref()
    {
        validate_feedback_applied_artifacts(
            &home,
            &state.target_manifest,
            &switch_receipt.apply_receipt,
        )?;
    }
    let writer =
        tracedecay_agent_hosts::agents::host_bundle::HostBundleWriterV1::open_with_lifecycle_root(
            &home,
            &lifecycle_root,
        )
        .map_err(host_bundle_error)?;
    let previous_manifest_digest = state
        .previous_manifest
        .canonical_digest()
        .map_err(host_bundle_error)?;
    let committed_restore_receipt = if state.restore_effect_started {
        use tracedecay_agent_hosts::agents::host_bundle::latest_host_component_receipt_at;
        latest_host_component_receipt_at(&lifecycle_root, state.host, feedback_component)
            .map_err(host_bundle_error)?
            .filter(|receipt| {
                Some(receipt.operation_id) == state.restore_operation_id
                    && receipt.manifest_digest == previous_manifest_digest
            })
    } else {
        None
    };
    if let (Some(restore_receipt), Some(switch_receipt)) = (
        committed_restore_receipt.as_ref(),
        state.switch_receipt.as_ref(),
    ) {
        use tracedecay_agent_hosts::agents::host_bundle::latest_host_component_set_receipt_at;

        let target_aggregate = aggregate_with_feedback_component(
            &state.previous_aggregate,
            &state.target_manifest,
            &switch_receipt.apply_receipt,
        );
        let restored_aggregate = aggregate_with_feedback_component(
            &state.previous_aggregate,
            &state.previous_manifest,
            restore_receipt,
        );
        let aggregate = latest_host_component_set_receipt_at(&lifecycle_root, state.host)
            .map_err(host_bundle_error)?;
        if aggregate.as_ref() != Some(&target_aggregate)
            && aggregate.as_ref() != Some(&restored_aggregate)
        {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "feedback aggregate changed during interrupted restore".to_string(),
            });
        }
    }
    let switch_receipt = if committed_restore_receipt.is_some() {
        state.switch_receipt.clone().ok_or_else(|| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: "feedback restore effect has no switch receipt identity".to_string(),
            }
        })?
    } else if let Some(switch_receipt) = state.switch_receipt.clone() {
        let target_aggregate = aggregate_with_feedback_component(
            &state.previous_aggregate,
            &state.target_manifest,
            &switch_receipt.apply_receipt,
        );
        let expected_aggregates = if state.status == FeedbackRollbackCliStatus::Applied {
            vec![target_aggregate]
        } else {
            vec![state.previous_aggregate.clone(), target_aggregate]
        };
        validate_feedback_active_receipts(
            &lifecycle_root,
            &switch_receipt,
            feedback_component,
            &expected_aggregates,
            (state.status != FeedbackRollbackCliStatus::Applied)
                .then_some(&state.previous_aggregate),
        )?;
        switch_receipt
    } else {
        use tracedecay_agent_hosts::agents::host_bundle::{
            FeedbackPathRollbackReceiptV1, latest_host_component_receipt_at,
            latest_host_component_set_receipt_at,
        };

        if latest_host_component_set_receipt_at(&lifecycle_root, state.host)
            .map_err(host_bundle_error)?
            .as_ref()
            != Some(&state.previous_aggregate)
        {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "feedback aggregate changed during interrupted apply".to_string(),
            });
        }
        let component =
            latest_host_component_receipt_at(&lifecycle_root, state.host, feedback_component)
                .map_err(host_bundle_error)?;
        let target_manifest_digest = state
            .target_manifest
            .canonical_digest()
            .map_err(host_bundle_error)?;
        if let Some(apply_receipt) = component.filter(|receipt| {
            receipt.operation_id == state.switch_operation_id
                && receipt.manifest_digest == target_manifest_digest
        }) {
            FeedbackPathRollbackReceiptV1 {
                host: state.host,
                previous_manifest_digest: state
                    .previous_manifest
                    .canonical_digest()
                    .map_err(host_bundle_error)?,
                applied_manifest_digest: target_manifest_digest,
                apply_receipt,
            }
        } else {
            // The writer already resolved the lower-level artifact journal.
            // No feedback registration or aggregate receipt effect occurred.
            validate_feedback_registration_restore(
                &home,
                integration.as_ref(),
                feedback_component,
                &state.registration_files,
                state.registration_effect_started,
                Some(&state.registration_intent_root),
            )?;
            read_feedback_contents(&home, &state.previous_manifest)?;
            restore_feedback_artifact_permissions(&home, &state.artifact_permissions)?;
            state.status = FeedbackRollbackCliStatus::Restored;
            persist_feedback_state(state_path, &lifecycle_root, &state)?;
            return Ok(());
        }
    };
    let verifier = feedback_pair_verifier(&state.previous_manifest, &state.target_manifest)?;
    if committed_restore_receipt.is_none() && !state.compensation_preserves_registration {
        validate_feedback_registration_restore(
            &home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
            state.registration_effect_started,
            Some(&state.registration_intent_root),
        )?;
        validate_feedback_applied_artifacts(
            &home,
            &state.target_manifest,
            &switch_receipt.apply_receipt,
        )?;
    }
    let mut request = feedback_request(
        &state.previous_manifest,
        tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1::Repair,
        true,
    )?;
    if let Some(operation_id) = state.restore_operation_id {
        request.operation_id = operation_id;
    } else {
        state.restore_operation_id = Some(request.operation_id);
    }
    if !state.restore_effect_started {
        state.restore_effect_started = true;
        persist_feedback_state(state_path, &lifecycle_root, &state)?;
    }
    let lifecycle = tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleRuntimeV1::new(
        verifier, writer,
    );
    let mut rollback =
        tracedecay_agent_hosts::agents::host_bundle::FeedbackPathRollbackSwitchV1::new(lifecycle);
    let restore = if let Some(restore_receipt) = committed_restore_receipt {
        tracedecay_agent_hosts::agents::host_bundle::FeedbackPathRestoreReceiptV1 {
            switch_operation_id: switch_receipt.apply_receipt.operation_id,
            restore_receipt,
        }
    } else {
        rollback
            .feedback_rollback_switch_restore(
                &switch_receipt,
                &state.previous_manifest,
                &request,
                &state.previous_contents,
                &[],
            )
            .map_err(host_bundle_error)?
    };
    #[cfg(feature = "test-transport")]
    if std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_FEEDBACK_RESTORE").is_some() {
        std::process::abort();
    }
    state.restore_receipt = Some(restore.clone());
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    let lifecycle = rollback.into_lifecycle();
    let writer = lifecycle.into_storage();
    let context = tracedecay_agent_hosts::agents::InstallContext {
        home,
        tracedecay_bin: tracedecay_agent_hosts::agents::which_tracedecay()
            .unwrap_or_else(|| "tracedecay".to_string()),
        tool_permissions: tracedecay_agent_hosts::agents::expected_tool_perms()?,
        project_root: None,
        dashboard: state.dashboard_enabled,
    };
    if !state.compensation_preserves_registration {
        restore_feedback_registration(
            &context.home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
            state.registration_effect_started,
            Some(&state.registration_intent_root),
        )?;
    }
    restore_feedback_artifact_permissions(&context.home, &state.artifact_permissions)?;
    if !state.compensation_preserves_registration {
        validate_feedback_registration_snapshot(
            &context.home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
        )?;
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

#[cfg(test)]
mod tests {
    fn pinned_host_profile() -> tracedecay_runtime_core::config::PinnedUserDataDir {
        tracedecay_runtime_core::config::PinnedUserDataDir::new()
    }

    #[test]
    fn feedback_registration_snapshot_rejects_pre_activation_edit() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let integration = tracedecay_agent_hosts::agents::get_integration("opencode").unwrap();
        let registration_paths = super::feedback_registration_paths(
            home.path(),
            integration.as_ref(),
            tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1::Core,
        )
        .unwrap();
        let config = home.path().join(".config/opencode/opencode.json");
        assert!(
            registration_paths.contains(&config),
            "the mutated file must be part of the host registration inventory: {registration_paths:?}"
        );
        let snapshot = super::snapshot_feedback_registration(
            home.path(),
            integration.as_ref(),
            tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1::Core,
        )
        .unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            br#"{"mcpServers":{"operator":{"command":"foreign"}}}"#,
        )
        .unwrap();

        let error = super::validate_feedback_registration_snapshot(
            home.path(),
            integration.as_ref(),
            tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1::Core,
            &snapshot,
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed before apply"));
        assert_eq!(
            std::fs::read(&config).unwrap(),
            br#"{"mcpServers":{"operator":{"command":"foreign"}}}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn feedback_registration_snapshot_rejects_metadata_only_drift() {
        use std::os::unix::fs::PermissionsExt;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let integration = tracedecay_agent_hosts::agents::get_integration("opencode").unwrap();
        let config = home.path().join(".config/opencode/opencode.json");
        assert!(
            super::feedback_registration_paths(
                home.path(),
                integration.as_ref(),
                tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1::Core,
            )
            .unwrap()
            .contains(&config),
            "the mutated file must be part of the host registration inventory"
        );
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            br#"{"mcpServers":{"operator":{"command":"keep"}}}"#,
        )
        .unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).unwrap();
        let snapshot = super::snapshot_feedback_registration(
            home.path(),
            integration.as_ref(),
            tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1::Core,
        )
        .unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = super::validate_feedback_registration_snapshot(
            home.path(),
            integration.as_ref(),
            tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1::Core,
            &snapshot,
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed before apply"));
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
