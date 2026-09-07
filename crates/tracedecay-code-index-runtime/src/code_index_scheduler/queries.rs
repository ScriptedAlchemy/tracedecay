//! Callable application operations over the latest sealed query generation.
//!
//! This is the production consumer of the exact, lexical, and graph owners.
//! It selects one already-mounted worktree generation and translates the
//! generic lane evidence into the typed application-operation records.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::future::Future;
#[cfg(test)]
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde::Serialize;

use tracedecay_application::retrieval::{
    CodeFacetDimension, CodeFacetRecord, CodeFacetRequest, CodeLexicalField, CodeNavigationRequest,
    CodeTimelineRecord, CodeTimelineRequest, SymbolPrimitiveRecord, SymbolRelationRecord,
    TypeHierarchyRecord,
};
use tracedecay_application::{
    CallableCodeQueryFuture, CallableCodeQueryPort, CancellationObservation, CancellationStage,
    CodeHierarchyRequest, CodeImpactRequest, CodeImplementationsRequest, CodeOccurrenceRecord,
    CodeQueryPage, CodeRelationRequest, CodeSignatureRequest, CodeSymbolSearchRequest,
    CoverageCompleteness, CoverageDomainState, EvidenceCoverage, EvidenceDomain,
    ExactOccurrenceRecord, ExactOccurrenceRequest, FreshnessState, LexicalOccurrenceRecord,
    ModuleApiRequest, Omission, OmissionReason, OpaqueCursor, OperationBudgetUsage, PageCursor,
    PageState, PhraseSearchRequest, QualifiedNameRequest, RequestAdmission, RequestContext,
    RetrievalEvidence, RetrievalPortContext, RetrievalPortOutcome, SourceMetadataRecord,
    SourceMetadataRequest, TemporalState,
};
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, CodeSearchChunkId, ComponentRevision,
    ExactAdmissionRuleRevision, FileOccurrenceId, FreshnessVectorDigest, ManifestDigest,
    PrincipalId, QueryNormalizationRevision, RelationEdgeKindV1, RetrievalAnchorId,
    RetrievalBudget, RetrievalBudgetUsage, RetrievalFailure, RetrievalRequest, RetrievalScope,
    RetrievalSnapshot, SanitizerRevision, ScoreDomainId, SingleRootScopeV1, SourceOccurrenceId,
    SymbolOccurrenceId, TemporalModeV1, UtcMicros, VectorWatermark, canonical_sha256,
};
#[cfg(test)]
use tracedecay_semantic_contracts::SemanticFallbackReasonV1;
use tracedecay_tool_catalog::SortContractId;

use super::{
    CodeIndexSchedulerRegistryV1, DaemonCodeIndexPublicationStoreV1, LatestCodeTextGenerationV1,
    LatestCompleteCodeIndexV1, ProductionCodeIndexQueryOwnersV1,
    registry::{UniqueMountedWorktree, latest_matches_scope_identity, unique_mounted_for_scope},
};
use tracedecay_query::code_search;
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLaneRequest,
};
use tracedecay_query::retrieval::graph::{GraphLaneRequest, GraphLaneRetriever};
use tracedecay_query::retrieval::lexical::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneRequest,
};
use tracedecay_query::retrieval::ports::{CodeCandidateBindingV1, CodeOccurrenceRefV1};
use tracedecay_query::retrieval::{
    AdmittedGenerationContextV1, NativeCodeOccurrenceV1, NativeExactRecordV1, NativeGraphRecordV1,
    NativeLaneOutcomeV1, NativeLanePageV1, NativeLexicalRecordV1, NativeRecordReadPortV1,
    NativeSymbolRecordV1, PreparedQueryBindingsV1, PreparedQueryErrorV1,
    PreparedQueryRoutingBindingsV1, PreparedQueryV1, QueryExecutionContractErrorV1,
    route_authenticated_prepared_query_cursor,
};

const CALLABLE_CODE_SORT: &str = "sort.application.code-index.v1";
const MAX_GENERATION_RESOLUTION_WAIT: Duration = Duration::from_secs(30);
/// Leave enough of the carried dispatch budget for timeout projection and the
/// typed response to cross the enclosing boundary.
const GENERATION_RESOLUTION_SETTLEMENT_MARGIN: Duration = Duration::from_secs(1);

type GenerationResolutionResultV1<T> =
    Result<Option<T>, code_search::CodeIndexSearchUnavailableReasonV1>;

enum GenerationResolutionSettlementV1<T> {
    Completed(GenerationResolutionResultV1<T>),
    JoinFailed,
    Terminated(code_search::CodeIndexSearchUnavailableReasonV1),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenerationResolutionTerminalV1 {
    Ready,
    Unavailable,
    Failed,
}

fn finish_generation_resolution_with<T>(
    settlement: GenerationResolutionSettlementV1<T>,
    observe: impl FnOnce(GenerationResolutionTerminalV1),
) -> GenerationResolutionResultV1<T> {
    let result = match settlement {
        GenerationResolutionSettlementV1::Completed(result) => result,
        GenerationResolutionSettlementV1::JoinFailed => {
            Err(code_search::CodeIndexSearchUnavailableReasonV1::Internal)
        }
        GenerationResolutionSettlementV1::Terminated(reason) => Err(reason),
    };
    observe(match &result {
        Ok(Some(_)) => GenerationResolutionTerminalV1::Ready,
        Ok(None) => GenerationResolutionTerminalV1::Unavailable,
        Err(_) => GenerationResolutionTerminalV1::Failed,
    });
    result
}

fn finish_generation_resolution<T>(
    settlement: GenerationResolutionSettlementV1<T>,
) -> GenerationResolutionResultV1<T> {
    finish_generation_resolution_with(settlement, |terminal| {
        #[cfg(feature = "hotpath")]
        match terminal {
            GenerationResolutionTerminalV1::Ready => {
                hotpath::gauge!("query.generation.resolve.outcome.ready_total").inc(1_u64);
            }
            GenerationResolutionTerminalV1::Unavailable => {
                hotpath::gauge!("query.generation.resolve.outcome.unavailable_total").inc(1_u64);
            }
            GenerationResolutionTerminalV1::Failed => {
                hotpath::gauge!("query.generation.resolve.outcome.failed_total").inc(1_u64);
            }
        }
        #[cfg(not(feature = "hotpath"))]
        let _ = terminal;
    })
}

/// Validated once per process. Live query pages and the unavailable
/// constructor share this identifier; do not rebuild it on the failure path.
static CALLABLE_CODE_SORT_CONTRACT: LazyLock<SortContractId> = LazyLock::new(|| {
    SortContractId::new(CALLABLE_CODE_SORT).unwrap_or_else(|_| panic!("static sort id"))
});

fn empty_callable_page() -> PageState {
    PageState {
        sort_contract_id: CALLABLE_CODE_SORT_CONTRACT.clone(),
        sort_revision: 1,
        total: Some(0),
        returned: 0,
        cursor: None,
        expires_at: None,
    }
}

mod graph_control;
use graph_control::{CallableGraphExecutionControl, current_utc_micros, graph_budget_for_request};

/// The reserved [`CodeGenerationId`] a caller supplies to request ordinary
/// (unpinned) search: it pins no specific immutable generation, so the serving
/// generation is resolved through the three-tier freshness ladder to the latest
/// complete compatible generation (NEXT.md clause 8). Any other value is an
/// explicit caller pin that is served generation-bound and read-only.
pub const UNPINNED_LATEST_GENERATION_SENTINEL: &str = "code-generation:unpinned-latest.v1";

/// Construct the reserved unpinned-latest [`CodeGenerationId`] sentinel a caller
/// passes when it wants the freshness-resolved latest complete generation
/// rather than a pinned one.
#[cfg(test)]
pub fn unpinned_latest_generation() -> CodeGenerationId {
    CodeGenerationId::new(UNPINNED_LATEST_GENERATION_SENTINEL)
        .unwrap_or_else(|_| panic!("static unpinned-latest generation sentinel is valid"))
}

pub fn callable_query_sanitizer_revision() -> SanitizerRevision {
    SanitizerRevision::new(tracedecay_query::retrieval::QUERY_SANITIZER_REVISION_V1)
        .unwrap_or_else(|_| panic!("static sanitizer revision"))
}

pub fn callable_query_normalization_revision() -> QueryNormalizationRevision {
    QueryNormalizationRevision::new(tracedecay_query::retrieval::QUERY_NORMALIZATION_REVISION_V1)
        .unwrap_or_else(|_| panic!("static normalization revision"))
}

fn is_unpinned_latest(generation: &CodeGenerationId) -> bool {
    generation.as_str() == UNPINNED_LATEST_GENERATION_SENTINEL
}

#[cfg(test)]
pub fn semantic_mcp_reason(
    current_source: Option<&CodeGenerationId>,
    latest_code_generation: &CodeGenerationId,
    runtime_state: Option<&tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1>,
) -> &'static str {
    if let Some(source_generation) = current_source {
        return if source_generation == latest_code_generation {
            // A current vector generation alone cannot authorize influence
            // without an accepted calibration authority.
            "calibration_unavailable"
        } else {
            "semantic_generation_stale"
        };
    }
    match runtime_state {
        None => "semantic_runtime_unavailable",
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Unavailable {
            reason,
        }) => match reason {
            SemanticFallbackReasonV1::ConfigurationUnavailable => {
                "semantic_configuration_unavailable"
            }
            SemanticFallbackReasonV1::Downloading => "semantic_model_downloading",
            SemanticFallbackReasonV1::Verifying => "semantic_model_verifying",
            SemanticFallbackReasonV1::Loading => "semantic_model_loading",
            SemanticFallbackReasonV1::SelectedNotDownloaded => "semantic_model_not_downloaded",
            SemanticFallbackReasonV1::ModelFailed => "semantic_failed",
            SemanticFallbackReasonV1::Indexing => "semantic_indexing",
            _ => "semantic_runtime_unavailable",
        },
        Some(
            tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::SelectedNotDownloaded {
                ..
            },
        ) => "semantic_model_not_downloaded",
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Downloading {
            ..
        }) => "semantic_model_downloading",
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Verifying {
            ..
        }) => "semantic_model_verifying",
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Installed {
            ..
        }) => "semantic_model_installed",
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Loading { .. }) => {
            "semantic_model_loading"
        }
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Indexing {
            ..
        }) => "semantic_indexing",
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Current { .. }) => {
            "semantic_generation_incompatible"
        }
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Degraded {
            ..
        }) => "semantic_degraded",
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Rollback {
            ..
        }) => "semantic_rollback",
        Some(tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Failed { .. }) => {
            "semantic_failed"
        }
    }
}

impl CodeIndexSchedulerRegistryV1 {
    /// Compose real exact/lexical/graph lane outcomes only through the
    /// accepted profile and query/cursor key authority mounted for this exact
    /// admitted scope.
    pub async fn compose_query_fallback(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        request: &tracedecay_domain::RetrievalRequest,
        query_view: &tracedecay_domain::EphemeralSanitizedQueryViewV1,
        lanes: Vec<tracedecay_query::retrieval::fusion::CompositionLaneInput>,
        page_size: usize,
        cursor: Option<&tracedecay_domain::RetrievalCursor>,
    ) -> Result<
        tracedecay_query::retrieval::AuthorizedQueryFallbackV1,
        tracedecay_query::retrieval::QueryAuthorityErrorV1,
    > {
        let authority = self
            .query_authority_for_scope(scope)
            .await
            .ok_or(tracedecay_query::retrieval::QueryAuthorityErrorV1::AuthorityUnavailable)?;
        authority.compose(request, query_view, lanes, page_size, cursor)
    }

    /// Resolve strict-semantic availability against this project's freshest
    /// complete code generation.
    ///
    /// No semantic query is constructed until an accepted calibration
    /// authority exists. That keeps ordinary query fallback byte-stable and
    /// makes a strict request fail with a typed reason instead of inventing a
    /// score, profile, or candidate.
    #[cfg(test)]
    pub async fn semantic_mcp_abstention(
        &self,
        project_root: &Path,
    ) -> code_search::CodeIndexSemanticAbstentionV1 {
        let Some(latest) = self.latest_complete_fresh(project_root).await else {
            return code_search::CodeIndexSemanticAbstentionV1 {
                code_generation: None,
                reason: "code_index_unavailable",
            };
        };
        let code_generation = latest.generation.manifest().generation_id.clone();
        let code_generation_display = Some(code_generation.as_str().to_owned());
        let current_source =
            tracedecay_usecases::semantic_runtime::project_semantic_source_generation(project_root);
        let status = tracedecay_usecases::semantic_runtime::project_semantic_application_status(
            project_root,
            None,
        );
        let reason = semantic_mcp_reason(
            current_source.as_ref(),
            &code_generation,
            status.as_ref().map(|status| &status.state),
        );
        code_search::CodeIndexSemanticAbstentionV1 {
            code_generation: code_generation_display,
            reason,
        }
    }

