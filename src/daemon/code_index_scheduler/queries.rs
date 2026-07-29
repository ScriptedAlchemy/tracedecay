//! Callable application operations over the latest sealed PR9 generation.
//!
//! This is the production consumer of the exact, lexical, and graph owners.
//! It selects one already-mounted worktree generation and translates the
//! generic lane evidence into the typed application-operation records.

use std::collections::{BTreeSet, VecDeque};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::retrieval::{
    CodeFacetDimension, CodeFacetRecord, CodeFacetRequest, CodeLexicalField, CodeNavigationRequest,
    CodeTimelineRecord, CodeTimelineRequest, SymbolPrimitiveRecord, SymbolRelationRecord,
    TypeHierarchyRecord,
};
use tracedecay_application::{
    CallableCodeQueryFuture, CallableCodeQueryPort, CodeHierarchyRequest, CodeImpactRequest,
    CodeImplementationsRequest, CodeOccurrenceRecord, CodeQueryPage, CodeRelationRequest,
    CodeSignatureRequest, CodeSymbolSearchRequest, CoverageCompleteness, CoverageDomainState,
    EvidenceCoverage, EvidenceDomain, ExactOccurrenceRecord, ExactOccurrenceRequest,
    LexicalOccurrenceRecord, ModuleApiRequest, Omission, OmissionReason, OpaqueCursor,
    OperationBudgetUsage, PageState, PhraseSearchRequest, QualifiedNameRequest, RequestAdmission,
    RequestContext, RetrievalEvidence, RetrievalPortContext, RetrievalPortOutcome,
    SourceMetadataRecord, SourceMetadataRequest, TemporalState,
};
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, ComponentRevision, ExactAdmissionRuleRevision,
    FreshnessVectorDigest, ManifestDigest, PrincipalId, QueryNormalizationRevision,
    RelationEdgeKindV1, RetrievalAnchorId, RetrievalBudget, RetrievalFailure, RetrievalRequest,
    RetrievalScope, RetrievalSnapshot, RetrieverBatch, RetrieverOutcome, SanitizerRevision,
    ScoreDomainId, SingleRootScopeV1, SourceOccurrenceId, SymbolOccurrenceId, TemporalModeV1,
    UtcMicros, VectorWatermark, canonical_sha256,
};
use tracedecay_tool_catalog::SortContractId;

use super::{CodeIndexSchedulerRegistryV1, LatestCompleteCodeIndexV1};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest,
    ExactLaneRetriever,
};
use tracedecay_query::retrieval::graph::{GraphLaneEvidence, GraphLaneRequest, GraphLaneRetriever};
use tracedecay_query::retrieval::lexical::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest,
    LexicalLaneRetriever,
};
use tracedecay_query::retrieval::ports::{CodeCandidateBindingV1, CodeOccurrenceRefV1};
use tracedecay_query::retrieval::{
    PreparedQueryBindingsV1, PreparedQueryErrorV1, PreparedQueryV1,
    inspect_prepared_query_cursor,
};

const CALLABLE_CODE_SORT: &str = "sort.application.code-index.v1";
const MAX_GENERATION_RESOLUTION_WAIT: Duration = Duration::from_secs(30);

/// The reserved [`CodeGenerationId`] a caller supplies to request ordinary
/// (unpinned) search: it pins no specific immutable generation, so the serving
/// generation is resolved through the three-tier freshness ladder to the latest
/// complete compatible generation (NEXT.md clause 8). Any other value is an
/// explicit caller pin that is served generation-bound and read-only.
pub(in crate::daemon) const UNPINNED_LATEST_GENERATION_SENTINEL: &str =
    "code-generation:unpinned-latest.v1";

/// Construct the reserved unpinned-latest [`CodeGenerationId`] sentinel a caller
/// passes when it wants the freshness-resolved latest complete generation
/// rather than a pinned one.
pub(in crate::daemon) fn unpinned_latest_generation() -> CodeGenerationId {
    CodeGenerationId::new(UNPINNED_LATEST_GENERATION_SENTINEL)
        .unwrap_or_else(|_| panic!("static unpinned-latest generation sentinel is valid"))
}

pub(in crate::daemon) fn callable_query_sanitizer_revision() -> SanitizerRevision {
    SanitizerRevision::new(tracedecay_query::retrieval::PR9_QUERY_SANITIZER_REVISION_V1)
        .unwrap_or_else(|_| panic!("static sanitizer revision"))
}

pub(in crate::daemon) fn callable_query_normalization_revision() -> QueryNormalizationRevision {
    QueryNormalizationRevision::new(
        tracedecay_query::retrieval::PR9_QUERY_NORMALIZATION_REVISION_V1,
    )
    .unwrap_or_else(|_| panic!("static normalization revision"))
}

fn is_unpinned_latest(generation: &CodeGenerationId) -> bool {
    generation.as_str() == UNPINNED_LATEST_GENERATION_SENTINEL
}

