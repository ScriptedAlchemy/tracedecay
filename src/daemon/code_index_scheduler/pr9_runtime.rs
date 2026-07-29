//! Fail-closed production activation and execution for authenticated PR9 search.
//!
//! The daemon owns only orchestration here. Accepted profile/evaluation state
//! and process-local query/cursor keys come from a configured authority port;
//! this module never chooses weights, anchors, revisions, or key material.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, ComponentRevision, DiversityPolicy,
    ExactAdmissionRuleRevision, FreshnessVectorDigest, FusionProfile, FusionProfileId, PrincipalId,
    PrivacyDomainId, QueryNormalizationRevision, RelationEdgeKindV1, RetrievalAnchorId,
    RetrievalCursor, RetrievalFailure, RetrievalRequest, RetrievalScope, RetrievalSnapshot,
    RetrieverKind, RetrieverOutcome, SanitizerRevision, ScoreDomainId, SingleRootScopeV1,
    TemporalModeV1, VectorWatermark,
};

use super::CodeIndexSchedulerRegistryV1;
use crate::query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest,
    ExactLaneRetriever,
};
use crate::query::retrieval::fusion::{CompositionLaneInput, RetrievalCursorKeyringV1};
use crate::query::retrieval::graph::{GraphLaneRequest, GraphLaneRetriever};
use crate::query::retrieval::lexical::{
    LexicalLaneEvidence, LexicalLaneRequest, LexicalLaneRetriever, lexical_query_parts,
};
use crate::query::retrieval::{
    AuthorizedPr9FallbackV1, Pr9QueryAuthorityErrorV1, Pr9QueryAuthorityV1, RawRetrievalRequestV1,
    RetrievalPortError, SanitizedRetrievalRequestV1,
};

/// Immutable evidence that a configured authority accepted one exact PR9
/// profile for one exact admitted scope.
pub(in crate::daemon) struct AcceptedPr9EvaluationV1 {
    pub status: crate::search_eval::DirectEvaluationStatusV1,
    pub scope_digest: tracedecay_domain::ManifestDigest,
    pub profile_id: FusionProfileId,
    pub evaluation_result_anchor: RetrievalAnchorId,
}

/// Provider-owned activation material. The keyring is already constructed by
/// the configured process-local secret authority, so raw key bytes never cross
/// this daemon orchestration boundary.
pub(in crate::daemon) struct Pr9AuthorityMaterialV1 {
    pub scope: ResolvedScope,
    pub evaluation: AcceptedPr9EvaluationV1,
    pub profile: FusionProfile,
    pub diversity: DiversityPolicy,
    pub ranking_revision: ComponentRevision,
    pub keyring: Option<RetrievalCursorKeyringV1>,
}

#[derive(Debug, Error)]
pub(in crate::daemon) enum Pr9AuthorityProviderErrorV1 {
    #[error("configured PR9 authority is unavailable: {0}")]
    Unavailable(String),
}

/// Daemon-facing port over an existing configuration/key authority.
///
/// Returning a list is intentional: the daemon rejects zero or multiple
/// candidates rather than selecting an arbitrary profile or key owner.
pub(in crate::daemon) trait Pr9AuthorityProviderV1: Send + Sync {
    fn accepted_authorities(
        &self,
        scope: &ResolvedScope,
        privacy_domain: &PrivacyDomainId,
    ) -> Result<Vec<Pr9AuthorityMaterialV1>, Pr9AuthorityProviderErrorV1>;
}

