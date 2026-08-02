//! Code-index MCP search executor with its hydration and display helpers.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic,
//! signatures, or behavior changed. `use super::*` re-exposes every name the
//! parent `daemon` module had in scope so the moved code resolves unchanged.

use std::collections::HashMap;
use std::sync::Arc;

use tracedecay_query::code_search;

use super::{code_index_scheduler, project_open_owners, query_mcp_admission};

const MAX_CONCURRENT_CODE_INDEX_SEARCHES: usize = 1;

struct McpSemanticExecutionControlV1 {
    started: std::time::Instant,
    admission_provider: query_mcp_admission::QueryMcpReadAdmissionProviderV1,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
}

impl McpSemanticExecutionControlV1 {
    fn request_termination(&self) -> Option<code_search::CodeIndexSearchUnavailableReasonV1> {
        mcp_search_request_termination(
            self.deadline.as_ref(),
            self.cancellation.as_ref(),
            tracedecay_application::clock::now_micros().0,
        )
    }
}

pub(super) fn mcp_search_request_termination(
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
    now_micros: i64,
) -> Option<code_search::CodeIndexSearchUnavailableReasonV1> {
    if cancellation.is_some_and(tracedecay_application::CancellationSignal::is_cancelled) {
        return Some(code_search::CodeIndexSearchUnavailableReasonV1::Cancelled);
    }
    deadline
        .is_some_and(|deadline| now_micros >= deadline.expires_at.0)
        .then_some(code_search::CodeIndexSearchUnavailableReasonV1::TimedOut)
}

/// Builds the `Unavailable` search outcome, optionally naming the code
/// generation the caller had already resolved.
///
/// Reaching here means no lane could serve: the coverage marker says every
/// lane is down for `semantic_reason`, so the transport reports a typed
/// failure immediately instead of waiting on an in-progress rebuild. A
/// request that still had one ready lane never lands here.
fn code_index_search_unavailable_for_generation(
    code_generation: Option<String>,
    reason: code_search::CodeIndexSearchUnavailableReasonV1,
    semantic_reason: &'static str,
) -> code_search::CodeIndexSearchOutcomeV1 {
    code_search::CodeIndexSearchOutcomeV1::Unavailable(code_search::CodeIndexSearchUnavailableV1 {
        code_generation,
        reason,
        semantic: code_search::CodeIndexSemanticStatusV1::Unavailable {
            reason: semantic_reason,
        },
        coverage: code_search::CodeIndexSearchCoverageV1::unavailable(semantic_reason),
    })
}

/// [`code_index_search_unavailable_for_generation`] for the paths that fail
/// before any generation is known.
fn code_index_search_unavailable(
    reason: code_search::CodeIndexSearchUnavailableReasonV1,
    semantic_reason: &'static str,
) -> code_search::CodeIndexSearchOutcomeV1 {
    code_index_search_unavailable_for_generation(None, reason, semantic_reason)
}

/// The authority check repeated at every point a search may still be abandoned:
/// the request itself may have been cancelled or timed out, or the MCP read
/// route may have been revoked underneath it. `Some` means stop and return.
fn search_terminated(
    control: &McpSemanticExecutionControlV1,
    admission_provider: &query_mcp_admission::QueryMcpReadAdmissionProviderV1,
    code_generation: Option<&str>,
) -> Option<code_search::CodeIndexSearchOutcomeV1> {
    if let Some(reason) = control.request_termination() {
        return Some(code_index_search_unavailable_for_generation(
            code_generation.map(str::to_owned),
            reason,
            reason.as_str(),
        ));
    }
    (!admission_provider.route_is_registered()).then(|| {
        code_index_search_unavailable_for_generation(
            code_generation.map(str::to_owned),
            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
            "route_revoked",
        )
    })
}

pub(super) fn code_index_scope_unavailable() -> code_search::CodeIndexSearchOutcomeV1 {
    code_index_search_unavailable(
        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
        "scope_unavailable",
    )
}

