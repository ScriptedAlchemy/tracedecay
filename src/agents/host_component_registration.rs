//! Receipt-backed host-native registration lifecycle shared by CLI and daemon owners.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub struct HostComponentRegistrationDelegate {
    integration: Box<dyn crate::agents::AgentIntegration>,
    context: crate::agents::InstallContext,
    health_context: crate::agents::HealthcheckContext,
    lifecycle_root: PathBuf,
    registration_path: Option<PathBuf>,
    project_path: Option<PathBuf>,
    operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    should_apply: bool,
    confirmed_registration_revision: Option<[u8; 32]>,
    registration_stage_completed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompatibilityRegistrationMode {
    /// The component artifacts are discovered directly by the host. The
    /// transaction's artifact verification is the complete lifecycle.
    ArtifactOnly,
    /// The host needs only a native registry entry after assets are deployed.
    DeployedActivation,
    /// Core-only compatibility hosts still use their bounded legacy editor.
    LegacyIntegration,
}

impl HostComponentRegistrationDelegate {
    pub fn new(
        agent_id: &str,
        home: &Path,
        lifecycle_root: &Path,
        operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    ) -> crate::errors::Result<Self> {
        let tracedecay_bin =
            crate::agents::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
        Self::new_with_tracedecay_bin(agent_id, home, lifecycle_root, operation, tracedecay_bin)
    }

    pub fn new_with_tracedecay_bin(
        agent_id: &str,
        home: &Path,
        lifecycle_root: &Path,
        operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
        tracedecay_bin: String,
    ) -> crate::errors::Result<Self> {
        let project_path = std::env::current_dir().unwrap_or_else(|_| home.to_path_buf());
        let integration = crate::agents::get_integration(agent_id)?;
        let registration_path = integration.primary_config_path(home);
        Ok(Self {
            integration,
            context: crate::agents::InstallContext {
                home: home.to_path_buf(),
                tracedecay_bin,
                tool_permissions: crate::agents::expected_tool_perms(),
                project_root: None,
                dashboard: true,
            },
            health_context: crate::agents::HealthcheckContext {
                home: home.to_path_buf(),
                project_path,
            },
            lifecycle_root: lifecycle_root.to_path_buf(),
            registration_path,
            project_path: None,
            operation,
            should_apply: false,
            confirmed_registration_revision: None,
            registration_stage_completed: false,
        })
    }

    pub fn new_project_local(
        agent_id: &str,
        home: &Path,
        project_path: &Path,
        lifecycle_root: &Path,
        operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    ) -> crate::errors::Result<Self> {
        let integration = crate::agents::get_integration(agent_id)?;
        let registration_path = project_local_registration_path(agent_id, home, project_path);
        Ok(Self {
            integration,
            context: crate::agents::InstallContext {
                home: home.to_path_buf(),
                tracedecay_bin: crate::agents::which_tracedecay()
                    .unwrap_or_else(|| "tracedecay".to_string()),
                tool_permissions: crate::agents::expected_tool_perms(),
                project_root: Some(project_path.to_path_buf()),
                dashboard: true,
            },
            health_context: crate::agents::HealthcheckContext {
                home: home.to_path_buf(),
                project_path: project_path.to_path_buf(),
            },
            lifecycle_root: lifecycle_root.to_path_buf(),
            registration_path,
            project_path: Some(project_path.to_path_buf()),
            operation,
            should_apply: false,
            confirmed_registration_revision: None,
            registration_stage_completed: false,
        })
    }

    fn registration_error(
        error: crate::errors::TraceDecayError,
    ) -> crate::agents::host_bundle_v2::HostBundleError {
        // The transaction error vocabulary is fixed, so surface the
        // integration's own message here before it is collapsed into the
        // generic storage failure — otherwise the actionable cause (for
        // example a refused symlinked project config) is lost.
        eprintln!("{error}");
        crate::agents::host_bundle_v2::HostBundleError::StorageFailure
    }

    fn backup_dir(&self, operation_id: [u8; 16]) -> PathBuf {
        self.lifecycle_root
            .join(".tracedecay-host-bundle-v1")
            .join("registration-backups")
            .join(hex::encode(operation_id))
            .join(self.integration.id())
    }

