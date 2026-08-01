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
use tracedecay_search_eval::DirectEvaluationReportV1;

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
    pub async fn publish(
        &self,
        report: DirectEvaluationReportV1,
        accepted_profile: AcceptedRetrievalProfileV1,
        runtime: RetrievalRuntimeCompatibilityV1,
        freshness_vector_digest: ManifestDigest,
    ) -> Result<(), SemanticAcceptedProfileAuthorityErrorV1> {
        let evaluation = PassingRetrievalEvaluationV1::from_report(
            &report,
            accepted_profile.evaluation().evaluated_profile_id(),
        )
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        if &evaluation != accepted_profile.evaluation() {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
        accepted_profile
            .executable_under(&runtime)
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        freshness_vector_digest
            .validate()
            .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        let stored = StoredAcceptedProfileAuthorityV1 {
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
        let evaluation = PassingRetrievalEvaluationV1::from_report(
            &self.report,
            self.accepted_profile.evaluation().evaluated_profile_id(),
        )
        .map_err(|_| SemanticAcceptedProfileAuthorityErrorV1::Rejected)?;
        if &evaluation != self.accepted_profile.evaluation() {
            return Err(SemanticAcceptedProfileAuthorityErrorV1::Rejected);
        }
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
