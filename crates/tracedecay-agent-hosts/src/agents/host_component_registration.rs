//! Receipt-backed host-native registration lifecycle shared by CLI and daemon owners.

use std::collections::BTreeSet;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_host_integration::host_bundle_stale_preview;
use tracedecay_host_integration::host_bundle_storage_failure;

const REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION: u16 = 2;

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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct RegistrationDirectoryAppliedStateV2 {
    metadata: crate::agents::HostFileMetadataIdentityV1,
    unix_identity: Option<(u64, u64)>,
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
    directories: Vec<PathBuf>,
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
        if self.schema_version != REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION {
            return Err(crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat);
        }
        let observed = Self::new(integration_id, home, profile, project)?;
        (self.integration_id == observed.integration_id
            && self.canonical_home == observed.canonical_home
            && self.canonical_profile == observed.canonical_profile
            && self.canonical_project == observed.canonical_project)
            .then_some(())
            .ok_or(crate::agents::host_bundle_v2::HostBundleError::WrongTarget)
    }
}

pub struct CatalogHostComponentRegistrationAuthority {
    integration: Box<dyn crate::agents::AgentIntegration>,
    context: crate::agents::InstallContext,
    health_context: crate::agents::HealthcheckContext,
    lifecycle_root: PathBuf,
    registration_path: Option<PathBuf>,
    project_path: Option<PathBuf>,
    operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    should_apply: bool,
    confirmed_registration_revision: Option<[u8; 32]>,
    /// Absolute paths the surrounding transaction declared it will write
    /// itself. A host whose registration surface *is* a managed artifact
    /// (Kiro's `~/.kiro/settings/mcp.json` is both) would otherwise read its
    /// own declared write back as foreign drift.
    declared_artifact_writes: BTreeSet<PathBuf>,
    /// Revision of every registration path *outside* `declared_artifact_writes`
    /// as observed at `stage`, i.e. the last moment before the transaction
    /// writes its own artifacts. `apply` compares against this so that only a
    /// genuinely foreign edit invalidates the transaction.
    staged_foreign_registration_revision: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CatalogRegistrationMode {
    /// The component artifacts are discovered directly by the host. The
    /// transaction's artifact verification is the complete lifecycle.
    ArtifactOnly,
    /// The host needs only a native registry entry after assets are deployed.
    DeployedActivation,
    /// The host exposes a bounded project-scoped registration projection.
    ProjectRegistration,
}

impl CatalogHostComponentRegistrationAuthority {
    fn validate_catalog_host(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        (self.integration.id() == crate::agents::integration_id_for_host(component_set.host))
            .then_some(())
            .ok_or(crate::agents::host_bundle_v2::HostBundleError::WrongTarget)
    }

    fn rollback_project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    pub fn new(
        agent_id: &str,
        home: &Path,
        lifecycle_root: &Path,
        operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    ) -> crate::errors::Result<Self> {
        let tracedecay_bin = current_tracedecay_binary()?;
        Self::new_with_tracedecay_bin(agent_id, home, lifecycle_root, operation, tracedecay_bin)
    }

    pub fn new_with_tracedecay_bin(
        agent_id: &str,
        home: &Path,
        lifecycle_root: &Path,
        operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
        tracedecay_bin: String,
    ) -> crate::errors::Result<Self> {
        Self::new_with_tracedecay_bin_and_dashboard(
            agent_id,
            home,
            lifecycle_root,
            operation,
            tracedecay_bin,
            true,
        )
    }

