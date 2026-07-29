//! Daemon-owned activation state and process-local key authority for PR9.
//!
//! This provider never chooses retrieval weights, calibration, diversity, or
//! evaluation identity. It exposes only the exact profile already accepted by
//! [`RetrievalProfileStateV1`] after a successful configuration activation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, RwLock};

use thiserror::Error;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    ComponentRevision, ManifestDigest, PrivacyDomainId, RetrievalAnchorId, RetrievalCursorKeyId,
    RetrieverKind,
};
use zeroize::Zeroizing;

use super::code_index_scheduler::pr9_runtime::{
    AcceptedPr9EvaluationV1, Pr9AuthorityMaterialV1, Pr9AuthorityProviderErrorV1,
    Pr9AuthorityProviderV1,
};
use crate::application::semantic_runtime::{
    CommittedRetrievalProfileStateV1, RetrievalProfileActivationObserverErrorV1,
    RetrievalProfileActivationObserverV1, SemanticRuntimeFuture,
    register_project_semantic_redundancy_authority,
    unregister_project_semantic_redundancy_authority,
};
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, RetrievalProfileAuditOperationV1, RetrievalProfileStateV1,
};
use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;

const PR9_KEY_BYTES: usize = 32;
const PR9_KEY_ID_BYTES: usize = 16;
const PR9_KEY_EPOCH: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pr9AuthorityUnavailableReasonV1 {
    ActivationUnavailable,
    ActivationNotCurrent,
    ScopeRequired,
    ScopeMismatch,
    KeyUnavailable,
    InvalidActivatedProfile,
    AmbiguousActivatedProfile,
}

impl Pr9AuthorityUnavailableReasonV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::ActivationUnavailable => "activation_unavailable",
            Self::ActivationNotCurrent => "activation_not_current",
            Self::ScopeRequired => "scope_required",
            Self::ScopeMismatch => "scope_mismatch",
            Self::KeyUnavailable => "key_unavailable",
            Self::InvalidActivatedProfile => "invalid_activated_profile",
            Self::AmbiguousActivatedProfile => "ambiguous_activated_profile",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pr9AuthorityProviderStatusV1 {
    Available {
        scope_digest: ManifestDigest,
        profile_id: tracedecay_domain::FusionProfileId,
        evaluation_anchor: RetrievalAnchorId,
    },
    Unavailable {
        reason: Pr9AuthorityUnavailableReasonV1,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum Pr9AuthorityUpdateErrorV1 {
    #[error("PR9 activated scope is invalid")]
    InvalidScope,
    #[error("PR9 initial profile state is not the exact evaluated fallback")]
    InvalidInitialState,
    #[error("PR9 profile state does not contain a successful current activation")]
    ActivationNotCurrent,
    #[error("PR9 activation does not match the provider's exact current scope")]
    ScopeMismatch,
    #[error("PR9 activation compare-and-swap state is stale")]
    CasConflict,
}

struct ProcessLocalPr9KeyV1 {
    key_id: RetrievalCursorKeyId,
    secret: Zeroizing<Vec<u8>>,
}

impl ProcessLocalPr9KeyV1 {
    fn generate() -> Option<Self> {
        let mut random = Zeroizing::new(vec![0_u8; PR9_KEY_ID_BYTES + PR9_KEY_BYTES]);
        getrandom::getrandom(random.as_mut_slice()).ok()?;
        let key_id = RetrievalCursorKeyId::new(format!(
            "retrieval-key.pr9.{}",
            hex::encode(&random[..PR9_KEY_ID_BYTES])
        ))
        .ok()?;
        Some(Self {
            key_id,
            secret: Zeroizing::new(random[PR9_KEY_ID_BYTES..].to_vec()),
        })
    }

    fn keyring(&self, privacy_domain: PrivacyDomainId) -> Option<RetrievalCursorKeyringV1> {
        RetrievalCursorKeyringV1::new(
            privacy_domain,
            self.key_id.clone(),
            PR9_KEY_EPOCH,
            self.secret.to_vec(),
            tracedecay_query::retrieval::PR9_CURSOR_TTL_MICROS_V1,
        )
        .ok()
    }
}

impl fmt::Debug for ProcessLocalPr9KeyV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessLocalPr9KeyV1")
            .field("key_id", &self.key_id)
            .field("key_material", &"REDACTED")
            .finish()
    }
}

