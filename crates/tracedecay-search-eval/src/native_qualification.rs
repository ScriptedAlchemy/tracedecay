//! Canonical, byte-pinned native-evaluation evidence for default activation.
//!
//! Native execution remains the evaluator's responsibility. This module only
//! encodes genuine opaque evaluator output and validates a completed report
//! against independently supplied portable runtime and packaged-workload
//! authorities. Loading never creates an evaluator root or model runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChunkerRevision, ComponentRevision, DirectorySyncPolicy,
    EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1,
    EmbeddingPrecisionV1, EmbeddingTruncationSideV1, ManifestDigest, SemanticSearchIndexKeyV1,
    atomic_write,
};

use crate::{
    DirectActivationEvaluationV1, DirectEvaluatedProfileMaterialV1, DirectEvaluationReportV1,
    DirectEvaluationStatusV1, EvaluationExecutionContractV1, SearchEvalError,
    activation_profile_chain, compute_profile_material_digest, compute_workload_digest,
    direct_evaluated_profile_material,
};

const PACKAGED_NATIVE_QUALIFICATION_SCHEMA_VERSION: u32 = 1;

// There is intentionally no `include_bytes!` until a genuine native run has
// generated and reviewed a report. An empty slice is an unavailable artifact,
// never a synthetic qualification, and is rejected before JSON decoding.
const PACKAGED_NATIVE_QUALIFICATION_BYTES: &[u8] = &[];
const PACKAGED_NATIVE_QUALIFICATION_SHA256: Option<&str> = None;

static PACKAGED_NATIVE_QUALIFICATION: OnceLock<
    Result<PackagedNativeQualificationV1, PackagedNativeQualificationErrorV1>,
> = OnceLock::new();

/// Exact evaluator inputs retained inside the report package.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeQualificationEvaluatorKeyV1 {
    pub workload_digest: String,
    pub corpus_digest: String,
    pub execution_contract: EvaluationExecutionContractV1,
    pub profile_material_digests: BTreeMap<String, String>,
    pub raw_output_digest: String,
}

impl NativeQualificationEvaluatorKeyV1 {
    pub fn from_report(report: &DirectEvaluationReportV1) -> Self {
        Self {
            workload_digest: report.workload_digest.clone(),
            corpus_digest: report.corpus_digest.clone(),
            execution_contract: report.execution_contract.clone(),
            profile_material_digests: report.profile_material_digests.clone(),
            raw_output_digest: report.raw_output_digest.clone(),
        }
    }
}

/// The platform on which a native measurement was actually performed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeQualificationPlatformV1 {
    pub operating_system: String,
    pub architecture: String,
}

impl NativeQualificationPlatformV1 {
    pub fn current() -> Self {
        Self {
            operating_system: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        }
    }
}

/// Portable model identity, deliberately excluding project-local privacy and
/// vector-generation fields. Those are rebound by publication to the current
/// admitted project snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeQualificationModelKeyV1 {
    pub model_artifact_digest: ManifestDigest,
    pub tokenizer_digest: ManifestDigest,
    pub config_digest: ManifestDigest,
    pub query_instruction_digest: Option<ManifestDigest>,
    pub document_instruction_digest: Option<ManifestDigest>,
    pub pooling: EmbeddingPoolingV1,
    pub truncation_side: EmbeddingTruncationSideV1,
    pub truncation_length: u32,
    pub runtime_backend: String,
    pub runtime_build_revision: String,
    pub device_class: EmbeddingDeviceClassV1,
    pub dimensions: u32,
    pub metric: EmbeddingMetricV1,
    pub normalization: EmbeddingNormalizationV1,
    pub precision: EmbeddingPrecisionV1,
    pub chunk_schema_revision: String,
    pub chunker_revision: ChunkerRevision,
}