pub(super) fn semantic_mcp_reason(
    current_source: Option<&CodeGenerationId>,
    latest_code_generation: &CodeGenerationId,
    runtime_state: Option<&crate::application::semantic_runtime::SemanticRuntimeStateV1>,
) -> &'static str {
    if let Some(source_generation) = current_source {
        return if source_generation == latest_code_generation {
            // Plan 15 has not published an accepted calibration authority. A
            // current vector generation alone cannot authorize influence.
            "calibration_unavailable"
        } else {
            "semantic_generation_stale"
        };
    }
    match runtime_state {
        None
        | Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Unavailable {
            ..
        }) => "semantic_runtime_unavailable",
        Some(
            crate::application::semantic_runtime::SemanticRuntimeStateV1::SelectedNotDownloaded {
                ..
            },
        ) => "semantic_model_not_downloaded",
        Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Downloading {
            ..
        }) => "semantic_model_downloading",
        Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Verifying {
            ..
        }) => "semantic_model_verifying",
        Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Installed {
            ..
        }) => "semantic_model_installed",
        Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Loading { .. }) => {
            "semantic_model_loading"
        }
        Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Indexing { .. }) => {
            "semantic_indexing"
        }
        Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Current { .. }) => {
            "semantic_generation_incompatible"
        }
        Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Degraded { .. }) => {
            "semantic_degraded"
        }
        Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Rollback { .. }) => {
            "semantic_rollback"
        }
        Some(crate::application::semantic_runtime::SemanticRuntimeStateV1::Failed { .. }) => {
            "semantic_failed"
        }
    }
}

impl CodeIndexSchedulerRegistryV1 {
    /// Compose real exact/lexical/graph lane outcomes only through the
    /// accepted profile and query/cursor key authority mounted for this exact
    /// admitted scope.
    pub(in crate::daemon) async fn compose_pr9_fallback(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        request: &tracedecay_domain::RetrievalRequest,
        query_view: &tracedecay_domain::EphemeralSanitizedQueryViewV1,
        lanes: Vec<tracedecay_query::retrieval::fusion::CompositionLaneInput>,
        page_size: usize,
        cursor: Option<&tracedecay_domain::RetrievalCursor>,
    ) -> Result<
        tracedecay_query::retrieval::AuthorizedPr9FallbackV1,
        tracedecay_query::retrieval::Pr9QueryAuthorityErrorV1,
    > {
        let authority = self
            .pr9_query_authority_for_scope(scope)
            .await
            .ok_or(tracedecay_query::retrieval::Pr9QueryAuthorityErrorV1::AuthorityUnavailable)?;
        authority.compose(request, query_view, lanes, page_size, cursor)
    }

    /// Resolve strict-semantic availability against this project's freshest
    /// complete code generation.
    ///
    /// No semantic query is constructed until an accepted calibration
    /// authority exists. That keeps ordinary PR9 fallback byte-stable and
    /// makes a strict request fail with a typed reason instead of inventing a
    /// score, profile, or candidate.
    pub(in crate::daemon) async fn semantic_mcp_abstention(
        &self,
        project_root: &Path,
    ) -> crate::mcp::server::CodeIndexSemanticAbstentionV1 {
        let Some(latest) = self.latest_complete_fresh(project_root).await else {
            return crate::mcp::server::CodeIndexSemanticAbstentionV1 {
                code_generation: None,
                reason: "code_index_unavailable",
            };
        };
        let code_generation = latest.generation.manifest().generation_id.clone();
        let code_generation_display = Some(code_generation.as_str().to_owned());
        let current_source =
            crate::application::semantic_runtime::project_semantic_source_generation(project_root);
        let status = crate::application::semantic_runtime::project_semantic_application_status(
            project_root,
            None,
        );
        let reason = semantic_mcp_reason(
            current_source.as_ref(),
            &code_generation,
            status.as_ref().map(|status| &status.state),
        );
        crate::mcp::server::CodeIndexSemanticAbstentionV1 {
            code_generation: code_generation_display,
            reason,
        }
    }

    pub(in crate::daemon) async fn generation_for(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        generation_id: &CodeGenerationId,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let (scheduler, serving_generation) = {
            let mounted = self.mounted.lock().await;
            let mut matched = None;
            for worktree in mounted.values() {
                if worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
                {
                    if matched.is_some() {
                        return None;
                    }
                    matched = Some((
                        std::sync::Arc::clone(&worktree.scheduler),
                        std::sync::Arc::clone(&worktree.serving_generation),
                    ));
                }
            }
            matched?
        };
        let scope = scope.clone();
        let generation_id = generation_id.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(generation) = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .filter(|generation| {
                    generation.generation.manifest().generation_id == generation_id
                        && Self::latest_matches_scope(generation, &scope)
                })
                .cloned()
            {
                return Some(generation);
            }
            let scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let generation = scheduler.generation(&generation_id).ok().flatten()?;
            Self::latest_matches_scope(&generation, &scope).then_some(generation)
        })
        .await
        .ok()
        .flatten()
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
    pub(super) async fn resolve_serving_generation(
        &self,
        request: &RequestContext,
        requested: &CodeGenerationId,
        page: &tracedecay_application::PageRequest,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let wait = remaining_generation_resolution_wait(request)?;
        let resolution = async {
            if is_unpinned_latest(requested) {
                if let Some(cursor) = page.cursor.as_ref() {
                    let cursor = inspect_prepared_query_cursor(cursor.as_str()).ok()?;
                    self.generation_for(request.scope(), &cursor.generation)
                        .await
                } else {
                    self.latest_complete_fresh_for_scope(request.scope()).await
                }
            } else {
                self.generation_for(request.scope(), requested).await
            }
        };
        let latest = tokio::time::timeout(wait, resolution)
            .await
            .ok()
            .flatten()?;
        matches!(
            request.admission_at(current_utc_micros().ok()?),
            RequestAdmission::Admitted
        )
        .then_some(latest)
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

pub(in crate::daemon) fn maximum_retrieval_budget() -> RetrievalBudget {
    retrieval_budget(tracedecay_application::MAX_APPLICATION_PAGE_SIZE)
}

type CallableCodeCursorError = PreparedQueryErrorV1;

fn current_utc_micros() -> Result<UtcMicros, CallableCodeCursorError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CallableCodeCursorError::Unavailable)?;
    i64::try_from(elapsed.as_micros())
        .map(UtcMicros)
        .map_err(|_| CallableCodeCursorError::Unavailable)
}