#[derive(Clone)]
struct ActivatedPr9StateV1 {
    scope: ResolvedScope,
    state: RetrievalProfileStateV1,
}

/// Daemon-generation owner for the current accepted PR9 profile and its
/// process-local query/cursor key.
#[derive(Clone)]
pub(crate) struct DaemonPr9AuthorityProviderV1 {
    activated: Arc<RwLock<BTreeMap<ManifestDigest, ActivatedPr9StateV1>>>,
    key: Option<Arc<ProcessLocalPr9KeyV1>>,
}

#[derive(Clone)]
pub(crate) struct DaemonPr9ActivationRegistrarV1 {
    provider: DaemonPr9AuthorityProviderV1,
    registry: super::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: std::path::PathBuf,
}

impl DaemonPr9ActivationRegistrarV1 {
    pub(crate) fn new(
        provider: DaemonPr9AuthorityProviderV1,
        registry: super::code_index_scheduler::CodeIndexSchedulerRegistryV1,
        project_root: std::path::PathBuf,
    ) -> Self {
        Self {
            provider,
            registry,
            project_root,
        }
    }
}

impl RetrievalProfileActivationObserverV1 for DaemonPr9ActivationRegistrarV1 {
    fn activation_committed(
        &self,
        committed: CommittedRetrievalProfileStateV1,
    ) -> SemanticRuntimeFuture<'_, Result<(), RetrievalProfileActivationObserverErrorV1>> {
        let provider = self.provider.clone();
        let registry = self.registry.clone();
        let project_root = self.project_root.clone();
        Box::pin(async move {
            let scope = committed.scope.clone();
            let semantic_enabled = committed.state.active().compatibility().semantic.is_some();
            unregister_project_semantic_redundancy_authority(&project_root);
            provider
                .update_after_successful_activation(scope.clone(), committed.state.clone())
                .map_err(map_update_observer_error)?;
            registry
                .clear_semantic_query_authority(&scope)
                .await
                .map_err(|_| RetrievalProfileActivationObserverErrorV1::Conflict)?;
            registry
                .clear_pr9_query_authority(&scope)
                .await
                .map_err(|_| RetrievalProfileActivationObserverErrorV1::Conflict)?;
            super::code_index_scheduler::pr9_runtime::mount_pr9_query_authority_on_project_open(
                &registry,
                &project_root,
                &scope,
                &provider,
            )
            .await
            .map_err(|_| RetrievalProfileActivationObserverErrorV1::Unavailable)?;
            if semantic_enabled {
                registry
                    .mount_semantic_query_authority_from_committed(
                        &project_root,
                        &scope,
                        committed.clone(),
                    )
                    .await
                    .map_err(|_| RetrievalProfileActivationObserverErrorV1::Unavailable)?;
                let _ = register_project_semantic_redundancy_authority(
                    project_root.clone(),
                    &committed,
                );
            }
            Ok(())
        })
    }
}

impl Default for DaemonPr9AuthorityProviderV1 {
    fn default() -> Self {
        Self {
            activated: Arc::new(RwLock::new(BTreeMap::new())),
            key: ProcessLocalPr9KeyV1::generate().map(Arc::new),
        }
    }
}

impl fmt::Debug for DaemonPr9AuthorityProviderV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DaemonPr9AuthorityProviderV1")
            .field(
                "activated_scope_count",
                &self
                    .activated
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len(),
            )
            .field("key_material", &"REDACTED")
            .finish()
    }
}