    fn backup_path(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("registration-{index}"))
    }

    fn missing_marker_path(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("registration-{index}.missing"))
    }

    fn registration_path_marker(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("registration-{index}.path.json"))
    }

    fn apply_marker_path(&self, operation_id: [u8; 16]) -> PathBuf {
        self.backup_dir(operation_id).join("apply")
    }

    fn should_apply_from_backup(&self, operation_id: [u8; 16]) -> bool {
        fs::read(self.apply_marker_path(operation_id))
            .ok()
            .is_some_and(|bytes| bytes == b"1")
    }

    fn registration_mode(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> CompatibilityRegistrationMode {
        let includes_core = component_set.components.iter().any(|component| {
            component.manifest.component
                == crate::agents::host_bundle_v2::HostBundleComponentV1::Core
        });
        if self.project_path.is_some() {
            CompatibilityRegistrationMode::LegacyIntegration
        } else if component_set.host == crate::agents::host_bundle_v2::HostKindV1::KimiCode
            || (component_set.host == crate::agents::host_bundle_v2::HostKindV1::OpenCode
                && component_set.components.iter().any(|component| {
                    matches!(
                        component.manifest.component,
                        crate::agents::host_bundle_v2::HostBundleComponentV1::Core
                            | crate::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp
                    )
                }))
        {
            CompatibilityRegistrationMode::DeployedActivation
        } else if component_set.host == crate::agents::host_bundle_v2::HostKindV1::OpenCode
            || !includes_core
        {
            CompatibilityRegistrationMode::ArtifactOnly
        } else {
            CompatibilityRegistrationMode::LegacyIntegration
        }
    }

    /// Whether managed artifact bytes are the component set's complete host
    /// lifecycle. Artifact backup/restore must refuse every other mode because
    /// it intentionally does not snapshot or reconcile native registration.
    pub fn supports_artifact_only_backup_restore(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> bool {
        self.registration_mode(component_set) == CompatibilityRegistrationMode::ArtifactOnly
    }

    fn requires_competing_analyzer_preflight(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> bool {
        component_set.host == crate::agents::host_bundle_v2::HostKindV1::OpenCode
            && component_set.components.iter().any(|component| {
                component.manifest.component
                    == crate::agents::host_bundle_v2::HostBundleComponentV1::Core
            })
    }

    fn component_registration_revision(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Result<[u8; 32], crate::agents::host_bundle_v2::HostBundleError> {
        match self.registration_mode(component_set) {
            CompatibilityRegistrationMode::ArtifactOnly
                if !self.requires_competing_analyzer_preflight(component_set) =>
            {
                Ok(Sha256::digest(b"tracedecay.host-registration.none.v1").into())
            }
            CompatibilityRegistrationMode::ArtifactOnly
            | CompatibilityRegistrationMode::DeployedActivation
            | CompatibilityRegistrationMode::LegacyIntegration => {
                self.current_registration_revision(component_set)
            }
        }
    }

    /// Refuse the one genuinely undecidable case: a non-`TraceDecay` LSP key
    /// whose command runs the `TraceDecay` binary. Ownership cannot be
    /// resolved from the host document, so the lifecycle stops rather than
    /// offering the operator a claim to confirm. An unreadable or unparseable
    /// document stops here too instead of passing as clear.
    fn refuse_ambiguous_opencode_analyzer(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        if !self.requires_competing_analyzer_preflight(component_set) {
            return Ok(());
        }
        let Some((config, _)) = self.opencode_registration_document(component_set)? else {
            return Ok(());
        };
        let aliased = config
            .get("lsp")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|servers| {
                servers.iter().any(|(name, registration)| {
                    name != "tracedecay"
                        && registration
                            .get("command")
                            .is_some_and(|command| command.to_string().contains("tracedecay"))
                })
            });
        if aliased {
            return Err(crate::agents::host_bundle_v2::HostBundleError::OwnershipConflict);
        }
        Ok(())
    }

    /// Parse the host's own registration document once. An unreadable or
    /// unparseable document is a refusal rather than "no conflict": discovery
    /// that cannot see the surface must never report it as clear.
    fn opencode_registration_document(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Result<Option<(serde_json::Value, [u8; 32])>, crate::agents::host_bundle_v2::HostBundleError>
    {
        if component_set.host != crate::agents::host_bundle_v2::HostKindV1::OpenCode {
            return Ok(None);
        }
        let Some(path) = &self.registration_path else {
            return Ok(None);
        };
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure),
        };
        let config = serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::InvalidObservedState)?;
        Ok(Some((config, Sha256::digest(&bytes).into())))
    }

    /// Third-party analyzers already registered for a language this component
    /// set's own analyzer would serve. OpenCode is the only host whose set
    /// registers a custom analyzer; every other host's component set writes
    /// TraceDecay-keyed entries that no third party can already own.
    fn competing_opencode_analyzer_claims(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Result<
        Vec<crate::agents::host_bundle_v2::CompetingHostExtensionClaimV1>,
        crate::agents::host_bundle_v2::HostBundleError,
    > {
        if !self.requires_competing_analyzer_preflight(component_set) {
            return Ok(Vec::new());
        }
        let Some((config, evidence_digest)) = self.opencode_registration_document(component_set)?
        else {
            return Ok(Vec::new());
        };
        let tracedecay_extensions = opencode_tracedecay_extensions(component_set);
        let Some(servers) = config.get("lsp").and_then(serde_json::Value::as_object) else {
            return Ok(Vec::new());
        };
        Ok(servers
            .iter()
            .filter(|(name, _)| name.as_str() != "tracedecay")
            .filter(|(_, registration)| {
                claims_any_extension(registration, tracedecay_extensions.as_deref())
            })
            .map(
                |(name, _)| crate::agents::host_bundle_v2::CompetingHostExtensionClaimV1 {
                    extension_id: claim_identifier(name),
                    capability: crate::agents::host_bundle_v2::HostCapabilityV1::Lsp,
                    evidence_digest,
                },
            )
            .collect())
    }

    fn registration_is_current(
        &self,
        component: crate::agents::host_bundle_v2::HostBundleComponentV1,
    ) -> crate::agents::host_bundle_v2::HostBundleRegistrationStateV1 {
        if self.project_path.is_none() {
            return self
                .integration
                .host_component_registration(component, &self.health_context);
        }
        let Some(path) = &self.registration_path else {
            return crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Missing;
        };
        let Ok(contents) = fs::read_to_string(path) else {
            return crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Missing;
        };
        if contents.contains("tracedecay") {
            crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Current
        } else {
            crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Missing
        }
    }

    fn registration_paths(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Vec<PathBuf> {
        if self.project_path.is_some() {
            return self.registration_path.iter().cloned().collect();
        }
        let components = component_set
            .components
            .iter()
            .map(|component| component.manifest.component)
            .collect::<Vec<_>>();
        let mut paths = self
            .integration
            .host_component_registration_paths(&components, &self.context.home);
        paths.sort();
        paths.dedup();
        paths
    }

    fn current_registration_revision(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Result<[u8; 32], crate::agents::host_bundle_v2::HostBundleError> {
        let mut digest = Sha256::new();
        digest.update(b"tracedecay.host-registration.revision.v1");
        digest.update((self.integration.id().len() as u64).to_be_bytes());
        digest.update(self.integration.id().as_bytes());
        let registration_paths = self.registration_paths(component_set);
        if !registration_paths.is_empty() {
            for (index, path) in registration_paths.iter().enumerate() {
                digest.update((index as u64).to_be_bytes());
                digest.update((path.as_os_str().len() as u64).to_be_bytes());
                digest.update(path.as_os_str().as_encoded_bytes());
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                        return Err(
                            crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath,
                        );
                    }
                    Ok(_) => {
                        let bytes = fs::read(path).map_err(|_| {
                            crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                        })?;
                        digest.update(b"file");
                        digest.update((bytes.len() as u64).to_be_bytes());
                        digest.update(bytes);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        digest.update(b"missing");
                    }
                    Err(_) => {
                        return Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure);
                    }
                }
            }
        } else {
            digest.update(b"typed-state");
            let mut components = component_set
                .components
                .iter()
                .map(|component| component.manifest.component)
                .collect::<Vec<_>>();
            components.sort_unstable();
            for component in components {
                digest.update([match self.registration_is_current(component) {
                    crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Current => 1,
                    crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Repairable => 2,
                    crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Missing => 3,
                    crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Corrupt => 4,
                }]);
            }
        }
        Ok(digest.finalize().into())
    }

    fn backup_registration(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        let backup_dir = self.backup_dir(operation_id);
        fs::create_dir_all(&backup_dir)
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
        fs::write(
            self.apply_marker_path(operation_id),
            if self.should_apply { b"1" } else { b"0" },
        )
        .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
        for (index, path) in self.registration_paths(component_set).iter().enumerate() {
            let path_bytes = serde_json::to_vec(path)
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
            fs::write(
                self.registration_path_marker(operation_id, index),
                path_bytes,
            )
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath);
                }
                Ok(_) => {
                    let bytes = fs::read(path).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                    })?;
                    write_registration_backup(&self.backup_path(operation_id, index), &bytes)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::write(self.missing_marker_path(operation_id, index), b"missing").map_err(
                        |_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure,
                    )?;
                }
                Err(_) => {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure);
                }
            }
        }
        Ok(())
    }

    fn restore_registration(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        let mut persisted_paths = Vec::new();
        for index in 0.. {
            match fs::read(self.registration_path_marker(operation_id, index)) {
                Ok(bytes) => {
                    let path = serde_json::from_slice::<PathBuf>(&bytes).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                    })?;
                    persisted_paths.push(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure);
                }
            }
        }
        let registration_paths = if persisted_paths.is_empty() {
            self.registration_paths(component_set)
        } else {
            persisted_paths
        };
        for (index, path) in registration_paths.iter().enumerate() {
            let backup = self.backup_path(operation_id, index);
            let missing = self.missing_marker_path(operation_id, index);
            if backup.is_file() {
                let bytes = fs::read(&backup)
                    .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
                write_registration_backup(path, &bytes)?;
            } else if missing.is_file() {
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_file() => fs::remove_file(path)
                        .map_err(|_| {
                            crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                        })?,
                    Ok(_) => {
                        return Err(
                            crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath,
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => {
                        return Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure);
                    }
                }
            }
        }
        Ok(())
    }

    fn retire_backup(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        let backup_dir = self.backup_dir(operation_id);
        match fs::symlink_metadata(&backup_dir) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(backup_dir)
                    .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)
            }
            Ok(_) => Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure),
        }
    }
}