pub(super) fn code_index_search_hydration_budget(
    accepted_semantic_budget: Option<&tracedecay_domain::RetrievalBudget>,
    query_budget: &tracedecay_domain::RetrievalBudget,
) -> tracedecay_domain::RetrievalBudget {
    accepted_semantic_budget.copied().unwrap_or(*query_budget)
}

struct CodeIndexSearchHydrationSourceV1<A, P, H> {
    authorize: A,
    preflight: P,
    hydrate: H,
}

impl<A, P, H> CodeIndexSearchHydrationSourceV1<A, P, H> {
    fn new(authorize: A, preflight: P, hydrate: H) -> Self {
        Self {
            authorize,
            preflight,
            hydrate,
        }
    }
}

impl<A, P, H>
    tracedecay_query::retrieval::hydrate::LateHydrationSource<code_search::CodeIndexSearchDisplayV1>
    for CodeIndexSearchHydrationSourceV1<A, P, H>
where
    A: FnMut(
        &tracedecay_domain::RetrievalRequest,
        &tracedecay_domain::RankedCandidate,
    ) -> tracedecay_query::retrieval::hydrate::HydrationAuthorizationV1,
    P: FnMut(
        &tracedecay_domain::RetrievalRequest,
        &tracedecay_domain::RankedCandidate,
        &tracedecay_query::retrieval::hydrate::HydrationWorkPermitV1,
    ) -> tracedecay_query::retrieval::hydrate::HydrationPreflightOutcomeV1,
    H: FnMut(
        &tracedecay_domain::RetrievalRequest,
        &tracedecay_domain::RankedCandidate,
        &tracedecay_query::retrieval::hydrate::HydrationWorkPermitV1,
    ) -> tracedecay_query::retrieval::hydrate::HydrationReadOutcomeV1<
        code_search::CodeIndexSearchDisplayV1,
    >,
{
    fn authorize(
        &mut self,
        request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
    ) -> tracedecay_query::retrieval::hydrate::HydrationAuthorizationV1 {
        (self.authorize)(request, candidate)
    }

    fn preflight_authorized(
        &mut self,
        request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        permit: &tracedecay_query::retrieval::hydrate::HydrationWorkPermitV1,
    ) -> tracedecay_query::retrieval::hydrate::HydrationPreflightOutcomeV1 {
        use tracedecay_query::retrieval::hydrate::{
            HydrationAuthorizationV1, HydrationPreflightOutcomeV1, HydrationUnavailableV1,
        };

        match (self.authorize)(request, candidate) {
            HydrationAuthorizationV1::Authorized => (self.preflight)(request, candidate, permit),
            HydrationAuthorizationV1::Denied => HydrationPreflightOutcomeV1::Unavailable(
                HydrationUnavailableV1::AuthorityUnavailable,
            ),
            HydrationAuthorizationV1::Unavailable(reason) => {
                HydrationPreflightOutcomeV1::Unavailable(reason)
            }
        }
    }

    fn hydrate_authorized(
        &mut self,
        request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        permit: &tracedecay_query::retrieval::hydrate::HydrationWorkPermitV1,
    ) -> tracedecay_query::retrieval::hydrate::HydrationReadOutcomeV1<
        code_search::CodeIndexSearchDisplayV1,
    > {
        use tracedecay_query::retrieval::hydrate::{
            HydrationAuthorizationV1, HydrationReadOutcomeV1, HydrationUnavailableV1,
        };

        match (self.authorize)(request, candidate) {
            HydrationAuthorizationV1::Authorized => (self.hydrate)(request, candidate, permit),
            HydrationAuthorizationV1::Denied => {
                HydrationReadOutcomeV1::Unavailable(HydrationUnavailableV1::AuthorityUnavailable)
            }
            HydrationAuthorizationV1::Unavailable(reason) => {
                HydrationReadOutcomeV1::Unavailable(reason)
            }
        }
    }
}