impl NativeQualificationModelKeyV1 {
    pub fn from_admitted_projection(projection: &AdmittedEmbeddingProjectionKeyV1) -> Self {
        let projection = projection.embedding_key();
        Self {
            model_artifact_digest: projection.model_artifact_digest.clone(),
            tokenizer_digest: projection.tokenizer_digest.clone(),
            config_digest: projection.config_digest.clone(),
            query_instruction_digest: projection.query_instruction_digest.clone(),
            document_instruction_digest: projection.document_instruction_digest.clone(),
            pooling: projection.pooling,
            truncation_side: projection.truncation_side,
            truncation_length: projection.truncation_length,
            runtime_backend: projection.runtime_backend.clone(),
            runtime_build_revision: projection.runtime_build_revision.clone(),
            device_class: projection.device_class,
            dimensions: projection.dimensions,
            metric: projection.metric,
            normalization: projection.normalization,
            precision: projection.precision,
            chunk_schema_revision: projection.chunk_schema_revision.clone(),
            chunker_revision: projection.chunker_revision.clone(),
        }
    }

    fn is_valid(&self) -> bool {
        self.model_artifact_digest.validate().is_ok()
            && self.tokenizer_digest.validate().is_ok()
            && self.config_digest.validate().is_ok()
            && self
                .query_instruction_digest
                .as_ref()
                .is_none_or(|digest| digest.validate().is_ok())
            && self
                .document_instruction_digest
                .as_ref()
                .is_none_or(|digest| digest.validate().is_ok())
            && self.truncation_length != 0
            && self.dimensions != 0
            && !self.runtime_backend.trim().is_empty()
            && !self.runtime_build_revision.trim().is_empty()
            && !self.chunk_schema_revision.trim().is_empty()
            && self.chunker_revision.validate().is_ok()
    }
}

/// Fixed execution/resource values observed by the genuine native evaluator.
/// These are portable runtime pins, not configurable ceilings.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeQualificationExecutionResourceKeyV1 {
    pub model_bytes: u64,
    pub tokenizer_bytes: u64,
    pub threads: u32,
    pub max_concurrent_sessions: u32,
    pub batch_size: u32,
    pub sequence_length: u32,
    pub load_deadline_ms: u64,
}

impl NativeQualificationExecutionResourceKeyV1 {
    fn is_valid(self) -> bool {
        self.model_bytes != 0
            && self.tokenizer_bytes != 0
            && self.threads != 0
            && self.max_concurrent_sessions != 0
            && self.batch_size != 0
            && self.sequence_length != 0
            && self.load_deadline_ms != 0
    }
}

/// Portable semantic runtime identity paired with a native report.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeQualificationRuntimeKeyV1 {
    pub implementation_revision: ComponentRevision,
    pub fusion_revision: ComponentRevision,
    pub runtime_compatibility_digest: ManifestDigest,
    pub model: NativeQualificationModelKeyV1,
    pub search_index_key: SemanticSearchIndexKeyV1,
    pub execution_resources: NativeQualificationExecutionResourceKeyV1,
}

/// Typed identity of the exact evaluator/runtime pair that produced a report.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeQualificationKeyV1 {
    pub evaluated_profile_id: String,
    pub evaluator: NativeQualificationEvaluatorKeyV1,
    pub runtime: NativeQualificationRuntimeKeyV1,
    pub platform: NativeQualificationPlatformV1,
}

impl NativeQualificationKeyV1 {
    pub fn new(
        report: &DirectEvaluationReportV1,
        evaluated_profile_id: String,
        runtime: NativeQualificationRuntimeKeyV1,
        platform: NativeQualificationPlatformV1,
    ) -> Self {
        Self {
            evaluated_profile_id,
            evaluator: NativeQualificationEvaluatorKeyV1::from_report(report),
            runtime,
            platform,
        }
    }
}

/// Independent current authorities required to accept a packaged report.
///
/// The daemon builds `runtime` from its mounted semantic authority. The
/// remaining fields come from the package's embedded workload/corpus metadata,
/// never from the report being loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeQualificationExpectationsV1 {
    pub evaluated_profile_id: String,
    pub workload_digest: String,
    pub corpus_digest: String,
    pub fixture_source_repository_commit: String,
    pub fixture_source_repository_tree: String,
    pub execution_contract: EvaluationExecutionContractV1,
    pub profile_material_digests: BTreeMap<String, String>,
    pub runtime: NativeQualificationRuntimeKeyV1,
    pub platform: NativeQualificationPlatformV1,
}