    pub async fn generation_for(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, code_search::CodeIndexSearchUnavailableReasonV1>
    {
        self.generation_for_controlled(scope, generation_id, None)
            .await
    }

    #[hotpath::measure(future = true, label = "query.generation.resolve")]
    pub async fn generation_for_controlled(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        generation_id: &CodeGenerationId,
        control: Option<super::branch_generations::BranchGenerationReadControlV1>,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, code_search::CodeIndexSearchUnavailableReasonV1>
    {
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.generation.resolve.attempts_total").inc(1_u64);
        let (scheduler, serving_generation) = {
            let mounted = self.mounted.lock().await;
            match unique_mounted_for_scope(&mounted, scope) {
                UniqueMountedWorktree::One { worktree, .. } => (
                    std::sync::Arc::clone(&worktree.scheduler),
                    std::sync::Arc::clone(&worktree.serving_generation),
                ),
                UniqueMountedWorktree::None => {
                    return finish_generation_resolution(
                        GenerationResolutionSettlementV1::Completed(Ok(None)),
                    );
                }
                UniqueMountedWorktree::Ambiguous => {
                    return finish_generation_resolution(
                        GenerationResolutionSettlementV1::Completed(Err(
                            code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
                        )),
                    );
                }
            }
        };
        let scope = scope.clone();
        let generation_id = generation_id.clone();
        let terminal_control = control.clone();
        // The blocking side may park on the single-flight decode barrier for a
        // sealed generation, which is an O(store) sweep. Do not hold an admission
        // slot across it.
        let task = tokio::task::spawn_blocking(move || {
            if let Some(reason) = control
                .as_ref()
                .and_then(super::branch_generations::BranchGenerationReadControlV1::termination)
            {
                return Err(reason);
            }
            if let Some(generation) = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .filter(|generation| {
                    generation.generation.manifest().generation_id == generation_id
                        && latest_matches_scope_identity(generation, &scope)
                })
                .cloned()
            {
                #[cfg(feature = "hotpath")]
                hotpath::gauge!("query.generation.resolve.serving_hit_total").inc(1_u64);
                return Ok(Some(generation));
            }
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("query.generation.resolve.durable_load_total").inc(1_u64);
            let scheduler = hotpath::measure_block!("query.generation.resolve.scheduler_wait", {
                scheduler
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            });
            let generation = hotpath::measure_block!("query.generation.resolve.load", {
                scheduler
                    .generation(&generation_id)
                    .map_err(|error| match error {
                        super::CodeIndexSchedulerErrorV1::Production(
                            crate::code_index::production::CodeIndexProductionErrorV1::Publication(
                                error,
                            ),
                        ) => DaemonCodeIndexPublicationStoreV1::exact_read_error(error),
                        _ => code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                    })
            })?;
            Ok(generation.filter(|generation| latest_matches_scope_identity(generation, &scope)))
        });
        let settlement = match crate::ports::park_admission(
            crate::code_index_task_support::settle_owned_blocking_task(
                task,
                std::time::Duration::from_millis(10),
                || {
                    terminal_control.as_ref().and_then(
                        super::branch_generations::BranchGenerationReadControlV1::termination,
                    )
                },
            ),
        )
        .await
        {
            Ok(Ok(result)) => GenerationResolutionSettlementV1::Completed(result),
            Ok(Err(_)) => GenerationResolutionSettlementV1::JoinFailed,
            Err(reason) => GenerationResolutionSettlementV1::Terminated(reason),
        };
        finish_generation_resolution(settlement)
    }

    /// Resolve the generation a callable-code query serves.
    ///
    /// An explicit, caller-pinned generation is matched exactly and served
    /// generation-bound and read-only — the freshness ladder is deliberately
    /// bypassed so a pin is a stable, reproducible read. The reserved unpinned
    /// sentinel instead runs the three-tier freshness ladder and serves the
    /// latest complete compatible generation, so out-of-band changes are
    /// reconciled at query admission without any standing filesystem watcher.
    /// Partial, stale, failed, or incompatible generations never surface here:
    /// the ladder only ever returns the latest complete generation.
    pub async fn resolve_serving_generation(
        &self,
        request: &RequestContext,
        requested: &CodeGenerationId,
        page: &tracedecay_application::PageRequest,
        authority: &tracedecay_query::retrieval::QueryAuthorityV1,
        routing: &PreparedQueryRoutingBindingsV1,
    ) -> Result<LatestCompleteCodeIndexV1, CallableCodeCursorError> {
        let wait = remaining_generation_resolution_wait(request)
            .ok_or(CallableCodeCursorError::Unavailable)?;
        let resolution = async {
            if let Some(cursor) = page.cursor.as_ref() {
                let expected_generation = (!is_unpinned_latest(requested)).then_some(requested);
                let scope = request.scope().clone();
                route_authenticated_prepared_query_cursor(
                    authority,
                    routing,
                    cursor.as_str(),
                    current_utc_micros()?,
                    expected_generation,
                    |generation| async move { self.generation_for(&scope, &generation).await },
                )?
                .await
                .map_err(|_| CallableCodeCursorError::Unavailable)?
                .ok_or(CallableCodeCursorError::Unavailable)
            } else if is_unpinned_latest(requested) {
                self.latest_complete_fresh_for_scope(request.scope())
                    .await
                    .ok_or(CallableCodeCursorError::Unavailable)
            } else {
                self.generation_for(request.scope(), requested)
                    .await
                    .map_err(|_| CallableCodeCursorError::Unavailable)?
                    .ok_or(CallableCodeCursorError::Unavailable)
            }
        };
        let latest = tokio::time::timeout(wait, resolution)
            .await
            .map_err(|_| CallableCodeCursorError::Unavailable)??;
        if !matches!(
            request.admission_at(current_utc_micros()?),
            RequestAdmission::Admitted
        ) {
            return Err(CallableCodeCursorError::Unavailable);
        }
        Ok(latest)
    }

    async fn resolve_text_serving_generation(
        &self,
        request: &RequestContext,
        requested: &CodeGenerationId,
        page: &tracedecay_application::PageRequest,
        authority: &tracedecay_query::retrieval::QueryAuthorityV1,
        routing: &PreparedQueryRoutingBindingsV1,
    ) -> Result<LatestCodeTextGenerationV1, CallableCodeCursorError> {
        let wait = remaining_generation_resolution_wait(request)
            .ok_or(CallableCodeCursorError::Unavailable)?;
        let resolution = async {
            if let Some(cursor) = page.cursor.as_ref() {
                let expected_generation = (!is_unpinned_latest(requested)).then_some(requested);
                let scope = request.scope().clone();
                route_authenticated_prepared_query_cursor(
                    authority,
                    routing,
                    cursor.as_str(),
                    current_utc_micros()?,
                    expected_generation,
                    |generation| async move {
                        if let Some(latest) = self
                            .latest_text_serving_for_scope(&scope)
                            .await
                            .filter(|latest| {
                                latest.metadata().manifest().generation_id == generation
                            })
                        {
                            return Ok::<_, code_search::CodeIndexSearchUnavailableReasonV1>(Some(
                                latest,
                            ));
                        }
                        self.generation_for(&scope, &generation)
                            .await
                            .map(|latest| latest.map(|latest| latest.text_generation_handle()))
                    },
                )?
                .await
                .map_err(|_| CallableCodeCursorError::Unavailable)?
                .ok_or(CallableCodeCursorError::Unavailable)
            } else if is_unpinned_latest(requested) {
                self.latest_text_fresh_for_scope(request.scope())
                    .await
                    .ok_or(CallableCodeCursorError::Unavailable)
            } else if let Some(latest) = self
                .latest_text_serving_for_scope(request.scope())
                .await
                .filter(|latest| latest.metadata().manifest().generation_id == *requested)
            {
                Ok(latest)
            } else {
                self.generation_for(request.scope(), requested)
                    .await
                    .map_err(|_| CallableCodeCursorError::Unavailable)?
                    .map(|latest| latest.text_generation_handle())
                    .ok_or(CallableCodeCursorError::Unavailable)
            }
        };
        let latest = tokio::time::timeout(wait, resolution)
            .await
            .map_err(|_| CallableCodeCursorError::Unavailable)??;
        if !matches!(
            request.admission_at(current_utc_micros()?),
            RequestAdmission::Admitted
        ) {
            return Err(CallableCodeCursorError::Unavailable);
        }
        Ok(latest)
    }
}

fn typed<T>(value: impl Into<String>) -> Result<T, String>
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.into()).map_err(|error| error.to_string())
}

fn retrieval_budget(page_size: u32) -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane: page_size,
        max_fused_candidates: page_size,
        max_hydrated_results: page_size,
        max_hydration_bytes: u64::from(page_size).saturating_mul(65_536),
        deadline_micros: None,
    }
}

fn prepared_routing_bindings(
    context: &RetrievalPortContext<'_>,
    temporal_mode: TemporalModeV1,
    operation: &'static str,
    query_binding_digest: ManifestDigest,
    page_size: u32,
) -> Result<PreparedQueryRoutingBindingsV1, CallableCodeCursorError> {
    Ok(PreparedQueryRoutingBindingsV1 {
        operation: operation.to_owned(),
        scope_digest: context.request.scope().scope_digest.clone(),
        principal: typed::<PrincipalId>(context.request.actor().to_string())
            .map_err(|_| CallableCodeCursorError::Invalid)?,
        root: SingleRootScopeV1 {
            repository: context.request.scope().repository_id.clone(),
            worktree: Some(context.request.scope().worktree_id.clone()),
            reference: context.request.scope().reference.clone(),
        },
        temporal_mode,
        query_binding_digest,
        page_size,
        authorization_revision: AuthorizationRevision::new(format!(
            "authorization.grant.{}",
            context.request.grant().revision
        ))
        .map_err(|_| CallableCodeCursorError::Invalid)?,
    })
}

pub fn maximum_retrieval_budget() -> RetrievalBudget {
    retrieval_budget(tracedecay_application::MAX_APPLICATION_PAGE_SIZE)
}

type CallableCodeCursorError = PreparedQueryErrorV1;

fn query_finished_at() -> UtcMicros {
    current_utc_micros().unwrap_or(UtcMicros(0))
}

fn generation_resolution_wait_from_remaining(remaining: Duration) -> Duration {
    remaining
        .saturating_sub(GENERATION_RESOLUTION_SETTLEMENT_MARGIN)
        .min(MAX_GENERATION_RESOLUTION_WAIT)
}

fn remaining_generation_resolution_wait(request: &RequestContext) -> Option<Duration> {
    let now = current_utc_micros().ok()?;
    if request.admission_at(now) != RequestAdmission::Admitted {
        return None;
    }
    let remaining = request.deadline().expires_at.0.checked_sub(now.0)?;
    let remaining = u64::try_from(remaining).ok().map(Duration::from_micros)?;
    Some(generation_resolution_wait_from_remaining(remaining))
}

fn base_request(
    context: &RetrievalPortContext<'_>,
    latest: &LatestCompleteCodeIndexV1,
    temporal_mode: TemporalModeV1,
    profile: &tracedecay_domain::FusionProfile,
) -> Result<RetrievalRequest, String> {
    let generation = &latest.generation;
    Ok(RetrievalRequest {
        principal: typed::<PrincipalId>(context.request.actor().to_string())?,
        scope: RetrievalScope {
            privacy_domain: generation.manifest().privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: generation.snapshot().repository.clone(),
                worktree: generation.snapshot().worktree.clone(),
                reference: generation.snapshot().reference.clone(),
            },
        },
        temporal_mode,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(
                generation.manifest().snapshot_digest.as_str(),
            )
            .map_err(|error| error.to_string())?,
            authorization_revision: AuthorizationRevision::new(format!(
                "authorization.grant.{}",
                context.request.grant().revision
            ))
            .map_err(|error| error.to_string())?,
            captured_at: generation.manifest().seal.sealed_at,
        },
        profile_id: profile.profile_id.clone(),
        budget: profile.retrieval_budget,
    })
}

fn text_base_request(
    context: &RetrievalPortContext<'_>,
    latest: &LatestCodeTextGenerationV1,
    temporal_mode: TemporalModeV1,
    profile: &tracedecay_domain::FusionProfile,
) -> Result<RetrievalRequest, String> {
    let manifest = latest.metadata().manifest();
    let snapshot = latest.metadata().snapshot();
    Ok(RetrievalRequest {
        principal: typed::<PrincipalId>(context.request.actor().to_string())?,
        scope: RetrievalScope {
            privacy_domain: manifest.privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: snapshot.repository.clone(),
                worktree: snapshot.worktree.clone(),
                reference: snapshot.reference.clone(),
            },
        },
        temporal_mode,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(manifest.snapshot_digest.as_str())
                .map_err(|error| error.to_string())?,
            authorization_revision: AuthorizationRevision::new(format!(
                "authorization.grant.{}",
                context.request.grant().revision
            ))
            .map_err(|error| error.to_string())?,
            captured_at: manifest.seal.sealed_at,
        },
        profile_id: profile.profile_id.clone(),
        budget: profile.retrieval_budget,
    })
}