pub(super) fn code_index_search_display_binding(
    generation: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    request: &tracedecay_domain::RetrievalRequest,
    candidate: &tracedecay_domain::RankedCandidate,
) -> std::result::Result<
    (
        code_search::CodeIndexSearchDisplayV1,
        tracedecay_domain::OccurrenceProvenance,
    ),
    tracedecay_query::retrieval::hydrate::HydrationUnavailableV1,
> {
    use tracedecay_query::retrieval::hydrate::HydrationUnavailableV1;

    if generation.symbols().generation_id != generation.manifest().generation_id
        || request.scope.privacy_domain != generation.manifest().privacy_domain
        || request.scope.root.repository != generation.snapshot().repository
        || request.scope.root.worktree != generation.snapshot().worktree
        || request.scope.root.reference != generation.snapshot().reference
        || request.snapshot.freshness_digest.as_str()
            != generation.manifest().snapshot_digest.as_str()
        || request.snapshot.captured_at != generation.manifest().seal.sealed_at
    {
        return Err(HydrationUnavailableV1::Stale);
    }
    let anchor = candidate.candidate.anchor_id.as_str();
    let (display, expected_source_occurrence) =
        if let Some(occurrence) = anchor.strip_prefix("code-symbol:") {
            let symbol = generation
                .symbols()
                .symbols
                .iter()
                .find(|symbol| symbol.occurrence.as_str() == occurrence)
                .ok_or(HydrationUnavailableV1::Invalid)?;
            (code_index_symbol_display(symbol), None)
        } else if let Some(chunk_id) = anchor.strip_prefix("code-chunk:") {
            let chunk_id = tracedecay_domain::CodeSearchChunkId::new(chunk_id.to_owned())
                .map_err(|_| HydrationUnavailableV1::Invalid)?;
            let chunk = generation
                .chunks()
                .chunk(&chunk_id)
                .ok_or(HydrationUnavailableV1::Invalid)?;
            if chunk.anchor.generation_id != generation.manifest().generation_id {
                return Err(HydrationUnavailableV1::Stale);
            }
            let display = match chunk.anchor.symbol_occurrence_id.as_ref() {
                Some(occurrence) => {
                    let symbol = generation
                        .symbols()
                        .symbols
                        .iter()
                        .find(|symbol| symbol.occurrence == *occurrence)
                        .ok_or(HydrationUnavailableV1::Invalid)?;
                    code_index_symbol_display(symbol)
                }
                None => {
                    let file = generation
                        .snapshot()
                        .files
                        .iter()
                        .find(|file| {
                            file.file_occurrence_id == chunk.anchor.file_occurrence_id
                                && file.disposition
                                    == tracedecay_domain::SnapshotFileDispositionV1::Present
                        })
                        .ok_or(HydrationUnavailableV1::Invalid)?;
                    code_search::CodeIndexSearchDisplayV1 {
                        name: file
                            .logical_path
                            .rsplit('/')
                            .next()
                            .unwrap_or(file.logical_path.as_str())
                            .to_owned(),
                        qualified_name: file.logical_path.clone(),
                        kind: "file".to_owned(),
                    }
                }
            };
            (display, Some(format!("code-chunk:{}", chunk_id.as_str())))
        } else {
            return Err(HydrationUnavailableV1::Invalid);
        };
    let provenance = candidate
        .candidate
        .occurrences
        .iter()
        .find(|provenance| {
            provenance.repository_id.as_ref() == Some(&request.scope.root.repository)
                && provenance.source_namespace == provenance.freshness.source_namespace
                && provenance.freshness.compatibility
                    == tracedecay_domain::FreshnessCompatibilityV1::Current
                && provenance.source_namespace.as_str() == "ns.code.daemon"
                && expected_source_occurrence
                    .as_ref()
                    .is_none_or(|expected| provenance.source_occurrence_id.as_str() == expected)
        })
        .cloned()
        .ok_or(HydrationUnavailableV1::Invalid)?;
    Ok((display, provenance))
}

fn code_index_symbol_display(
    symbol: &crate::code_index::lineage::LineageSymbolRecordV1,
) -> code_search::CodeIndexSearchDisplayV1 {
    code_search::CodeIndexSearchDisplayV1 {
        name: symbol
            .qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(symbol.qualified_name.as_str())
            .to_owned(),
        qualified_name: symbol.qualified_name.clone(),
        kind: symbol.kind.clone(),
    }
}