impl NativeQualificationExpectationsV1 {
    /// Build the package-local evaluator authority without materializing its
    /// runtime-root fixture. The caller must supply a separately observed
    /// portable runtime identity and platform.
    pub fn packaged_default(
        evaluated_profile_id: String,
        runtime: NativeQualificationRuntimeKeyV1,
        platform: NativeQualificationPlatformV1,
    ) -> Result<Self, PackagedNativeQualificationErrorV1> {
        let workload = crate::load_authoritative_default_workload_metadata()
            .map_err(|_| PackagedNativeQualificationErrorV1::StaleWorkload)?;
        let profile_ids = activation_profile_chain(&workload, &evaluated_profile_id)
            .map_err(|_| PackagedNativeQualificationErrorV1::InvalidQualificationKey)?;
        let mut profile_material_digests = BTreeMap::new();
        for profile_id in profile_ids {
            let profile = workload
                .profile_matrix
                .iter()
                .find(|profile| profile.profile_id == profile_id)
                .ok_or(PackagedNativeQualificationErrorV1::StaleWorkload)?;
            let digest = compute_profile_material_digest(profile)
                .map_err(|_| PackagedNativeQualificationErrorV1::StaleWorkload)?;
            profile_material_digests.insert(profile_id, digest);
        }
        Ok(Self {
            evaluated_profile_id,
            workload_digest: compute_workload_digest(&workload)
                .map_err(|_| PackagedNativeQualificationErrorV1::StaleWorkload)?,
            corpus_digest: crate::packaged_assets::current_corpus_digest(&workload)
                .map_err(|_| PackagedNativeQualificationErrorV1::StaleCorpus)?,
            fixture_source_repository_commit: workload.source_repository_commit,
            fixture_source_repository_tree: workload.source_repository_tree,
            execution_contract: workload.execution_contract,
            profile_material_digests,
            runtime,
            platform,
        })
    }
}

/// The checked-in native qualification document. It is intentionally report
/// evidence plus immutable identity only; activation remains the daemon's
/// compare-and-swap publication responsibility.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackagedNativeQualificationV1 {
    pub schema_version: u32,
    pub qualification_key: NativeQualificationKeyV1,
    pub portable_evidence: PortableNativeQualificationEvidenceV1,
}

/// An explicit retention state for evidence that is portable only because its
/// project-local vector generations were removed, never substituted.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeQualificationVectorGenerationRetentionV1 {
    RedactedForPortableQualification,
}

/// Genuine evaluator evidence with local vector-generation identifiers
/// deliberately removed. The retained report still contains all queries,
/// stages, aggregates, resources, and portable runtime provenance needed for
/// complete qualification validation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PortableNativeQualificationEvidenceV1 {
    pub vector_generation_retention: NativeQualificationVectorGenerationRetentionV1,
    pub report: DirectEvaluationReportV1,
}

/// Opaque activation material reconstructed only after portable evidence has
/// passed package validation. It is intentionally distinct from a normal
/// `DirectActivationEvaluationV1`: its report cannot claim evaluator-local
/// vector-generation identities that the package does not retain.
#[derive(Clone, Debug)]
pub struct PackagedNativeActivationCandidateV1 {
    portable_evidence: PortableNativeQualificationEvidenceV1,
    evaluated_material: DirectEvaluatedProfileMaterialV1,
}

impl PackagedNativeActivationCandidateV1 {
    pub fn portable_evidence(&self) -> &PortableNativeQualificationEvidenceV1 {
        &self.portable_evidence
    }

    pub fn into_parts(
        self,
    ) -> (
        PortableNativeQualificationEvidenceV1,
        DirectEvaluatedProfileMaterialV1,
    ) {
        (self.portable_evidence, self.evaluated_material)
    }
}