fn unavailable<T>(finished_at: tracedecay_domain::UtcMicros) -> RetrievalPortOutcome<T> {
    RetrievalPortOutcome::Unavailable(RetrievalEvidence {
        payload: None,
        temporal: TemporalState::current(finished_at),
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Symbol],
            visited: None,
            eligible: None,
            returned: 0,
            completeness: CoverageCompleteness::Unknown,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Symbol,
                completeness: CoverageCompleteness::Unknown,
            }],
        },
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: empty_callable_page(),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    })
}

fn unavailable_for_generation<T>(
    finished_at: tracedecay_domain::UtcMicros,
    generation: CodeGenerationId,
) -> RetrievalPortOutcome<T> {
    let RetrievalPortOutcome::Unavailable(mut evidence) = unavailable(finished_at) else {
        unreachable!("unavailable helper returns the unavailable variant")
    };
    evidence.temporal.source_generation = Some(generation);
    evidence.omissions.push(Omission {
        domain: EvidenceDomain::Symbol,
        count: 0,
        reason: OmissionReason::Unavailable,
    });
    RetrievalPortOutcome::Unavailable(evidence)
}

fn rejected_cursor<T>(
    finished_at: UtcMicros,
    generation: CodeGenerationId,
    error: CallableCodeCursorError,
) -> RetrievalPortOutcome<T> {
    let RetrievalPortOutcome::Unavailable(mut evidence) = unavailable(finished_at) else {
        unreachable!("unavailable helper returns the unavailable variant")
    };
    evidence.temporal.source_generation = Some(generation);
    evidence.omissions.push(Omission {
        domain: EvidenceDomain::Symbol,
        count: 0,
        reason: match error {
            CallableCodeCursorError::Stale => OmissionReason::Stale,
            CallableCodeCursorError::Invalid => OmissionReason::Failed,
            CallableCodeCursorError::Unavailable => OmissionReason::Unavailable,
        },
    });
    RetrievalPortOutcome::Unavailable(evidence)
}

fn bounded_result<T>(
    page: CodeQueryPage<T>,
    coverage: tracedecay_domain::RetrieverCoverage,
    finished_at: tracedecay_domain::UtcMicros,
    partial_reason: Option<OmissionReason>,
    cursor_expires_at: Option<UtcMicros>,
) -> RetrievalPortOutcome<CodeQueryPage<T>> {
    let returned = page.items.len() as u64;
    let represented = page.total.unwrap_or(returned);
    let mut temporal = TemporalState::current(finished_at);
    temporal.source_generation = Some(page.generation.clone());
    let is_partial = partial_reason.is_some()
        || coverage.capped > 0
        || coverage.unknown > 0
        || represented < coverage.eligible
        || coverage.examined < coverage.eligible;
    let completeness = if is_partial {
        CoverageCompleteness::Partial
    } else {
        CoverageCompleteness::Complete
    };
    let evidence_coverage = EvidenceCoverage {
        requested_domains: vec![EvidenceDomain::Symbol],
        visited: Some(coverage.examined),
        eligible: Some(coverage.eligible),
        returned,
        completeness,
        domains: vec![CoverageDomainState {
            domain: EvidenceDomain::Symbol,
            completeness,
        }],
    };
    let omissions = is_partial
        .then(|| Omission {
            domain: EvidenceDomain::Symbol,
            count: coverage
                .eligible
                .saturating_sub(represented)
                .max(coverage.capped.saturating_add(coverage.unknown)),
            reason: partial_reason.unwrap_or(OmissionReason::Budget),
        })
        .into_iter()
        .collect();
    // `first_page` rejects `returned > total`. Both numbers are store readings
    // on a live query, so disagreement is a stale count, not a caller error.
    let page_state =
        PageState::first_page(CALLABLE_CODE_SORT_CONTRACT.clone(), 1, page.total, returned);
    let mut page_state = match page_state {
        Ok(page_state) => page_state,
        Err(error) => {
            tracing::warn!(
                %error,
                "bounded code query page state failed the application contract"
            );
            return unavailable_for_generation(finished_at, page.generation.clone());
        }
    };
    page_state.cursor = page.next_cursor.clone().map(PageCursor::from);
    page_state.expires_at = cursor_expires_at;
    let evidence = RetrievalEvidence {
        page: page_state,
        payload: Some(page),
        temporal,
        evidence_authorities: Vec::new(),
        coverage: evidence_coverage,
        omissions,
        scores: Vec::new(),
        contributions: Vec::new(),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    };
    if is_partial {
        RetrievalPortOutcome::Partial(evidence)
    } else {
        RetrievalPortOutcome::Completed(evidence)
    }
}

fn path_is_in_code_query_scope(path: &str, scope: &tracedecay_application::CodeQueryScope) -> bool {
    tracedecay_runtime_core::path_scope::path_matches_scope(path, scope.path_prefix.as_deref())
}

fn relation_edge_kind_name(kind: RelationEdgeKindV1) -> &'static str {
    match kind {
        RelationEdgeKindV1::Calls => "calls",
        RelationEdgeKindV1::Uses => "uses",
        RelationEdgeKindV1::TypeOf => "typeof",
        RelationEdgeKindV1::Contains => "contains",
        RelationEdgeKindV1::Implements => "implements",
        RelationEdgeKindV1::Extends => "extends",
        RelationEdgeKindV1::Annotates => "annotates",
        RelationEdgeKindV1::Returns => "returns",
        RelationEdgeKindV1::Receives => "receives",
    }
}

fn ascii_contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    haystack
        .as_bytes()
        .windows(needle_lower.len())
        .any(|window| window.eq_ignore_ascii_case(needle_lower.as_bytes()))
}

fn ascii_starts_with_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    haystack
        .as_bytes()
        .get(..needle_lower.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(needle_lower.as_bytes()))
}

/// One-time point-lookup indices over a sealed generation's in-memory record
/// vectors.
///
/// Every map here replaces a linear `.iter().find(..)` scan that previously ran
/// once per retrieval candidate, making each serving lane
/// `O(candidates x records)`. Building the whole index costs a single
/// `O(files + chunks + symbols + edges)` pass, and
/// [`LatestCompleteCodeIndexV1::record_index`] memoizes it per generation so
/// concurrent queries share one build.
///
/// Equivalence rule: `Iterator::find` returns the *first* match, so duplicate
/// keys keep the lowest position (`or_insert` never overwrites) and the
/// adjacency lists stay in ascending position order. Indexed lookups therefore
/// return the same record the scan returned, and a key that is absent maps to
/// the same miss the scan produced.
pub struct GenerationRecordIndexV1 {
    files_by_occurrence: HashMap<FileOccurrenceId, usize>,
    chunks_by_id: HashMap<CodeSearchChunkId, usize>,
    symbols_by_occurrence: HashMap<SymbolOccurrenceId, usize>,
    chunk_by_symbol: HashMap<SymbolOccurrenceId, usize>,
    chunk_by_file_symbol: HashMap<(FileOccurrenceId, SymbolOccurrenceId), usize>,
    edges_from: HashMap<SymbolOccurrenceId, Vec<usize>>,
    edges_to: HashMap<SymbolOccurrenceId, Vec<usize>>,
    kind_facet_rows: Vec<(usize, usize)>,
    qualified_name_order: Vec<usize>,
    last_segment_order: Vec<usize>,
}

/// The final `::` segment of a qualified name, or the whole name when it has
/// none. `rsplit` always yields at least one item, so the fallback only
/// satisfies the type.
fn last_qualified_segment(qualified_name: &str) -> &str {
    qualified_name.rsplit("::").next().unwrap_or(qualified_name)
}

/// The sub-slice of `order` whose keys equal `selector`.
///
/// `order` must be sorted ascending by `key` with ties in ascending position
/// order, which is what [`GenerationRecordIndexV1::build`] produces; the
/// returned positions are therefore in the same ascending order a full scan
/// would have visited them.
fn sorted_positions_for<'a>(
    order: &'a [usize],
    selector: &str,
    key: impl Fn(usize) -> &'a str,
) -> &'a [usize] {
    let start = order.partition_point(|&position| key(position) < selector);
    let end = start + order[start..].partition_point(|&position| key(position) == selector);
    &order[start..end]
}

impl GenerationRecordIndexV1 {
    pub fn build(
        generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    ) -> Self {
        let files = &generation.snapshot().files;
        let mut files_by_occurrence = HashMap::with_capacity(files.len());
        for (position, file) in files.iter().enumerate() {
            files_by_occurrence
                .entry(file.file_occurrence_id.clone())
                .or_insert(position);
        }

        let chunks = generation.chunks().chunks();
        let mut chunks_by_id = HashMap::with_capacity(chunks.len());
        let mut chunk_by_symbol = HashMap::new();
        let mut chunk_by_file_symbol = HashMap::new();
        for (position, chunk) in chunks.iter().enumerate() {
            chunks_by_id.entry(chunk.id.clone()).or_insert(position);
            if let Some(symbol) = chunk.anchor.symbol_occurrence_id.as_ref() {
                chunk_by_symbol.entry(symbol.clone()).or_insert(position);
                chunk_by_file_symbol
                    .entry((chunk.anchor.file_occurrence_id.clone(), symbol.clone()))
                    .or_insert(position);
            }
        }

        let symbols = &generation.symbols().symbols;
        let mut symbols_by_occurrence = HashMap::with_capacity(symbols.len());
        for (position, record) in symbols.iter().enumerate() {
            symbols_by_occurrence
                .entry(record.occurrence.clone())
                .or_insert(position);
        }

        let mut qualified_name_order = (0..symbols.len()).collect::<Vec<_>>();
        qualified_name_order.sort_unstable_by(|left, right| {
            symbols[*left]
                .qualified_name
                .cmp(&symbols[*right].qualified_name)
                .then(left.cmp(right))
        });
        let mut last_segment_order = (0..symbols.len()).collect::<Vec<_>>();
        last_segment_order.sort_unstable_by(|left, right| {
            last_qualified_segment(&symbols[*left].qualified_name)
                .cmp(last_qualified_segment(&symbols[*right].qualified_name))
                .then(left.cmp(right))
        });

        // Keep only the symbol/file joins that `symbol_record_by_id` could
        // resolve. Facet reads can then use the borrowed lineage kind and file
        // path directly instead of allocating a complete application record
        // for every symbol on every request.
        let mut kind_facet_rows = Vec::with_capacity(symbols.len());
        for (symbol_position, symbol) in symbols.iter().enumerate() {
            let Some(chunk_position) = chunk_by_symbol.get(&symbol.occurrence) else {
                continue;
            };
            let file_occurrence = &chunks[*chunk_position].anchor.file_occurrence_id;
            let Some(file_position) = files_by_occurrence.get(file_occurrence) else {
                continue;
            };
            kind_facet_rows.push((symbol_position, *file_position));
        }

        let mut edges_from: HashMap<SymbolOccurrenceId, Vec<usize>> = HashMap::new();
        let mut edges_to: HashMap<SymbolOccurrenceId, Vec<usize>> = HashMap::new();
        for (position, edge) in generation.edges().iter().enumerate() {
            edges_from
                .entry(edge.from_occurrence.clone())
                .or_default()
                .push(position);
            edges_to
                .entry(edge.to_occurrence.clone())
                .or_default()
                .push(position);
        }

        Self {
            files_by_occurrence,
            chunks_by_id,
            symbols_by_occurrence,
            chunk_by_symbol,
            chunk_by_file_symbol,
            edges_from,
            edges_to,
            kind_facet_rows,
            qualified_name_order,
            last_segment_order,
        }
    }

    /// Position of the first snapshot file with this occurrence identity.
    pub fn file_position(&self, file: &FileOccurrenceId) -> Option<usize> {
        self.files_by_occurrence.get(file).copied()
    }

    /// Position of the first chunk with this chunk identity.
    pub fn chunk_position(&self, chunk: &CodeSearchChunkId) -> Option<usize> {
        self.chunks_by_id.get(chunk).copied()
    }

    /// Position of the first lineage symbol record with this occurrence.
    pub fn symbol_position(&self, symbol: &SymbolOccurrenceId) -> Option<usize> {
        self.symbols_by_occurrence.get(symbol).copied()
    }

    /// Position of the first chunk anchored to this symbol occurrence.
    pub fn chunk_position_for_symbol(&self, symbol: &SymbolOccurrenceId) -> Option<usize> {
        self.chunk_by_symbol.get(symbol).copied()
    }

    /// Position of the first chunk anchored to this file and symbol pair.
    pub fn chunk_position_for_file_symbol(
        &self,
        file: &FileOccurrenceId,
        symbol: &SymbolOccurrenceId,
    ) -> Option<usize> {
        self.chunk_by_file_symbol
            .get(&(file.clone(), symbol.clone()))
            .copied()
    }

    /// Ascending positions of the edges incident to `symbol` in the requested
    /// direction, preserving the order a full edge scan would have produced.
    pub fn incident_edge_positions(&self, symbol: &SymbolOccurrenceId, reverse: bool) -> &[usize] {
        let adjacency = if reverse {
            self.edges_to.get(symbol)
        } else {
            self.edges_from.get(symbol)
        };
        adjacency.map_or(&[][..], Vec::as_slice)
    }

