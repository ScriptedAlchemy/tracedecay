//! Canonical signed model-artifact manifest (Plan 31, packet
//! `pr10/prep-artifact-manifest`, Model Artifact Contract).
//!
//! `ModelArtifactManifestV1` pins the Ed25519 detached signature and trust-root
//! key ID, the SHA-256 digest and byte length of the model bytes, the SPDX
//! license, tokenizer/config/instruction digests, dimensions, metric,
//! normalization, pooling, truncation side/length, precision, runtime/build/
//! device constraints, and the complete resource ceiling. The detached
//! signature covers the canonical manifest payload bytes
//! (`ModelArtifactManifestV1::canonical_bytes`); trust roots are never fetched
//! from the artifact being verified (Plan 20 configuration identifies the
//! admitted trust-root ID and rotation epoch).
//!
//! QUARANTINE: this module is not reachable from production code yet. It
//! performs no I/O, no network access, and no query/retrieval wiring. It is
//! pure values plus validation, matching the PR9 contract-spine style.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Schema marker pinned into every V1 manifest payload.
pub const MODEL_ARTIFACT_MANIFEST_SCHEMA_V1: &str = "tracedecay.model-artifact-manifest.v1";

/// A lowercase-hex SHA-256 digest (64 chars), validated at construction and at
/// serde deserialization time so malformed digests cannot enter the system.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256DigestHex(String);

impl Sha256DigestHex {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestValidationErrorV1> {
        let value = value.into();
        if value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            Ok(Self(value))
        } else {
            Err(ManifestValidationErrorV1::MalformedHexDigest {
                field: "sha256_digest".to_string(),
            })
        }
    }

    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(hex::encode(hasher.finalize()))
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

impl std::fmt::Display for Sha256DigestHex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A lowercase-hex Ed25519 public key (32 bytes, 64 chars).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Ed25519PublicKeyHex(String);

impl Ed25519PublicKeyHex {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestValidationErrorV1> {
        let value = value.into();
        let valid_len = hex::decode(&value).ok().is_some_and(|b| b.len() == 32);
        if valid_len
            && value
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            Ok(Self(value))
        } else {
            Err(ManifestValidationErrorV1::MalformedHexDigest {
                field: "ed25519_public_key".to_string(),
            })
        }
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        let decoded = hex::decode(&self.0).expect("validated 32-byte hex at construction");
        let mut out = [0u8; 32];
        out.copy_from_slice(&decoded);
        out
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Ed25519PublicKeyHex {
    type Error = ManifestValidationErrorV1;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Ed25519PublicKeyHex> for String {
    fn from(value: Ed25519PublicKeyHex) -> Self {
        value.0
    }
}

/// A lowercase-hex Ed25519 detached signature (64 bytes, 128 chars).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Ed25519SignatureHex(String);

impl Ed25519SignatureHex {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestValidationErrorV1> {
        let value = value.into();
        let valid_len = hex::decode(&value).ok().is_some_and(|b| b.len() == 64);
        if valid_len
            && value
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            Ok(Self(value))
        } else {
            Err(ManifestValidationErrorV1::MalformedHexDigest {
                field: "ed25519_signature".to_string(),
            })
        }
    }

    pub fn to_bytes(&self) -> [u8; 64] {
        let decoded = hex::decode(&self.0).expect("validated 64-byte hex at construction");
        let mut out = [0u8; 64];
        out.copy_from_slice(&decoded);
        out
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Ed25519SignatureHex {
    type Error = ManifestValidationErrorV1;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Ed25519SignatureHex> for String {
    fn from(value: Ed25519SignatureHex) -> Self {
        value.0
    }
}

/// Which semantic stage the artifact serves. Configuration selects an
/// installed signed embedding profile and, independently, an optional
/// reranker profile (Plan 31 "Model and offline lifecycle").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtifactProfileKindV1 {
    Embedding,
    Reranker,
}

/// Signature algorithm for the detached manifest signature. V1 admits only
/// Ed25519; anything else is a typed rejection, never an implicit fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignatureAlgorithmV1 {
    Ed25519,
}

/// Canonical vector distance metric pinned by the projection identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SemanticMetricV1 {
    Cosine,
    DotProduct,
    EuclideanL2,
}

/// Output-vector normalization pinned by the projection identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmbeddingNormalizationV1 {
    L2,
    None,
}

