//! Receipt-backed host-native registration lifecycle shared by CLI and daemon owners.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct RegistrationBackupIdentityV1 {
    schema_version: u16,
    integration_id: String,
    canonical_home: PathBuf,
    canonical_profile: PathBuf,
    canonical_project: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct RegistrationObservedStateV1 {
    present: bool,
    digest: [u8; 32],
    metadata: Option<crate::agents::HostFileMetadataIdentityV1>,
}

#[derive(Deserialize)]
struct HostConfigWriteIntentV2 {
    schema_version: u16,
    digest: [u8; 32],
    metadata: Option<crate::agents::HostFileMetadataIdentityV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct RegistrationMutationPlanV1 {
    schema_version: u16,
    integration_id: String,
    operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    paths: Vec<PathBuf>,
}

impl RegistrationBackupIdentityV1 {
    fn new(
        integration_id: &str,
        home: &Path,
        profile: &Path,
        project: Option<&Path>,
    ) -> Result<Self, crate::agents::host_bundle_v2::HostBundleError> {
        Ok(Self {
            schema_version: REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION,
            integration_id: integration_id.to_string(),
            canonical_home: canonical_path(home)?,
            canonical_profile: canonical_path(profile)?,
            canonical_project: project.map(canonical_path).transpose()?,
        })
    }

    fn validate(
        &self,
        integration_id: &str,
        home: &Path,
        profile: &Path,
        project: Option<&Path>,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        let observed = Self::new(integration_id, home, profile, project)?;
        (self == &observed)
            .then_some(())
            .ok_or(crate::agents::host_bundle_v2::HostBundleError::WrongTarget)
    }
}

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

