//! Callable application operations over the latest sealed PR9 generation.
//!
//! This is the production consumer of the exact, lexical, and graph owners.
//! It selects one already-mounted worktree generation and translates the
//! generic lane evidence into the typed application-operation records.

use std::future::Future;
use std::pin::Pin;

use tracedecay_application::retrieval::{
    SymbolPrimitiveRecord, SymbolRelationRecord, TypeHierarchyRecord,
};
use tracedecay_application::{
    CallableCodeQueryFuture, CallableCodeQueryPort, CodeHierarchyRequest, CodeImpactRequest,
    CodeImplementationsRequest, CodeOccurrenceRecord, CodeQueryPage, CodeRelationRequest,
    CodeSignatureRequest, CodeSymbolSearchRequest, CoverageCompleteness, CoverageDomainState,
    EvidenceCoverage, EvidenceDomain, ExactOccurrenceRecord, ExactOccurrenceRequest,
    LexicalOccurrenceRecord, ModuleApiRequest, OperationBudgetUsage, PageState,
    PhraseSearchRequest, QualifiedNameRequest, RetrievalEvidence, RetrievalPortContext,
    RetrievalPortOutcome, SourceMetadataRecord, SourceMetadataRequest, TemporalState,
};
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, ComponentRevision, ExactAdmissionRuleRevision,
    FreshnessVectorDigest, FusionProfileId, PrincipalId, QueryNormalizationRevision,
    RelationEdgeKindV1, RetrievalAnchorId, RetrievalBudget, RetrievalRequest, RetrievalScope,
    RetrievalSnapshot, RetrieverBatch, RetrieverOutcome, SanitizerRevision, ScoreDomainId,
    SingleRootScopeV1, SourceOccurrenceId, SymbolOccurrenceId, TemporalModeV1, VectorWatermark,
};
use tracedecay_tool_catalog::SortContractId;

use super::{CodeIndexSchedulerRegistryV1, LatestCompleteCodeIndexV1};
use crate::query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest,
    ExactLaneRetriever,
};
use crate::query::retrieval::graph::{GraphLaneEvidence, GraphLaneRequest, GraphLaneRetriever};
use crate::query::retrieval::lexical::{
    LexicalLaneEvidence, LexicalLaneRequest, LexicalLaneRetriever, MAX_FUZZY_TERM_EXPANSIONS_V1,
};
use crate::query::retrieval::ports::{CodeCandidateBindingV1, CodeOccurrenceRefV1};

const CALLABLE_CODE_SORT: &str = "sort.application.code-index.v1";

impl CodeIndexSchedulerRegistryV1 {
    pub(super) async fn generation_for(
        &self,
        generation_id: &CodeGenerationId,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let mounted = self.mounted.lock().await;
        for worktree in mounted.values() {
            let scheduler = worktree.scheduler.lock().ok()?;
            let latest = scheduler.latest_complete()?;
            if latest.generation.manifest().generation_id == *generation_id {
                return Some(latest);
            }
        }
        None
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

fn base_request(
    context: &RetrievalPortContext<'_>,
    latest: &LatestCompleteCodeIndexV1,
    temporal_mode: TemporalModeV1,
    page_size: u32,
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
        profile_id: FusionProfileId::new("profile.code-index.daemon.v1")
            .map_err(|error| error.to_string())?,
        budget: retrieval_budget(page_size),
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
            SortContractId::new(CALLABLE_CODE_SORT).expect("static sort id"),
            1,
            None,
            0,
        )
        .expect("empty application page"),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    })
}