    /// Resolvable symbol/file rows in canonical symbol position order.
    pub fn kind_facet_rows(&self) -> &[(usize, usize)] {
        &self.kind_facet_rows
    }

    /// Canonical symbol positions whose full qualified name equals `selector`.
    pub fn qualified_name_positions<'a>(
        &'a self,
        symbols: &'a [Arc<tracedecay_code_index::lineage::LineageSymbolRecordV1>],
        selector: &str,
    ) -> &'a [usize] {
        sorted_positions_for(&self.qualified_name_order, selector, |position| {
            symbols[position].qualified_name.as_str()
        })
    }

    /// Canonical symbol positions whose last qualified segment equals `selector`.
    pub fn last_segment_positions<'a>(
        &'a self,
        symbols: &'a [Arc<tracedecay_code_index::lineage::LineageSymbolRecordV1>],
        selector: &str,
    ) -> &'a [usize] {
        sorted_positions_for(&self.last_segment_order, selector, |position| {
            last_qualified_segment(&symbols[position].qualified_name)
        })
    }
}

struct LatestCompleteNativeRecordReadPortV1<'a> {
    latest: &'a LatestCompleteCodeIndexV1,
}

impl NativeRecordReadPortV1 for LatestCompleteNativeRecordReadPortV1<'_> {
    fn generation(&self) -> &CodeGenerationId {
        &self.latest.generation.manifest().generation_id
    }

    fn occurrence(
        &self,
        binding: &CodeCandidateBindingV1,
    ) -> Result<NativeCodeOccurrenceV1, QueryExecutionContractErrorV1> {
        if &binding.occurrence.generation != self.generation() {
            return Err(QueryExecutionContractErrorV1::GenerationMismatch);
        }
        let index = self.latest.record_index();
        let file = index
            .file_position(&binding.occurrence.file)
            .map(|position| &self.latest.generation.snapshot().files[position])
            .ok_or(QueryExecutionContractErrorV1::RecordUnavailable)?;
        let chunk = match binding.occurrence.chunk.as_ref() {
            Some(chunk_id) => Some(
                index
                    .chunk_position(chunk_id)
                    .map(|position| &self.latest.generation.chunks().chunks()[position])
                    .ok_or(QueryExecutionContractErrorV1::RecordUnavailable)?,
            ),
            None => None,
        };
        Ok(NativeCodeOccurrenceV1 {
            file: binding.occurrence.file.clone(),
            symbol: binding.occurrence.symbol.clone(),
            chunk: binding.occurrence.chunk.clone(),
            path: file.logical_path.clone(),
            span: chunk.map_or(
                tracedecay_domain::SourceSpan {
                    start_byte: 0,
                    end_byte: 0,
                },
                |chunk| chunk.anchor.source_span,
            ),
        })
    }

    fn occurrence_by_chunk(
        &self,
        chunk_id: &CodeSearchChunkId,
    ) -> Result<NativeCodeOccurrenceV1, QueryExecutionContractErrorV1> {
        let index = self.latest.record_index();
        let chunk = index
            .chunk_position(chunk_id)
            .map(|position| &self.latest.generation.chunks().chunks()[position])
            .ok_or(QueryExecutionContractErrorV1::RecordUnavailable)?;
        let file = index
            .file_position(&chunk.anchor.file_occurrence_id)
            .map(|position| &self.latest.generation.snapshot().files[position])
            .ok_or(QueryExecutionContractErrorV1::RecordUnavailable)?;
        Ok(NativeCodeOccurrenceV1 {
            file: chunk.anchor.file_occurrence_id.clone(),
            symbol: chunk.anchor.symbol_occurrence_id.clone(),
            chunk: Some(chunk.id.clone()),
            path: file.logical_path.clone(),
            span: chunk.anchor.source_span,
        })
    }

    fn symbol(
        &self,
        symbol: &SymbolOccurrenceId,
        file: &FileOccurrenceId,
    ) -> Result<NativeSymbolRecordV1, QueryExecutionContractErrorV1> {
        let index = self.latest.record_index();
        let lineage = index
            .symbol_position(symbol)
            .map(|position| &self.latest.generation.symbols().symbols[position])
            .ok_or(QueryExecutionContractErrorV1::RecordUnavailable)?;
        let source = index
            .file_position(file)
            .map(|position| &self.latest.generation.snapshot().files[position])
            .ok_or(QueryExecutionContractErrorV1::RecordUnavailable)?;
        let chunk = index
            .chunk_position_for_file_symbol(file, symbol)
            .map(|position| &self.latest.generation.chunks().chunks()[position]);
        let signature = chunk
            .and_then(|chunk| {
                chunk
                    .sanitized_text
                    .as_str()
                    .lines()
                    .find(|line| !line.trim().is_empty())
            })
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned);
        let is_async = signature
            .as_deref()
            .is_some_and(|line| line.split_whitespace().any(|part| part == "async"));
        let qualified_name = lineage.qualified_name.clone();
        let name = qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(&qualified_name)
            .to_owned();
        Ok(NativeSymbolRecordV1 {
            occurrence: symbol.clone(),
            name,
            qualified_name,
            kind: lineage.kind.clone(),
            path: source.logical_path.clone(),
            span: chunk.map_or(
                tracedecay_domain::SourceSpan {
                    start_byte: 0,
                    end_byte: 0,
                },
                |chunk| chunk.anchor.source_span,
            ),
            signature,
            is_async,
        })
    }
}

struct TextArtifactNativeRecordReadPortV1 {
    generation: CodeGenerationId,
    owners: std::sync::Arc<ProductionCodeIndexQueryOwnersV1>,
}

impl NativeRecordReadPortV1 for TextArtifactNativeRecordReadPortV1 {
    fn generation(&self) -> &CodeGenerationId {
        &self.generation
    }

    fn occurrence(
        &self,
        binding: &CodeCandidateBindingV1,
    ) -> Result<NativeCodeOccurrenceV1, QueryExecutionContractErrorV1> {
        if &binding.occurrence.generation != self.generation() {
            return Err(QueryExecutionContractErrorV1::GenerationMismatch);
        }
        self.owners.occurrence_by_binding(binding)
    }

    fn occurrence_by_chunk(
        &self,
        chunk_id: &CodeSearchChunkId,
    ) -> Result<NativeCodeOccurrenceV1, QueryExecutionContractErrorV1> {
        self.owners.occurrence_by_chunk(chunk_id)
    }

    fn symbol(
        &self,
        _symbol: &SymbolOccurrenceId,
        _file: &FileOccurrenceId,
    ) -> Result<NativeSymbolRecordV1, QueryExecutionContractErrorV1> {
        Err(QueryExecutionContractErrorV1::RecordUnavailable)
    }
}

fn application_occurrence(record: NativeCodeOccurrenceV1) -> CodeOccurrenceRecord {
    CodeOccurrenceRecord {
        file: record.file,
        symbol: record.symbol,
        chunk: record.chunk,
        path: record.path,
        span: record.span,
    }
}

fn application_exact_record(record: NativeExactRecordV1) -> ExactOccurrenceRecord {
    ExactOccurrenceRecord {
        occurrence: application_occurrence(record.occurrence),
        matched_kind: record.matched_kind,
        matched_literal: record.matched_literal,
    }
}

fn application_lexical_record(record: NativeLexicalRecordV1) -> LexicalOccurrenceRecord {
    LexicalOccurrenceRecord {
        occurrence: application_occurrence(record.occurrence),
        score_micros: record.score_micros,
        matched_phrases: record.matched_phrases,
        matched_terms: record.matched_terms,
    }
}

fn application_symbol_record(record: NativeSymbolRecordV1) -> SymbolPrimitiveRecord {
    SymbolPrimitiveRecord {
        node_id: record.occurrence.as_str().to_owned(),
        name: record.name,
        qualified_name: record.qualified_name,
        kind: record.kind,
        file: record.path,
        start_line_zero_based: 0,
        end_line_zero_based: 0,
        line: 1,
        end_line: 1,
        signature: record.signature,
        is_async: record.is_async,
        score: None,
    }
}

fn application_graph_record(record: NativeGraphRecordV1) -> SymbolRelationRecord {
    SymbolRelationRecord {
        symbol: application_symbol_record(record.symbol),
        edge_kind: record.edge_kind.map_or_else(
            || "unknown".to_owned(),
            |edge| relation_edge_kind_name(edge).to_owned(),
        ),
        dispatch_via_trait: false,
        dispatch_from: None,
        depth: Some(record.depth),
    }
}

fn symbol_record(
    latest: &LatestCompleteCodeIndexV1,
    symbol: &SymbolOccurrenceId,
    file: &tracedecay_domain::FileOccurrenceId,
) -> Option<SymbolPrimitiveRecord> {
    let records = LatestCompleteNativeRecordReadPortV1 { latest };
    records
        .symbol(symbol, file)
        .ok()
        .map(application_symbol_record)
}

fn symbol_record_by_id(
    latest: &LatestCompleteCodeIndexV1,
    symbol: &SymbolOccurrenceId,
) -> Option<SymbolPrimitiveRecord> {
    let position = latest.record_index().chunk_position_for_symbol(symbol)?;
    let chunk = &latest.generation.chunks().chunks()[position];
    symbol_record(latest, symbol, &chunk.anchor.file_occurrence_id)
}

fn symbol_scope_path<'a>(
    latest: &'a LatestCompleteCodeIndexV1,
    symbol: &SymbolOccurrenceId,
) -> Option<&'a str> {
    let index = latest.record_index();
    let chunk_position = index.chunk_position_for_symbol(symbol)?;
    let file = &latest.generation.chunks().chunks()[chunk_position]
        .anchor
        .file_occurrence_id;
    let file_position = index.file_position(file)?;
    Some(
        latest.generation.snapshot().files[file_position]
            .logical_path
            .as_str(),
    )
}

fn symbol_signature_line<'a>(
    latest: &'a LatestCompleteCodeIndexV1,
    symbol: &SymbolOccurrenceId,
) -> Option<&'a str> {
    let chunk_position = latest.record_index().chunk_position_for_symbol(symbol)?;
    latest.generation.chunks().chunks()[chunk_position]
        .sanitized_text
        .as_str()
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .filter(|line| !line.is_empty())
}

struct PreparedCallableQueryV1 {
    latest: LatestCompleteCodeIndexV1,
    query: PreparedQueryV1,
}

struct PreparedTextCallableQueryV1 {
    latest: LatestCodeTextGenerationV1,
    query: PreparedQueryV1,
}

trait PreparedCallableQueryStateV1 {
    fn generation(&self) -> &CodeGenerationId;
    fn query(&self) -> &PreparedQueryV1;
}

impl PreparedCallableQueryStateV1 for PreparedCallableQueryV1 {
    fn generation(&self) -> &CodeGenerationId {
        &self.latest.generation.manifest().generation_id
    }

    fn query(&self) -> &PreparedQueryV1 {
        &self.query
    }
}

impl PreparedCallableQueryStateV1 for PreparedTextCallableQueryV1 {
    fn generation(&self) -> &CodeGenerationId {
        &self.latest.metadata().manifest().generation_id
    }

    fn query(&self) -> &PreparedQueryV1 {
        &self.query
    }
}

macro_rules! prepare_callable_query_or_return {
    ($registry:expr, $context:expr, $request:expr, $operation:expr, $binding:expr) => {{
        let Ok(query_binding_digest) = canonical_sha256(&$binding) else {
            return unavailable(query_finished_at());
        };
        match $registry
            .prepare_callable_query(
                &$context,
                &$request.scope.generation,
                &$request.meta.page,
                $request.meta.temporal,
                $operation,
                query_binding_digest.clone(),
            )
            .await
        {
            Ok(prepared) => (prepared, query_binding_digest),
            Err(error) => {
                return rejected_cursor(
                    query_finished_at(),
                    $request.scope.generation.clone(),
                    error,
                );
            }
        }
    }};
}

macro_rules! prepare_text_callable_query_or_return {
    ($registry:expr, $context:expr, $request:expr, $operation:expr, $binding:expr) => {{
        let Ok(query_binding_digest) = canonical_sha256(&$binding) else {
            return unavailable(query_finished_at());
        };
        match $registry
            .prepare_text_callable_query(
                &$context,
                &$request.scope.generation,
                &$request.meta.page,
                $request.meta.temporal,
                $operation,
                query_binding_digest.clone(),
            )
            .await
        {
            Ok(prepared) => (prepared, query_binding_digest),
            Err(error) => {
                return rejected_cursor(
                    query_finished_at(),
                    $request.scope.generation.clone(),
                    error,
                );
            }
        }
    }};
}

/// Resolves the traversal start symbol a relation query is anchored on.
///
/// A node id that is not a symbol occurrence is unavailable outright; one that
/// no longer exists in the served generation is unavailable *for that
/// generation*, so the caller learns the anchor was dropped rather than that the
/// request was malformed.
macro_rules! resolve_start_symbol {
    ($prepared:expr, $node_id:expr) => {{
        let Ok(start) = typed::<SymbolOccurrenceId>($node_id.clone()) else {
            return unavailable(query_finished_at());
        };
        if symbol_record_by_id(&$prepared.latest, &start).is_none() {
            return unavailable_for_generation(
                query_finished_at(),
                $prepared.latest.generation.manifest().generation_id.clone(),
            );
        }
        start
    }};
}