/// Every load denial is explicit so stale or incomplete evidence can never
/// appear as an activation-ready result.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PackagedNativeQualificationErrorV1 {
    #[error("embedded native qualification is unavailable")]
    EmbeddedAssetUnavailable,
    #[error("native qualification bytes are corrupt")]
    CorruptBytes,
    #[error("native qualification schema is unsupported")]
    UnsupportedSchema,
    #[error("native qualification key is invalid")]
    InvalidQualificationKey,
    #[error("native qualification workload is stale")]
    StaleWorkload,
    #[error("native qualification corpus is stale")]
    StaleCorpus,
    #[error("native qualification execution revision is stale")]
    StaleExecutionRevision,
    #[error("native qualification model identity does not match")]
    ModelMismatch,
    #[error("native qualification build identity does not match")]
    BuildMismatch,
    #[error("native qualification search-index identity does not match")]
    SearchIndexMismatch,
    #[error("native qualification runtime identity does not match")]
    RuntimeMismatch,
    #[error("native qualification platform identity does not match")]
    PlatformMismatch,
    #[error("native qualification raw output evidence is invalid")]
    InvalidRawOutputEvidence,
    #[error("native qualification lacks complete native evidence")]
    IncompleteNativeEvidence,
    #[error("native qualification did not pass")]
    FailedQualification,
}

/// Encode opaque output returned by the genuine evaluator. There is no
/// report-shaped encoder: a daemon must hold `DirectActivationEvaluationV1`
/// in-process to create package bytes.
pub fn encode_packaged_native_qualification(
    evaluation: DirectActivationEvaluationV1,
    mut qualification_key: NativeQualificationKeyV1,
) -> Result<Vec<u8>, PackagedNativeQualificationErrorV1> {
    let (report, material) = evaluation.into_parts();
    if material.profile.profile_id.as_str() != qualification_key.evaluated_profile_id {
        return Err(PackagedNativeQualificationErrorV1::InvalidQualificationKey);
    }
    if qualification_key.evaluator != NativeQualificationEvaluatorKeyV1::from_report(&report) {
        return Err(PackagedNativeQualificationErrorV1::InvalidQualificationKey);
    }
    let expectations = NativeQualificationExpectationsV1::packaged_default(
        qualification_key.evaluated_profile_id.clone(),
        qualification_key.runtime.clone(),
        qualification_key.platform.clone(),
    )?;
    let portable_evidence = redact_genuine_vector_generations(report)?;
    qualification_key.evaluator =
        NativeQualificationEvaluatorKeyV1::from_report(&portable_evidence.report);
    let qualification = PackagedNativeQualificationV1 {
        schema_version: PACKAGED_NATIVE_QUALIFICATION_SCHEMA_VERSION,
        qualification_key,
        portable_evidence,
    };
    validate_qualification(&qualification, &expectations)?;
    serde_json::to_vec(&qualification).map_err(|_| PackagedNativeQualificationErrorV1::CorruptBytes)
}

fn redact_genuine_vector_generations(
    mut report: DirectEvaluationReportV1,
) -> Result<PortableNativeQualificationEvidenceV1, PackagedNativeQualificationErrorV1> {
    for output in &mut report.raw_outputs {
        let resources = output
            .native_resources
            .as_mut()
            .ok_or(PackagedNativeQualificationErrorV1::IncompleteNativeEvidence)?;
        for sample in resources.samples.values_mut() {
            let crate::semantic_native::SemanticNativeStageResultV1::Complete(sample) = sample
            else {
                return Err(PackagedNativeQualificationErrorV1::IncompleteNativeEvidence);
            };
            if sample
                .provenance
                .vector_generation_id
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return Err(PackagedNativeQualificationErrorV1::IncompleteNativeEvidence);
            }
            sample.provenance.vector_generation_id = None;
        }
    }
    report.raw_output_digest = crate::report::raw_output_digest(&report.raw_outputs)
        .map_err(|_| PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence)?;
    Ok(PortableNativeQualificationEvidenceV1 {
        vector_generation_retention:
            NativeQualificationVectorGenerationRetentionV1::RedactedForPortableQualification,
        report,
    })
}