#[derive(Debug, Error)]
pub(in crate::daemon) enum Pr9RuntimeMountErrorV1 {
    #[error(transparent)]
    Provider(#[from] Pr9AuthorityProviderErrorV1),
    #[error("no accepted PR9 authority exists for the exact admitted scope")]
    AuthorityMissing,
    #[error("multiple PR9 authorities match the exact admitted scope")]
    AuthorityAmbiguous,
    #[error("PR9 authority scope does not exactly match the admitted scope")]
    ScopeMismatch,
    #[error("PR9 evaluation is not PASS")]
    EvaluationNotPassed,
    #[error("PR9 evaluation no longer binds the supplied profile")]
    EvaluationStale,
    #[error("PR9 process-local query/cursor key is unavailable")]
    KeyUnavailable,
    #[error("PR9 query/cursor key privacy domain does not match the published generation")]
    PrivacyDomainMismatch,
    #[error("no complete current code generation exists for the exact admitted scope")]
    GenerationUnavailable,
    #[error(transparent)]
    Authority(#[from] Pr9QueryAuthorityErrorV1),
    #[error("PR9 authority mount failed: {0}")]
    Mount(String),
}

/// Resolve and validate one exact accepted authority without mounting it.
///
/// Kept separate from the async registry mutation so project-open callers can
/// report precise configuration failures before changing mounted state.
pub(in crate::daemon) fn prepare_pr9_query_authority(
    scope: &ResolvedScope,
    privacy_domain: &PrivacyDomainId,
    provider: &dyn Pr9AuthorityProviderV1,
) -> Result<Arc<Pr9QueryAuthorityV1>, Pr9RuntimeMountErrorV1> {
    scope
        .validate()
        .map_err(|_| Pr9RuntimeMountErrorV1::ScopeMismatch)?;
    privacy_domain
        .validate()
        .map_err(|_| Pr9RuntimeMountErrorV1::PrivacyDomainMismatch)?;
    let mut candidates = provider.accepted_authorities(scope, privacy_domain)?;
    if candidates.is_empty() {
        return Err(Pr9RuntimeMountErrorV1::AuthorityMissing);
    }
    if candidates.len() != 1 {
        return Err(Pr9RuntimeMountErrorV1::AuthorityAmbiguous);
    }
    let material = candidates
        .pop()
        .ok_or(Pr9RuntimeMountErrorV1::AuthorityMissing)?;
    material
        .scope
        .validate()
        .map_err(|_| Pr9RuntimeMountErrorV1::ScopeMismatch)?;
    if material.scope != *scope {
        return Err(Pr9RuntimeMountErrorV1::ScopeMismatch);
    }
    if material.evaluation.scope_digest != scope.scope_digest {
        return Err(Pr9RuntimeMountErrorV1::EvaluationStale);
    }
    if material.evaluation.status != crate::search_eval::DirectEvaluationStatusV1::Pass {
        return Err(Pr9RuntimeMountErrorV1::EvaluationNotPassed);
    }
    if material.evaluation.profile_id != material.profile.profile_id
        || material.evaluation.evaluation_result_anchor != material.profile.evaluation_result_anchor
    {
        return Err(Pr9RuntimeMountErrorV1::EvaluationStale);
    }
    let keyring = material
        .keyring
        .ok_or(Pr9RuntimeMountErrorV1::KeyUnavailable)?;
    if keyring.privacy_domain() != privacy_domain {
        return Err(Pr9RuntimeMountErrorV1::PrivacyDomainMismatch);
    }
    Ok(Arc::new(Pr9QueryAuthorityV1::new(
        material.profile,
        material.diversity,
        material.ranking_revision,
        keyring,
    )?))
}

/// Callable project-open hook: resolve a configured accepted authority and
/// mount it only for the same exact scope as the already-mounted worktree.
pub(in crate::daemon) async fn mount_pr9_query_authority_on_project_open(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    scope: &ResolvedScope,
    provider: &dyn Pr9AuthorityProviderV1,
) -> Result<(), Pr9RuntimeMountErrorV1> {
    let latest = registry
        .latest_complete_fresh_for_scope(scope)
        .await
        .ok_or(Pr9RuntimeMountErrorV1::GenerationUnavailable)?;
    let privacy_domain = latest.generation.manifest().privacy_domain.clone();
    let authority = prepare_pr9_query_authority(scope, &privacy_domain, provider)?;
    registry
        .mount_pr9_query_authority(project_root, scope, authority)
        .await
        .map_err(|error| Pr9RuntimeMountErrorV1::Mount(error.to_string()))
}

/// Caller-owned, versioned lane policy for one raw PR9 query.
///
/// Query bytes are intentionally private and omitted from `Debug`; they are
/// consumed immediately by [`RawRetrievalRequestV1::sanitize`].
pub(in crate::daemon) struct Pr9SearchExecutionRequestV1 {
    query: String,
    pub principal: PrincipalId,
    pub authorization_revision: AuthorizationRevision,
    pub sanitizer_revision: SanitizerRevision,
    pub normalization_revision: QueryNormalizationRevision,
    pub exact_rule_revision: ExactAdmissionRuleRevision,
    pub lexical_profile_revision: ComponentRevision,
    pub lexical_score_domain: ScoreDomainId,
    pub fuzzy_budget: u32,
    pub graph_edge_kinds: Vec<RelationEdgeKindV1>,
    pub graph_max_depth: u32,
    pub page_size: usize,
    pub cursor: Option<RetrievalCursor>,
}

impl Pr9SearchExecutionRequestV1 {
    pub fn new(query: impl Into<String>, policy: Pr9SearchExecutionPolicyV1) -> Self {
        Self {
            query: query.into(),
            principal: policy.principal,
            authorization_revision: policy.authorization_revision,
            sanitizer_revision: policy.sanitizer_revision,
            normalization_revision: policy.normalization_revision,
            exact_rule_revision: policy.exact_rule_revision,
            lexical_profile_revision: policy.lexical_profile_revision,
            lexical_score_domain: policy.lexical_score_domain,
            fuzzy_budget: policy.fuzzy_budget,
            graph_edge_kinds: policy.graph_edge_kinds,
            graph_max_depth: policy.graph_max_depth,
            page_size: policy.page_size,
            cursor: policy.cursor,
        }
    }
}

/// Non-secret execution policy supplied by the mounted MCP/application owner.
pub(in crate::daemon) struct Pr9SearchExecutionPolicyV1 {
    pub principal: PrincipalId,
    pub authorization_revision: AuthorizationRevision,
    pub sanitizer_revision: SanitizerRevision,
    pub normalization_revision: QueryNormalizationRevision,
    pub exact_rule_revision: ExactAdmissionRuleRevision,
    pub lexical_profile_revision: ComponentRevision,
    pub lexical_score_domain: ScoreDomainId,
    pub fuzzy_budget: u32,
    pub graph_edge_kinds: Vec<RelationEdgeKindV1>,
    pub graph_max_depth: u32,
    pub page_size: usize,
    pub cursor: Option<RetrievalCursor>,
}

pub(in crate::daemon) struct ExecutedPr9SearchV1 {
    pub generation: CodeGenerationId,
    pub authorized: AuthorizedPr9FallbackV1,
    pub sanitized: SanitizedRetrievalRequestV1,
}

#[derive(Debug, Error)]
pub(in crate::daemon) enum Pr9SearchExecutionErrorV1 {
    #[error("PR9 search scope is invalid: {0}")]
    InvalidScope(String),
    #[error("no complete current code generation matches the exact admitted scope")]
    GenerationUnavailable,
    #[error("PR9 query authority is unavailable for the exact admitted scope")]
    AuthorityUnavailable,
    #[error("PR9 search policy is invalid: {0}")]
    InvalidPolicy(String),
    #[error("PR9 request or production lane boundary failed: {0}")]
    Retrieval(#[from] RetrievalPortError),
    #[error("PR9 query authorization/composition failed: {0}")]
    Authority(#[from] Pr9QueryAuthorityErrorV1),
}

impl CodeIndexSchedulerRegistryV1 {
    /// Execute exact, lexical, and graph independently against the newest
    /// complete generation for one exact scope, then pass their typed outcomes
    /// unchanged into the authenticated PR9 composition authority.
    pub(in crate::daemon) async fn execute_pr9_search(
        &self,
        scope: &ResolvedScope,
        input: Pr9SearchExecutionRequestV1,
    ) -> Result<ExecutedPr9SearchV1, Pr9SearchExecutionErrorV1> {
        scope
            .validate()
            .map_err(|error| Pr9SearchExecutionErrorV1::InvalidScope(error.to_string()))?;
        validate_search_policy(&input)?;
        let latest = self
            .latest_complete_ready_for_scope(scope)
            .await
            .ok_or(Pr9SearchExecutionErrorV1::GenerationUnavailable)?;
        let authority = self
            .pr9_query_authority_for_scope(scope)
            .await
            .ok_or(Pr9SearchExecutionErrorV1::AuthorityUnavailable)?;
        let generation = latest.generation.manifest().generation_id.clone();
        let request = RetrievalRequest {
            principal: input.principal,
            scope: RetrievalScope {
                privacy_domain: latest.generation.manifest().privacy_domain.clone(),
                root: SingleRootScopeV1 {
                    repository: latest.generation.snapshot().repository.clone(),
                    worktree: latest.generation.snapshot().worktree.clone(),
                    reference: latest.generation.snapshot().reference.clone(),
                },
            },
            temporal_mode: TemporalModeV1::Current,
            snapshot: RetrievalSnapshot {
                watermarks: VectorWatermark::default(),
                freshness_digest: FreshnessVectorDigest::new(
                    latest.generation.manifest().snapshot_digest.as_str(),
                )
                .map_err(|error| Pr9SearchExecutionErrorV1::InvalidPolicy(error.to_string()))?,
                authorization_revision: input.authorization_revision,
                captured_at: latest.generation.manifest().seal.sealed_at,
            },
            profile_id: authority.profile().profile_id.clone(),
            budget: authority.profile().retrieval_budget,
        };
        let sanitized = RawRetrievalRequestV1::new(input.query, request)
            .sanitize(input.sanitizer_revision, input.normalization_revision)?;
        let request = sanitized.request();
        let query_view = sanitized.query_view();
        let owners = latest.production_query_owners()?;

        let parser = CentralExactAdmissionAuthorityV1::new(input.exact_rule_revision);
        let exact_request = ExactLaneRequest {
            base: request.clone(),
            query_view,
            generation: generation.clone(),
            literals: parser.parse_literals(query_view, request),
            budget: request.budget,
        };
        let exact = owners.exact.retrieve_exact(&exact_request)?;

        let lexical_parts = lexical_query_parts(query_view.as_str())?;
        let lexical_request = LexicalLaneRequest {
            base: request.clone(),
            query_view,
            generation: generation.clone(),
            whole_terms: lexical_parts.whole_terms,
            subtokens: lexical_parts.subtokens,
            phrases: lexical_parts.phrases,
            field_filters: Vec::new(),
            fuzzy_budget: input.fuzzy_budget,
            lexical_profile_revision: input.lexical_profile_revision,
            score_domain: input.lexical_score_domain,
            budget: request.budget,
        };
        let lexical = owners.lexical.retrieve_lexical(&lexical_request)?;

        let graph_seeds = graph_seeds_from_outcomes(&exact, &lexical);
        let graph = if graph_seeds.is_empty() {
            RetrieverOutcome::Unavailable(RetrievalFailure::AuthorityUnavailable {
                detail: "exact and lexical lanes produced no graph seed".to_owned(),
            })
        } else {
            owners.graph.retrieve_graph(&GraphLaneRequest {
                base: request.clone(),
                generation: generation.clone(),
                seed_anchors: graph_seeds,
                edge_kinds: input.graph_edge_kinds,
                max_depth: input.graph_max_depth,
                budget: request.budget,
            })?
        };
        let lanes = vec![
            CompositionLaneInput::new(RetrieverKind::ExactLiteral, exact)
                .map_err(Pr9QueryAuthorityErrorV1::from)?,
            CompositionLaneInput::new(RetrieverKind::Lexical, lexical)
                .map_err(Pr9QueryAuthorityErrorV1::from)?,
            CompositionLaneInput::new(RetrieverKind::Graph, graph)
                .map_err(Pr9QueryAuthorityErrorV1::from)?,
        ];
        let authorized = self
            .compose_pr9_fallback(
                scope,
                request,
                query_view,
                lanes,
                input.page_size,
                input.cursor.as_ref(),
            )
            .await?;
        Ok(ExecutedPr9SearchV1 {
            generation,
            authorized,
            sanitized,
        })
    }
}

fn validate_search_policy(
    input: &Pr9SearchExecutionRequestV1,
) -> Result<(), Pr9SearchExecutionErrorV1> {
    input
        .authorization_revision
        .validate()
        .map_err(|error| Pr9SearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .sanitizer_revision
        .validate()
        .map_err(|error| Pr9SearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .normalization_revision
        .validate()
        .map_err(|error| Pr9SearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .exact_rule_revision
        .validate()
        .map_err(|error| Pr9SearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .lexical_profile_revision
        .validate()
        .map_err(|error| Pr9SearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .lexical_score_domain
        .validate()
        .map_err(|error| Pr9SearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    if input.page_size == 0 {
        return Err(Pr9SearchExecutionErrorV1::InvalidPolicy(
            "page size must be positive".to_owned(),
        ));
    }
    if input.graph_max_depth == 0 || input.graph_edge_kinds.is_empty() {
        return Err(Pr9SearchExecutionErrorV1::InvalidPolicy(
            "graph depth and edge-kind policy must be non-empty".to_owned(),
        ));
    }
    let unique_edges = input
        .graph_edge_kinds
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if unique_edges.len() != input.graph_edge_kinds.len() {
        return Err(Pr9SearchExecutionErrorV1::InvalidPolicy(
            "graph edge kinds must be unique".to_owned(),
        ));
    }
    Ok(())
}

fn graph_seeds_from_outcomes(
    exact: &RetrieverOutcome<tracedecay_domain::RetrieverBatch<ExactLaneEvidence>>,
    lexical: &RetrieverOutcome<tracedecay_domain::RetrieverBatch<LexicalLaneEvidence>>,
) -> Vec<crate::query::retrieval::ports::CodeCandidateBindingV1> {
    let mut seeds = Vec::new();
    let mut seen_occurrences = BTreeSet::new();
    let mut seen_symbols = BTreeSet::new();
    let mut add_batch = |bindings: Vec<&crate::query::retrieval::ports::CodeCandidateBindingV1>| {
        for binding in bindings {
            let Some(symbol) = binding.occurrence.symbol.as_ref() else {
                continue;
            };
            if seen_occurrences.insert(binding.source_occurrence.clone())
                && seen_symbols.insert(symbol.clone())
            {
                seeds.push(binding.clone());
            }
        }
    };
    match exact {
        RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => {
            add_batch(
                batch
                    .evidence_by_occurrence
                    .values()
                    .map(|evidence| &evidence.binding)
                    .collect(),
            );
        }
        _ => {}
    }
    match lexical {
        RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => {
            add_batch(
                batch
                    .evidence_by_occurrence
                    .values()
                    .map(|evidence| &evidence.binding)
                    .collect(),
            );
        }
        _ => {}
    }
    seeds
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use tracedecay_application::ResolvedScope;
    use tracedecay_domain::{
        CalibrationProfileId, ComponentRevision, DiversityPolicy, FusionProfile, ManifestDigest,
        PrivacyDomainId, RefId, RepositoryId, RetrievalAnchorId, RetrievalBudget,
        RetrievalCursorKeyId, RetrieverKind, WorktreeId,
    };

    use super::{
        AcceptedPr9EvaluationV1, Pr9AuthorityMaterialV1, Pr9AuthorityProviderErrorV1,
        Pr9AuthorityProviderV1, Pr9RuntimeMountErrorV1, prepare_pr9_query_authority,
    };
    use crate::query::retrieval::fusion::RetrievalCursorKeyringV1;

    struct OneShotProvider {
        candidates: Mutex<Option<Vec<Pr9AuthorityMaterialV1>>>,
    }

    impl Pr9AuthorityProviderV1 for OneShotProvider {
        fn accepted_authorities(
            &self,
            _scope: &ResolvedScope,
            _privacy_domain: &PrivacyDomainId,
        ) -> Result<Vec<Pr9AuthorityMaterialV1>, Pr9AuthorityProviderErrorV1> {
            Ok(self
                .candidates
                .lock()
                .expect("provider lock")
                .take()
                .expect("provider called once"))
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn scope(suffix: &str) -> ResolvedScope {
        ResolvedScope::new(
            id(&format!("project.{suffix}")),
            id::<RepositoryId>(&format!("repository.{suffix}")),
            id::<WorktreeId>(&format!("worktree.{suffix}")),
            Some(id::<RefId>(&format!("refs/heads/{suffix}"))),
        )
        .expect("scope")
    }

    fn profile() -> FusionProfile {
        FusionProfile {
            profile_id: id("profile.pr9.accepted.v1"),
            evaluation_result_anchor: id("evaluation.pr9.accepted.v1"),
            calibrations: RetrieverKind::PR9_FALLBACK_LANES
                .into_iter()
                .map(|lane| {
                    (
                        lane,
                        id::<CalibrationProfileId>(&format!(
                            "calibration.{}.accepted.v1",
                            lane.as_str()
                        )),
                    )
                })
                .collect(),
            score_domain_calibrations: BTreeMap::new(),
            weights_micros: [
                (RetrieverKind::ExactLiteral, 1_000_000),
                (RetrieverKind::Lexical, 500_000),
                (RetrieverKind::Graph, 250_000),
            ]
            .into_iter()
            .collect(),
            diversity_policy_id: id("diversity.pr9.accepted.v1"),
            rerank_policy_id: None,
            retrieval_budget: RetrievalBudget {
                max_candidates_per_lane: 32,
                max_fused_candidates: 16,
                max_hydrated_results: 8,
                max_hydration_bytes: 65_536,
                deadline_micros: None,
            },
        }
    }

    fn privacy_domain() -> PrivacyDomainId {
        id("privacy.pr9.fixture")
    }

    fn material(scope: ResolvedScope) -> Pr9AuthorityMaterialV1 {
        let profile = profile();
        let evaluation = AcceptedPr9EvaluationV1 {
            status: crate::search_eval::DirectEvaluationStatusV1::Pass,
            scope_digest: scope.scope_digest.clone(),
            profile_id: profile.profile_id.clone(),
            evaluation_result_anchor: profile.evaluation_result_anchor.clone(),
        };
        Pr9AuthorityMaterialV1 {
            scope,
            evaluation,
            profile: profile.clone(),
            diversity: DiversityPolicy {
                policy_id: profile.diversity_policy_id,
                evaluation_result_anchor: Some(profile.evaluation_result_anchor),
                per_source_namespace: None,
                per_source_instance: None,
                per_repository: None,
                per_file: None,
                per_session_or_thread: None,
                per_copy_cluster: None,
                per_evidence_role: None,
            },
            ranking_revision: id::<ComponentRevision>("ranking.pr9.accepted.v1"),
            keyring: Some(
                RetrievalCursorKeyringV1::new(
                    privacy_domain(),
                    id::<RetrievalCursorKeyId>("retrieval-key.pr9.fixture"),
                    1,
                    vec![7_u8; 32],
                    1_000_000,
                )
                .expect("keyring"),
            ),
        }
    }

    #[test]
    fn exact_passed_authority_is_constructed_from_provider_material() {
        let scope = scope("main");
        let provider = OneShotProvider {
            candidates: Mutex::new(Some(vec![material(scope.clone())])),
        };

        let authority = prepare_pr9_query_authority(&scope, &privacy_domain(), &provider)
            .expect("accepted authority");

        assert_eq!(authority.profile().profile_id, profile().profile_id);
    }

    #[test]
    fn missing_or_ambiguous_authority_fails_closed() {
        let scope = scope("main");
        let missing = OneShotProvider {
            candidates: Mutex::new(Some(Vec::new())),
        };
        assert!(matches!(
            prepare_pr9_query_authority(&scope, &privacy_domain(), &missing),
            Err(Pr9RuntimeMountErrorV1::AuthorityMissing)
        ));

        let ambiguous = OneShotProvider {
            candidates: Mutex::new(Some(vec![material(scope.clone()), material(scope.clone())])),
        };
        assert!(matches!(
            prepare_pr9_query_authority(&scope, &privacy_domain(), &ambiguous),
            Err(Pr9RuntimeMountErrorV1::AuthorityAmbiguous)
        ));
    }

    #[test]
    fn non_pass_stale_scope_and_missing_key_fail_closed() {
        let active_scope = scope("main");

        let mut pending = material(active_scope.clone());
        pending.evaluation.status = crate::search_eval::DirectEvaluationStatusV1::Pending;
        let provider = OneShotProvider {
            candidates: Mutex::new(Some(vec![pending])),
        };
        assert!(matches!(
            prepare_pr9_query_authority(&active_scope, &privacy_domain(), &provider),
            Err(Pr9RuntimeMountErrorV1::EvaluationNotPassed)
        ));

        let provider = OneShotProvider {
            candidates: Mutex::new(Some(vec![material(scope("other"))])),
        };
        assert!(matches!(
            prepare_pr9_query_authority(&active_scope, &privacy_domain(), &provider),
            Err(Pr9RuntimeMountErrorV1::ScopeMismatch)
        ));

        let mut missing_key = material(active_scope.clone());
        missing_key.keyring = None;
        let provider = OneShotProvider {
            candidates: Mutex::new(Some(vec![missing_key])),
        };
        assert!(matches!(
            prepare_pr9_query_authority(&active_scope, &privacy_domain(), &provider),
            Err(Pr9RuntimeMountErrorV1::KeyUnavailable)
        ));
    }

    #[test]
    fn profile_or_anchor_drift_is_rejected_as_stale() {
        let scope = scope("main");
        let mut stale = material(scope.clone());
        stale.evaluation.evaluation_result_anchor =
            id::<RetrievalAnchorId>("evaluation.pr9.superseded.v1");
        let provider = OneShotProvider {
            candidates: Mutex::new(Some(vec![stale])),
        };

        assert!(matches!(
            prepare_pr9_query_authority(&scope, &privacy_domain(), &provider),
            Err(Pr9RuntimeMountErrorV1::EvaluationStale)
        ));

        let mut stale_scope_evaluation = material(scope.clone());
        stale_scope_evaluation.evaluation.scope_digest =
            id::<ManifestDigest>(&format!("sha256:{}", "f".repeat(64)));
        let provider = OneShotProvider {
            candidates: Mutex::new(Some(vec![stale_scope_evaluation])),
        };
        assert!(matches!(
            prepare_pr9_query_authority(&scope, &privacy_domain(), &provider),
            Err(Pr9RuntimeMountErrorV1::EvaluationStale)
        ));
    }

    #[test]
    fn cursor_key_privacy_domain_must_match_published_generation() {
        let scope = scope("main");
        let provider = OneShotProvider {
            candidates: Mutex::new(Some(vec![material(scope.clone())])),
        };

        assert!(matches!(
            prepare_pr9_query_authority(
                &scope,
                &id::<PrivacyDomainId>("privacy.pr9.other"),
                &provider,
            ),
            Err(Pr9RuntimeMountErrorV1::PrivacyDomainMismatch)
        ));
    }
}