impl CodeIndexSchedulerRegistryV1 {
    async fn prepare_callable_query(
        &self,
        context: &RetrievalPortContext<'_>,
        generation: &CodeGenerationId,
        page: &tracedecay_application::PageRequest,
        temporal: TemporalModeV1,
        operation: &'static str,
        query_binding_digest: ManifestDigest,
    ) -> Result<PreparedCallableQueryV1, CallableCodeCursorError> {
        let authority = self
            .query_authority_for_scope(context.request.scope())
            .await
            .ok_or(CallableCodeCursorError::Unavailable)?;
        let routing = prepared_routing_bindings(
            context,
            temporal,
            operation,
            query_binding_digest,
            page.page_size,
        )?;
        let latest = self
            .resolve_serving_generation(
                context.request,
                generation,
                page,
                authority.as_ref(),
                &routing,
            )
            .await?;
        let base = base_request(context, &latest, temporal, authority.profile())
            .map_err(|_| CallableCodeCursorError::Unavailable)?;
        let query = PreparedQueryV1::prepare(
            authority,
            base,
            page.cursor.as_ref().map(OpaqueCursor::as_str),
        )?;
        Ok(PreparedCallableQueryV1 { latest, query })
    }

    async fn prepare_text_callable_query(
        &self,
        context: &RetrievalPortContext<'_>,
        generation: &CodeGenerationId,
        page: &tracedecay_application::PageRequest,
        temporal: TemporalModeV1,
        operation: &'static str,
        query_binding_digest: ManifestDigest,
    ) -> Result<PreparedTextCallableQueryV1, CallableCodeCursorError> {
        let authority = self
            .query_authority_for_scope(context.request.scope())
            .await
            .ok_or(CallableCodeCursorError::Unavailable)?;
        let routing = prepared_routing_bindings(
            context,
            temporal,
            operation,
            query_binding_digest,
            page.page_size,
        )?;
        let latest = self
            .resolve_text_serving_generation(
                context.request,
                generation,
                page,
                authority.as_ref(),
                &routing,
            )
            .await?;
        let base = text_base_request(context, &latest, temporal, authority.profile())
            .map_err(|_| CallableCodeCursorError::Unavailable)?;
        let query = PreparedQueryV1::prepare(
            authority,
            base,
            page.cursor.as_ref().map(OpaqueCursor::as_str),
        )?;
        Ok(PreparedTextCallableQueryV1 { latest, query })
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_direct_query<T: serde::Serialize>(
    prepared: &PreparedCallableQueryV1,
    context: &RetrievalPortContext<'_>,
    operation: &'static str,
    query_binding_digest: ManifestDigest,
    page: CodeQueryPage<T>,
    requested_page: &tracedecay_application::PageRequest,
    eligible: u64,
) -> RetrievalPortOutcome<CodeQueryPage<T>> {
    finish_query_with_coverage(
        prepared,
        context,
        operation,
        query_binding_digest,
        page,
        requested_page,
        tracedecay_domain::RetrieverCoverage {
            eligible,
            examined: eligible,
            excluded: 0,
            capped: 0,
            unknown: 0,
        },
    )
}

/// Pages a fully materialized item list owned by the served generation and hands
/// it to [`finish_direct_query`].
///
/// The whole list is in hand, so every item is eligible and no cursor is needed.
/// A page that still fails the application contract means the served generation
/// identity did not validate, which is a degraded read rather than a caller
/// error; `page_label` names what was being paged in that report.
fn finish_generation_page<T: serde::Serialize>(
    prepared: &PreparedCallableQueryV1,
    context: &RetrievalPortContext<'_>,
    operation: &'static str,
    query_binding_digest: ManifestDigest,
    items: Vec<T>,
    requested_page: &tracedecay_application::PageRequest,
    page_label: &'static str,
) -> RetrievalPortOutcome<CodeQueryPage<T>> {
    let eligible = items.len() as u64;
    let generation = prepared.latest.generation.manifest().generation_id.clone();
    let page = match CodeQueryPage::new(generation.clone(), items, None, None, None) {
        Ok(page) => page,
        Err(error) => {
            tracing::warn!(
                operation,
                page_label,
                %error,
                "generation-owned page failed the application contract"
            );
            return unavailable_for_generation(query_finished_at(), generation);
        }
    };
    finish_direct_query(
        prepared,
        context,
        operation,
        query_binding_digest,
        page,
        requested_page,
        eligible,
    )
}

/// Pages compact candidate keys through [`PreparedQueryV1::paginate_candidates`]
/// and hydrates only the returned slice.
#[allow(clippy::too_many_arguments)]
fn finish_generation_candidate_page<K, T>(
    prepared: &PreparedCallableQueryV1,
    context: &RetrievalPortContext<'_>,
    operation: &'static str,
    query_binding_digest: ManifestDigest,
    keys: Vec<K>,
    hydrate: impl FnOnce(&[K]) -> Result<Vec<T>, PreparedQueryErrorV1>,
    requested_page: &tracedecay_application::PageRequest,
    page_label: &'static str,
) -> RetrievalPortOutcome<CodeQueryPage<T>>
where
    K: Serialize,
    T: Serialize,
{
    let eligible = keys.len() as u64;
    let finished_at = query_finished_at();
    let generation = prepared.latest.generation.manifest().generation_id.clone();
    let bindings = PreparedQueryBindingsV1::new(
        operation,
        context.request.scope().scope_digest.clone(),
        generation.clone(),
        query_binding_digest,
    );
    let pagination = bindings.and_then(|bindings| {
        prepared.query().paginate_candidates(
            &bindings,
            keys,
            requested_page.page_size,
            finished_at,
            hydrate,
        )
    });
    match pagination {
        Ok(pagination) => {
            let cursor_expires_at = pagination.expires_at;
            let next_cursor = pagination
                .next_cursor
                .map(OpaqueCursor::new)
                .transpose()
                .map_err(|_| PreparedQueryErrorV1::Unavailable);
            let Ok(next_cursor) = next_cursor else {
                return rejected_cursor(finished_at, generation, PreparedQueryErrorV1::Unavailable);
            };
            let page = match CodeQueryPage::new(
                generation.clone(),
                pagination.items,
                Some(pagination.total),
                next_cursor,
                None,
            ) {
                Ok(page) => page,
                Err(error) => {
                    tracing::warn!(
                        operation,
                        page_label,
                        %error,
                        "prepared candidate page failed the application contract"
                    );
                    return rejected_cursor(
                        finished_at,
                        generation,
                        PreparedQueryErrorV1::Unavailable,
                    );
                }
            };
            bounded_result(
                page,
                tracedecay_domain::RetrieverCoverage {
                    eligible,
                    examined: eligible,
                    excluded: 0,
                    capped: 0,
                    unknown: 0,
                },
                finished_at,
                None,
                cursor_expires_at,
            )
        }
        Err(error) => rejected_cursor(finished_at, generation, error),
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_query_with_coverage<T: serde::Serialize>(
    prepared: &impl PreparedCallableQueryStateV1,
    context: &RetrievalPortContext<'_>,
    operation: &'static str,
    query_binding_digest: ManifestDigest,
    page: CodeQueryPage<T>,
    requested_page: &tracedecay_application::PageRequest,
    coverage: tracedecay_domain::RetrieverCoverage,
) -> RetrievalPortOutcome<CodeQueryPage<T>> {
    let finished_at = query_finished_at();
    let generation = prepared.generation().clone();
    let bindings = PreparedQueryBindingsV1::new(
        operation,
        context.request.scope().scope_digest.clone(),
        generation.clone(),
        query_binding_digest,
    );
    let pagination = bindings.and_then(|bindings| {
        prepared
            .query()
            .paginate(&bindings, page.items, requested_page.page_size, finished_at)
    });
    match pagination {
        Ok(pagination) => {
            let cursor_expires_at = pagination.expires_at;
            let next_cursor = pagination
                .next_cursor
                .map(OpaqueCursor::new)
                .transpose()
                .map_err(|_| PreparedQueryErrorV1::Unavailable);
            let Ok(next_cursor) = next_cursor else {
                return rejected_cursor(finished_at, generation, PreparedQueryErrorV1::Unavailable);
            };
            // `pagination.total` and the served generation identity are both
            // store readings, so a page that fails the contract is a stale or
            // inconsistent read. Answer with the typed evidence this surface
            // already returns for a rejected cursor.
            let page = match CodeQueryPage::new(
                generation.clone(),
                pagination.items,
                Some(pagination.total),
                next_cursor,
                page.query_fallback,
            ) {
                Ok(page) => page,
                Err(error) => {
                    tracing::warn!(
                        operation,
                        %error,
                        "prepared query page failed the application contract"
                    );
                    return rejected_cursor(
                        finished_at,
                        generation,
                        PreparedQueryErrorV1::Unavailable,
                    );
                }
            };
            bounded_result(page, coverage, finished_at, None, cursor_expires_at)
        }
        Err(error) => rejected_cursor(finished_at, generation, error),
    }
}

/// Compact BFS identity for a relation neighborhood.
///
/// `depth` is the emitted one-based depth (`parent_depth + 1`), matching the
/// `SymbolRelationRecord::depth` the hydrating path used to store.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct RelationKeyV1 {
    pub occurrence: SymbolOccurrenceId,
    pub edge_kind: RelationEdgeKindV1,
    pub dispatch_from: Option<SymbolOccurrenceId>,
    pub depth: u32,
}

#[cfg(any(test, feature = "test-helpers"))]
mod relation_hydration_counters {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SYMBOL_PRIMITIVE_HYDRATIONS: AtomicU64 = AtomicU64::new(0);

    pub fn record() {
        SYMBOL_PRIMITIVE_HYDRATIONS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn take() -> u64 {
        SYMBOL_PRIMITIVE_HYDRATIONS.swap(0, Ordering::Relaxed)
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn take_relation_symbol_hydrations() -> u64 {
    relation_hydration_counters::take()
}

fn record_relation_symbol_hydration() {
    #[cfg(any(test, feature = "test-helpers"))]
    relation_hydration_counters::record();
}

pub(crate) fn relation_keys(
    latest: &LatestCompleteCodeIndexV1,
    start: &SymbolOccurrenceId,
    kinds: &[RelationEdgeKindV1],
    reverse: bool,
    maximum_depth: u32,
    scope: &tracedecay_application::CodeQueryScope,
) -> Vec<RelationKeyV1> {
    let index = latest.record_index();
    let edges = latest.generation.edges();
    let mut queue = VecDeque::from([(start.clone(), 0_u32)]);
    let mut visited = BTreeSet::from([start.clone()]);
    let mut keys = Vec::new();
    while let Some((current, depth)) = queue.pop_front() {
        if depth >= maximum_depth {
            continue;
        }
        // Adjacency lookup replaces a full `edges()` scan per dequeued symbol,
        // which made this traversal O(visited x edges). The positions arrive in
        // ascending order and carry the same incidence test the scan applied,
        // so the surviving `kinds` filter yields the identical edge sequence.
        for edge in index
            .incident_edge_positions(&current, reverse)
            .iter()
            .map(|position| &edges[*position])
            .filter(|edge| kinds.contains(&edge.kind))
        {
            let next = if reverse {
                &edge.from_occurrence
            } else {
                &edge.to_occurrence
            };
            if !visited.insert(next.clone()) {
                continue;
            }
            let Some(path) = symbol_scope_path(latest, next) else {
                continue;
            };
            if !path_is_in_code_query_scope(path, scope) {
                continue;
            }
            keys.push(RelationKeyV1 {
                occurrence: next.clone(),
                edge_kind: edge.kind,
                dispatch_from: (edge.kind == RelationEdgeKindV1::Implements)
                    .then(|| current.clone()),
                depth: depth + 1,
            });
            queue.push_back((next.clone(), depth + 1));
        }
    }
    keys.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then(left.occurrence.cmp(&right.occurrence))
    });
    keys
}

pub(crate) fn hydrate_relation_records(
    latest: &LatestCompleteCodeIndexV1,
    keys: &[RelationKeyV1],
) -> Result<Vec<SymbolRelationRecord>, PreparedQueryErrorV1> {
    keys.iter()
        .map(|key| {
            record_relation_symbol_hydration();
            let symbol = symbol_record_by_id(latest, &key.occurrence)
                .ok_or(PreparedQueryErrorV1::Unavailable)?;
            Ok(SymbolRelationRecord {
                symbol,
                edge_kind: relation_edge_kind_name(key.edge_kind).to_owned(),
                dispatch_via_trait: key.edge_kind == RelationEdgeKindV1::Implements,
                dispatch_from: key
                    .dispatch_from
                    .as_ref()
                    .map(|identity| identity.as_str().to_owned()),
                depth: Some(key.depth),
            })
        })
        .collect()
}

fn retrieval_failure_omission(reason: &RetrievalFailure) -> OmissionReason {
    match reason {
        RetrievalFailure::AuthorityUnavailable { .. } => OmissionReason::Unavailable,
        RetrievalFailure::IncompatibleProjection { .. } => OmissionReason::Unsupported,
        RetrievalFailure::StaleSource => OmissionReason::Stale,
        RetrievalFailure::InvalidRequest { .. } | RetrievalFailure::Internal { .. } => {
            OmissionReason::Failed
        }
    }
}

fn terminal_lane_evidence<T>(
    finished_at: UtcMicros,
    generation: CodeGenerationId,
    reason: OmissionReason,
) -> RetrievalEvidence<CodeQueryPage<T>> {
    let RetrievalPortOutcome::Unavailable(mut evidence) =
        unavailable_for_generation::<CodeQueryPage<T>>(finished_at, generation)
    else {
        unreachable!("generation unavailable helper returns unavailable evidence")
    };
    evidence.omissions[0].reason = reason;
    if reason == OmissionReason::Stale {
        evidence.temporal.freshness = FreshnessState::Stale;
    }
    evidence
}

#[allow(clippy::too_many_arguments)]
fn finish_native_lane_page<T, N>(
    prepared: &impl PreparedCallableQueryStateV1,
    context: &RetrievalPortContext<'_>,
    operation: &'static str,
    query_binding_digest: ManifestDigest,
    requested_page: &tracedecay_application::PageRequest,
    page: NativeLanePageV1<N>,
    map: impl FnMut(N) -> T,
) -> RetrievalPortOutcome<CodeQueryPage<T>>
where
    T: serde::Serialize,
{
    let coverage = page.coverage;
    let generation = page.generation.clone();
    // `total_eligible` is the lane's own count of what it could have returned;
    // if it disagrees with the rows the lane actually produced, the lane read
    // is inconsistent, not the request.
    let page = match CodeQueryPage::new(
        page.generation,
        page.items.into_iter().map(map).collect(),
        Some(page.total_eligible),
        None,
        None,
    ) {
        Ok(page) => page,
        Err(error) => {
            tracing::warn!(
                operation,
                %error,
                "query-native lane page failed the application contract"
            );
            return unavailable_for_generation(query_finished_at(), generation);
        }
    };
    finish_query_with_coverage(
        prepared,
        context,
        operation,
        query_binding_digest,
        page,
        requested_page,
        coverage,
    )
}

fn mark_lane_partial<T>(
    outcome: RetrievalPortOutcome<CodeQueryPage<T>>,
    coverage: tracedecay_domain::RetrieverCoverage,
    reason: OmissionReason,
) -> RetrievalPortOutcome<CodeQueryPage<T>> {
    match outcome {
        RetrievalPortOutcome::Completed(mut evidence)
        | RetrievalPortOutcome::Partial(mut evidence) => {
            evidence.omissions.push(Omission {
                domain: EvidenceDomain::Symbol,
                count: coverage.eligible.saturating_sub(evidence.coverage.returned),
                reason,
            });
            evidence.coverage.completeness = CoverageCompleteness::Partial;
            for domain in &mut evidence.coverage.domains {
                domain.completeness = CoverageCompleteness::Partial;
            }
            RetrievalPortOutcome::Partial(evidence)
        }
        outcome => outcome,
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_native_lane_query<T, N>(
    prepared: &impl PreparedCallableQueryStateV1,
    context: &RetrievalPortContext<'_>,
    operation: &'static str,
    query_binding_digest: ManifestDigest,
    requested_page: &tracedecay_application::PageRequest,
    outcome: NativeLaneOutcomeV1<N>,
    mut map: impl FnMut(N) -> T,
) -> RetrievalPortOutcome<CodeQueryPage<T>>
where
    T: serde::Serialize,
{
    let finished_at = query_finished_at();
    match outcome {
        NativeLaneOutcomeV1::Complete(page) => finish_native_lane_page(
            prepared,
            context,
            operation,
            query_binding_digest,
            requested_page,
            page,
            &mut map,
        ),
        NativeLaneOutcomeV1::Partial { page, reason } => {
            let coverage = page.coverage;
            let omission = retrieval_failure_omission(&reason);
            let outcome = finish_native_lane_page(
                prepared,
                context,
                operation,
                query_binding_digest,
                requested_page,
                page,
                &mut map,
            );
            mark_lane_partial(outcome, coverage, omission)
        }
        NativeLaneOutcomeV1::Unavailable(reason) => {
            let omission = retrieval_failure_omission(&reason);
            let evidence =
                terminal_lane_evidence(finished_at, prepared.generation().clone(), omission);
            match reason {
                RetrievalFailure::InvalidRequest { .. } | RetrievalFailure::Internal { .. } => {
                    RetrievalPortOutcome::Failed(evidence)
                }
                RetrievalFailure::AuthorityUnavailable { .. }
                | RetrievalFailure::IncompatibleProjection { .. }
                | RetrievalFailure::StaleSource => RetrievalPortOutcome::Unavailable(evidence),
            }
        }
        NativeLaneOutcomeV1::Denied => RetrievalPortOutcome::Unavailable(terminal_lane_evidence(
            finished_at,
            prepared.generation().clone(),
            OmissionReason::Unavailable,
        )),
        NativeLaneOutcomeV1::Stale(_) => RetrievalPortOutcome::Unavailable(terminal_lane_evidence(
            finished_at,
            prepared.generation().clone(),
            OmissionReason::Stale,
        )),
        NativeLaneOutcomeV1::BudgetExceeded(usage) => {
            let mut evidence = terminal_lane_evidence(
                finished_at,
                prepared.generation().clone(),
                OmissionReason::Budget,
            );
            evidence.budget = application_budget_usage(usage);
            RetrievalPortOutcome::Partial(evidence)
        }
        NativeLaneOutcomeV1::TimedOut(usage) => {
            let mut evidence = terminal_lane_evidence(
                finished_at,
                prepared.generation().clone(),
                OmissionReason::TimedOut,
            );
            evidence.budget = application_budget_usage(usage);
            RetrievalPortOutcome::TimedOut(evidence)
        }
        NativeLaneOutcomeV1::Cancelled => {
            let mut evidence = terminal_lane_evidence(
                finished_at,
                prepared.generation().clone(),
                OmissionReason::Cancelled,
            );
            evidence.cancellation = Some(CancellationObservation {
                stage: CancellationStage::DuringRead,
                observed_at: finished_at,
            });
            RetrievalPortOutcome::Cancelled(evidence)
        }
    }
}

fn application_budget_usage(usage: RetrievalBudgetUsage) -> OperationBudgetUsage {
    OperationBudgetUsage {
        units_consumed: usage
            .candidates_examined
            .saturating_add(usage.hydrated_results),
        bytes_consumed: usage.hydration_bytes,
        elapsed_micros: usage.elapsed_micros,
    }
}

type PortFuture<'a, T> =
    Pin<Box<dyn Future<Output = RetrievalPortOutcome<CodeQueryPage<T>>> + Send + 'a>>;

impl CallableCodeQueryPort for CodeIndexSchedulerRegistryV1 {
    fn exact_occurrence<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a ExactOccurrenceRequest,
    ) -> CallableCodeQueryFuture<'a, ExactOccurrenceRecord> {
        Box::pin(async move {
            let (prepared, query_binding_digest) = prepare_text_callable_query_or_return!(
                self,
                context,
                request,
                "code_exact_occurrence",
                (
                    "code_exact_occurrence",
                    &request.literal,
                    &request.kind,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let latest = &prepared.latest;
            let served_generation = latest.metadata().manifest().generation_id.clone();
            let finished_at = query_finished_at();
            let base = prepared.query.request();
            let Ok(query_view) = tracedecay_domain::EphemeralSanitizedQueryViewV1::sanitize(
                request.literal.clone(),
                callable_query_sanitizer_revision(),
                callable_query_normalization_revision(),
            ) else {
                return unavailable(finished_at);
            };
            let authority = CentralExactAdmissionAuthorityV1::new(
                ExactAdmissionRuleRevision::new(
                    tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
                )
                .unwrap_or_else(|_| panic!("static exact rule revision")),
            );
            let lane_request = ExactLaneRequest {
                literals: authority.parse_literals(&query_view, base),
                generation: served_generation.clone(),
                budget: base.budget,
                base: base.clone(),
                query_view: &query_view,
            };
            let Ok(owners) = latest.production_query_owners_with_budget(&base.budget) else {
                return unavailable(finished_at);
            };
            let records = TextArtifactNativeRecordReadPortV1 {
                generation: served_generation.clone(),
                owners: std::sync::Arc::clone(&owners),
            };
            let Ok(native_context) =
                AdmittedGenerationContextV1::admit(served_generation.clone(), &records)
            else {
                return unavailable_for_generation(finished_at, served_generation);
            };
            let outcome = owners.retrieve_exact(&lane_request);
            match outcome {
                Ok(outcome) => {
                    let Ok(outcome) =
                        native_context.exact(outcome, &request.literal, request.kind, |path| {
                            path_is_in_code_query_scope(path, &request.scope)
                        })
                    else {
                        return unavailable(finished_at);
                    };
                    finish_native_lane_query(
                        &prepared,
                        &context,
                        "code_exact_occurrence",
                        query_binding_digest,
                        &request.meta.page,
                        outcome,
                        application_exact_record,
                    )
                }
                Err(_) => unavailable(finished_at),
            }
        })
    }

    fn phrase_search<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a PhraseSearchRequest,
    ) -> CallableCodeQueryFuture<'a, LexicalOccurrenceRecord> {
        Box::pin(async move {
            let (prepared, query_binding_digest) = prepare_text_callable_query_or_return!(
                self,
                context,
                request,
                "code_phrase_search",
                (
                    "code_phrase_search",
                    request.query.as_str(),
                    &request.phrases,
                    &request.field_filters,
                    request.fuzzy_budget,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let latest = &prepared.latest;
            let served_generation = latest.metadata().manifest().generation_id.clone();
            let finished_at = query_finished_at();
            let base = prepared.query.request();
            let whole_terms = request
                .query
                .as_str()
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let lane_request = LexicalLaneRequest {
                query_view: &request.query,
                generation: served_generation.clone(),
                whole_terms: whole_terms.clone(),
                subtokens: whole_terms
                    .iter()
                    .map(|term| term.to_ascii_lowercase())
                    .collect(),
                phrases: request.phrases.clone(),
                field_filters: request
                    .field_filters
                    .iter()
                    .map(|filter| LexicalFieldFilterV1 {
                        field: match filter.field {
                            CodeLexicalField::SymbolName => LexicalFieldV1::SymbolName,
                            CodeLexicalField::QualifiedName => LexicalFieldV1::QualifiedName,
                            CodeLexicalField::Path => LexicalFieldV1::Path,
                            CodeLexicalField::BodyText => LexicalFieldV1::BodyText,
                            CodeLexicalField::PreambleText => LexicalFieldV1::PreambleText,
                            CodeLexicalField::ExactTerm => LexicalFieldV1::ExactTerm,
                            CodeLexicalField::Subtoken => LexicalFieldV1::Subtoken,
                        },
                        include: filter.include,
                    })
                    .collect(),
                fuzzy_budget: request.fuzzy_budget,
                lexical_profile_revision: ComponentRevision::new(
                    tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
                )
                .unwrap_or_else(|_| panic!("static lexical profile")),
                score_domain: ScoreDomainId::new(
                    tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
                )
                .unwrap_or_else(|_| panic!("static lexical score domain")),
                budget: base.budget,
                base: base.clone(),
            };
            let Ok(owners) = latest.production_query_owners_with_budget(&base.budget) else {
                return unavailable(finished_at);
            };
            let records = TextArtifactNativeRecordReadPortV1 {
                generation: served_generation.clone(),
                owners: std::sync::Arc::clone(&owners),
            };
            let Ok(native_context) =
                AdmittedGenerationContextV1::admit(served_generation.clone(), &records)
            else {
                return unavailable_for_generation(finished_at, served_generation);
            };
            let outcome = owners.retrieve_lexical(&lane_request);
            match outcome {
                Ok(outcome) => {
                    let Ok(outcome) = native_context.lexical(outcome, |path| {
                        path_is_in_code_query_scope(path, &request.scope)
                    }) else {
                        return unavailable(finished_at);
                    };
                    finish_native_lane_query(
                        &prepared,
                        &context,
                        "code_phrase_search",
                        query_binding_digest,
                        &request.meta.page,
                        outcome,
                        application_lexical_record,
                    )
                }
                Err(_) => unavailable(finished_at),
            }
        })
    }

    fn callees<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeRelationRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let (prepared, query_binding_digest) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_callees",
                (
                    "code_callees",
                    &request.node_id,
                    request.maximum_depth,
                    request.resolve_trait_dispatch,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let latest = &prepared.latest;
            let served_generation = latest.generation.manifest().generation_id.clone();
            let finished_at = query_finished_at();
            let base = prepared.query.request();
            let Ok(symbol) = typed::<SymbolOccurrenceId>(request.node_id.clone()) else {
                return unavailable(finished_at);
            };
            let Some(chunk) = latest
                .record_index()
                .chunk_position_for_symbol(&symbol)
                .map(|position| &latest.generation.chunks().chunks()[position])
            else {
                return unavailable(finished_at);
            };
            let source_occurrence =
                SourceOccurrenceId::new(format!("code-symbol:{}", symbol.as_str()))
                    .unwrap_or_else(|_| panic!("validated symbol creates source occurrence"));
            let seed = CodeCandidateBindingV1 {
                candidate_anchor: RetrievalAnchorId::new(format!(
                    "code-symbol:{}",
                    symbol.as_str()
                ))
                .unwrap_or_else(|_| panic!("validated symbol creates anchor")),
                occurrence: CodeOccurrenceRefV1 {
                    generation: served_generation.clone(),
                    file: chunk.anchor.file_occurrence_id.clone(),
                    symbol: Some(symbol),
                    chunk: Some(chunk.id.clone()),
                },
                language_descriptor_revision: chunk.language_descriptor_revision.clone(),
                matched_term_kinds: Vec::new(),
                source_occurrence,
            };
            let lane_request = GraphLaneRequest {
                generation: served_generation.clone(),
                seed_anchors: vec![seed],
                edge_kinds: vec![RelationEdgeKindV1::Calls],
                max_depth: request.maximum_depth,
                budget: graph_budget_for_request(base.budget, context.request),
                base: base.clone(),
            };
            let Ok(graph_serving) = latest.production_graph_serving() else {
                return unavailable(finished_at);
            };
            let records = LatestCompleteNativeRecordReadPortV1 { latest };
            let Ok(native_context) =
                AdmittedGenerationContextV1::admit(served_generation.clone(), &records)
            else {
                return unavailable_for_generation(finished_at, served_generation);
            };
            let graph_control = CallableGraphExecutionControl::for_request(context.request);
            let outcome = graph_serving
                .graph
                .retrieve_graph(&lane_request, graph_control);
            match outcome {
                Ok(outcome) => {
                    let Ok(outcome) = native_context.graph(outcome, |path| {
                        path_is_in_code_query_scope(path, &request.scope)
                    }) else {
                        return unavailable(finished_at);
                    };
                    finish_native_lane_query(
                        &prepared,
                        &context,
                        "code_callees",
                        query_binding_digest,
                        &request.meta.page,
                        outcome,
                        application_graph_record,
                    )
                }
                Err(_) => unavailable(finished_at),
            }
        })
    }

    fn symbol_search<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeSymbolSearchRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_symbol_search",
                (
                    "code_symbol_search",
                    request.query.as_str(),
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let query = request.query.as_str().to_ascii_lowercase();
            let mut ranked = prepared
                .latest
                .generation
                .symbols()
                .symbols
                .iter()
                .filter_map(|symbol| {
                    if !ascii_contains_ignore_case(&symbol.simple_name, &query)
                        && !ascii_contains_ignore_case(&symbol.qualified_name, &query)
                    {
                        return None;
                    }
                    let path = symbol_scope_path(&prepared.latest, &symbol.occurrence)?;
                    if !path_is_in_code_query_scope(path, &request.scope) {
                        return None;
                    }
                    let name = last_qualified_segment(&symbol.qualified_name);
                    let qualified = symbol.qualified_name.as_str();
                    let tier = if name.eq_ignore_ascii_case(&query)
                        || qualified.eq_ignore_ascii_case(&query)
                    {
                        0_u8
                    } else if ascii_starts_with_ignore_case(name, &query)
                        || ascii_starts_with_ignore_case(qualified, &query)
                    {
                        1
                    } else if ascii_contains_ignore_case(name, &query)
                        || ascii_contains_ignore_case(qualified, &query)
                    {
                        2
                    } else {
                        return None;
                    };
                    let mut record = symbol_record_by_id(&prepared.latest, &symbol.occurrence)?;
                    record.score = Some(match tier {
                        0 => 1.0,
                        1 => 0.75,
                        _ => 0.5,
                    });
                    Some((tier, record))
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| {
                left.0
                    .cmp(&right.0)
                    .then(left.1.qualified_name.cmp(&right.1.qualified_name))
                    .then(left.1.node_id.cmp(&right.1.node_id))
            });
            let items = ranked
                .into_iter()
                .map(|(_, record)| record)
                .collect::<Vec<_>>();
            finish_generation_page(
                &prepared,
                &context,
                "code_symbol_search",
                binding,
                items,
                &request.meta.page,
                "symbols",
            )
        })
    }

    fn qualified_name<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a QualifiedNameRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_qualified_name",
                (
                    "code_qualified_name",
                    &request.qualified_name,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let symbols = &prepared.latest.generation.symbols().symbols;
            let mut items = prepared
                .latest
                .record_index()
                .qualified_name_positions(symbols, &request.qualified_name)
                .iter()
                .map(|position| &symbols[*position])
                .filter_map(|symbol| {
                    let path = symbol_scope_path(&prepared.latest, &symbol.occurrence)?;
                    if !path_is_in_code_query_scope(path, &request.scope) {
                        return None;
                    }
                    symbol_record_by_id(&prepared.latest, &symbol.occurrence)
                })
                .collect::<Vec<_>>();
            items.sort_by(|left, right| left.node_id.cmp(&right.node_id));
            finish_generation_page(
                &prepared,
                &context,
                "code_qualified_name",
                binding,
                items,
                &request.meta.page,
                "symbols",
            )
        })
    }

    fn signature_search<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeSignatureRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_signature_search",
                (
                    "code_signature_search",
                    &request.returns,
                    &request.params,
                    request.is_async,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let mut items = prepared
                .latest
                .generation
                .symbols()
                .symbols
                .iter()
                .filter_map(|symbol| {
                    let path = symbol_scope_path(&prepared.latest, &symbol.occurrence)?;
                    if !path_is_in_code_query_scope(path, &request.scope) {
                        return None;
                    }
                    let signature = symbol_signature_line(&prepared.latest, &symbol.occurrence)?;
                    if request
                        .returns
                        .as_ref()
                        .is_some_and(|returns| !signature.contains(returns))
                        || request
                            .params
                            .iter()
                            .any(|param| !signature.contains(param))
                    {
                        return None;
                    }
                    if request.is_async.is_some_and(|is_async| {
                        signature.split_whitespace().any(|part| part == "async") != is_async
                    }) {
                        return None;
                    }
                    symbol_record_by_id(&prepared.latest, &symbol.occurrence)
                })
                .collect::<Vec<_>>();
            items.sort_by(|left, right| {
                left.qualified_name
                    .cmp(&right.qualified_name)
                    .then(left.node_id.cmp(&right.node_id))
            });
            finish_generation_page(
                &prepared,
                &context,
                "code_signature_search",
                binding,
                items,
                &request.meta.page,
                "symbols",
            )
        })
    }

    fn implementations<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeImplementationsRequest,
    ) -> PortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_implementations",
                (
                    "code_implementations",
                    &request.selector,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let selector = match &request.selector {
                tracedecay_application::retrieval::ImplementationSelector::Trait { name }
                | tracedecay_application::retrieval::ImplementationSelector::Method { name } => {
                    name
                }
            };
            let symbols = &prepared.latest.generation.symbols().symbols;
            let index = prepared.latest.record_index();
            let target_positions = index
                .qualified_name_positions(symbols, selector)
                .iter()
                .chain(index.last_segment_positions(symbols, selector))
                .copied()
                .collect::<BTreeSet<_>>();
            let mut keys = Vec::new();
            for target_position in target_positions {
                let target = &symbols[target_position];
                keys.extend(relation_keys(
                    &prepared.latest,
                    &target.occurrence,
                    &[RelationEdgeKindV1::Implements],
                    true,
                    1,
                    &request.scope,
                ));
            }
            keys.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
            keys.dedup_by(|left, right| left.occurrence == right.occurrence);
            let latest = &prepared.latest;
            finish_generation_candidate_page(
                &prepared,
                &context,
                "code_implementations",
                binding,
                keys,
                |slice| hydrate_relation_records(latest, slice),
                &request.meta.page,
                "implementations",
            )
        })
    }

    fn type_hierarchy<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeHierarchyRequest,
    ) -> PortFuture<'a, TypeHierarchyRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_type_hierarchy",
                (
                    "code_type_hierarchy",
                    &request.node_id,
                    request.maximum_depth,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let start = resolve_start_symbol!(prepared, request.node_id);
            let keys = relation_keys(
                &prepared.latest,
                &start,
                &[RelationEdgeKindV1::Implements, RelationEdgeKindV1::Extends],
                false,
                request.maximum_depth,
                &request.scope,
            );
            let latest = &prepared.latest;
            let parent_node_id = request.node_id.clone();
            finish_generation_candidate_page(
                &prepared,
                &context,
                "code_type_hierarchy",
                binding,
                keys,
                |slice| {
                    hydrate_relation_records(latest, slice).map(|relations| {
                        relations
                            .into_iter()
                            .map(|relation| TypeHierarchyRecord {
                                parent_node_id: parent_node_id.clone(),
                                edge_kind: relation.edge_kind,
                                depth: relation.depth.unwrap_or(1),
                                symbol: relation.symbol,
                            })
                            .collect()
                    })
                },
                &request.meta.page,
                "hierarchy entries",
            )
        })
    }

    fn callers<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeRelationRequest,
    ) -> PortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_callers",
                (
                    "code_callers",
                    &request.node_id,
                    request.maximum_depth,
                    request.resolve_trait_dispatch,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let start = resolve_start_symbol!(prepared, request.node_id);
            let keys = relation_keys(
                &prepared.latest,
                &start,
                &[RelationEdgeKindV1::Calls],
                true,
                request.maximum_depth,
                &request.scope,
            );
            let latest = &prepared.latest;
            finish_generation_candidate_page(
                &prepared,
                &context,
                "code_callers",
                binding,
                keys,
                |slice| hydrate_relation_records(latest, slice),
                &request.meta.page,
                "callers",
            )
        })
    }

    fn impact<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeImpactRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_impact",
                (
                    "code_impact",
                    &request.node_id,
                    request.maximum_depth,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let start = resolve_start_symbol!(prepared, request.node_id);
            let keys = relation_keys(
                &prepared.latest,
                &start,
                &[
                    RelationEdgeKindV1::Calls,
                    RelationEdgeKindV1::Uses,
                    RelationEdgeKindV1::TypeOf,
                    RelationEdgeKindV1::Contains,
                    RelationEdgeKindV1::Implements,
                    RelationEdgeKindV1::Extends,
                    RelationEdgeKindV1::Annotates,
                ],
                true,
                request.maximum_depth,
                &request.scope,
            );
            let latest = &prepared.latest;
            finish_generation_candidate_page(
                &prepared,
                &context,
                "code_impact",
                binding,
                keys,
                |slice| {
                    hydrate_relation_records(latest, slice).map(|relations| {
                        relations
                            .into_iter()
                            .map(|relation| relation.symbol)
                            .collect()
                    })
                },
                &request.meta.page,
                "symbols",
            )
        })
    }

    fn module_api<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a ModuleApiRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_module_api",
                (
                    "code_module_api",
                    &request.path,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let prefix = format!("{}/", request.path.trim_end_matches('/'));
            let mut items = prepared
                .latest
                .generation
                .symbols()
                .symbols
                .iter()
                .filter_map(|symbol| {
                    let path = symbol_scope_path(&prepared.latest, &symbol.occurrence)?;
                    if path != request.path && !path.starts_with(&prefix) {
                        return None;
                    }
                    if !path_is_in_code_query_scope(path, &request.scope) {
                        return None;
                    }
                    let signature = symbol_signature_line(&prepared.latest, &symbol.occurrence)
                        .map(str::trim_start)?;
                    if !signature.starts_with("pub ")
                        && !signature.starts_with("export ")
                        && !signature.starts_with("public ")
                    {
                        return None;
                    }
                    symbol_record_by_id(&prepared.latest, &symbol.occurrence)
                })
                .collect::<Vec<_>>();
            items.sort_by(|left, right| {
                left.file
                    .cmp(&right.file)
                    .then(left.qualified_name.cmp(&right.qualified_name))
                    .then(left.node_id.cmp(&right.node_id))
            });
            finish_generation_page(
                &prepared,
                &context,
                "code_module_api",
                binding,
                items,
                &request.meta.page,
                "symbols",
            )
        })
    }

    fn source_metadata<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceMetadataRequest,
    ) -> PortFuture<'a, SourceMetadataRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_source_metadata",
                (
                    "code_source_metadata",
                    &request.files,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let requested = request.files.iter().collect::<BTreeSet<_>>();
            let items = prepared
                .latest
                .generation
                .snapshot()
                .files
                .iter()
                .filter(|file| {
                    requested.contains(&file.file_occurrence_id)
                        && path_is_in_code_query_scope(&file.logical_path, &request.scope)
                })
                .map(|file| SourceMetadataRecord {
                    file: file.file_occurrence_id.clone(),
                    path: file.logical_path.clone(),
                    language: file
                        .language
                        .as_ref()
                        .map(|language| language.as_str().to_owned()),
                    indexed_at: Some(prepared.latest.generation.manifest().seal.sealed_at),
                    byte_size: None,
                })
                .collect::<Vec<_>>();
            let eligible = request.files.len() as u64;
            let generation = prepared.latest.generation.manifest().generation_id.clone();
            let page = match CodeQueryPage::new(generation.clone(), items, None, None, None) {
                Ok(page) => page,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "generation-owned source metadata page failed the application contract"
                    );
                    return unavailable_for_generation(query_finished_at(), generation);
                }
            };
            finish_direct_query(
                &prepared,
                &context,
                "code_source_metadata",
                binding,
                page,
                &request.meta.page,
                eligible,
            )
        })
    }

    fn facets<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeFacetRequest,
    ) -> PortFuture<'a, CodeFacetRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_facets",
                (
                    "code_facets",
                    request.dimension,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let mut counts = std::collections::BTreeMap::<String, u64>::new();
            match request.dimension {
                CodeFacetDimension::Kind => {
                    let symbols = &prepared.latest.generation.symbols().symbols;
                    let files = &prepared.latest.generation.snapshot().files;
                    for (symbol_position, file_position) in
                        prepared.latest.record_index().kind_facet_rows()
                    {
                        let file = &files[*file_position];
                        if path_is_in_code_query_scope(&file.logical_path, &request.scope) {
                            *counts
                                .entry(symbols[*symbol_position].kind.clone())
                                .or_default() += 1;
                        }
                    }
                }
                CodeFacetDimension::Language => {
                    for file in &prepared.latest.generation.snapshot().files {
                        if path_is_in_code_query_scope(&file.logical_path, &request.scope) {
                            let value = file.language.as_ref().map_or_else(
                                || "unknown".to_owned(),
                                |value| value.as_str().to_owned(),
                            );
                            *counts.entry(value).or_default() += 1;
                        }
                    }
                }
                CodeFacetDimension::Path => {
                    for file in &prepared.latest.generation.snapshot().files {
                        if path_is_in_code_query_scope(&file.logical_path, &request.scope) {
                            *counts.entry(file.logical_path.clone()).or_default() += 1;
                        }
                    }
                }
            }
            let items = counts
                .into_iter()
                .map(|(value, count)| CodeFacetRecord {
                    dimension: request.dimension,
                    value,
                    count,
                })
                .collect::<Vec<_>>();
            finish_generation_page(
                &prepared,
                &context,
                "code_facets",
                binding,
                items,
                &request.meta.page,
                "facets",
            )
        })
    }

    fn timeline<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeTimelineRequest,
    ) -> PortFuture<'a, CodeTimelineRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_timeline",
                (
                    "code_timeline",
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let generation = prepared.latest.generation.manifest().generation_id.clone();
            let items = vec![CodeTimelineRecord {
                generation: generation.clone(),
                indexed_at: prepared.latest.generation.manifest().seal.sealed_at,
                file_count: prepared
                    .latest
                    .generation
                    .snapshot()
                    .files
                    .iter()
                    .filter(|file| path_is_in_code_query_scope(&file.logical_path, &request.scope))
                    .count() as u64,
                symbol_count: prepared
                    .latest
                    .record_index()
                    .kind_facet_rows()
                    .iter()
                    .filter(|(_, file_position)| {
                        path_is_in_code_query_scope(
                            &prepared.latest.generation.snapshot().files[*file_position]
                                .logical_path,
                            &request.scope,
                        )
                    })
                    .count() as u64,
            }];
            let page = match CodeQueryPage::new(generation.clone(), items, None, None, None) {
                Ok(page) => page,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "generation-owned timeline page failed the application contract"
                    );
                    return unavailable_for_generation(query_finished_at(), generation);
                }
            };
            finish_direct_query(
                &prepared,
                &context,
                "code_timeline",
                binding,
                page,
                &request.meta.page,
                1,
            )
        })
    }

    fn declaration<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeNavigationRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        navigation_symbol_query(self, context, request, "code_declaration", false)
    }

    fn definition<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeNavigationRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        navigation_symbol_query(self, context, request, "code_definition", false)
    }

    fn type_definition<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeNavigationRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        navigation_symbol_query(self, context, request, "code_type_definition", true)
    }

    fn references<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeNavigationRequest,
    ) -> PortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let (prepared, binding) = prepare_callable_query_or_return!(
                self,
                context,
                request,
                "code_references",
                (
                    "code_references",
                    &request.node_id,
                    &request.scope,
                    &request.meta.projection,
                    &request.meta.order,
                )
            );
            let start = resolve_start_symbol!(prepared, request.node_id);
            let keys = relation_keys(
                &prepared.latest,
                &start,
                &[
                    RelationEdgeKindV1::Calls,
                    RelationEdgeKindV1::Uses,
                    RelationEdgeKindV1::TypeOf,
                    RelationEdgeKindV1::Annotates,
                ],
                true,
                1,
                &request.scope,
            );
            let latest = &prepared.latest;
            finish_generation_candidate_page(
                &prepared,
                &context,
                "code_references",
                binding,
                keys,
                |slice| hydrate_relation_records(latest, slice),
                &request.meta.page,
                "references",
            )
        })
    }
}