fn validate_redacted_vector_generation_shape(
    report: &DirectEvaluationReportV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    for output in &report.raw_outputs {
        let Some(resources) = &output.native_resources else {
            continue;
        };
        for sample in resources.samples.values() {
            let crate::semantic_native::SemanticNativeStageResultV1::Complete(sample) = sample
            else {
                continue;
            };
            if sample.provenance.vector_generation_id.is_some() {
                return Err(PackagedNativeQualificationErrorV1::InvalidQualificationKey);
            }
        }
    }
    Ok(())
}

/// Write canonical qualification bytes through the workspace's durable atomic
/// writer. This writes only an artifact file and never publishes activation.
pub fn write_packaged_native_qualification(
    output: &Path,
    bytes: &[u8],
    expectations: &NativeQualificationExpectationsV1,
) -> Result<(), SearchEvalError> {
    let qualification = serde_json::from_slice::<PackagedNativeQualificationV1>(bytes)
        .map_err(|_| PackagedNativeQualificationErrorV1::CorruptBytes)
        .and_then(|qualification| {
            validate_qualification(&qualification, expectations)?;
            Ok(qualification)
        })
        .map_err(|error| SearchEvalError::Contract(error.to_string()))?;
    let canonical = serde_json::to_vec(&qualification).map_err(|error| {
        SearchEvalError::Contract(format!("serialize native qualification: {error}"))
    })?;
    if canonical != bytes {
        return Err(SearchEvalError::Contract(
            "native qualification bytes are not canonical".to_owned(),
        ));
    }
    atomic_write(
        output,
        "native-qualification",
        &canonical,
        DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|error| {
        SearchEvalError::Contract(format!(
            "durably write native qualification {}: {error}",
            output.display()
        ))
    })
}

/// Validate arbitrary package bytes without materializing evaluator assets.
pub fn load_packaged_native_qualification_from_bytes(
    bytes: &[u8],
    expectations: &NativeQualificationExpectationsV1,
) -> Result<PackagedNativeActivationCandidateV1, PackagedNativeQualificationErrorV1> {
    let qualification = serde_json::from_slice::<PackagedNativeQualificationV1>(bytes)
        .map_err(|_| PackagedNativeQualificationErrorV1::CorruptBytes)?;
    validate_qualification(&qualification, expectations)?;
    activation_candidate_from_qualification(qualification, expectations)
}

/// The literal bytes intended for `include_bytes!` after a genuine native run
/// produces a reviewed report. Empty means the package makes no claim.
pub fn packaged_native_qualification_bytes() -> &'static [u8] {
    PACKAGED_NATIVE_QUALIFICATION_BYTES
}

/// Test-only access to the canonical package materialization counter. Loading
/// qualification must leave it unchanged.
#[cfg(test)]
pub fn packaged_native_qualification_materialization_count() -> u64 {
    crate::packaged_assets::materialization_count()
}

/// Load embedded bytes through one process-wide SHA-pinned structural parse,
/// then validate them against the caller's independent current authorities.
pub fn qualified_default_activation_candidate(
    expectations: &NativeQualificationExpectationsV1,
) -> Result<PackagedNativeActivationCandidateV1, PackagedNativeQualificationErrorV1> {
    if PACKAGED_NATIVE_QUALIFICATION_BYTES.is_empty()
        || PACKAGED_NATIVE_QUALIFICATION_SHA256.is_none()
    {
        return Err(PackagedNativeQualificationErrorV1::EmbeddedAssetUnavailable);
    }
    let qualification = PACKAGED_NATIVE_QUALIFICATION
        .get_or_init(load_embedded_qualification)
        .clone()?;
    validate_qualification(&qualification, expectations)?;
    activation_candidate_from_qualification(qualification, expectations)
}

fn load_embedded_qualification()
-> Result<PackagedNativeQualificationV1, PackagedNativeQualificationErrorV1> {
    let expected = PACKAGED_NATIVE_QUALIFICATION_SHA256
        .ok_or(PackagedNativeQualificationErrorV1::EmbeddedAssetUnavailable)?;
    if canonical_sha256(PACKAGED_NATIVE_QUALIFICATION_BYTES) != expected {
        return Err(PackagedNativeQualificationErrorV1::CorruptBytes);
    }
    let qualification = serde_json::from_slice::<PackagedNativeQualificationV1>(
        PACKAGED_NATIVE_QUALIFICATION_BYTES,
    )
    .map_err(|_| PackagedNativeQualificationErrorV1::CorruptBytes)?;
    validate_document_bindings(&qualification)?;
    Ok(qualification)
}

