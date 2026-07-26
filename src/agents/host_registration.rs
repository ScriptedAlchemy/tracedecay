use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::agents::host_bundle_v2::{
    CompetingHostExtensionClaimV1, HostBundleComponentV1, HostBundleError, HostBundleLifecycleOpV1,
    HostBundleRegistrationStateV1, HostCapabilityV1, HostComponentSetExecutionRequestV1,
    HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1, HostComponentSetV1,
};
use crate::agents::{
    AgentIntegration, HealthcheckContext, InstallContext, expected_tool_perms, get_integration,
    which_tracedecay,
};
use crate::errors::{Result as TraceDecayResult, TraceDecayError};

/// Production registration owner used by host component-set lifecycle transactions.
///
/// The delegate snapshots every host registration path before mutation and owns
/// verification and rollback. Both CLI and daemon Doctor repairs use this same
/// authority so they cannot drift into different host registration behavior.
pub struct CompatibilityAgentRegistrationDelegate {
    integration: Box<dyn AgentIntegration>,
    context: InstallContext,
    health_context: HealthcheckContext,
    lifecycle_root: PathBuf,
    registration_path: Option<PathBuf>,
    registration_paths: Vec<PathBuf>,
    project_path: Option<PathBuf>,
    operation: HostBundleLifecycleOpV1,
    should_apply: bool,
    confirmed_registration_revision: Option<[u8; 32]>,
    registration_stage_completed: bool,
}