fn completed<T>(
    page: CodeQueryPage<T>,
    coverage: tracedecay_domain::RetrieverCoverage,
    finished_at: tracedecay_domain::UtcMicros,
) -> RetrievalPortOutcome<CodeQueryPage<T>> {
    let returned = page.items.len() as u64;
    let mut temporal = TemporalState::current(finished_at);
    temporal.source_generation = Some(page.generation.clone());
    let evidence_coverage = EvidenceCoverage::complete(
        vec![EvidenceDomain::Symbol],
        coverage.examined,
        coverage.eligible,
        returned,
    )
    .unwrap_or(EvidenceCoverage {
        requested_domains: vec![EvidenceDomain::Symbol],
        visited: Some(coverage.examined),
        eligible: Some(coverage.eligible),
        returned,
        completeness: CoverageCompleteness::Partial,
        domains: vec![CoverageDomainState {
            domain: EvidenceDomain::Symbol,
            completeness: CoverageCompleteness::Partial,
        }],
    });
    RetrievalPortOutcome::Completed(RetrievalEvidence {
        page: PageState::first_page(
            SortContractId::new(CALLABLE_CODE_SORT).expect("static sort id"),
            1,
            page.total,
            returned,
        )
        .expect("bounded code query page"),
        payload: Some(page),
        temporal,
        evidence_authorities: Vec::new(),
        coverage: evidence_coverage,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    })
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

fn exact_page(
    latest: &LatestCompleteCodeIndexV1,
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
        items.push(ExactOccurrenceRecord {
            occurrence,
            matched_kind,
            matched_literal: request.literal.clone(),
        });
    }
    CodeQueryPage::new(
        request.scope.generation.clone(),
        items,
        Some(batch.coverage.eligible),
        None,
        None,
    )
    .expect("validated exact lane creates a valid page")
}

fn lexical_page(
    latest: &LatestCompleteCodeIndexV1,
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
        request.scope.generation.clone(),
        items,
        Some(batch.coverage.eligible),
        None,
        None,
    )
    .expect("validated lexical lane creates a valid page")
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

fn graph_page(
    latest: &LatestCompleteCodeIndexV1,
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
        request.scope.generation.clone(),
        items,
        Some(batch.coverage.eligible),
        None,
        None,
    )
    .expect("validated graph lane creates a valid page")
}

fn lane_result<T, E>(
    outcome: RetrieverOutcome<RetrieverBatch<E>>,
    finished_at: tracedecay_domain::UtcMicros,
    map: impl FnOnce(RetrieverBatch<E>) -> CodeQueryPage<T>,
) -> RetrievalPortOutcome<CodeQueryPage<T>> {
    match outcome {
        RetrieverOutcome::Complete(batch) => {
            let coverage = batch.coverage;
            completed(map(batch), coverage, finished_at)
        }
        RetrieverOutcome::Partial { value, .. } => {
            let coverage = value.coverage;
            completed(map(value), coverage, finished_at)
        }
        _ => unavailable(finished_at),
    }
}

type PortFuture<'a, T> =
    Pin<Box<dyn Future<Output = RetrievalPortOutcome<CodeQueryPage<T>>> + Send + 'a>>;

macro_rules! unavailable_method {
    ($name:ident, $request:ty, $item:ty) => {
        fn $name<'a>(
            &'a self,
            _context: RetrievalPortContext<'a>,
            _request: &'a $request,
        ) -> PortFuture<'a, $item> {
            Box::pin(async move { unavailable(tracedecay_domain::UtcMicros(0)) })
        }
    };
}