fn activation_candidate_from_qualification(
    qualification: PackagedNativeQualificationV1,
    expectations: &NativeQualificationExpectationsV1,
) -> Result<PackagedNativeActivationCandidateV1, PackagedNativeQualificationErrorV1> {
    let workload = crate::load_authoritative_default_workload_metadata()
        .map_err(|_| PackagedNativeQualificationErrorV1::StaleWorkload)?;
    let material = direct_evaluated_profile_material(&workload, &expectations.evaluated_profile_id)
        .map_err(|_| PackagedNativeQualificationErrorV1::InvalidQualificationKey)?;
    Ok(PackagedNativeActivationCandidateV1 {
        portable_evidence: qualification.portable_evidence,
        evaluated_material: material,
    })
}

fn validate_qualification(
    qualification: &PackagedNativeQualificationV1,
    expectations: &NativeQualificationExpectationsV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    validate_document_bindings(qualification)?;
    validate_expectations(expectations)?;
    validate_expected_identities(qualification, expectations)?;
    validate_required_profile_matrix(qualification, expectations)?;
    validate_expected_profile_materials(qualification, expectations)?;
    let workload = crate::load_authoritative_default_workload_metadata()
        .map_err(|_| PackagedNativeQualificationErrorV1::StaleWorkload)?;
    qualification
        .portable_evidence
        .report
        .validate_portable_qualification_against_authoritative_corpus(
            &workload,
            &expectations.corpus_digest,
        )
        .map_err(|error| match error {
            crate::report::PortableNativeQualificationValidationErrorV1::Report => {
                PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence
            }
            crate::report::PortableNativeQualificationValidationErrorV1::NativeEvidence => {
                PackagedNativeQualificationErrorV1::IncompleteNativeEvidence
            }
        })?;
    validate_report_runtime_bindings(
        &qualification.portable_evidence.report,
        &qualification.qualification_key.runtime,
    )?;
    if qualification.portable_evidence.report.raw_output_digest
        != qualification.qualification_key.evaluator.raw_output_digest
    {
        return Err(PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence);
    }
    Ok(())
}

fn validate_document_bindings(
    qualification: &PackagedNativeQualificationV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    if qualification.schema_version != PACKAGED_NATIVE_QUALIFICATION_SCHEMA_VERSION {
        return Err(PackagedNativeQualificationErrorV1::UnsupportedSchema);
    }
    validate_key(&qualification.qualification_key)?;
    let report = &qualification.portable_evidence.report;
    if qualification.portable_evidence.vector_generation_retention
        != NativeQualificationVectorGenerationRetentionV1::RedactedForPortableQualification
    {
        return Err(PackagedNativeQualificationErrorV1::InvalidQualificationKey);
    }
    validate_redacted_vector_generation_shape(report)?;
    let evaluator = &qualification.qualification_key.evaluator;
    if report.workload_digest != evaluator.workload_digest {
        return Err(PackagedNativeQualificationErrorV1::StaleWorkload);
    }
    if report.corpus_digest != evaluator.corpus_digest {
        return Err(PackagedNativeQualificationErrorV1::StaleCorpus);
    }
    validate_execution_contract_pair(&report.execution_contract, &evaluator.execution_contract)?;
    if report.profile_material_digests != evaluator.profile_material_digests {
        return Err(PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence);
    }
    let raw_output_digest = crate::report::raw_output_digest(&report.raw_outputs)
        .map_err(|_| PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence)?;
    if report.raw_output_digest != raw_output_digest {
        return Err(PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence);
    }
    if report.status != DirectEvaluationStatusV1::Pass {
        return Err(PackagedNativeQualificationErrorV1::FailedQualification);
    }
    Ok(())
}

