//! Receipt-backed host-native registration lifecycle shared by CLI and daemon owners.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use tracedecay_runtime_core::config::ProfileRoot;

use sha2::{Digest, Sha256};
use tracedecay_host_integration::host_bundle_stale_preview;
use tracedecay_host_integration::host_bundle_storage_failure;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RegistrationObservedStateV1 {
    present: bool,
    digest: [u8; 32],
    metadata: Option<crate::agents::HostFileMetadataIdentityV1>,
}

type RegistrationFileBytes = Option<(Vec<u8>, crate::agents::HostFileMetadataIdentityV1)>;

/// Pre-effect registration bytes for the one in-flight operation, held only in
/// this authority. Nothing is persisted: a restarted process has no copy to
/// restore, and the next lifecycle run reconciles the host registration.
struct StagedRegistration {
    operation_id: [u8; 16],
    files: Vec<(PathBuf, RegistrationFileBytes)>,
    applied: Option<Vec<RegistrationObservedStateV1>>,
    effect_started: bool,
}

pub struct CatalogHostComponentRegistrationAuthority {
    integration: Box<dyn crate::agents::AgentIntegration>,
    context: crate::agents::InstallContext,
    health_context: crate::agents::HealthcheckContext,
    registration_path: Option<PathBuf>,
    operation: crate::agents::host_bundle::HostBundleLifecycleOpV1,
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
    staged: Option<StagedRegistration>,
    /// The operator step for a host whose only activation route is
    /// interactive: the transaction commits the staged source that route
    /// consumes and leaves the host registration untouched.
    deferred_activation: Option<crate::agents::DeferredUserAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CatalogRegistrationMode {
    /// The component artifacts are discovered directly by the host. The
    /// transaction's artifact verification is the complete lifecycle.
    ArtifactOnly,
    /// The host needs only a native registry entry after assets are deployed.
    DeployedActivation,
}

impl CatalogHostComponentRegistrationAuthority {
    fn validate_catalog_host(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
        (self.integration.id() == crate::agents::integration_id_for_host(component_set.host))
            .then_some(())
            .ok_or(crate::agents::host_bundle::HostBundleError::WrongTarget)
    }

    pub fn new(
        profile: &ProfileRoot,
        agent_id: &str,
        home: &Path,
        operation: crate::agents::host_bundle::HostBundleLifecycleOpV1,
    ) -> tracedecay_domain::errors::Result<Self> {
        let tracedecay_bin = current_tracedecay_binary()?;
        Self::new_with_tracedecay_bin(profile, agent_id, home, operation, tracedecay_bin)
    }

    pub fn new_with_tracedecay_bin(
        profile: &ProfileRoot,
        agent_id: &str,
        home: &Path,
        operation: crate::agents::host_bundle::HostBundleLifecycleOpV1,
        tracedecay_bin: String,
    ) -> tracedecay_domain::errors::Result<Self> {
        Self::new_with_tracedecay_bin_and_dashboard(
            profile,
            agent_id,
            home,
            operation,
            tracedecay_bin,
            true,
        )
    }

