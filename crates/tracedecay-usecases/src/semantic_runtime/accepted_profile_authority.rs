use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::ManifestDigest;

use super::SemanticRuntimeFuture;
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, PassingRetrievalEvaluationV1, RetrievalRuntimeCompatibilityV1,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_search_eval::semantic_native::SemanticNativeStageResultV1;
use tracedecay_search_eval::{
    CandidateWorkloadV1, DirectEvaluationReportV1, direct_evaluated_profile_material,
};

const ACTIVATION_WORKLOAD_JSON: &str = include_str!(
    "../../../../tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json"
);

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS configuration_semantic_accepted_profiles_v1 (
    profile_digest TEXT PRIMARY KEY NOT NULL,
    authority_json TEXT NOT NULL
);";

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
    pub async fn open(
        database: Arc<RegisteredGlobalDb>,
    ) -> Result<Self, SemanticAcceptedProfileAuthorityErrorV1> {
        let authority = Self { database };
        authority.ensure_schema().await?;
        Ok(authority)
    }

    /// Persists only a profile whose private evaluation value can be
    /// reconstructed from this real direct-evaluator report.
    pub(super) async fn publish(
        &self,
        evaluation_repository_root: &Path,
        report: DirectEvaluationReportV1,
        accepted_profile: AcceptedRetrievalProfileV1,
        runtime: RetrievalRuntimeCompatibilityV1,
        freshness_vector_digest: ManifestDigest,
    ) -> Result<(), SemanticAcceptedProfileAuthorityErrorV1> {
        let evaluation_repository_root = evaluation_repository_root
            .canonicalize()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
        validate_report_authority(&evaluation_repository_root, &report, &accepted_profile)?;
        accepted_profile
            .executable_under(&runtime)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        freshness_vector_digest
            .validate()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        let stored = StoredAcceptedProfileAuthorityV1 {
            evaluation_repository_root,
            report,
            accepted_profile: accepted_profile.clone(),
            runtime,
            freshness_vector_digest,
        };
        let json = serde_json::to_string(&stored)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        self.ensure_schema().await?;
        let transaction = self
            .database
            .begin_write_transaction()
            .await
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
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

    async fn ensure_schema(&self) -> Result<(), SemanticAcceptedProfileAuthorityErrorV1> {
        self.database
            .writer_connection()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?
            .execute_batch(SCHEMA)
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
        self.ensure_schema().await?;
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
        let stored: StoredAcceptedProfileAuthorityV1 = serde_json::from_str(&json)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        stored.validate(profile_digest)
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
struct StoredAcceptedProfileAuthorityV1 {
    evaluation_repository_root: PathBuf,
    report: DirectEvaluationReportV1,
    accepted_profile: AcceptedRetrievalProfileV1,
    runtime: RetrievalRuntimeCompatibilityV1,
    freshness_vector_digest: ManifestDigest,
}

impl StoredAcceptedProfileAuthorityV1 {
    fn validate(
        self,
        expected_digest: &ManifestDigest,
    ) -> Result<SemanticAcceptedProfileAuthorityRecordV1, SemanticAcceptedProfileAuthorityErrorV1>
    {
        if self.accepted_profile.profile_digest() != expected_digest {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
        let evaluation_repository_root = self
            .evaluation_repository_root
            .canonicalize()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Unavailable)?;
        if evaluation_repository_root != self.evaluation_repository_root {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
        validate_report_authority(
            &evaluation_repository_root,
            &self.report,
            &self.accepted_profile,
        )?;
        self.accepted_profile
            .executable_under(&self.runtime)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        self.freshness_vector_digest
            .validate()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        Ok(SemanticAcceptedProfileAuthorityRecordV1 {
            accepted_profile: self.accepted_profile,
            runtime: self.runtime,
            freshness_vector_digest: self.freshness_vector_digest,
        })
    }
}

fn validate_report_authority(
    evaluation_repository_root: &Path,
    report: &DirectEvaluationReportV1,
    accepted_profile: &AcceptedRetrievalProfileV1,
) -> Result<(), SemanticAcceptedProfileAuthorityErrorV1> {
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
    validate_runtime_evidence(report, accepted_profile, evaluated_profile_id)
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