fn validate_expectations(
    expectations: &NativeQualificationExpectationsV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    if expectations.evaluated_profile_id.trim().is_empty()
        || expectations.workload_digest.trim().is_empty()
        || expectations.corpus_digest.trim().is_empty()
        || expectations
            .fixture_source_repository_commit
            .trim()
            .is_empty()
        || expectations
            .fixture_source_repository_tree
            .trim()
            .is_empty()
        || expectations.platform.operating_system.trim().is_empty()
        || expectations.platform.architecture.trim().is_empty()
    {
        return Err(PackagedNativeQualificationErrorV1::InvalidQualificationKey);
    }
    validate_runtime_key(&expectations.runtime)
}

fn validate_key(key: &NativeQualificationKeyV1) -> Result<(), PackagedNativeQualificationErrorV1> {
    if key.evaluated_profile_id.trim().is_empty()
        || key.evaluator.workload_digest.trim().is_empty()
        || key.evaluator.corpus_digest.trim().is_empty()
        || key.evaluator.raw_output_digest.trim().is_empty()
        || key.platform.operating_system.trim().is_empty()
        || key.platform.architecture.trim().is_empty()
    {
        return Err(PackagedNativeQualificationErrorV1::InvalidQualificationKey);
    }
    validate_runtime_key(&key.runtime)
}

fn validate_runtime_key(
    runtime: &NativeQualificationRuntimeKeyV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    if runtime.implementation_revision.validate().is_err()
        || runtime.fusion_revision.validate().is_err()
        || runtime.runtime_compatibility_digest.validate().is_err()
        || !runtime.model.is_valid()
        || runtime.search_index_key.validate().is_err()
        || !runtime.execution_resources.is_valid()
    {
        return Err(PackagedNativeQualificationErrorV1::InvalidQualificationKey);
    }
    Ok(())
}

fn validate_expected_identities(
    qualification: &PackagedNativeQualificationV1,
    expectations: &NativeQualificationExpectationsV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    let key = &qualification.qualification_key;
    let report = &qualification.portable_evidence.report;
    if key.evaluated_profile_id != expectations.evaluated_profile_id {
        return Err(PackagedNativeQualificationErrorV1::InvalidQualificationKey);
    }
    if key.evaluator.workload_digest != expectations.workload_digest
        || report.workload_digest != expectations.workload_digest
    {
        return Err(PackagedNativeQualificationErrorV1::StaleWorkload);
    }
    if key.evaluator.corpus_digest != expectations.corpus_digest
        || report.corpus_digest != expectations.corpus_digest
        || report.fixture_source_repository_commit != expectations.fixture_source_repository_commit
        || report.fixture_source_repository_tree != expectations.fixture_source_repository_tree
    {
        return Err(PackagedNativeQualificationErrorV1::StaleCorpus);
    }
    validate_execution_contract_pair(
        &key.evaluator.execution_contract,
        &expectations.execution_contract,
    )?;
    validate_execution_contract_pair(&report.execution_contract, &expectations.execution_contract)?;
    validate_runtime_identity(&key.runtime, &expectations.runtime)?;
    if key.platform != expectations.platform
        || expectations.platform != NativeQualificationPlatformV1::current()
    {
        return Err(PackagedNativeQualificationErrorV1::PlatformMismatch);
    }
    Ok(())
}

fn validate_expected_profile_materials(
    qualification: &PackagedNativeQualificationV1,
    expectations: &NativeQualificationExpectationsV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    let evaluator = &qualification.qualification_key.evaluator;
    if evaluator.profile_material_digests != expectations.profile_material_digests
        || qualification
            .portable_evidence
            .report
            .profile_material_digests
            != expectations.profile_material_digests
    {
        return Err(PackagedNativeQualificationErrorV1::StaleWorkload);
    }
    Ok(())
}

fn validate_execution_contract_pair(
    observed: &EvaluationExecutionContractV1,
    expected: &EvaluationExecutionContractV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    if observed.model_revision != expected.model_revision {
        return Err(PackagedNativeQualificationErrorV1::ModelMismatch);
    }
    if observed.runtime_revision != expected.runtime_revision {
        return Err(PackagedNativeQualificationErrorV1::RuntimeMismatch);
    }
    if observed != expected {
        return Err(PackagedNativeQualificationErrorV1::StaleExecutionRevision);
    }
    Ok(())
}

