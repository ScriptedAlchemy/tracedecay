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
const MIN_SUPPORTED_REGISTRATION_BACKUP_SCHEMA_VERSION: u16 = 1;

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
    #[serde(default)]
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
    #[serde(default)]
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
        if !(MIN_SUPPORTED_REGISTRATION_BACKUP_SCHEMA_VERSION
            ..=REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION)
            .contains(&self.schema_version)
        {
            return Err(crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat);
        }
        let observed = Self::new(integration_id, home, profile, project)?;
        // Schema v1 did not bind global Claude recovery to the project path.
        // Its persisted mutation paths still bind recovery to the exact
        // project files, so retain compatibility without weakening v2.
        let project_matches = self.canonical_project == observed.canonical_project
            || (self.schema_version == 1
                && self.integration_id == "claude"
                && self.canonical_project.is_none());
        (self.integration_id == observed.integration_id
            && self.canonical_home == observed.canonical_home
            && self.canonical_profile == observed.canonical_profile
            && project_matches)
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
    fn rollback_project_path(&self) -> Option<&Path> {
        self.project_path.as_deref().or_else(|| {
            (self.integration.id() == "claude")
                .then_some(self.health_context.project_path.as_path())
        })
    }

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
    ) -> CompatibilityRegistrationMode {
        let includes_core = component_set.components.iter().any(|component| {
            component.manifest.component
                == crate::agents::host_bundle_v2::HostBundleComponentV1::Core
        });
        if self.project_path.is_some() {
            CompatibilityRegistrationMode::LegacyIntegration
        } else if kiro_registration_is_the_artifact(component_set) {
            // Kiro's `context_mcp` artifact *is* its MCP registration: the
            // component set installs `~/.kiro/settings/mcp.json`, which is the
            // exact file `install_mcp_server` would otherwise register into.
            // Running both writers over one file gives it two owners — the
            // native activation reserializes the document the artifact layer
            // just wrote byte-for-byte, so the receipt's own artifact
            // verification then fails with a content mismatch. The artifact
            // write is the complete lifecycle here, which is what
            // `supports_artifact_only_backup_restore` has always claimed for
            // this set.
            CompatibilityRegistrationMode::ArtifactOnly
        } else if component_set.host == crate::agents::host_bundle_v2::HostKindV1::ClaudeCode
            || component_set.host == crate::agents::host_bundle_v2::HostKindV1::Codex
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
        let mut paths = self.integration.host_component_registration_paths_at(
            &components,
            &self.context.home,
            &self.health_context.project_path,
        );
        if self.integration.id() == "claude" {
            let artifact_owned_manifest = self
                .context
                .home
                .join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/marketplace.json");
            paths.retain(|path| path != &artifact_owned_manifest);
        }
        paths.sort();
        paths.dedup();
        paths
    }

    fn allowed_registration_directories(&self) -> Vec<PathBuf> {
        if self.integration.id() != "claude" {
            return Vec::new();
        }
        let mut paths = vec![
            self.context.home.join(".claude"),
            self.context.home.join(".claude/agents"),
            self.health_context.project_path.join(".claude"),
        ];
        paths.sort();
        paths.dedup();
        paths
    }

    fn registration_directory_in_recovery_scope(&self, path: &Path) -> bool {
        // New backups and recovery use the same ownership boundary. The
        // recovery-side check additionally protects authentic v1 project
        // backups that over-inventoried user-global Claude directories.
        self.project_path.is_none() || !path.starts_with(self.context.home.join(".claude"))
    }

    fn registration_directories(
        &self,
        component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
    ) -> Vec<PathBuf> {
        let allowed = self.allowed_registration_directories();
        if allowed.is_empty() {
            return allowed;
        }
        let registration_paths = self.registration_paths(component_set);
        let claude_root = self.context.home.join(".claude");
        allowed
            .into_iter()
            .filter(|directory| {
                (self.project_path.is_none() && directory == &claude_root)
                    || registration_paths
                        .iter()
                        .any(|path| path.parent() == Some(directory.as_path()))
            })
            .collect()
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
        let registration_paths = self.registration_paths(component_set);
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
        let registration_paths = self.registration_paths(component_set);
        let mut registration_directories = Vec::new();
        for path in self
            .registration_directories(component_set)
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
        for (index, path) in self.registration_paths(component_set).iter().enumerate() {
            let observed = registration_observed_state(path)?;
            let bytes =
                serde_json::to_vec(&observed).map_err(|_| host_bundle_storage_failure!())?;
            write_registration_backup(&self.applied_state_marker(operation_id, index), &bytes)?;
        }
        Ok(())
    }

    /// Whether `directory` can only have come into existence because this
    /// transaction wrote a declared artifact underneath it.
    ///
    /// The caller has already established that the backup recorded `directory`
    /// as absent, so a strict ancestor relationship to a declared write is
    /// proof of authorship: nothing else in the operation touches that path.
    fn declared_write_created_directory(&self, directory: &Path) -> bool {
        self.declared_artifact_writes
            .iter()
            .any(|write| write != directory && write.starts_with(directory))
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
                // `apply` runs *after* the transaction wrote its declared
                // artifacts, and writing `<dir>/artifact` creates `<dir>`. A
                // registration directory that the backup recorded as absent is
                // therefore expected to exist by now whenever it is an ancestor
                // of one of this transaction's own declared writes. Treating
                // that as foreign drift made the very first install into a
                // fresh home fail, so adopt the directory instead: record the
                // applied state the rollback path needs, then move on. Any
                // other pre-existing entry is still genuine drift.
                Ok(metadata)
                    if !metadata.file_type().is_symlink()
                        && metadata.is_dir()
                        && self.declared_write_created_directory(path) =>
                {
                    let applied = registration_directory_applied_state(path)?;
                    write_registration_backup(
                        &self.directory_applied_metadata_marker(operation_id, index),
                        &serde_json::to_vec(&applied)
                            .map_err(|_| host_bundle_storage_failure!())?,
                    )?;
                    continue;
                }
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
        for (index, path) in self.registration_paths(component_set).iter().enumerate() {
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
        if !(MIN_SUPPORTED_REGISTRATION_BACKUP_SCHEMA_VERSION
            ..=REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION)
            .contains(&mutation_plan.schema_version)
        {
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
        let current_registration_paths = self.registration_paths(component_set);
        let registration_paths = if mutation_plan.schema_version == 1 {
            if mutation_plan
                .paths
                .iter()
                .any(|path| !current_registration_paths.contains(path))
            {
                return Err(crate::agents::host_bundle_v2::HostBundleError::WrongTarget);
            }
            mutation_plan.paths.clone()
        } else {
            current_registration_paths
        };
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
                            // No applied-state marker means the directory came
                            // back before `prepare_missing_registration_directories`
                            // could claim it -- typically because the transaction's
                            // own artifact write created it and then a later step
                            // failed. Refusing here left the journal on disk with
                            // no convergence path, so every later lifecycle command
                            // reported stale preview forever. Claim it instead and
                            // assert nothing about its exact state: the removal pass
                            // below is empty-guarded, so a directory that anyone else
                            // is using survives untouched.
                            recovery_owned_directories.push(path);
                            continue;
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
                // Legacy schema-v1 backups never recorded applied state, and a
                // schema-v2 operation that died between its artifact write and
                // `prepare_missing_registration_directories` did not get to
                // record it either. Both cases still have to roll back, so
                // assert nothing about the directory's exact state and let the
                // empty-guarded removal below decide.
                let unattributed_missing_directory = !applied_marker.is_file();
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
                if !unattributed_missing_directory {
                    let applied: RegistrationDirectoryAppliedStateV2 = serde_json::from_slice(
                        &fs::read(applied_marker).map_err(|_| host_bundle_storage_failure!())?,
                    )
                    .map_err(|_| {
                        crate::agents::host_bundle_v2::HostBundleError::UnsupportedRecoveryFormat
                    })?;
                    if registration_directory_applied_state(path)? != applied {
                        return Err(host_bundle_stale_preview!());
                    }
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
    // Each pointer must name the key that the host's own project-local
    // installer writes. A pointer that disagrees with the installer makes
    // `verify` observe `Missing` immediately after a successful apply, which
    // rolls the whole transaction back and reports it as a storage failure.
    let current = match integration_id {
        "codex" => document.get("name").and_then(serde_json::Value::as_str) == Some("tracedecay"),
        // Kilo registers under `mcp` (not `mcpServers`), same as OpenCode.
        "opencode" | "kilo" => document.pointer("/mcp/tracedecay").is_some(),
        "kimi" | "roo-code" => document.pointer("/mcpServers/tracedecay").is_some(),
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
        self.refuse_ambiguous_opencode_analyzer(component_set)?;
        if self.registration_mode(component_set) == CompatibilityRegistrationMode::ArtifactOnly {
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
        let mode = self.registration_mode(component_set);
        if mode == CompatibilityRegistrationMode::ArtifactOnly {
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
            return Err(host_bundle_storage_failure!());
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

/// Whether this component set is Kiro's `context_mcp` set, whose single managed
/// artifact (`~/.kiro/settings/mcp.json`) is simultaneously the file Kiro's
/// native MCP registration writes. The artifact bytes are already a complete,
/// valid registration document, so the artifact write is the whole lifecycle
/// and the native activation must not run as a second writer.
fn kiro_registration_is_the_artifact(
    component_set: &crate::agents::host_bundle_v2::HostComponentSetV1,
) -> bool {
    component_set.host == crate::agents::host_bundle_v2::HostKindV1::Kiro
        && component_set.components.len() == 1
        && component_set.components[0].manifest.component
            == crate::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp
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

    fn empty_claude_component_set() -> crate::agents::host_bundle_v2::HostComponentSetV1 {
        crate::agents::host_bundle_v2::HostComponentSetV1 {
            host: crate::agents::host_bundle_v2::HostKindV1::ClaudeCode,
            components: Vec::new(),
        }
    }

    /// Build a delegate plus an on-disk registration backup whose mutation plan
    /// declares `directories`, marking every one of them absent at backup time.
    fn missing_directory_fixture(
        home: &Path,
        lifecycle_root: &Path,
        directories: &[PathBuf],
    ) -> (HostComponentRegistrationDelegate, [u8; 16]) {
        let delegate = HostComponentRegistrationDelegate::new_with_tracedecay_bin(
            "claude",
            home,
            lifecycle_root,
            crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install,
            "tracedecay".to_string(),
        )
        .expect("delegate");
        let operation_id = [7u8; 16];
        fs::create_dir_all(delegate.backup_dir(operation_id)).expect("backup dir");
        let registration_paths = delegate.registration_paths(&empty_claude_component_set());
        let plan = RegistrationMutationPlanV1 {
            schema_version: 2,
            integration_id: "claude".to_string(),
            operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install,
            paths: registration_paths.clone(),
            directories: directories.to_vec(),
        };
        write_registration_backup(
            &delegate.mutation_plan_path(operation_id),
            &serde_json::to_vec(&plan).unwrap(),
        )
        .expect("plan");
        for (index, directory) in directories.iter().enumerate() {
            write_registration_backup(
                &delegate.directory_missing_marker(operation_id, index),
                b"missing",
            )
            .expect("missing marker");
            write_registration_backup(
                &delegate.directory_path_marker(operation_id, index),
                &serde_json::to_vec(directory).unwrap(),
            )
            .expect("directory path marker");
        }
        // Every registration path the rollback will consult needs an original
        // state on disk; this fixture is only about the directory inventory, so
        // record them all as absent.
        for (index, path) in registration_paths.iter().enumerate() {
            write_registration_backup(
                &delegate.missing_marker_path(operation_id, index),
                b"missing",
            )
            .expect("registration missing marker");
            write_registration_backup(
                &delegate.registration_path_marker(operation_id, index),
                &serde_json::to_vec(path).unwrap(),
            )
            .expect("registration path marker");
        }
        let identity = RegistrationBackupIdentityV1::new(
            "claude",
            home,
            lifecycle_root,
            delegate.rollback_project_path(),
        )
        .expect("identity");
        write_registration_backup(
            &delegate.identity_path(operation_id),
            &serde_json::to_vec(&identity).unwrap(),
        )
        .expect("identity write");
        (delegate, operation_id)
    }

    /// The transaction writes its declared artifacts *before* `apply` runs, and
    /// writing `<dir>/artifact.md` creates `<dir>`. A registration directory the
    /// backup recorded as absent is therefore expected to be present by then.
    /// Reporting that as foreign drift made the very first `install` into a
    /// fresh home fail outright.
    #[test]
    fn declared_artifact_write_directory_is_adopted_not_reported_as_drift() {
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let managed = home.path().join(".claude/agents");

        let (mut delegate, operation_id) =
            missing_directory_fixture(home.path(), lifecycle.path(), &[managed.clone()]);
        delegate.declared_artifact_writes = [managed.join("code-explorer.md")].into();

        // The artifact writer already created the directory tree.
        fs::create_dir_all(&managed).unwrap();

        delegate
            .prepare_missing_registration_directories(operation_id)
            .expect("a directory created by this transaction's own write is not foreign drift");

        assert!(
            delegate
                .directory_applied_metadata_marker(operation_id, 0)
                .is_file(),
            "adoption must record the applied state that rollback needs to reclaim the directory"
        );
        assert!(managed.is_dir(), "the adopted directory is left in place");
    }

    /// The same directory appearing without any declared write underneath it is
    /// still a genuinely foreign mutation and must abort the apply.
    #[test]
    fn undeclared_directory_reappearing_is_still_drift() {
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let foreign = home.path().join(".claude/agents");

        let (delegate, operation_id) =
            missing_directory_fixture(home.path(), lifecycle.path(), &[foreign.clone()]);
        fs::create_dir_all(&foreign).unwrap();

        assert!(
            matches!(
                delegate.prepare_missing_registration_directories(operation_id),
                Err(HostBundleError::StalePreview(_))
            ),
            "nothing in this transaction claims the path, so its reappearance is foreign"
        );
    }

    /// A directory is only adopted for a *strict* descendant write. A declared
    /// write whose path is the directory itself is a file, not a parent, and
    /// proves nothing about who created the directory.
    #[test]
    fn declared_write_equal_to_the_directory_does_not_claim_it() {
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude/agents");

        let (mut delegate, operation_id) =
            missing_directory_fixture(home.path(), lifecycle.path(), &[path.clone()]);
        delegate.declared_artifact_writes = [path.clone()].into();
        fs::create_dir_all(&path).unwrap();

        assert!(matches!(
            delegate.prepare_missing_registration_directories(operation_id),
            Err(HostBundleError::StalePreview(_))
        ));
    }

    /// The wedge this pair of defects produced: an operation died after its
    /// artifact write created a registration directory but before `apply` could
    /// record the applied-state marker. Rollback then found a directory the
    /// backup said was absent, with nothing attributing it, and refused. The
    /// journal stayed on disk and every later lifecycle command reported the
    /// same stale preview, with no way out short of deleting state by hand.
    #[test]
    fn rollback_converges_when_a_missing_directory_has_no_applied_marker() {
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let orphaned = home.path().join(".claude/agents");

        let (delegate, operation_id) =
            missing_directory_fixture(home.path(), lifecycle.path(), &[orphaned.clone()]);
        // The state the dead operation left behind: the directory exists, but
        // no `directory-0.applied.metadata.json` ever got written.
        fs::create_dir_all(&orphaned).unwrap();
        assert!(
            !delegate
                .directory_applied_metadata_marker(operation_id, 0)
                .is_file()
        );

        let component_set = empty_claude_component_set();

        delegate
            .restore_registration(&component_set, operation_id)
            .expect("rollback must converge instead of wedging on an unattributable directory");

        assert!(
            !orphaned.exists(),
            "an empty directory this operation introduced is removed, restoring the pre-op tree"
        );
    }

    /// Convergence must not become a licence to delete a directory somebody
    /// else is using: the removal stays empty-guarded.
    #[test]
    fn rollback_leaves_a_non_empty_unattributed_directory_in_place() {
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let orphaned = home.path().join(".claude/agents");

        let (delegate, operation_id) =
            missing_directory_fixture(home.path(), lifecycle.path(), &[orphaned.clone()]);
        fs::create_dir_all(&orphaned).unwrap();
        fs::write(orphaned.join("someone-elses.md"), b"keep me").unwrap();

        let component_set = empty_claude_component_set();

        delegate
            .restore_registration(&component_set, operation_id)
            .expect("rollback still converges");

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

    /// The pointer each host is classified by must match the key its own
    /// `install_local` writes. When the two disagree, a successful apply is
    /// observed as `Missing` by `verify`, the transaction rolls back, and the
    /// operator sees "atomic filesystem operation failed" for a host that was
    /// registered correctly. Driving the real installer keeps the two in step.
    #[test]
    fn project_registration_state_matches_what_each_installer_writes() {
        use crate::agents::host_bundle_v2::HostBundleRegistrationStateV1 as State;

        let integrations: Vec<Box<dyn crate::agents::AgentIntegration>> = vec![
            Box::new(crate::agents::KiloIntegration),
            Box::new(crate::agents::KimiIntegration),
            Box::new(crate::agents::RooCodeIntegration),
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
                .install_local(&context, project.path())
                .unwrap_or_else(|error| {
                    panic!("{} local install failed: {error}", integration.id())
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
                "{} project-local registration must be observed as current after install_local",
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

    #[test]
    fn legacy_global_claude_identity_accepts_project_bound_by_persisted_paths() {
        let home = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut identity =
            RegistrationBackupIdentityV1::new("claude", home.path(), profile.path(), None).unwrap();
        identity.schema_version = 1;

        assert_eq!(
            identity.validate("claude", home.path(), profile.path(), Some(project.path())),
            Ok(())
        );
        assert_eq!(
            identity.validate("claude", other.path(), profile.path(), Some(project.path())),
            Err(HostBundleError::WrongTarget)
        );

        identity.schema_version = 2;
        assert_eq!(
            identity.validate("claude", home.path(), profile.path(), Some(project.path())),
            Err(HostBundleError::WrongTarget)
        );
    }
}