impl CallableCodeQueryPort for CodeIndexSchedulerRegistryV1 {
    fn exact_occurrence<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a ExactOccurrenceRequest,
    ) -> CallableCodeQueryFuture<'a, ExactOccurrenceRecord> {
        Box::pin(async move {
            let Some(latest) = self.generation_for(&request.scope.generation).await else {
                return unavailable(tracedecay_domain::UtcMicros(0));
            };
            let finished_at = latest.generation.manifest().seal.sealed_at;
            let Ok(base) = base_request(
                &context,
                &latest,
                request.meta.temporal,
                request.meta.page.page_size,
            ) else {
                return unavailable(finished_at);
            };
            let Ok(query_view) = tracedecay_domain::EphemeralSanitizedQueryViewV1::sanitize(
                request.literal.clone(),
                SanitizerRevision::new("query-sanitizer.daemon.v1")
                    .expect("static sanitizer revision"),
                QueryNormalizationRevision::new("query-normalization.daemon.v1")
                    .expect("static normalization revision"),
            ) else {
                return unavailable(finished_at);
            };
            let authority = CentralExactAdmissionAuthorityV1::new(
                ExactAdmissionRuleRevision::new("exact-rules.daemon.v1")
                    .expect("static exact rule revision"),
            );
            let lane_request = ExactLaneRequest {
                literals: authority.parse_literals(&query_view, &base),
                generation: request.scope.generation.clone(),
                budget: base.budget,
                base,
                query_view: &query_view,
            };
            let Ok(owners) = latest.production_query_owners() else {
                return unavailable(finished_at);
            };
            match owners.exact.retrieve_exact(&lane_request) {
                Ok(outcome) => lane_result(outcome, finished_at, |batch| {
                    exact_page(&latest, request, batch)
                }),
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
            let Some(latest) = self.generation_for(&request.scope.generation).await else {
                return unavailable(tracedecay_domain::UtcMicros(0));
            };
            let finished_at = latest.generation.manifest().seal.sealed_at;
            let Ok(base) = base_request(
                &context,
                &latest,
                request.meta.temporal,
                request.meta.page.page_size,
            ) else {
                return unavailable(finished_at);
            };
            let whole_terms = request
                .query
                .as_str()
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let lane_request = LexicalLaneRequest {
                query_view: &request.query,
                generation: request.scope.generation.clone(),
                whole_terms: whole_terms.clone(),
                subtokens: whole_terms
                    .iter()
                    .map(|term| term.to_ascii_lowercase())
                    .collect(),
                phrases: request.phrases.clone(),
                field_filters: Vec::new(),
                fuzzy_budget: MAX_FUZZY_TERM_EXPANSIONS_V1,
                lexical_profile_revision: ComponentRevision::new("lexical-profile.daemon.v1")
                    .expect("static lexical profile"),
                score_domain: ScoreDomainId::new("score.lexical.daemon.v1")
                    .expect("static lexical score domain"),
                budget: base.budget,
                base,
            };
            let Ok(owners) = latest.production_query_owners() else {
                return unavailable(finished_at);
            };
            match owners.lexical.retrieve_lexical(&lane_request) {
                Ok(outcome) => lane_result(outcome, finished_at, |batch| {
                    lexical_page(&latest, request, batch)
                }),
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
            let Some(latest) = self.generation_for(&request.scope.generation).await else {
                return unavailable(tracedecay_domain::UtcMicros(0));
            };
            let finished_at = latest.generation.manifest().seal.sealed_at;
            let Ok(base) = base_request(
                &context,
                &latest,
                request.meta.temporal,
                request.meta.page.page_size,
            ) else {
                return unavailable(finished_at);
            };
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
                    .expect("validated symbol creates source occurrence");
            let seed = CodeCandidateBindingV1 {
                candidate_anchor: RetrievalAnchorId::new(format!(
                    "code-symbol:{}",
                    symbol.as_str()
                ))
                .expect("validated symbol creates anchor"),
                occurrence: CodeOccurrenceRefV1 {
                    generation: request.scope.generation.clone(),
                    file: chunk.anchor.file_occurrence_id.clone(),
                    symbol: Some(symbol),
                    chunk: Some(chunk.id.clone()),
                },
                language_descriptor_revision: chunk.language_descriptor_revision.clone(),
                matched_term_kinds: Vec::new(),
                source_occurrence,
            };
            let lane_request = GraphLaneRequest {
                generation: request.scope.generation.clone(),
                seed_anchors: vec![seed],
                edge_kinds: vec![RelationEdgeKindV1::Calls],
                max_depth: request.maximum_depth,
                budget: base.budget,
                base,
            };
            let Ok(owners) = latest.production_query_owners() else {
                return unavailable(finished_at);
            };
            match owners.graph.retrieve_graph(&lane_request) {
                Ok(outcome) => lane_result(outcome, finished_at, |batch| {
                    graph_page(&latest, request, batch)
                }),
                Err(_) => unavailable(finished_at),
            }
        })
    }

    unavailable_method!(
        symbol_search,
        CodeSymbolSearchRequest,
        SymbolPrimitiveRecord
    );
    unavailable_method!(qualified_name, QualifiedNameRequest, SymbolPrimitiveRecord);
    unavailable_method!(
        signature_search,
        CodeSignatureRequest,
        SymbolPrimitiveRecord
    );
    unavailable_method!(
        implementations,
        CodeImplementationsRequest,
        SymbolRelationRecord
    );
    unavailable_method!(type_hierarchy, CodeHierarchyRequest, TypeHierarchyRecord);
    unavailable_method!(callers, CodeRelationRequest, SymbolRelationRecord);
    unavailable_method!(impact, CodeImpactRequest, SymbolPrimitiveRecord);
    unavailable_method!(module_api, ModuleApiRequest, SymbolPrimitiveRecord);
    unavailable_method!(source_metadata, SourceMetadataRequest, SourceMetadataRecord);
}