fn validate_runtime_identity(
    observed: &NativeQualificationRuntimeKeyV1,
    expected: &NativeQualificationRuntimeKeyV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    if observed.model != expected.model {
        return Err(PackagedNativeQualificationErrorV1::ModelMismatch);
    }
    if observed.search_index_key != expected.search_index_key {
        return Err(PackagedNativeQualificationErrorV1::SearchIndexMismatch);
    }
    if observed.implementation_revision != expected.implementation_revision
        || observed.fusion_revision != expected.fusion_revision
    {
        return Err(PackagedNativeQualificationErrorV1::BuildMismatch);
    }
    if observed.runtime_compatibility_digest != expected.runtime_compatibility_digest
        || observed.execution_resources != expected.execution_resources
    {
        return Err(PackagedNativeQualificationErrorV1::RuntimeMismatch);
    }
    Ok(())
}

fn validate_required_profile_matrix(
    qualification: &PackagedNativeQualificationV1,
    expectations: &NativeQualificationExpectationsV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    let workload = crate::load_authoritative_default_workload_metadata()
        .map_err(|_| PackagedNativeQualificationErrorV1::StaleWorkload)?;
    let profiles = activation_profile_chain(&workload, &expectations.evaluated_profile_id)
        .map_err(|_| PackagedNativeQualificationErrorV1::InvalidQualificationKey)?;
    let expected = profiles
        .iter()
        .flat_map(|profile| {
            ["train", "validation"]
                .into_iter()
                .map(move |partition| (profile.as_str(), partition))
        })
        .collect::<BTreeSet<_>>();
    let observed = qualification
        .portable_evidence
        .report
        .raw_outputs
        .iter()
        .map(|output| (output.profile_id.as_str(), output.partition.as_str()))
        .collect::<BTreeSet<_>>();
    if observed != expected
        || qualification.portable_evidence.report.raw_outputs.len() != expected.len()
    {
        return Err(PackagedNativeQualificationErrorV1::IncompleteNativeEvidence);
    }
    Ok(())
}

fn validate_report_runtime_bindings(
    report: &DirectEvaluationReportV1,
    runtime: &NativeQualificationRuntimeKeyV1,
) -> Result<(), PackagedNativeQualificationErrorV1> {
    for output in &report.raw_outputs {
        let resources = output
            .native_resources
            .as_ref()
            .ok_or(PackagedNativeQualificationErrorV1::IncompleteNativeEvidence)?;
        for scale in ["current", "10x"] {
            let sample = resources
                .samples
                .get(scale)
                .ok_or(PackagedNativeQualificationErrorV1::IncompleteNativeEvidence)?;
            let crate::semantic_native::SemanticNativeStageResultV1::Complete(sample) = sample
            else {
                return Err(PackagedNativeQualificationErrorV1::IncompleteNativeEvidence);
            };
            if sample.provenance.artifact_digest.as_deref()
                != Some(runtime.model.model_artifact_digest.as_str())
            {
                return Err(PackagedNativeQualificationErrorV1::ModelMismatch);
            }
            let observed = NativeQualificationExecutionResourceKeyV1 {
                model_bytes: sample
                    .model_bytes
                    .ok_or(PackagedNativeQualificationErrorV1::IncompleteNativeEvidence)?,
                tokenizer_bytes: sample
                    .tokenizer_bytes
                    .ok_or(PackagedNativeQualificationErrorV1::IncompleteNativeEvidence)?,
                threads: sample.provenance.threads,
                max_concurrent_sessions: sample.provenance.max_concurrent_sessions,
                batch_size: sample.provenance.batch_size,
                sequence_length: sample.provenance.sequence_length,
                load_deadline_ms: sample.provenance.load_deadline_ms,
            };
            if observed != runtime.execution_resources {
                return Err(PackagedNativeQualificationErrorV1::RuntimeMismatch);
            }
        }
    }
    Ok(())
}

fn canonical_sha256(bytes: &[u8]) -> String {
    let mut digest = String::from("sha256:");
    digest.push_str(&hex::encode(Sha256::digest(bytes)));
    digest
}
