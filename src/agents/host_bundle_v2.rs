//! Manifest-driven host bundle lifecycle contracts (Plan 27 PR13).
//!
//! This module plans host-registration mutations only after an external Plan
//! 20 verifier accepts the signed manifest. It contains no signing key,
//! credential, daemon lifecycle, product semantics, or host-specific business
//! authority.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const HOST_BUNDLE_SCHEMA_VERSION: u16 = 1;
const MAX_MANIFEST_ARTIFACTS: usize = 128;
const MAX_RELATIVE_PATH_BYTES: usize = 512;
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_SIGNATURE_BYTES: usize = 256;

/// Hosts and host surfaces covered by the five-host stock conformance set.
/// Cursor desktop/cloud remain projections of one Cursor integration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKindV1 {
    ClaudeCode,
    CursorDesktop,
    CursorCloud,
    Codex,
    Hermes,
    Kiro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleComponentV1 {
    Core,
    ContextMcp,
    OperatorMcp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleLifecycleOpV1 {
    Install,
    Update,
    Repair,
    Uninstall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapabilityV1 {
    Lsp,
    NativeDiagnostics,
    Hooks,
    Mcp,
    Cli,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapabilityUnavailableReasonV1 {
    HostApiAbsent,
    HostRegistrationUnsupported,
    NativeFixtureLimited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum HostCapabilityStateV1 {
    Supported,
    Degraded(HostCapabilityUnavailableReasonV1),
    Unavailable(HostCapabilityUnavailableReasonV1),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCapabilityRecordV1 {
    pub capability: HostCapabilityV1,
    pub state: HostCapabilityStateV1,
}

/// Static host-surface facts only. Runtime installation and availability are
/// observed elsewhere and must not be inferred from this matrix.
pub fn stock_host_capabilities(host: HostKindV1) -> [HostCapabilityRecordV1; 5] {
    use HostCapabilityStateV1::{Degraded, Supported, Unavailable};
    use HostCapabilityUnavailableReasonV1::{
        HostApiAbsent, HostRegistrationUnsupported, NativeFixtureLimited,
    };
    use HostCapabilityV1::{Cli, Hooks, Lsp, Mcp, NativeDiagnostics};

    let (lsp, native_diagnostics, hooks) = match host {
        HostKindV1::ClaudeCode => (Supported, Unavailable(HostApiAbsent), Supported),
        HostKindV1::CursorDesktop => (
            Unavailable(HostRegistrationUnsupported),
            Supported,
            Supported,
        ),
        HostKindV1::CursorCloud | HostKindV1::Codex | HostKindV1::Hermes => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Supported,
        ),
        HostKindV1::Kiro => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Degraded(NativeFixtureLimited),
        ),
    };
    [
        HostCapabilityRecordV1 {
            capability: Lsp,
            state: lsp,
        },
        HostCapabilityRecordV1 {
            capability: NativeDiagnostics,
            state: native_diagnostics,
        },
        HostCapabilityRecordV1 {
            capability: Hooks,
            state: hooks,
        },
        HostCapabilityRecordV1 {
            capability: Mcp,
            state: Supported,
        },
        HostCapabilityRecordV1 {
            capability: Cli,
            state: Supported,
        },
    ]
}

/// One generated artifact. Contents and credentials never enter the manifest;
/// the signed digest identifies bytes obtained from the verified bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleArtifactV1 {
    pub relative_path: String,
    pub artifact_digest: [u8; 32],
    pub ownership_marker: String,
}

/// Generated signed projection for one host/component. It references the one
/// integration/catalog authority and duplicates no workflow semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleManifestV1 {
    pub schema_version: u16,
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub integration_manifest_digest: [u8; 32],
    pub catalog_digest: [u8; 32],
    pub configuration_snapshot_id: String,
    pub effective_behavior_digest: [u8; 32],
    pub resolution_provenance_digest: [u8; 32],
    pub protocol_min: u16,
    pub protocol_max: u16,
    pub signer_key_id: String,
    pub signature: Vec<u8>,
    pub artifacts: Vec<HostBundleArtifactV1>,
}

impl HostBundleManifestV1 {
    pub fn validate_structure(&self) -> Result<(), HostBundleError> {
        if self.schema_version != HOST_BUNDLE_SCHEMA_VERSION {
            return Err(HostBundleError::UnsupportedManifestVersion);
        }
        if self.integration_manifest_digest == [0; 32]
            || self.catalog_digest == [0; 32]
            || self.effective_behavior_digest == [0; 32]
            || self.resolution_provenance_digest == [0; 32]
            || self.protocol_min == 0
            || self.protocol_min > self.protocol_max
        {
            return Err(HostBundleError::InvalidManifest);
        }
        validate_identifier(&self.configuration_snapshot_id)?;
        validate_identifier(&self.signer_key_id)?;
        if self.signature.is_empty() || self.signature.len() > MAX_SIGNATURE_BYTES {
            return Err(HostBundleError::InvalidManifest);
        }
        if self.artifacts.is_empty() || self.artifacts.len() > MAX_MANIFEST_ARTIFACTS {
            return Err(HostBundleError::InvalidManifest);
        }
        for (index, artifact) in self.artifacts.iter().enumerate() {
            validate_relative_install_path(Path::new(&artifact.relative_path))?;
            validate_identifier(&artifact.ownership_marker)?;
            if artifact.artifact_digest == [0; 32]
                || self.artifacts[..index]
                    .iter()
                    .any(|existing| existing.relative_path == artifact.relative_path)
            {
                return Err(HostBundleError::InvalidManifest);
            }
        }
        Ok(())
    }
}

fn validate_identifier(value: &str) -> Result<(), HostBundleError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(HostBundleError::InvalidManifest);
    }
    Ok(())
}