fn query_finished_at() -> UtcMicros {
    current_utc_micros().unwrap_or(UtcMicros(0))
}

fn remaining_generation_resolution_wait(request: &RequestContext) -> Option<Duration> {
    let now = current_utc_micros().ok()?;
    if request.admission_at(now) != RequestAdmission::Admitted {
        return None;
    }
    let remaining = request.deadline().expires_at.0.checked_sub(now.0)?;
    let remaining = u64::try_from(remaining).ok().map(Duration::from_micros)?;
    Some(remaining.min(MAX_GENERATION_RESOLUTION_WAIT))
}

fn reject_unresolved_cursor<T>(
    requested_page: &tracedecay_application::PageRequest,
    generation: &CodeGenerationId,
) -> RetrievalPortOutcome<T> {
    if requested_page.cursor.is_some() {
        rejected_cursor(
            query_finished_at(),
            generation.clone(),
            CallableCodeCursorError::Invalid,
        )
    } else {
        unavailable(query_finished_at())
    }
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
        page: PageState::first_page(
            SortContractId::new(CALLABLE_CODE_SORT).unwrap_or_else(|_| panic!("static sort id")),
            1,
            None,
            0,
        )
        .unwrap_or_else(|_| panic!("empty application page")),
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
    let mut page_state = PageState::first_page(
        SortContractId::new(CALLABLE_CODE_SORT).unwrap_or_else(|_| panic!("static sort id")),
        1,
        page.total,
        returned,
    )
    .unwrap_or_else(|_| panic!("bounded code query page"));
    page_state.cursor.clone_from(&page.next_cursor);
    page_state.expires_at = page
        .next_cursor
        .as_ref()
        .and_then(|cursor| inspect_prepared_query_cursor(cursor.as_str()).ok())
        .map(|cursor| cursor.expires_at);
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

fn chunk_occurrence(
    latest: &LatestCompleteCodeIndexV1,
    binding: &CodeCandidateBindingV1,
) -> Option<CodeOccurrenceRecord> {
    let file = latest
        .generation
        .snapshot()
        .files
        .iter()
        .find(|file| file.file_occurrence_id == binding.occurrence.file)?;
    let chunk = binding.occurrence.chunk.as_ref().and_then(|chunk_id| {
        latest
            .generation
            .chunks()
            .chunks()
            .iter()
            .find(|chunk| &chunk.id == chunk_id)
    });
    Some(CodeOccurrenceRecord {
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

fn path_is_in_code_query_scope(path: &str, scope: &tracedecay_application::CodeQueryScope) -> bool {
    crate::path_scope::path_matches_scope(path, scope.path_prefix.as_deref())
}

fn exact_page(
    latest: &LatestCompleteCodeIndexV1,
    served_generation: &CodeGenerationId,
    request: &ExactOccurrenceRequest,
    batch: RetrieverBatch<ExactLaneEvidence>,
) -> CodeQueryPage<ExactOccurrenceRecord> {
    let mut items = Vec::new();
    for candidate in &batch.candidates {
        let Some(evidence) = batch
            .evidence_by_occurrence
            .get(&candidate.source_occurrence_id)
        else {
            continue;
        };
        let Some(matched_kind) = evidence
            .binding
            .matched_term_kinds
            .iter()
            .copied()
            .find(|kind| request.kind.is_none_or(|expected| expected == *kind))
        else {
            continue;
        };
        let Some(occurrence) = chunk_occurrence(latest, &evidence.binding) else {
            continue;
        };
        if !path_is_in_code_query_scope(&occurrence.path, &request.scope) {
            continue;
        }
        items.push(ExactOccurrenceRecord {
            occurrence,
            matched_kind,
            matched_literal: request.literal.clone(),
        });
    }
    CodeQueryPage::new(
        served_generation.clone(),
        items,
        Some(batch.coverage.eligible),
        None,
        None,
    )
    .unwrap_or_else(|_| panic!("validated exact lane creates a valid page"))
}

fn lexical_page(
    latest: &LatestCompleteCodeIndexV1,
    served_generation: &CodeGenerationId,
    request: &PhraseSearchRequest,
    batch: RetrieverBatch<LexicalLaneEvidence>,
) -> CodeQueryPage<LexicalOccurrenceRecord> {
    let mut items = Vec::new();
    for candidate in &batch.candidates {
        let Some(evidence) = batch
            .evidence_by_occurrence
            .get(&candidate.source_occurrence_id)
        else {
            continue;
        };
        let Some(occurrence) = chunk_occurrence(latest, &evidence.binding) else {
            continue;
        };
        if !path_is_in_code_query_scope(&occurrence.path, &request.scope) {
            continue;
        }
        items.push(LexicalOccurrenceRecord {
            occurrence,
            score_micros: candidate.raw_score.0,
            matched_phrases: request.phrases.clone(),
            matched_terms: evidence
                .matched_whole_terms
                .iter()
                .chain(&evidence.matched_subtokens)
                .cloned()
                .collect(),
        });
    }
    CodeQueryPage::new(
        served_generation.clone(),
        items,
        Some(batch.coverage.eligible),
        None,
        None,
    )
    .unwrap_or_else(|_| panic!("validated lexical lane creates a valid page"))
}

fn symbol_record(
    latest: &LatestCompleteCodeIndexV1,
    symbol: &SymbolOccurrenceId,
    file: &tracedecay_domain::FileOccurrenceId,
) -> Option<SymbolPrimitiveRecord> {
    let lineage = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| &record.occurrence == symbol)?;
    let source = latest
        .generation
        .snapshot()
        .files
        .iter()
        .find(|source| &source.file_occurrence_id == file)?;
    let qualified_name = lineage.qualified_name.clone();
    let name = qualified_name
        .rsplit("::")
        .next()
        .unwrap_or(&qualified_name)
        .to_owned();
    Some(SymbolPrimitiveRecord {
        node_id: symbol.as_str().to_owned(),
        name,
        qualified_name,
        kind: lineage.kind.clone(),
        file: source.logical_path.clone(),
        start_line_zero_based: 0,
        end_line_zero_based: 0,
        line: 1,
        end_line: 1,
        signature: None,
        is_async: false,
        score: None,
    })
}

fn symbol_record_by_id(
    latest: &LatestCompleteCodeIndexV1,
    symbol: &SymbolOccurrenceId,
) -> Option<SymbolPrimitiveRecord> {
    let chunk = latest
        .generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.anchor.symbol_occurrence_id.as_ref() == Some(symbol))?;
    let mut record = symbol_record(latest, symbol, &chunk.anchor.file_occurrence_id)?;
    let signature = chunk
        .sanitized_text
        .as_str()
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned);
    record.is_async = signature
        .as_deref()
        .is_some_and(|line| line.split_whitespace().any(|part| part == "async"));
    record.signature = signature;
    Some(record)
}

fn symbol_page(
    generation: &CodeGenerationId,
    items: Vec<SymbolPrimitiveRecord>,
) -> CodeQueryPage<SymbolPrimitiveRecord> {
    CodeQueryPage::new(generation.clone(), items, None, None, None)
        .unwrap_or_else(|_| panic!("generation-owned symbols create a valid page"))
}

struct PreparedCallableQueryV1 {
    latest: LatestCompleteCodeIndexV1,
    query: PreparedQueryV1,
}

macro_rules! prepare_callable_query_or_return {
    ($registry:expr, $context:expr, $request:expr) => {
        match $registry
            .prepare_callable_query(
                &$context,
                &$request.scope.generation,
                &$request.meta.page,
                $request.meta.temporal,
            )
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return rejected_cursor(
                    query_finished_at(),
                    $request.scope.generation.clone(),
                    error,
                );
            }
        }
    };
}