fn navigation_symbol_query<'a>(
    registry: &'a CodeIndexSchedulerRegistryV1,
    context: RetrievalPortContext<'a>,
    request: &'a CodeNavigationRequest,
    operation: &'static str,
    resolve_type: bool,
) -> PortFuture<'a, SymbolPrimitiveRecord> {
    Box::pin(async move {
        let (prepared, binding) = prepare_callable_query_or_return!(
            registry,
            context,
            request,
            operation,
            (
                operation,
                &request.node_id,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )
        );
        let start = resolve_start_symbol!(prepared, request.node_id);
        let mut items = Vec::new();
        if let Some(symbol) = symbol_record_by_id(&prepared.latest, &start) {
            let is_type = ["struct", "enum", "class", "interface", "trait", "type"]
                .iter()
                .any(|kind| symbol.kind.to_ascii_lowercase().contains(kind));
            if !resolve_type || is_type {
                items.push(symbol);
            }
        }
        if resolve_type && items.is_empty() {
            let keys = relation_keys(
                &prepared.latest,
                &start,
                &[RelationEdgeKindV1::TypeOf],
                false,
                1,
                &request.scope,
            );
            let latest = &prepared.latest;
            return finish_generation_candidate_page(
                &prepared,
                &context,
                operation,
                binding,
                keys,
                |slice| {
                    hydrate_relation_records(latest, slice).map(|relations| {
                        relations
                            .into_iter()
                            .map(|relation| relation.symbol)
                            .collect()
                    })
                },
                &request.meta.page,
                "symbols",
            );
        }
        items.retain(|symbol| path_is_in_code_query_scope(&symbol.file, &request.scope));
        finish_generation_page(
            &prepared,
            &context,
            operation,
            binding,
            items,
            &request.meta.page,
            "symbols",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_resolution_wait_reserves_outer_settlement_margin() {
        assert_eq!(
            generation_resolution_wait_from_remaining(Duration::ZERO),
            Duration::ZERO
        );
        assert_eq!(
            generation_resolution_wait_from_remaining(Duration::from_millis(999)),
            Duration::ZERO
        );
        assert_eq!(
            generation_resolution_wait_from_remaining(Duration::from_secs(11)),
            Duration::from_secs(10)
        );
        assert_eq!(
            generation_resolution_wait_from_remaining(Duration::from_secs(90)),
            MAX_GENERATION_RESOLUTION_WAIT
        );
    }

    #[tokio::test(start_paused = true)]
    async fn generation_resolution_inner_timeout_precedes_outer_dispatch_expiry() {
        let remaining = Duration::from_secs(5);
        let inner = generation_resolution_wait_from_remaining(remaining);
        let settled = tokio::time::timeout(remaining, async move {
            tokio::time::timeout(inner, std::future::pending::<()>()).await
        })
        .await;

        assert!(
            matches!(settled, Ok(Err(_))),
            "the inner typed timeout must settle before the outer dispatch horizon"
        );
    }

    #[test]
    fn generation_resolution_terminal_projection_records_exactly_one_outcome() {
        use code_search::CodeIndexSearchUnavailableReasonV1 as Reason;

        let cases = [
            (
                "serving hit",
                GenerationResolutionSettlementV1::Completed(Ok(Some("serving"))),
                Ok(Some("serving")),
                GenerationResolutionTerminalV1::Ready,
            ),
            (
                "durable ready",
                GenerationResolutionSettlementV1::Completed(Ok(Some("durable"))),
                Ok(Some("durable")),
                GenerationResolutionTerminalV1::Ready,
            ),
            (
                "unavailable",
                GenerationResolutionSettlementV1::Completed(Ok(None)),
                Ok(None),
                GenerationResolutionTerminalV1::Unavailable,
            ),
            (
                "generation error",
                GenerationResolutionSettlementV1::Completed(Err(Reason::GenerationUnavailable)),
                Err(Reason::GenerationUnavailable),
                GenerationResolutionTerminalV1::Failed,
            ),
            (
                "cancellation",
                GenerationResolutionSettlementV1::Terminated(Reason::Cancelled),
                Err(Reason::Cancelled),
                GenerationResolutionTerminalV1::Failed,
            ),
            (
                "join error",
                GenerationResolutionSettlementV1::JoinFailed,
                Err(Reason::Internal),
                GenerationResolutionTerminalV1::Failed,
            ),
        ];

        for (label, settlement, expected_result, expected_terminal) in cases {
            let mut terminals = Vec::new();
            let result = finish_generation_resolution_with(settlement, |terminal| {
                terminals.push(terminal);
            });
            assert_eq!(result, expected_result, "{label}");
            assert_eq!(terminals, vec![expected_terminal], "{label}");
        }
    }

    fn page(items: Vec<&str>, total: u64) -> CodeQueryPage<String> {
        CodeQueryPage::new(
            CodeGenerationId::new("generation.callable-page").expect("generation"),
            items.into_iter().map(str::to_owned).collect(),
            Some(total),
            None,
            None,
        )
        .expect("page")
    }

    #[test]
    fn bounded_result_preserves_complete_coverage() {
        let outcome = bounded_result(
            page(vec!["one"], 1),
            tracedecay_domain::RetrieverCoverage {
                examined: 1,
                eligible: 1,
                ..Default::default()
            },
            tracedecay_domain::UtcMicros(1),
            None,
            None,
        );
        let RetrievalPortOutcome::Completed(evidence) = outcome else {
            panic!("uncapped complete lane stays complete");
        };
        assert_eq!(
            evidence.coverage.completeness,
            CoverageCompleteness::Complete
        );
        assert!(evidence.omissions.is_empty());
    }

    #[test]
    fn bounded_result_preserves_capped_lane_as_partial() {
        let outcome = bounded_result(
            page(vec!["one"], 3),
            tracedecay_domain::RetrieverCoverage {
                examined: 3,
                eligible: 3,
                capped: 2,
                ..Default::default()
            },
            tracedecay_domain::UtcMicros(1),
            None,
            None,
        );
        let RetrievalPortOutcome::Partial(evidence) = outcome else {
            panic!("capped lane must remain partial");
        };
        assert_eq!(
            evidence.coverage.completeness,
            CoverageCompleteness::Partial
        );
        assert_eq!(evidence.omissions.len(), 1);
        assert_eq!(evidence.omissions[0].reason, OmissionReason::Budget);
        assert_eq!(evidence.omissions[0].count, 2);
    }

    #[test]
    fn bounded_result_treats_a_resumable_page_as_complete() {
        let mut page = page(vec!["one"], 3);
        page.next_cursor =
            Some(OpaqueCursor::new("opaque.resumable").expect("bounded opaque cursor"));
        let outcome = bounded_result(
            page,
            tracedecay_domain::RetrieverCoverage {
                examined: 3,
                eligible: 3,
                ..Default::default()
            },
            UtcMicros(1),
            None,
            None,
        );
        let RetrievalPortOutcome::Completed(evidence) = outcome else {
            panic!("a continuation is not a budget omission");
        };
        assert!(evidence.omissions.is_empty());
        assert_eq!(evidence.page.returned, 1);
        assert_eq!(evidence.page.total, Some(3));
        assert_eq!(evidence.page.expires_at, None);
    }

    #[test]
    fn code_query_path_prefix_is_segment_bounded() {
        let scoped = tracedecay_application::CodeQueryScope::new(
            CodeGenerationId::new("generation.callable-page").expect("generation"),
            Some("crates/app".to_owned()),
        )
        .expect("scope");
        assert!(path_is_in_code_query_scope("crates/app", &scoped));
        assert!(path_is_in_code_query_scope(
            "crates/app/src/lib.rs",
            &scoped
        ));
        assert!(!path_is_in_code_query_scope(
            "crates/application/src/lib.rs",
            &scoped
        ));
        let trailing_slash = tracedecay_application::CodeQueryScope::new(
            CodeGenerationId::new("generation.callable-page").expect("generation"),
            Some("crates/app/".to_owned()),
        )
        .expect("scope");
        assert!(path_is_in_code_query_scope(
            "crates/app/src/lib.rs",
            &trailing_slash
        ));
    }
}