    fn registration_permission_marker(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("registration-{index}.permissions.json"))
    }

    fn applied_state_marker(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("registration-{index}.applied.json"))
    }

    fn identity_path(&self, operation_id: [u8; 16]) -> PathBuf {
        self.backup_dir(operation_id).join("identity.v1.json")
    }

    fn backup_complete_path(&self, operation_id: [u8; 16]) -> PathBuf {
        self.backup_dir(operation_id).join("backup.complete")
    }

    fn registration_effect_path(&self, operation_id: [u8; 16]) -> PathBuf {
        self.backup_dir(operation_id)
            .join("registration-effect.started")
    }

    fn mutation_plan_path(&self, operation_id: [u8; 16]) -> PathBuf {
        self.backup_dir(operation_id).join("mutation-plan.v1.json")
    }

    fn write_intent_root(&self, operation_id: [u8; 16]) -> PathBuf {
        self.backup_dir(operation_id).join("write-intents")
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
        } else if component_set.host == crate::agents::host_bundle_v2::HostKindV1::Codex
            || component_set.host == crate::agents::host_bundle_v2::HostKindV1::KimiCode
            || component_set.host == crate::agents::host_bundle_v2::HostKindV1::Kiro
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
        } else if matches!(
            component_set.host,
            crate::agents::host_bundle_v2::HostKindV1::CursorDesktop
                | crate::agents::host_bundle_v2::HostKindV1::OpenCode
        ) || !includes_core
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
            || (component_set.host == crate::agents::host_bundle_v2::HostKindV1::Kiro
                && component_set.components.len() == 1
                && component_set.components[0].manifest.component
                    == crate::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp)
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
    /// set's own analyzer would serve. `OpenCode` is the only host whose set
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
        let Ok(contents) = fs::read(path) else {
            return crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Missing;
        };
        project_registration_state(self.integration.id(), &contents)
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
        tracedecay_application::sync_parent_directory(
            &backup_dir,
            tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
        )
        .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
        let identity = RegistrationBackupIdentityV1::new(
            self.integration.id(),
            &self.context.home,
            &self.lifecycle_root,
            self.project_path.as_deref(),
        )?;
        let identity_bytes = serde_json::to_vec(&identity)
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
        write_registration_backup(&self.identity_path(operation_id), &identity_bytes)?;
        let registration_paths = self.registration_paths(component_set);
        let mutation_plan = RegistrationMutationPlanV1 {
            schema_version: REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION,
            integration_id: self.integration.id().to_string(),
            operation: self.operation,
            paths: registration_paths.clone(),
        };
        let mutation_plan = serde_json::to_vec(&mutation_plan)
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
        write_registration_backup(&self.mutation_plan_path(operation_id), &mutation_plan)?;
        for (index, path) in registration_paths.iter().enumerate() {
            let path_bytes = serde_json::to_vec(path)
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
            write_registration_backup(
                &self.registration_path_marker(operation_id, index),
                &path_bytes,
            )?;
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath);
                }
                Ok(_) => {
                    let bytes = fs::read(path).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                    })?;
                    write_registration_backup(&self.backup_path(operation_id, index), &bytes)?;
                    let permissions =
                        crate::agents::capture_host_file_metadata(path).map_err(|_| {
                            crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                        })?;
                    let permissions = serde_json::to_vec(&permissions).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                    })?;
                    write_registration_backup(
                        &self.registration_permission_marker(operation_id, index),
                        &permissions,
                    )?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    write_registration_backup(
                        &self.missing_marker_path(operation_id, index),
                        b"missing",
                    )?;
                }
                Err(_) => {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure);
                }
            }
        }
        write_registration_backup(&self.backup_complete_path(operation_id), b"complete")?;
        Ok(())
    }

    fn capture_applied_registration(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        for (index, path) in self.registration_paths(component_set).iter().enumerate() {
            let observed = registration_observed_state(path)?;
            let bytes = serde_json::to_vec(&observed)
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
            write_registration_backup(&self.applied_state_marker(operation_id, index), &bytes)?;
        }
        Ok(())
    }

    fn validate_applied_registration(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        for (index, path) in self.registration_paths(component_set).iter().enumerate() {
            let expected = fs::read(self.applied_state_marker(operation_id, index))
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
            let expected: RegistrationObservedStateV1 = serde_json::from_slice(&expected)
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
            if registration_observed_state(path)? != expected {
                return Err(crate::agents::host_bundle_v2::HostBundleError::StalePreview);
            }
        }
        Ok(())
    }

    fn original_registration_state(
        &self,
        operation_id: [u8; 16],
        index: usize,
    ) -> Result<RegistrationObservedStateV1, crate::agents::host_bundle_v2::HostBundleError> {
        let backup = self.backup_path(operation_id, index);
        if backup.is_file() {
            let bytes = fs::read(backup)
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
            return Ok(RegistrationObservedStateV1 {
                present: true,
                digest: Sha256::digest(bytes).into(),
                metadata: Some(self.original_registration_permissions(operation_id, index)?),
            });
        }
        if self.missing_marker_path(operation_id, index).is_file() {
            return Ok(RegistrationObservedStateV1 {
                present: false,
                digest: [0; 32],
                metadata: None,
            });
        }
        Err(crate::agents::host_bundle_v2::HostBundleError::WrongTarget)
    }

    fn original_registration_permissions(
        &self,
        operation_id: [u8; 16],
        index: usize,
    ) -> Result<
        crate::agents::HostFileMetadataIdentityV1,
        crate::agents::host_bundle_v2::HostBundleError,
    > {
        let bytes = fs::read(self.registration_permission_marker(operation_id, index))
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)
    }

    fn intended_registration_state(
        &self,
        operation_id: [u8; 16],
        path: &Path,
    ) -> Result<Option<RegistrationObservedStateV1>, crate::agents::host_bundle_v2::HostBundleError>
    {
        match fs::read(
            crate::agents::host_config_write_intent_path(
                &self.write_intent_root(operation_id),
                path,
            )
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?,
        ) {
            Ok(intent) if intent != [0] => {
                let intent: HostConfigWriteIntentV2 = serde_json::from_slice(&intent)
                    .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
                if intent.schema_version != 2 {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::WrongTarget);
                }
                Ok(Some(RegistrationObservedStateV1 {
                    present: true,
                    digest: intent.digest,
                    metadata: intent.metadata,
                }))
            }
            Ok(intent) if intent == [0] => Ok(Some(RegistrationObservedStateV1 {
                present: false,
                digest: [0; 32],
                metadata: None,
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            _ => Err(crate::agents::host_bundle_v2::HostBundleError::WrongTarget),
        }
    }

    fn restore_registration(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        let identity_bytes = fs::read(self.identity_path(operation_id))
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
        let identity: RegistrationBackupIdentityV1 = serde_json::from_slice(&identity_bytes)
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
        identity.validate(
            self.integration.id(),
            &self.context.home,
            &self.lifecycle_root,
            self.project_path.as_deref(),
        )?;
        let mutation_plan = fs::read(self.mutation_plan_path(operation_id))
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
        let mutation_plan: RegistrationMutationPlanV1 = serde_json::from_slice(&mutation_plan)
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
        if mutation_plan.schema_version != REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION
            || mutation_plan.integration_id != self.integration.id()
            || mutation_plan.operation != self.operation
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::WrongTarget);
        }
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
        let registration_paths = self.registration_paths(component_set);
        if mutation_plan.paths != registration_paths || persisted_paths != registration_paths {
            return Err(crate::agents::host_bundle_v2::HostBundleError::WrongTarget);
        }
        for (index, path) in registration_paths.iter().enumerate() {
            let observed = registration_observed_state(path)?;
            let original = self.original_registration_state(operation_id, index)?;
            if observed != original
                && self
                    .intended_registration_state(operation_id, path)?
                    .as_ref()
                    != Some(&observed)
            {
                return Err(crate::agents::host_bundle_v2::HostBundleError::StalePreview);
            }
        }
        for (index, path) in registration_paths.iter().enumerate() {
            let observed = registration_observed_state(path)?;
            let original = self.original_registration_state(operation_id, index)?;
            let backup = self.backup_path(operation_id, index);
            let missing = self.missing_marker_path(operation_id, index);
            if backup.is_file() {
                if observed != original {
                    let bytes = fs::read(&backup).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                    })?;
                    crate::agents::safe_write_bytes_file(path, &bytes, None).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                    })?;
                    #[cfg(feature = "test-transport")]
                    if std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_REGISTRATION_ROLLBACK_WRITE")
                        .is_some()
                        || std::env::var_os(
                            "TRACEDECAY_TEST_ABORT_AFTER_REGISTRATION_ROLLBACK_WRITE_PATH",
                        )
                        .is_some_and(|expected| Path::new(&expected) == path)
                    {
                        std::process::abort();
                    }
                }
                let permissions = fs::read(
                    self.registration_permission_marker(operation_id, index),
                )
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
                let permissions: crate::agents::HostFileMetadataIdentityV1 =
                    serde_json::from_slice(&permissions).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                    })?;
                crate::agents::restore_host_file_metadata(path, &permissions)
                    .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
                sync_registration_metadata(path)?;
            } else if missing.is_file() {
                if observed == original {
                    continue;
                }
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

fn project_registration_state(
    integration_id: &str,
    contents: &[u8],
) -> crate::agents::host_bundle_v2::HostBundleRegistrationStateV1 {
    use crate::agents::host_bundle_v2::HostBundleRegistrationStateV1 as State;

    if integration_id == "claude" {
        return match std::str::from_utf8(contents) {
            Ok(text) if text.contains("tracedecay") => State::Current,
            Ok(_) => State::Missing,
            Err(_) => State::Corrupt,
        };
    }
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(contents) else {
        return State::Corrupt;
    };
    let current = match integration_id {
        "codex" => document.get("name").and_then(serde_json::Value::as_str) == Some("tracedecay"),
        "opencode" => document.pointer("/mcp/tracedecay").is_some(),
        "kimi" | "roo-code" | "kilo" => document.pointer("/mcpServers/tracedecay").is_some(),
        _ => false,
    };
    if current {
        State::Current
    } else {
        State::Missing
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
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        self.refuse_ambiguous_opencode_analyzer(component_set)?;
        if self.registration_mode(component_set) == CompatibilityRegistrationMode::ArtifactOnly {
            self.should_apply = false;
            return Ok(());
        }
        if (self.project_path.is_none()
            && component_set.host == crate::agents::host_bundle_v2::HostKindV1::Codex
            && request.lifecycle.operation
                != crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall)
            || (self.project_path.is_some()
                && component_set.host == crate::agents::host_bundle_v2::HostKindV1::CursorDesktop)
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::UnsupportedCapability);
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
        Ok(())
    }

    fn apply(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        let mode = self.registration_mode(component_set);
        if mode == CompatibilityRegistrationMode::ArtifactOnly {
            return Ok(());
        }
        if let Some(expected) = self.confirmed_registration_revision
            && self.current_registration_revision(component_set)? != expected
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::StalePreview);
        }
        if !self.should_apply {
            return self.capture_applied_registration(component_set, request.operation_id);
        }
        write_registration_backup(
            &self.registration_effect_path(request.operation_id),
            b"started",
        )?;
        let result =
            crate::agents::with_host_config_write_intents(
                self.write_intent_root(request.operation_id),
                || match mode {
                    CompatibilityRegistrationMode::ArtifactOnly => unreachable!(),
                    CompatibilityRegistrationMode::DeployedActivation => {
                        let components = component_set
                            .components
                            .iter()
                            .map(|component| component.manifest.component)
                            .collect::<Vec<_>>();
                        match request.lifecycle.operation {
                            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => {
                                self.integration
                                    .deactivate_deployed_host_component_registration(
                                        &components,
                                        &self.context,
                                    )
                                    .map_err(Self::registration_error)
                            }
                            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install
                            | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
                            | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => {
                                self.integration
                                    .activate_deployed_host_component_registration(
                                        &components,
                                        &self.context,
                                    )
                                    .map_err(Self::registration_error)
                            }
                        }
                    }
                    CompatibilityRegistrationMode::LegacyIntegration => {
                        if let Some(project_path) = &self.project_path {
                            match request.lifecycle.operation {
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
                    }
                        } else {
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
                    }
                },
            );
        let captured = self.capture_applied_registration(component_set, request.operation_id);
        match (result, captured) {
            (_, Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn verify(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        #[cfg(feature = "test-transport")]
        if std::env::var_os("TRACEDECAY_TEST_FAIL_HOST_REGISTRATION_VERIFY").is_some() {
            return Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure);
        }
        if self.registration_mode(component_set) == CompatibilityRegistrationMode::ArtifactOnly {
            return Ok(());
        }
        self.validate_applied_registration(component_set, request.operation_id)?;
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
        if self.registration_mode(component_set) == CompatibilityRegistrationMode::ArtifactOnly {
            return Ok(());
        }
        if !self.backup_complete_path(request.operation_id).is_file()
            || !self
                .registration_effect_path(request.operation_id)
                .is_file()
        {
            return Ok(());
        }
        self.restore_registration(component_set, request.operation_id)
    }
}

/// Languages the component set's own `OpenCode` analyzer registration declares.
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

/// Whether a third-party analyzer registration claims a language `TraceDecay`'s
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

/// Host extension names are not `TraceDecay` identifiers. A name the lifecycle
/// vocabulary cannot carry is still reported under a stable derived id so a
/// real conflict is never dropped for being unrepresentable.
fn claim_identifier(name: &str) -> String {
    if crate::agents::host_bundle_v2::validate_identifier(name).is_ok() {
        return name.to_string();
    }
    format!("opaque-{}", hex::encode(&Sha256::digest(name)[..8]))
}

fn canonical_path(path: &Path) -> Result<PathBuf, crate::agents::host_bundle_v2::HostBundleError> {
    fs::canonicalize(path)
        .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)
}