impl CodeIndexSchedulerRegistryV1 {
    async fn prepare_callable_query(
        &self,
        context: &RetrievalPortContext<'_>,
        generation: &CodeGenerationId,
        page: &tracedecay_application::PageRequest,
        temporal: TemporalModeV1,
    ) -> Result<PreparedCallableQueryV1, CallableCodeCursorError> {
        let latest = self
            .resolve_serving_generation(context.request, generation, page)
            .await
            .ok_or(if page.cursor.is_some() {
                CallableCodeCursorError::Invalid
            } else {
                CallableCodeCursorError::Unavailable
            })?;
        let authority = self
            .pr9_query_authority_for_scope(context.request.scope())
            .await
            .ok_or(CallableCodeCursorError::Unavailable)?;
        let base = base_request(context, &latest, temporal, authority.profile())
            .map_err(|_| CallableCodeCursorError::Unavailable)?;
        let query = PreparedQueryV1::prepare(
            authority,
            base,
            page.cursor.as_ref().map(OpaqueCursor::as_str),
        )?;
        Ok(PreparedCallableQueryV1 {
            latest,
            query,
        })
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
    let finished_at = query_finished_at();
    let generation = prepared.latest.generation.manifest().generation_id.clone();
    let bindings = PreparedQueryBindingsV1::new(
        operation,
        context.request.scope().scope_digest.clone(),
        generation.clone(),
        query_binding_digest,
    );
    let pagination = bindings.and_then(|bindings| {
        prepared.query.paginate(
            &bindings,
            page.items,
            requested_page.page_size,
            finished_at,
        )
    });
    match pagination {
        Ok(pagination) => {
            let next_cursor = pagination
                .next_cursor
                .map(OpaqueCursor::new)
                .transpose()
                .map_err(|_| PreparedQueryErrorV1::Unavailable);
            let Ok(next_cursor) = next_cursor else {
                return rejected_cursor(
                    finished_at,
                    generation,
                    PreparedQueryErrorV1::Unavailable,
                );
            };
            let page = CodeQueryPage::new(
                generation,
                pagination.items,
                Some(pagination.total),
                next_cursor,
                page.previous_cursor,
            )
            .unwrap_or_else(|_| panic!("prepared query creates a valid application page"));
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
            )
        }
        Err(error) => rejected_cursor(finished_at, generation, error),
    }
}

