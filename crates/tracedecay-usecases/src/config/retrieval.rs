//! Authenticated activation authority for immutable evaluated retrieval profiles.
//!
//! This owner performs no evaluation, model selection, or profile tuning. It
//! accepts caller-supplied typed values only after a real in-memory direct
//! evaluation result reports `pass`, then atomically compare-and-swaps the
//! active and rollback profiles under the configuration mutation capability.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use tracedecay_domain::configuration::{
    ConfigurationMutationEffectV1, ConfigurationMutationOperationV1, ConfigurationMutationSinkV1,
    ConfigurationRevisionId,
};
use tracedecay_domain::{
    ActorId, AdmittedEmbeddingProjectionKeyV1, ComponentRevision, DiversityPolicy, FusionProfile,
    FusionProfileId, ManifestDigest, RerankPolicy, RetrievalAnchorId, RetrievalBudget,
    RetrieverKind, SemanticSearchIndexKeyV1, UtcMicros, VectorGenerationIdV1, canonical_sha256,
};

use crate::configuration::{
    ConfigurationMutationAuthority, CurrentConfigurationMutationAuthorizationV1,
};
use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;
use tracedecay_search_eval::{
    DirectEvaluationReportV1, DirectEvaluationStatusV1, DirectProfileEvaluationV1,
};

const EVALUATION_ID_DOMAIN: &str = "tracedecay.retrieval.evaluation-pass.v1";
const PROFILE_ID_DOMAIN: &str = "tracedecay.retrieval.accepted-profile.v1";
const AUDIT_ID_DOMAIN: &str = "tracedecay.retrieval.profile-audit.v1";

/// A passing direct evaluation value. Private fields prevent paths, filenames,
/// or caller-authored status labels from being substituted for evaluated data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PassingRetrievalEvaluationV1 {
    report_digest: ManifestDigest,
    evaluation_anchor: RetrievalAnchorId,
    workload_digest: String,
    corpus_digest: String,
    evaluated_profile_id: String,
}

impl<'de> Deserialize<'de> for PassingRetrievalEvaluationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            report_digest: ManifestDigest,
            evaluation_anchor: RetrievalAnchorId,
            workload_digest: String,
            corpus_digest: String,
            evaluated_profile_id: String,
        }

        let raw = Raw::deserialize(deserializer)?;
        raw.report_digest
            .validate()
            .map_err(serde::de::Error::custom)?;
        raw.evaluation_anchor
            .validate()
            .map_err(serde::de::Error::custom)?;
        let expected_anchor =
            RetrievalAnchorId::new(format!("search-eval:{}", raw.report_digest.as_str()))
                .map_err(serde::de::Error::custom)?;
        if raw.evaluation_anchor != expected_anchor
            || raw.workload_digest.trim().is_empty()
            || raw.corpus_digest.trim().is_empty()
            || raw.evaluated_profile_id.trim().is_empty()
        {
            return Err(serde::de::Error::custom(
                "persisted passing retrieval evaluation is invalid",
            ));
        }
        Ok(Self {
            report_digest: raw.report_digest,
            evaluation_anchor: raw.evaluation_anchor,
            workload_digest: raw.workload_digest,
            corpus_digest: raw.corpus_digest,
            evaluated_profile_id: raw.evaluated_profile_id,
        })
    }
}

impl PassingRetrievalEvaluationV1 {
    pub fn from_report(
        report: &DirectEvaluationReportV1,
        evaluated_profile_id: &str,
    ) -> Result<Self, RetrievalProfileActivationErrorV1> {
        if report.status != DirectEvaluationStatusV1::Pass {
            return Err(RetrievalProfileActivationErrorV1::EvaluationDidNotPass);
        }
        let rows = report
            .profiles
            .iter()
            .filter(|row| row.profile_id == evaluated_profile_id)
            .collect::<Vec<_>>();
        validate_passing_profile_rows(&rows)?;
        let report_digest =
            canonical_sha256(&(EVALUATION_ID_DOMAIN, report)).map_err(contract_error)?;
        let evaluation_anchor =
            RetrievalAnchorId::new(format!("search-eval:{}", report_digest.as_str()))
                .map_err(contract_error)?;
        Ok(Self {
            report_digest,
            evaluation_anchor,
            workload_digest: report.workload_digest.clone(),
            corpus_digest: report.corpus_digest.clone(),
            evaluated_profile_id: evaluated_profile_id.to_owned(),
        })
    }