    pub fn new_with_tracedecay_bin_and_dashboard(
        agent_id: &str,
        home: &Path,
        lifecycle_root: &Path,
        operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
        tracedecay_bin: String,
        dashboard: bool,
    ) -> crate::errors::Result<Self> {
        let project_path =
            std::env::current_dir().map_err(|error| crate::errors::TraceDecayError::Config {
                message: format!("failed to resolve host lifecycle project path: {error}"),
            })?;
        let integration = crate::agents::get_integration(agent_id)?;
        let registration_path = integration.primary_config_path(home);
        Ok(Self {
            integration,
            context: crate::agents::InstallContext {
                home: home.to_path_buf(),
                tracedecay_bin,
                tool_permissions: crate::agents::expected_tool_perms(),
                project_root: None,
                dashboard,
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
            declared_artifact_writes: BTreeSet::new(),
            staged_foreign_registration_revision: None,
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
        let tracedecay_bin = current_tracedecay_binary()?;
        Ok(Self {
            integration,
            context: crate::agents::InstallContext {
                home: home.to_path_buf(),
                tracedecay_bin,
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
            declared_artifact_writes: BTreeSet::new(),
            staged_foreign_registration_revision: None,
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
        host_bundle_storage_failure!()
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

    fn directory_path_marker(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("directory-{index}.path.json"))
    }

    fn directory_metadata_marker(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("directory-{index}.metadata.json"))
    }

    fn directory_missing_marker(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("directory-{index}.missing"))
    }

    fn directory_applied_metadata_marker(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("directory-{index}.applied.metadata.json"))
    }

    fn directory_recovery_metadata_marker(&self, operation_id: [u8; 16], index: usize) -> PathBuf {
        self.backup_dir(operation_id)
            .join(format!("directory-{index}.recovery.metadata.json"))
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
    ) -> CatalogRegistrationMode {
        if self.project_path.is_some() {
            CatalogRegistrationMode::ProjectRegistration
        } else if component_set.host == crate::agents::host_bundle_v2::HostKindV1::ClaudeCode
            || component_set.host == crate::agents::host_bundle_v2::HostKindV1::Codex
            || component_set.host == crate::agents::host_bundle_v2::HostKindV1::Hermes
            || component_set.host == crate::agents::host_bundle_v2::HostKindV1::KimiCode
            || component_set.host == crate::agents::host_bundle_v2::HostKindV1::Kiro
            // Gemini's deployed artifacts are the extension *source*; the host
            // only carries the integration once `gemini extensions install`
            // adopts them, so the deployed bytes alone are not the lifecycle.
            || component_set.host == crate::agents::host_bundle_v2::HostKindV1::Gemini
            || (component_set.host == crate::agents::host_bundle_v2::HostKindV1::OpenCode
                && component_set.components.iter().any(|component| {
                    matches!(
                        component.manifest.component,
                        crate::agents::host_bundle_v2::HostBundleComponentV1::Core
                            | crate::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp
                    )
                }))
        {
            CatalogRegistrationMode::DeployedActivation
        } else {
            // Cursor and component sets without native activation
            // are fully represented by their catalog artifacts. Unsupported
            // hosts are refused by the catalog before this authority exists.
            CatalogRegistrationMode::ArtifactOnly
        }
    }

    /// Whether managed artifact bytes are the component set's complete host
    /// lifecycle. Artifact backup/restore must refuse every other mode because
    /// it intentionally does not snapshot or reconcile native registration.
    pub fn supports_artifact_only_backup_restore(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> bool {
        self.registration_mode(component_set) == CatalogRegistrationMode::ArtifactOnly
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
        self.validate_catalog_host(component_set)?;
        match self.registration_mode(component_set) {
            CatalogRegistrationMode::ArtifactOnly
                if !self.requires_competing_analyzer_preflight(component_set) =>
            {
                Ok(Sha256::digest(b"tracedecay.host-registration.none.v1").into())
            }
            CatalogRegistrationMode::ArtifactOnly
            | CatalogRegistrationMode::DeployedActivation
            | CatalogRegistrationMode::ProjectRegistration => {
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
            Err(_) => return Err(host_bundle_storage_failure!()),
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
            return self.integration.host_component_registration_for_lifecycle(
                component,
                &self.health_context,
                &self.context,
            );
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
    ) -> Result<Vec<PathBuf>, crate::agents::host_bundle_v2::HostBundleError> {
        let components = component_set
            .components
            .iter()
            .map(|component| component.manifest.component)
            .collect::<Vec<_>>();
        if let Some(project_path) = self.project_path.as_deref() {
            let mut paths = self
                .integration
                .project_host_component_registration_paths(
                    &components,
                    &self.context.home,
                    project_path,
                )
                .map_err(Self::registration_error)?;
            paths.sort();
            paths.dedup();
            return Ok(paths);
        }
        let mut paths = self
            .integration
            .host_component_registration_paths_checked(&components, &self.context.home)
            .map_err(Self::registration_error)?;
        if self.integration.id() == "claude" {
            let artifact_owned_manifest = self
                .context
                .home
                .join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/marketplace.json");
            paths.retain(|path| path != &artifact_owned_manifest);
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    fn allowed_registration_directories(&self) -> Vec<PathBuf> {
        match (self.integration.id(), self.project_path.as_deref()) {
            ("claude", Some(project_path)) => vec![project_path.join(".claude")],
            _ => Vec::new(),
        }
    }

    fn registration_directory_in_recovery_scope(&self, path: &Path) -> bool {
        self.allowed_registration_directories()
            .iter()
            .any(|allowed| allowed == path)
    }

    fn registration_directories(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Result<Vec<PathBuf>, crate::agents::host_bundle_v2::HostBundleError> {
        let allowed = self.allowed_registration_directories();
        if allowed.is_empty() {
            return Ok(allowed);
        }
        let registration_paths = self.registration_paths(component_set)?;
        Ok(allowed
            .into_iter()
            .filter(|directory| {
                registration_paths
                    .iter()
                    .any(|path| path.parent() == Some(directory.as_path()))
            })
            .collect())
    }

    fn current_registration_revision(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Result<[u8; 32], crate::agents::host_bundle_v2::HostBundleError> {
        self.registration_revision_excluding(component_set, &BTreeSet::new())
    }

    /// Revision of the registration surface with the transaction's own
    /// declared writes held constant.
    ///
    /// Some hosts register themselves *through* a file this component set also
    /// installs as a managed artifact — Kiro's `~/.kiro/settings/mcp.json` is
    /// simultaneously the registration path and the `context_mcp` artifact. A
    /// revision taken over the raw bytes of such a path necessarily changes the
    /// moment the transaction performs its own declared write, so a post-write
    /// recheck against the pre-write value can only ever fail.
    ///
    /// Excluded paths keep their position and name in the digest and
    /// contribute a fixed marker instead of their observed content, so the
    /// digest stays unambiguous: excluding a path is not the same as the path
    /// being absent, and the set of registration paths is still covered.
    fn registration_revision_excluding(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        excluded: &BTreeSet<PathBuf>,
    ) -> Result<[u8; 32], crate::agents::host_bundle_v2::HostBundleError> {
        if self.integration.id() == "claude" && self.project_path.is_none() {
            let claude_root = self.context.home.join(".claude");
            if fs::symlink_metadata(&claude_root)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(
                    crate::agents::host_bundle_v2::HostBundleError::UnsafeClaudeHomeSymlink,
                );
            }
        }
        let mut digest = Sha256::new();
        digest.update(b"tracedecay.host-registration.revision.v2");
        digest.update((self.integration.id().len() as u64).to_be_bytes());
        digest.update(self.integration.id().as_bytes());
        let registration_paths = self.registration_paths(component_set)?;
        if !registration_paths.is_empty() {
            for (index, path) in registration_paths.iter().enumerate() {
                digest.update((index as u64).to_be_bytes());
                digest.update((path.as_os_str().len() as u64).to_be_bytes());
                digest.update(path.as_os_str().as_encoded_bytes());
                if excluded.contains(path) {
                    digest.update(b"transaction-declared-write");
                    continue;
                }
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                        return Err(
                            crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath,
                        );
                    }
                    Ok(_) => {
                        let bytes = fs::read(path).map_err(|_| host_bundle_storage_failure!())?;
                        digest.update(b"file");
                        digest.update((bytes.len() as u64).to_be_bytes());
                        digest.update(bytes);
                        let metadata = crate::agents::capture_host_file_metadata(path)
                            .map_err(|_| host_bundle_storage_failure!())?;
                        let metadata = serde_json::to_vec(&metadata)
                            .map_err(|_| host_bundle_storage_failure!())?;
                        digest.update((metadata.len() as u64).to_be_bytes());
                        digest.update(metadata);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        digest.update(b"missing");
                    }
                    Err(_) => {
                        return Err(host_bundle_storage_failure!());
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
        fs::create_dir_all(&backup_dir).map_err(|_| host_bundle_storage_failure!())?;
        tracedecay_application::sync_parent_directory(
            &backup_dir,
            tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
        )
        .map_err(|_| host_bundle_storage_failure!())?;
        let identity = RegistrationBackupIdentityV1::new(
            self.integration.id(),
            &self.context.home,
            &self.lifecycle_root,
            self.rollback_project_path(),
        )?;
        let identity_bytes =
            serde_json::to_vec(&identity).map_err(|_| host_bundle_storage_failure!())?;
        write_registration_backup(&self.identity_path(operation_id), &identity_bytes)?;
        let registration_paths = self.registration_paths(component_set)?;
        let mut registration_directories = Vec::new();
        for path in self
            .registration_directories(component_set)?
            .into_iter()
            .filter(|path| self.registration_directory_in_recovery_scope(path))
        {
            match fs::symlink_metadata(&path) {
                Ok(metadata)
                    if metadata.file_type().is_symlink()
                        && self.project_path.is_none()
                        && path == self.context.home.join(".claude") =>
                {
                    return Err(
                        crate::agents::host_bundle_v2::HostBundleError::UnsafeClaudeHomeSymlink,
                    );
                }
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath);
                }
                Ok(_) => registration_directories.push(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    registration_directories.push(path);
                }
                Err(_) => {
                    return Err(host_bundle_storage_failure!());
                }
            }
        }
        let mutation_plan = RegistrationMutationPlanV1 {
            schema_version: REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION,
            integration_id: self.integration.id().to_string(),
            operation: self.operation,
            paths: registration_paths.clone(),
            directories: registration_directories.clone(),
        };
        let mutation_plan =
            serde_json::to_vec(&mutation_plan).map_err(|_| host_bundle_storage_failure!())?;
        write_registration_backup(&self.mutation_plan_path(operation_id), &mutation_plan)?;
        for (index, path) in registration_directories.iter().enumerate() {
            let path_bytes =
                serde_json::to_vec(path).map_err(|_| host_bundle_storage_failure!())?;
            write_registration_backup(
                &self.directory_path_marker(operation_id, index),
                &path_bytes,
            )?;
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath);
                }
                Ok(_) => {
                    let metadata = crate::agents::capture_host_file_metadata(path)
                        .map_err(|_| host_bundle_storage_failure!())?;
                    let metadata = serde_json::to_vec(&metadata)
                        .map_err(|_| host_bundle_storage_failure!())?;
                    write_registration_backup(
                        &self.directory_metadata_marker(operation_id, index),
                        &metadata,
                    )?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    write_registration_backup(
                        &self.directory_missing_marker(operation_id, index),
                        b"missing",
                    )?;
                }
                Err(_) => {
                    return Err(host_bundle_storage_failure!());
                }
            }
        }
        for (index, path) in registration_paths.iter().enumerate() {
            let path_bytes =
                serde_json::to_vec(path).map_err(|_| host_bundle_storage_failure!())?;
            write_registration_backup(
                &self.registration_path_marker(operation_id, index),
                &path_bytes,
            )?;
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath);
                }
                Ok(_) => {
                    let bytes = fs::read(path).map_err(|_| host_bundle_storage_failure!())?;
                    write_registration_backup(&self.backup_path(operation_id, index), &bytes)?;
                    let permissions = crate::agents::capture_host_file_metadata(path)
                        .map_err(|_| host_bundle_storage_failure!())?;
                    let permissions = serde_json::to_vec(&permissions)
                        .map_err(|_| host_bundle_storage_failure!())?;
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
                    return Err(host_bundle_storage_failure!());
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
        for (index, path) in self.registration_paths(component_set)?.iter().enumerate() {
            let observed = registration_observed_state(path)?;
            let bytes =
                serde_json::to_vec(&observed).map_err(|_| host_bundle_storage_failure!())?;
            write_registration_backup(&self.applied_state_marker(operation_id, index), &bytes)?;
        }
        Ok(())
    }

    fn prepare_missing_registration_directories(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        let mutation_plan = fs::read(self.mutation_plan_path(operation_id))
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
        let mutation_plan: RegistrationMutationPlanV1 = serde_json::from_slice(&mutation_plan)
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
        for (index, path) in mutation_plan.directories.iter().enumerate() {
            if !self.directory_missing_marker(operation_id, index).is_file() {
                continue;
            }
            match fs::symlink_metadata(path) {
                // Project registration directories are disjoint from catalog
                // artifact paths. If an absent directory appears before this
                // authority creates it, that state is foreign drift.
                Ok(_) => return Err(host_bundle_stale_preview!()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(host_bundle_storage_failure!());
                }
            }
            let parent = path.parent().ok_or(
                crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable,
            )?;
            let staging_path = parent.join(format!(
                ".tracedecay-registration-apply-{}-{index}",
                hex::encode(operation_id)
            ));
            match fs::symlink_metadata(&staging_path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(
                        crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable,
                    );
                }
                Ok(_) => {
                    if fs::read_dir(&staging_path)
                        .map_err(|_| host_bundle_storage_failure!())?
                        .next()
                        .is_some()
                    {
                        return Err(
                            crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable,
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&staging_path).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable
                    })?;
                }
                Err(_) => {
                    return Err(
                        crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable,
                    );
                }
            }
            let applied = registration_directory_applied_state(&staging_path)?;
            write_registration_backup(
                &self.directory_applied_metadata_marker(operation_id, index),
                &serde_json::to_vec(&applied).map_err(|_| host_bundle_storage_failure!())?,
            )?;
            sync_registration_metadata(&staging_path)?;
            fs::rename(&staging_path, path).map_err(|_| {
                crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable
            })?;
            tracedecay_application::sync_parent_directory(
                path,
                tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
            )
            .map_err(|_| host_bundle_storage_failure!())?;
        }
        Ok(())
    }

    fn validate_applied_registration(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        for (index, path) in self.registration_paths(component_set)?.iter().enumerate() {
            let expected = fs::read(self.applied_state_marker(operation_id, index))
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
            let expected: RegistrationObservedStateV1 = serde_json::from_slice(&expected)
                .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
            if registration_observed_state(path)? != expected {
                return Err(host_bundle_stale_preview!());
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
            let bytes = fs::read(backup).map_err(|_| host_bundle_storage_failure!())?;
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
            .map_err(|_| host_bundle_storage_failure!())?;
        serde_json::from_slice(&bytes).map_err(|_| host_bundle_storage_failure!())
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
            .map_err(|_| host_bundle_storage_failure!())?,
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
            self.rollback_project_path(),
        )?;
        let mutation_plan = fs::read(self.mutation_plan_path(operation_id))
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
        let mutation_plan: RegistrationMutationPlanV1 = serde_json::from_slice(&mutation_plan)
            .map_err(|_| crate::agents::host_bundle_v2::HostBundleError::WrongTarget)?;
        if mutation_plan.schema_version != REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION {
            return Err(crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat);
        }
        if mutation_plan.integration_id != self.integration.id()
            || mutation_plan.operation != self.operation
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::WrongTarget);
        }
        let mut persisted_paths = Vec::new();
        for index in 0.. {
            match fs::read(self.registration_path_marker(operation_id, index)) {
                Ok(bytes) => {
                    let path = serde_json::from_slice::<PathBuf>(&bytes)
                        .map_err(|_| host_bundle_storage_failure!())?;
                    persisted_paths.push(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => {
                    return Err(host_bundle_storage_failure!());
                }
            }
        }
        let current_registration_paths = self.registration_paths(component_set)?;
        // The inventory recorded at backup time is what the backup markers are
        // indexed by, so it -- not a fresh recomputation -- is the set to
        // restore. Some inventories are derived from state the operation
        // itself rewrites: Codex's `.tracedecay-managed-agents.json` is both a
        // registration path and the record naming the previous bundle's
        // exports, so a Core apply that retires a stale export shrinks the
        // recomputed set. Require the recorded inventory to cover everything
        // the registration surface is today, which rules out restoring a
        // backup taken for a different target.
        if current_registration_paths
            .iter()
            .any(|path| !persisted_paths.contains(path))
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::WrongTarget);
        }
        let registration_paths = persisted_paths.clone();
        let allowed_registration_directories = self.allowed_registration_directories();
        let registration_directories = mutation_plan.directories.clone();
        let mut persisted_directories = Vec::new();
        for index in 0.. {
            match fs::read(self.directory_path_marker(operation_id, index)) {
                Ok(bytes) => {
                    persisted_directories.push(
                        serde_json::from_slice::<PathBuf>(&bytes)
                            .map_err(|_| host_bundle_storage_failure!())?,
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => {
                    return Err(host_bundle_storage_failure!());
                }
            }
        }
        if mutation_plan.paths != registration_paths
            || persisted_paths != registration_paths
            || persisted_directories != registration_directories
            || registration_directories
                .iter()
                .any(|path| !allowed_registration_directories.contains(path))
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::WrongTarget);
        }
        let mut vanished_directories = Vec::new();
        let mut recovery_owned_directories = Vec::new();
        for (index, path) in registration_directories.iter().enumerate() {
            if !self.registration_directory_in_recovery_scope(path) {
                continue;
            }
            let metadata_marker = self.directory_metadata_marker(operation_id, index);
            let missing_marker = self.directory_missing_marker(operation_id, index);
            if metadata_marker.is_file() == missing_marker.is_file() {
                return Err(
                    crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat,
                );
            }
            if metadata_marker.is_file() {
                // Parse every metadata record before restoring any file. The
                // original identity is also the only permitted identity for a
                // pre-existing directory: registration never changes its
                // permissions or ACLs, so any other metadata is foreign drift.
                let original_metadata: crate::agents::HostFileMetadataIdentityV1 =
                    serde_json::from_slice(
                        &fs::read(metadata_marker).map_err(|_| host_bundle_storage_failure!())?,
                    )
                    .map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat
                    })?;
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                        return Err(
                            crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath,
                        );
                    }
                    Ok(_) => {
                        let observed = crate::agents::capture_host_file_metadata(path)
                            .map_err(|_| host_bundle_storage_failure!())?;
                        if observed != original_metadata {
                            return Err(host_bundle_stale_preview!());
                        }
                        let recovery_marker =
                            self.directory_recovery_metadata_marker(operation_id, index);
                        if recovery_marker.is_file() {
                            let recovery_metadata: crate::agents::HostFileMetadataIdentityV1 =
                                serde_json::from_slice(&fs::read(recovery_marker).map_err(|_| {
                                    host_bundle_storage_failure!()
                                })?)
                                .map_err(|_| {
                                    crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat
                                })?;
                            if recovery_metadata != original_metadata {
                                return Err(host_bundle_stale_preview!());
                            }
                            recovery_owned_directories.push(path);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        vanished_directories.push((index, path));
                        recovery_owned_directories.push(path);
                    }
                    Err(_) => {
                        return Err(host_bundle_storage_failure!());
                    }
                }
            } else {
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                        return Err(
                            crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath,
                        );
                    }
                    Ok(_) => {
                        let applied_marker =
                            self.directory_applied_metadata_marker(operation_id, index);
                        if !applied_marker.is_file() {
                            return Err(host_bundle_stale_preview!());
                        }
                        let applied: RegistrationDirectoryAppliedStateV2 =
                            serde_json::from_slice(&fs::read(applied_marker).map_err(|_| {
                                host_bundle_storage_failure!()
                            })?)
                            .map_err(|_| {
                                crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat
                            })?;
                        if registration_directory_applied_state(path)? != applied {
                            return Err(host_bundle_stale_preview!());
                        }
                        recovery_owned_directories.push(path);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => {
                        return Err(host_bundle_storage_failure!());
                    }
                }
            }
        }
        for (index, path) in registration_paths.iter().enumerate() {
            let observed = registration_observed_state(path)?;
            let original = self.original_registration_state(operation_id, index)?;
            let intended = self.intended_registration_state(operation_id, path)?;
            let parent_will_be_recreated = !observed.present
                && recovery_owned_directories
                    .iter()
                    .any(|directory| path.starts_with(directory));
            if observed != original
                && intended.as_ref() != Some(&observed)
                && !parent_will_be_recreated
            {
                return Err(host_bundle_stale_preview!());
            }
        }
        vanished_directories.sort_by_key(|(_, path)| path.components().count());
        for (index, path) in vanished_directories {
            let metadata: crate::agents::HostFileMetadataIdentityV1 = serde_json::from_slice(
                &fs::read(self.directory_metadata_marker(operation_id, index))
                    .map_err(|_| host_bundle_storage_failure!())?,
            )
            .map_err(|_| {
                crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat
            })?;
            let parent = path.parent().ok_or(
                crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable,
            )?;
            let staging_path = parent.join(format!(
                ".tracedecay-registration-recovery-{}-{index}",
                hex::encode(operation_id)
            ));
            match fs::symlink_metadata(&staging_path) {
                Ok(staging_metadata)
                    if staging_metadata.file_type().is_symlink() || !staging_metadata.is_dir() =>
                {
                    return Err(
                        crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable,
                    );
                }
                Ok(_) => {
                    if fs::read_dir(&staging_path)
                        .map_err(|_| host_bundle_storage_failure!())?
                        .next()
                        .is_some()
                    {
                        return Err(
                            crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable,
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&staging_path).map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable
                    })?;
                }
                Err(_) => {
                    return Err(
                        crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable,
                    );
                }
            }
            crate::agents::restore_host_file_metadata(&staging_path, &metadata).map_err(|_| {
                crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable
            })?;
            sync_registration_metadata(&staging_path)?;
            write_registration_backup(
                &self.directory_recovery_metadata_marker(operation_id, index),
                &serde_json::to_vec(&metadata).map_err(|_| host_bundle_storage_failure!())?,
            )?;
            fs::rename(&staging_path, path).map_err(|_| {
                crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable
            })?;
            tracedecay_application::sync_parent_directory(
                path,
                tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
            )
            .map_err(|_| host_bundle_storage_failure!())?;
        }
        for (index, path) in registration_paths.iter().enumerate() {
            let observed = registration_observed_state(path)?;
            let original = self.original_registration_state(operation_id, index)?;
            let backup = self.backup_path(operation_id, index);
            let missing = self.missing_marker_path(operation_id, index);
            if backup.is_file() {
                let permissions =
                    fs::read(self.registration_permission_marker(operation_id, index))
                        .map_err(|_| host_bundle_storage_failure!())?;
                let permissions: crate::agents::HostFileMetadataIdentityV1 =
                    serde_json::from_slice(&permissions)
                        .map_err(|_| host_bundle_storage_failure!())?;
                if observed != original {
                    let bytes = fs::read(&backup).map_err(|_| host_bundle_storage_failure!())?;
                    crate::agents::safe_write_bytes_file_with_metadata(
                        path,
                        &bytes,
                        None,
                        Some(&permissions),
                    )
                    .map_err(|_| host_bundle_storage_failure!())?;
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
                crate::agents::restore_host_file_metadata(path, &permissions)
                    .map_err(|_| host_bundle_storage_failure!())?;
                sync_registration_metadata(path)?;
            } else if missing.is_file() {
                if observed == original {
                    continue;
                }
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_file() => {
                        fs::remove_file(path).map_err(|_| host_bundle_storage_failure!())?
                    }
                    Ok(_) => {
                        return Err(
                            crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath,
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => {
                        return Err(host_bundle_storage_failure!());
                    }
                }
            }
        }
        let mut directory_restore_order = registration_directories
            .iter()
            .enumerate()
            .filter(|(_, path)| self.registration_directory_in_recovery_scope(path))
            .collect::<Vec<_>>();
        directory_restore_order
            .sort_by_key(|(_, path)| std::cmp::Reverse(path.components().count()));
        for (index, path) in directory_restore_order {
            let metadata_marker = self.directory_metadata_marker(operation_id, index);
            if metadata_marker.is_file() {
                let metadata: crate::agents::HostFileMetadataIdentityV1 = serde_json::from_slice(
                    &fs::read(metadata_marker).map_err(|_| host_bundle_storage_failure!())?,
                )
                .map_err(|_| {
                    crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat
                })?;
                if crate::agents::capture_host_file_metadata(path)
                    .map_err(|_| host_bundle_storage_failure!())?
                    != metadata
                {
                    return Err(host_bundle_stale_preview!());
                }
                crate::agents::restore_host_file_metadata(path, &metadata)
                    .map_err(|_| host_bundle_storage_failure!())?;
            } else {
                let applied_marker = self.directory_applied_metadata_marker(operation_id, index);
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                        return Err(
                            crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath,
                        );
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let parent = path.parent().ok_or(
                            crate::agents::host_bundle_v2::HostBundleError::RecoveryDirectoryUnavailable,
                        )?;
                        let staging_path = parent.join(format!(
                            ".tracedecay-registration-apply-{}-{index}",
                            hex::encode(operation_id)
                        ));
                        match fs::remove_dir(&staging_path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(_) => {
                                return Err(host_bundle_storage_failure!());
                            }
                        }
                        continue;
                    }
                    Err(_) => {
                        return Err(host_bundle_storage_failure!());
                    }
                }
                if !applied_marker.is_file() {
                    return Err(host_bundle_stale_preview!());
                }
                let applied: RegistrationDirectoryAppliedStateV2 = serde_json::from_slice(
                    &fs::read(applied_marker).map_err(|_| host_bundle_storage_failure!())?,
                )
                .map_err(|_| {
                    crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat
                })?;
                if registration_directory_applied_state(path)? != applied {
                    return Err(host_bundle_stale_preview!());
                }
                match fs::remove_dir(path) {
                    Ok(()) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                        ) => {}
                    Err(_) => {
                        return Err(host_bundle_storage_failure!());
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
                fs::remove_dir_all(backup_dir).map_err(|_| host_bundle_storage_failure!())
            }
            Ok(_) => Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(host_bundle_storage_failure!()),
        }
    }
}