fn relation_records(
    latest: &LatestCompleteCodeIndexV1,
    start: &SymbolOccurrenceId,
    kinds: &[RelationEdgeKindV1],
    reverse: bool,
    maximum_depth: u32,
    scope: &tracedecay_application::CodeQueryScope,
) -> Vec<SymbolRelationRecord> {
    let mut queue = VecDeque::from([(start.clone(), 0_u32)]);
    let mut visited = BTreeSet::from([start.clone()]);
    let mut records = Vec::new();
    while let Some((current, depth)) = queue.pop_front() {
        if depth >= maximum_depth {
            continue;
        }
        for edge in latest.generation.edges().iter().filter(|edge| {
            kinds.contains(&edge.kind)
                && if reverse {
                    edge.to_occurrence == current
                } else {
                    edge.from_occurrence == current
                }
        }) {
            let next = if reverse {
                &edge.from_occurrence
            } else {
                &edge.to_occurrence
            };
            if !visited.insert(next.clone()) {
                continue;
            }
            let Some(symbol) = symbol_record_by_id(latest, next) else {
                continue;
            };
            if !path_is_in_code_query_scope(&symbol.file, scope) {
                continue;
            }
            records.push(SymbolRelationRecord {
                symbol,
                edge_kind: format!("{:?}", edge.kind).to_ascii_lowercase(),
                dispatch_via_trait: edge.kind == RelationEdgeKindV1::Implements,
                dispatch_from: (edge.kind == RelationEdgeKindV1::Implements)
                    .then(|| current.as_str().to_owned()),
                depth: Some(depth + 1),
            });
            queue.push_back((next.clone(), depth + 1));
        }
    }
    records.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then(left.symbol.node_id.cmp(&right.symbol.node_id))
    });
    records
}

fn graph_page(
    latest: &LatestCompleteCodeIndexV1,
    served_generation: &CodeGenerationId,
    request: &CodeRelationRequest,
    batch: RetrieverBatch<GraphLaneEvidence>,
) -> CodeQueryPage<SymbolRelationRecord> {
    let mut items = Vec::new();
    for candidate in &batch.candidates {
        let Some(evidence) = batch
            .evidence_by_occurrence
            .get(&candidate.source_occurrence_id)
        else {
            continue;
        };
        let Some(symbol) = evidence.binding.occurrence.symbol.as_ref() else {
            continue;
        };
        let Some(record) = symbol_record(latest, symbol, &evidence.binding.occurrence.file) else {
            continue;
        };
        if !path_is_in_code_query_scope(&record.file, &request.scope) {
            continue;
        }
        let edge_kind = evidence.path.last().map_or_else(
            || "unknown".to_owned(),
            |edge| format!("{:?}", edge.edge_kind).to_ascii_lowercase(),
        );
        items.push(SymbolRelationRecord {
            symbol: record,
            edge_kind,
            dispatch_via_trait: false,
            dispatch_from: None,
            depth: Some(evidence.path.len() as u32),
        });
    }
    CodeQueryPage::new(
        served_generation.clone(),
        items,
        Some(batch.coverage.eligible),
        None,
        None,
    )
    .unwrap_or_else(|_| panic!("validated graph lane creates a valid page"))
}