fn registration_observed_state(
    path: &Path,
) -> Result<RegistrationObservedStateV1, crate::agents::host_bundle_v2::HostBundleError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath)
        }
        Ok(_) => {
            let bytes = fs::read(path)
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)?;
            Ok(RegistrationObservedStateV1 {
                present: true,
                digest: Sha256::digest(bytes).into(),
                metadata: Some(
                    crate::agents::capture_host_file_metadata(path).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::StorageFailure
                    })?,
                ),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(RegistrationObservedStateV1 {
                present: false,
                digest: [0; 32],
                metadata: None,
            })
        }
        Err(_) => Err(crate::agents::host_bundle_v2::HostBundleError::StorageFailure),
    }
}

fn sync_registration_metadata(
    path: &Path,
) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .and_then(|()| {
            tracedecay_application::sync_parent_directory(
                path,
                tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
            )
        })
        .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)
}

fn write_registration_backup(
    path: &Path,
    bytes: &[u8],
) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
    tracedecay_application::atomic_write(
        path,
        "host-registration-state",
        bytes,
        tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::StorageFailure)
}

pub fn project_local_registration_path(
    agent_id: &str,
    _home: &Path,
    project_path: &Path,
) -> Option<PathBuf> {
    match agent_id {
        "claude" => Some(project_path.join(".claude/CLAUDE.md")),
        // `install_local` deploys the Codex repo plugin bundle at the
        // repository root (`codex_repo_plugin_install_dir`), not under
        // `.codex/`.
        "codex" => Some(project_path.join("plugins/tracedecay/.codex-plugin/plugin.json")),
        // Cursor exposes no project-local registration surface. The shared
        // home plugin must never be inventoried as project-owned state.
        "cursor" => None,
        "kimi" => Some(project_path.join(".kimi-code/mcp.json")),
        "opencode" => Some(project_path.join("opencode.json")),
        "roo-code" => Some(project_path.join(".roo/mcp.json")),
        "kilo" => Some(project_path.join("kilo.json")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::host_bundle_v2::HostBundleError;

    #[test]
    fn project_registration_rejects_corrupt_structured_config() {
        use crate::agents::host_bundle_v2::HostBundleRegistrationStateV1 as State;

        assert_eq!(
            project_registration_state("opencode", br#"{"mcp":{"tracedecay":"#),
            State::Corrupt
        );
        assert_eq!(
            project_registration_state(
                "opencode",
                br#"{"mcp":{"tracedecay":{"command":"tracedecay"}}}"#
            ),
            State::Current
        );
    }

    #[test]
    fn rollback_identity_rejects_other_home_profile_project_and_integration() {
        let home = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let identity = RegistrationBackupIdentityV1::new(
            "codex",
            home.path(),
            profile.path(),
            Some(project.path()),
        )
        .unwrap();

        assert_eq!(
            identity.validate("codex", home.path(), profile.path(), Some(project.path())),
            Ok(())
        );
        for result in [
            identity.validate("codex", other.path(), profile.path(), Some(project.path())),
            identity.validate("codex", home.path(), other.path(), Some(project.path())),
            identity.validate("codex", home.path(), profile.path(), Some(other.path())),
            identity.validate("cursor", home.path(), profile.path(), Some(project.path())),
        ] {
            assert_eq!(result, Err(HostBundleError::WrongTarget));
        }
    }
}