fn current_tracedecay_binary() -> crate::errors::Result<String> {
    std::env::current_exe()
        .map_err(|error| crate::errors::TraceDecayError::Config {
            message: format!("failed to resolve the running tracedecay binary: {error}"),
        })?
        .into_os_string()
        .into_string()
        .map_err(|path| crate::errors::TraceDecayError::Config {
            message: format!(
                "the running tracedecay binary path is not valid UTF-8: {}",
                PathBuf::from(path).display()
            ),
        })
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
    // Each pointer must name the key that the catalog-selected project
    // projection writes. A mismatch makes
    // `verify` observe `Missing` immediately after a successful apply, which
    // rolls the whole transaction back and reports it as a storage failure.
    let current = match integration_id {
        "codex" => document.get("name").and_then(serde_json::Value::as_str) == Some("tracedecay"),
        // Kilo registers under `mcp` (not `mcpServers`), same as OpenCode.
        "opencode" | "kilo" => document.pointer("/mcp/tracedecay").is_some(),
        "kimi" | "kiro" | "roo-code" => document.pointer("/mcpServers/tracedecay").is_some(),
        _ => false,
    };
    if current {
        State::Current
    } else {
        State::Missing
    }
}

impl crate::agents::host_bundle_v2::HostComponentSetRegistrationV1
    for CatalogHostComponentRegistrationAuthority
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
            return Err(host_bundle_stale_preview!());
        }
        self.confirmed_registration_revision = Some(preview.base_registration_revision);
        Ok(())
    }

    fn declare_artifact_writes(
        &mut self,
        _component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        _request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
        paths: &[PathBuf],
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        self.declared_artifact_writes = paths.iter().cloned().collect();
        Ok(())
    }

    fn preflight(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        _request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        self.validate_catalog_host(component_set)?;
        self.refuse_ambiguous_opencode_analyzer(component_set)?;
        if self.registration_mode(component_set) == CatalogRegistrationMode::ArtifactOnly {
            self.should_apply = false;
            return Ok(());
        }
        if self.project_path.is_some()
            && component_set.host == crate::agents::host_bundle_v2::HostKindV1::CursorDesktop
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::UnsupportedCapability);
        }
        if let Some(expected) = self.confirmed_registration_revision
            && self.current_registration_revision(component_set)? != expected
        {
            return Err(host_bundle_stale_preview!());
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
        let claude_global_install = self.project_path.is_none()
            && component_set.host == crate::agents::host_bundle_v2::HostKindV1::ClaudeCode
            && self.operation == crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install;
        self.should_apply = match self.operation {
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install => {
                if !all_current && !all_missing && !claude_global_install {
                    return Err(crate::agents::host_bundle_v2::HostBundleError::OwnershipConflict);
                }
                all_missing || claude_global_install
            }
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => !all_missing,
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
            | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => true,
        };
        if self.project_path.is_none()
            && self.integration.interactive_activation_guidance().is_some()
        {
            let native_state_already_matches = match self.operation {
                crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => all_missing,
                crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install
                | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
                | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => all_current,
            };
            if native_state_already_matches {
                self.should_apply = false;
            } else if self.operation
                == crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall
            {
                // Removal is the host's to perform first: stripping a bundle the
                // host still has registered would leave it resolving a
                // marketplace that no longer exists. This arm must precede the
                // update/activation arms below — both of those remediate as
                // "refresh or activate the plugin", which can never unblock an
                // uninstall, so routing removal through them makes the host's
                // integration impossible to remove. Once the operator has run
                // the host's own removal the registration reads `Missing`,
                // `native_state_already_matches` holds, and the transaction
                // proceeds to delete the receipt-owned artifacts.
                return Err(crate::agents::host_bundle_v2::HostBundleError::NativeRemovalRequired);
            } else if matches!(
                self.operation,
                crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
                    | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair
            ) || states.iter().any(|state| {
                *state == crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Repairable
            }) {
                return Err(crate::agents::host_bundle_v2::HostBundleError::NativeUpdateRequired);
            } else {
                // Native-only activation must complete in the host before the
                // transaction claims any staged artifact.
                return Err(crate::agents::host_bundle_v2::HostBundleError::UnsupportedCapability);
            }
        }
        Ok(())
    }

    fn stage(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        self.validate_catalog_host(component_set)?;
        self.refuse_ambiguous_opencode_analyzer(component_set)?;
        if self.registration_mode(component_set) == CatalogRegistrationMode::ArtifactOnly {
            if let Some(expected) = self.confirmed_registration_revision
                && self.component_registration_revision(component_set)? != expected
            {
                return Err(host_bundle_stale_preview!());
            }
            return Ok(());
        }
        if let Some(expected) = self.confirmed_registration_revision
            && self.current_registration_revision(component_set)? != expected
        {
            return Err(host_bundle_stale_preview!());
        }
        // Last observation before the transaction writes its own artifacts.
        // Everything outside the declared write set must still look like this
        // when `apply` runs; anything else is a foreign mutation.
        self.staged_foreign_registration_revision =
            match self.confirmed_registration_revision {
                Some(_) => Some(self.registration_revision_excluding(
                    component_set,
                    &self.declared_artifact_writes,
                )?),
                None => None,
            };
        self.backup_registration(component_set, request.operation_id)?;
        Ok(())
    }

    fn apply(
        &mut self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
        request: &crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle_v2::HostBundleError> {
        self.validate_catalog_host(component_set)?;
        let mode = self.registration_mode(component_set);
        if mode == CatalogRegistrationMode::ArtifactOnly {
            return Ok(());
        }
        // `apply` runs *after* the transaction wrote its declared artifacts,
        // so the drift check here is scoped to the registration paths this
        // transaction did not claim. Comparing the full revision against the
        // confirmed base at this point would make every host whose
        // registration file is also a managed artifact (Kiro) invalidate its
        // own write and roll back on every run. Foreign edits to a *declared*
        // path are not lost: `verify_component_set_artifacts` re-digests
        // exactly those files immediately after this step.
        match self.staged_foreign_registration_revision {
            Some(expected) => {
                if self.registration_revision_excluding(
                    component_set,
                    &self.declared_artifact_writes,
                )? != expected
                {
                    return Err(host_bundle_stale_preview!());
                }
            }
            // No staged observation (a caller driving the adapter directly
            // rather than through the transaction): fall back to the confirmed
            // base, which is the pre-write value.
            None => {
                if let Some(expected) = self.confirmed_registration_revision
                    && self.current_registration_revision(component_set)? != expected
                {
                    return Err(host_bundle_stale_preview!());
                }
            }
        }
        if !self.should_apply {
            return self.capture_applied_registration(component_set, request.operation_id);
        }
        write_registration_backup(
            &self.registration_effect_path(request.operation_id),
            b"started",
        )?;
        if request.lifecycle.operation
            != crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall
        {
            self.prepare_missing_registration_directories(request.operation_id)?;
        }
        let result = crate::agents::with_host_config_write_intents(
            self.write_intent_root(request.operation_id),
            || match mode {
                CatalogRegistrationMode::ArtifactOnly => Ok(()),
                CatalogRegistrationMode::DeployedActivation => {
                    let components = component_set
                        .components
                        .iter()
                        .map(|component| component.manifest.component)
                        .collect::<Vec<_>>();
                    match request.lifecycle.operation {
                        crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => self
                            .integration
                            .deactivate_deployed_host_component_registration(
                                &components,
                                &self.context,
                            )
                            .map_err(Self::registration_error),
                        crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install
                        | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
                        | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => self
                            .integration
                            .activate_deployed_host_component_registration(
                                &components,
                                &self.context,
                            )
                            .map_err(Self::registration_error),
                    }
                }
                CatalogRegistrationMode::ProjectRegistration => {
                    let project_path = self.project_path.as_deref().ok_or(
                        crate::agents::host_bundle_v2::HostBundleError::UnsupportedCapability,
                    )?;
                    let components = component_set
                        .components
                        .iter()
                        .map(|component| component.manifest.component)
                        .collect::<Vec<_>>();
                    match request.lifecycle.operation {
                        crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall => self
                            .integration
                            .deactivate_project_host_component_registration(
                                &components,
                                &self.context,
                                project_path,
                            )
                            .map_err(Self::registration_error),
                        crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install
                        | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
                        | crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair => self
                            .integration
                            .activate_project_host_component_registration(
                                &components,
                                &self.context,
                                project_path,
                            )
                            .map_err(Self::registration_error),
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
        self.validate_catalog_host(component_set)?;
        #[cfg(feature = "test-transport")]
        if std::env::var_os("TRACEDECAY_TEST_FAIL_HOST_REGISTRATION_VERIFY").is_some() {
            return Err(host_bundle_storage_failure!());
        }
        if self.registration_mode(component_set) == CatalogRegistrationMode::ArtifactOnly {
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
            Err(host_bundle_storage_failure!())
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
        self.validate_catalog_host(component_set)?;
        if self.registration_mode(component_set) == CatalogRegistrationMode::ArtifactOnly {
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
    fs::canonicalize(path).map_err(|_| host_bundle_storage_failure!())
}

fn registration_observed_state(
    path: &Path,
) -> Result<RegistrationObservedStateV1, crate::agents::host_bundle_v2::HostBundleError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath)
        }
        Ok(_) => {
            let bytes = fs::read(path).map_err(|_| host_bundle_storage_failure!())?;
            Ok(RegistrationObservedStateV1 {
                present: true,
                digest: Sha256::digest(bytes).into(),
                metadata: Some(
                    crate::agents::capture_host_file_metadata(path)
                        .map_err(|_| host_bundle_storage_failure!())?,
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
        Err(_) => Err(host_bundle_storage_failure!()),
    }
}

fn registration_directory_applied_state(
    path: &Path,
) -> Result<RegistrationDirectoryAppliedStateV2, crate::agents::host_bundle_v2::HostBundleError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| host_bundle_storage_failure!())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(crate::agents::host_bundle_v2::HostBundleError::UnsafeInstallPath);
    }
    #[cfg(unix)]
    let unix_identity = Some((metadata.dev(), metadata.ino()));
    #[cfg(not(unix))]
    let unix_identity = None;
    Ok(RegistrationDirectoryAppliedStateV2 {
        metadata: crate::agents::capture_host_file_metadata(path)
            .map_err(|_| host_bundle_storage_failure!())?,
        unix_identity,
    })
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
        .map_err(|_| host_bundle_storage_failure!())
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
    .map_err(|_| host_bundle_storage_failure!())
}

pub fn project_local_registration_path(
    agent_id: &str,
    _home: &Path,
    project_path: &Path,
) -> Option<PathBuf> {
    match agent_id {
        "claude" => Some(project_path.join(".claude/CLAUDE.md")),
        // The Codex project projection lives at the repository root, not
        // under `.codex/`.
        "codex" => Some(project_path.join("plugins/tracedecay/.codex-plugin/plugin.json")),
        // Cursor exposes no project-local registration surface. The shared
        // home plugin must never be inventoried as project-owned state.
        "cursor" => None,
        "kimi" => Some(project_path.join(".kimi-code/mcp.json")),
        "kiro" => Some(project_path.join(".kiro/settings/mcp.json")),
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

    fn empty_claude_component_set() -> crate::agents::host_bundle_v2::HostComponentSetV1 {
        crate::agents::host_bundle_v2::HostComponentSetV1 {
            host: crate::agents::host_bundle_v2::HostKindV1::ClaudeCode,
            components: Vec::new(),
        }
    }

    /// Gemini's deployed artifacts are an extension *source*: the host carries
    /// nothing until `gemini extensions install` adopts them. Classifying the
    /// set as artifact-only would let a lifecycle report an activation that
    /// never happened, and would let artifact backup/restore claim it can
    /// reverse a host registration it never snapshots.
    #[test]
    fn gemini_component_sets_are_not_artifact_only_lifecycles() {
        let home = tempfile::tempdir().expect("home");
        let lifecycle_root = tempfile::tempdir().expect("lifecycle root");
        let authority = CatalogHostComponentRegistrationAuthority::new(
            "gemini",
            home.path(),
            lifecycle_root.path(),
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install,
        )
        .expect("catalog registration authority");
        let component_set =
            crate::agents::host_bundle_registry::verified_embedded_default_host_component_set(
                crate::agents::host_bundle_v2::HostKindV1::Gemini,
                0,
            )
            .expect("Gemini has a compiled default set");

        assert!(
            !authority.supports_artifact_only_backup_restore(&component_set.component_set),
            "the Gemini lifecycle drives `gemini extensions install`, so its deployed \
             bytes are not the whole lifecycle"
        );
        // Control: Cursor's component set really is fully represented by its
        // managed artifacts, so the assertion above is about Gemini's
        // classification and not about a predicate that always answers false.
        let cursor = CatalogHostComponentRegistrationAuthority::new(
            "cursor",
            home.path(),
            lifecycle_root.path(),
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install,
        )
        .expect("catalog registration authority");
        let cursor_set =
            crate::agents::host_bundle_registry::verified_embedded_default_host_component_set(
                crate::agents::host_bundle_v2::HostKindV1::CursorDesktop,
                0,
            )
            .expect("Cursor has a compiled default set");
        assert!(cursor.supports_artifact_only_backup_restore(&cursor_set.component_set));
    }

    /// Build an authority plus an on-disk registration backup whose mutation plan
    /// declares `directories`, marking every one of them absent at backup time.
    fn missing_directory_fixture(
        home: &Path,
        lifecycle_root: &Path,
        directories: &[PathBuf],
    ) -> (CatalogHostComponentRegistrationAuthority, [u8; 16]) {
        let authority = CatalogHostComponentRegistrationAuthority::new_project_local(
            "claude",
            home,
            home,
            lifecycle_root,
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install,
        )
        .expect("catalog registration authority");
        let operation_id = [7u8; 16];
        fs::create_dir_all(authority.backup_dir(operation_id)).expect("backup dir");
        let registration_paths = authority
            .registration_paths(&empty_claude_component_set())
            .expect("registration paths");
        let plan = RegistrationMutationPlanV1 {
            schema_version: 2,
            integration_id: "claude".to_string(),
            operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install,
            paths: registration_paths.clone(),
            directories: directories.to_vec(),
        };
        write_registration_backup(
            &authority.mutation_plan_path(operation_id),
            &serde_json::to_vec(&plan).unwrap(),
        )
        .expect("plan");
        for (index, directory) in directories.iter().enumerate() {
            write_registration_backup(
                &authority.directory_missing_marker(operation_id, index),
                b"missing",
            )
            .expect("missing marker");
            write_registration_backup(
                &authority.directory_path_marker(operation_id, index),
                &serde_json::to_vec(directory).unwrap(),
            )
            .expect("directory path marker");
        }
        // Every registration path the rollback will consult needs an original
        // state on disk; this fixture is only about the directory inventory, so
        // record them all as absent.
        for (index, path) in registration_paths.iter().enumerate() {
            write_registration_backup(
                &authority.missing_marker_path(operation_id, index),
                b"missing",
            )
            .expect("registration missing marker");
            write_registration_backup(
                &authority.registration_path_marker(operation_id, index),
                &serde_json::to_vec(path).unwrap(),
            )
            .expect("registration path marker");
        }
        let identity = RegistrationBackupIdentityV1::new(
            "claude",
            home,
            lifecycle_root,
            authority.rollback_project_path(),
        )
        .expect("identity");
        write_registration_backup(
            &authority.identity_path(operation_id),
            &serde_json::to_vec(&identity).unwrap(),
        )
        .expect("identity write");
        (authority, operation_id)
    }

    /// A directory appearing after backup but before this authority creates it
    /// is foreign mutation and must abort the apply.
    #[test]
    fn undeclared_directory_reappearing_is_still_drift() {
        // Hold the process-global profile pin across the whole fixture and
        // prepare lifetime so concurrent tests cannot repoint discovery
        // mid-transaction.
        let _profile = crate::config::PinnedUserDataDir::new();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let foreign = home.path().join(".claude");

        let (authority, operation_id) = missing_directory_fixture(
            home.path(),
            lifecycle.path(),
            std::slice::from_ref(&foreign),
        );
        fs::create_dir_all(&foreign).unwrap();

        assert!(
            matches!(
                authority.prepare_missing_registration_directories(operation_id),
                Err(HostBundleError::StalePreview(_))
            ),
            "nothing in this transaction claims the path, so its reappearance is foreign"
        );
    }

    #[test]
    fn rollback_refuses_an_unattributed_directory() {
        // Hold the process-global profile pin for the whole fixture+restore
        // lifetime; without it a concurrent test can repoint profile
        // discovery between backup and rollback.
        let _profile = crate::config::PinnedUserDataDir::new();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let orphaned = home.path().join(".claude");

        let (authority, operation_id) = missing_directory_fixture(
            home.path(),
            lifecycle.path(),
            std::slice::from_ref(&orphaned),
        );
        // The state the dead operation left behind: the directory exists, but
        // no `directory-0.applied.metadata.json` ever got written.
        fs::create_dir_all(&orphaned).unwrap();
        assert!(
            !authority
                .directory_applied_metadata_marker(operation_id, 0)
                .is_file()
        );

        let component_set = empty_claude_component_set();

        assert!(
            matches!(
                authority.restore_registration(&component_set, operation_id),
                Err(HostBundleError::StalePreview(_))
            ),
            "recovery must not adopt a directory it never recorded creating"
        );
        assert!(orphaned.is_dir(), "foreign directory was removed");
    }

    /// Recovery refuses unattributed content and preserves it byte-for-byte.
    #[test]
    fn rollback_leaves_a_non_empty_unattributed_directory_in_place() {
        // Same profile-pin discipline as `rollback_refuses_an_unattributed_directory`.
        let _profile = crate::config::PinnedUserDataDir::new();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let orphaned = home.path().join(".claude");

        let (authority, operation_id) = missing_directory_fixture(
            home.path(),
            lifecycle.path(),
            std::slice::from_ref(&orphaned),
        );
        fs::create_dir_all(&orphaned).unwrap();
        fs::write(orphaned.join("someone-elses.md"), b"keep me").unwrap();

        let component_set = empty_claude_component_set();

        assert!(matches!(
            authority.restore_registration(&component_set, operation_id),
            Err(HostBundleError::StalePreview(_))
        ));

        assert_eq!(
            fs::read(orphaned.join("someone-elses.md")).unwrap(),
            b"keep me",
            "unaccountable content is preserved, never silently discarded"
        );
    }

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

    /// The pointer each host is classified by must match its catalog-selected
    /// project projection. When the two disagree, a successful apply is
    /// observed as `Missing` by `verify`, the transaction rolls back, and the
    /// operator sees "atomic filesystem operation failed" for a host that was
    /// registered correctly.
    #[test]
    fn project_registration_state_matches_catalog_projection() {
        use crate::agents::host_bundle_v2::HostBundleRegistrationStateV1 as State;

        let integrations: Vec<Box<dyn crate::agents::AgentIntegration>> = vec![
            Box::new(crate::agents::KimiIntegration),
            Box::new(crate::agents::KiroIntegration),
            Box::new(crate::agents::OpenCodeIntegration),
        ];
        for integration in integrations {
            let home = tempfile::tempdir().unwrap();
            let project = tempfile::tempdir().unwrap();
            let context = crate::agents::InstallContext {
                home: home.path().to_path_buf(),
                tracedecay_bin: "/usr/bin/tracedecay".to_string(),
                tool_permissions: Vec::new(),
                project_root: Some(project.path().to_path_buf()),
                dashboard: false,
            };
            integration
                .activate_project_host_component_registration(
                    &[crate::agents::host_bundle_v2::HostBundleComponentV1::Core],
                    &context,
                    project.path(),
                )
                .unwrap_or_else(|error| {
                    panic!("{} project projection failed: {error}", integration.id())
                });
            let path =
                project_local_registration_path(integration.id(), home.path(), project.path())
                    .unwrap_or_else(|| {
                        panic!(
                            "{} must expose a project-local registration path",
                            integration.id()
                        )
                    });
            let contents = fs::read(&path).unwrap_or_else(|error| {
                panic!(
                    "read {} registration {}: {error}",
                    integration.id(),
                    path.display()
                )
            });
            assert_eq!(
                project_registration_state(integration.id(), &contents),
                State::Current,
                "{} project-local registration must be observed as current after projection",
                integration.id()
            );
        }
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
        let mut future_identity = identity;
        future_identity.schema_version = REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION + 1;
        assert_eq!(
            future_identity.validate("codex", home.path(), profile.path(), Some(project.path())),
            Err(HostBundleError::UnsupportedRecoveryFormat)
        );
    }
}