#[allow(clippy::too_many_arguments)]
fn finish_lane_query<T, E>(
    prepared: &PreparedCallableQueryV1,
    context: &RetrievalPortContext<'_>,
    operation: &'static str,
    query_binding_digest: ManifestDigest,
    requested_page: &tracedecay_application::PageRequest,
    outcome: RetrieverOutcome<RetrieverBatch<E>>,
    map: impl FnOnce(RetrieverBatch<E>) -> CodeQueryPage<T>,
) -> RetrievalPortOutcome<CodeQueryPage<T>>
where
    T: serde::Serialize,
{
    let finished_at = query_finished_at();
    match outcome {
        RetrieverOutcome::Complete(batch) => {
            let coverage = batch.coverage;
            finish_direct_query(
                prepared,
                context,
                operation,
                query_binding_digest,
                map(batch),
                requested_page,
                coverage.eligible,
            )
        }
        RetrieverOutcome::Partial { value, reason } => {
            let coverage = value.coverage;
            let outcome = finish_direct_query(
                prepared,
                context,
                operation,
                query_binding_digest,
                map(value),
                requested_page,
                coverage.eligible,
            );
            let partial_reason = match reason {
                RetrievalFailure::AuthorityUnavailable { .. } => OmissionReason::Unavailable,
                RetrievalFailure::IncompatibleProjection { .. } => OmissionReason::Unsupported,
                RetrievalFailure::StaleSource => OmissionReason::Stale,
                RetrievalFailure::InvalidRequest { .. } | RetrievalFailure::Internal { .. } => {
                    OmissionReason::Failed
                }
            };
            match outcome {
                RetrievalPortOutcome::Completed(evidence)
                | RetrievalPortOutcome::Partial(evidence) => {
                    let mut evidence = evidence;
                    evidence.omissions.push(Omission {
                        domain: EvidenceDomain::Symbol,
                        count: coverage
                            .eligible
                            .saturating_sub(evidence.coverage.returned),
                        reason: partial_reason,
                    });
                    evidence.coverage.completeness = CoverageCompleteness::Partial;
                    RetrievalPortOutcome::Partial(evidence)
                }
                outcome => outcome,
            }
        }
        _ => unavailable(finished_at),
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
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let latest = &prepared.latest;
            let served_generation = latest.generation.manifest().generation_id.clone();
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
                    tracedecay_query::retrieval::PR9_EXACT_RULE_REVISION_V1,
                )
                .unwrap_or_else(|_| panic!("static exact rule revision")),
            );
            let lane_request = ExactLaneRequest {
                literals: authority.parse_literals(&query_view, &base),
                generation: served_generation.clone(),
                budget: base.budget,
                base: base.clone(),
                query_view: &query_view,
            };
            let Ok(query_binding_digest) = canonical_sha256(&(
                "code_exact_occurrence",
                &request.literal,
                &request.kind,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(finished_at);
            };
            let Ok(owners) = latest.production_query_owners() else {
                return unavailable(finished_at);
            };
            let outcome = owners.exact.retrieve_exact(&lane_request);
            match outcome {
                Ok(outcome) => finish_lane_query(
                    &prepared,
                    &context,
                    "code_exact_occurrence",
                    query_binding_digest,
                    &request.meta.page,
                    outcome,
                    |batch| exact_page(latest, &served_generation, request, batch),
                ),
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
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let latest = &prepared.latest;
            let served_generation = latest.generation.manifest().generation_id.clone();
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
                    tracedecay_query::retrieval::PR9_LEXICAL_PROFILE_REVISION_V1,
                )
                .unwrap_or_else(|_| panic!("static lexical profile")),
                score_domain: ScoreDomainId::new(
                    tracedecay_query::retrieval::PR9_LEXICAL_SCORE_DOMAIN_V1,
                )
                .unwrap_or_else(|_| panic!("static lexical score domain")),
                budget: base.budget,
                base: base.clone(),
            };
            let Ok(query_binding_digest) = canonical_sha256(&(
                "code_phrase_search",
                request.query.as_str(),
                &request.phrases,
                &request.field_filters,
                request.fuzzy_budget,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(finished_at);
            };
            let Ok(owners) = latest.production_query_owners() else {
                return unavailable(finished_at);
            };
            let outcome = owners.lexical.retrieve_lexical(&lane_request);
            match outcome {
                Ok(outcome) => finish_lane_query(
                    &prepared,
                    &context,
                    "code_phrase_search",
                    query_binding_digest,
                    &request.meta.page,
                    outcome,
                    |batch| lexical_page(latest, &served_generation, request, batch),
                ),
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
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let latest = &prepared.latest;
            let served_generation = latest.generation.manifest().generation_id.clone();
            let finished_at = query_finished_at();
            let base = prepared.query.request();
            let Ok(symbol) = typed::<SymbolOccurrenceId>(request.node_id.clone()) else {
                return unavailable(finished_at);
            };
            let Some(chunk) = latest
                .generation
                .chunks()
                .chunks()
                .iter()
                .find(|chunk| chunk.anchor.symbol_occurrence_id.as_ref() == Some(&symbol))
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
                budget: base.budget,
                base: base.clone(),
            };
            let Ok(query_binding_digest) = canonical_sha256(&(
                "code_callees",
                &request.node_id,
                request.maximum_depth,
                request.resolve_trait_dispatch,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(finished_at);
            };
            let Ok(owners) = latest.production_query_owners() else {
                return unavailable(finished_at);
            };
            let outcome = owners.graph.retrieve_graph(&lane_request);
            match outcome {
                Ok(outcome) => finish_lane_query(
                    &prepared,
                    &context,
                    "code_callees",
                    query_binding_digest,
                    &request.meta.page,
                    outcome,
                    |batch| graph_page(latest, &served_generation, request, batch),
                ),
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
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let query = request.query.as_str().to_ascii_lowercase();
            let mut ranked = prepared
                .latest
                .generation
                .symbols()
                .symbols
                .iter()
                .filter_map(|symbol| {
                    let mut record = symbol_record_by_id(&prepared.latest, &symbol.occurrence)?;
                    if !path_is_in_code_query_scope(&record.file, &request.scope) {
                        return None;
                    }
                    let name = record.name.to_ascii_lowercase();
                    let qualified = record.qualified_name.to_ascii_lowercase();
                    let tier = if name == query || qualified == query {
                        0_u8
                    } else if name.starts_with(&query) || qualified.starts_with(&query) {
                        1
                    } else if name.contains(&query) || qualified.contains(&query) {
                        2
                    } else {
                        return None;
                    };
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
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_symbol_search",
                request.query.as_str(),
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            finish_direct_query(
                &prepared,
                &context,
                "code_symbol_search",
                binding,
                symbol_page(&prepared.latest.generation.manifest().generation_id, items),
                &request.meta.page,
                eligible,
            )
        })
    }

    fn qualified_name<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a QualifiedNameRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let mut items = prepared
                .latest
                .generation
                .symbols()
                .symbols
                .iter()
                .filter(|symbol| symbol.qualified_name == request.qualified_name)
                .filter_map(|symbol| symbol_record_by_id(&prepared.latest, &symbol.occurrence))
                .filter(|record| path_is_in_code_query_scope(&record.file, &request.scope))
                .collect::<Vec<_>>();
            items.sort_by(|left, right| left.node_id.cmp(&right.node_id));
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_qualified_name",
                &request.qualified_name,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            finish_direct_query(
                &prepared,
                &context,
                "code_qualified_name",
                binding,
                symbol_page(&prepared.latest.generation.manifest().generation_id, items),
                &request.meta.page,
                eligible,
            )
        })
    }

    fn signature_search<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeSignatureRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let mut items = prepared
                .latest
                .generation
                .symbols()
                .symbols
                .iter()
                .filter_map(|symbol| symbol_record_by_id(&prepared.latest, &symbol.occurrence))
                .filter(|record| path_is_in_code_query_scope(&record.file, &request.scope))
                .filter(|record| {
                    let Some(signature) = record.signature.as_deref() else {
                        return false;
                    };
                    request
                        .returns
                        .as_ref()
                        .is_none_or(|returns| signature.contains(returns))
                        && request.params.iter().all(|param| signature.contains(param))
                        && request
                            .is_async
                            .is_none_or(|is_async| record.is_async == is_async)
                })
                .collect::<Vec<_>>();
            items.sort_by(|left, right| {
                left.qualified_name
                    .cmp(&right.qualified_name)
                    .then(left.node_id.cmp(&right.node_id))
            });
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_signature_search",
                &request.returns,
                &request.params,
                request.is_async,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            finish_direct_query(
                &prepared,
                &context,
                "code_signature_search",
                binding,
                symbol_page(&prepared.latest.generation.manifest().generation_id, items),
                &request.meta.page,
                eligible,
            )
        })
    }

    fn implementations<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeImplementationsRequest,
    ) -> PortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let selector = match &request.selector {
                tracedecay_application::retrieval::ImplementationSelector::Trait { name }
                | tracedecay_application::retrieval::ImplementationSelector::Method { name } => {
                    name
                }
            };
            let mut items = Vec::new();
            for target in prepared
                .latest
                .generation
                .symbols()
                .symbols
                .iter()
                .filter(|symbol| {
                    symbol.qualified_name == *selector
                        || symbol
                            .qualified_name
                            .rsplit("::")
                            .next()
                            .is_some_and(|name| name == selector)
                })
            {
                items.extend(relation_records(
                    &prepared.latest,
                    &target.occurrence,
                    &[RelationEdgeKindV1::Implements],
                    true,
                    1,
                    &request.scope,
                ));
            }
            items.sort_by(|left, right| left.symbol.node_id.cmp(&right.symbol.node_id));
            items.dedup_by(|left, right| left.symbol.node_id == right.symbol.node_id);
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_implementations",
                &request.selector,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            let page = CodeQueryPage::new(
                prepared.latest.generation.manifest().generation_id.clone(),
                items,
                None,
                None,
                None,
            )
            .unwrap_or_else(|_| panic!("generation-owned implementations create a valid page"));
            finish_direct_query(
                &prepared,
                &context,
                "code_implementations",
                binding,
                page,
                &request.meta.page,
                eligible,
            )
        })
    }

    fn type_hierarchy<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeHierarchyRequest,
    ) -> PortFuture<'a, TypeHierarchyRecord> {
        Box::pin(async move {
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let Ok(start) = typed::<SymbolOccurrenceId>(request.node_id.clone()) else {
                return unavailable(query_finished_at());
            };
            if symbol_record_by_id(&prepared.latest, &start).is_none() {
                return unavailable_for_generation(
                    query_finished_at(),
                    prepared.latest.generation.manifest().generation_id.clone(),
                );
            }
            let relations = relation_records(
                &prepared.latest,
                &start,
                &[RelationEdgeKindV1::Implements, RelationEdgeKindV1::Extends],
                false,
                request.maximum_depth,
                &request.scope,
            );
            let items = relations
                .into_iter()
                .map(|relation| TypeHierarchyRecord {
                    parent_node_id: request.node_id.clone(),
                    edge_kind: relation.edge_kind,
                    depth: relation.depth.unwrap_or(1),
                    symbol: relation.symbol,
                })
                .collect::<Vec<_>>();
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_type_hierarchy",
                &request.node_id,
                request.maximum_depth,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            let page = CodeQueryPage::new(
                prepared.latest.generation.manifest().generation_id.clone(),
                items,
                None,
                None,
                None,
            )
            .unwrap_or_else(|_| panic!("generation-owned hierarchy creates a valid page"));
            finish_direct_query(
                &prepared,
                &context,
                "code_type_hierarchy",
                binding,
                page,
                &request.meta.page,
                eligible,
            )
        })
    }

    fn callers<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeRelationRequest,
    ) -> PortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let Ok(start) = typed::<SymbolOccurrenceId>(request.node_id.clone()) else {
                return unavailable(query_finished_at());
            };
            if symbol_record_by_id(&prepared.latest, &start).is_none() {
                return unavailable_for_generation(
                    query_finished_at(),
                    prepared.latest.generation.manifest().generation_id.clone(),
                );
            }
            let items = relation_records(
                &prepared.latest,
                &start,
                &[RelationEdgeKindV1::Calls],
                true,
                request.maximum_depth,
                &request.scope,
            );
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_callers",
                &request.node_id,
                request.maximum_depth,
                request.resolve_trait_dispatch,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            let page = CodeQueryPage::new(
                prepared.latest.generation.manifest().generation_id.clone(),
                items,
                None,
                None,
                None,
            )
            .unwrap_or_else(|_| panic!("generation-owned callers create a valid page"));
            finish_direct_query(
                &prepared,
                &context,
                "code_callers",
                binding,
                page,
                &request.meta.page,
                eligible,
            )
        })
    }

    fn impact<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeImpactRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let Ok(start) = typed::<SymbolOccurrenceId>(request.node_id.clone()) else {
                return unavailable(query_finished_at());
            };
            if symbol_record_by_id(&prepared.latest, &start).is_none() {
                return unavailable_for_generation(
                    query_finished_at(),
                    prepared.latest.generation.manifest().generation_id.clone(),
                );
            }
            let relations = relation_records(
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
            let items = relations
                .into_iter()
                .map(|relation| relation.symbol)
                .collect::<Vec<_>>();
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_impact",
                &request.node_id,
                request.maximum_depth,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            finish_direct_query(
                &prepared,
                &context,
                "code_impact",
                binding,
                symbol_page(&prepared.latest.generation.manifest().generation_id, items),
                &request.meta.page,
                eligible,
            )
        })
    }

    fn module_api<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a ModuleApiRequest,
    ) -> PortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let prefix = format!("{}/", request.path.trim_end_matches('/'));
            let mut items = prepared
                .latest
                .generation
                .symbols()
                .symbols
                .iter()
                .filter_map(|symbol| symbol_record_by_id(&prepared.latest, &symbol.occurrence))
                .filter(|record| {
                    (record.file == request.path || record.file.starts_with(&prefix))
                        && path_is_in_code_query_scope(&record.file, &request.scope)
                        && record.signature.as_deref().is_some_and(|signature| {
                            let signature = signature.trim_start();
                            signature.starts_with("pub ")
                                || signature.starts_with("export ")
                                || signature.starts_with("public ")
                        })
                })
                .collect::<Vec<_>>();
            items.sort_by(|left, right| {
                left.file
                    .cmp(&right.file)
                    .then(left.qualified_name.cmp(&right.qualified_name))
                    .then(left.node_id.cmp(&right.node_id))
            });
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_module_api",
                &request.path,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            finish_direct_query(
                &prepared,
                &context,
                "code_module_api",
                binding,
                symbol_page(&prepared.latest.generation.manifest().generation_id, items),
                &request.meta.page,
                eligible,
            )
        })
    }

    fn source_metadata<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceMetadataRequest,
    ) -> PortFuture<'a, SourceMetadataRecord> {
        Box::pin(async move {
            let prepared = prepare_callable_query_or_return!(self, context, request);
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
            let Ok(binding) = canonical_sha256(&(
                "code_source_metadata",
                &request.files,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            let page = CodeQueryPage::new(
                prepared.latest.generation.manifest().generation_id.clone(),
                items,
                None,
                None,
                None,
            )
            .unwrap_or_else(|_| panic!("generation-owned metadata creates a valid page"));
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
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let mut counts = std::collections::BTreeMap::<String, u64>::new();
            match request.dimension {
                CodeFacetDimension::Kind => {
                    for symbol in &prepared.latest.generation.symbols().symbols {
                        let Some(record) =
                            symbol_record_by_id(&prepared.latest, &symbol.occurrence)
                        else {
                            continue;
                        };
                        if path_is_in_code_query_scope(&record.file, &request.scope) {
                            *counts.entry(record.kind).or_default() += 1;
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
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_facets",
                request.dimension,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            let page = CodeQueryPage::new(
                prepared.latest.generation.manifest().generation_id.clone(),
                items,
                None,
                None,
                None,
            )
            .unwrap_or_else(|_| panic!("generation-owned facets create a valid page"));
            finish_direct_query(
                &prepared,
                &context,
                "code_facets",
                binding,
                page,
                &request.meta.page,
                eligible,
            )
        })
    }

    fn timeline<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeTimelineRequest,
    ) -> PortFuture<'a, CodeTimelineRecord> {
        Box::pin(async move {
            let prepared = prepare_callable_query_or_return!(self, context, request);
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
                    .generation
                    .symbols()
                    .symbols
                    .iter()
                    .filter_map(|symbol| symbol_record_by_id(&prepared.latest, &symbol.occurrence))
                    .filter(|symbol| path_is_in_code_query_scope(&symbol.file, &request.scope))
                    .count() as u64,
            }];
            let Ok(binding) = canonical_sha256(&(
                "code_timeline",
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            let page = CodeQueryPage::new(generation, items, None, None, None)
                .unwrap_or_else(|_| panic!("generation-owned timeline creates a valid page"));
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
            let prepared = prepare_callable_query_or_return!(self, context, request);
            let Ok(start) = typed::<SymbolOccurrenceId>(request.node_id.clone()) else {
                return unavailable(query_finished_at());
            };
            if symbol_record_by_id(&prepared.latest, &start).is_none() {
                return unavailable_for_generation(
                    query_finished_at(),
                    prepared.latest.generation.manifest().generation_id.clone(),
                );
            }
            let items = relation_records(
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
            let eligible = items.len() as u64;
            let Ok(binding) = canonical_sha256(&(
                "code_references",
                &request.node_id,
                &request.scope,
                &request.meta.projection,
                &request.meta.order,
            )) else {
                return unavailable(query_finished_at());
            };
            let page = CodeQueryPage::new(
                prepared.latest.generation.manifest().generation_id.clone(),
                items,
                None,
                None,
                None,
            )
            .unwrap_or_else(|_| panic!("generation-owned references create a valid page"));
            finish_direct_query(
                &prepared,
                &context,
                "code_references",
                binding,
                page,
                &request.meta.page,
                eligible,
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
        let prepared = prepare_callable_query_or_return!(registry, context, request);
        let Ok(start) = typed::<SymbolOccurrenceId>(request.node_id.clone()) else {
            return unavailable(query_finished_at());
        };
        if symbol_record_by_id(&prepared.latest, &start).is_none() {
            return unavailable_for_generation(
                query_finished_at(),
                prepared.latest.generation.manifest().generation_id.clone(),
            );
        }
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
            items.extend(
                relation_records(
                    &prepared.latest,
                    &start,
                    &[RelationEdgeKindV1::TypeOf],
                    false,
                    1,
                    &request.scope,
                )
                .into_iter()
                .map(|relation| relation.symbol),
            );
        }
        items.retain(|symbol| path_is_in_code_query_scope(&symbol.file, &request.scope));
        let eligible = items.len() as u64;
        let Ok(binding) = canonical_sha256(&(
            operation,
            &request.node_id,
            &request.scope,
            &request.meta.projection,
            &request.meta.order,
        )) else {
            return unavailable(query_finished_at());
        };
        finish_direct_query(
            &prepared,
            &context,
            operation,
            binding,
            symbol_page(&prepared.latest.generation.manifest().generation_id, items),
            &request.meta.page,
            eligible,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