/// Token-to-vector pooling pinned by the projection identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmbeddingPoolingV1 {
    Mean,
    Cls,
    LastToken,
    MeanSqrtLength,
}

/// Numeric precision / quantization of the artifact weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmbeddingPrecisionV1 {
    Fp32,
    Fp16,
    Bf16,
    Int8,
}

/// Deterministic device class. PR10 admits CPU only; accelerator classes are
/// later measured candidate profiles, not defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeviceClassV1 {
    Cpu,
}

/// Truncation policy: side and maximum token length, both part of the
/// projection identity (changing either creates a new projection generation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TruncationPolicyV1 {
    pub side: TruncationSideV1,
    pub max_length: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TruncationSideV1 {
    Left,
    Right,
}

/// Digest + byte length pin for the legacy primary model member. The complete
/// package identity is carried by [`ArtifactPackageMemberV1`] entries in the
/// signed payload.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMemberPinV1 {
    pub digest: Sha256DigestHex,
    pub byte_length: u64,
}

/// Stable role for one signed package member. Each role has at most one
/// member; the role and portable package path are both part of the signed
/// identity and are never used as a local filesystem path.
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

/// Complete signed identity for one artifact package member.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactPackageMemberV1 {
    pub role: ArtifactMemberRoleV1,
    /// Portable package-relative identity, not a destination path.
    pub path: String,
    pub digest: Sha256DigestHex,
    pub byte_length: u64,
}

/// Runtime/build identity the artifact was produced and verified against.
/// Admission requires an exact match against the host runtime evidence; there
/// is no silent substitution or cascade to an unmeasured representation.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCompatibilityV1 {
    /// Runtime family name, e.g. `fastembed-ort` (value chosen during PR10).
    pub runtime: String,
    /// Exact build revision of the runtime the artifact pins.
    pub build_revision: String,
    /// Supported (os, arch) pairs, e.g. ("linux", `x86_64`).
    pub platforms: Vec<PlatformTargetV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformTargetV1 {
    pub os: String,
    pub arch: String,
}

/// Complete resource ceiling pinned by the manifest. Admission verifies the
/// host can honor the ceiling before enabling the semantic stage.
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

/// Upstream provenance metadata. Deliberately scheme-free: import accepts
/// caller-provided bytes or an explicitly configured source as a separate
/// user action; the manifest never carries an implicit fetch address.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamSourceV1 {
    pub name: String,
    pub version: String,
    pub revision: String,
}

/// The signed portion of the manifest. The detached signature covers the
/// canonical bytes of this payload (and only this payload).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSignedPayloadV1 {
    pub schema: String,
    pub artifact_id: String,
    /// Root identity is duplicated in the signed payload so the detached
    /// envelope cannot redirect a valid signature to another root/epoch.
    pub signing_root_id: String,
    pub signing_root_epoch: u32,
    pub profile_kind: ArtifactProfileKindV1,
    /// SPDX license expression for the model weights.
    pub spdx_license: String,
    pub model_member: ArtifactMemberPinV1,
    pub tokenizer_digest: Sha256DigestHex,
    pub config_digest: Sha256DigestHex,
    pub query_instruction_digest: Option<Sha256DigestHex>,
    pub document_instruction_digest: Option<Sha256DigestHex>,
    /// Every imported byte-bearing member, including its role, package path,
    /// digest, and exact length. This list is signed with the rest of the
    /// payload and is the importer's source of truth.
    pub members: Vec<ArtifactPackageMemberV1>,
    pub dimensions: u32,
    pub metric: SemanticMetricV1,
    pub normalization: EmbeddingNormalizationV1,
    pub pooling: EmbeddingPoolingV1,
    pub truncation: TruncationPolicyV1,
    pub precision: EmbeddingPrecisionV1,
    pub runtime: RuntimeCompatibilityV1,
    pub device: DeviceClassV1,
    pub resource_ceiling: ResourceCeilingV1,
    pub upstream: UpstreamSourceV1,
}

/// The detached Ed25519 signature over the canonical payload bytes, plus the
/// trust-root key ID Plan 20 configuration uses to resolve the admitted root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetachedSignatureV1 {
    pub algorithm: SignatureAlgorithmV1,
    pub trust_root_id: String,
    /// The admitted root's rotation epoch. A root ID alone is insufficient:
    /// a rotated key must not authorize a package signed for an older epoch.
    pub trust_root_epoch: u32,
    pub signature: Ed25519SignatureHex,
}

