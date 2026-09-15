//! Production activation and execution for authenticated query search.
//!
//! Exact/lexical/graph search mounts the checked-in fallback policy and the
//! durable query/cursor keys of the published generation's privacy domain.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_contracts::ResolvedScope;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::{
    AuthorizationRevision, CalibrationProfileId, CodeGenerationId, ComponentRevision,
    DiversityPolicy, ExactAdmissionRuleRevision, FreshnessVectorDigest, FusionProfile, PrincipalId,
    QueryNormalizationRevision, RelationEdgeKindV1, RetrievalAnchorId, RetrievalBudget,
    RetrievalCursor, RetrievalFailure, RetrievalRequest, RetrievalScope, RetrievalSnapshot,
    RetrieverBatch, RetrieverCoverage, RetrieverKind, RetrieverOutcome, SanitizerRevision,
    ScoreDomainCalibrationV1, ScoreDomainId, SingleRootScopeV1, TemporalModeV1, VectorWatermark,
};

use super::{CodeIndexSchedulerRegistryV1, serving::CodeTextQueryOwnerReadinessV1};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest,
};
use tracedecay_query::retrieval::fusion::CompositionLaneInput;
use tracedecay_query::retrieval::graph::{GraphLaneRequest, GraphLaneRetriever};
use tracedecay_query::retrieval::lexical::{
    LexicalLaneEvidence, LexicalLaneRequest, LexicalRouteOutcomeV1, LexicalRoutePlanV1,
    LexicalRouteReceiptV1, LexicalRoutingV1, merge_lexical_routes,
};
use tracedecay_query::retrieval::ports::RetrievalExecutionControl;
use tracedecay_query::retrieval::{
    AuthorizedQueryFallbackV1, QUERY_EXACT_SCORE_DOMAIN_V1, QUERY_GRAPH_SCORE_DOMAIN_V1,
    QUERY_LEXICAL_SCORE_DOMAIN_V1, QueryAuthorityErrorV1, QueryAuthorityV1, RawRetrievalRequestV1,
    RetrievalPortError, SanitizedRetrievalRequestV1,
};

const QUERY_FALLBACK_PROFILE_ID: &str = "query-fallback";