impl DaemonPr9AuthorityProviderV1 {
    /// Restore the evaluated fallback installed as the configuration store's
    /// initial state. Initial installation has no mutation audit event, so it
    /// is admitted only while the exact PR9 profile is active with no rollback
    /// slot or audit history.
    pub(crate) fn install_evaluated_initial_state(
        &self,
        scope: ResolvedScope,
        initial: RetrievalProfileStateV1,
    ) -> Result<Pr9AuthorityProviderStatusV1, Pr9AuthorityUpdateErrorV1> {
        scope
            .validate()
            .map_err(|_| Pr9AuthorityUpdateErrorV1::InvalidScope)?;
        if !initial.audit().is_empty()
            || initial.rollback_profile().is_some()
            || exact_pr9_profile(&initial).is_err()
        {
            return Err(Pr9AuthorityUpdateErrorV1::InvalidInitialState);
        }
        let mut current = self
            .activated
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(prior) = current.get(&scope.scope_digest) {
            if prior.scope != scope {
                return Err(Pr9AuthorityUpdateErrorV1::ScopeMismatch);
            }
            if prior.state != initial {
                return Err(Pr9AuthorityUpdateErrorV1::CasConflict);
            }
        } else {
            current.insert(
                scope.scope_digest.clone(),
                ActivatedPr9StateV1 {
                    scope: scope.clone(),
                    state: initial,
                },
            );
        }
        drop(current);
        Ok(self.status(Some(&scope)))
    }

    /// Publish a state only after its configuration activation succeeded.
    ///
    /// Subsequent publications are compare-and-swapped against the previous
    /// active profile digest and exact scope captured by the activation event.
    pub(crate) fn update_after_successful_activation(
        &self,
        scope: ResolvedScope,
        activated: RetrievalProfileStateV1,
    ) -> Result<Pr9AuthorityProviderStatusV1, Pr9AuthorityUpdateErrorV1> {
        scope
            .validate()
            .map_err(|_| Pr9AuthorityUpdateErrorV1::InvalidScope)?;
        let event = current_transition(&activated)
            .ok_or(Pr9AuthorityUpdateErrorV1::ActivationNotCurrent)?;
        let mut current = self
            .activated
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(prior) = current.get(&scope.scope_digest) {
            if prior.scope != scope {
                return Err(Pr9AuthorityUpdateErrorV1::ScopeMismatch);
            }
            if prior.state.active().profile_digest() != &event.prior_active_digest {
                return Err(Pr9AuthorityUpdateErrorV1::CasConflict);
            }
        }
        current.insert(
            scope.scope_digest.clone(),
            ActivatedPr9StateV1 {
                scope: scope.clone(),
                state: activated,
            },
        );
        drop(current);
        Ok(self.status(Some(&scope)))
    }

    pub(crate) fn status(&self, scope: Option<&ResolvedScope>) -> Pr9AuthorityProviderStatusV1 {
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(scope) = scope else {
            return if current.is_empty() {
                unavailable(Pr9AuthorityUnavailableReasonV1::ActivationUnavailable)
            } else {
                unavailable(Pr9AuthorityUnavailableReasonV1::ScopeRequired)
            };
        };
        let Some(activated) = current.get(&scope.scope_digest) else {
            return unavailable(Pr9AuthorityUnavailableReasonV1::ActivationUnavailable);
        };
        if scope != &activated.scope {
            return unavailable(Pr9AuthorityUnavailableReasonV1::ScopeMismatch);
        }
        if self.key.is_none() {
            return unavailable(Pr9AuthorityUnavailableReasonV1::KeyUnavailable);
        }
        if !has_current_pr9_authority(&activated.state) {
            return unavailable(Pr9AuthorityUnavailableReasonV1::ActivationNotCurrent);
        }
        let profile = match exact_pr9_profile(&activated.state) {
            Ok(profile) => profile,
            Err(reason) => return unavailable(reason),
        };
        Pr9AuthorityProviderStatusV1::Available {
            scope_digest: activated.scope.scope_digest.clone(),
            profile_id: profile.profile().profile_id.clone(),
            evaluation_anchor: profile.profile().evaluation_result_anchor.clone(),
        }
    }