    pub fn new_with_tracedecay_bin_and_dashboard(
        profile: &ProfileRoot,
        agent_id: &str,
        home: &Path,
        operation: crate::agents::host_bundle::HostBundleLifecycleOpV1,
        tracedecay_bin: String,
        dashboard: bool,
    ) -> tracedecay_domain::errors::Result<Self> {
        let project_path = std::env::current_dir().map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to resolve host lifecycle project path: {error}"),
            }
        })?;
        let integration = crate::agents::get_integration(agent_id)?;
        let registration_path = integration.primary_config_path(home, profile);
        Ok(Self {
            integration,
            context: crate::agents::InstallContext {
                home: home.to_path_buf(),
                profile: profile.clone(),
                tracedecay_bin,
                project_root: None,
                dashboard,
            },
            health_context: crate::agents::HealthcheckContext {
                home: home.to_path_buf(),
                profile: profile.clone(),
                project_path,
            },
            registration_path,
            operation,
            should_apply: false,
            confirmed_registration_revision: None,
            declared_artifact_writes: BTreeSet::new(),
            staged_foreign_registration_revision: None,
            staged: None,
            deferred_activation: None,
        })
    }

    /// The host action still required after this transaction committed its
    /// staged source, or `None` when the registration was fully applied.
    pub fn deferred_activation(&self) -> Option<&crate::agents::DeferredUserAction> {
        self.deferred_activation.as_ref()
    }

    fn registration_error(
        host: crate::agents::host_bundle::HostKindV1,
        error: tracedecay_domain::errors::TraceDecayError,
    ) -> crate::agents::host_bundle::HostBundleError {
        if matches!(
            &error,
            tracedecay_domain::errors::TraceDecayError::HostCliUnavailable { .. }
        ) {
            return crate::agents::host_bundle::HostBundleError::HostCliUnavailable { host };
        }
        // The transaction error vocabulary is fixed, so surface the
        // integration's own message here before it is collapsed into the
        // generic storage failure, otherwise the actionable cause (for
        // example a refused symlinked project config) is lost.
        eprintln!("{error}");
        host_bundle_storage_failure!()
    }

    fn registration_mode(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
    ) -> CatalogRegistrationMode {
        if component_set.host == crate::agents::host_bundle::HostKindV1::ClaudeCode
            || component_set.host == crate::agents::host_bundle::HostKindV1::Codex
            || component_set.host == crate::agents::host_bundle::HostKindV1::Devin
        || component_set.host == crate::agents::host_bundle::HostKindV1::Zed
        || component_set.host == crate::agents::host_bundle::HostKindV1::Antigravity
        || component_set.host == crate::agents::host_bundle::HostKindV1::Vibe
            || component_set.host == crate::agents::host_bundle::HostKindV1::Hermes
            || component_set.host == crate::agents::host_bundle::HostKindV1::KimiCode
            || component_set.host == crate::agents::host_bundle::HostKindV1::Kiro
            // Gemini's deployed artifacts are the extension *source*; the host
            // only carries the integration once `gemini extensions install`
            // adopts them, so the deployed bytes alone are not the lifecycle.
            || component_set.host == crate::agents::host_bundle::HostKindV1::Gemini
            // Copilot's deployed artifact is a receipt-owned component
            // descriptor; the host carries nothing until `copilot mcp add`
            // writes its own registry, so the deployed bytes alone are not the
            // lifecycle.
            || component_set.host == crate::agents::host_bundle::HostKindV1::Copilot
            // Factory Droid is Copilot's shape exactly: `droid mcp add`
            // writes the host-owned `~/.factory/mcp.json`; the deployed
            // descriptor alone is not the lifecycle.
            || component_set.host == crate::agents::host_bundle::HostKindV1::FactoryDroid
            || component_set.host == crate::agents::host_bundle::HostKindV1::Cline
            || component_set.host == crate::agents::host_bundle::HostKindV1::RooCode
            || component_set.host == crate::agents::host_bundle::HostKindV1::Kilo
            // Pi loads its deployed artifacts directly unless
            // `PI_CODING_AGENT_DIR` relocates it; only then does it carry
            // native registration state, the mirrors activation writes.
            || (component_set.host == crate::agents::host_bundle::HostKindV1::Pi
                && !self
                    .integration
                    .host_component_registration_paths(
                        &component_set
                            .components
                            .iter()
                            .map(|component| component.manifest.component)
                            .collect::<Vec<_>>(),
                        &self.context.home,
                        &self.context.profile,
                    )
                    .is_empty())
            || (component_set.host == crate::agents::host_bundle::HostKindV1::OpenCode
                && component_set.components.iter().any(|component| {
                    matches!(
                        component.manifest.component,
                        crate::agents::host_bundle::HostComponentV1::Core
                            | crate::agents::host_bundle::HostComponentV1::ContextMcp
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

    fn requires_competing_analyzer_preflight(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
    ) -> bool {
        component_set.host == crate::agents::host_bundle::HostKindV1::OpenCode
            && component_set.components.iter().any(|component| {
                component.manifest.component == crate::agents::host_bundle::HostComponentV1::Core
            })
    }

    fn component_registration_revision(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
    ) -> Result<[u8; 32], crate::agents::host_bundle::HostBundleError> {
        self.validate_catalog_host(component_set)?;
        match self.registration_mode(component_set) {
            CatalogRegistrationMode::ArtifactOnly
                if !self.requires_competing_analyzer_preflight(component_set) =>
            {
                Ok(Sha256::digest(b"tracedecay.host-registration.none.v1").into())
            }
            CatalogRegistrationMode::ArtifactOnly | CatalogRegistrationMode::DeployedActivation => {
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
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
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
            let surface = self.registration_path.as_deref().map_or_else(
                || "the opencode configuration".to_string(),
                |path| path.display().to_string(),
            );
            return Err(
                crate::agents::host_bundle::HostBundleError::OwnershipConflict(format!(
                    "{surface}: a non-tracedecay LSP entry runs the tracedecay binary, so \
                     ownership of the analyzer registration cannot be resolved"
                )),
            );
        }
        Ok(())
    }

    /// Parse the host's own registration document once. An unreadable or
    /// unparseable document is a refusal rather than "no conflict": discovery
    /// that cannot see the surface must never report it as clear.
    fn opencode_registration_document(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
    ) -> Result<Option<(serde_json::Value, [u8; 32])>, crate::agents::host_bundle::HostBundleError>
    {
        if component_set.host != crate::agents::host_bundle::HostKindV1::OpenCode {
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
            .map_err(|_| crate::agents::host_bundle::HostBundleError::InvalidObservedState)?;
        Ok(Some((config, Sha256::digest(&bytes).into())))
    }

    /// Third-party analyzers already registered for a language this component
    /// set's own analyzer would serve. `OpenCode` is the only host whose set
    /// registers a custom analyzer; every other host's component set writes
    /// TraceDecay-keyed entries that no third party can already own.
    fn competing_opencode_analyzer_claims(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
    ) -> Result<
        Vec<crate::agents::host_bundle::CompetingHostExtensionClaimV1>,
        crate::agents::host_bundle::HostBundleError,
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
                |(name, _)| crate::agents::host_bundle::CompetingHostExtensionClaimV1 {
                    extension_id: claim_identifier(name),
                    capability: crate::agents::host_bundle::HostCapabilityV1::Lsp,
                    evidence_digest,
                },
            )
            .collect())
    }

    fn registration_is_current(
        &self,
        component: crate::agents::host_bundle::HostComponentV1,
    ) -> crate::agents::host_bundle::HostBundleRegistrationStateV1 {
        self.integration.host_component_registration_for_lifecycle(
            component,
            &self.health_context,
            &self.context,
        )
    }

    fn registration_paths(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
    ) -> Result<Vec<PathBuf>, crate::agents::host_bundle::HostBundleError> {
        let components = component_set
            .components
            .iter()
            .map(|component| component.manifest.component)
            .collect::<Vec<_>>();
        let mut paths = self
            .integration
            .host_component_registration_paths_checked(
                &components,
                &self.context.home,
                &self.context.profile,
            )
            .map_err(|error| Self::registration_error(component_set.host, error))?;
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

    fn current_registration_revision(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
    ) -> Result<[u8; 32], crate::agents::host_bundle::HostBundleError> {
        self.registration_revision_excluding(component_set, &BTreeSet::new())
    }

    /// Revision of the registration surface with the transaction's own
    /// declared writes held constant.
    ///
    /// Some hosts register themselves *through* a file this component set also
    /// installs as a managed artifact. Kiro's `~/.kiro/settings/mcp.json` is
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
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        excluded: &BTreeSet<PathBuf>,
    ) -> Result<[u8; 32], crate::agents::host_bundle::HostBundleError> {
        if self.integration.id() == "claude" {
            let claude_root = self.context.home.join(".claude");
            if fs::symlink_metadata(&claude_root)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(crate::agents::host_bundle::HostBundleError::UnsafeClaudeHomeSymlink);
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
                        return Err(crate::agents::host_bundle::HostBundleError::UnsafeInstallPath);
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
                    crate::agents::host_bundle::HostBundleRegistrationStateV1::Current => 1,
                    crate::agents::host_bundle::HostBundleRegistrationStateV1::Repairable => 2,
                    crate::agents::host_bundle::HostBundleRegistrationStateV1::Missing => 3,
                    crate::agents::host_bundle::HostBundleRegistrationStateV1::Corrupt => 4,
                }]);
            }
        }
        Ok(digest.finalize().into())
    }

    fn stage_registration(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        operation_id: [u8; 16],
    ) -> Result<StagedRegistration, crate::agents::host_bundle::HostBundleError> {
        let mut files = Vec::new();
        for path in self.registration_paths(component_set)? {
            let original = match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    return Err(crate::agents::host_bundle::HostBundleError::UnsafeInstallPath);
                }
                Ok(_) => Some((
                    fs::read(&path).map_err(|_| host_bundle_storage_failure!())?,
                    crate::agents::capture_host_file_metadata(&path)
                        .map_err(|_| host_bundle_storage_failure!())?,
                )),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(host_bundle_storage_failure!()),
            };
            files.push((path, original));
        }
        Ok(StagedRegistration {
            operation_id,
            files,
            applied: None,
            effect_started: false,
        })
    }

    fn staged_registration(
        &mut self,
        operation_id: [u8; 16],
    ) -> Result<&mut StagedRegistration, crate::agents::host_bundle::HostBundleError> {
        self.staged
            .as_mut()
            .filter(|staged| staged.operation_id == operation_id)
            .ok_or(crate::agents::host_bundle::HostBundleError::WrongTarget)
    }

    fn capture_applied_registration(
        &mut self,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
        let staged = self.staged_registration(operation_id)?;
        let applied = staged
            .files
            .iter()
            .map(|(path, _)| registration_observed_state(path))
            .collect::<Result<Vec<_>, _>>()?;
        staged.applied = Some(applied);
        Ok(())
    }

    fn validate_applied_registration(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
        let staged = self
            .staged
            .as_ref()
            .filter(|staged| staged.operation_id == operation_id)
            .ok_or(crate::agents::host_bundle::HostBundleError::WrongTarget)?;
        let applied = staged
            .applied
            .as_ref()
            .ok_or(crate::agents::host_bundle::HostBundleError::WrongTarget)?;
        for ((path, _), expected) in staged.files.iter().zip(applied) {
            if registration_observed_state(path)? != *expected {
                return Err(host_bundle_stale_preview!());
            }
        }
        Ok(())
    }

    /// Restore the staged pre-effect bytes. Every path must still hold either
    /// its original or its just-applied state before any byte is rewritten, so
    /// a foreign edit made during the operation is never overwritten.
    fn restore_registration(
        staged: &StagedRegistration,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
        for (index, (path, original)) in staged.files.iter().enumerate() {
            let observed = registration_observed_state(path)?;
            if observed != original_observed_state(original.as_ref())
                && staged.applied.as_ref().map(|applied| &applied[index]) != Some(&observed)
            {
                return Err(host_bundle_stale_preview!());
            }
        }
        for (path, original) in &staged.files {
            let observed = registration_observed_state(path)?;
            match original {
                Some((bytes, metadata)) => {
                    if observed != original_observed_state(original.as_ref()) {
                        crate::agents::safe_write_bytes_file_with_metadata(
                            path,
                            bytes,
                            Some(metadata),
                        )
                        .map_err(|_| host_bundle_storage_failure!())?;
                    }
                    crate::agents::restore_host_file_metadata(path, metadata)
                        .map_err(|_| host_bundle_storage_failure!())?;
                    sync_registration_metadata(path)?;
                }
                None if observed.present => match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_file() => {
                        fs::remove_file(path).map_err(|_| host_bundle_storage_failure!())?;
                    }
                    Ok(_) => {
                        return Err(crate::agents::host_bundle::HostBundleError::UnsafeInstallPath);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(host_bundle_storage_failure!()),
                },
                None => {}
            }
        }
        Ok(())
    }
}

