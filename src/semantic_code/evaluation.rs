//! Quarantined PR10 semantic benchmark artifact schemas.
//!
//! This preparation packet defines workload, result, and evidence-anchor
//! wire formats plus validation. It performs no I/O, executes no benchmark,
//! reads no fixture or label data, makes no quality decision, and has no
//! activation or promotion authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::Digest as _;
use thiserror::Error;

pub const SEMANTIC_BENCHMARK_WORKLOAD_SCHEMA_VERSION: u32 = 1;
pub const SEMANTIC_BENCHMARK_RESULT_SCHEMA_VERSION: u32 = 1;

pub const QUERY_WARMUP_REPETITIONS: u32 = 10;
pub const QUERY_MEASURED_REPETITIONS: u32 = 1_000;
pub const PROJECTION_WARMUP_REPETITIONS: u32 = 5;
pub const PROJECTION_MEASURED_REPETITIONS: u32 = 30;

pub const SEMANTIC_BENCHMARK_WORKLOAD_DIGEST_DOMAIN: &str =
    "tracedecay.semantic-benchmark.workload.v1";
pub const SEMANTIC_BENCHMARK_RESULT_DIGEST_DOMAIN: &str = "tracedecay.semantic-benchmark.result.v1";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticBenchmarkSchemaError {
    #[error("invalid identity for {field}")]
    InvalidIdentity { field: &'static str },
    #[error("invalid SHA-256 digest")]
    InvalidDigest,
    #[error("invalid repository-relative path")]
    InvalidPath,
    #[error("unsupported {schema} schema version {actual}")]
    UnsupportedSchemaVersion { schema: &'static str, actual: u32 },
    #[error("invalid evidence anchor: {field}")]
    InvalidEvidenceAnchor { field: &'static str },
    #[error("invalid workload: {field}")]
    InvalidWorkload { field: &'static str },
    #[error("invalid result: {field}")]
    InvalidResult { field: &'static str },
    #[error("duplicate evidence anchor {field}")]
    DuplicateEvidenceAnchor { field: &'static str },
    #[error("workload digest mismatch")]
    WorkloadDigestMismatch,
    #[error("result digest mismatch")]
    ResultDigestMismatch,
    #[error("result does not match workload: {field}")]
    WorkloadResultMismatch { field: &'static str },
    #[error("10x workload does not match current workload: {field}")]
    TenXWorkloadMismatch { field: &'static str },
    #[error("canonical serialization failed")]
    CanonicalSerialization,
}

pub type SchemaResult<T> = Result<T, SemanticBenchmarkSchemaError>;

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

macro_rules! benchmark_string_id {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> SchemaResult<Self> {
                let value = value.into();
                if !valid_identity(&value) {
                    return Err(SemanticBenchmarkSchemaError::InvalidIdentity {
                        field: stringify!($name),
                    });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

benchmark_string_id!(
    SemanticBenchmarkWorkloadIdV1,
    SemanticBenchmarkResultIdV1,
    WorkloadGroupIdV1,
    EvidenceAnchorIdV1,
    ComponentRevisionIdV1,
    HardwareClassIdV1,
);

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct Sha256DigestV1(String);

impl Sha256DigestV1 {
    pub fn new(value: impl Into<String>) -> SchemaResult<Self> {
        let value = value.into();
        let valid = value.strip_prefix("sha256:").is_some_and(|encoded| {
            encoded.len() == 64
                && encoded
                    .chars()
                    .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
        });
        if !valid {
            return Err(SemanticBenchmarkSchemaError::InvalidDigest);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Sha256DigestV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Sha256DigestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct GitCommitShaV1(String);

impl GitCommitShaV1 {
    pub fn new(value: impl Into<String>) -> SchemaResult<Self> {
        let value = value.into();
        if value.len() != 40
            || !value
                .chars()
                .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
        {
            return Err(SemanticBenchmarkSchemaError::InvalidIdentity {
                field: "GitCommitShaV1",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for GitCommitShaV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct RepositoryRelativePathV1(String);

impl RepositoryRelativePathV1 {
    pub fn new(value: impl Into<String>) -> SchemaResult<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 1_024
            && value.trim() == value
            && !value.starts_with('/')
            && !value.contains('\\')
            && !value.chars().any(char::is_control)
            && value.split('/').all(|component| {
                !component.is_empty()
                    && component != "."
                    && component != ".."
                    && !component.contains(':')
            });
        if !valid {
            return Err(SemanticBenchmarkSchemaError::InvalidPath);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RepositoryRelativePathV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkEvidenceRoleV1 {
    WorkloadManifest,
    CorpusDescriptor,
    QuerySet,
    HardwareManifest,
    RuntimeManifest,
    RawSamples,
    CandidateList,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticBenchmarkEvidenceAnchorV1 {
    pub anchor_id: EvidenceAnchorIdV1,
    pub role: BenchmarkEvidenceRoleV1,
    pub artifact_path: RepositoryRelativePathV1,
    pub artifact_digest: Sha256DigestV1,
    pub byte_len: u64,
}

impl SemanticBenchmarkEvidenceAnchorV1 {
    pub fn validate(&self) -> SchemaResult<()> {
        if self.byte_len == 0 {
            return Err(SemanticBenchmarkSchemaError::InvalidEvidenceAnchor { field: "byte_len" });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkKindV1 {
    Projection,
    Query,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkScaleV1 {
    Current,
    TenX,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkCacheStateV1 {
    Cold,
    Warm,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticBenchmarkRevisionSetV1 {
    pub model: ComponentRevisionIdV1,
    pub projection: ComponentRevisionIdV1,
    pub fusion: ComponentRevisionIdV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticBenchmarkWorkloadV1 {
    pub schema_version: u32,
    pub workload_id: SemanticBenchmarkWorkloadIdV1,
    pub workload_group_id: WorkloadGroupIdV1,
    pub kind: BenchmarkKindV1,
    pub scale: BenchmarkScaleV1,
    pub corpus_anchor: SemanticBenchmarkEvidenceAnchorV1,
    pub query_set_anchor: Option<SemanticBenchmarkEvidenceAnchorV1>,
    pub file_count: u64,
    pub eligible_chunk_count: u64,
    pub query_count: u64,
    pub language_source_strata: BTreeMap<String, u64>,
    pub seed: u64,
    pub revisions: SemanticBenchmarkRevisionSetV1,
    pub hardware_class: HardwareClassIdV1,
    pub hardware_manifest_anchor: SemanticBenchmarkEvidenceAnchorV1,
    pub runtime_manifest_anchor: SemanticBenchmarkEvidenceAnchorV1,
    pub cache_state: BenchmarkCacheStateV1,
    pub concurrency: u32,
    pub warmup_repetitions: u32,
    pub measured_repetitions: u32,
    pub digest: Sha256DigestV1,
}

#[derive(Serialize)]
struct WorkloadDigestInput<'a> {
    schema_version: u32,
    workload_id: &'a SemanticBenchmarkWorkloadIdV1,
    workload_group_id: &'a WorkloadGroupIdV1,
    kind: BenchmarkKindV1,
    scale: BenchmarkScaleV1,
    corpus_anchor: &'a SemanticBenchmarkEvidenceAnchorV1,
    query_set_anchor: &'a Option<SemanticBenchmarkEvidenceAnchorV1>,
    file_count: u64,
    eligible_chunk_count: u64,
    query_count: u64,
    language_source_strata: &'a BTreeMap<String, u64>,
    seed: u64,
    revisions: &'a SemanticBenchmarkRevisionSetV1,
    hardware_class: &'a HardwareClassIdV1,
    hardware_manifest_anchor: &'a SemanticBenchmarkEvidenceAnchorV1,
    runtime_manifest_anchor: &'a SemanticBenchmarkEvidenceAnchorV1,
    cache_state: BenchmarkCacheStateV1,
    concurrency: u32,
    warmup_repetitions: u32,
    measured_repetitions: u32,
}

impl SemanticBenchmarkWorkloadV1 {
    pub fn compute_digest(&self) -> SchemaResult<Sha256DigestV1> {
        canonical_sha256(
            SEMANTIC_BENCHMARK_WORKLOAD_DIGEST_DOMAIN,
            &WorkloadDigestInput {
                schema_version: self.schema_version,
                workload_id: &self.workload_id,
                workload_group_id: &self.workload_group_id,
                kind: self.kind,
                scale: self.scale,
                corpus_anchor: &self.corpus_anchor,
                query_set_anchor: &self.query_set_anchor,
                file_count: self.file_count,
                eligible_chunk_count: self.eligible_chunk_count,
                query_count: self.query_count,
                language_source_strata: &self.language_source_strata,
                seed: self.seed,
                revisions: &self.revisions,
                hardware_class: &self.hardware_class,
                hardware_manifest_anchor: &self.hardware_manifest_anchor,
                runtime_manifest_anchor: &self.runtime_manifest_anchor,
                cache_state: self.cache_state,
                concurrency: self.concurrency,
                warmup_repetitions: self.warmup_repetitions,
                measured_repetitions: self.measured_repetitions,
            },
        )
    }

    pub fn validate(&self) -> SchemaResult<()> {
        if self.schema_version != SEMANTIC_BENCHMARK_WORKLOAD_SCHEMA_VERSION {
            return Err(SemanticBenchmarkSchemaError::UnsupportedSchemaVersion {
                schema: "workload",
                actual: self.schema_version,
            });
        }
        if self.file_count == 0 || self.eligible_chunk_count == 0 || self.concurrency == 0 {
            return Err(SemanticBenchmarkSchemaError::InvalidWorkload { field: "counts" });
        }
        validate_anchor_role(
            &self.corpus_anchor,
            BenchmarkEvidenceRoleV1::CorpusDescriptor,
            "corpus_anchor",
        )?;
        validate_anchor_role(
            &self.hardware_manifest_anchor,
            BenchmarkEvidenceRoleV1::HardwareManifest,
            "hardware_manifest_anchor",
        )?;
        validate_anchor_role(
            &self.runtime_manifest_anchor,
            BenchmarkEvidenceRoleV1::RuntimeManifest,
            "runtime_manifest_anchor",
        )?;

        let mut anchors = vec![
            &self.corpus_anchor,
            &self.hardware_manifest_anchor,
            &self.runtime_manifest_anchor,
        ];
        if let Some(query_set_anchor) = &self.query_set_anchor {
            validate_anchor_role(
                query_set_anchor,
                BenchmarkEvidenceRoleV1::QuerySet,
                "query_set_anchor",
            )?;
            anchors.push(query_set_anchor);
        }
        validate_unique_anchors(anchors)?;

        match self.kind {
            BenchmarkKindV1::Projection => self.validate_projection_contract()?,
            BenchmarkKindV1::Query => self.validate_query_contract()?,
        }

        if self.compute_digest()? != self.digest {
            return Err(SemanticBenchmarkSchemaError::WorkloadDigestMismatch);
        }
        Ok(())
    }

    pub fn validate_ten_x_against(&self, current: &Self) -> SchemaResult<()> {
        current.validate()?;
        self.validate()?;
        if current.scale != BenchmarkScaleV1::Current || self.scale != BenchmarkScaleV1::TenX {
            return Err(SemanticBenchmarkSchemaError::TenXWorkloadMismatch { field: "scale" });
        }
        let expected_chunks = current.eligible_chunk_count.checked_mul(10).ok_or(
            SemanticBenchmarkSchemaError::TenXWorkloadMismatch {
                field: "eligible_chunk_count",
            },
        )?;
        if self.eligible_chunk_count != expected_chunks {
            return Err(SemanticBenchmarkSchemaError::TenXWorkloadMismatch {
                field: "eligible_chunk_count",
            });
        }
        if self.workload_group_id != current.workload_group_id
            || self.kind != current.kind
            || self.query_set_anchor != current.query_set_anchor
            || self.query_count != current.query_count
            || self.language_source_strata != current.language_source_strata
            || self.seed != current.seed
            || self.revisions != current.revisions
            || self.hardware_class != current.hardware_class
            || self.hardware_manifest_anchor != current.hardware_manifest_anchor
            || self.runtime_manifest_anchor != current.runtime_manifest_anchor
            || self.cache_state != current.cache_state
            || self.concurrency != current.concurrency
            || self.warmup_repetitions != current.warmup_repetitions
            || self.measured_repetitions != current.measured_repetitions
        {
            return Err(SemanticBenchmarkSchemaError::TenXWorkloadMismatch {
                field: "paired_contract",
            });
        }
        Ok(())
    }

    fn validate_projection_contract(&self) -> SchemaResult<()> {
        if self.query_set_anchor.is_some()
            || self.query_count != 0
            || !self.language_source_strata.is_empty()
            || self.warmup_repetitions != PROJECTION_WARMUP_REPETITIONS
            || self.measured_repetitions != PROJECTION_MEASURED_REPETITIONS
        {
            return Err(SemanticBenchmarkSchemaError::InvalidWorkload {
                field: "projection_contract",
            });
        }
        Ok(())
    }

    fn validate_query_contract(&self) -> SchemaResult<()> {
        if self.query_set_anchor.is_none()
            || self.query_count == 0
            || self.language_source_strata.is_empty()
            || self.warmup_repetitions != QUERY_WARMUP_REPETITIONS
            || self.measured_repetitions != QUERY_MEASURED_REPETITIONS
        {
            return Err(SemanticBenchmarkSchemaError::InvalidWorkload {
                field: "query_contract",
            });
        }
        let stratum_query_count =
            self.language_source_strata
                .iter()
                .try_fold(0_u64, |total, (stratum, count)| {
                    if !valid_identity(stratum) || *count == 0 {
                        return None;
                    }
                    total.checked_add(*count)
                });
        if stratum_query_count != Some(self.query_count) {
            return Err(SemanticBenchmarkSchemaError::InvalidWorkload {
                field: "language_source_strata",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticBenchmarkSampleV1 {
    pub ordinal: u32,
    pub wall_time_ns: u64,
    pub cpu_time_ns: u64,
    pub peak_rss_bytes: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub model_bytes: u64,
    pub vector_bytes: u64,
    pub cache_bytes: u64,
    pub candidates: u64,
    pub chunks_embedded: u64,
    pub chunks_reused: u64,
    pub chunks_deleted: u64,
    pub hydration_fetches: u64,
}

impl SemanticBenchmarkSampleV1 {
    pub fn validate(&self) -> SchemaResult<()> {
        if self.wall_time_ns == 0 || self.cpu_time_ns == 0 || self.peak_rss_bytes == 0 {
            return Err(SemanticBenchmarkSchemaError::InvalidResult {
                field: "sample_resource_observation",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticBenchmarkResultV1 {
    pub schema_version: u32,
    pub result_id: SemanticBenchmarkResultIdV1,
    pub workload_id: SemanticBenchmarkWorkloadIdV1,
    pub workload_digest: Sha256DigestV1,
    pub code_revision: GitCommitShaV1,
    pub captured_at_unix_micros: i64,
    pub samples: Vec<SemanticBenchmarkSampleV1>,
    pub evidence_anchors: Vec<SemanticBenchmarkEvidenceAnchorV1>,
    pub digest: Sha256DigestV1,
}

#[derive(Serialize)]
struct ResultDigestInput<'a> {
    schema_version: u32,
    result_id: &'a SemanticBenchmarkResultIdV1,
    workload_id: &'a SemanticBenchmarkWorkloadIdV1,
    workload_digest: &'a Sha256DigestV1,
    code_revision: &'a GitCommitShaV1,
    captured_at_unix_micros: i64,
    samples: &'a [SemanticBenchmarkSampleV1],
    evidence_anchors: &'a [SemanticBenchmarkEvidenceAnchorV1],
}

impl SemanticBenchmarkResultV1 {
    pub fn compute_digest(&self) -> SchemaResult<Sha256DigestV1> {
        canonical_sha256(
            SEMANTIC_BENCHMARK_RESULT_DIGEST_DOMAIN,
            &ResultDigestInput {
                schema_version: self.schema_version,
                result_id: &self.result_id,
                workload_id: &self.workload_id,
                workload_digest: &self.workload_digest,
                code_revision: &self.code_revision,
                captured_at_unix_micros: self.captured_at_unix_micros,
                samples: &self.samples,
                evidence_anchors: &self.evidence_anchors,
            },
        )
    }

    pub fn validate(&self) -> SchemaResult<()> {
        if self.schema_version != SEMANTIC_BENCHMARK_RESULT_SCHEMA_VERSION {
            return Err(SemanticBenchmarkSchemaError::UnsupportedSchemaVersion {
                schema: "result",
                actual: self.schema_version,
            });
        }
        if self.captured_at_unix_micros <= 0 || self.samples.is_empty() {
            return Err(SemanticBenchmarkSchemaError::InvalidResult {
                field: "capture_or_samples",
            });
        }
        for (expected_ordinal, sample) in self.samples.iter().enumerate() {
            sample.validate()?;
            if usize::try_from(sample.ordinal).ok() != Some(expected_ordinal) {
                return Err(SemanticBenchmarkSchemaError::InvalidResult {
                    field: "sample_ordinal",
                });
            }
        }
        validate_unique_anchors(self.evidence_anchors.iter())?;

        let workload_anchors: Vec<_> = self
            .evidence_anchors
            .iter()
            .filter(|anchor| anchor.role == BenchmarkEvidenceRoleV1::WorkloadManifest)
            .collect();
        let raw_sample_count = self
            .evidence_anchors
            .iter()
            .filter(|anchor| anchor.role == BenchmarkEvidenceRoleV1::RawSamples)
            .count();
        if workload_anchors.len() != 1
            || workload_anchors[0].artifact_digest != self.workload_digest
        {
            return Err(SemanticBenchmarkSchemaError::InvalidResult {
                field: "workload_manifest_anchor",
            });
        }
        if raw_sample_count != 1 {
            return Err(SemanticBenchmarkSchemaError::InvalidResult {
                field: "raw_samples_anchor",
            });
        }
        if self.compute_digest()? != self.digest {
            return Err(SemanticBenchmarkSchemaError::ResultDigestMismatch);
        }
        Ok(())
    }

    pub fn validate_against_workload(
        &self,
        workload: &SemanticBenchmarkWorkloadV1,
    ) -> SchemaResult<()> {
        workload.validate()?;
        self.validate()?;
        if self.workload_id != workload.workload_id {
            return Err(SemanticBenchmarkSchemaError::WorkloadResultMismatch {
                field: "workload_id",
            });
        }
        if self.workload_digest != workload.digest {
            return Err(SemanticBenchmarkSchemaError::WorkloadResultMismatch {
                field: "workload_digest",
            });
        }
        if self.samples.len()
            != usize::try_from(workload.measured_repetitions).unwrap_or(usize::MAX)
        {
            return Err(SemanticBenchmarkSchemaError::WorkloadResultMismatch {
                field: "measured_repetitions",
            });
        }
        Ok(())
    }
}

fn validate_anchor_role(
    anchor: &SemanticBenchmarkEvidenceAnchorV1,
    expected: BenchmarkEvidenceRoleV1,
    field: &'static str,
) -> SchemaResult<()> {
    anchor.validate()?;
    if anchor.role != expected {
        return Err(SemanticBenchmarkSchemaError::InvalidWorkload { field });
    }
    Ok(())
}

fn validate_unique_anchors<'a>(
    anchors: impl IntoIterator<Item = &'a SemanticBenchmarkEvidenceAnchorV1>,
) -> SchemaResult<()> {
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for anchor in anchors {
        anchor.validate()?;
        if !ids.insert(&anchor.anchor_id) {
            return Err(SemanticBenchmarkSchemaError::DuplicateEvidenceAnchor {
                field: "anchor_id",
            });
        }
        if !paths.insert(&anchor.artifact_path) {
            return Err(SemanticBenchmarkSchemaError::DuplicateEvidenceAnchor {
                field: "artifact_path",
            });
        }
    }
    Ok(())
}

fn canonical_sha256<T: Serialize>(domain: &str, value: &T) -> SchemaResult<Sha256DigestV1> {
    let payload = serde_json::to_vec(value)
        .map_err(|_| SemanticBenchmarkSchemaError::CanonicalSerialization)?;
    let mut hasher = sha2::Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(payload);
    Sha256DigestV1::new(format!("sha256:{}", hex::encode(hasher.finalize())))
}