    fn material_for(
        &self,
        scope: &ResolvedScope,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<Pr9AuthorityMaterialV1, Pr9AuthorityUnavailableReasonV1> {
        match self.status(Some(scope)) {
            Pr9AuthorityProviderStatusV1::Available { .. } => {}
            Pr9AuthorityProviderStatusV1::Unavailable { reason } => return Err(reason),
        }
        let current = self
            .activated
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let activated = current
            .get(&scope.scope_digest)
            .ok_or(Pr9AuthorityUnavailableReasonV1::ActivationUnavailable)?;
        if &activated.scope != scope {
            return Err(Pr9AuthorityUnavailableReasonV1::ScopeMismatch);
        }
        if !has_current_pr9_authority(&activated.state) {
            return Err(Pr9AuthorityUnavailableReasonV1::ActivationNotCurrent);
        }
        let pr9 = exact_pr9_profile(&activated.state)?;
        let ranking_revision =
            ComponentRevision::new(tracedecay_query::retrieval::PR9_RANKING_REVISION_V1)
                .map_err(|_| Pr9AuthorityUnavailableReasonV1::InvalidActivatedProfile)?;
        let keyring = self
            .key
            .as_ref()
            .and_then(|key| key.keyring(privacy_domain.clone()))
            .ok_or(Pr9AuthorityUnavailableReasonV1::KeyUnavailable)?;
        Ok(Pr9AuthorityMaterialV1 {
            scope: activated.scope.clone(),
            evaluation: AcceptedPr9EvaluationV1 {
                status: crate::search_eval::DirectEvaluationStatusV1::Pass,
                scope_digest: activated.scope.scope_digest.clone(),
                profile_id: pr9.profile().profile_id.clone(),
                evaluation_result_anchor: pr9.profile().evaluation_result_anchor.clone(),
            },
            profile: pr9.profile().clone(),
            diversity: pr9.diversity().clone(),
            ranking_revision,
            keyring: Some(keyring),
        })
    }
}

impl Pr9AuthorityProviderV1 for DaemonPr9AuthorityProviderV1 {
    fn accepted_authorities(
        &self,
        scope: &ResolvedScope,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<Vec<Pr9AuthorityMaterialV1>, Pr9AuthorityProviderErrorV1> {
        self.material_for(scope, privacy_domain)
            .map(|material| vec![material])
            .map_err(|reason| Pr9AuthorityProviderErrorV1::Unavailable(reason.as_str().to_owned()))
    }
}

fn current_transition(
    state: &RetrievalProfileStateV1,
) -> Option<&crate::config::retrieval::RetrievalProfileAuditEventV1> {
    let event = state.audit().last()?;
    if !matches!(
        &event.operation,
        RetrievalProfileAuditOperationV1::Activate
            | RetrievalProfileAuditOperationV1::Rollback { .. }
    ) || event.resulting_active_profile_id.as_str()
        != state.active().profile().profile_id.as_str()
        || event.resulting_active_digest.as_str() != state.active().profile_digest().as_str()
        || event.evaluation_anchor.as_str()
            != state.active().profile().evaluation_result_anchor.as_str()
    {
        return None;
    }
    Some(event)
}

fn has_current_pr9_authority(state: &RetrievalProfileStateV1) -> bool {
    current_transition(state).is_some()
        || (state.audit().is_empty()
            && state.rollback_profile().is_none()
            && exact_pr9_profile(state).is_ok())
}

fn exact_pr9_profile(
    state: &RetrievalProfileStateV1,
) -> Result<&AcceptedRetrievalProfileV1, Pr9AuthorityUnavailableReasonV1> {
    exact_pr9_profile_from_slots(state.active(), state.rollback_profile())
}

fn exact_pr9_profile_from_slots<'a>(
    active: &'a AcceptedRetrievalProfileV1,
    rollback: Option<&'a AcceptedRetrievalProfileV1>,
) -> Result<&'a AcceptedRetrievalProfileV1, Pr9AuthorityUnavailableReasonV1> {
    let mut matches = [Some(active), rollback]
        .into_iter()
        .flatten()
        .filter(|profile| is_exact_pr9_profile(profile));
    let profile = matches
        .next()
        .ok_or(Pr9AuthorityUnavailableReasonV1::InvalidActivatedProfile)?;
    if matches.next().is_some() {
        return Err(Pr9AuthorityUnavailableReasonV1::AmbiguousActivatedProfile);
    }
    Ok(profile)
}

fn is_exact_pr9_profile(active: &AcceptedRetrievalProfileV1) -> bool {
    let profile = active.profile();
    let expected = BTreeSet::from(RetrieverKind::PR9_FALLBACK_LANES);
    profile
        .calibrations
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        == expected
        && profile
            .weights_micros
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            == expected
        && profile.rerank_policy_id.is_none()
        && active.compatibility().semantic.is_none()
        && active.compatibility().rerank.is_none()
}