impl crate::agents::host_bundle_v2::HostComponentSetRegistrationV1
    for HostComponentRegistrationDelegate
{
    fn current_revision(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        _request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<[u8; 32], crate::agents::host_bundle_v2::HostBundleError> {
        self.component_registration_revision(component_set)
    }

    fn discover_competing_extension_claims(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        _request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<
        Vec<crate::agents::host_bundle_v2::CompetingHostExtensionClaimV1>,
        crate::agents::host_bundle_v2::HostBundleError,
    > {
        self.competing_opencode_analyzer_claims(component_set)
    }

    fn confirm_preview(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
        preview: &crate::agents::host_bundle_v2::HostComponentSetLifecyclePreviewV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        if preview.operation_id != request.operation_id
            || preview.current_registration_revision != preview.base_registration_revision
            || self.component_registration_revision(component_set)?
                != preview.base_registration_revision
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::StalePreview);
        }
        self.confirmed_registration_revision = Some(preview.base_registration_revision);
        Ok(())
    }

    fn preflight(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        _request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        self.refuse_ambiguous_opencode_analyzer(component_set)?;
        if self.registration_mode(component_set) == CompatibilityRegistrationMode::ArtifactOnly {
            self.should_apply = false;
            return Ok(());
        }
        if let Some(expected) = self.confirmed_registration_revision
            && self.current_registration_revision(component_set)? != expected
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::StalePreview);
        }
        let states = component_set
            .components
            .iter()
            .map(|component| self.registration_is_current(component.manifest.component))
            .collect::<Vec<_>>();
        let all_current = states.iter().all(|state| {
            *state == crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Current
        });
        let all_missing = states.iter().all(|state| {
            *state == crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Missing
        });
        let any_corrupt = states.iter().any(|state| {
            *state == crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Corrupt
        });
        if any_corrupt {
            return Err(crate::agents::host_bundle_v2::HostBundleError::OwnershipConflict);
        }
        self.should_apply = match self.operation {
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install => {
                if !all_current && !all_missing {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::OwnershipConflict);
                }
                all_missing
            }
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => !all_missing,
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
            | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => true,
        };
        if self.project_path.is_none()
            && component_set.host == crate::agents::host_bundle_v2::HostKindV1::KimiCode
            && self.should_apply
        {
            // Kimi's global plugin lifecycle is interactive (`/plugins`) only.
            // Project-local registration is a separate MCP/prompt integration
            // implemented by `install_local`, so it remains eligible for this
            // transaction's legacy-integration path.
            return Err(crate::agents::host_bundle_v2::HostBundleError::UnsupportedCapability);
        }
        Ok(())
    }

    fn stage(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        self.refuse_ambiguous_opencode_analyzer(component_set)?;
        if self.registration_mode(component_set) == CompatibilityRegistrationMode::ArtifactOnly {
            if let Some(expected) = self.confirmed_registration_revision
                && self.component_registration_revision(component_set)? != expected
            {
                return Err(crate::agents::host_bundle_v2::HostBundleError::StalePreview);
            }
            return Ok(());
        }
        if let Some(expected) = self.confirmed_registration_revision
            && self.current_registration_revision(component_set)? != expected
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::StalePreview);
        }
        self.backup_registration(component_set, request.operation_id)?;
        self.registration_stage_completed = true;
        Ok(())
    }

    fn apply(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        match self.registration_mode(component_set) {
            CompatibilityRegistrationMode::ArtifactOnly => return Ok(()),
            CompatibilityRegistrationMode::DeployedActivation => {
                if !self.should_apply {
                    return Ok(());
                }
                let components = component_set
                    .components
                    .iter()
                    .map(|component| component.manifest.component)
                    .collect::<Vec<_>>();
                return match request.lifecycle.operation {
                    crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => self
                        .integration
                        .deactivate_deployed_host_component_registration(&components, &self.context)
                        .map_err(Self::registration_error),
                    crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install
                    | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
                    | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => self
                        .integration
                        .activate_deployed_host_component_registration(&components, &self.context)
                        .map_err(Self::registration_error),
                };
            }
            CompatibilityRegistrationMode::LegacyIntegration => {}
        }
        if !self.should_apply {
            return Ok(());
        }
        if let Some(project_path) = &self.project_path {
            return match request.lifecycle.operation {
                crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => self
                    .integration
                    .uninstall_local(&self.context, project_path)
                    .map_err(Self::registration_error),
                crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install
                | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
                | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => self
                    .integration
                    .install_local(&self.context, project_path)
                    .map_err(Self::registration_error),
            };
        }
        match request.lifecycle.operation {
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => self
                .integration
                .uninstall(&self.context)
                .map_err(Self::registration_error),
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install
            | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
            | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => self
                .integration
                .install(&self.context)
                .map_err(Self::registration_error),
        }
    }

    fn verify(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        if self.registration_mode(component_set) == CompatibilityRegistrationMode::ArtifactOnly {
            return Ok(());
        }
        let expected = if request.lifecycle.operation
            == crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall
        {
            crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Missing
        } else {
            crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Current
        };
        if component_set
            .components
            .iter()
            .all(|component| self.registration_is_current(component.manifest.component) == expected)
        {
            Ok(())
        } else {
            Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure)
        }
    }

    fn commit(
        &mut self,
        _component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        self.retire_backup(request.operation_id)
    }

    fn rollback(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        match self.registration_mode(component_set) {
            CompatibilityRegistrationMode::ArtifactOnly => return Ok(()),
            CompatibilityRegistrationMode::DeployedActivation => {
                return self.restore_registration(component_set, request.operation_id);
            }
            CompatibilityRegistrationMode::LegacyIntegration => {}
        }
        if !self.registration_stage_completed && !self.backup_dir(request.operation_id).is_dir() {
            return Ok(());
        }
        self.should_apply |= self.should_apply_from_backup(request.operation_id);
        if !self.should_apply {
            self.restore_registration(component_set, request.operation_id)?;
            return Ok(());
        }
        if let Some(project_path) = &self.project_path {
            let result = match request.lifecycle.operation {
                crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install => self
                    .integration
                    .uninstall_local(&self.context, project_path)
                    .map_err(Self::registration_error),
                crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => self
                    .integration
                    .install_local(&self.context, project_path)
                    .map_err(Self::registration_error),
                crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
                | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => Ok(()),
            };
            result?;
            return self.restore_registration(component_set, request.operation_id);
        }
        let result = match request.lifecycle.operation {
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install => self
                .integration
                .uninstall(&self.context)
                .map_err(Self::registration_error),
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => self
                .integration
                .install(&self.context)
                .map_err(Self::registration_error),
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
            | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => Ok(()),
        };
        result?;
        self.restore_registration(component_set, request.operation_id)
    }
}