/// Lexically validate a manifest path. Absolute paths, parent traversal,
/// platform prefixes, NUL, and ambiguous `.` components are rejected.
pub fn validate_relative_install_path(path: &Path) -> Result<(), HostBundleError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_RELATIVE_PATH_BYTES
        || bytes.contains(&0)
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_)
                    | Component::RootDir
                    | Component::ParentDir
                    | Component::CurDir
            )
        })
    {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    Ok(())
}

/// Resolve a validated target while rejecting a symlink at the install root or
/// any already-existing path component. Missing descendants are permitted;
/// the writer must create them without following links and recheck at commit.
pub fn inspect_install_target(root: &Path, relative: &Path) -> Result<PathBuf, HostBundleError> {
    validate_relative_install_path(relative)?;
    if std::fs::symlink_metadata(root)
        .map_err(|_| HostBundleError::UnsafeInstallPath)?
        .file_type()
        .is_symlink()
    {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    let mut target = root.to_path_buf();
    for component in relative.components() {
        target.push(component.as_os_str());
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(HostBundleError::UnsafeInstallPath);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(HostBundleError::UnsafeInstallPath),
        }
    }
    Ok(target)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedArtifactKindV1 {
    Missing,
    RegularFile,
    Directory,
    Symlink,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedHostArtifactV1 {
    pub relative_path: String,
    pub kind: ObservedArtifactKindV1,
    pub artifact_digest: Option<[u8; 32]>,
    pub ownership_marker: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostArtifactActionV1 {
    Noop,
    WriteNew,
    BackupThenReplace,
    BackupThenRemove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostArtifactMutationV1 {
    pub relative_path: String,
    pub action: HostArtifactActionV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleLifecycleRequestV1 {
    pub operation: HostBundleLifecycleOpV1,
    pub expected_host: HostKindV1,
    pub expected_component: HostBundleComponentV1,
    pub explicit_confirmation: bool,
    /// Hermes has one user-profile binding. Other hosts must pass zero here;
    /// this is not an ambient profile-discovery mechanism.
    pub hermes_profile_bindings: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleMutationPlanV1 {
    pub operation: HostBundleLifecycleOpV1,
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub mutations: Vec<HostArtifactMutationV1>,
    pub rollback_required: bool,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HostBundleError {
    #[error("host capability is unsupported and must not be emulated")]
    UnsupportedCapability,
    #[error("bundle manifest schema version is unsupported")]
    UnsupportedManifestVersion,
    #[error("bundle manifest is structurally invalid")]
    InvalidManifest,
    #[error("bundle signature, trust record, or signed digests are invalid")]
    VerificationFailed,
    #[error("bundle does not address the requested host/component")]
    WrongTarget,
    #[error("lifecycle mutation requires explicit confirmation")]
    ConfirmationRequired,
    #[error("bundle ownership marker conflicts or is ambiguous")]
    OwnershipConflict,
    #[error("install target is absolute, traversing, symlinked, or otherwise unsafe")]
    UnsafeInstallPath,
    #[error("observed installation state is incomplete or duplicated")]
    InvalidObservedState,
    #[error("Hermes must bind exactly one user TraceDecay profile")]
    InvalidHermesProfileBinding,
}

/// Refuse silent emulation of unsupported/degraded host capabilities.
pub fn require_capability(
    host: HostKindV1,
    capability: HostCapabilityV1,
) -> Result<(), HostBundleError> {
    let record = stock_host_capabilities(host)
        .into_iter()
        .find(|record| record.capability == capability)
        .ok_or(HostBundleError::UnsupportedCapability)?;
    if record.state == HostCapabilityStateV1::Supported {
        Ok(())
    } else {
        Err(HostBundleError::UnsupportedCapability)
    }
}

/// Verify first, then produce a mutation-only plan. The verifier is the Plan
/// 20 authority and must check canonical signed bytes, key trust/revocation,
/// expiry, configuration/provenance digests, and protocol compatibility.
pub fn plan_lifecycle_mutation(
    manifest: &HostBundleManifestV1,
    request: &HostBundleLifecycleRequestV1,
    observed: &[ObservedHostArtifactV1],
    verify: impl FnOnce(&HostBundleManifestV1) -> Result<(), HostBundleError>,
) -> Result<HostBundleMutationPlanV1, HostBundleError> {
    manifest.validate_structure()?;
    verify(manifest).map_err(|_| HostBundleError::VerificationFailed)?;
    if manifest.host != request.expected_host || manifest.component != request.expected_component {
        return Err(HostBundleError::WrongTarget);
    }
    if !request.explicit_confirmation {
        return Err(HostBundleError::ConfirmationRequired);
    }
    match manifest.host {
        HostKindV1::Hermes if request.hermes_profile_bindings != 1 => {
            return Err(HostBundleError::InvalidHermesProfileBinding);
        }
        HostKindV1::Hermes => {}
        _ if request.hermes_profile_bindings != 0 => {
            return Err(HostBundleError::InvalidHermesProfileBinding);
        }
        _ => {}
    }

    for (index, state) in observed.iter().enumerate() {
        validate_relative_install_path(Path::new(&state.relative_path))?;
        if observed[..index]
            .iter()
            .any(|existing| existing.relative_path == state.relative_path)
        {
            return Err(HostBundleError::InvalidObservedState);
        }
    }

    let mut mutations = Vec::with_capacity(manifest.artifacts.len());
    for artifact in &manifest.artifacts {
        let state = observed
            .iter()
            .find(|state| state.relative_path == artifact.relative_path);
        let action = plan_artifact_action(request.operation, artifact, state)?;
        mutations.push(HostArtifactMutationV1 {
            relative_path: artifact.relative_path.clone(),
            action,
        });
    }
    let rollback_required = mutations.iter().any(|mutation| {
        matches!(
            mutation.action,
            HostArtifactActionV1::BackupThenReplace | HostArtifactActionV1::BackupThenRemove
        )
    });
    Ok(HostBundleMutationPlanV1 {
        operation: request.operation,
        host: manifest.host,
        component: manifest.component,
        mutations,
        rollback_required,
    })
}

fn plan_artifact_action(
    operation: HostBundleLifecycleOpV1,
    artifact: &HostBundleArtifactV1,
    state: Option<&ObservedHostArtifactV1>,
) -> Result<HostArtifactActionV1, HostBundleError> {
    let Some(state) = state else {
        return match operation {
            HostBundleLifecycleOpV1::Install
            | HostBundleLifecycleOpV1::Update
            | HostBundleLifecycleOpV1::Repair => Ok(HostArtifactActionV1::WriteNew),
            HostBundleLifecycleOpV1::Uninstall => Ok(HostArtifactActionV1::Noop),
        };
    };
    match state.kind {
        ObservedArtifactKindV1::Missing => return plan_artifact_action(operation, artifact, None),
        ObservedArtifactKindV1::Symlink | ObservedArtifactKindV1::Directory => {
            return Err(HostBundleError::UnsafeInstallPath);
        }
        ObservedArtifactKindV1::RegularFile => {}
    }
    if state.ownership_marker.as_deref() != Some(artifact.ownership_marker.as_str()) {
        return Err(HostBundleError::OwnershipConflict);
    }
    match operation {
        HostBundleLifecycleOpV1::Uninstall => Ok(HostArtifactActionV1::BackupThenRemove),
        HostBundleLifecycleOpV1::Install
        | HostBundleLifecycleOpV1::Update
        | HostBundleLifecycleOpV1::Repair => {
            if state.artifact_digest == Some(artifact.artifact_digest) {
                Ok(HostArtifactActionV1::Noop)
            } else {
                Ok(HostArtifactActionV1::BackupThenReplace)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(host: HostKindV1) -> HostBundleManifestV1 {
        HostBundleManifestV1 {
            schema_version: HOST_BUNDLE_SCHEMA_VERSION,
            host,
            component: HostBundleComponentV1::Core,
            integration_manifest_digest: [1; 32],
            catalog_digest: [2; 32],
            configuration_snapshot_id: "config.v1".to_owned(),
            effective_behavior_digest: [3; 32],
            resolution_provenance_digest: [4; 32],
            protocol_min: 1,
            protocol_max: 2,
            signer_key_id: "release-key.v1".to_owned(),
            signature: vec![5; 64],
            artifacts: vec![HostBundleArtifactV1 {
                relative_path: "plugins/tracedecay.json".to_owned(),
                artifact_digest: [6; 32],
                ownership_marker: "tracedecay.install.v1".to_owned(),
            }],
        }
    }

    fn request(
        host: HostKindV1,
        operation: HostBundleLifecycleOpV1,
    ) -> HostBundleLifecycleRequestV1 {
        HostBundleLifecycleRequestV1 {
            operation,
            expected_host: host,
            expected_component: HostBundleComponentV1::Core,
            explicit_confirmation: true,
            hermes_profile_bindings: u8::from(host == HostKindV1::Hermes),
        }
    }

    #[test]
    fn claude_and_cursor_project_only_native_diagnostic_routes() {
        assert!(require_capability(HostKindV1::ClaudeCode, HostCapabilityV1::Lsp).is_ok());
        assert_eq!(
            require_capability(HostKindV1::ClaudeCode, HostCapabilityV1::NativeDiagnostics),
            Err(HostBundleError::UnsupportedCapability)
        );
        assert!(
            require_capability(
                HostKindV1::CursorDesktop,
                HostCapabilityV1::NativeDiagnostics
            )
            .is_ok()
        );
        assert_eq!(
            require_capability(HostKindV1::CursorCloud, HostCapabilityV1::Lsp),
            Err(HostBundleError::UnsupportedCapability)
        );
    }

    #[test]
    fn unsafe_manifest_paths_are_rejected() {
        let mut bundle = manifest(HostKindV1::Codex);
        bundle.artifacts[0].relative_path = "../credentials".to_owned();
        assert_eq!(
            bundle.validate_structure(),
            Err(HostBundleError::UnsafeInstallPath)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_install_component_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("plugins")).unwrap();
        assert_eq!(
            inspect_install_target(root.path(), Path::new("plugins/tracedecay.json")),
            Err(HostBundleError::UnsafeInstallPath)
        );
    }

    #[test]
    fn signature_failure_prevents_any_mutation_plan() {
        let bundle = manifest(HostKindV1::Codex);
        let error = plan_lifecycle_mutation(
            &bundle,
            &request(HostKindV1::Codex, HostBundleLifecycleOpV1::Install),
            &[],
            |_| Err(HostBundleError::VerificationFailed),
        )
        .unwrap_err();
        assert_eq!(error, HostBundleError::VerificationFailed);
    }

    #[test]
    fn owned_replacement_is_backed_up_and_conflicts_are_refused() {
        let bundle = manifest(HostKindV1::Codex);
        let mut state = ObservedHostArtifactV1 {
            relative_path: bundle.artifacts[0].relative_path.clone(),
            kind: ObservedArtifactKindV1::RegularFile,
            artifact_digest: Some([9; 32]),
            ownership_marker: Some(bundle.artifacts[0].ownership_marker.clone()),
        };
        let plan = plan_lifecycle_mutation(
            &bundle,
            &request(HostKindV1::Codex, HostBundleLifecycleOpV1::Update),
            &[state.clone()],
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            plan.mutations[0].action,
            HostArtifactActionV1::BackupThenReplace
        );
        assert!(plan.rollback_required);

        state.ownership_marker = Some("somebody-else".to_owned());
        assert_eq!(
            plan_lifecycle_mutation(
                &bundle,
                &request(HostKindV1::Codex, HostBundleLifecycleOpV1::Update),
                &[state],
                |_| Ok(()),
            ),
            Err(HostBundleError::OwnershipConflict)
        );
    }

    #[test]
    fn uninstall_removes_only_owned_artifacts() {
        let bundle = manifest(HostKindV1::ClaudeCode);
        let state = ObservedHostArtifactV1 {
            relative_path: bundle.artifacts[0].relative_path.clone(),
            kind: ObservedArtifactKindV1::RegularFile,
            artifact_digest: Some(bundle.artifacts[0].artifact_digest),
            ownership_marker: Some(bundle.artifacts[0].ownership_marker.clone()),
        };
        let plan = plan_lifecycle_mutation(
            &bundle,
            &request(HostKindV1::ClaudeCode, HostBundleLifecycleOpV1::Uninstall),
            &[state],
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            plan.mutations[0].action,
            HostArtifactActionV1::BackupThenRemove
        );
    }

    #[test]
    fn hermes_requires_exactly_one_profile_binding() {
        let bundle = manifest(HostKindV1::Hermes);
        let mut lifecycle = request(HostKindV1::Hermes, HostBundleLifecycleOpV1::Install);
        lifecycle.hermes_profile_bindings = 2;
        assert_eq!(
            plan_lifecycle_mutation(&bundle, &lifecycle, &[], |_| Ok(())),
            Err(HostBundleError::InvalidHermesProfileBinding)
        );
    }
}