fn unavailable(reason: Pr9AuthorityUnavailableReasonV1) -> Pr9AuthorityProviderStatusV1 {
    Pr9AuthorityProviderStatusV1::Unavailable { reason }
}

fn map_update_observer_error(
    error: Pr9AuthorityUpdateErrorV1,
) -> RetrievalProfileActivationObserverErrorV1 {
    match error {
        Pr9AuthorityUpdateErrorV1::InvalidScope
        | Pr9AuthorityUpdateErrorV1::InvalidInitialState
        | Pr9AuthorityUpdateErrorV1::ActivationNotCurrent => {
            RetrievalProfileActivationObserverErrorV1::Rejected
        }
        Pr9AuthorityUpdateErrorV1::ScopeMismatch | Pr9AuthorityUpdateErrorV1::CasConflict => {
            RetrievalProfileActivationObserverErrorV1::Conflict
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::application::semantic_runtime::{
        CommittedRetrievalProfileStateV1, SemanticActivationCommandV1, SemanticActivationReceiptV1,
        SemanticActivationRequestV1, SemanticConfigurationPinV1, SemanticCurrentLinkedActivationV1,
    };
    use crate::config::retrieval::{
        PassingRetrievalEvaluationV1, RetrievalCompatibilityPinsV1, RetrievalProfileAuditEventV1,
        RetrievalProfileStateSnapshotV1, RetrievalRuntimeCompatibilityV1,
        SemanticCompatibilityPinsV1, SemanticResourceRequirementV1,
    };
    use crate::search_eval::{
        DirectEvaluationReportV1, DirectEvaluationStatusV1, DirectProfileEvaluationV1,
        DirectQualityMetricsV1, DirectRatioMetricV1, OptionalStageMeasurementV1,
        OptionalStageMeasurementsV1,
    };
    use std::{path::Path, process::Command};
    use tempfile::TempDir;
    use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
    use tracedecay_domain::{
        CalibrationProfileId, ChunkerRevision, ComponentRevision, DiversityPolicy,
        EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1,
        EmbeddingPrecisionV1, EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, FusionProfile,
        ManifestDigest, ProjectId, RetrievalBudget, UtcMicros, VectorGenerationIdV1,
        canonical_sha256,
    };
    use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture id")
    }

    fn passing_report(evaluated_profile_id: &str) -> DirectEvaluationReportV1 {
        let empty_ratio = || DirectRatioMetricV1 {
            numerator: 0,
            denominator: 0,
            ppm: 0,
        };
        let row = |partition: &str| DirectProfileEvaluationV1 {
            profile_id: evaluated_profile_id.to_owned(),
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
            status: DirectEvaluationStatusV1::Pass,
            queries: Vec::new(),
        };
        DirectEvaluationReportV1 {
            command: "compare".to_owned(),
            status: DirectEvaluationStatusV1::Pass,
            workload_digest: "workload".to_owned(),
            corpus_digest: "corpus".to_owned(),
            fixture_source_repository_commit: "commit".to_owned(),
            fixture_source_repository_tree: "tree".to_owned(),
            profiles: vec![row("train"), row("validation")],
        }
    }

    pub(crate) fn accepted_profile(
        evaluated_profile_id: &str,
        lanes: &[RetrieverKind],
    ) -> AcceptedRetrievalProfileV1 {
        accepted_profile_with_compatibility(
            evaluated_profile_id,
            lanes,
            RetrievalCompatibilityPinsV1::default(),
        )
    }

    fn accepted_profile_with_compatibility(
        evaluated_profile_id: &str,
        lanes: &[RetrieverKind],
        compatibility: RetrievalCompatibilityPinsV1,
    ) -> AcceptedRetrievalProfileV1 {
        let evaluation = PassingRetrievalEvaluationV1::from_report(
            &passing_report(evaluated_profile_id),
            evaluated_profile_id,
        )
        .expect("passing evaluation");
        let profile = FusionProfile {
            profile_id: id(&format!("profile.{evaluated_profile_id}")),
            evaluation_result_anchor: evaluation.evaluation_anchor().clone(),
            calibrations: lanes
                .iter()
                .copied()
                .map(|lane| {
                    (
                        lane,
                        id::<CalibrationProfileId>(&format!(
                            "calibration.{}.{}",
                            lane.as_str(),
                            evaluated_profile_id
                        )),
                    )
                })
                .collect(),
            score_domain_calibrations: BTreeMap::new(),
            weights_micros: lanes.iter().copied().map(|lane| (lane, 1)).collect(),
            diversity_policy_id: id(&format!("diversity.{evaluated_profile_id}")),
            rerank_policy_id: None,
            retrieval_budget: RetrievalBudget {
                max_candidates_per_lane: 8,
                max_fused_candidates: 8,
                max_hydrated_results: 4,
                max_hydration_bytes: 4096,
                deadline_micros: None,
            },
        };
        let diversity = DiversityPolicy {
            policy_id: profile.diversity_policy_id.clone(),
            evaluation_result_anchor: Some(profile.evaluation_result_anchor.clone()),
            per_source_namespace: None,
            per_source_instance: None,
            per_repository: None,
            per_file: None,
            per_session_or_thread: None,
            per_copy_cluster: None,
            per_evidence_role: None,
        };
        AcceptedRetrievalProfileV1::new(profile, diversity, None, compatibility, evaluation)
            .expect("accepted profile")
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn semantic_pins() -> SemanticCompatibilityPinsV1 {
        let artifact = digest('a');
        let projection = EmbeddingProjectionKeyV1 {
            model_artifact_digest: artifact.clone(),
            tokenizer_digest: digest('b'),
            config_digest: digest('c'),
            query_instruction_digest: None,
            document_instruction_digest: None,
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 128,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "runtime.pr9-activation-test.v1".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: 4,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: id::<ChunkerRevision>("chunker.pr9-activation-test.v1"),
            privacy_domain: id("privacy.pr9-activation-test"),
            privacy_key_epoch: 1,
        }
        .admit()
        .expect("admitted semantic projection");
        let vector_generation_id = VectorGenerationIdV1::new(digest('d'));
        SemanticCompatibilityPinsV1 {
            implementation_revision: ComponentRevision::new("semantic.pr9-activation-test.v1")
                .expect("implementation revision"),
            fusion_revision: ComponentRevision::new("fusion.pr9-activation-test.v1")
                .expect("fusion revision"),
            artifact_manifest_digest: artifact,
            runtime_compatibility_digest: digest('e'),
            search_index_key: tracedecay_domain::SemanticSearchIndexProfileV1::exact_flat_v1()
                .and_then(|profile| profile.index_key())
                .expect("search index key"),
            calibration: SemanticCalibrationProfileV1 {
                calibration_profile_id: id("calibration.semantic.semantic-active"),
                cohort_digest: digest('f'),
                projection_key: projection.projection_key().clone(),
                vector_generation: vector_generation_id.clone(),
                capability_manifest_digest: digest('1'),
                maximum_distance_micros: 2_000_000,
                minimum_margin_micros: 0,
            },
            projection,
            vector_generation_id,
            resources: SemanticResourceRequirementV1 {
                model_bytes: 10,
                tokenizer_bytes: 5,
                resident_bytes: 20,
                threads: 2,
                batch_size: 4,
                sequence_length: 128,
                load_deadline_ms: 1_000,
            },
        }
    }

    fn semantic_committed_state(scope: ResolvedScope) -> CommittedRetrievalProfileStateV1 {
        let pr9 = accepted_profile("pr9-baseline", &RetrieverKind::PR9_FALLBACK_LANES);
        let pins = semantic_pins();
        let semantic = accepted_profile_with_compatibility(
            "semantic-active",
            &[
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Graph,
                RetrieverKind::Semantic,
            ],
            RetrievalCompatibilityPinsV1 {
                semantic: Some(pins.clone()),
                rerank: None,
            },
        );
        let base_revision = id::<ConfigurationRevisionId>("configuration.pr9-activation-test.1");
        let result_revision = id::<ConfigurationRevisionId>("configuration.pr9-activation-test.2");
        let actor_id = id("actor.pr9-activation-test");
        let operation = RetrievalProfileAuditOperationV1::Activate;
        let freshness_vector_digest = digest('2');
        let occurred_at = UtcMicros(20);
        let audit = RetrievalProfileAuditEventV1 {
            event_id: canonical_sha256(&(
                "tracedecay.retrieval.profile-audit.v1",
                &actor_id,
                &operation,
                &pr9.profile().profile_id,
                &semantic.profile().profile_id,
                pr9.profile_digest(),
                semantic.profile_digest(),
                &semantic.profile().evaluation_result_anchor,
                &freshness_vector_digest,
                &base_revision,
                &result_revision,
                occurred_at,
            ))
            .expect("audit digest"),
            actor_id,
            operation,
            prior_active_profile_id: pr9.profile().profile_id.clone(),
            resulting_active_profile_id: semantic.profile().profile_id.clone(),
            prior_active_digest: pr9.profile_digest().clone(),
            resulting_active_digest: semantic.profile_digest().clone(),
            evaluation_anchor: semantic.profile().evaluation_result_anchor.clone(),
            freshness_vector_digest,
            base_revision,
            result_revision: result_revision.clone(),
            occurred_at,
        };
        let state = serde_json::from_value::<RetrievalProfileStateSnapshotV1>(serde_json::json!({
            "configuration_revision": result_revision,
            "active": semantic,
            "rollback": pr9,
            "audit": [audit],
        }))
        .expect("persisted semantic retrieval state")
        .into_state()
        .expect("semantic retrieval state");
        let configuration = SemanticConfigurationPinV1 {
            revision_id: state.configuration_revision().clone(),
            snapshot_id: id::<ConfigurationSnapshotId>(
                "configuration.snapshot.pr9-activation-test",
            ),
            effective_behavior_digest: digest('3'),
        };
        let command = SemanticActivationCommandV1::new(
            configuration,
            SemanticActivationRequestV1::new(pins.vector_generation_id.clone(), None, None)
                .expect("semantic activation request"),
        )
        .expect("semantic activation command");
        let receipt = SemanticActivationReceiptV1::issue(&command, UtcMicros(30))
            .expect("semantic activation receipt");
        CommittedRetrievalProfileStateV1 {
            scope,
            state,
            current_activation: Some(
                SemanticCurrentLinkedActivationV1::new(receipt, pins)
                    .expect("current semantic activation"),
            ),
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .expect("run git fixture command");
        assert!(status.success(), "git fixture command failed: {args:?}");
    }

    #[test]
    fn process_key_debug_never_emits_secret_bytes() {
        let secret = vec![0xab; PR9_KEY_BYTES];
        let key = ProcessLocalPr9KeyV1 {
            key_id: RetrievalCursorKeyId::new("retrieval-key.pr9.test").unwrap(),
            secret: Zeroizing::new(secret.clone()),
        };

        let debug = format!("{key:?}");
        assert!(!debug.contains(&hex::encode(secret)));
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn unavailable_provider_status_contains_no_key_material() {
        let provider = DaemonPr9AuthorityProviderV1 {
            activated: Arc::new(RwLock::new(BTreeMap::new())),
            key: Some(Arc::new(ProcessLocalPr9KeyV1 {
                key_id: RetrievalCursorKeyId::new("retrieval-key.pr9.test").unwrap(),
                secret: Zeroizing::new(vec![0xcd; PR9_KEY_BYTES]),
            })),
        };

        assert_eq!(
            provider.status(None),
            Pr9AuthorityProviderStatusV1::Unavailable {
                reason: Pr9AuthorityUnavailableReasonV1::ActivationUnavailable,
            }
        );
        assert!(!format!("{provider:?}").contains(&hex::encode(vec![0xcd; PR9_KEY_BYTES])));
    }

    #[test]
    fn semantic_activation_selects_exact_pr9_rollback_profile() {
        let pr9 = accepted_profile("pr9-baseline", &RetrieverKind::PR9_FALLBACK_LANES);
        let semantic_active = accepted_profile(
            "semantic-active",
            &[RetrieverKind::ExactLiteral, RetrieverKind::Lexical],
        );

        let selected = exact_pr9_profile_from_slots(&semantic_active, Some(&pr9))
            .expect("rollback PR9 profile");

        assert_eq!(selected.profile().profile_id, pr9.profile().profile_id);
    }

    #[test]
    fn evaluated_initial_pr9_state_is_available_without_a_fake_activation_event() {
        let provider = DaemonPr9AuthorityProviderV1::default();
        let scope = ResolvedScope::new(
            id("project.initial"),
            id("repository.initial"),
            id("worktree.initial"),
            Some(id("refs/heads/main")),
        )
        .expect("scope");
        let pr9 = accepted_profile("pr9-baseline", &RetrieverKind::PR9_FALLBACK_LANES);
        let state = RetrievalProfileStateV1::new(
            id::<ConfigurationRevisionId>("configuration.pr9-initial.1"),
            pr9.clone(),
            &RetrievalRuntimeCompatibilityV1 {
                retrieval_ceiling: RetrievalBudget {
                    max_candidates_per_lane: 32,
                    max_fused_candidates: 32,
                    max_hydrated_results: 16,
                    max_hydration_bytes: 65_536,
                    deadline_micros: None,
                },
                semantic: None,
                semantic_ceiling: None,
                rerank: None,
                rerank_ceiling: None,
            },
        )
        .expect("initial state");

        let status = provider
            .install_evaluated_initial_state(scope.clone(), state)
            .expect("evaluated initial state");

        assert!(matches!(
            status,
            Pr9AuthorityProviderStatusV1::Available { profile_id, .. }
                if profile_id == pr9.profile().profile_id
        ));
    }

    #[test]
    fn semantic_rollback_selects_restored_exact_pr9_active_profile() {
        let pr9 = accepted_profile("pr9-baseline", &RetrieverKind::PR9_FALLBACK_LANES);
        let prior_semantic = accepted_profile(
            "semantic-prior",
            &[RetrieverKind::ExactLiteral, RetrieverKind::Lexical],
        );

        let selected =
            exact_pr9_profile_from_slots(&pr9, Some(&prior_semantic)).expect("active PR9 profile");

        assert_eq!(selected.profile().profile_id, pr9.profile().profile_id);
    }

    #[test]
    fn zero_or_multiple_exact_pr9_profiles_fail_closed() {
        let non_pr9 = accepted_profile(
            "semantic-active",
            &[RetrieverKind::ExactLiteral, RetrieverKind::Lexical],
        );
        assert!(matches!(
            exact_pr9_profile_from_slots(&non_pr9, None),
            Err(Pr9AuthorityUnavailableReasonV1::InvalidActivatedProfile)
        ));

        let first = accepted_profile("pr9-first", &RetrieverKind::PR9_FALLBACK_LANES);
        let second = accepted_profile("pr9-second", &RetrieverKind::PR9_FALLBACK_LANES);
        assert!(matches!(
            exact_pr9_profile_from_slots(&first, Some(&second)),
            Err(Pr9AuthorityUnavailableReasonV1::AmbiguousActivatedProfile)
        ));
    }

    #[tokio::test]
    async fn semantic_activation_keeps_the_exact_pr9_fallback_available() {
        let project = TempDir::new().expect("project root");
        git(project.path(), &["init", "-q", "-b", "main"]);
        git(project.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            project.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::create_dir_all(project.path().join("src")).expect("source directory");
        std::fs::write(project.path().join("src/lib.rs"), "pub fn indexed() {}\n")
            .expect("source file");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "fixture"]);

        let scope = crate::daemon::project_open_owners::resolved_scope_for_project(
            project.path(),
            &ProjectId::new("project.pr9-semantic-activation").expect("project id"),
        )
        .expect("resolved scope");
        let store = TempDir::new().expect("store root");
        let registry = super::super::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(1);
        registry
            .mount_worktree(project.path(), store.path().to_path_buf(), None)
            .await
            .expect("mount code index");
        let provider = DaemonPr9AuthorityProviderV1::default();
        let registrar = DaemonPr9ActivationRegistrarV1::new(
            provider.clone(),
            registry.clone(),
            project.path().to_path_buf(),
        );

        registrar
            .activation_committed(semantic_committed_state(scope.clone()))
            .await
            .expect("semantic activation registration");

        assert!(matches!(
            provider.status(Some(&scope)),
            Pr9AuthorityProviderStatusV1::Available { profile_id, .. }
                if profile_id.as_str() == "profile.pr9-baseline"
        ));
        assert!(
            registry.has_pr9_query_authority_for_scope(&scope).await,
            "semantic activation must keep the mounted PR9 fallback query authority"
        );
        registry.shutdown().await;
    }
}