    pub fn evaluation_anchor(&self) -> &RetrievalAnchorId {
        &self.evaluation_anchor
    }

    pub fn report_digest(&self) -> &ManifestDigest {
        &self.report_digest
    }

    pub fn evaluated_profile_id(&self) -> &str {
        &self.evaluated_profile_id
    }
}

fn validate_passing_profile_rows(
    rows: &[&DirectProfileEvaluationV1],
) -> Result<(), RetrievalProfileActivationErrorV1> {
    if rows.is_empty()
        || rows
            .iter()
            .any(|row| row.status != DirectEvaluationStatusV1::Pass)
    {
        return Err(RetrievalProfileActivationErrorV1::EvaluationDidNotPass);
    }
    let partitions = rows
        .iter()
        .map(|row| row.partition.as_str())
        .collect::<BTreeSet<_>>();
    if partitions != BTreeSet::from(["train", "validation"]) {
        return Err(RetrievalProfileActivationErrorV1::EvaluationDidNotPass);
    }
    Ok(())
}

/// Resource requirements frozen into an evaluated semantic profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticResourceRequirementV1 {
    pub model_bytes: u64,
    pub tokenizer_bytes: u64,
    pub resident_bytes: u64,
    pub threads: u32,
    pub batch_size: u32,
    pub sequence_length: u32,
    pub load_deadline_ms: u64,
}

impl SemanticResourceRequirementV1 {
    fn valid(self) -> bool {
        self.model_bytes > 0
            && self.tokenizer_bytes > 0
            && self.resident_bytes >= self.model_bytes
            && self.resident_bytes >= self.tokenizer_bytes
            && self.threads > 0
            && self.batch_size > 0
            && self.sequence_length > 0
            && self.load_deadline_ms > 0
    }

    fn covered_by(self, ceiling: Self) -> bool {
        ceiling.model_bytes >= self.model_bytes
            && ceiling.tokenizer_bytes >= self.tokenizer_bytes
            && ceiling.resident_bytes >= self.resident_bytes
            && ceiling.threads >= self.threads
            && ceiling.batch_size >= self.batch_size
            && ceiling.sequence_length >= self.sequence_length
            && ceiling.load_deadline_ms >= self.load_deadline_ms
    }
}

/// Exact artifact, runtime, projection, and generation pins for semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticCompatibilityPinsV1 {
    pub implementation_revision: ComponentRevision,
    pub fusion_revision: ComponentRevision,
    pub artifact_manifest_digest: ManifestDigest,
    pub runtime_compatibility_digest: ManifestDigest,
    pub projection: AdmittedEmbeddingProjectionKeyV1,
    pub search_index_key: SemanticSearchIndexKeyV1,
    pub vector_generation_id: VectorGenerationIdV1,
    pub calibration: SemanticCalibrationProfileV1,
    pub resources: SemanticResourceRequirementV1,
}

impl SemanticCompatibilityPinsV1 {
    fn valid(&self) -> bool {
        self.implementation_revision.validate().is_ok()
            && self.fusion_revision.validate().is_ok()
            && self.artifact_manifest_digest.validate().is_ok()
            && self.runtime_compatibility_digest.validate().is_ok()
            && self.projection.embedding_key().validate().is_ok()
            && self.search_index_key.validate().is_ok()
            && self.projection.embedding_key().model_artifact_digest
                == self.artifact_manifest_digest
            && self.vector_generation_id.as_digest().validate().is_ok()
            && self.calibration.canonical_digest().is_ok()
            && self.calibration.projection_key == *self.projection.projection_key()
            && self.calibration.vector_generation == self.vector_generation_id
    }
}

/// Exact artifact and runtime pins for the optional bounded reranker. The
/// shape is owned by the semantic runtime crate, which mounts against it.
pub use tracedecay_semantic::RerankCompatibilityPinsV1;

/// All optional-stage compatibility selected by one evaluated profile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalCompatibilityPinsV1 {
    pub semantic: Option<SemanticCompatibilityPinsV1>,
    pub rerank: Option<RerankCompatibilityPinsV1>,
}

/// Runtime ceilings observed immediately before the atomic mutation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalRuntimeCompatibilityV1 {
    pub retrieval_ceiling: RetrievalBudget,
    pub semantic: Option<SemanticCompatibilityPinsV1>,
    pub semantic_ceiling: Option<SemanticResourceRequirementV1>,
    pub rerank: Option<RerankCompatibilityPinsV1>,
    pub rerank_ceiling: Option<RerankPolicy>,
}