impl CompatibilityAgentRegistrationDelegate {
    pub fn new(
        agent_id: &str,
        home: &Path,
        lifecycle_root: &Path,
        operation: HostBundleLifecycleOpV1,
    ) -> TraceDecayResult<Self> {
        let project_path = std::env::current_dir().unwrap_or_else(|_| home.to_path_buf());
        let integration = get_integration(agent_id)?;
        let registration_path = integration.primary_config_path(home);
        let mut registration_paths = integration.host_registration_paths(home);
        registration_paths.sort();
        registration_paths.dedup();
        Ok(Self {
            integration,
            context: InstallContext {
                home: home.to_path_buf(),
                tracedecay_bin: which_tracedecay().unwrap_or_else(|| "tracedecay".to_string()),
                tool_permissions: expected_tool_perms(),
                project_root: None,
                dashboard: true,
            },
            health_context: HealthcheckContext {
                home: home.to_path_buf(),
                project_path,
            },
            lifecycle_root: lifecycle_root.to_path_buf(),
            registration_path,
            registration_paths,
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
        operation: HostBundleLifecycleOpV1,
    ) -> TraceDecayResult<Self> {
        let integration = get_integration(agent_id)?;
        let registration_path = project_local_registration_path(agent_id, home, project_path);
        let registration_paths = registration_path.iter().cloned().collect();
        Ok(Self {
            integration,
            context: InstallContext {
                home: home.to_path_buf(),
                tracedecay_bin: which_tracedecay().unwrap_or_else(|| "tracedecay".to_string()),
                tool_permissions: expected_tool_perms(),
                project_root: Some(project_path.to_path_buf()),
                dashboard: true,
            },
            health_context: HealthcheckContext {
                home: home.to_path_buf(),
                project_path: project_path.to_path_buf(),
            },
            lifecycle_root: lifecycle_root.to_path_buf(),
            registration_path,
            registration_paths,
            project_path: Some(project_path.to_path_buf()),
            operation,
            should_apply: false,
            confirmed_registration_revision: None,
            registration_stage_completed: false,
        })
    }

    pub fn registration_path(&self) -> Option<&Path> {
        self.registration_path.as_deref()
    }

    fn registration_error(error: TraceDecayError) -> HostBundleError {
        // Preserve the integration's actionable cause before it is mapped into
        // the fixed component-set transaction error vocabulary.
        eprintln!("{error}");
        HostBundleError::StorageFailure
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

    fn apply_marker_path(&self, operation_id: [u8; 16]) -> PathBuf {
        self.backup_dir(operation_id).join("apply")
    }

    fn should_apply_from_backup(&self, operation_id: [u8; 16]) -> bool {
        fs::read(self.apply_marker_path(operation_id))
            .ok()
            .is_some_and(|bytes| bytes == b"1")
    }

    fn competing_opencode_analyzer_claim(&self) -> Option<CompetingHostExtensionClaimV1> {
        if self.integration.id() != "opencode" {
            return None;
        }
        let Some(path) = &self.registration_path else {
            return None;
        };
        let Ok(bytes) = fs::read(path) else {
            return None;
        };
        let Ok(config) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return None;
        };
        let evidence_digest = Sha256::digest(&bytes).into();
        config
            .get("lsp")
            .and_then(serde_json::Value::as_object)
            .and_then(|servers| {
                servers.iter().find_map(|(name, registration)| {
                    if name == "tracedecay" {
                        return None;
                    }
                    let aliases_tracedecay = registration
                        .get("command")
                        .is_some_and(|command| command.to_string().contains("tracedecay"));
                    let overlaps_extensions = registration
                        .get("extensions")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|extensions| {
                            extensions.iter().filter_map(serde_json::Value::as_str).any(
                                |extension| {
                                    super::opencode::TRACEDECAY_LSP_EXTENSIONS.contains(&extension)
                                },
                            )
                        });
                    (aliases_tracedecay || overlaps_extensions).then(|| {
                        CompetingHostExtensionClaimV1 {
                            extension_id: name.clone(),
                            capability: HostCapabilityV1::Lsp,
                            evidence_digest,
                        }
                    })
                })
            })
    }

    fn registration_is_current(
        &self,
        component: HostBundleComponentV1,
    ) -> HostBundleRegistrationStateV1 {
        if self.project_path.is_none() {
            return self
                .integration
                .host_component_registration(component, &self.health_context);
        }
        let Some(path) = &self.registration_path else {
            return HostBundleRegistrationStateV1::Missing;
        };
        let Ok(contents) = fs::read_to_string(path) else {
            return HostBundleRegistrationStateV1::Missing;
        };
        if contents.contains("tracedecay") {
            HostBundleRegistrationStateV1::Current
        } else {
            HostBundleRegistrationStateV1::Missing
        }
    }

    fn current_registration_revision(
        &self,
        component_set: &HostComponentSetV1,
    ) -> Result<[u8; 32], HostBundleError> {
        let mut digest = Sha256::new();
        digest.update(b"tracedecay.host-registration.revision.v1");
        digest.update((self.integration.id().len() as u64).to_be_bytes());
        digest.update(self.integration.id().as_bytes());
        if !self.registration_paths.is_empty() {
            for (index, path) in self.registration_paths.iter().enumerate() {
                digest.update((index as u64).to_be_bytes());
                digest.update((path.as_os_str().len() as u64).to_be_bytes());
                digest.update(path.as_os_str().as_encoded_bytes());
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                        return Err(HostBundleError::UnsafeInstallPath);
                    }
                    Ok(_) => {
                        let bytes = fs::read(path).map_err(|_| HostBundleError::StorageFailure)?;
                        digest.update(b"file");
                        digest.update((bytes.len() as u64).to_be_bytes());
                        digest.update(bytes);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        digest.update(b"missing");
                    }
                    Err(_) => return Err(HostBundleError::StorageFailure),
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
                    HostBundleRegistrationStateV1::Current => 1,
                    HostBundleRegistrationStateV1::Repairable => 2,
                    HostBundleRegistrationStateV1::Missing => 3,
                    HostBundleRegistrationStateV1::Corrupt => 4,
                }]);
            }
        }
        Ok(digest.finalize().into())
    }

    fn backup_registration(&self, operation_id: [u8; 16]) -> Result<(), HostBundleError> {
        let backup_dir = self.backup_dir(operation_id);
        fs::create_dir_all(&backup_dir).map_err(|_| HostBundleError::StorageFailure)?;
        fs::write(
            self.apply_marker_path(operation_id),
            if self.should_apply { b"1" } else { b"0" },
        )
        .map_err(|_| HostBundleError::StorageFailure)?;
        for (index, path) in self.registration_paths.iter().enumerate() {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    return Err(HostBundleError::UnsafeInstallPath);
                }
                Ok(_) => {
                    let bytes = fs::read(path).map_err(|_| HostBundleError::StorageFailure)?;
                    write_registration_backup(&self.backup_path(operation_id, index), &bytes)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::write(self.missing_marker_path(operation_id, index), b"missing")
                        .map_err(|_| HostBundleError::StorageFailure)?;
                }
                Err(_) => return Err(HostBundleError::StorageFailure),
            }
        }
        Ok(())
    }

    fn restore_registration(&self, operation_id: [u8; 16]) -> Result<(), HostBundleError> {
        for (index, path) in self.registration_paths.iter().enumerate() {
            let backup = self.backup_path(operation_id, index);
            let missing = self.missing_marker_path(operation_id, index);
            if backup.is_file() {
                let bytes = fs::read(&backup).map_err(|_| HostBundleError::StorageFailure)?;
                write_registration_backup(path, &bytes)?;
            } else if missing.is_file() {
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_file() => {
                        fs::remove_file(path).map_err(|_| HostBundleError::StorageFailure)?
                    }
                    Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(HostBundleError::StorageFailure),
                }
            }
        }
        Ok(())
    }

    fn retire_backup(&self, operation_id: [u8; 16]) -> Result<(), HostBundleError> {
        let backup_dir = self.backup_dir(operation_id);
        match fs::symlink_metadata(&backup_dir) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(backup_dir).map_err(|_| HostBundleError::StorageFailure)
            }
            Ok(_) => Err(HostBundleError::UnsafeInstallPath),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(HostBundleError::StorageFailure),
        }
    }
}