/// Languages the component set's own OpenCode analyzer registration declares.
/// `None` means the projection declares no bounded extension list, so every
/// other analyzer must be treated as overlapping.
fn opencode_tracedecay_extensions(
    component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
) -> Option<Vec<String>> {
    let registration = component_set
        .components
        .iter()
        .flat_map(|component| &component.contents)
        .find(|asset| asset.relative_path.ends_with("opencode.registration.json"))?;
    let document = serde_json::from_slice::<serde_json::Value>(&registration.bytes).ok()?;
    Some(
        document
            .pointer("/lsp/tracedecay/extensions")?
            .as_array()?
            .iter()
            .filter_map(|extension| extension.as_str().map(str::to_string))
            .collect(),
    )
}

/// Whether a third-party analyzer registration claims a language TraceDecay's
/// own analyzer would serve. An entry without a bounded `extensions` list
/// claims by host default, which cannot be proven disjoint.
fn claims_any_extension(registration: &serde_json::Value, tracedecay: Option<&[String]>) -> bool {
    let Some(tracedecay) = tracedecay else {
        return true;
    };
    let Some(extensions) = registration
        .get("extensions")
        .and_then(serde_json::Value::as_array)
    else {
        return true;
    };
    extensions.iter().any(|extension| {
        extension
            .as_str()
            .is_some_and(|extension| tracedecay.iter().any(|owned| owned == extension))
    })
}

