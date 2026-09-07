use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::canonical_text::{is_lowercase_hex, sha256_hex};
use tracedecay_domain::{
    EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1,
    EmbeddingPrecisionV1, EmbeddingTruncationSideV1,
};

pub const MODEL_ARTIFACT_MANIFEST_SCHEMA_V1: &str = "tracedecay.model-artifact-manifest.v1";

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256DigestHex(String);

impl Sha256DigestHex {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestValidationErrorV1> {
        let value = value.into();
        if is_lowercase_hex(&value, 64) {
            Ok(Self(value))
        } else {
            Err(ManifestValidationErrorV1::MalformedHexDigest {
                field: "sha256_digest".to_owned(),
            })
        }
    }

    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(sha256_hex(bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Sha256DigestHex {
    type Error = ManifestValidationErrorV1;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Sha256DigestHex> for String {
    fn from(value: Sha256DigestHex) -> Self {
        value.0
    }
}

impl fmt::Display for Sha256DigestHex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactProfileKindV1 {
    #[serde(alias = "Embedding")]
    Embedding,
    #[serde(alias = "Reranker")]
    Reranker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TruncationPolicyV1 {
    pub side: EmbeddingTruncationSideV1,
    pub max_length: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMemberPinV1 {
    pub digest: Sha256DigestHex,
    pub byte_length: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactMemberRoleV1 {
    Model,
    Tokenizer,
    Config,
    SpecialTokensMap,
    TokenizerConfig,
    QueryInstruction,
    DocumentInstruction,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactPackageMemberV1 {
    pub role: ArtifactMemberRoleV1,
    pub path: String,
    pub digest: Sha256DigestHex,
    pub byte_length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCompatibilityV1 {
    pub runtime: String,
    pub build_revision: String,
    pub platforms: Vec<PlatformTargetV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformTargetV1 {
    pub os: String,
    pub arch: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCeilingV1 {
    pub max_model_bytes: u64,
    pub max_tokenizer_bytes: u64,
    pub max_resident_bytes: u64,
    pub max_threads: u32,
    pub max_batch_size: u32,
    pub max_sequence_length: u32,
    pub load_deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamSourceV1 {
    pub name: String,
    pub version: String,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelArtifactManifestPayloadV1 {
    pub schema: String,
    pub artifact_id: String,
    pub profile_kind: ArtifactProfileKindV1,
    pub spdx_license: String,
    pub model_member: ArtifactMemberPinV1,
    pub tokenizer_digest: Sha256DigestHex,
    pub config_digest: Sha256DigestHex,
    pub query_instruction_digest: Option<Sha256DigestHex>,
    pub document_instruction_digest: Option<Sha256DigestHex>,
    pub members: Vec<ArtifactPackageMemberV1>,
    pub dimensions: u32,
    pub metric: EmbeddingMetricV1,
    pub normalization: EmbeddingNormalizationV1,
    pub pooling: EmbeddingPoolingV1,
    pub truncation: TruncationPolicyV1,
    pub precision: EmbeddingPrecisionV1,
    pub runtime: RuntimeCompatibilityV1,
    pub device: EmbeddingDeviceClassV1,
    pub resource_ceiling: ResourceCeilingV1,
    pub upstream: UpstreamSourceV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelArtifactManifestV1 {
    pub payload: ModelArtifactManifestPayloadV1,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ManifestValidationErrorV1 {
    #[error("unsupported manifest schema: {0}")]
    UnsupportedSchema(String),
    #[error("malformed lowercase-hex field: {field}")]
    MalformedHexDigest { field: String },
    #[error("empty required field: {field}")]
    EmptyField { field: String },
    #[error("dimensions must be non-zero")]
    ZeroDimensions,
    #[error("truncation max_length must be non-zero")]
    ZeroTruncationLength,
    #[error("resource ceiling field must be non-zero: {field}")]
    ZeroResourceCeiling { field: String },
    #[error("resource ceiling max_model_bytes below declared model byte length")]
    CeilingBelowDeclaredModelBytes,
    #[error("resource ceiling max_tokenizer_bytes below declared tokenizer byte length")]
    CeilingBelowDeclaredTokenizerBytes,
    #[error("manifest declares no supported platforms")]
    NoSupportedPlatforms,
    #[error("manifest has no complete package member list")]
    MissingPackageMembers,
    #[error("manifest package member identity is invalid")]
    InvalidPackageMember,
    #[error("manifest package member identity is duplicated")]
    DuplicatePackageMember,
    #[error("manifest package member identity is incomplete or inconsistent")]
    InconsistentPackageMembers,
    #[error("manifest is not canonical JSON: {0}")]
    NonCanonicalEncoding(String),
}

impl ModelArtifactManifestV1 {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ManifestValidationErrorV1> {
        serde_json::to_vec(self)
            .map_err(|error| ManifestValidationErrorV1::NonCanonicalEncoding(error.to_string()))
    }

    pub fn canonical_digest(&self) -> Sha256DigestHex {
        match self.canonical_bytes() {
            Ok(bytes) => Sha256DigestHex::of_bytes(&bytes),
            Err(error) => {
                unreachable!("model artifact manifest serialization failed: {error}")
            }
        }
    }

    pub fn artifact_identity_digest(&self) -> Sha256DigestHex {
        self.canonical_digest()
    }

    pub fn package_member(&self, role: ArtifactMemberRoleV1) -> Option<&ArtifactPackageMemberV1> {
        self.payload
            .members
            .iter()
            .find(|member| member.role == role)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestValidationErrorV1> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| ManifestValidationErrorV1::NonCanonicalEncoding(error.to_string()))?;
        manifest.validate()?;
        if manifest.to_canonical_bytes()? != bytes {
            return Err(ManifestValidationErrorV1::NonCanonicalEncoding(
                "input bytes differ from the canonical manifest encoding".to_owned(),
            ));
        }
        Ok(manifest)
    }

    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, ManifestValidationErrorV1> {
        self.canonical_bytes()
    }

    pub fn validate(&self) -> Result<(), ManifestValidationErrorV1> {
        let payload = &self.payload;
        if payload.schema != MODEL_ARTIFACT_MANIFEST_SCHEMA_V1 {
            return Err(ManifestValidationErrorV1::UnsupportedSchema(
                payload.schema.clone(),
            ));
        }
        for (field, value) in [
            ("artifact_id", payload.artifact_id.as_str()),
            ("spdx_license", payload.spdx_license.as_str()),
            ("runtime.runtime", payload.runtime.runtime.as_str()),
            (
                "runtime.build_revision",
                payload.runtime.build_revision.as_str(),
            ),
            ("upstream.name", payload.upstream.name.as_str()),
            ("upstream.version", payload.upstream.version.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ManifestValidationErrorV1::EmptyField {
                    field: field.to_owned(),
                });
            }
        }
        if payload.dimensions == 0 {
            return Err(ManifestValidationErrorV1::ZeroDimensions);
        }
        if payload.truncation.max_length == 0 {
            return Err(ManifestValidationErrorV1::ZeroTruncationLength);
        }
        let ceiling = &payload.resource_ceiling;
        for (field, value) in [
            ("max_model_bytes", ceiling.max_model_bytes),
            ("max_tokenizer_bytes", ceiling.max_tokenizer_bytes),
            ("max_resident_bytes", ceiling.max_resident_bytes),
            ("max_threads", u64::from(ceiling.max_threads)),
            ("max_batch_size", u64::from(ceiling.max_batch_size)),
            (
                "max_sequence_length",
                u64::from(ceiling.max_sequence_length),
            ),
            ("load_deadline_ms", ceiling.load_deadline_ms),
        ] {
            if value == 0 {
                return Err(ManifestValidationErrorV1::ZeroResourceCeiling {
                    field: field.to_owned(),
                });
            }
        }
        if ceiling.max_model_bytes < payload.model_member.byte_length {
            return Err(ManifestValidationErrorV1::CeilingBelowDeclaredModelBytes);
        }
        if payload.runtime.platforms.is_empty() {
            return Err(ManifestValidationErrorV1::NoSupportedPlatforms);
        }
        let mut roles = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for member in &payload.members {
            if member.byte_length == 0 || !is_portable_member_path(&member.path) {
                return Err(ManifestValidationErrorV1::InvalidPackageMember);
            }
            if !roles.insert(member.role) || !paths.insert(&member.path) {
                return Err(ManifestValidationErrorV1::DuplicatePackageMember);
            }
        }

        let model = self
            .package_member(ArtifactMemberRoleV1::Model)
            .ok_or(ManifestValidationErrorV1::MissingPackageMembers)?;
        let tokenizer = self
            .package_member(ArtifactMemberRoleV1::Tokenizer)
            .ok_or(ManifestValidationErrorV1::MissingPackageMembers)?;
        let config = self
            .package_member(ArtifactMemberRoleV1::Config)
            .ok_or(ManifestValidationErrorV1::MissingPackageMembers)?;
        if model.digest != payload.model_member.digest
            || model.byte_length != payload.model_member.byte_length
            || tokenizer.digest != payload.tokenizer_digest
            || config.digest != payload.config_digest
        {
            return Err(ManifestValidationErrorV1::InconsistentPackageMembers);
        }
        if ceiling.max_tokenizer_bytes < tokenizer.byte_length {
            return Err(ManifestValidationErrorV1::CeilingBelowDeclaredTokenizerBytes);
        }
        for (role, declared) in [
            (
                ArtifactMemberRoleV1::QueryInstruction,
                payload.query_instruction_digest.as_ref(),
            ),
            (
                ArtifactMemberRoleV1::DocumentInstruction,
                payload.document_instruction_digest.as_ref(),
            ),
        ] {
            match (declared, self.package_member(role)) {
                (Some(declared), Some(member)) if member.digest == *declared => {}
                (None, None) => {}
                _ => return Err(ManifestValidationErrorV1::InconsistentPackageMembers),
            }
        }
        Ok(())
    }
}

fn is_portable_member_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}