fn code_index_search_display_bytes(
    display: &code_search::CodeIndexSearchDisplayV1,
) -> std::result::Result<u64, tracedecay_query::retrieval::hydrate::HydrationUnavailableV1> {
    serde_json::to_vec(&(
        display.name.as_str(),
        display.qualified_name.as_str(),
        display.kind.as_str(),
    ))
    .ok()
    .and_then(|bytes| u64::try_from(bytes.len()).ok())
    .ok_or(tracedecay_query::retrieval::hydrate::HydrationUnavailableV1::Internal)
}

impl tracedecay_query::retrieval::semantic::SemanticExecutionControl
    for McpSemanticExecutionControlV1
{
    fn is_cancelled(&self) -> bool {
        !self.admission_provider.route_is_registered() || self.request_termination().is_some()
    }

    fn elapsed_micros(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

impl tracedecay_query::retrieval::hydrate::HydrationExecutionControlV1
    for McpSemanticExecutionControlV1
{
    fn elapsed_micros(&self) -> u64 {
        tracedecay_query::retrieval::semantic::SemanticExecutionControl::elapsed_micros(self)
    }

    fn is_cancelled(&self) -> bool {
        !self.admission_provider.route_is_registered() || self.request_termination().is_some()
    }
}

pub(super) fn code_index_search_executor(
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_id: tracedecay_domain::ProjectId,
    admission_provider: query_mcp_admission::QueryMcpReadAdmissionProviderV1,
) -> code_search::CodeIndexSearchExecutor {
    let execution_admission = Arc::new(tokio::sync::Semaphore::new(
        MAX_CONCURRENT_CODE_INDEX_SEARCHES,
    ));
    Arc::new(move |request| {
        let schedulers = schedulers.clone();
        let project_id = project_id.clone();
        let admission_provider = admission_provider.clone();
        let execution_admission = Arc::clone(&execution_admission);
        Box::pin(async move {
            let scope = match project_open_owners::resolved_scope_for_project(
                &request.project_root,
                &project_id,
            ) {
                Ok(scope) => scope,
                Err(_) => return code_index_scope_unavailable(),
            };
            let admission = match admission_provider.admit_current(&scope) {
                Ok(admission) => admission,
                Err(error) => {
                    return code_index_search_unavailable(
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        error.reason(),
                    );
                }
            };
            let current_authority = admission.search_authority();
            let authority = match admission.authorize(&scope, Some(&current_authority)) {
                Ok(authority) => authority,
                Err(error) => {
                    return code_index_search_unavailable(
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        error.reason(),
                    );
                }
            };
            let terminal_expected_authority = authority.clone();
            let policy = match (
                tracedecay_domain::SanitizerRevision::new(
                    tracedecay_query::retrieval::QUERY_SANITIZER_REVISION_V1,
                ),
                tracedecay_domain::QueryNormalizationRevision::new(
                    tracedecay_query::retrieval::QUERY_NORMALIZATION_REVISION_V1,
                ),
                tracedecay_domain::ExactAdmissionRuleRevision::new(
                    tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
                ),
                tracedecay_domain::ComponentRevision::new(
                    tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
                ),
                tracedecay_domain::ScoreDomainId::new(
                    tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
                ),
            ) {
                (
                    Ok(sanitizer_revision),
                    Ok(normalization_revision),
                    Ok(exact_rule_revision),
                    Ok(lexical_profile_revision),
                    Ok(lexical_score_domain),
                ) => code_index_scheduler::query_runtime::QuerySearchExecutionPolicyV1 {
                    principal: authority.principal,
                    authorization_revision: authority.authorization_revision,
                    sanitizer_revision,
                    normalization_revision,
                    exact_rule_revision,
                    lexical_profile_revision,
                    lexical_score_domain,
                    fuzzy_budget:
                        tracedecay_query::retrieval::lexical::MAX_FUZZY_TERM_EXPANSIONS_V1,
                    graph_edge_kinds: vec![tracedecay_domain::RelationEdgeKindV1::Calls],
                    graph_max_depth: 1,
                    page_size: request.limit,
                    cursor: request.cursor,
                },
                _ => {
                    return code_index_search_unavailable(
                        code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest,
                        "invalid_request",
                    );
                }
            };
            let project_root = request.project_root;
            let mode = request.mode;
            let deadline = request.deadline;
            let cancellation = request.cancellation;
            let semantic_mode = match mode {
                code_search::CodeIndexSearchModeV1::FallbackAllowed => {
                    tracedecay_query::retrieval::semantic::SemanticQueryModeV1::FallbackAllowed
                }
                code_search::CodeIndexSearchModeV1::StrictSemantic => {
                    tracedecay_query::retrieval::semantic::SemanticQueryModeV1::StrictSemantic
                }
            };
            let control = Arc::new(McpSemanticExecutionControlV1 {
                started: std::time::Instant::now(),
                admission_provider: admission_provider.clone(),
                deadline,
                cancellation,
            });
            if let Some(outcome) = search_terminated(&control, &admission_provider, None) {
                return outcome;
            }
            let execution_permit = match execution_admission.try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    return code_index_search_unavailable(
                        code_search::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable,
                        "search_capacity_unavailable",
                    );
                }
            };
            let execution_result = {
                let execution_schedulers = schedulers.clone();
                let execution_project_root = project_root.clone();
                let execution_scope = scope.clone();
                let execution_control = Arc::clone(&control);
                let execution_request =
                    code_index_scheduler::query_runtime::QuerySearchExecutionRequestV1::new(
                        request.query,
                        policy,
                    );
                let runtime = tokio::runtime::Handle::current();
                let mut execution = tokio::task::spawn_blocking(move || {
                    let _execution_permit = execution_permit;
                    runtime.block_on(async move {
                        execution_schedulers
                            .execute_query_with_semantic(
                                &execution_project_root,
                                &execution_scope,
                                execution_request,
                                execution_control.as_ref(),
                                semantic_mode,
                            )
                            .await
                    })
                });
                let mut control_poll = tokio::time::interval(std::time::Duration::from_millis(10));
                control_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        result = &mut execution => match result {
                            Ok(result) => break result,
                            Err(_) => return code_index_search_unavailable(
                                code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                                "search_task_failed",
                            ),
                        },
                        _ = control_poll.tick() => {
                            if let Some(outcome) =
                                search_terminated(&control, &admission_provider, None)
                            {
                                execution.abort();
                                return outcome;
                            }
                        }
                    }
                }
            };
            if let Some(outcome) = search_terminated(&control, &admission_provider, None) {
                return outcome;
            }
            let executed = match execution_result {
                Ok(executed) => executed,
                Err(error) => {
                    use code_index_scheduler::query_runtime::QuerySearchExecutionErrorV1;
                    use code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1;
                    tracing::warn!(
                        project_id = %project_id.as_str(),
                        error = %error,
                        "code_index_search_failed"
                    );
                    if let QuerySemanticSearchExecutionErrorV1::StrictSemanticUnavailable {
                        generation,
                        abstention,
                    } = &error
                    {
                        return code_index_search_unavailable_for_generation(
                            Some(generation.as_str().to_owned()),
                            code_search::CodeIndexSearchUnavailableReasonV1::SemanticUnavailable,
                            code_index_scheduler::semantic_query_runtime::semantic_abstention_reason(
                                abstention,
                            ),
                        );
                    }
                    // The lane reason travels with the failure so a caller can
                    // tell "this scope has no index" from "the index this
                    // scope already had is being rebuilt". Only the latter is
                    // worth retrying, and only the latter leaves the retained
                    // lexical lane able to answer.
                    let (reason, lane_reason) = match error {
                        QuerySemanticSearchExecutionErrorV1::Query(error) => match error {
                        QuerySearchExecutionErrorV1::AuthorityUnavailable
                        | QuerySearchExecutionErrorV1::Authority(
                            tracedecay_query::retrieval::QueryAuthorityErrorV1::AuthorityUnavailable,
                        ) => (
                            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable
                                .as_str(),
                        ),
                        QuerySearchExecutionErrorV1::GenerationUnavailable => (
                            code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
                            code_search::lane_reason::GENERATION_REBUILDING,
                        ),
                        QuerySearchExecutionErrorV1::InvalidScope(_)
                        | QuerySearchExecutionErrorV1::InvalidPolicy(_) => (
                            code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest,
                            code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest.as_str(),
                        ),
                        QuerySearchExecutionErrorV1::Retrieval(_)
                        | QuerySearchExecutionErrorV1::Authority(_) => (
                            code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                            code_search::CodeIndexSearchUnavailableReasonV1::Internal.as_str(),
                        ),
                        },
                        QuerySemanticSearchExecutionErrorV1::Semantic(_) => (
                            code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                            "semantic_unavailable",
                        ),
                        QuerySemanticSearchExecutionErrorV1::StrictSemanticUnavailable { .. } => (
                            code_search::CodeIndexSearchUnavailableReasonV1::SemanticUnavailable,
                            "semantic_unavailable",
                        ),
                    };
                    return code_index_search_unavailable(reason, lane_reason);
                }
            };
            if let Some(outcome) = search_terminated(
                &control,
                &admission_provider,
                Some(executed.query.generation.as_str()),
            ) {
                return outcome;
            }
            let terminal_scope =
                match project_open_owners::resolved_scope_for_project(&project_root, &project_id) {
                    Ok(terminal_scope) if terminal_scope == scope => terminal_scope,
                    _ => {
                        return code_index_search_unavailable_for_generation(
                            Some(executed.query.generation.as_str().to_owned()),
                            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            "scope_changed_before_publication",
                        );
                    }
                };
            let terminal_admission = match admission_provider.admit_current(&terminal_scope) {
                Ok(admission) => admission,
                Err(error) => {
                    return code_index_search_unavailable_for_generation(
                        Some(executed.query.generation.as_str().to_owned()),
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        error.reason(),
                    );
                }
            };
            let terminal_authority = terminal_admission.search_authority();
            if terminal_authority != terminal_expected_authority
                || terminal_admission
                    .authorize(&terminal_scope, Some(&terminal_authority))
                    .is_err()
            {
                return code_index_search_unavailable_for_generation(
                    Some(executed.query.generation.as_str().to_owned()),
                    code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    "authorization_changed_before_publication",
                );
            }
            let (semantic, ordered_candidates, next_cursor, accepted_semantic_budget) =
                match &executed.semantic {
                code_index_scheduler::semantic_query_runtime::SemanticAugmentationOutcomeV1::Augmented(
                    augmented,
                ) => (
                    code_search::CodeIndexSemanticStatusV1::Complete,
                    augmented.composition.ranked_candidates.clone(),
                    augmented.cursor.clone(),
                    Some(&augmented.hydration_budget),
                ),
                code_index_scheduler::semantic_query_runtime::SemanticAugmentationOutcomeV1::Fallback {
                    abstention,
                    fallback,
                } => (
                    code_search::CodeIndexSemanticStatusV1::Unavailable {
                        reason: code_index_scheduler::semantic_query_runtime::semantic_abstention_reason(
                            abstention,
                        ),
                    },
                    executed.query.authorized.fallback.ordered_candidates.clone(),
                    fallback.cursor.clone(),
                    None,
                ),
            };
            let Some(latest) = schedulers
                .generation_for(&terminal_scope, &executed.query.generation)
                .await
            else {
                return code_index_search_unavailable_for_generation(
                    Some(executed.query.generation.as_str().to_owned()),
                    code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
                    "generation_changed_before_hydration",
                );
            };
            let mut hydration_request = executed.query.sanitized.request().clone();
            let hydration_budget = code_index_search_hydration_budget(
                accepted_semantic_budget,
                &hydration_request.budget,
            );
            hydration_request.budget = hydration_budget;
            let authorize =
                |request: &tracedecay_domain::RetrievalRequest,
                 _candidate: &tracedecay_domain::RankedCandidate| {
                    use tracedecay_query::retrieval::hydrate::HydrationAuthorizationV1;

                    let Ok(current_scope) =
                        project_open_owners::resolved_scope_for_project(&project_root, &project_id)
                    else {
                        return HydrationAuthorizationV1::Denied;
                    };
                    if current_scope != terminal_scope
                        || request.principal != terminal_expected_authority.principal
                        || request.snapshot.authorization_revision
                            != terminal_expected_authority.authorization_revision
                    {
                        return HydrationAuthorizationV1::Denied;
                    }
                    let Ok(current_admission) = admission_provider.admit_current(&current_scope)
                    else {
                        return HydrationAuthorizationV1::Denied;
                    };
                    let current_authority = current_admission.search_authority();
                    if current_authority != terminal_expected_authority
                        || current_admission
                            .authorize(&current_scope, Some(&current_authority))
                            .is_err()
                    {
                        HydrationAuthorizationV1::Denied
                    } else {
                        HydrationAuthorizationV1::Authorized
                    }
                };
            let preflight =
                |request: &tracedecay_domain::RetrievalRequest,
                 candidate: &tracedecay_domain::RankedCandidate,
                 _permit: &tracedecay_query::retrieval::hydrate::HydrationWorkPermitV1| {
                    use tracedecay_query::retrieval::hydrate::HydrationPreflightOutcomeV1;

                    match code_index_search_display_binding(
                        latest.generation(),
                        request,
                        candidate,
                    )
                    .and_then(|(display, _)| code_index_search_display_bytes(&display))
                    {
                        Ok(estimated_bytes) => {
                            HydrationPreflightOutcomeV1::Ready { estimated_bytes }
                        }
                        Err(reason) => HydrationPreflightOutcomeV1::Unavailable(reason),
                    }
                };
            let hydrate =
                |request: &tracedecay_domain::RetrievalRequest,
                 candidate: &tracedecay_domain::RankedCandidate,
                 _permit: &tracedecay_query::retrieval::hydrate::HydrationWorkPermitV1| {
                    use tracedecay_query::retrieval::hydrate::HydrationReadOutcomeV1;

                    let (display, provenance) = match code_index_search_display_binding(
                        latest.generation(),
                        request,
                        candidate,
                    ) {
                        Ok(binding) => binding,
                        Err(reason) => return HydrationReadOutcomeV1::Unavailable(reason),
                    };
                    let bytes_hydrated = match code_index_search_display_bytes(&display) {
                        Ok(bytes) => bytes,
                        Err(reason) => return HydrationReadOutcomeV1::Unavailable(reason),
                    };
                    let hydration_revision = match tracedecay_domain::HydrationRevision::new(
                        "hydration.code-index.display.v1",
                    ) {
                        Ok(revision) => revision,
                        Err(_) => {
                            return HydrationReadOutcomeV1::Unavailable(
                                tracedecay_query::retrieval::hydrate::HydrationUnavailableV1::Internal,
                            );
                        }
                    };
                    HydrationReadOutcomeV1::Complete {
                        payload: display,
                        receipt: tracedecay_domain::HydrationReceipt {
                            anchor_id: candidate.candidate.anchor_id.clone(),
                            source_occurrence_id: provenance.source_occurrence_id,
                            hydration_revision,
                            bytes_hydrated,
                            authorized: true,
                            freshness: provenance.freshness,
                        },
                    }
                };
            let mut source = CodeIndexSearchHydrationSourceV1::new(authorize, preflight, hydrate);
            let hydrated = match tracedecay_query::retrieval::hydrate::CanonicalLateHydration::new(
                &mut source,
            )
            .hydrate_with_control(
                &hydration_request,
                ordered_candidates.as_slice(),
                &hydration_budget,
                control.as_ref(),
            ) {
                Ok(hydrated) => hydrated,
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id.as_str(),
                        error = %error,
                        "code_index_search_hydration_failed"
                    );
                    return code_index_search_unavailable_for_generation(
                        Some(executed.query.generation.as_str().to_owned()),
                        code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                        "late_hydration_failed",
                    );
                }
            };
            let hydrated_prefix_len = hydrated.results.len();
            let mut display_by_anchor = HashMap::new();
            let mut hydrated_candidates = Vec::with_capacity(ordered_candidates.len());
            for result in hydrated.results {
                use tracedecay_query::retrieval::hydrate::{
                    HydrationOutcomeV1, HydrationUnavailableV1,
                };

                match result.outcome {
                    HydrationOutcomeV1::Complete(display)
                    | HydrationOutcomeV1::Partial {
                        payload: display, ..
                    } => {
                        display_by_anchor
                            .insert(result.ranked.candidate.anchor_id.clone(), display);
                        hydrated_candidates.push(result.ranked);
                    }
                    HydrationOutcomeV1::Unavailable(
                        HydrationUnavailableV1::AuthorityUnavailable,
                    ) => {}
                    HydrationOutcomeV1::Unavailable(_) => {
                        hydrated_candidates.push(result.ranked);
                    }
                }
            }
            hydrated_candidates.extend(ordered_candidates.into_iter().skip(hydrated_prefix_len));
            let ordered_candidates = hydrated_candidates;
            if let Some(reason) = control.request_termination() {
                return code_index_search_unavailable_for_generation(
                    Some(executed.query.generation.as_str().to_owned()),
                    reason,
                    reason.as_str(),
                );
            }
            // Not `search_terminated`: this guard reports a distinct revocation
            // reason because the search already produced results by this point.
            if !admission_provider.route_is_registered() {
                return code_index_search_unavailable_for_generation(
                    Some(executed.query.generation.as_str().to_owned()),
                    code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    "route_revoked_before_publication",
                );
            }
            let publication_scope =
                match project_open_owners::resolved_scope_for_project(&project_root, &project_id) {
                    Ok(publication_scope) if publication_scope == terminal_scope => {
                        publication_scope
                    }
                    _ => {
                        return code_index_search_unavailable_for_generation(
                            Some(executed.query.generation.as_str().to_owned()),
                            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            "scope_changed_during_publication",
                        );
                    }
                };
            let publication_admission = match admission_provider.admit_current(&publication_scope) {
                Ok(admission) => admission,
                Err(error) => {
                    return code_index_search_unavailable_for_generation(
                        Some(executed.query.generation.as_str().to_owned()),
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        error.reason(),
                    );
                }
            };
            let publication_authority = publication_admission.search_authority();
            if publication_authority != terminal_expected_authority
                || publication_admission
                    .authorize(&publication_scope, Some(&publication_authority))
                    .is_err()
            {
                return code_index_search_unavailable_for_generation(
                    Some(executed.query.generation.as_str().to_owned()),
                    code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    "authorization_changed_during_publication",
                );
            }
            // Additive only: the generation-bound lanes all ran against the
            // admitted generation here, so warm coverage restates what the
            // existing candidates already mean. Ranking identity, fallback
            // bytes, and the cursor are untouched. When query admission had to
            // fall back to the last complete generation because no current one
            // was admissible, the same lanes are reported stale against the
            // generation that actually answered.
            let coverage = if executed.query.served_stale {
                code_search::CodeIndexSearchCoverageV1::fused_stale(
                    executed.query.generation.as_str(),
                    &semantic,
                )
            } else {
                code_search::CodeIndexSearchCoverageV1::fused(&semantic)
            };
            code_search::CodeIndexSearchOutcomeV1::Complete(
                code_search::CodeIndexSearchCompletedV1 {
                    code_generation: executed.query.generation.as_str().to_owned(),
                    ordered_candidates,
                    query_fallback: executed.query.authorized.fallback,
                    display_by_anchor,
                    semantic,
                    next_cursor,
                    coverage,
                },
            )
        })
    })
}
