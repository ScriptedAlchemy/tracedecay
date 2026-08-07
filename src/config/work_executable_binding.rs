//! Fail-closed resolution of digest-pinned provider executables.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::configuration::{
    ConfigurationRevisionId, ConfigurationSnapshotId, ConfigurationValueV1, SettingKey,
    WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1, WorkExecutableCapabilityV1,
};
use tracedecay_domain::{
    ManifestDigest, WorkExecutableReference, WorkProviderBackendV1, WorkProviderProtocol,
};

use super::PinnedRuntimeConfiguration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedWorkExecutableBinding {
    canonical_path: PathBuf,
    executable: WorkExecutableReference,
    backend: WorkProviderBackendV1,
    protocol: WorkProviderProtocol,
    capability: WorkExecutableCapabilityV1,
    configuration_revision_id: ConfigurationRevisionId,
    configuration_snapshot_id: ConfigurationSnapshotId,
    verified_byte_length: u64,
}

impl ResolvedWorkExecutableBinding {
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) fn executable(&self) -> &WorkExecutableReference {
        &self.executable
    }

    pub(crate) fn configuration_revision_id(&self) -> &ConfigurationRevisionId {
        &self.configuration_revision_id
    }

    pub(crate) fn configuration_snapshot_id(&self) -> &ConfigurationSnapshotId {
        &self.configuration_snapshot_id
    }

    pub(crate) const fn verified_byte_length(&self) -> u64 {
        self.verified_byte_length
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum WorkExecutableBindingError {
    #[error("no configured executable binding exists for {executable_id}")]
    Absent { executable_id: String },
    #[error("the configured executable binding for {executable_id} is stale")]
    Stale { executable_id: String },
    #[error("the configured executable binding for {executable_id} does not admit this provider")]
    Unsupported { executable_id: String },
    #[error("the executable bytes for {executable_id} do not match the pinned digest")]
    DigestMismatch { executable_id: String },
    #[error("the configured executable binding for {executable_id} is unavailable")]
    Unavailable { executable_id: String },
}

pub(crate) trait WorkExecutableBindingResolver {
    fn resolve(
        &self,
        reference: &WorkExecutableReference,
        backend: WorkProviderBackendV1,
        protocol: WorkProviderProtocol,
    ) -> Result<ResolvedWorkExecutableBinding, WorkExecutableBindingError>;
}

#[derive(Clone, Debug)]
pub(crate) struct PinnedWorkExecutableBindingResolver {
    bindings: BTreeMap<String, WorkExecutableBindingV1>,
    configuration_revision_id: ConfigurationRevisionId,
    configuration_snapshot_id: ConfigurationSnapshotId,
}

impl PinnedWorkExecutableBindingResolver {
    pub(crate) fn from_configuration(
        configuration: &PinnedRuntimeConfiguration,
    ) -> Result<Self, WorkExecutableBindingError> {
        let key = SettingKey::new(WORK_EXECUTABLE_BINDINGS_SETTING_KEY).map_err(|_| {
            WorkExecutableBindingError::Unavailable {
                executable_id: "configuration.work-executable-bindings".to_owned(),
            }
        })?;
        let Some(ConfigurationValueV1::WorkExecutableBindings(configured)) =
            configuration.snapshot.effective_values.get(&key)
        else {
            return Err(WorkExecutableBindingError::Unavailable {
                executable_id: "configuration.work-executable-bindings".to_owned(),
            });
        };
        let bindings = configured
            .iter()
            .cloned()
            .map(|binding| (binding.executable().executable_id().to_owned(), binding))
            .collect();
        Ok(Self {
            bindings,
            configuration_revision_id: configuration.revision_id.clone(),
            configuration_snapshot_id: configuration.snapshot.snapshot_id.clone(),
        })
    }
}

impl WorkExecutableBindingResolver for PinnedWorkExecutableBindingResolver {
    fn resolve(
        &self,
        reference: &WorkExecutableReference,
        backend: WorkProviderBackendV1,
        protocol: WorkProviderProtocol,
    ) -> Result<ResolvedWorkExecutableBinding, WorkExecutableBindingError> {
        let executable_id = reference.executable_id().to_owned();
        let binding = self
            .bindings
            .get(reference.executable_id())
            .ok_or_else(|| WorkExecutableBindingError::Absent {
                executable_id: executable_id.clone(),
            })?;
        if binding.executable() != reference {
            return Err(WorkExecutableBindingError::Stale { executable_id });
        }
        let capability = binding
            .capabilities()
            .iter()
            .copied()
            .find(|capability| capability.admits(backend, protocol))
            .ok_or_else(|| WorkExecutableBindingError::Unsupported {
                executable_id: executable_id.clone(),
            })?;
        let canonical_path = binding.canonical_path().canonicalize().map_err(|_| {
            WorkExecutableBindingError::Unavailable {
                executable_id: executable_id.clone(),
            }
        })?;
        if canonical_path != binding.canonical_path() {
            return Err(WorkExecutableBindingError::Stale { executable_id });
        }
        let (actual_digest, verified_byte_length) =
            digest_file(&canonical_path).map_err(|_| WorkExecutableBindingError::Unavailable {
                executable_id: executable_id.clone(),
            })?;
        if &actual_digest != reference.artifact_digest() {
            return Err(WorkExecutableBindingError::DigestMismatch { executable_id });
        }
        Ok(ResolvedWorkExecutableBinding {
            canonical_path,
            executable: reference.clone(),
            backend,
            protocol,
            capability,
            configuration_revision_id: self.configuration_revision_id.clone(),
            configuration_snapshot_id: self.configuration_snapshot_id.clone(),
            verified_byte_length,
        })
    }
}

fn digest_file(path: &Path) -> std::io::Result<(ManifestDigest, u64)> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || !is_executable(&metadata) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "configured provider executable is not an executable file",
        ));
    }
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("provider executable byte length overflow"))?;
    }
    let digest = ManifestDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
        .map_err(std::io::Error::other)?;
    Ok((digest, bytes))
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tracedecay_domain::configuration::{
        ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1, SettingKey,
        WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1, WorkExecutableCapabilityV1,
    };
    use tracedecay_domain::{
        ManifestDigest, ProjectId, WorkExecutableReference, WorkProviderBackendV1,
        WorkProviderProtocol,
    };

    use crate::config::registry::ConfigurationRegistry;
    use crate::config::resolver::{ConfigurationLayerV1, resolve_configuration};
    use crate::config::{
        PinnedRuntimeConfiguration, RuntimeConfigurationTarget,
        work_executable_binding::{
            PinnedWorkExecutableBindingResolver, WorkExecutableBindingError,
            WorkExecutableBindingResolver,
        },
    };

    fn digest(bytes: &[u8]) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(bytes)))).unwrap()
    }

    fn pinned(root: &Path, bindings: Vec<WorkExecutableBindingV1>) -> PinnedRuntimeConfiguration {
        let project_id = ProjectId::new("project.work-executable-resolver").unwrap();
        let revision_id =
            ConfigurationRevisionId::new("configuration.work-executable-resolver").unwrap();
        let key = SettingKey::new(WORK_EXECUTABLE_BINDINGS_SETTING_KEY).unwrap();
        let resolution = resolve_configuration(
            &ConfigurationRegistry::core().unwrap(),
            &[ConfigurationLayerV1 {
                layer: ConfigurationLayerIdV1::Project {
                    project_id: project_id.clone(),
                },
                revision_id: revision_id.clone(),
                entries: BTreeMap::from([(
                    key,
                    ConfigurationValueV1::WorkExecutableBindings(bindings),
                )]),
            }],
        )
        .unwrap();
        PinnedRuntimeConfiguration::new(
            RuntimeConfigurationTarget {
                project_id,
                project_root: root.to_path_buf(),
            },
            revision_id,
            resolution.snapshot,
        )
        .unwrap()
    }

    #[test]
    fn resolver_requires_exact_capability_digest_and_current_file_bytes() {
        let directory = TempDir::new().unwrap();
        let executable_path = directory.path().join("codex");
        let original = b"#!/bin/sh\nexit 0\n";
        std::fs::write(&executable_path, original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&executable_path).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&executable_path, permissions).unwrap();
        }
        let executable_path = executable_path.canonicalize().unwrap();
        let reference =
            WorkExecutableReference::new("codex.pinned".to_owned(), digest(original)).unwrap();
        let binding = WorkExecutableBindingV1::new(
            reference.clone(),
            executable_path.clone(),
            vec![WorkExecutableCapabilityV1::CodexAppServerJsonRpc],
        )
        .unwrap();
        let configuration = pinned(directory.path(), vec![binding]);
        let resolver =
            PinnedWorkExecutableBindingResolver::from_configuration(&configuration).unwrap();

        let resolved = resolver
            .resolve(
                &reference,
                WorkProviderBackendV1::CodexAppServer,
                WorkProviderProtocol::CodexAppServerJsonRpc,
            )
            .unwrap();
        assert_eq!(resolved.canonical_path(), executable_path);
        assert_eq!(resolved.executable(), &reference);
        assert_eq!(resolved.verified_byte_length(), original.len() as u64);
        assert_eq!(
            resolved.configuration_revision_id(),
            &configuration.revision_id
        );
        assert_eq!(
            resolved.configuration_snapshot_id(),
            &configuration.snapshot.snapshot_id
        );

        assert!(matches!(
            resolver.resolve(
                &reference,
                WorkProviderBackendV1::CodexCli,
                WorkProviderProtocol::CodexExecJson,
            ),
            Err(WorkExecutableBindingError::Unsupported { .. })
        ));
        let stale = WorkExecutableReference::new(
            reference.executable_id().to_owned(),
            digest(b"other pinned bytes"),
        )
        .unwrap();
        assert!(matches!(
            resolver.resolve(
                &stale,
                WorkProviderBackendV1::CodexAppServer,
                WorkProviderProtocol::CodexAppServerJsonRpc,
            ),
            Err(WorkExecutableBindingError::Stale { .. })
        ));

        std::fs::write(&executable_path, b"tampered").unwrap();
        assert!(matches!(
            resolver.resolve(
                &reference,
                WorkProviderBackendV1::CodexAppServer,
                WorkProviderProtocol::CodexAppServerJsonRpc,
            ),
            Err(WorkExecutableBindingError::DigestMismatch { .. })
        ));

        std::fs::remove_file(&executable_path).unwrap();
        assert!(matches!(
            resolver.resolve(
                &reference,
                WorkProviderBackendV1::CodexAppServer,
                WorkProviderProtocol::CodexAppServerJsonRpc,
            ),
            Err(WorkExecutableBindingError::Unavailable { .. })
        ));
    }

    #[test]
    fn resolver_does_not_fall_back_for_an_absent_executable_id() {
        let directory = TempDir::new().unwrap();
        let configuration = pinned(directory.path(), Vec::new());
        let resolver =
            PinnedWorkExecutableBindingResolver::from_configuration(&configuration).unwrap();
        let reference =
            WorkExecutableReference::new("codex.absent".to_owned(), digest(b"absent")).unwrap();

        assert!(matches!(
            resolver.resolve(
                &reference,
                WorkProviderBackendV1::CodexAppServer,
                WorkProviderProtocol::CodexAppServerJsonRpc,
            ),
            Err(WorkExecutableBindingError::Absent { .. })
        ));
    }
}