fn current_tracedecay_binary() -> tracedecay_domain::errors::Result<String> {
    std::env::current_exe()
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("failed to resolve the running tracedecay binary: {error}"),
        })?
        .into_os_string()
        .into_string()
        .map_err(|path| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "the running tracedecay binary path is not valid UTF-8: {}",
                PathBuf::from(path).display()
            ),
        })
}

impl crate::agents::host_bundle::HostComponentSetRegistrationV1
    for CatalogHostComponentRegistrationAuthority
{
    fn current_revision(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        _request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
    ) -> Result<[u8; 32], crate::agents::host_bundle::HostBundleError> {
        self.component_registration_revision(component_set)
    }

    fn discover_competing_extension_claims(
        &self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        _request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
    ) -> Result<
        Vec<crate::agents::host_bundle::CompetingHostExtensionClaimV1>,
        crate::agents::host_bundle::HostBundleError,
    > {
        self.competing_opencode_analyzer_claims(component_set)
    }

    fn confirm_preview(
        &mut self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
        preview: &crate::agents::host_bundle::HostComponentSetLifecyclePreviewV1,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
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
        _component_set: &crate::agents::host_bundle::HostComponentSetV1,
        _request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
        paths: &[PathBuf],
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
        self.declared_artifact_writes = paths.iter().cloned().collect();
        Ok(())
    }

    #[hotpath::measure(label = "hosts.agent.host_bundle.registration_preflight")]
    fn preflight(
        &mut self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        _request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
        self.validate_catalog_host(component_set)?;
        self.refuse_ambiguous_opencode_analyzer(component_set)?;
        if self.registration_mode(component_set) == CatalogRegistrationMode::ArtifactOnly {
            self.should_apply = false;
            return Ok(());
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
            *state == crate::agents::host_bundle::HostBundleRegistrationStateV1::Current
        });
        let all_missing = states.iter().all(|state| {
            *state == crate::agents::host_bundle::HostBundleRegistrationStateV1::Missing
        });
        let corrupt_components = component_set
            .components
            .iter()
            .zip(&states)
            .filter(|(_, state)| {
                **state == crate::agents::host_bundle::HostBundleRegistrationStateV1::Corrupt
            })
            .map(|(component, _)| format!("{:?}", component.manifest.component))
            .collect::<Vec<_>>();
        if !corrupt_components.is_empty() {
            let surfaces = self
                .registration_paths(component_set)?
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(
                crate::agents::host_bundle::HostBundleError::OwnershipConflict(format!(
                    "host-native registration for {} is unreadable or contradictory; inspect {}",
                    corrupt_components.join(", "),
                    surfaces
                )),
            );
        }
        if self.operation != crate::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall {
            let components = component_set
                .components
                .iter()
                .map(|component| component.manifest.component)
                .collect::<Vec<_>>();
            let foreign = self
                .integration
                .foreign_bundle_entrypoints(
                    &components,
                    &self.context.home,
                    self.context.profile.data_dir(),
                )
                .map_err(|error| Self::registration_error(component_set.host, error))?;
            if !foreign.is_empty() {
                let paths = foreign
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(
                    crate::agents::host_bundle::HostBundleError::OwnershipConflict(format!(
                        "{paths}: not shipped by any TraceDecay release, but the host would load \
                         it as part of the TraceDecay plugin; move it out of the plugin \
                         directory and retry"
                    )),
                );
            }
        }
        // Claude's global install, Hermes' named-profile projection, and Pi's
        // relocated mirror all derive host-owned registration from deployed
        // component bytes. An install may replace those bytes while the
        // preflight registration still reads Current, so each must re-activate
        // after every install.
        let always_refresh_registration_on_install = matches!(
            component_set.host,
            crate::agents::host_bundle::HostKindV1::ClaudeCode
                | crate::agents::host_bundle::HostKindV1::Hermes
                | crate::agents::host_bundle::HostKindV1::Pi
        ) && self.operation
            == crate::agents::host_bundle::HostBundleLifecycleOpV1::Install;
        self.should_apply = match self.operation {
            // A registration that is partially present or `Repairable` on
            // install is TraceDecay's own residue, staged sources, a
            // marketplace entry, or a stale native cache left by a prior
            // install of this same bundle. Reinstall/update over it must
            // converge by re-activating, exactly as `Update` does; only a
            // `Corrupt` (unreadable/contradictory) surface refuses above.
            // Refusing the mixed states here made every reinstall of a
            // partially activated host fail as a phantom ownership conflict.
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Install => {
                !all_current || always_refresh_registration_on_install
            }
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall => !all_missing,
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Update
            | crate::agents::host_bundle::HostBundleLifecycleOpV1::Repair => true,
        };
        self.deferred_activation = None;
        let interactive_guidance = match self.operation {
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall => {
                self.integration.interactive_removal_guidance()
            }
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Install
            | crate::agents::host_bundle::HostBundleLifecycleOpV1::Update
            | crate::agents::host_bundle::HostBundleLifecycleOpV1::Repair => {
                if component_set.host == crate::agents::host_bundle::HostKindV1::KimiCode {
                    match self
                        .integration
                        .preflight_non_interactive_install(&self.context)
                        .map_err(|error| Self::registration_error(component_set.host, error))?
                    {
                        crate::agents::NonInteractiveInstallOutcome::Ready => None,
                        // Kimi's interactive `/plugins install` consumes the
                        // staged source, so the transaction must still commit
                        // it; the registration stays untouched.
                        crate::agents::NonInteractiveInstallOutcome::DeferredUserAction(action) => {
                            self.should_apply = false;
                            self.deferred_activation = Some(action);
                            return Ok(());
                        }
                    }
                } else {
                    self.integration.interactive_activation_guidance()
                }
            }
        };
        if interactive_guidance.is_some() {
            let native_state_already_matches = match self.operation {
                crate::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall => all_missing,
                crate::agents::host_bundle::HostBundleLifecycleOpV1::Install
                | crate::agents::host_bundle::HostBundleLifecycleOpV1::Update
                | crate::agents::host_bundle::HostBundleLifecycleOpV1::Repair => all_current,
            };
            if native_state_already_matches {
                self.should_apply = false;
            } else if self.operation
                == crate::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall
            {
                // Removal is the host's to perform first: stripping a bundle the
                // host still has registered would leave it resolving a
                // marketplace that no longer exists. This arm must precede the
                // update/activation arms below, both of those remediate as
                // "refresh or activate the plugin", which can never unblock an
                // uninstall, so routing removal through them makes the host's
                // integration impossible to remove. Once the operator has run
                // the host's own removal the registration reads `Missing`,
                // `native_state_already_matches` holds, and the transaction
                // proceeds to delete the receipt-owned artifacts.
                return Err(crate::agents::host_bundle::HostBundleError::NativeRemovalRequired);
            } else if matches!(
                self.operation,
                crate::agents::host_bundle::HostBundleLifecycleOpV1::Update
                    | crate::agents::host_bundle::HostBundleLifecycleOpV1::Repair
            ) || states.iter().any(|state| {
                *state == crate::agents::host_bundle::HostBundleRegistrationStateV1::Repairable
            }) {
                return Err(crate::agents::host_bundle::HostBundleError::NativeUpdateRequired);
            } else {
                // Native-only activation must complete in the host before the
                // transaction claims any staged artifact.
                return Err(crate::agents::host_bundle::HostBundleError::UnsupportedCapability);
            }
        }
        Ok(())
    }

    #[hotpath::measure(label = "hosts.agent.host_bundle.registration_stage")]
    fn stage(
        &mut self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
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
        self.staged = Some(self.stage_registration(component_set, request.operation_id)?);
        Ok(())
    }

    #[hotpath::measure(label = "hosts.agent.host_bundle.registration_apply")]
    fn apply(
        &mut self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
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
            return self.capture_applied_registration(request.operation_id);
        }
        self.staged_registration(request.operation_id)?
            .effect_started = true;
        let components = component_set
            .components
            .iter()
            .map(|component| component.manifest.component)
            .collect::<Vec<_>>();
        let result = match request.lifecycle.operation {
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall => self
                .integration
                .deactivate_deployed_host_component_registration(&components, &self.context),
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Install
            | crate::agents::host_bundle::HostBundleLifecycleOpV1::Update
            | crate::agents::host_bundle::HostBundleLifecycleOpV1::Repair => self
                .integration
                .activate_deployed_host_component_registration(&components, &self.context),
        }
        .map_err(|error| Self::registration_error(component_set.host, error));
        let captured = self.capture_applied_registration(request.operation_id);
        match (result, captured) {
            (_, Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn verify(
        &mut self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
        self.validate_catalog_host(component_set)?;
        #[cfg(feature = "test-transport")]
        if std::env::var_os("TRACEDECAY_TEST_FAIL_HOST_REGISTRATION_VERIFY").is_some() {
            return Err(host_bundle_storage_failure!());
        }
        if self.registration_mode(component_set) == CatalogRegistrationMode::ArtifactOnly {
            return Ok(());
        }
        self.validate_applied_registration(request.operation_id)?;
        if self.deferred_activation.is_some() {
            return Ok(());
        }
        let expected = if request.lifecycle.operation
            == crate::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall
        {
            crate::agents::host_bundle::HostBundleRegistrationStateV1::Missing
        } else {
            crate::agents::host_bundle::HostBundleRegistrationStateV1::Current
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
        _component_set: &crate::agents::host_bundle::HostComponentSetV1,
        _request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
        self.staged = None;
        Ok(())
    }

    fn rollback(
        &mut self,
        component_set: &crate::agents::host_bundle::HostComponentSetV1,
        request: &crate::agents::host_bundle::HostComponentSetExecutionRequestV1,
    ) -> Result<(), crate::agents::host_bundle::HostBundleError> {
        self.validate_catalog_host(component_set)?;
        if self.registration_mode(component_set) == CatalogRegistrationMode::ArtifactOnly {
            return Ok(());
        }
        // A rollback before `stage` ran finds nothing staged; there is no
        // pre-effect copy outside this process by design.
        let Some(staged) = self
            .staged
            .take()
            .filter(|staged| staged.operation_id == request.operation_id)
        else {
            return Ok(());
        };
        if !staged.effect_started {
            return Ok(());
        }
        Self::restore_registration(&staged)
    }
}

/// Languages the component set's own `OpenCode` analyzer registration declares.
/// `None` means the projection declares no bounded extension list, so every
/// other analyzer must be treated as overlapping.
fn opencode_tracedecay_extensions(
    component_set: &crate::agents::host_bundle::HostComponentSetV1,
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
    if crate::agents::host_bundle::validate_identifier(name).is_ok() {
        return name.to_string();
    }
    format!("opaque-{}", hex::encode(&Sha256::digest(name)[..8]))
}

fn registration_observed_state(
    path: &Path,
) -> Result<RegistrationObservedStateV1, crate::agents::host_bundle::HostBundleError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(crate::agents::host_bundle::HostBundleError::UnsafeInstallPath)
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

fn original_observed_state(
    original: Option<&(Vec<u8>, crate::agents::HostFileMetadataIdentityV1)>,
) -> RegistrationObservedStateV1 {
    match original {
        Some((bytes, metadata)) => RegistrationObservedStateV1 {
            present: true,
            digest: Sha256::digest(bytes).into(),
            metadata: Some(metadata.clone()),
        },
        None => RegistrationObservedStateV1 {
            present: false,
            digest: [0; 32],
            metadata: None,
        },
    }
}

fn sync_registration_metadata(
    path: &Path,
) -> Result<(), crate::agents::host_bundle::HostBundleError> {
    tracedecay_private_fs::framed_log::sync_file_at(path)
        .and_then(|()| {
            tracedecay_private_fs::framed_log::sync_parent_directory(
                path,
                tracedecay_private_fs::framed_log::DirectorySyncPolicy::TolerateUnsupported,
            )
        })
        .map_err(|_| host_bundle_storage_failure!())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::host_bundle::{HostBundleError, HostKindV1};

    #[test]
    fn typed_host_cli_absence_stays_distinct_from_config_failure() {
        let unavailable = CatalogHostComponentRegistrationAuthority::registration_error(
            HostKindV1::Kiro,
            tracedecay_domain::errors::TraceDecayError::HostCliUnavailable {
                program: "kiro-cli".to_string(),
                lifecycle: "kiro MCP registry lifecycle".to_string(),
            },
        );
        assert_eq!(
            unavailable,
            HostBundleError::HostCliUnavailable {
                host: HostKindV1::Kiro,
            },
            "a proven absent Kiro CLI must not be relabelled as a filesystem failure"
        );

        let config_failure = CatalogHostComponentRegistrationAuthority::registration_error(
            HostKindV1::Kiro,
            tracedecay_domain::errors::TraceDecayError::Config {
                message: "malformed Kiro MCP config".to_string(),
            },
        );
        assert!(
            matches!(config_failure, HostBundleError::StorageFailure(_)),
            "a genuine host config failure must retain the existing lifecycle failure mapping"
        );
    }

    /// Gemini's deployed artifacts are an extension *source*: the host carries
    /// nothing until `gemini extensions install` adopts them. Classifying the
    /// set as artifact-only would let a lifecycle report an activation that
    /// never happened.
    #[test]
    fn gemini_component_sets_are_not_artifact_only_lifecycles() {
        let home = tempfile::tempdir().expect("home");
        let authority = CatalogHostComponentRegistrationAuthority::new(
            &tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
            "gemini",
            home.path(),
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Install,
        )
        .expect("catalog registration authority");
        let component_set =
            crate::agents::host_bundle_registry::verified_embedded_default_host_component_set(
                crate::agents::host_bundle::HostKindV1::Gemini,
                0,
                crate::agents::TEST_GENERATOR_COMMIT,
            )
            .expect("Gemini has a compiled default set");

        assert!(
            authority.registration_mode(&component_set.component_set)
                != CatalogRegistrationMode::ArtifactOnly,
            "the Gemini lifecycle drives `gemini extensions install`, so its deployed \
             bytes are not the whole lifecycle"
        );
        // Control: Cursor's component set really is fully represented by its
        // managed artifacts, so the assertion above is about Gemini's
        // classification and not about a predicate that always answers false.
        let cursor = CatalogHostComponentRegistrationAuthority::new(
            &tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
            "cursor",
            home.path(),
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Install,
        )
        .expect("catalog registration authority");
        let cursor_set =
            crate::agents::host_bundle_registry::verified_embedded_default_host_component_set(
                crate::agents::host_bundle::HostKindV1::CursorDesktop,
                0,
                crate::agents::TEST_GENERATOR_COMMIT,
            )
            .expect("Cursor has a compiled default set");
        assert_eq!(
            cursor.registration_mode(&cursor_set.component_set),
            CatalogRegistrationMode::ArtifactOnly
        );
    }

    /// The live reinstall journey: TraceDecay's own staging residue (a
    /// personal marketplace entry with no native activation yet) makes every
    /// Codex component registration read `Repairable`. An install over that
    /// self-owned residue must proceed and re-activate, refusing it as an
    /// ownership conflict made `tracedecay install --agent codex` fail on
    /// every reinstall/update of TraceDecay's own prior install.
    #[test]
    fn install_preflight_converges_over_own_repairable_registration() {
        use crate::agents::host_bundle::HostComponentSetRegistrationV1;

        let home = tempfile::tempdir().unwrap();
        let marketplace = home.path().join(".agents/plugins/marketplace.json");
        std::fs::create_dir_all(marketplace.parent().unwrap()).unwrap();
        std::fs::write(
            &marketplace,
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "personal",
                "plugins": [{
                    "name": "tracedecay",
                    "source": {"source": "local", "path": "./.codex/plugins/tracedecay"}
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let component_set =
            crate::agents::host_bundle_registry::verified_embedded_default_host_component_set(
                HostKindV1::Codex,
                0,
                crate::agents::TEST_GENERATOR_COMMIT,
            )
            .expect("Codex has a compiled default set");
        let mut authority = CatalogHostComponentRegistrationAuthority::new(
            &tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
            "codex",
            home.path(),
            crate::agents::host_bundle::HostBundleLifecycleOpV1::Install,
        )
        .expect("catalog registration authority");
        let request = crate::agents::host_bundle::HostComponentSetExecutionRequestV1 {
            lifecycle: crate::agents::host_bundle::HostComponentSetLifecycleRequestV1 {
                operation: crate::agents::host_bundle::HostBundleLifecycleOpV1::Install,
                expected_host: HostKindV1::Codex,
                expected_components: component_set
                    .component_set
                    .components
                    .iter()
                    .map(|component| component.manifest.component)
                    .collect(),
                explicit_confirmation: true,
                hermes_profile_bindings: 0,
                explicit_adoption: false,
            },
            operation_id: [7; 16],
        };
        authority
            .preflight(&component_set.component_set, &request)
            .expect("install over TraceDecay's own repairable registration must proceed");
        assert!(
            authority.should_apply,
            "the converging install must re-activate the host registration"
        );
    }
}