#[derive(Debug, Error)]
pub enum QueryRuntimeMountErrorV1 {
    #[error("no complete current code generation exists for the exact admitted scope")]
    GenerationUnavailable,
    #[error("checked-in query fallback policy is invalid: {0}")]
    InvalidFallbackPolicy(String),
    #[error("durable query fallback cursor key is unavailable: {0}")]
    FallbackKeyUnavailable(String),
    #[error(transparent)]
    Authority(#[from] QueryAuthorityErrorV1),
    #[error("query authority mount failed: {0}")]
    Mount(String),
}

/// Mount the checked-in exact/lexical/graph policy for one exact admitted
/// scope.
pub async fn mount_core_query_authority_on_project_open(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    scope: &ResolvedScope,
    cursor_keys: &tracedecay_session_temporal_store::SessionTemporalCursorKeyProvider,
) -> Result<(), QueryRuntimeMountErrorV1> {
    let authority =
        prepare_core_query_authority_on_project_open(registry, project_root, scope, cursor_keys)
            .await?;
    registry
        .mount_query_authority(project_root, scope, authority)
        .await
        .map_err(|error| QueryRuntimeMountErrorV1::Mount(error.to_string()))
}

/// One deferred mount attempt, terminal unless the generation is still
/// unpublished for this exact scope.
#[derive(PartialEq, Eq)]
pub enum DeferredMountAttemptV1 {
    Terminal,
    AwaitNextPublication,
}

/// Waits for the first retained generation of `project_root` and then
/// retries the query-authority mount. Exits when the mount reaches any terminal
/// outcome or the publication channel closes (daemon shutdown).
///
/// The open-time mount runs before code-index activation, so the first ready
/// check usually misses. A later `Published` event wakes the waiter on a
/// fresh build; a restart that restores the same sealed generation records
/// `Noop` and never repeats that event, so the serving slot is polled too.
pub async fn retry_deferred_query_authority_until_serving<F, Fut>(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    mut attempt: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = DeferredMountAttemptV1>,
{
    let mut publications = registry.subscribe_generation_publications();
    let mut ready_poll = tokio::time::interval(Duration::from_secs(1));
    ready_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if registry
            .retained_text_owner_for_root(&project_root)
            .await
            .is_some()
            && attempt().await != DeferredMountAttemptV1::AwaitNextPublication
        {
            return;
        }
        tokio::select! {
            _ = ready_poll.tick() => {}
            publication = publications.recv() => match publication {
                Ok(publication) if publication.project_root == project_root => {}
                Ok(_) => {}
                // A lagged receiver dropped publications; one of them may have
                // been this project's, so attempt the mount anyway.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

/// Classify one deferred mount outcome. `GenerationUnavailable` keeps waiting;
/// every other result is terminal.
pub fn classify_deferred_query_authority_mount(
    scope: &ResolvedScope,
    outcome: Result<(), QueryRuntimeMountErrorV1>,
) -> DeferredMountAttemptV1 {
    match outcome {
        Ok(()) => {
            tracing::info!(
                event = "query_authority_mount",
                outcome = "mounted",
                project_id = %scope.project_id,
                deferred = true,
            );
            DeferredMountAttemptV1::Terminal
        }
        Err(QueryRuntimeMountErrorV1::GenerationUnavailable) => {
            DeferredMountAttemptV1::AwaitNextPublication
        }
        Err(error) => {
            tracing::warn!(
                event = "query_authority_mount",
                outcome = "deferred_failed",
                project_id = %scope.project_id,
                reason = %error,
                "deferred query authority mount abandoned"
            );
            DeferredMountAttemptV1::Terminal
        }
    }
}

async fn prepare_core_query_authority_on_project_open(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    scope: &ResolvedScope,
    cursor_keys: &tracedecay_session_temporal_store::SessionTemporalCursorKeyProvider,
) -> Result<Arc<QueryAuthorityV1>, QueryRuntimeMountErrorV1> {
    let root_text = registry.retained_text_owner_for_root(project_root).await;
    let privacy_domain = registry
        .retained_text_owner_freshness_for_scope(scope)
        .await
        .map(|(text, _)| text)
        .or(root_text)
        .ok_or(QueryRuntimeMountErrorV1::GenerationUnavailable)?
        .metadata()
        .manifest()
        .privacy_domain
        .clone();
    let (profile, diversity) = core_query_policy()?;
    let ranking_revision =
        ComponentRevision::new(tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1)
            .map_err(|error| QueryRuntimeMountErrorV1::InvalidFallbackPolicy(error.to_string()))?;
    let keyring = cursor_keys
        .retrieval_keyring(privacy_domain)
        .map_err(|error| QueryRuntimeMountErrorV1::FallbackKeyUnavailable(error.to_string()))?;
    let authority = Arc::new(QueryAuthorityV1::new(
        profile,
        diversity,
        ranking_revision,
        keyring,
    )?);
    Ok(authority)
}

fn fallback_policy_id<T>(value: &str) -> Result<T, QueryRuntimeMountErrorV1>
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.to_owned())
        .map_err(|error| QueryRuntimeMountErrorV1::InvalidFallbackPolicy(error.to_string()))
}

/// The checked-in exact/lexical/graph fusion policy.
///
/// Its evaluation anchor is content-addressed over the ranking material, so a
/// changed weight, budget, calibration, or diversity cap invalidates cursors
/// signed under the previous policy instead of resuming them with different
/// ranking.
fn core_query_policy() -> Result<(FusionProfile, DiversityPolicy), QueryRuntimeMountErrorV1> {
    let lanes = [
        (
            RetrieverKind::ExactLiteral,
            QUERY_EXACT_SCORE_DOMAIN_V1,
            1_000_000,
        ),
        (
            RetrieverKind::Lexical,
            QUERY_LEXICAL_SCORE_DOMAIN_V1,
            1_000_000,
        ),
        (RetrieverKind::Graph, QUERY_GRAPH_SCORE_DOMAIN_V1, 250_000),
    ];
    let mut calibrations = BTreeMap::new();
    let mut score_domain_calibrations = BTreeMap::new();
    let mut weights_micros = BTreeMap::new();
    for (lane, score_domain, weight_micros) in lanes {
        let calibration_profile_id: CalibrationProfileId = fallback_policy_id(&format!(
            "calibration.{}.{QUERY_FALLBACK_PROFILE_ID}",
            lane.as_str()
        ))?;
        let score_domain: ScoreDomainId = fallback_policy_id(score_domain)?;
        calibrations.insert(lane, calibration_profile_id.clone());
        score_domain_calibrations.insert(
            score_domain.clone(),
            ScoreDomainCalibrationV1 {
                calibration_profile_id,
                score_domain,
                raw_min_micros: 0,
                raw_max_micros: 1_000_000,
            },
        );
        weights_micros.insert(lane, weight_micros);
    }
    let material_anchor: RetrievalAnchorId =
        fallback_policy_id(&format!("policy.{QUERY_FALLBACK_PROFILE_ID}.v1"))?;
    let mut profile = FusionProfile {
        profile_id: fallback_policy_id(&format!("profile.{QUERY_FALLBACK_PROFILE_ID}"))?,
        evaluation_result_anchor: material_anchor.clone(),
        calibrations,
        score_domain_calibrations,
        minimum_calibrated_feature_micros: BTreeMap::new(),
        weights_micros,
        diversity_policy_id: fallback_policy_id("diversity.candidate.v1")?,
        retrieval_budget: RetrievalBudget {
            max_candidates_per_lane: 32,
            max_fused_candidates: 32,
            max_hydrated_results: 16,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        },
    };
    let mut diversity = DiversityPolicy {
        policy_id: profile.diversity_policy_id.clone(),
        evaluation_result_anchor: Some(material_anchor),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: Some(2),
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    };
    let material = serde_json::to_vec(&(&profile, &diversity))
        .map_err(|error| QueryRuntimeMountErrorV1::InvalidFallbackPolicy(error.to_string()))?;
    let policy_anchor: RetrievalAnchorId = fallback_policy_id(&format!(
        "policy.{QUERY_FALLBACK_PROFILE_ID}.v1.sha256:{}",
        sha256_hex(&material)
    ))?;
    profile.evaluation_result_anchor = policy_anchor.clone();
    diversity.evaluation_result_anchor = Some(policy_anchor);
    Ok((profile, diversity))
}

/// Caller-owned, versioned lane policy for one raw query.
///
/// Query bytes are intentionally private and omitted from `Debug`; they are
/// consumed immediately by [`RawRetrievalRequestV1::sanitize`].
pub struct QuerySearchExecutionRequestV1 {
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
    pub lexical_routing: LexicalRoutingV1,
}

impl QuerySearchExecutionRequestV1 {
    pub fn new(query: impl Into<String>, policy: QuerySearchExecutionPolicyV1) -> Self {
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
            lexical_routing: policy.lexical_routing,
        }
    }
}

/// Non-secret execution policy supplied by the mounted MCP/application owner.
pub struct QuerySearchExecutionPolicyV1 {
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
    /// Additive lexical routes requested by the caller; query-only by default.
    pub lexical_routing: LexicalRoutingV1,
}

pub struct ExecutedQuerySearchV1 {
    pub generation: CodeGenerationId,
    pub authorized: AuthorizedQueryFallbackV1,
    pub sanitized: SanitizedRetrievalRequestV1,
    /// The generation-bound lanes answered from an older complete generation
    /// because no already-current generation was admissible. Recall is sound
    /// for `generation`; freshness is not. Callers must report the lanes as
    /// `CodeIndexLaneStatusV1::Stale` rather than complete.
    pub served_stale: bool,
    /// The lexical routes that ran and the per-candidate route evidence when
    /// additive routes were requested.
    pub lexical_routes: LexicalRouteReceiptV1,
}

#[derive(Debug, Error)]
pub enum QuerySearchExecutionErrorV1 {
    #[error("query search scope is invalid: {0}")]
    InvalidScope(String),
    #[error("no complete current code generation matches the exact admitted scope")]
    GenerationUnavailable,
    #[error("the exact code generation is mounted but still warming or unverified")]
    GenerationUnverified,
    #[error("exact generation read is unavailable: {0:?}")]
    ExactGenerationUnavailable(tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1),
    #[error("exact generation cursor does not match the selected code source")]
    ExactCursorInvalid,
    #[error("query authority is unavailable for the exact admitted scope")]
    AuthorityUnavailable,
    #[error("query search policy is invalid: {0}")]
    InvalidPolicy(String),
    #[error("query request or production lane boundary failed: {0}")]
    Retrieval(#[from] RetrievalPortError),
    #[error("query authorization/composition failed: {0}")]
    Authority(#[from] QueryAuthorityErrorV1),
}

struct HistoricalTextRequestControlV1<'a> {
    request: &'a dyn RetrievalExecutionControl,
}

impl CodeIndexExecutionControlV1 for HistoricalTextRequestControlV1<'_> {
    fn is_cancelled(&self) -> bool {
        self.request.is_cancelled()
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

impl CodeIndexSchedulerRegistryV1 {
    /// Execute exact, lexical, and graph independently against the newest
    /// complete generation for one exact scope, then pass their typed outcomes
    /// unchanged into the authenticated query composition authority.
    #[cfg(test)]
    pub async fn execute_query_search(
        &self,
        scope: &ResolvedScope,
        input: QuerySearchExecutionRequestV1,
    ) -> Result<ExecutedQuerySearchV1, QuerySearchExecutionErrorV1> {
        struct TestGraphControlV1;
        impl RetrievalExecutionControl for TestGraphControlV1 {
            fn is_cancelled(&self) -> bool {
                false
            }

            fn elapsed_micros(&self) -> u64 {
                0
            }
        }
        self.execute_controlled_query(scope, input, Arc::new(TestGraphControlV1))
            .await
    }

    #[hotpath::measure(label = "daemon.code_index.query.execute", future = true)]
    pub async fn execute_controlled_query<C>(
        &self,
        scope: &ResolvedScope,
        input: QuerySearchExecutionRequestV1,
        graph_control: Arc<C>,
    ) -> Result<ExecutedQuerySearchV1, QuerySearchExecutionErrorV1>
    where
        C: RetrievalExecutionControl + 'static,
    {
        scope
            .validate()
            .map_err(|error| QuerySearchExecutionErrorV1::InvalidScope(error.to_string()))?;
        validate_search_policy(&input)?;
        // Stale-while-revalidate, resolved serve-old first. The ready gate
        // admits only an *already current* generation, so it abstains for the
        // whole window of any rebuild — freshness unknown, git metadata moved,
        // staleness threshold elapsed. Every other callable code query keeps
        // serving the last complete generation through that window, and search
        // must not be the one lane that collapses.
        //
        // Resolution order is load-bearing, not cosmetic. Asking the ready gate
        // first made every query queue on the single-flight decode of whatever
        // generation was being activated, so a query with a perfectly servable
        // generation in hand still paid an O(store) sweep before it could reach
        // this fallback. Await-new must never preempt serve-old: check the O(1)
        // `serving_generation` first, and when it holds a complete generation,
        // resolve freshness through the decode-free ready probe. Only when
        // nothing is servable may the query await the in-flight decode, and
        // when no complete generation exists at all this stays a typed
        // fail-fast rather than degrading into an empty answer.
        let (latest, served_stale) = match self.latest_complete_serving_for_scope(scope).await {
            Some(serving) => match self.latest_complete_ready_decoded_for_scope(scope).await {
                Some(ready) => {
                    // The graph-bearing generation and the lightweight text
                    // projection can be restored through distinct handles.
                    // The mounted text slot is the canonical exact/lexical
                    // owner, so use its ready owner when the graph ready gate
                    // has proved the same generation current. Falling back to
                    // the decoded generation's independent warming handle
                    // would report `generation_unverified` forever even after
                    // the background projection had finished.
                    if let Some(text) = self.latest_text_serving_for_scope(scope).await {
                        if text.metadata().manifest().generation_id
                            == ready.generation().manifest().generation_id
                        {
                            return execute_query_search_on_text(
                                self,
                                scope,
                                input,
                                text,
                                Some(ready),
                                false,
                                graph_control,
                            )
                            .await;
                        }
                        if let Some((text, true)) =
                            self.latest_text_serving_freshness_for_scope(scope).await
                        {
                            return execute_query_search_on_text(
                                self,
                                scope,
                                input,
                                text,
                                None,
                                false,
                                graph_control,
                            )
                            .await;
                        }
                    }
                    (ready, false)
                }
                None => {
                    // Graph decode/activation is optional enrichment. Its
                    // readiness gate may abstain while the authenticated text
                    // owner for the same generation is already current. Keep
                    // exact and lexical truthful in that window without
                    // awaiting the decode. If text has advanced beyond the
                    // seated graph, serve that newer text generation alone;
                    // the graph lane remains typed unavailable until its own
                    // generation catches up.
                    match self.latest_text_serving_freshness_for_scope(scope).await {
                        Some((text, true))
                            if text.metadata().manifest().generation_id
                                == serving.generation().manifest().generation_id =>
                        {
                            (serving, false)
                        }
                        Some((text, true)) => {
                            return execute_query_search_on_text(
                                self,
                                scope,
                                input,
                                text,
                                None,
                                false,
                                graph_control,
                            )
                            .await;
                        }
                        Some((_, false)) | None => (serving, true),
                    }
                }
            },
            None => {
                if let Some((text, current)) =
                    self.latest_text_serving_freshness_for_scope(scope).await
                {
                    return execute_query_search_on_text(
                        self,
                        scope,
                        input,
                        text,
                        None,
                        !current,
                        graph_control,
                    )
                    .await;
                }
                match self.latest_complete_ready_for_scope(scope).await {
                    Some(ready) => (ready, false),
                    None => {
                        // Nothing servable and the ready gate refused. Search is the
                        // one lane whose resolution never runs the freshness ladder,
                        // so nothing else on this path will ever request the rebuild
                        // that would remedy the failure — it would return this typed
                        // error forever. Ask for the remedy exactly once per
                        // admission (debounced on the pending wake), never inline and
                        // never parking, then still fail typed rather than degrade
                        // into an empty answer.
                        self.request_query_background_reconcile(scope).await;
                        let unverified = self.generation_is_unverified_for_scope(scope).await;
                        return Err(if unverified {
                            QuerySearchExecutionErrorV1::GenerationUnverified
                        } else {
                            QuerySearchExecutionErrorV1::GenerationUnavailable
                        });
                    }
                }
            }
        };
        execute_query_search_on_latest(self, scope, input, latest, served_stale, graph_control)
            .await
    }

    pub async fn execute_query_search_on_generation<C>(
        &self,
        scope: &ResolvedScope,
        input: QuerySearchExecutionRequestV1,
        latest: super::LatestCompleteCodeIndexV1,
        graph_control: Arc<C>,
    ) -> Result<ExecutedQuerySearchV1, QuerySearchExecutionErrorV1>
    where
        C: RetrievalExecutionControl + 'static,
    {
        scope
            .validate()
            .map_err(|error| QuerySearchExecutionErrorV1::InvalidScope(error.to_string()))?;
        validate_search_policy(&input)?;
        // Checkout-identity gate: the caller pinned this generation
        // explicitly, so a foreign project/repository/worktree is refused
        // while a branch-label difference stays servable — the sealed
        // reference is attribution, not identity (see
        // [`super::registry::latest_matches_scope_identity`]).
        if !super::registry::latest_matches_scope_identity(&latest, scope) {
            return Err(QuerySearchExecutionErrorV1::GenerationUnavailable);
        }
        let text = latest.text_generation_handle();
        let request_control = HistoricalTextRequestControlV1 {
            request: graph_control.as_ref(),
        };
        if !text.finish_query_owner_warmup_for_request(&request_control)? {
            return Err(QuerySearchExecutionErrorV1::GenerationUnverified);
        }
        execute_query_search_on_latest(self, scope, input, latest, false, graph_control).await
    }
}

async fn execute_query_search_on_latest<C>(
    schedulers: &CodeIndexSchedulerRegistryV1,
    scope: &ResolvedScope,
    input: QuerySearchExecutionRequestV1,
    latest: super::LatestCompleteCodeIndexV1,
    served_stale: bool,
    graph_control: Arc<C>,
) -> Result<ExecutedQuerySearchV1, QuerySearchExecutionErrorV1>
where
    C: RetrievalExecutionControl + 'static,
{
    let text = latest.text_generation_handle();
    execute_query_search_on_text(
        schedulers,
        scope,
        input,
        text,
        Some(latest),
        served_stale,
        graph_control,
    )
    .await
}

async fn execute_query_search_on_text<C>(
    schedulers: &CodeIndexSchedulerRegistryV1,
    scope: &ResolvedScope,
    input: QuerySearchExecutionRequestV1,
    text: super::LatestCodeTextGenerationV1,
    graph_latest: Option<super::LatestCompleteCodeIndexV1>,
    served_stale: bool,
    graph_control: Arc<C>,
) -> Result<ExecutedQuerySearchV1, QuerySearchExecutionErrorV1>
where
    C: RetrievalExecutionControl + 'static,
{
    let authority = schedulers
        .query_authority_for_scope(scope)
        .await
        .ok_or(QuerySearchExecutionErrorV1::AuthorityUnavailable)?;
    let metadata = text.metadata();
    let generation = metadata.manifest().generation_id.clone();
    let request = RetrievalRequest {
        principal: input.principal,
        scope: RetrievalScope {
            privacy_domain: metadata.manifest().privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: metadata.snapshot().repository.clone(),
                worktree: metadata.snapshot().worktree.clone(),
                reference: metadata.snapshot().reference.clone(),
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(
                metadata.manifest().snapshot_digest.as_str(),
            )
            .map_err(|error| QuerySearchExecutionErrorV1::InvalidPolicy(error.to_string()))?,
            authorization_revision: input.authorization_revision,
            captured_at: metadata.manifest().seal.sealed_at,
        },
        profile_id: authority.profile().profile_id.clone(),
        budget: authority.profile().retrieval_budget,
    };
    // Serving-phase decomposition under the one `daemon.code_index.query.execute`
    // lifetime: admission (sanitize + production owner resolution), one span per
    // retrieval lane, and composition/encode below. The lanes run sequentially,
    // so their spans are disjoint slices of the outer wall time.
    let (sanitized, owners) = hotpath::measure_block!("daemon.code_index.query.admission", {
        let sanitized = RawRetrievalRequestV1::new(input.query, request)
            .sanitize(input.sanitizer_revision, input.normalization_revision)?;
        let readiness = text.query_owner_readiness();
        if !matches!(&readiness, CodeTextQueryOwnerReadinessV1::Ready(_))
            || text.text_projection_needs_work()
        {
            schedulers.request_query_background_reconcile(scope).await;
        }
        let CodeTextQueryOwnerReadinessV1::Ready(owners) = readiness else {
            return Err(QuerySearchExecutionErrorV1::GenerationUnverified);
        };
        (sanitized, owners)
    });
    let request = sanitized.request();
    let query_view = sanitized.query_view();
    let parser = CentralExactAdmissionAuthorityV1::new(input.exact_rule_revision);
    let exact = hotpath::measure_block!("daemon.code_index.query.lane.exact", {
        owners.retrieve_exact(&ExactLaneRequest {
            base: request.clone(),
            query_view,
            generation: generation.clone(),
            literals: parser.parse_literals(query_view, request),
            budget: request.budget,
        })
    })?;
    let route_plan = LexicalRoutePlanV1::plan(query_view.as_str(), &input.lexical_routing)?;
    let (lexical, lexical_routes) =
        hotpath::measure_block!("daemon.code_index.query.lane.lexical", {
            let mut route_outcomes = Vec::with_capacity(route_plan.routes().len());
            for route in route_plan.routes() {
                let outcome = owners.retrieve_lexical(&LexicalLaneRequest {
                    base: request.clone(),
                    query_view,
                    generation: generation.clone(),
                    whole_terms: route.parts.whole_terms.clone(),
                    subtokens: route.parts.subtokens.clone(),
                    phrases: route.parts.phrases.clone(),
                    proximities: route.proximities.clone(),
                    field_filters: route.field_filters.clone(),
                    fuzzy_budget: input.fuzzy_budget,
                    lexical_profile_revision: input.lexical_profile_revision.clone(),
                    score_domain: input.lexical_score_domain.clone(),
                    budget: request.budget,
                    control: graph_control.as_ref(),
                })?;
                route_outcomes.push(LexicalRouteOutcomeV1 {
                    kind: route.kind.clone(),
                    outcome,
                });
            }
            merge_lexical_routes(
                &generation,
                &request.budget,
                &request.budget,
                route_outcomes,
            )
        })?;
    let graph_seeds = graph_seeds_from_outcomes(&exact, &lexical);
    let graph = hotpath::measure_block!("daemon.code_index.query.lane.graph", {
        // Graph retrieval requires at least one seed. An empty seed list is
        // "the lane had nothing to expand", not "the retriever is missing",
        // once a generation has seated native graph serving. Reporting
        // Unavailable here made a terminal delete/miss search look like a
        // seating failure (`retriever_unavailable`) after exact and lexical
        // had already completed. Text-only or still-pending generations keep
        // the typed unavailable receipt.
        //
        // Graph activation state is owned per sealed generation and shared by
        // every handle bound to it, so the text owner answers for its own
        // generation when no decoded complete generation accompanies it. A
        // clean restart whose retained revision-7 head recovered serves graph
        // reads from exactly that owner and deliberately leaves the sealed
        // seat empty; resolving the lane only through the seat reported the
        // recovered graph as `retriever_unavailable` until the next publish.
        let graph_serving = graph_latest
            .as_ref()
            .map_or_else(
                || text.production_graph_serving(),
                |latest| latest.production_graph_serving(),
            )
            .ok();
        if graph_seeds.is_empty() {
            if graph_serving.is_some() {
                RetrieverOutcome::Complete(RetrieverBatch {
                    candidates: Vec::new(),
                    evidence_by_occurrence: BTreeMap::default(),
                    coverage: RetrieverCoverage::default(),
                    continuation: None,
                })
            } else {
                RetrieverOutcome::Unavailable(RetrievalFailure::AuthorityUnavailable {
                    detail: "exact and lexical lanes produced no graph seed".to_owned(),
                })
            }
        } else if let Some(graph_serving) = graph_serving {
            graph_serving.graph.retrieve_graph(
                &GraphLaneRequest {
                    base: request.clone(),
                    generation: generation.clone(),
                    seed_anchors: graph_seeds,
                    edge_kinds: input.graph_edge_kinds,
                    max_depth: input.graph_max_depth,
                    budget: request.budget,
                },
                graph_control,
            )?
        } else {
            RetrieverOutcome::Unavailable(RetrievalFailure::AuthorityUnavailable {
                detail: "persistent code graph is unavailable for this generation".to_owned(),
            })
        }
    });
    let lanes = vec![
        CompositionLaneInput::new(RetrieverKind::ExactLiteral, exact)
            .map_err(QueryAuthorityErrorV1::from)?,
        CompositionLaneInput::new(RetrieverKind::Lexical, lexical)
            .map_err(QueryAuthorityErrorV1::from)?,
        CompositionLaneInput::new(RetrieverKind::Graph, graph)
            .map_err(QueryAuthorityErrorV1::from)?,
    ];
    // The caller's page size is an upper bound on results, while
    // composition pagination refuses any page larger than the accepted
    // profile's deterministic budget (`max_fused_candidates`). Serve
    // budget-bounded pages instead of failing every request whose limit
    // exceeds the evaluated budget. Nothing is silently dropped: a page
    // smaller than the fused set always returns a continuation cursor.
    let page_size = input
        .page_size
        .min(request.budget.max_fused_candidates as usize);
    // Encode phase: fusion, pagination, and cursor encoding under the
    // composition authority.
    let authorized = hotpath::future!(
        schedulers.compose_query_fallback(
            scope,
            request,
            query_view,
            lanes,
            page_size,
            input.cursor.as_ref(),
        ),
        label = "daemon.code_index.query.compose"
    )
    .await?;
    // Retrieval-pipeline observation from the composition this query actually
    // ran; an uninstalled lane records nothing.
    if let Some(observability) = schedulers.index_observability_for_scope(scope).await {
        observability.record_retrieval_composition(&authorized, &request.budget);
    }
    // Names what was served and why at the one site every search and context
    // answer passes through: a lane that is partial from the current complete
    // generation (for example lexical `candidate_sources_pruned`, with the
    // pruned terms and their document frequencies) is a query policy bound,
    // not an unconverged index (#917).
    if authorized
        .composition
        .internal_lane_outcomes
        .values()
        .any(|outcome| !matches!(outcome, RetrieverOutcome::Complete(())))
    {
        tracing::info!(
            event = "code_index_query_lane_coverage",
            generation = generation.as_str(),
            served_stale,
            lane_outcomes = ?authorized.composition.internal_lane_outcomes,
            "code-index query served with degraded lane coverage"
        );
    }
    Ok(ExecutedQuerySearchV1 {
        generation,
        authorized,
        sanitized,
        served_stale,
        lexical_routes,
    })
}

fn validate_search_policy(
    input: &QuerySearchExecutionRequestV1,
) -> Result<(), QuerySearchExecutionErrorV1> {
    input
        .authorization_revision
        .validate()
        .map_err(|error| QuerySearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .sanitizer_revision
        .validate()
        .map_err(|error| QuerySearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .normalization_revision
        .validate()
        .map_err(|error| QuerySearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .exact_rule_revision
        .validate()
        .map_err(|error| QuerySearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .lexical_profile_revision
        .validate()
        .map_err(|error| QuerySearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    input
        .lexical_score_domain
        .validate()
        .map_err(|error| QuerySearchExecutionErrorV1::InvalidPolicy(error.to_string()))?;
    if input.page_size == 0 {
        return Err(QuerySearchExecutionErrorV1::InvalidPolicy(
            "page size must be positive".to_owned(),
        ));
    }
    if input.graph_max_depth == 0 || input.graph_edge_kinds.is_empty() {
        return Err(QuerySearchExecutionErrorV1::InvalidPolicy(
            "graph depth and edge-kind policy must be non-empty".to_owned(),
        ));
    }
    let unique_edges = input
        .graph_edge_kinds
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if unique_edges.len() != input.graph_edge_kinds.len() {
        return Err(QuerySearchExecutionErrorV1::InvalidPolicy(
            "graph edge kinds must be unique".to_owned(),
        ));
    }
    Ok(())
}

fn graph_seeds_from_outcomes(
    exact: &RetrieverOutcome<tracedecay_domain::RetrieverBatch<ExactLaneEvidence>>,
    lexical: &RetrieverOutcome<tracedecay_domain::RetrieverBatch<LexicalLaneEvidence>>,
) -> Vec<tracedecay_query::retrieval::ports::CodeCandidateBindingV1> {
    let mut seeds = Vec::new();
    let mut seen_occurrences = BTreeSet::new();
    let mut seen_symbols = BTreeSet::new();
    let mut add_batch =
        |bindings: Vec<&tracedecay_query::retrieval::ports::CodeCandidateBindingV1>| {
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
    use std::collections::BTreeSet;

    use tracedecay_domain::{
        ComponentRevision, PrivacyDomainId, RetrievalCursorKeyId, RetrieverKind,
    };
    use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
    use tracedecay_query::retrieval::{QUERY_RANKING_REVISION_V1, QueryAuthorityV1};

    #[test]
    fn checked_in_fallback_policy_mounts_as_a_fallback_query_authority() {
        let (profile, diversity) = super::core_query_policy().expect("checked-in core policy");
        assert_eq!(profile.profile_id.as_str(), "profile.query-fallback");
        assert_eq!(
            profile
                .weights_micros
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            RetrieverKind::QUERY_FALLBACK_LANES.into_iter().collect(),
        );
        assert_eq!(profile.weights_micros[&RetrieverKind::Graph], 250_000);
        assert_eq!(diversity.per_file, Some(2));
        assert!(
            profile
                .evaluation_result_anchor
                .as_str()
                .starts_with("policy.query-fallback.v1.sha256:")
        );

        let privacy_domain = PrivacyDomainId::new("privacy.query.fixture").expect("privacy domain");
        let keyring = RetrievalCursorKeyringV1::new(
            privacy_domain,
            RetrievalCursorKeyId::new("retrieval-key.query.fixture").expect("key id"),
            1,
            vec![7_u8; 32],
            1_000_000,
        )
        .expect("keyring");
        let ranking_revision =
            ComponentRevision::new(QUERY_RANKING_REVISION_V1).expect("ranking revision");
        QueryAuthorityV1::new(profile, diversity, ranking_revision, keyring)
            .expect("fallback policy is accepted by the fallback authority mode");
    }
}