/// Immutable executable profile admitted from one passing evaluation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AcceptedRetrievalProfileV1 {
    profile: FusionProfile,
    diversity: DiversityPolicy,
    rerank: Option<RerankPolicy>,
    compatibility: RetrievalCompatibilityPinsV1,
    evaluation: PassingRetrievalEvaluationV1,
    profile_digest: ManifestDigest,
}

impl<'de> Deserialize<'de> for AcceptedRetrievalProfileV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            profile: FusionProfile,
            diversity: DiversityPolicy,
            rerank: Option<RerankPolicy>,
            compatibility: RetrievalCompatibilityPinsV1,
            evaluation: PassingRetrievalEvaluationV1,
            profile_digest: ManifestDigest,
        }

        let raw = Raw::deserialize(deserializer)?;
        let accepted = Self {
            profile: raw.profile,
            diversity: raw.diversity,
            rerank: raw.rerank,
            compatibility: raw.compatibility,
            evaluation: raw.evaluation,
            profile_digest: raw.profile_digest,
        };
        accepted
            .validate_integrity()
            .map_err(serde::de::Error::custom)?;
        Ok(accepted)
    }
}

impl AcceptedRetrievalProfileV1 {
    pub fn new(
        profile: FusionProfile,
        diversity: DiversityPolicy,
        rerank: Option<RerankPolicy>,
        compatibility: RetrievalCompatibilityPinsV1,
        evaluation: PassingRetrievalEvaluationV1,
    ) -> Result<Self, RetrievalProfileActivationErrorV1> {
        let profile_digest = canonical_sha256(&(
            PROFILE_ID_DOMAIN,
            &profile,
            &diversity,
            &rerank,
            &compatibility,
            &evaluation,
        ))
        .map_err(contract_error)?;
        let accepted = Self {
            profile,
            diversity,
            rerank,
            compatibility,
            evaluation,
            profile_digest,
        };
        accepted.validate_fields()?;
        Ok(accepted)
    }

    pub fn profile(&self) -> &FusionProfile {
        &self.profile
    }

    pub fn diversity(&self) -> &DiversityPolicy {
        &self.diversity
    }

    pub fn rerank(&self) -> Option<&RerankPolicy> {
        self.rerank.as_ref()
    }

    pub fn compatibility(&self) -> &RetrievalCompatibilityPinsV1 {
        &self.compatibility
    }

    pub fn profile_digest(&self) -> &ManifestDigest {
        &self.profile_digest
    }

    pub fn evaluation(&self) -> &PassingRetrievalEvaluationV1 {
        &self.evaluation
    }

    pub(crate) fn is_exact_query_fallback(&self) -> bool {
        let expected = BTreeSet::from(RetrieverKind::QUERY_FALLBACK_LANES);
        self.profile
            .calibrations
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            == expected
            && self
                .profile
                .weights_micros
                .keys()
                .copied()
                .collect::<BTreeSet<_>>()
                == expected
            && self.profile.rerank_policy_id.is_none()
            && self.compatibility.semantic.is_none()
            && self.compatibility.rerank.is_none()
    }

