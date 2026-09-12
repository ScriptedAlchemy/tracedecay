use std::path::{Component, Path};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::HostKindV1;
use tracedecay_domain::canonical_json_bytes;

use crate::HostBundleError;

pub const HOST_BUNDLE_SCHEMA_VERSION: u16 = 1;
pub const MAX_MANIFEST_ARTIFACTS: usize = 128;
pub const MAX_HOST_COMPONENTS: usize = 4;
pub const MAX_RELATIVE_PATH_BYTES: usize = 512;
pub const MAX_IDENTIFIER_BYTES: usize = 128;
/// Per-artifact byte cap for compiled first-party host-bundle contents.
/// Sized to admit the Cursor desktop native-diagnostics extension
/// (`plugin/cursor-native-extension/embedded/extension.js`).
pub const MAX_ARTIFACT_CONTENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleComponentV1 {
    Core,
    Agent,
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
/// One generated artifact. Contents and credentials never enter the manifest;
/// the content digest identifies bytes compiled into the first-party catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleArtifactV1 {
    pub relative_path: String,
    pub artifact_digest: [u8; 32],
    pub ownership_marker: String,
}

/// Generated first-party projection for one host/component. It references the
/// one integration/catalog authority and duplicates no workflow semantics.
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
    pub artifacts: Vec<HostBundleArtifactV1>,
}

impl HostBundleManifestV1 {
    #[hotpath::measure(label = "host_integration.manifest.validate")]
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

    /// Canonical first-party catalog bytes used for content identity.
    #[hotpath::measure(label = "host_integration.manifest.canonicalize")]
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, HostBundleError> {
        canonical_json_bytes(&HostBundleCatalogPayloadV1 {
            schema_version: self.schema_version,
            host: self.host,
            component: self.component,
            integration_manifest_digest: self.integration_manifest_digest,
            catalog_digest: self.catalog_digest,
            configuration_snapshot_id: &self.configuration_snapshot_id,
            effective_behavior_digest: self.effective_behavior_digest,
            resolution_provenance_digest: self.resolution_provenance_digest,
            protocol_min: self.protocol_min,
            protocol_max: self.protocol_max,
            artifacts: &self.artifacts,
        })
        .map_err(|_| HostBundleError::CanonicalizationFailed)
    }

    #[hotpath::measure(label = "host_integration.manifest.digest")]
    pub fn canonical_digest(&self) -> Result<[u8; 32], HostBundleError> {
        Ok(Sha256::digest(self.canonical_bytes()?).into())
    }
}

#[derive(Serialize)]
struct HostBundleCatalogPayloadV1<'a> {
    schema_version: u16,
    host: HostKindV1,
    component: HostBundleComponentV1,
    integration_manifest_digest: [u8; 32],
    catalog_digest: [u8; 32],
    configuration_snapshot_id: &'a str,
    effective_behavior_digest: [u8; 32],
    resolution_provenance_digest: [u8; 32],
    protocol_min: u16,
    protocol_max: u16,
    artifacts: &'a [HostBundleArtifactV1],
}

/// First-party catalog identity verifier.
pub trait HostBundleVerificationAdapterV1 {
    fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError>;
}

pub fn validate_identifier(value: &str) -> Result<(), HostBundleError> {
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
/// Bytes obtained from the verified embedded host bundle. They are checked
/// against the cataloged artifact digest before any host path is touched and
/// are never copied into receipts or journals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBundleArtifactContentV1 {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}
