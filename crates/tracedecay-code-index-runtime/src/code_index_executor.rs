//! Code-index MCP search executor with its hydration and display helpers.

use std::collections::HashMap;
use std::sync::Arc;

use tracedecay_query::code_search;

use crate::code_index_scheduler;
use crate::code_index_task_support;
use crate::mcp_admission::{
    CodeIndexMcpAdmissionUnavailableV1, CodeIndexMcpReadAdmissionV1, CodeIndexMcpReadGrantV1,
    CodeIndexScopeResolverV1,
};
use code_index_task_support::{
    code_index_scope_unavailable, code_index_search_hydration_budget,
    code_index_search_unavailable, code_index_search_unavailable_for_generation,
    generation_for_hydration,
};

const MAX_CONCURRENT_CODE_INDEX_SEARCHES: usize = 1;

struct McpSemanticExecutionControlV1<A> {
    started: std::time::Instant,
    admission_provider: A,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
}

impl<A> McpSemanticExecutionControlV1<A> {
    fn request_termination(&self) -> Option<code_search::CodeIndexSearchUnavailableReasonV1> {
        mcp_search_request_termination(
            self.deadline.as_ref(),
            self.cancellation.as_ref(),
            tracedecay_application::clock::now_micros().0,
        )
    }
}

pub fn mcp_search_request_termination(
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

/// The authority check repeated at every point a search may still be abandoned:
/// the request itself may have been cancelled or timed out, or the MCP read
/// route may have been revoked underneath it. `Some` means stop and return.
fn search_terminated<A: CodeIndexMcpReadAdmissionV1>(
    control: &McpSemanticExecutionControlV1<A>,
    admission_provider: &A,
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

/// Generation-pinned map from file identity digest to logical path, built
/// once per search request so per-candidate display binding can name the
/// declaring file without re-deriving identities for every candidate. The
/// paths feed the response's referenced-file set, which savings accounting
/// uses as the raw-read counterfactual on the executor-served route.
pub struct CodeIndexDisplayPathIndexV1 {
    by_identity: HashMap<String, String>,
}

impl CodeIndexDisplayPathIndexV1 {
    pub fn for_generation(
        generation: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    ) -> std::result::Result<Self, tracedecay_query::retrieval::hydrate::HydrationUnavailableV1>
    {
        use tracedecay_query::retrieval::hydrate::HydrationUnavailableV1;

        let snapshot = generation.snapshot();
        let mut by_identity = HashMap::with_capacity(snapshot.files.len());
        for file in &snapshot.files {
            if file.disposition != tracedecay_domain::SnapshotFileDispositionV1::Present {
                continue;
            }
            let identity = crate::code_index::chunks::code_file_identity(
                snapshot.repository.as_str(),
                &file.logical_path,
            )
            .map_err(|_| HydrationUnavailableV1::Internal)?;
            by_identity.insert(identity.as_str().to_owned(), file.logical_path.clone());
        }
        Ok(Self { by_identity })
    }

    fn logical_path(&self, identity: &str) -> Option<&str> {
        self.by_identity.get(identity).map(String::as_str)
    }
}

pub fn code_index_search_display_binding(
    generation: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    display_paths: &CodeIndexDisplayPathIndexV1,
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
            (code_index_symbol_display(symbol, display_paths)?, None)
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
                    code_index_symbol_display(symbol, display_paths)?
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
                        path: file.logical_path.clone(),
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

pub fn code_index_text_search_display_binding(
    latest: &code_index_scheduler::LatestCodeTextGenerationV1,
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

    let metadata = latest.metadata();
    let manifest = metadata.manifest();
    let snapshot = metadata.snapshot();
    if request.scope.privacy_domain != manifest.privacy_domain
        || request.scope.root.repository != snapshot.repository
        || request.scope.root.worktree != snapshot.worktree
        || request.scope.root.reference != snapshot.reference
        || request.snapshot.freshness_digest.as_str() != manifest.snapshot_digest.as_str()
        || request.snapshot.captured_at != manifest.seal.sealed_at
    {
        return Err(HydrationUnavailableV1::Stale);
    }

    let source_prefix = format!("code-chunk:{}:", manifest.generation_id.as_str());
    let (provenance, chunk_id) = candidate
        .candidate
        .occurrences
        .iter()
        .find_map(|provenance| {
            let chunk_id = provenance
                .source_occurrence_id
                .as_str()
                .strip_prefix(&source_prefix)?;
            (provenance.repository_id.as_ref() == Some(&request.scope.root.repository)
                && provenance.source_namespace == provenance.freshness.source_namespace
                && provenance.freshness.compatibility
                    == tracedecay_domain::FreshnessCompatibilityV1::Current
                && provenance.source_namespace.as_str() == "ns.code.daemon")
                .then_some((provenance.clone(), chunk_id))
        })
        .ok_or(HydrationUnavailableV1::Invalid)?;
    let chunk_id = tracedecay_domain::CodeSearchChunkId::new(chunk_id.to_owned())
        .map_err(|_| HydrationUnavailableV1::Invalid)?;
    let occurrence = latest
        .artifact_occurrence_by_chunk(&chunk_id)
        .map_err(|_| HydrationUnavailableV1::AuthorityUnavailable)?;
    if occurrence.generation != manifest.generation_id
        || !snapshot.files.iter().any(|file| {
            file.file_occurrence_id == occurrence.file
                && file.logical_path == occurrence.logical_path
                && file.disposition == tracedecay_domain::SnapshotFileDispositionV1::Present
        })
    {
        return Err(HydrationUnavailableV1::Stale);
    }

    let anchor = candidate.candidate.anchor_id.as_str();
    if let Some(symbol) = anchor.strip_prefix("code-symbol:") {
        if occurrence
            .symbol
            .as_ref()
            .map(tracedecay_domain::SymbolOccurrenceId::as_str)
            != Some(symbol)
        {
            return Err(HydrationUnavailableV1::Invalid);
        }
    } else if anchor.strip_prefix("code-chunk:") != Some(chunk_id.as_str()) {
        return Err(HydrationUnavailableV1::Invalid);
    }

    let display = match occurrence.symbol {
        Some(_) => code_search::CodeIndexSearchDisplayV1 {
            name: occurrence
                .simple_name
                .ok_or(HydrationUnavailableV1::Invalid)?,
            qualified_name: occurrence
                .qualified_name
                .ok_or(HydrationUnavailableV1::Invalid)?,
            kind: occurrence.kind.ok_or(HydrationUnavailableV1::Invalid)?,
            path: occurrence.logical_path,
        },
        None => {
            if occurrence.simple_name.is_some()
                || occurrence.qualified_name.is_some()
                || occurrence.kind.is_some()
            {
                return Err(HydrationUnavailableV1::Invalid);
            }
            let name = occurrence
                .logical_path
                .rsplit('/')
                .next()
                .unwrap_or(occurrence.logical_path.as_str())
                .to_owned();
            code_search::CodeIndexSearchDisplayV1 {
                name,
                qualified_name: occurrence.logical_path.clone(),
                kind: "file".to_owned(),
                path: occurrence.logical_path,
            }
        }
    };
    Ok((display, provenance))
}

enum CodeIndexSearchDisplaySourceV1 {
    Text(code_index_scheduler::LatestCodeTextGenerationV1),
    Complete {
        latest: code_index_scheduler::LatestCompleteCodeIndexV1,
        paths: CodeIndexDisplayPathIndexV1,
    },
}

impl CodeIndexSearchDisplaySourceV1 {
    fn binding(
        &self,
        request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
    ) -> std::result::Result<
        (
            code_search::CodeIndexSearchDisplayV1,
            tracedecay_domain::OccurrenceProvenance,
        ),
        tracedecay_query::retrieval::hydrate::HydrationUnavailableV1,
    > {
        match self {
            Self::Text(latest) => {
                code_index_text_search_display_binding(latest, request, candidate)
            }
            Self::Complete { latest, paths } => {
                code_index_search_display_binding(latest.generation(), paths, request, candidate)
            }
        }
    }
}

fn code_index_symbol_display(
    symbol: &crate::code_index::lineage::LineageSymbolRecordV1,
    display_paths: &CodeIndexDisplayPathIndexV1,
) -> std::result::Result<
    code_search::CodeIndexSearchDisplayV1,
    tracedecay_query::retrieval::hydrate::HydrationUnavailableV1,
> {
    // A published symbol whose declaring file is absent from its own
    // generation snapshot is corrupt lineage, not a display-time default.
    let path = display_paths
        .logical_path(symbol.file_identity.as_str())
        .ok_or(tracedecay_query::retrieval::hydrate::HydrationUnavailableV1::Invalid)?
        .to_owned();
    Ok(code_search::CodeIndexSearchDisplayV1 {
        name: symbol
            .qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(symbol.qualified_name.as_str())
            .to_owned(),
        qualified_name: symbol.qualified_name.clone(),
        kind: symbol.kind.clone(),
        path,
    })
}

fn code_index_search_display_bytes(
    display: &code_search::CodeIndexSearchDisplayV1,
) -> std::result::Result<u64, tracedecay_query::retrieval::hydrate::HydrationUnavailableV1> {
    serde_json::to_vec(&(
        display.name.as_str(),
        display.qualified_name.as_str(),
        display.kind.as_str(),
        display.path.as_str(),
    ))
    .ok()
    .and_then(|bytes| u64::try_from(bytes.len()).ok())
    .ok_or(tracedecay_query::retrieval::hydrate::HydrationUnavailableV1::Internal)
}

impl<A: CodeIndexMcpReadAdmissionV1> tracedecay_query::retrieval::semantic::SemanticExecutionControl
    for McpSemanticExecutionControlV1<A>
{
    fn is_cancelled(&self) -> bool {
        !self.admission_provider.route_is_registered() || self.request_termination().is_some()
    }

    fn elapsed_micros(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

impl<A: CodeIndexMcpReadAdmissionV1>
    tracedecay_query::retrieval::hydrate::HydrationExecutionControlV1
    for McpSemanticExecutionControlV1<A>
{
    fn elapsed_micros(&self) -> u64 {
        tracedecay_query::retrieval::semantic::SemanticExecutionControl::elapsed_micros(self)
    }

    fn is_cancelled(&self) -> bool {
        !self.admission_provider.route_is_registered() || self.request_termination().is_some()
    }
}

pub fn code_index_search_executor<A, S>(
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_id: tracedecay_domain::ProjectId,
    admission_provider: A,
    scope_resolver: S,
) -> code_search::CodeIndexSearchExecutor
where
    A: CodeIndexMcpReadAdmissionV1,
    S: CodeIndexScopeResolverV1,
{
    let execution_admission = Arc::new(tokio::sync::Semaphore::new(
        MAX_CONCURRENT_CODE_INDEX_SEARCHES,
    ));
    Arc::new(move |request| {
        let schedulers = schedulers.clone();
        let project_id = project_id.clone();
        let admission_provider = admission_provider.clone();
        let scope_resolver = scope_resolver.clone();
        let execution_admission = Arc::clone(&execution_admission);
        Box::pin(hotpath::future!(
            async move {
                let scope = match scope_resolver
                    .resolved_scope_for_project(&request.project_root, &project_id)
                {
                    Ok(scope) => scope,
                    Err(crate::mcp_admission::CodeIndexScopeUnavailableV1) => {
                        return code_index_scope_unavailable();
                    }
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
                let authority = match admission.authorize(&scope, request.authority.as_ref()) {
                    Ok(authority) => authority,
                    // The executor resolves live HEAD. A mid-session checkout mints a
                    // new grant for the new ref while the route-constructed authority
                    // still names the open-time revision. This authority is daemon
                    // state, not client input; rebind to the grant just issued.
                    Err(CodeIndexMcpAdmissionUnavailableV1::AuthorizationStale) => {
                        admission.search_authority()
                    }
                    Err(error) => {
                        return code_index_search_unavailable(
                            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            error.reason(),
                        );
                    }
                };
                let terminal_expected_authority = authority.clone();
                let request_cursor = request.cursor.clone();
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
                let source_revision = request.source_revision;
                let source_tree = request.source_tree;
                let source_reference = request.source_reference;
                if !code_index_task_support::exact_source_is_complete(
                    source_reference.as_ref(),
                    source_revision.as_ref(),
                    source_tree.as_ref(),
                ) || (source_revision.is_none()
                    && request_cursor
                        .as_ref()
                        .is_some_and(|cursor| cursor.code_source.is_some()))
                {
                    return code_index_search_unavailable(
                        code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest,
                        "exact_source_binding_required",
                    );
                }
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
                    let execution_source_revision = source_revision.clone();
                    let execution_source_tree = source_tree.clone();
                    let execution_source_reference = source_reference.clone();
                    let execution_cursor = request_cursor.clone();
                    let execution_request =
                        code_index_scheduler::query_runtime::QuerySearchExecutionRequestV1::new(
                            request.query,
                            policy,
                        );
                    let runtime = tokio::runtime::Handle::current();
                    let execution = tokio::task::spawn_blocking(move || {
                        let _execution_permit = execution_permit;
                        runtime.block_on(async move {
                        let Some(revision) = execution_source_revision else {
                            return execution_schedulers
                                .execute_query_with_semantic(
                                    &execution_project_root,
                                    &execution_scope,
                                    execution_request,
                                    execution_control.clone(),
                                    semantic_mode,
                                )
                                .await;
                        };
                        let tree = execution_source_tree.ok_or(
                            code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1::Query(
                                code_index_scheduler::query_runtime::QuerySearchExecutionErrorV1::GenerationUnavailable,
                            ),
                        )?;
                        let reference = execution_source_reference.ok_or(
                            code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1::Query(
                                code_index_scheduler::query_runtime::QuerySearchExecutionErrorV1::ExactCursorInvalid,
                            ),
                        )?;
                        let control = code_index_scheduler::branch_generations::BranchGenerationReadControlV1 {
                            deadline: execution_control.deadline.clone(),
                            cancellation: execution_control.cancellation.clone(),
                        };
                        let generations = execution_schedulers
                            .generations_for_revisions(
                                &execution_scope,
                                &reference,
                                &revision,
                                &tree,
                                &reference,
                                &revision,
                                &tree,
                                control,
                            )
                            .await
                            .map_err(|reason| {
                                code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1::Query(
                                    code_index_scheduler::query_runtime::QuerySearchExecutionErrorV1::ExactGenerationUnavailable(reason),
                                )
                            })?;
                        let exact_source = tracedecay_domain::CodeSourceCursorBindingV1 {
                            reference,
                            commit: revision,
                            tree,
                            generation: generations
                                .base
                                .generation()
                                .manifest()
                                .generation_id
                                .clone(),
                        };
                        code_index_task_support::verify_exact_source_cursor(
                            &execution_schedulers,
                            &execution_scope,
                            execution_cursor.as_ref(),
                            &exact_source,
                        )
                        .await?;
                        let query = execution_schedulers
                            .execute_query_search_on_generation(
                                &execution_scope,
                                execution_request,
                                generations.base,
                                execution_control.clone(),
                            )
                            .await
                            .map_err(
                                code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1::Query,
                            )?;
                        let generation = query.generation.clone();
                        let semantic = execution_schedulers
                            .execute_semantic_after_query(
                                &execution_project_root,
                                &execution_scope,
                                generations.head.generation(),
                                query.sanitized.request(),
                                query.sanitized.query_view(),
                                &query.authorized,
                                execution_control.as_ref(),
                                semantic_mode,
                            )
                            .await
                            .map_err(|error| match error {
                                tracedecay_query::retrieval::semantic::SemanticQueryServiceError::StrictUnavailable(
                                    abstention,
                                ) => code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1::StrictSemanticUnavailable {
                                    generation,
                                    abstention,
                                },
                                error => code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1::Semantic(error),
                            })?;
                        Ok(code_index_scheduler::semantic_query_runtime::ExecutedQuerySemanticSearchV1 {
                            query,
                            semantic,
                        })
                    })
                    });
                    match hotpath::future!(
                        code_index_task_support::settle_owned_blocking_task(
                            execution,
                            std::time::Duration::from_millis(10),
                            || search_terminated(&control, &admission_provider, None),
                        ),
                        label = "daemon.code_index.search_execute"
                    )
                    .await
                    {
                        Ok(Ok(result)) => result,
                        Ok(Err(_)) => {
                            return code_index_search_unavailable(
                                code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                                "search_task_failed",
                            );
                        }
                        Err(outcome) => return outcome,
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
                            tracedecay_query::retrieval::QueryAuthorityErrorV1::AuthorityUnavailable
                            | tracedecay_query::retrieval::QueryAuthorityErrorV1::Retrieval(
                                tracedecay_domain::RetrievalError::AuthorityUnavailable(_),
                            ),
                        )
                        | QuerySearchExecutionErrorV1::Retrieval(
                            tracedecay_query::retrieval::RetrievalPortError::AuthorityUnavailable(_),
                        ) => (
                            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable
                                .as_str(),
                        ),
                        QuerySearchExecutionErrorV1::GenerationUnavailable => (
                            code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
                            code_search::lane_reason::GENERATION_REBUILDING,
                        ),
                        QuerySearchExecutionErrorV1::GenerationUnverified => (
                            code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnverified,
                            code_search::lane_reason::GENERATION_REBUILDING,
                        ),
                        QuerySearchExecutionErrorV1::ExactGenerationUnavailable(reason) => {
                            (reason, reason.as_str())
                        }
                        QuerySearchExecutionErrorV1::ExactCursorInvalid => (
                            code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest,
                            "exact_cursor_mismatch",
                        ),
                        QuerySearchExecutionErrorV1::InvalidScope(_)
                        | QuerySearchExecutionErrorV1::InvalidPolicy(_) => (
                            code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest,
                            code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest.as_str(),
                        ),
                        // A request the retrieval pipeline itself rejected
                        // (page/cursor shape, budget contract) is the caller's
                        // typed invalid_request, not an internal failure.
                        QuerySearchExecutionErrorV1::Authority(
                            tracedecay_query::retrieval::QueryAuthorityErrorV1::Retrieval(
                                tracedecay_domain::RetrievalError::InvalidRequest(_),
                            ),
                        ) => (
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
                    match scope_resolver.resolved_scope_for_project(&project_root, &project_id) {
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
                let (semantic, ordered_candidates, mut next_cursor, accepted_semantic_budget) =
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
                let display_source = if let Some(text) = schedulers
                    .latest_text_serving_for_scope(&terminal_scope)
                    .await
                    .filter(|text| {
                        text.metadata().manifest().generation_id == executed.query.generation
                    }) {
                    CodeIndexSearchDisplaySourceV1::Text(text)
                } else {
                    let latest = match generation_for_hydration(
                        &schedulers,
                        &terminal_scope,
                        &executed.query.generation,
                        control.deadline.clone(),
                        control.cancellation.clone(),
                    )
                    .await
                    {
                        Ok(latest) => latest,
                        Err(outcome) => return outcome,
                    };
                    let paths =
                        match CodeIndexDisplayPathIndexV1::for_generation(latest.generation()) {
                            Ok(paths) => paths,
                            Err(_) => {
                                return code_index_search_unavailable_for_generation(
                                    Some(executed.query.generation.as_str().to_owned()),
                                    code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                                    "display_path_index_unavailable",
                                );
                            }
                        };
                    CodeIndexSearchDisplaySourceV1::Complete { latest, paths }
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
                            scope_resolver.resolved_scope_for_project(&project_root, &project_id)
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
                        let Ok(current_admission) =
                            admission_provider.admit_current(&current_scope)
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
                let exact_source = source_reference
                    .as_ref()
                    .zip(source_revision.as_ref())
                    .zip(source_tree.as_ref())
                    .map(|((reference, commit), tree)| {
                        tracedecay_domain::CodeSourceCursorBindingV1 {
                            reference: reference.clone(),
                            commit: commit.clone(),
                            tree: tree.clone(),
                            generation: executed.query.generation.clone(),
                        }
                    });
                if let Err(error) = code_index_task_support::bind_exact_source_cursor(
                    &schedulers,
                    &terminal_scope,
                    next_cursor.as_mut(),
                    exact_source,
                )
                .await
                {
                    use code_index_task_support::ExactCursorPublicationErrorV1;
                    return match error {
                    ExactCursorPublicationErrorV1::AuthorityUnavailable => {
                        code_index_search_unavailable_for_generation(
                            Some(executed.query.generation.as_str().to_owned()),
                            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            "query_authority_unavailable",
                        )
                    }
                    ExactCursorPublicationErrorV1::BindingFailed => {
                        code_index_search_unavailable_for_generation(
                            Some(executed.query.generation.as_str().to_owned()),
                            code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                            "cursor_binding_failed",
                        )
                    }
                };
                }
                let preflight =
                |request: &tracedecay_domain::RetrievalRequest,
                 candidate: &tracedecay_domain::RankedCandidate,
                 _permit: &tracedecay_query::retrieval::hydrate::HydrationWorkPermitV1| {
                    use tracedecay_query::retrieval::hydrate::HydrationPreflightOutcomeV1;

                    match display_source.binding(request, candidate)
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

                    let (display, provenance) = match display_source.binding(request, candidate) {
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
                let mut source =
                    CodeIndexSearchHydrationSourceV1::new(authorize, preflight, hydrate);
                let hydrated = match hotpath::measure_block!("daemon.code_index.search_hydrate", {
                    tracedecay_query::retrieval::hydrate::CanonicalLateHydration::new(&mut source)
                        .hydrate_with_control(
                            &hydration_request,
                            ordered_candidates.as_slice(),
                            &hydration_budget,
                            control.as_ref(),
                        )
                }) {
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
                hydrated_candidates
                    .extend(ordered_candidates.into_iter().skip(hydrated_prefix_len));
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
                    match scope_resolver.resolved_scope_for_project(&project_root, &project_id) {
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
                let publication_admission =
                    match admission_provider.admit_current(&publication_scope) {
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
                let coverage = code_search::CodeIndexSearchCoverageV1::from_fallback_lane_coverage(
                    &executed
                        .query
                        .authorized
                        .fallback
                        .public_fallback_lane_coverage,
                    executed.query.generation.as_str(),
                    executed.query.served_stale,
                    &semantic,
                );
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
            },
            label = "daemon.code_index.search"
        ))
    })
}