impl HostComponentSetRegistrationV1 for CompatibilityAgentRegistrationDelegate {
    fn current_revision(
        &self,
        component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<[u8; 32], HostBundleError> {
        self.current_registration_revision(component_set)
    }

    fn confirm_preview(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        preview: &HostComponentSetLifecyclePreviewV1,
    ) -> Result<(), HostBundleError> {
        if preview.operation_id != request.operation_id
            || preview.current_registration_revision != preview.base_registration_revision
            || self.current_registration_revision(component_set)?
                != preview.base_registration_revision
        {
            return Err(HostBundleError::StalePreview);
        }
        self.confirmed_registration_revision = Some(preview.base_registration_revision);
        Ok(())
    }

    fn preflight(
        &mut self,
        component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        if let Some(expected) = self.confirmed_registration_revision
            && self.current_registration_revision(component_set)? != expected
        {
            return Err(HostBundleError::StalePreview);
        }
        if self.competing_opencode_analyzer_claim().is_some() {
            return Err(HostBundleError::OwnershipConflict);
        }
        let states = component_set
            .components
            .iter()
            .map(|component| self.registration_is_current(component.manifest.component))
            .collect::<Vec<_>>();
        let all_current = states
            .iter()
            .all(|state| *state == HostBundleRegistrationStateV1::Current);
        let all_missing = states
            .iter()
            .all(|state| *state == HostBundleRegistrationStateV1::Missing);
        let any_corrupt = states
            .iter()
            .any(|state| *state == HostBundleRegistrationStateV1::Corrupt);
        if any_corrupt {
            return Err(HostBundleError::OwnershipConflict);
        }
        self.should_apply = match self.operation {
            HostBundleLifecycleOpV1::Install => {
                if !all_current && !all_missing {
                    return Err(HostBundleError::OwnershipConflict);
                }
                all_missing
            }
            HostBundleLifecycleOpV1::Uninstall => !all_missing,
            HostBundleLifecycleOpV1::Update | HostBundleLifecycleOpV1::Repair => true,
        };
        Ok(())
    }

    fn stage(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        if let Some(expected) = self.confirmed_registration_revision
            && self.current_registration_revision(component_set)? != expected
        {
            return Err(HostBundleError::StalePreview);
        }
        self.backup_registration(request.operation_id)?;
        self.registration_stage_completed = true;
        Ok(())
    }

    fn apply(
        &mut self,
        _component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        if !self.should_apply {
            return Ok(());
        }
        if let Some(project_path) = &self.project_path {
            return match request.lifecycle.operation {
                HostBundleLifecycleOpV1::Uninstall => self
                    .integration
                    .uninstall_local(&self.context, project_path)
                    .map_err(Self::registration_error),
                HostBundleLifecycleOpV1::Install
                | HostBundleLifecycleOpV1::Update
                | HostBundleLifecycleOpV1::Repair => self
                    .integration
                    .install_local(&self.context, project_path)
                    .map_err(Self::registration_error),
            };
        }
        match request.lifecycle.operation {
            HostBundleLifecycleOpV1::Uninstall => self
                .integration
                .uninstall(&self.context)
                .map_err(Self::registration_error),
            HostBundleLifecycleOpV1::Install
            | HostBundleLifecycleOpV1::Update
            | HostBundleLifecycleOpV1::Repair => self
                .integration
                .install(&self.context)
                .map_err(Self::registration_error),
        }
    }

    fn verify(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        let expected = if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall {
            HostBundleRegistrationStateV1::Missing
        } else {
            HostBundleRegistrationStateV1::Current
        };
        if component_set
            .components
            .iter()
            .all(|component| self.registration_is_current(component.manifest.component) == expected)
        {
            Ok(())
        } else {
            Err(HostBundleError::StorageFailure)
        }
    }

    fn commit(
        &mut self,
        _component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        self.retire_backup(request.operation_id)
    }

    fn rollback(
        &mut self,
        _component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        if !self.registration_stage_completed && !self.backup_dir(request.operation_id).is_dir() {
            return Ok(());
        }
        self.should_apply |= self.should_apply_from_backup(request.operation_id);
        if !self.should_apply {
            self.restore_registration(request.operation_id)?;
            return Ok(());
        }
        if let Some(project_path) = &self.project_path {
            let result = match request.lifecycle.operation {
                HostBundleLifecycleOpV1::Install => self
                    .integration
                    .uninstall_local(&self.context, project_path)
                    .map_err(Self::registration_error),
                HostBundleLifecycleOpV1::Uninstall => self
                    .integration
                    .install_local(&self.context, project_path)
                    .map_err(Self::registration_error),
                HostBundleLifecycleOpV1::Update | HostBundleLifecycleOpV1::Repair => Ok(()),
            };
            result?;
            return self.restore_registration(request.operation_id);
        }
        let result = match request.lifecycle.operation {
            HostBundleLifecycleOpV1::Install => self
                .integration
                .uninstall(&self.context)
                .map_err(Self::registration_error),
            HostBundleLifecycleOpV1::Uninstall => self
                .integration
                .install(&self.context)
                .map_err(Self::registration_error),
            HostBundleLifecycleOpV1::Update | HostBundleLifecycleOpV1::Repair => Ok(()),
        };
        result?;
        self.restore_registration(request.operation_id)
    }
}

fn write_registration_backup(path: &Path, bytes: &[u8]) -> Result<(), HostBundleError> {
    let parent = path.parent().ok_or(HostBundleError::UnsafeInstallPath)?;
    fs::create_dir_all(parent).map_err(|_| HostBundleError::StorageFailure)?;
    let digest = hex::encode(Sha256::digest(bytes));
    let temporary = parent.join(format!(".registration-{digest}.tmp"));
    fs::write(&temporary, bytes).map_err(|_| HostBundleError::StorageFailure)?;
    fs::rename(&temporary, path).map_err(|_| HostBundleError::StorageFailure)
}

pub fn project_local_registration_path(
    agent_id: &str,
    home: &Path,
    project_path: &Path,
) -> Option<PathBuf> {
    match agent_id {
        "claude" => Some(project_path.join(".claude/CLAUDE.md")),
        "codex" => Some(project_path.join("plugins/tracedecay/.codex-plugin/plugin.json")),
        "cursor" => Some(home.join(".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json")),
        "kimi" => Some(project_path.join(".kimi-code/mcp.json")),
        "opencode" => Some(project_path.join("opencode.json")),
        "roo-code" => Some(project_path.join(".roo/mcp.json")),
        "kilo" => Some(project_path.join("kilo.json")),
        _ => None,
    }
}