/// Host extension names are not TraceDecay identifiers. A name the lifecycle
/// vocabulary cannot carry is still reported under a stable derived id so a
/// real conflict is never dropped for being unrepresentable.
fn claim_identifier(name: &str) -> String {
    if crate::agents::host_bundle_v2::validate_identifier(name).is_ok() {
        return name.to_string();
    }
    format!("opaque-{}", hex::encode(&Sha256::digest(name)[..8]))
}

fn write_registration_backup(
    path: &Path,
    bytes: &[u8],
) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
    let parent = path
        .parent()
        .ok_or(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath)?;
    fs::create_dir_all(parent)
        .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
    let digest = hex::encode(Sha256::digest(bytes));
    let temporary = parent.join(format!(".registration-{digest}.tmp"));
    fs::write(&temporary, bytes)
        .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
    fs::rename(&temporary, path)
        .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)
}

pub fn project_local_registration_path(
    agent_id: &str,
    home: &Path,
    project_path: &Path,
) -> Option<PathBuf> {
    match agent_id {
        "claude" => Some(project_path.join(".claude/CLAUDE.md")),
        // `install_local` deploys the Codex repo plugin bundle at the
        // repository root (`codex_repo_plugin_install_dir`), not under
        // `.codex/`.
        "codex" => Some(project_path.join("plugins/tracedecay/.codex-plugin/plugin.json")),
        // Cursor's project-local install registers the shared home plugin;
        // the project itself carries only receipt markers.
        "cursor" => Some(home.join(".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json")),
        "kimi" => Some(project_path.join(".kimi-code/mcp.json")),
        "opencode" => Some(project_path.join("opencode.json")),
        "roo-code" => Some(project_path.join(".roo/mcp.json")),
        "kilo" => Some(project_path.join("kilo.json")),
        _ => None,
    }
}