/// The frozen V1 model artifact manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelArtifactManifestV1 {
    pub payload: ManifestSignedPayloadV1,
    pub signature: DetachedSignatureV1,
}

/// Structural manifest validation failures (signature/trust verification
/// lives in `super::trust_roots` / `super::artifact_store`; this type covers
/// only self-contained checks).
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
    #[error("manifest trust-root rotation epoch must be non-zero")]
    ZeroTrustRootEpoch,
    #[error("signed payload and detached envelope trust bindings disagree")]
    InconsistentTrustBinding,
    #[error("manifest is not canonical JSON: {0}")]
    NonCanonicalEncoding(String),
}

impl ModelArtifactManifestV1 {
    /// Canonical bytes the detached signature covers: compact JSON of the
    /// signed payload. serde emits struct fields in declaration order and the
    /// payload contains no maps, so this encoding is byte-stable across
    /// processes and platforms; `canonical_digest` is its SHA-256.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.payload).expect("manifest payload serialization is infallible")
    }

    /// SHA-256 over `canonical_bytes`. Stable identity for signature
    /// verification, import-session identity, and receipts.
    pub fn canonical_digest(&self) -> Sha256DigestHex {
        Sha256DigestHex::of_bytes(&self.canonical_bytes())
    }

    /// Identity of the complete signed envelope. Unlike [`Self::canonical_digest`],
    /// this binds the detached signature and admitted root rotation as well as
    /// every package-member pin.
    pub fn signed_identity_digest(&self) -> Sha256DigestHex {
        Sha256DigestHex::of_bytes(&self.to_canonical_envelope_bytes())
    }

    pub fn package_member(&self, role: ArtifactMemberRoleV1) -> Option<&ArtifactPackageMemberV1> {
        self.payload
            .members
            .iter()
            .find(|member| member.role == role)
    }

    /// Parse a manifest from JSON bytes, then run full structural validation.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestValidationErrorV1> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|e| ManifestValidationErrorV1::NonCanonicalEncoding(e.to_string()))?;
        manifest.validate()?;
        if manifest.to_canonical_envelope_bytes() != bytes {
            return Err(ManifestValidationErrorV1::NonCanonicalEncoding(
                "input bytes differ from the canonical envelope encoding".to_string(),
            ));
        }
        Ok(manifest)
    }

    /// Serialize to canonical JSON bytes (round-trips through `parse`).
    pub fn to_canonical_envelope_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("manifest serialization is infallible")
    }

    /// Structural validation only: schema pin, non-empty identity fields,
    /// well-formed pins (enforced by the digest newtypes at parse time),
    /// non-zero dimensions/truncation/ceilings, ceiling consistency, and at
    /// least one supported platform.
    pub fn validate(&self) -> Result<(), ManifestValidationErrorV1> {
        let p = &self.payload;
        if p.schema != MODEL_ARTIFACT_MANIFEST_SCHEMA_V1 {
            return Err(ManifestValidationErrorV1::UnsupportedSchema(
                p.schema.clone(),
            ));
        }
        for (field, value) in [
            ("artifact_id", p.artifact_id.as_str()),
            ("signing_root_id", p.signing_root_id.as_str()),
            ("spdx_license", p.spdx_license.as_str()),
            ("runtime.runtime", p.runtime.runtime.as_str()),
            ("runtime.build_revision", p.runtime.build_revision.as_str()),
            ("upstream.name", p.upstream.name.as_str()),
            ("upstream.version", p.upstream.version.as_str()),
            ("trust_root_id", self.signature.trust_root_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ManifestValidationErrorV1::EmptyField {
                    field: field.to_string(),
                });
            }
        }
        if p.dimensions == 0 {
            return Err(ManifestValidationErrorV1::ZeroDimensions);
        }
        if p.truncation.max_length == 0 {
            return Err(ManifestValidationErrorV1::ZeroTruncationLength);
        }
        let c = &p.resource_ceiling;
        for (field, value) in [
            ("max_model_bytes", c.max_model_bytes),
            ("max_tokenizer_bytes", c.max_tokenizer_bytes),
            ("max_resident_bytes", c.max_resident_bytes),
            ("max_threads", u64::from(c.max_threads)),
            ("max_batch_size", u64::from(c.max_batch_size)),
            ("max_sequence_length", u64::from(c.max_sequence_length)),
            ("load_deadline_ms", c.load_deadline_ms),
        ] {
            if value == 0 {
                return Err(ManifestValidationErrorV1::ZeroResourceCeiling {
                    field: field.to_string(),
                });
            }
        }
        if c.max_model_bytes < p.model_member.byte_length {
            return Err(ManifestValidationErrorV1::CeilingBelowDeclaredModelBytes);
        }
        if p.runtime.platforms.is_empty() {
            return Err(ManifestValidationErrorV1::NoSupportedPlatforms);
        }
        if self.signature.trust_root_epoch == 0 {
            return Err(ManifestValidationErrorV1::ZeroTrustRootEpoch);
        }
        if p.signing_root_epoch == 0 {
            return Err(ManifestValidationErrorV1::ZeroTrustRootEpoch);
        }
        if p.signing_root_id != self.signature.trust_root_id
            || p.signing_root_epoch != self.signature.trust_root_epoch
        {
            return Err(ManifestValidationErrorV1::InconsistentTrustBinding);
        }

        let mut roles = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        for member in &p.members {
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
        if model.digest != p.model_member.digest
            || model.byte_length != p.model_member.byte_length
            || tokenizer.digest != p.tokenizer_digest
            || config.digest != p.config_digest
        {
            return Err(ManifestValidationErrorV1::InconsistentPackageMembers);
        }
        if c.max_tokenizer_bytes < tokenizer.byte_length {
            return Err(ManifestValidationErrorV1::CeilingBelowDeclaredTokenizerBytes);
        }
        for (role, declared) in [
            (
                ArtifactMemberRoleV1::QueryInstruction,
                p.query_instruction_digest.as_ref(),
            ),
            (
                ArtifactMemberRoleV1::DocumentInstruction,
                p.document_instruction_digest.as_ref(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn digest_of(text: &str) -> Sha256DigestHex {
        Sha256DigestHex::of_bytes(text.as_bytes())
    }

    fn sample_payload() -> ManifestSignedPayloadV1 {
        ManifestSignedPayloadV1 {
            schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_string(),
            artifact_id: "bge-small-en-v1.5".to_string(),
            signing_root_id: "tracedecay-release-2026".to_string(),
            signing_root_epoch: 1,
            profile_kind: ArtifactProfileKindV1::Embedding,
            spdx_license: "MIT".to_string(),
            model_member: ArtifactMemberPinV1 {
                digest: digest_of("model-bytes"),
                byte_length: 133_000_000,
            },
            tokenizer_digest: digest_of("tokenizer"),
            config_digest: digest_of("config"),
            query_instruction_digest: Some(digest_of("query-instruction")),
            document_instruction_digest: None,
            members: vec![
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Model,
                    path: "model.onnx".to_string(),
                    digest: digest_of("model-bytes"),
                    byte_length: 133_000_000,
                },
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Tokenizer,
                    path: "tokenizer.json".to_string(),
                    digest: digest_of("tokenizer"),
                    byte_length: 10_000_000,
                },
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Config,
                    path: "config.json".to_string(),
                    digest: digest_of("config"),
                    byte_length: 2_000,
                },
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::QueryInstruction,
                    path: "instructions/query.txt".to_string(),
                    digest: digest_of("query-instruction"),
                    byte_length: 64,
                },
            ],
            dimensions: 384,
            metric: SemanticMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            pooling: EmbeddingPoolingV1::Mean,
            truncation: TruncationPolicyV1 {
                side: TruncationSideV1::Right,
                max_length: 512,
            },
            precision: EmbeddingPrecisionV1::Fp32,
            runtime: RuntimeCompatibilityV1 {
                runtime: "fastembed-ort".to_string(),
                build_revision: "ort-1.22.0-fastembed-5".to_string(),
                platforms: vec![
                    PlatformTargetV1 {
                        os: "linux".to_string(),
                        arch: "x86_64".to_string(),
                    },
                    PlatformTargetV1 {
                        os: "linux".to_string(),
                        arch: "aarch64".to_string(),
                    },
                ],
            },
            device: DeviceClassV1::Cpu,
            resource_ceiling: ResourceCeilingV1 {
                max_model_bytes: 200_000_000,
                max_tokenizer_bytes: 20_000_000,
                max_resident_bytes: 1_000_000_000,
                max_threads: 4,
                max_batch_size: 32,
                max_sequence_length: 512,
                load_deadline_ms: 30_000,
            },
            upstream: UpstreamSourceV1 {
                name: "BAAI/bge-small-en-v1.5".to_string(),
                version: "1.5".to_string(),
                revision: "onnx-revision-placeholder".to_string(),
            },
        }
    }

    fn sample_manifest() -> ModelArtifactManifestV1 {
        ModelArtifactManifestV1 {
            payload: sample_payload(),
            signature: DetachedSignatureV1 {
                algorithm: SignatureAlgorithmV1::Ed25519,
                trust_root_id: "tracedecay-release-2026".to_string(),
                trust_root_epoch: 1,
                signature: Ed25519SignatureHex::new(hex::encode([7u8; 64])).unwrap(),
            },
        }
    }

    #[test]
    fn manifest_round_trip_preserves_every_field() {
        let manifest = sample_manifest();
        let bytes = manifest.to_canonical_envelope_bytes();
        let parsed = ModelArtifactManifestV1::parse(&bytes).unwrap();
        assert_eq!(manifest, parsed);
    }

    #[test]
    fn canonical_bytes_and_digest_are_stable_across_reserialization() {
        let manifest = sample_manifest();
        let first = manifest.canonical_bytes();
        let reparsed =
            ModelArtifactManifestV1::parse(&manifest.to_canonical_envelope_bytes()).unwrap();
        let second = reparsed.canonical_bytes();
        assert_eq!(first, second);
        assert_eq!(manifest.canonical_digest(), reparsed.canonical_digest());
        assert_eq!(
            manifest.canonical_digest(),
            Sha256DigestHex::of_bytes(&first)
        );
    }

    #[test]
    fn canonical_digest_changes_with_any_signed_field_change() {
        let base = sample_manifest();
        let mut changed = base.clone();
        changed.payload.dimensions = 768;
        assert_ne!(base.canonical_digest(), changed.canonical_digest());

        let mut changed_license = base.clone();
        changed_license.payload.spdx_license = "Apache-2.0".to_string();
        assert_ne!(base.canonical_digest(), changed_license.canonical_digest());

        // Signature bytes are detached: they never enter the signed digest.
        let mut changed_sig = base.clone();
        changed_sig.signature.signature = Ed25519SignatureHex::new(hex::encode([9u8; 64])).unwrap();
        assert_eq!(base.canonical_digest(), changed_sig.canonical_digest());
    }

    #[test]
    fn signed_payload_binds_complete_named_package_members_and_root_epoch() {
        let manifest = sample_manifest();
        let payload = serde_json::to_value(&manifest.payload).unwrap();
        let members = payload
            .get("members")
            .and_then(serde_json::Value::as_array)
            .expect("signed payload must declare every package member");

        assert_eq!(members.len(), 4);
        assert_eq!(
            payload
                .get("signing_root_id")
                .and_then(serde_json::Value::as_str),
            Some("tracedecay-release-2026")
        );
        assert_eq!(
            payload
                .get("signing_root_epoch")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        for member in members {
            let member = member.as_object().unwrap();
            assert!(member.contains_key("role"));
            assert!(member.contains_key("path"));
            assert!(member.contains_key("digest"));
            assert!(member.contains_key("byte_length"));
        }

        let signature = serde_json::to_value(&manifest.signature).unwrap();
        assert_eq!(
            signature
                .get("trust_root_epoch")
                .and_then(serde_json::Value::as_u64),
            Some(1),
            "the signature must bind the admitted trust-root rotation"
        );
    }

    #[test]
    fn validate_rejects_unsafe_or_inconsistent_package_and_trust_bindings() {
        let mut traversal = sample_manifest();
        traversal.payload.members[0].path = "../model.onnx".to_string();
        assert_eq!(
            traversal.validate(),
            Err(ManifestValidationErrorV1::InvalidPackageMember)
        );

        let mut duplicate_path = sample_manifest();
        duplicate_path.payload.members[1].path = duplicate_path.payload.members[0].path.clone();
        assert_eq!(
            duplicate_path.validate(),
            Err(ManifestValidationErrorV1::DuplicatePackageMember)
        );

        let mut rotated_envelope = sample_manifest();
        rotated_envelope.signature.trust_root_epoch = 2;
        assert_eq!(
            rotated_envelope.validate(),
            Err(ManifestValidationErrorV1::InconsistentTrustBinding)
        );
    }

    #[test]
    fn validate_rejects_wrong_schema_zero_dimensions_and_empty_license() {
        let mut bad_schema = sample_manifest();
        bad_schema.payload.schema = "tracedecay.model-artifact-manifest.v0".to_string();
        assert!(matches!(
            bad_schema.validate(),
            Err(ManifestValidationErrorV1::UnsupportedSchema(_))
        ));

        let mut zero_dims = sample_manifest();
        zero_dims.payload.dimensions = 0;
        assert_eq!(
            zero_dims.validate(),
            Err(ManifestValidationErrorV1::ZeroDimensions)
        );

        let mut empty_license = sample_manifest();
        empty_license.payload.spdx_license = "  ".to_string();
        assert_eq!(
            empty_license.validate(),
            Err(ManifestValidationErrorV1::EmptyField {
                field: "spdx_license".to_string()
            })
        );
    }

    #[test]
    fn validate_rejects_zero_ceiling_and_ceiling_below_declared_bytes() {
        let mut zero_ceiling = sample_manifest();
        zero_ceiling.payload.resource_ceiling.max_threads = 0;
        assert_eq!(
            zero_ceiling.validate(),
            Err(ManifestValidationErrorV1::ZeroResourceCeiling {
                field: "max_threads".to_string()
            })
        );

        let mut low_ceiling = sample_manifest();
        low_ceiling.payload.resource_ceiling.max_model_bytes =
            low_ceiling.payload.model_member.byte_length - 1;
        assert_eq!(
            low_ceiling.validate(),
            Err(ManifestValidationErrorV1::CeilingBelowDeclaredModelBytes)
        );
    }

    #[test]
    fn parse_rejects_malformed_hex_and_uppercase_digests() {
        assert!(Sha256DigestHex::new("zz").is_err());
        // 0xab produces hex letters, so uppercasing is a real corruption.
        assert!(Sha256DigestHex::new(hex::encode([0xab; 32]).to_uppercase()).is_err());
        assert!(Ed25519PublicKeyHex::new(hex::encode([1u8; 31])).is_err());
        assert!(Ed25519SignatureHex::new(hex::encode([1u8; 63])).is_err());

        let mut manifest = sample_manifest();
        manifest.payload.tokenizer_digest = Sha256DigestHex::new(hex::encode([0xab; 32])).unwrap();
        let text = String::from_utf8(manifest.to_canonical_envelope_bytes()).unwrap();
        let corrupted = text.replacen(
            &hex::encode([0xab; 32]),
            &hex::encode([0xab; 32]).to_uppercase(),
            1,
        );
        assert!(matches!(
            ModelArtifactManifestV1::parse(corrupted.as_bytes()),
            Err(ManifestValidationErrorV1::MalformedHexDigest { .. }
                | ManifestValidationErrorV1::NonCanonicalEncoding(_))
        ));
    }

    #[test]
    fn parse_rejects_noncanonical_and_unknown_fields() {
        let canonical = sample_manifest().to_canonical_envelope_bytes();
        let mut padded = b" ".to_vec();
        padded.extend_from_slice(&canonical);
        assert!(matches!(
            ModelArtifactManifestV1::parse(&padded),
            Err(ManifestValidationErrorV1::NonCanonicalEncoding(_))
        ));

        let mut value: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unsigned_extension".to_string(), serde_json::json!(true));
        let with_unknown = serde_json::to_vec(&value).unwrap();
        assert!(ModelArtifactManifestV1::parse(&with_unknown).is_err());

        let mut nested: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        nested["payload"]["runtime"]
            .as_object_mut()
            .unwrap()
            .insert("ambient_cache".to_string(), serde_json::json!(true));
        let with_nested_unknown = serde_json::to_vec(&nested).unwrap();
        assert!(ModelArtifactManifestV1::parse(&with_nested_unknown).is_err());
    }
}
