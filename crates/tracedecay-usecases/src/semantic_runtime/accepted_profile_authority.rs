use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use tracedecay_domain::{CodeGenerationId, ManifestDigest, VectorGenerationIdV1, canonical_sha256};
use zeroize::Zeroizing;

use super::SemanticRuntimeFuture;
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, PassingRetrievalEvaluationV1, RetrievalRuntimeCompatibilityV1,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_search_eval::semantic_native::SemanticNativeStageResultV1;
use tracedecay_search_eval::{
    CandidateWorkloadV1, DirectEvaluationReportV1, direct_evaluated_profile_material,
};

const ACTIVATION_WORKLOAD_JSON: &str = include_str!(
    "../../../../tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json"
);
const VALIDATION_RECEIPT_DOMAIN: &str =
    "tracedecay.semantic.accepted-profile-validation-receipt.v1";
const VALIDATION_RECEIPT_KEY_BYTES: usize = 32;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticAcceptedProfileAuthorityErrorV1 {
    #[error("accepted semantic profile authority is unavailable")]
    Unavailable,
    #[error("accepted semantic profile authority was rejected")]
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticAcceptedProfileAuthorityRecordV1 {
    pub accepted_profile: AcceptedRetrievalProfileV1,
    pub runtime: RetrievalRuntimeCompatibilityV1,
    pub freshness_vector_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct SemanticEvaluationPublicationIdentityV1 {
    pub scope_digest: ManifestDigest,
    pub code_generation: CodeGenerationId,
    pub code_source_manifest_digest: ManifestDigest,
    pub code_snapshot_digest: ManifestDigest,
    pub semantic_source_generation: Option<CodeGenerationId>,
    pub vector_state_revision: Option<i64>,
    pub vector_generation_id: Option<VectorGenerationIdV1>,
}

pub trait SemanticAcceptedProfileAuthorityPortV1 {
    fn resolve<'a>(
        &'a self,
        profile_digest: &'a ManifestDigest,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticAcceptedProfileAuthorityRecordV1, SemanticAcceptedProfileAuthorityErrorV1>,
    >;
}

#[derive(Clone)]
pub struct RegisteredSemanticAcceptedProfileAuthorityV1 {
    database: Arc<RegisteredGlobalDb>,
}

impl RegisteredSemanticAcceptedProfileAuthorityV1 {
    /// The accepted-profile tables are part of the canonical configuration
    /// schema, provisioned and shape-validated at registered database
    /// admission, so construction performs no schema work.
    pub fn new(database: Arc<RegisteredGlobalDb>) -> Self {
        Self { database }
    }

    /// Persists only a profile whose private evaluation value can be
    /// reconstructed from this real direct-evaluator report.
    pub(super) async fn publish(
        &self,
        evaluation_repository_root: &Path,
        report: DirectEvaluationReportV1,
        accepted_profile: AcceptedRetrievalProfileV1,
        runtime: RetrievalRuntimeCompatibilityV1,
        publication_identity: SemanticEvaluationPublicationIdentityV1,
        freshness_vector_digest: ManifestDigest,
    ) -> Result<(), SemanticAcceptedProfileAuthorityErrorV1> {
        let evaluation_repository_root = evaluation_repository_root
            .canonicalize()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
        let evidence = validate_publication_authority(
            &evaluation_repository_root,
            &report,
            &accepted_profile,
            &runtime,
            &publication_identity,
            &freshness_vector_digest,
        )?;
        let stored = StoredAcceptedProfileAuthorityPayloadV1 {
            report,
            accepted_profile: accepted_profile.clone(),
            runtime,
            publication_identity,
            freshness_vector_digest,
        };
        let payload_json = serde_json::to_string(&stored)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        let transaction = self
            .database
            .begin_write_transaction()
            .await
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
        let key = ensure_validation_receipt_key(&transaction).await?;
        let receipt =
            evidence.into_receipt(&key, accepted_profile.profile_digest(), &payload_json)?;
        let json = serde_json::to_string(&StoredAcceptedProfileAuthorityEnvelopeV1 {
            schema_version: 1,
            payload_json,
            receipt,
        })
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        let affected = transaction
            .execute(
                "INSERT INTO configuration_semantic_accepted_profiles_v1 (
                    profile_digest, authority_json
                 ) VALUES (?1, ?2)
                 ON CONFLICT(profile_digest) DO UPDATE SET
                    authority_json = excluded.authority_json
                 WHERE authority_json = excluded.authority_json",
                params![accepted_profile.profile_digest().as_str(), json],
            )
            .await
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        if affected != 1 {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
        transaction
            .commit()
            .await
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)
    }

    async fn resolve_record(
        &self,
        profile_digest: &ManifestDigest,
    ) -> Result<SemanticAcceptedProfileAuthorityRecordV1, SemanticAcceptedProfileAuthorityErrorV1>
    {
        profile_digest
            .validate()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        let snapshot = self
            .database
            .read_snapshot()
            .await
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
        let mut rows = snapshot
            .query(
                "SELECT authority_json
                 FROM configuration_semantic_accepted_profiles_v1
                 WHERE profile_digest = ?1",
                params![profile_digest.as_str()],
            )
            .await
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?
            .ok_or(SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
        let json: String = row
            .get(0)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        let key = read_validation_receipt_key(&snapshot)
            .await?
            .ok_or(SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        let envelope: StoredAcceptedProfileAuthorityEnvelopeV1 = serde_json::from_str(&json)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        if envelope.schema_version != 1 {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
        envelope
            .receipt
            .verify_authenticity(&key, profile_digest, &envelope.payload_json)?;
        let stored: StoredAcceptedProfileAuthorityPayloadV1 =
            serde_json::from_str(&envelope.payload_json)
                .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        stored.validate(profile_digest, &envelope.receipt.bindings)
    }
}

impl SemanticAcceptedProfileAuthorityPortV1 for RegisteredSemanticAcceptedProfileAuthorityV1 {
    fn resolve<'a>(
        &'a self,
        profile_digest: &'a ManifestDigest,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticAcceptedProfileAuthorityRecordV1, SemanticAcceptedProfileAuthorityErrorV1>,
    > {
        Box::pin(async move { self.resolve_record(profile_digest).await })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAcceptedProfileAuthorityEnvelopeV1 {
    schema_version: u32,
    payload_json: String,
    receipt: AcceptedProfileValidationReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAcceptedProfileAuthorityPayloadV1 {
    report: DirectEvaluationReportV1,
    accepted_profile: AcceptedRetrievalProfileV1,
    runtime: RetrievalRuntimeCompatibilityV1,
    publication_identity: SemanticEvaluationPublicationIdentityV1,
    freshness_vector_digest: ManifestDigest,
}

impl StoredAcceptedProfileAuthorityPayloadV1 {
    fn validate(
        self,
        expected_digest: &ManifestDigest,
        expected_bindings: &AcceptedProfileValidationBindingsV1,
    ) -> Result<SemanticAcceptedProfileAuthorityRecordV1, SemanticAcceptedProfileAuthorityErrorV1>
    {
        if self.accepted_profile.profile_digest() != expected_digest {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
        validate_retained_authority(
            &self.report,
            &self.accepted_profile,
            &self.runtime,
            &self.publication_identity,
            &self.freshness_vector_digest,
            expected_bindings,
        )?;
        Ok(SemanticAcceptedProfileAuthorityRecordV1 {
            accepted_profile: self.accepted_profile,
            runtime: self.runtime,
            freshness_vector_digest: self.freshness_vector_digest,
        })
    }
}

struct ValidatedActivationEvidenceV1 {
    bindings: AcceptedProfileValidationBindingsV1,
}

impl ValidatedActivationEvidenceV1 {
    fn into_receipt(
        self,
        key: &[u8],
        profile_digest: &ManifestDigest,
        payload_json: &str,
    ) -> Result<AcceptedProfileValidationReceiptV1, SemanticAcceptedProfileAuthorityErrorV1> {
        AcceptedProfileValidationReceiptV1::from_validated(self, key, profile_digest, payload_json)
    }
}

fn validate_publication_authority(
    evaluation_repository_root: &Path,
    report: &DirectEvaluationReportV1,
    accepted_profile: &AcceptedRetrievalProfileV1,
    runtime: &RetrievalRuntimeCompatibilityV1,
    publication_identity: &SemanticEvaluationPublicationIdentityV1,
    freshness_vector_digest: &ManifestDigest,
) -> Result<ValidatedActivationEvidenceV1, SemanticAcceptedProfileAuthorityErrorV1> {
    let workload: CandidateWorkloadV1 = serde_json::from_str(ACTIVATION_WORKLOAD_JSON)
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    report
        .validate_for_activation(evaluation_repository_root, &workload)
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    let evaluated_profile_id = accepted_profile.evaluation().evaluated_profile_id();
    let evaluation = PassingRetrievalEvaluationV1::from_report(report, evaluated_profile_id)
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    if &evaluation != accepted_profile.evaluation() {
        return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
    }
    let material = direct_evaluated_profile_material(&workload, evaluated_profile_id)
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    let mut expected_profile = material.profile;
    expected_profile.evaluation_result_anchor = evaluation.evaluation_anchor().clone();
    let mut expected_diversity = material.diversity;
    expected_diversity.evaluation_result_anchor = Some(evaluation.evaluation_anchor().clone());
    let expected_rerank = material.rerank.map(|mut rerank| {
        rerank.evaluation_result_anchor = evaluation.evaluation_anchor().clone();
        rerank
    });
    if accepted_profile.profile() != &expected_profile
        || accepted_profile.diversity() != &expected_diversity
        || accepted_profile.rerank() != expected_rerank.as_ref()
    {
        return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
    }
    validate_runtime_evidence(report, accepted_profile, evaluated_profile_id)?;
    accepted_profile
        .executable_under(runtime)
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    freshness_vector_digest
        .validate()
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    publication_identity.validate(freshness_vector_digest)?;
    Ok(ValidatedActivationEvidenceV1 {
        bindings: receipt_bindings(
            report,
            accepted_profile,
            runtime,
            publication_identity,
            freshness_vector_digest,
            &evaluation,
        )?,
    })
}

fn validate_retained_authority(
    report: &DirectEvaluationReportV1,
    accepted_profile: &AcceptedRetrievalProfileV1,
    runtime: &RetrievalRuntimeCompatibilityV1,
    publication_identity: &SemanticEvaluationPublicationIdentityV1,
    freshness_vector_digest: &ManifestDigest,
    expected_bindings: &AcceptedProfileValidationBindingsV1,
) -> Result<(), SemanticAcceptedProfileAuthorityErrorV1> {
    let evaluation = PassingRetrievalEvaluationV1::from_report(
        report,
        accepted_profile.evaluation().evaluated_profile_id(),
    )
    .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    if &evaluation != accepted_profile.evaluation() {
        return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
    }
    validate_runtime_evidence(
        report,
        accepted_profile,
        accepted_profile.evaluation().evaluated_profile_id(),
    )?;
    accepted_profile
        .executable_under(runtime)
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    freshness_vector_digest
        .validate()
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    publication_identity.validate(freshness_vector_digest)?;
    let bindings = receipt_bindings(
        report,
        accepted_profile,
        runtime,
        publication_identity,
        freshness_vector_digest,
        &evaluation,
    )?;
    if expected_bindings != &bindings {
        return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AcceptedProfileValidationBindingsV1 {
    report_digest: ManifestDigest,
    workload_digest: ManifestDigest,
    corpus_digest: ManifestDigest,
    fixture_source_repository_commit: String,
    fixture_source_repository_tree: String,
    raw_output_digest: ManifestDigest,
    profile_material_digests: BTreeMap<String, ManifestDigest>,
    evaluated_profile_id: String,
    accepted_profile_digest: ManifestDigest,
    runtime_digest: ManifestDigest,
    publication_identity: SemanticEvaluationPublicationIdentityV1,
    freshness_vector_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AcceptedProfileValidationReceiptV1 {
    schema_version: u32,
    bindings: AcceptedProfileValidationBindingsV1,
    authentication_digest: ManifestDigest,
}

impl AcceptedProfileValidationReceiptV1 {
    fn from_validated(
        evidence: ValidatedActivationEvidenceV1,
        key: &[u8],
        profile_digest: &ManifestDigest,
        payload_json: &str,
    ) -> Result<Self, SemanticAcceptedProfileAuthorityErrorV1> {
        let bindings = evidence.bindings;
        let authentication_digest =
            receipt_authentication_digest(key, profile_digest, payload_json, &bindings)?;
        Ok(Self {
            schema_version: 1,
            bindings,
            authentication_digest,
        })
    }

    fn verify_authenticity(
        &self,
        key: &[u8],
        expected_profile_digest: &ManifestDigest,
        payload_json: &str,
    ) -> Result<(), SemanticAcceptedProfileAuthorityErrorV1> {
        self.authentication_digest
            .validate()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        if self.schema_version != 1
            || &self.bindings.accepted_profile_digest != expected_profile_digest
            || self.authentication_digest
                != receipt_authentication_digest(
                    key,
                    expected_profile_digest,
                    payload_json,
                    &self.bindings,
                )?
        {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
        Ok(())
    }
}

fn receipt_bindings(
    report: &DirectEvaluationReportV1,
    accepted_profile: &AcceptedRetrievalProfileV1,
    runtime: &RetrievalRuntimeCompatibilityV1,
    publication_identity: &SemanticEvaluationPublicationIdentityV1,
    freshness_vector_digest: &ManifestDigest,
    evaluation: &PassingRetrievalEvaluationV1,
) -> Result<AcceptedProfileValidationBindingsV1, SemanticAcceptedProfileAuthorityErrorV1> {
    let workload_digest = manifest_digest(&report.workload_digest)?;
    let corpus_digest = manifest_digest(&report.corpus_digest)?;
    let raw_output_digest = manifest_digest(&report.raw_output_digest)?;
    if report.fixture_source_repository_commit.trim().is_empty()
        || report.fixture_source_repository_tree.trim().is_empty()
        || report.profile_material_digests.is_empty()
    {
        return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
    }
    let profile_material_digests = report
        .profile_material_digests
        .iter()
        .map(|(profile_id, digest)| {
            if profile_id.trim().is_empty() {
                return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
            }
            Ok((profile_id.clone(), manifest_digest(digest)?))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let runtime_digest =
        canonical_sha256(&("tracedecay.semantic.accepted-profile-runtime.v1", runtime))
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    Ok(AcceptedProfileValidationBindingsV1 {
        report_digest: evaluation.report_digest().clone(),
        workload_digest,
        corpus_digest,
        fixture_source_repository_commit: report.fixture_source_repository_commit.clone(),
        fixture_source_repository_tree: report.fixture_source_repository_tree.clone(),
        raw_output_digest,
        profile_material_digests,
        evaluated_profile_id: evaluation.evaluated_profile_id().to_owned(),
        accepted_profile_digest: accepted_profile.profile_digest().clone(),
        runtime_digest,
        publication_identity: publication_identity.clone(),
        freshness_vector_digest: freshness_vector_digest.clone(),
    })
}

fn receipt_authentication_digest(
    key: &[u8],
    profile_digest: &ManifestDigest,
    payload_json: &str,
    bindings: &AcceptedProfileValidationBindingsV1,
) -> Result<ManifestDigest, SemanticAcceptedProfileAuthorityErrorV1> {
    let authenticated = serde_json::to_vec(&(
        VALIDATION_RECEIPT_DOMAIN,
        profile_digest,
        payload_json,
        bindings,
    ))
    .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key)
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
    mac.update(&authenticated);
    ManifestDigest::new(format!(
        "sha256:{}",
        hex::encode(mac.finalize().into_bytes())
    ))
    .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)
}

async fn read_validation_receipt_key(
    executor: &impl QueryExecutor,
) -> Result<Option<Zeroizing<Vec<u8>>>, SemanticAcceptedProfileAuthorityErrorV1> {
    let mut rows = executor
        .query(
            "SELECT key_material
             FROM configuration_semantic_accepted_profile_receipt_key_v1
             WHERE singleton = 1",
            (),
        )
        .await
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?
    else {
        return Ok(None);
    };
    let material = Zeroizing::new(
        row.get::<Vec<u8>>(0)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?,
    );
    if material.len() != VALIDATION_RECEIPT_KEY_BYTES
        || rows
            .next()
            .await
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?
            .is_some()
    {
        return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
    }
    Ok(Some(material))
}

async fn ensure_validation_receipt_key(
    executor: &impl Executor,
) -> Result<Zeroizing<Vec<u8>>, SemanticAcceptedProfileAuthorityErrorV1> {
    if let Some(material) = read_validation_receipt_key(executor).await? {
        return Ok(material);
    }
    let mut material = Zeroizing::new(vec![0_u8; VALIDATION_RECEIPT_KEY_BYTES]);
    getrandom::getrandom(material.as_mut_slice())
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
    executor
        .execute(
            "INSERT INTO configuration_semantic_accepted_profile_receipt_key_v1 (
                singleton, key_material
             ) VALUES (1, ?1)",
            params![material.as_slice()],
        )
        .await
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
    Ok(material)
}

fn manifest_digest(value: &str) -> Result<ManifestDigest, SemanticAcceptedProfileAuthorityErrorV1> {
    ManifestDigest::new(value.to_owned())
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)
}

impl SemanticEvaluationPublicationIdentityV1 {
    fn validate(
        &self,
        freshness_vector_digest: &ManifestDigest,
    ) -> Result<(), SemanticAcceptedProfileAuthorityErrorV1> {
        self.scope_digest
            .validate()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        self.code_generation
            .validate()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        self.code_source_manifest_digest
            .validate()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        self.code_snapshot_digest
            .validate()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        if &self.code_snapshot_digest != freshness_vector_digest
            || self
                .semantic_source_generation
                .as_ref()
                .is_some_and(|generation| generation.validate().is_err())
            || self
                .vector_generation_id
                .as_ref()
                .is_some_and(|generation| generation.validate().is_err())
        {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
        Ok(())
    }
}

fn validate_runtime_evidence(
    report: &DirectEvaluationReportV1,
    accepted_profile: &AcceptedRetrievalProfileV1,
    evaluated_profile_id: &str,
) -> Result<(), SemanticAcceptedProfileAuthorityErrorV1> {
    let outputs = report
        .raw_outputs
        .iter()
        .filter(|output| output.profile_id == evaluated_profile_id)
        .collect::<Vec<_>>();
    if outputs.is_empty() {
        return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
    }
    if let Some(semantic) = accepted_profile.compatibility().semantic.as_ref() {
        let measured = report
            .semantic_activation_resource_pins(evaluated_profile_id)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        if semantic.resources.model_bytes != measured.model_bytes
            || semantic.resources.tokenizer_bytes != measured.tokenizer_bytes
            || semantic.resources.resident_bytes != measured.resident_bytes
            || semantic.resources.threads != measured.threads
            || semantic.resources.max_concurrent_sessions != measured.max_concurrent_sessions
            || semantic.resources.batch_size != measured.batch_size
            || semantic.resources.sequence_length != measured.sequence_length
            || semantic.resources.load_deadline_ms != measured.load_deadline_ms
        {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
        for output in &outputs {
            let resources = output
                .native_resources
                .as_ref()
                .ok_or(SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
            for sample in resources.samples.values() {
                let SemanticNativeStageResultV1::Complete(sample) = sample else {
                    return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
                };
                if sample.provenance.vector_generation_id.as_deref()
                    != Some(semantic.vector_generation_id.as_digest().as_str())
                    || sample.provenance.artifact_digest.as_deref()
                        != Some(semantic.artifact_manifest_digest.as_str())
                {
                    return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
                }
            }
        }
    }
    if let Some(rerank) = accepted_profile.compatibility().rerank.as_ref() {
        for output in outputs {
            for query in &output.queries {
                let native = query
                    .native
                    .as_ref()
                    .ok_or(SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
                let SemanticNativeStageResultV1::Complete(execution) = &native.rerank.execution
                else {
                    return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
                };
                if execution.artifact_manifest_digest != rerank.artifact_manifest_digest {
                    return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn retained_bindings() -> AcceptedProfileValidationBindingsV1 {
        AcceptedProfileValidationBindingsV1 {
            report_digest: digest('1'),
            workload_digest: digest('2'),
            corpus_digest: digest('3'),
            fixture_source_repository_commit: "fixture-commit".to_owned(),
            fixture_source_repository_tree: "fixture-tree".to_owned(),
            raw_output_digest: digest('4'),
            profile_material_digests: BTreeMap::from([(
                "hybrid-conservative".to_owned(),
                digest('5'),
            )]),
            evaluated_profile_id: "hybrid-conservative".to_owned(),
            accepted_profile_digest: digest('6'),
            runtime_digest: digest('7'),
            publication_identity: SemanticEvaluationPublicationIdentityV1 {
                scope_digest: digest('8'),
                code_generation: CodeGenerationId::new("generation.receipt-test").unwrap(),
                code_source_manifest_digest: digest('9'),
                code_snapshot_digest: digest('a'),
                semantic_source_generation: None,
                vector_state_revision: None,
                vector_generation_id: None,
            },
            freshness_vector_digest: digest('a'),
        }
    }

    fn retained_receipt(
        bindings: AcceptedProfileValidationBindingsV1,
        key: &[u8],
        profile_digest: &ManifestDigest,
        payload_json: &str,
    ) -> AcceptedProfileValidationReceiptV1 {
        let authentication_digest =
            receipt_authentication_digest(key, profile_digest, payload_json, &bindings).unwrap();
        AcceptedProfileValidationReceiptV1 {
            schema_version: 1,
            bindings,
            authentication_digest,
        }
    }

    #[test]
    fn retained_receipt_resolution_survives_source_move_delete_and_change() {
        let key = [0x19; VALIDATION_RECEIPT_KEY_BYTES];
        let payload_json = r#"{"retained":"evaluation-evidence"}"#;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("publishing-worktree");
        let moved = directory.path().join("moved-worktree");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("fixture.rs"), "pub fn original() {}\n").unwrap();

        let bindings = retained_bindings();
        let profile_digest = bindings.accepted_profile_digest.clone();
        let receipt = retained_receipt(bindings, &key, &profile_digest, payload_json);
        let retained_json = serde_json::to_string(&receipt).unwrap();
        assert!(!retained_json.contains(source.to_string_lossy().as_ref()));
        receipt
            .verify_authenticity(&key, &profile_digest, payload_json)
            .unwrap();

        std::fs::rename(&source, &moved).unwrap();
        receipt
            .verify_authenticity(&key, &profile_digest, payload_json)
            .unwrap();
        std::fs::write(moved.join("fixture.rs"), "pub fn changed() {}\n").unwrap();
        receipt
            .verify_authenticity(&key, &profile_digest, payload_json)
            .unwrap();
        std::fs::remove_dir_all(&moved).unwrap();
        receipt
            .verify_authenticity(&key, &profile_digest, payload_json)
            .unwrap();
    }

    #[test]
    fn retained_receipt_rejects_wrong_secret_profile_payload_and_bindings() {
        let key = [0x2a; VALIDATION_RECEIPT_KEY_BYTES];
        let wrong_key = [0x3b; VALIDATION_RECEIPT_KEY_BYTES];
        let payload_json = r#"{"retained":"raw-evaluation-evidence"}"#;
        let bindings = retained_bindings();
        let profile_digest = bindings.accepted_profile_digest.clone();
        let receipt = retained_receipt(bindings, &key, &profile_digest, payload_json);

        let mut tampered_receipt = receipt.clone();
        tampered_receipt.authentication_digest = digest('c');
        assert_eq!(
            tampered_receipt.verify_authenticity(&key, &profile_digest, payload_json),
            Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected)
        );

        let mut tampered_bindings = receipt.clone();
        tampered_bindings.bindings.report_digest = digest('d');
        assert_eq!(
            tampered_bindings.verify_authenticity(&key, &profile_digest, payload_json),
            Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected)
        );

        assert_eq!(
            receipt.verify_authenticity(&wrong_key, &profile_digest, payload_json),
            Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected)
        );
        assert_eq!(
            receipt.verify_authenticity(&key, &digest('e'), payload_json),
            Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected)
        );
        assert_eq!(
            receipt.verify_authenticity(
                &key,
                &profile_digest,
                r#"{"retained":"changed-evaluation-evidence"}"#,
            ),
            Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected)
        );
    }

    #[tokio::test]
    async fn persisted_resolve_rejects_self_consistent_public_forgery_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = RegisteredGlobalDbTestRuntime::profile(directory.path().join("profile"))
            .await
            .unwrap();
        let database = runtime.profile_database_arc();
        let authority = RegisteredSemanticAcceptedProfileAuthorityV1::new(Arc::clone(&database));

        let transaction = database.begin_write_transaction().await.unwrap();
        let original_key = ensure_validation_receipt_key(&transaction).await.unwrap();
        let original_key = original_key.to_vec();
        transaction.commit().await.unwrap();

        let bindings = retained_bindings();
        let profile_digest = bindings.accepted_profile_digest.clone();
        let payload_json = r#"{"status":"PASS","raw_outputs":[]}"#.to_owned();
        let legacy_public_digest =
            canonical_sha256(&(VALIDATION_RECEIPT_DOMAIN, &bindings)).unwrap();
        let forged = StoredAcceptedProfileAuthorityEnvelopeV1 {
            schema_version: 1,
            payload_json,
            receipt: AcceptedProfileValidationReceiptV1 {
                schema_version: 1,
                bindings,
                authentication_digest: legacy_public_digest,
            },
        };
        let forged_json = serde_json::to_string(&forged).unwrap();
        database
            .writer_connection()
            .unwrap()
            .execute(
                "INSERT INTO configuration_semantic_accepted_profiles_v1 (
                    profile_digest, authority_json
                 ) VALUES (?1, ?2)",
                params![profile_digest.as_str(), forged_json],
            )
            .await
            .unwrap();
        drop(authority);
        drop(database);

        let remounted = runtime.remount_profile_database_for_test().await.unwrap();
        let snapshot = remounted.read_snapshot().await.unwrap();
        let remounted_key = read_validation_receipt_key(&snapshot)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(remounted_key.as_slice(), original_key.as_slice());
        drop(snapshot);

        let authority = RegisteredSemanticAcceptedProfileAuthorityV1::new(Arc::clone(&remounted));
        assert_eq!(
            authority.resolve_record(&profile_digest).await,
            Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected)
        );

        let transaction = remounted.begin_write_transaction().await.unwrap();
        assert!(
            transaction
                .execute(
                    "UPDATE configuration_semantic_accepted_profile_receipt_key_v1
                     SET key_material = ?1
                     WHERE singleton = 1",
                    params![vec![0x7c_u8; VALIDATION_RECEIPT_KEY_BYTES]],
                )
                .await
                .is_err()
        );
        transaction.rollback().await.unwrap();
    }
}