    fn validate_integrity(&self) -> Result<(), RetrievalProfileActivationErrorV1> {
        self.validate_fields()?;
        if self.compute_digest()? != self.profile_digest {
            return Err(RetrievalProfileActivationErrorV1::TamperedProfile);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), RetrievalProfileActivationErrorV1> {
        self.profile
            .retrieval_budget
            .validate()
            .map_err(contract_error)?;
        if self.profile.profile_id.as_str()
            != format!("profile.{}", self.evaluation.evaluated_profile_id)
            || self.profile.evaluation_result_anchor != self.evaluation.evaluation_anchor
            || self.diversity.policy_id != self.profile.diversity_policy_id
            || self.diversity.evaluation_result_anchor.as_ref()
                != Some(&self.profile.evaluation_result_anchor)
        {
            return Err(RetrievalProfileActivationErrorV1::IncompatibleProfile);
        }
        let lanes = self
            .profile
            .calibrations
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        if lanes.is_empty()
            || lanes
                != self
                    .profile
                    .weights_micros
                    .keys()
                    .copied()
                    .collect::<BTreeSet<_>>()
            || !lanes.contains(&RetrieverKind::ExactLiteral)
            || !lanes.contains(&RetrieverKind::Lexical)
            || lanes.contains(&RetrieverKind::Semantic) != self.compatibility.semantic.is_some()
            || self.profile.rerank_policy_id.as_ref()
                != self.rerank.as_ref().map(|policy| &policy.policy_id)
            || self.rerank.is_some() != self.compatibility.rerank.is_some()
        {
            return Err(RetrievalProfileActivationErrorV1::IncompatibleProfile);
        }
        if let Some(pins) = self.compatibility.semantic.as_ref()
            && (!pins.resources.valid()
                || !pins.valid()
                || self.profile.calibrations.get(&RetrieverKind::Semantic)
                    != Some(&pins.calibration.calibration_profile_id))
        {
            return Err(RetrievalProfileActivationErrorV1::IncompatibleProfile);
        }
        if let Some(policy) = &self.rerank
            && (policy.evaluation_result_anchor != self.profile.evaluation_result_anchor
                || policy.max_candidates == 0
                || policy.max_input_bytes == 0
                || policy.max_input_tokens == 0
                || policy.max_work_units == 0
                || policy.max_model_invocations == 0)
        {
            return Err(RetrievalProfileActivationErrorV1::IncompatibleProfile);
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<ManifestDigest, RetrievalProfileActivationErrorV1> {
        canonical_sha256(&(
            PROFILE_ID_DOMAIN,
            &self.profile,
            &self.diversity,
            &self.rerank,
            &self.compatibility,
            &self.evaluation,
        ))
        .map_err(contract_error)
    }

    pub(crate) fn executable_under(
        &self,
        runtime: &RetrievalRuntimeCompatibilityV1,
    ) -> Result<(), RetrievalProfileActivationErrorV1> {
        self.validate_integrity()?;
        if !budget_covered(self.profile.retrieval_budget, runtime.retrieval_ceiling)
            || self.compatibility.semantic != runtime.semantic
            || self.compatibility.rerank != runtime.rerank
        {
            return Err(RetrievalProfileActivationErrorV1::RuntimeIncompatible);
        }
        match (
            self.compatibility.semantic.as_ref(),
            runtime.semantic_ceiling,
        ) {
            (Some(required), Some(ceiling)) if required.resources.covered_by(ceiling) => {}
            (None, None) => {}
            _ => return Err(RetrievalProfileActivationErrorV1::RuntimeCeilingTooLow),
        }
        match (&self.rerank, &runtime.rerank_ceiling) {
            (Some(required), Some(ceiling)) if rerank_covered(required, ceiling) => {}
            (None, None) => {}
            _ => return Err(RetrievalProfileActivationErrorV1::RuntimeCeilingTooLow),
        }
        Ok(())
    }
}

fn budget_covered(required: RetrievalBudget, ceiling: RetrievalBudget) -> bool {
    required.validate().is_ok()
        && ceiling.validate().is_ok()
        && ceiling.max_candidates_per_lane >= required.max_candidates_per_lane
        && ceiling.max_fused_candidates >= required.max_fused_candidates
        && ceiling.max_hydrated_results >= required.max_hydrated_results
        && ceiling.max_hydration_bytes >= required.max_hydration_bytes
        && match (required.deadline_micros, ceiling.deadline_micros) {
            (_, None) => true,
            (Some(required), Some(ceiling)) => ceiling >= required,
            (None, Some(_)) => false,
        }
}

fn rerank_covered(required: &RerankPolicy, ceiling: &RerankPolicy) -> bool {
    ceiling.max_candidates >= required.max_candidates
        && ceiling.max_input_bytes >= required.max_input_bytes
        && ceiling.max_input_tokens >= required.max_input_tokens
        && ceiling.max_work_units >= required.max_work_units
        && ceiling.max_model_invocations >= required.max_model_invocations
        && match (required.deadline_micros, ceiling.deadline_micros) {
            (_, None) => true,
            (Some(required), Some(ceiling)) => ceiling >= required,
            (None, Some(_)) => false,
        }
}

/// Result of the policy owner's current grant recheck. Construction is crate
/// restricted so transport callers cannot turn an actor label into authority.
#[derive(Clone, Debug)]
pub struct RetrievalProfileMutationCapabilityV1 {
    authority: ConfigurationMutationAuthority,
    current: CurrentConfigurationMutationAuthorizationV1,
}

impl RetrievalProfileMutationCapabilityV1 {
    pub(crate) fn from_current_authorization(
        authority: ConfigurationMutationAuthority,
        current: CurrentConfigurationMutationAuthorizationV1,
    ) -> Result<Self, RetrievalProfileActivationErrorV1> {
        authority
            .validate_integrity()
            .map_err(|_| RetrievalProfileActivationErrorV1::Unauthorized)?;
        if authority.receipt.scope_digest != current.scope_digest
            || authority.receipt.policy_epoch != current.policy_epoch
            || authority.receipt.policy_digest != current.policy_digest
        {
            return Err(RetrievalProfileActivationErrorV1::Unauthorized);
        }
        Ok(Self { authority, current })
    }

    pub(crate) fn authority(&self) -> &ConfigurationMutationAuthority {
        &self.authority
    }

    fn validate(
        &self,
        expected_revision: &ConfigurationRevisionId,
        now: UtcMicros,
    ) -> Result<&ActorId, RetrievalProfileActivationErrorV1> {
        let receipt = &self.authority.receipt;
        receipt
            .validate_for(
                &receipt.actor_id,
                ConfigurationMutationOperationV1::DirectMutation,
                &self.current.scope_digest,
                expected_revision,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
                now,
            )
            .map_err(|_| RetrievalProfileActivationErrorV1::Unauthorized)?;
        Ok(&receipt.actor_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalProfileCasV1 {
    pub expected_configuration_revision: ConfigurationRevisionId,
    pub expected_active_digest: ManifestDigest,
    pub expected_rollback_digest: Option<ManifestDigest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalProfileAuditOperationV1 {
    Activate,
    Rollback { trigger: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalProfileAuditEventV1 {
    pub event_id: ManifestDigest,
    pub actor_id: ActorId,
    pub operation: RetrievalProfileAuditOperationV1,
    pub prior_active_profile_id: FusionProfileId,
    pub resulting_active_profile_id: FusionProfileId,
    pub prior_active_digest: ManifestDigest,
    pub resulting_active_digest: ManifestDigest,
    pub evaluation_anchor: RetrievalAnchorId,
    pub freshness_vector_digest: ManifestDigest,
    pub base_revision: ConfigurationRevisionId,
    pub result_revision: ConfigurationRevisionId,
    pub occurred_at: UtcMicros,
}

struct RetrievalProfileCommitMetadataV1 {
    freshness_vector_digest: ManifestDigest,
    result_revision: ConfigurationRevisionId,
    now: UtcMicros,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetrievalProfileStateV1 {
    configuration_revision: ConfigurationRevisionId,
    active: AcceptedRetrievalProfileV1,
    rollback: Option<AcceptedRetrievalProfileV1>,
    audit: Vec<RetrievalProfileAuditEventV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RetrievalProfileStateSnapshotV1 {
    configuration_revision: ConfigurationRevisionId,
    active: AcceptedRetrievalProfileV1,
    rollback: Option<AcceptedRetrievalProfileV1>,
    audit: Vec<RetrievalProfileAuditEventV1>,
}

impl<'de> Deserialize<'de> for RetrievalProfileStateSnapshotV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            configuration_revision: ConfigurationRevisionId,
            active: AcceptedRetrievalProfileV1,
            rollback: Option<AcceptedRetrievalProfileV1>,
            audit: Vec<RetrievalProfileAuditEventV1>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let snapshot = Self {
            configuration_revision: raw.configuration_revision,
            active: raw.active,
            rollback: raw.rollback,
            audit: raw.audit,
        };
        snapshot
            .clone()
            .into_state()
            .map_err(serde::de::Error::custom)?;
        Ok(snapshot)
    }
}

impl RetrievalProfileStateSnapshotV1 {
    pub fn into_state(self) -> Result<RetrievalProfileStateV1, RetrievalProfileActivationErrorV1> {
        let state = RetrievalProfileStateV1 {
            configuration_revision: self.configuration_revision,
            active: self.active,
            rollback: self.rollback,
            audit: self.audit,
        };
        state.validate_persisted()?;
        Ok(state)
    }
}

impl RetrievalProfileStateV1 {
    pub fn new(
        configuration_revision: ConfigurationRevisionId,
        active: AcceptedRetrievalProfileV1,
        runtime: &RetrievalRuntimeCompatibilityV1,
    ) -> Result<Self, RetrievalProfileActivationErrorV1> {
        configuration_revision.validate().map_err(contract_error)?;
        active.executable_under(runtime)?;
        Ok(Self {
            configuration_revision,
            active,
            rollback: None,
            audit: Vec::new(),
        })
    }

    pub fn active(&self) -> &AcceptedRetrievalProfileV1 {
        &self.active
    }

    pub fn configuration_revision(&self) -> &ConfigurationRevisionId {
        &self.configuration_revision
    }

    pub fn rollback_profile(&self) -> Option<&AcceptedRetrievalProfileV1> {
        self.rollback.as_ref()
    }

    pub fn audit(&self) -> &[RetrievalProfileAuditEventV1] {
        &self.audit
    }

    pub fn snapshot(
        &self,
    ) -> Result<RetrievalProfileStateSnapshotV1, RetrievalProfileActivationErrorV1> {
        self.validate_persisted()?;
        Ok(RetrievalProfileStateSnapshotV1 {
            configuration_revision: self.configuration_revision.clone(),
            active: self.active.clone(),
            rollback: self.rollback.clone(),
            audit: self.audit.clone(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn activate(
        &mut self,
        capability: &RetrievalProfileMutationCapabilityV1,
        expected: &RetrievalProfileCasV1,
        candidate: AcceptedRetrievalProfileV1,
        current_runtime: &RetrievalRuntimeCompatibilityV1,
        candidate_runtime: &RetrievalRuntimeCompatibilityV1,
        freshness_vector_digest: ManifestDigest,
        result_revision: ConfigurationRevisionId,
        now: UtcMicros,
    ) -> Result<&RetrievalProfileAuditEventV1, RetrievalProfileActivationErrorV1> {
        self.validate_cas(expected)?;
        let actor = capability
            .validate(&expected.expected_configuration_revision, now)?
            .clone();
        self.active.executable_under(current_runtime)?;
        candidate.executable_under(candidate_runtime)?;
        let commit = RetrievalProfileCommitMetadataV1 {
            freshness_vector_digest,
            result_revision: result_revision.clone(),
            now,
        };
        let event = audit_event(
            actor,
            RetrievalProfileAuditOperationV1::Activate,
            &self.active,
            &candidate,
            self.configuration_revision.clone(),
            commit,
        )?;
        let prior = std::mem::replace(&mut self.active, candidate);
        self.rollback = Some(prior);
        self.configuration_revision = result_revision;
        self.audit.push(event);
        let index = self.audit.len() - 1;
        Ok(&self.audit[index])
    }

    pub fn rollback(
        &mut self,
        capability: &RetrievalProfileMutationCapabilityV1,
        expected: &RetrievalProfileCasV1,
        restored_runtime: &RetrievalRuntimeCompatibilityV1,
        trigger: String,
        freshness_vector_digest: ManifestDigest,
        result_revision: ConfigurationRevisionId,
        now: UtcMicros,
    ) -> Result<&RetrievalProfileAuditEventV1, RetrievalProfileActivationErrorV1> {
        self.validate_cas(expected)?;
        if trigger.trim().is_empty()
            || trigger.trim() != trigger
            || trigger.chars().any(char::is_control)
        {
            return Err(RetrievalProfileActivationErrorV1::InvalidRollbackTrigger);
        }
        let actor = capability
            .validate(&expected.expected_configuration_revision, now)?
            .clone();
        let restored = self
            .rollback
            .as_ref()
            .ok_or(RetrievalProfileActivationErrorV1::RollbackUnavailable)?;
        restored.executable_under(restored_runtime)?;
        let commit = RetrievalProfileCommitMetadataV1 {
            freshness_vector_digest,
            result_revision: result_revision.clone(),
            now,
        };
        let event = audit_event(
            actor,
            RetrievalProfileAuditOperationV1::Rollback { trigger },
            &self.active,
            restored,
            self.configuration_revision.clone(),
            commit,
        )?;
        let restored = self
            .rollback
            .take()
            .ok_or(RetrievalProfileActivationErrorV1::RollbackUnavailable)?;
        let failed = std::mem::replace(&mut self.active, restored);
        self.rollback = Some(failed);
        self.configuration_revision = result_revision;
        self.audit.push(event);
        let index = self.audit.len() - 1;
        Ok(&self.audit[index])
    }

    fn validate_cas(
        &self,
        expected: &RetrievalProfileCasV1,
    ) -> Result<(), RetrievalProfileActivationErrorV1> {
        self.active.validate_integrity()?;
        if expected.expected_configuration_revision != self.configuration_revision
            || expected.expected_active_digest != self.active.profile_digest
            || expected.expected_rollback_digest.as_ref()
                != self
                    .rollback
                    .as_ref()
                    .map(|profile| &profile.profile_digest)
        {
            return Err(RetrievalProfileActivationErrorV1::CasConflict);
        }
        if let Some(rollback) = &self.rollback {
            rollback.validate_integrity()?;
        }
        Ok(())
    }

    fn validate_persisted(&self) -> Result<(), RetrievalProfileActivationErrorV1> {
        self.configuration_revision
            .validate()
            .map_err(contract_error)?;
        self.active.validate_integrity()?;
        if let Some(rollback) = &self.rollback {
            rollback.validate_integrity()?;
        }
        if self.audit.is_empty() {
            if self.rollback.is_some() {
                return Err(RetrievalProfileActivationErrorV1::TamperedProfile);
            }
            return Ok(());
        }
        for event in &self.audit {
            validate_audit_event(event)?;
        }
        for pair in self.audit.windows(2) {
            if pair[1].base_revision != pair[0].result_revision
                || pair[1].prior_active_digest != pair[0].resulting_active_digest
            {
                return Err(RetrievalProfileActivationErrorV1::TamperedProfile);
            }
        }
        let last = self
            .audit
            .last()
            .ok_or(RetrievalProfileActivationErrorV1::TamperedProfile)?;
        if last.result_revision != self.configuration_revision
            || last.resulting_active_profile_id != self.active.profile.profile_id
            || last.resulting_active_digest != self.active.profile_digest
            || last.evaluation_anchor != self.active.profile.evaluation_result_anchor
            || self
                .rollback
                .as_ref()
                .map(|profile| (&profile.profile.profile_id, &profile.profile_digest))
                != Some((&last.prior_active_profile_id, &last.prior_active_digest))
        {
            return Err(RetrievalProfileActivationErrorV1::TamperedProfile);
        }
        Ok(())
    }
}

fn validate_audit_event(
    event: &RetrievalProfileAuditEventV1,
) -> Result<(), RetrievalProfileActivationErrorV1> {
    event.actor_id.validate().map_err(contract_error)?;
    event
        .prior_active_profile_id
        .validate()
        .map_err(contract_error)?;
    event
        .resulting_active_profile_id
        .validate()
        .map_err(contract_error)?;
    event
        .prior_active_digest
        .validate()
        .map_err(contract_error)?;
    event
        .resulting_active_digest
        .validate()
        .map_err(contract_error)?;
    event.evaluation_anchor.validate().map_err(contract_error)?;
    event
        .freshness_vector_digest
        .validate()
        .map_err(contract_error)?;
    event.base_revision.validate().map_err(contract_error)?;
    event.result_revision.validate().map_err(contract_error)?;
    if event.base_revision == event.result_revision {
        return Err(RetrievalProfileActivationErrorV1::StaleRevision);
    }
    let expected = canonical_sha256(&(
        AUDIT_ID_DOMAIN,
        &event.actor_id,
        &event.operation,
        &event.prior_active_profile_id,
        &event.resulting_active_profile_id,
        &event.prior_active_digest,
        &event.resulting_active_digest,
        &event.evaluation_anchor,
        &event.freshness_vector_digest,
        &event.base_revision,
        &event.result_revision,
        event.occurred_at,
    ))
    .map_err(contract_error)?;
    if expected != event.event_id {
        return Err(RetrievalProfileActivationErrorV1::TamperedProfile);
    }
    Ok(())
}

fn audit_event(
    actor_id: ActorId,
    operation: RetrievalProfileAuditOperationV1,
    prior: &AcceptedRetrievalProfileV1,
    resulting: &AcceptedRetrievalProfileV1,
    base_revision: ConfigurationRevisionId,
    commit: RetrievalProfileCommitMetadataV1,
) -> Result<RetrievalProfileAuditEventV1, RetrievalProfileActivationErrorV1> {
    if commit.result_revision == base_revision {
        return Err(RetrievalProfileActivationErrorV1::StaleRevision);
    }
    commit
        .freshness_vector_digest
        .validate()
        .map_err(contract_error)?;
    let event_id = canonical_sha256(&(
        AUDIT_ID_DOMAIN,
        &actor_id,
        &operation,
        &prior.profile.profile_id,
        &resulting.profile.profile_id,
        &prior.profile_digest,
        &resulting.profile_digest,
        &resulting.profile.evaluation_result_anchor,
        &commit.freshness_vector_digest,
        &base_revision,
        &commit.result_revision,
        commit.now,
    ))
    .map_err(contract_error)?;
    Ok(RetrievalProfileAuditEventV1 {
        event_id,
        actor_id,
        operation,
        prior_active_profile_id: prior.profile.profile_id.clone(),
        resulting_active_profile_id: resulting.profile.profile_id.clone(),
        prior_active_digest: prior.profile_digest.clone(),
        resulting_active_digest: resulting.profile_digest.clone(),
        evaluation_anchor: resulting.profile.evaluation_result_anchor.clone(),
        freshness_vector_digest: commit.freshness_vector_digest,
        base_revision,
        result_revision: commit.result_revision,
        occurred_at: commit.now,
    })
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RetrievalProfileActivationErrorV1 {
    #[error("retrieval evaluation did not pass")]
    EvaluationDidNotPass,
    #[error("retrieval profile is incompatible with its evaluated values")]
    IncompatibleProfile,
    #[error("retrieval profile integrity check failed")]
    TamperedProfile,
    #[error("configuration mutation capability is unauthorized or stale")]
    Unauthorized,
    #[error("retrieval profile compare-and-swap conflicted")]
    CasConflict,
    #[error("runtime artifact or projection compatibility does not match")]
    RuntimeIncompatible,
    #[error("runtime ceiling is below the evaluated profile requirement")]
    RuntimeCeilingTooLow,
    #[error("rollback profile is unavailable")]
    RollbackUnavailable,
    #[error("rollback trigger is invalid")]
    InvalidRollbackTrigger,
    #[error("result configuration revision is stale")]
    StaleRevision,
    #[error("retrieval activation contract is invalid: {0}")]
    Contract(String),
}

fn contract_error(error: impl std::fmt::Display) -> RetrievalProfileActivationErrorV1 {
    RetrievalProfileActivationErrorV1::Contract(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_search_eval::{
        DirectProfileEvaluationV1, DirectQualityMetricsV1, DirectRatioMetricV1,
        OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
    };

    fn report(status: DirectEvaluationStatusV1) -> DirectEvaluationReportV1 {
        let empty_ratio = || DirectRatioMetricV1 {
            numerator: 0,
            denominator: 0,
            ppm: 0,
        };
        let row = |partition: &str| DirectProfileEvaluationV1 {
            profile_id: "profile-v1".to_owned(),
            partition: partition.to_owned(),
            query_count: 0,
            failed_queries: 0,
            fallback_stable: true,
            fallback_matches_expected: true,
            cancellation_bounded: true,
            offline: true,
            resource_status: DirectEvaluationStatusV1::Pass,
            optional_stages: OptionalStageMeasurementsV1 {
                semantic: OptionalStageMeasurementV1::NotRequested,
                rerank: OptionalStageMeasurementV1::NotRequested,
            },
            quality: DirectQualityMetricsV1 {
                relevant_query_count: 0,
                recall_at_10: empty_ratio(),
                precision_at_10: empty_ratio(),
                mean_reciprocal_rank_ppm: 0,
                ndcg_at_10_ppm: 0,
                duplicate_rate: empty_ratio(),
                protected_recall_at_10: empty_ratio(),
                strata: Vec::new(),
                worst_stratum: None,
            },
            status,
            queries: Vec::new(),
        };
        DirectEvaluationReportV1 {
            command: "compare".to_owned(),
            status,
            workload_digest: "workload".to_owned(),
            corpus_digest: "corpus".to_owned(),
            fixture_source_repository_commit: "commit".to_owned(),
            fixture_source_repository_tree: "tree".to_owned(),
            profiles: vec![row("train"), row("validation")],
        }
    }

    #[test]
    fn only_a_passing_result_value_becomes_activation_evidence() {
        assert!(
            PassingRetrievalEvaluationV1::from_report(
                &report(DirectEvaluationStatusV1::Pass),
                "profile-v1",
            )
            .is_ok()
        );
        assert_eq!(
            PassingRetrievalEvaluationV1::from_report(
                &report(DirectEvaluationStatusV1::Pending),
                "profile-v1",
            ),
            Err(RetrievalProfileActivationErrorV1::EvaluationDidNotPass)
        );
    }

    #[test]
    fn runtime_ceiling_cannot_bind_below_evaluated_budget() {
        let required = RetrievalBudget {
            max_candidates_per_lane: 10,
            max_fused_candidates: 20,
            max_hydrated_results: 5,
            max_hydration_bytes: 1_000,
            deadline_micros: Some(100),
        };
        let mut ceiling = required;
        ceiling.max_hydration_bytes = 999;
        assert!(!budget_covered(required, ceiling));
    }
}
