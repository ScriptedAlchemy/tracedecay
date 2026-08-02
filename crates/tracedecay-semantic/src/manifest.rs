//! Canonical model-artifact manifest (Plan 31).
//!
//! `ModelArtifactManifestV1` pins every member's SHA-256 digest and byte
//! length, the SPDX license, tokenizer/config/instruction digests, dimensions,
//! metric, normalization, pooling, truncation, precision, runtime/build/device
//! constraints, and the complete resource ceiling. These immutable pins are
//! the complete local artifact-integrity contract; no signature or trust-root
//! authority is layered on top.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
pub use tracedecay_domain::{
    EmbeddingDeviceClassV1 as DeviceClassV1, EmbeddingMetricV1 as SemanticMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
    EmbeddingTruncationSideV1 as TruncationSideV1,
};

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

/// Which semantic stage the artifact serves. Configuration selects an
/// installed embedding profile and, independently, an optional
/// reranker profile (Plan 31 "Model and offline lifecycle").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactProfileKindV1 {
    #[serde(alias = "Embedding")]
    Embedding,
    #[serde(alias = "Reranker")]
    Reranker,
}

/// Truncation policy: side and maximum token length, both part of the
/// projection identity (changing either creates a new projection generation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TruncationPolicyV1 {
    pub side: TruncationSideV1,
    pub max_length: u32,
}

/// Digest + byte length pin for the primary model member. The complete package
/// identity is carried by [`ArtifactPackageMemberV1`] entries in the manifest.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMemberPinV1 {
    pub digest: Sha256DigestHex,
    pub byte_length: u64,
}

/// Stable role for one package member. Each role has at most one member; the
/// role and portable package path are both part of the manifest identity and
/// are never used as a local filesystem path.
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

/// Complete immutable identity for one artifact package member.
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
    /// Runtime family name, e.g. `fastembed-ort` (value chosen during SEMANTIC).
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

/// Canonical manifest payload. Its compact canonical JSON bytes are the
/// manifest identity verified before import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelArtifactManifestPayloadV1 {
    pub schema: String,
    pub artifact_id: String,
    pub profile_kind: ArtifactProfileKindV1,
    /// SPDX license expression for the model weights.
    pub spdx_license: String,
    pub model_member: ArtifactMemberPinV1,
    pub tokenizer_digest: Sha256DigestHex,
    pub config_digest: Sha256DigestHex,
    pub query_instruction_digest: Option<Sha256DigestHex>,
    pub document_instruction_digest: Option<Sha256DigestHex>,
    /// Every imported byte-bearing member, including its role, package path,
    /// digest, and exact length. This is the importer's source of truth.
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

/// The frozen V1 model artifact manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelArtifactManifestV1 {
    pub payload: ModelArtifactManifestPayloadV1,
}

/// Structural manifest validation failures.
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
    /// Canonical bytes of the complete immutable manifest. serde emits struct
    /// fields in declaration order and the manifest contains no maps, so this
    /// encoding is byte-stable across processes and platforms.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_else(|_| panic!("manifest serialization is infallible"))
    }

    /// SHA-256 over `canonical_bytes`. Stable manifest and artifact identity.
    pub fn canonical_digest(&self) -> Sha256DigestHex {
        Sha256DigestHex::of_bytes(&self.canonical_bytes())
    }

    /// Identity used by artifact storage and receipts.
    pub fn artifact_identity_digest(&self) -> Sha256DigestHex {
        self.canonical_digest()
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
        if manifest.to_canonical_bytes() != bytes {
            return Err(ManifestValidationErrorV1::NonCanonicalEncoding(
                "input bytes differ from the canonical manifest encoding".to_string(),
            ));
        }
        Ok(manifest)
    }

    /// Serialize to canonical JSON bytes (round-trips through `parse`).
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        self.canonical_bytes()
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
            ("spdx_license", p.spdx_license.as_str()),
            ("runtime.runtime", p.runtime.runtime.as_str()),
            ("runtime.build_revision", p.runtime.build_revision.as_str()),
            ("upstream.name", p.upstream.name.as_str()),
            ("upstream.version", p.upstream.version.as_str()),
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

    fn sample_payload() -> ModelArtifactManifestPayloadV1 {
        ModelArtifactManifestPayloadV1 {
            schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_string(),
            artifact_id: "bge-small-en-v1.5".to_string(),
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
        }
    }

    #[test]
    fn manifest_uses_domain_embedding_vocabulary_directly() {
        let payload = sample_payload();

        let _: tracedecay_domain::EmbeddingMetricV1 = payload.metric;
        let _: tracedecay_domain::EmbeddingNormalizationV1 = payload.normalization;
        let _: tracedecay_domain::EmbeddingPoolingV1 = payload.pooling;
        let _: tracedecay_domain::EmbeddingTruncationSideV1 = payload.truncation.side;
        let _: tracedecay_domain::EmbeddingPrecisionV1 = payload.precision;
        let _: tracedecay_domain::EmbeddingDeviceClassV1 = payload.device;
    }

    #[test]
    fn manifest_round_trip_preserves_every_field() {
        let manifest = sample_manifest();
        let bytes = manifest.to_canonical_bytes();
        let parsed = ModelArtifactManifestV1::parse(&bytes).unwrap();
        assert_eq!(manifest, parsed);
    }

    #[test]
    fn canonical_bytes_and_digest_are_stable_across_reserialization() {
        let manifest = sample_manifest();
        let first = manifest.canonical_bytes();
        let reparsed = ModelArtifactManifestV1::parse(&manifest.to_canonical_bytes()).unwrap();
        let second = reparsed.canonical_bytes();
        assert_eq!(first, second);
        assert_eq!(manifest.canonical_digest(), reparsed.canonical_digest());
        assert_eq!(
            manifest.canonical_digest(),
            Sha256DigestHex::of_bytes(&first)
        );
    }

    #[test]
    fn canonical_digest_changes_with_any_manifest_field_change() {
        let base = sample_manifest();
        let mut changed = base.clone();
        changed.payload.dimensions = 768;
        assert_ne!(base.canonical_digest(), changed.canonical_digest());

        let mut changed_license = base.clone();
        changed_license.payload.spdx_license = "Apache-2.0".to_string();
        assert_ne!(base.canonical_digest(), changed_license.canonical_digest());
    }

    #[test]
    fn manifest_binds_complete_named_package_members() {
        let manifest = sample_manifest();
        let payload = serde_json::to_value(&manifest.payload).unwrap();
        let members = payload
            .get("members")
            .and_then(serde_json::Value::as_array)
            .expect("manifest must declare every package member");

        assert_eq!(members.len(), 4);
        for member in members {
            let member = member.as_object().unwrap();
            assert!(member.contains_key("role"));
            assert!(member.contains_key("path"));
            assert!(member.contains_key("digest"));
            assert!(member.contains_key("byte_length"));
        }
    }

    #[test]
    fn validate_rejects_unsafe_or_inconsistent_package_bindings() {
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

        let mut manifest = sample_manifest();
        manifest.payload.tokenizer_digest = Sha256DigestHex::new(hex::encode([0xab; 32])).unwrap();
        let text = String::from_utf8(manifest.to_canonical_bytes()).unwrap();
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
        let canonical = sample_manifest().to_canonical_bytes();
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
