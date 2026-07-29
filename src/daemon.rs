use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};

use crate::application::context::CancellationToken;
use crate::client_identity::DaemonClientIdentity;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::ReplayTransport;
use crate::mcp::server::{McpMethod, SERVER_INSTRUCTIONS, classify_mcp_method, initialize_result};
use crate::mcp::tools::{
    ToolRegistryMode, default_catalog_discovery_authority, explore_call_budget,
    get_catalog_filtered_tool_definitions_with_budget,
    get_catalog_filtered_tool_definitions_with_warming_budget, project_catalog_discovery_scope,
};
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};
use branch_add::{branch_add_response, coordinated_hook_branch_writer, parse_branch_add_request};
use branch_admin::{StoreAdministration, parse_branch_admin_request, write_branch_admin_response};
#[cfg(all(unix, test))]
use memory_repair_scheduler::{
    MemoryRepairPassDecision, MemoryRepairSchedulerHandle, MemoryRepairTickOutcome,
    legacy_memory_cutover_should_retry, memory_repair_tick_outcome,
    run_memory_repair_scheduler_tick,
};
#[cfg(all(unix, test))]
use scheduler::{
    AutomationSchedulerHandle, automation_scheduler_configured,
    automation_scheduler_tick_secs_for_project, automation_staged_log_fields,
    daemon_scheduler_record_log_line, run_automation_scheduler_tick, scheduler_task_log_fields,
    user_config_for_client,
};
use transport::{BrokerListener, BrokerStream, DaemonAuthPreface, DaemonEndpoint};

/// Captures the daemon's exact native Git transaction precondition for
/// transport-parity tests. This is not compiled into production builds.
#[cfg(all(unix, feature = "test-transport"))]
#[doc(hidden)]
pub fn capture_exact_git_snapshot_for_test(
    repository_root: &Path,
    project_id: tracedecay_domain::ProjectId,
    repository_id: tracedecay_domain::RepositoryId,
    worktree_id: tracedecay_domain::WorktreeId,
    captured_at: tracedecay_domain::UtcMicros,
) -> tracedecay_domain::RepositoryStateSnapshotV1 {
    git_transactions::capture_exact_snapshot_for_test(
        repository_root,
        project_id,
        repository_id,
        worktree_id,
        captured_at,
    )
}

pub const SERVICE_NAME: &str = "tracedecay.service";
pub const SOCKET_ENV: &str = "TRACEDECAY_DAEMON_SOCKET";
pub(crate) const PROJECT_WARMING_RETRY_HINT: &str =
    "is warming in the background; retry the same tool shortly";
#[cfg(unix)]
const TOOL_LIST_CHANGED_METHOD: &str = "notifications/tools/list_changed";
#[cfg(unix)]
const MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION: usize = 1_024;
const MAX_CACHED_PROJECT_SERVERS: usize = 8;
const MAX_TRACKED_PROJECT_OPEN_TASKS: usize = MAX_CACHED_PROJECT_SERVERS;
const PROJECT_OPEN_REQUEST_DEADLINE: Duration = Duration::from_millis(500);
const PROJECT_OPEN_FAILURE_RETRY_BACKOFF: Duration = Duration::from_millis(250);
/// Backoff for a persisted-row authority defect, which only an operator can
/// clear. Reopening re-runs the exhaustive authority audit over every
/// `observations` row and fails on the same row every time, so the debounce
/// cadence above would saturate a core for as long as the daemon runs.
const PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF: Duration = Duration::from_mins(5);
const PROJECT_OPEN_FAILURE_RETRY_HINT: &str =
    "project route open is backed off after an invariant rejection";

fn sole_mounted_graph_matching(
    graphs: &[Arc<crate::tracedecay::TraceDecay>],
    predicate: impl Fn(&crate::tracedecay::TraceDecay) -> bool,
) -> std::result::Result<Option<Arc<crate::tracedecay::TraceDecay>>, ()> {
    let mut matches = graphs.iter().filter(|graph| predicate(graph.as_ref()));
    let Some(graph) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(());
    }
    Ok(Some(Arc::clone(graph)))
}

fn retained_project_graph_resolver(
    administration: StoreAdministration,
) -> crate::mcp::server::RetainedProjectGraphResolver {
    Arc::new(move |request| {
        let administration = administration.clone();
        Box::pin(async move {
            let graphs = administration.mounted_project_graphs().await;
            let requested_root = authority::canonical_identity_path(
                &request.requested_worktree_root,
            )
            .map_err(|error| {
                TraceDecayError::project_route(
                    "project_route_unavailable",
                    true,
                    format!(
                        "workspace identity is unavailable for {}: {error}",
                        request.requested_worktree_root.display()
                    ),
                )
            })?;
            let registered_root = authority::canonical_identity_path(&request.registered_root)
                .map_err(|error| {
                    TraceDecayError::project_route(
                        "project_route_unavailable",
                        true,
                        format!(
                            "registered project identity is unavailable for {}: {error}",
                            request.registered_root.display()
                        ),
                    )
                })?;
            let Some(owner) = request.owner.as_ref() else {
                return sole_mounted_graph_matching(&graphs, |graph| {
                    authority::canonical_identity_path(graph.project_root()).ok()
                        == Some(requested_root.clone())
                })
                .map_err(|()| {
                    TraceDecayError::project_route(
                        "project_route_ambiguous",
                        false,
                        format!(
                            "multiple mounted graphs claim workspace {}",
                            request.requested_worktree_root.display()
                        ),
                    )
                });
            };
            let project_id = owner.project.project_id.as_str();
            let candidates = graphs
                .into_iter()
                .filter(|graph| {
                    graph.store_layout().identity.project_id.as_deref() == Some(project_id)
                        && request
                            .requested_git_common_dir
                            .as_ref()
                            .is_none_or(|requested| {
                                let requested = authority::canonical_identity_path(requested).ok();
                                let mounted = crate::worktree::git_common_dir(graph.project_root())
                                    .and_then(|path| {
                                        authority::canonical_identity_path(&path).ok()
                                    });
                                mounted.is_none() || mounted == requested
                            })
                })
                .collect::<Vec<_>>();
            let branch_matches = |graph: &crate::tracedecay::TraceDecay| {
                request.requested_branch.as_deref().is_some_and(|branch| {
                    graph.serving_branch() == Some(branch) || graph.active_branch() == Some(branch)
                })
            };
            let root_matches = |graph: &crate::tracedecay::TraceDecay, root: &Path| {
                authority::canonical_identity_path(graph.project_root()).ok()
                    == Some(root.to_path_buf())
            };
            for selected in [
                sole_mounted_graph_matching(&candidates, |graph| {
                    root_matches(graph, &requested_root) && branch_matches(graph)
                }),
                sole_mounted_graph_matching(&candidates, branch_matches),
                sole_mounted_graph_matching(&candidates, |graph| {
                    root_matches(graph, &requested_root)
                }),
                sole_mounted_graph_matching(&candidates, |graph| {
                    root_matches(graph, &registered_root)
                }),
                sole_mounted_graph_matching(&candidates, |_| true),
            ] {
                match selected {
                    Ok(Some(graph)) => return Ok(Some(graph)),
                    Ok(None) => {}
                    Err(()) => {
                        return Err(TraceDecayError::project_route(
                            "project_route_ambiguous",
                            false,
                            format!(
                                "multiple mounted graphs claim registered project '{}'",
                                owner.project.project_id
                            ),
                        ));
                    }
                }
            }
            Ok(None)
        })
    })
}

struct McpSemanticExecutionControlV1 {
    started: std::time::Instant,
    admission_provider: pr9_mcp_admission::Pr9McpReadAdmissionProviderV1,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
}

impl McpSemanticExecutionControlV1 {
    fn request_termination(
        &self,
    ) -> Option<crate::mcp::server::CodeIndexSearchUnavailableReasonV1> {
        mcp_search_request_termination(
            self.deadline.as_ref(),
            self.cancellation.as_ref(),
            mcp_now_micros(),
        )
    }
}

fn mcp_now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_micros()).ok())
        .unwrap_or(i64::MAX)
}

fn mcp_search_request_termination(
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
    now_micros: i64,
) -> Option<crate::mcp::server::CodeIndexSearchUnavailableReasonV1> {
    if cancellation.is_some_and(tracedecay_application::CancellationSignal::is_cancelled) {
        return Some(crate::mcp::server::CodeIndexSearchUnavailableReasonV1::Cancelled);
    }
    deadline
        .is_some_and(|deadline| now_micros >= deadline.expires_at.0)
        .then_some(crate::mcp::server::CodeIndexSearchUnavailableReasonV1::TimedOut)
}

fn code_index_scope_unavailable() -> crate::mcp::server::CodeIndexSearchOutcomeV1 {
    crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
        crate::mcp::server::CodeIndexSearchUnavailableV1 {
            code_generation: None,
            reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                reason: "scope_unavailable",
            },
        },
    )
}

fn code_index_search_hydration_budget(
    accepted_semantic_budget: Option<&tracedecay_domain::RetrievalBudget>,
    pr9_budget: &tracedecay_domain::RetrievalBudget,
) -> tracedecay_domain::RetrievalBudget {
    accepted_semantic_budget.copied().unwrap_or(*pr9_budget)
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
    crate::query::retrieval::hydrate::LateHydrationSource<
        crate::mcp::server::CodeIndexSearchDisplayV1,
    > for CodeIndexSearchHydrationSourceV1<A, P, H>
where
    A: FnMut(
        &tracedecay_domain::RetrievalRequest,
        &tracedecay_domain::RankedCandidate,
    ) -> crate::query::retrieval::hydrate::HydrationAuthorizationV1,
    P: FnMut(
        &tracedecay_domain::RetrievalRequest,
        &tracedecay_domain::RankedCandidate,
        &crate::query::retrieval::hydrate::HydrationWorkPermitV1,
    ) -> crate::query::retrieval::hydrate::HydrationPreflightOutcomeV1,
    H: FnMut(
        &tracedecay_domain::RetrievalRequest,
        &tracedecay_domain::RankedCandidate,
        &crate::query::retrieval::hydrate::HydrationWorkPermitV1,
    ) -> crate::query::retrieval::hydrate::HydrationReadOutcomeV1<
        crate::mcp::server::CodeIndexSearchDisplayV1,
    >,
{
    fn authorize(
        &mut self,
        request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
    ) -> crate::query::retrieval::hydrate::HydrationAuthorizationV1 {
        (self.authorize)(request, candidate)
    }

    fn preflight_authorized(
        &mut self,
        request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        permit: &crate::query::retrieval::hydrate::HydrationWorkPermitV1,
    ) -> crate::query::retrieval::hydrate::HydrationPreflightOutcomeV1 {
        use crate::query::retrieval::hydrate::{
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
        permit: &crate::query::retrieval::hydrate::HydrationWorkPermitV1,
    ) -> crate::query::retrieval::hydrate::HydrationReadOutcomeV1<
        crate::mcp::server::CodeIndexSearchDisplayV1,
    > {
        use crate::query::retrieval::hydrate::{
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

fn code_index_search_display_binding(
    generation: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    request: &tracedecay_domain::RetrievalRequest,
    candidate: &tracedecay_domain::RankedCandidate,
) -> std::result::Result<
    (
        crate::mcp::server::CodeIndexSearchDisplayV1,
        tracedecay_domain::OccurrenceProvenance,
    ),
    crate::query::retrieval::hydrate::HydrationUnavailableV1,
> {
    use crate::query::retrieval::hydrate::HydrationUnavailableV1;

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
                    crate::mcp::server::CodeIndexSearchDisplayV1 {
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
) -> crate::mcp::server::CodeIndexSearchDisplayV1 {
    crate::mcp::server::CodeIndexSearchDisplayV1 {
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
    display: &crate::mcp::server::CodeIndexSearchDisplayV1,
) -> std::result::Result<u64, crate::query::retrieval::hydrate::HydrationUnavailableV1> {
    serde_json::to_vec(&(
        display.name.as_str(),
        display.qualified_name.as_str(),
        display.kind.as_str(),
    ))
    .ok()
    .and_then(|bytes| u64::try_from(bytes.len()).ok())
    .ok_or(crate::query::retrieval::hydrate::HydrationUnavailableV1::Internal)
}

impl crate::query::retrieval::semantic::SemanticExecutionControl for McpSemanticExecutionControlV1 {
    fn is_cancelled(&self) -> bool {
        !self.admission_provider.route_is_registered() || self.request_termination().is_some()
    }

    fn elapsed_micros(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

impl crate::query::retrieval::hydrate::HydrationExecutionControlV1
    for McpSemanticExecutionControlV1
{
    fn elapsed_micros(&self) -> u64 {
        crate::query::retrieval::semantic::SemanticExecutionControl::elapsed_micros(self)
    }

    fn is_cancelled(&self) -> bool {
        !self.admission_provider.route_is_registered() || self.request_termination().is_some()
    }
}

fn code_index_search_executor(
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_id: tracedecay_domain::ProjectId,
    admission_provider: pr9_mcp_admission::Pr9McpReadAdmissionProviderV1,
) -> crate::mcp::server::CodeIndexSearchExecutor {
    Arc::new(move |request| {
        let schedulers = schedulers.clone();
        let project_id = project_id.clone();
        let admission_provider = admission_provider.clone();
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
                    return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                        crate::mcp::server::CodeIndexSearchUnavailableV1 {
                            code_generation: None,
                            reason:
                                crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                reason: error.reason(),
                            },
                        },
                    );
                }
            };
            let current_authority = admission.search_authority();
            let authority = match admission.authorize(&scope, Some(&current_authority)) {
                Ok(authority) => authority,
                Err(error) => {
                    return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                        crate::mcp::server::CodeIndexSearchUnavailableV1 {
                            code_generation: None,
                            reason:
                                crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                reason: error.reason(),
                            },
                        },
                    );
                }
            };
            let terminal_expected_authority = authority.clone();
            let policy = match (
                tracedecay_domain::SanitizerRevision::new(
                    crate::query::retrieval::PR9_QUERY_SANITIZER_REVISION_V1,
                ),
                tracedecay_domain::QueryNormalizationRevision::new(
                    crate::query::retrieval::PR9_QUERY_NORMALIZATION_REVISION_V1,
                ),
                tracedecay_domain::ExactAdmissionRuleRevision::new(
                    crate::query::retrieval::PR9_EXACT_RULE_REVISION_V1,
                ),
                tracedecay_domain::ComponentRevision::new(
                    crate::query::retrieval::PR9_LEXICAL_PROFILE_REVISION_V1,
                ),
                tracedecay_domain::ScoreDomainId::new(
                    crate::query::retrieval::PR9_LEXICAL_SCORE_DOMAIN_V1,
                ),
            ) {
                (
                    Ok(sanitizer_revision),
                    Ok(normalization_revision),
                    Ok(exact_rule_revision),
                    Ok(lexical_profile_revision),
                    Ok(lexical_score_domain),
                ) => code_index_scheduler::pr9_runtime::Pr9SearchExecutionPolicyV1 {
                    principal: authority.principal,
                    authorization_revision: authority.authorization_revision,
                    sanitizer_revision,
                    normalization_revision,
                    exact_rule_revision,
                    lexical_profile_revision,
                    lexical_score_domain,
                    fuzzy_budget: crate::query::retrieval::lexical::MAX_FUZZY_TERM_EXPANSIONS_V1,
                    graph_edge_kinds: vec![tracedecay_domain::RelationEdgeKindV1::Calls],
                    graph_max_depth: 1,
                    page_size: request.limit,
                    cursor: request.cursor,
                },
                _ => {
                    return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                        crate::mcp::server::CodeIndexSearchUnavailableV1 {
                            code_generation: None,
                            reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::InvalidRequest,
                            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                reason: "invalid_request",
                            },
                        },
                    );
                }
            };
            let project_root = request.project_root;
            let mode = request.mode;
            let deadline = request.deadline;
            let cancellation = request.cancellation;
            let semantic_mode = match mode {
                crate::mcp::server::CodeIndexSearchModeV1::FallbackAllowed => {
                    crate::query::retrieval::semantic::SemanticQueryModeV1::FallbackAllowed
                }
                crate::mcp::server::CodeIndexSearchModeV1::StrictSemantic => {
                    crate::query::retrieval::semantic::SemanticQueryModeV1::StrictSemantic
                }
            };
            let control = Arc::new(McpSemanticExecutionControlV1 {
                started: std::time::Instant::now(),
                admission_provider: admission_provider.clone(),
                deadline,
                cancellation,
            });
            if let Some(reason) = control.request_termination() {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: None,
                        reason,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: reason.as_str(),
                        },
                    },
                );
            }
            if !admission_provider.route_is_registered() {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: None,
                        reason:
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: "route_revoked",
                        },
                    },
                );
            }
            let execution_result = {
                let execution_schedulers = schedulers.clone();
                let execution_project_root = project_root.clone();
                let execution_scope = scope.clone();
                let execution_control = Arc::clone(&control);
                let execution_request =
                    code_index_scheduler::pr9_runtime::Pr9SearchExecutionRequestV1::new(
                        request.query,
                        policy,
                    );
                let runtime = tokio::runtime::Handle::current();
                let mut execution = tokio::task::spawn_blocking(move || {
                    runtime.block_on(async move {
                        execution_schedulers
                            .execute_pr9_with_semantic(
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
                            Err(_) => return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                                crate::mcp::server::CodeIndexSearchUnavailableV1 {
                                    code_generation: None,
                                    reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::Internal,
                                    semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                        reason: "search_task_failed",
                                    },
                                },
                            ),
                        },
                        _ = control_poll.tick() => {
                            if let Some(reason) = control.request_termination() {
                                execution.abort();
                                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                                        code_generation: None,
                                        reason,
                                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                            reason: reason.as_str(),
                                        },
                                    },
                                );
                            }
                            if !admission_provider.route_is_registered() {
                                execution.abort();
                                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                                        code_generation: None,
                                        reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                            reason: "route_revoked",
                                        },
                                    },
                                );
                            }
                        }
                    }
                }
            };
            if let Some(reason) = control.request_termination() {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: None,
                        reason,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: reason.as_str(),
                        },
                    },
                );
            }
            if !admission_provider.route_is_registered() {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: None,
                        reason:
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: "route_revoked",
                        },
                    },
                );
            }
            let executed = match execution_result {
                Ok(executed) => executed,
                Err(error) => {
                    use code_index_scheduler::pr9_runtime::Pr9SearchExecutionErrorV1;
                    use code_index_scheduler::semantic_query_runtime::Pr9SemanticSearchExecutionErrorV1;
                    tracing::warn!(
                        project_id = %project_id.as_str(),
                        error = %error,
                        "code_index_search_failed"
                    );
                    if let Pr9SemanticSearchExecutionErrorV1::StrictSemanticUnavailable {
                        generation,
                        abstention,
                    } = &error
                    {
                        return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                            crate::mcp::server::CodeIndexSearchUnavailableV1 {
                                code_generation: Some(generation.as_str().to_owned()),
                                reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::SemanticUnavailable,
                                semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                    reason: code_index_scheduler::semantic_query_runtime::semantic_abstention_reason(abstention),
                                },
                            },
                        );
                    }
                    let reason = match error {
                        Pr9SemanticSearchExecutionErrorV1::Pr9(error) => match error {
                        Pr9SearchExecutionErrorV1::AuthorityUnavailable
                        | Pr9SearchExecutionErrorV1::Authority(
                            crate::query::retrieval::Pr9QueryAuthorityErrorV1::AuthorityUnavailable,
                        ) => crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        Pr9SearchExecutionErrorV1::GenerationUnavailable => {
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
                        }
                        Pr9SearchExecutionErrorV1::InvalidScope(_)
                        | Pr9SearchExecutionErrorV1::InvalidPolicy(_) => {
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::InvalidRequest
                        }
                        Pr9SearchExecutionErrorV1::Retrieval(_)
                        | Pr9SearchExecutionErrorV1::Authority(_) => {
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::Internal
                        }
                        },
                        Pr9SemanticSearchExecutionErrorV1::Semantic(_) => {
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::Internal
                        }
                        Pr9SemanticSearchExecutionErrorV1::StrictSemanticUnavailable { .. } => {
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::SemanticUnavailable
                        }
                    };
                    return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                        crate::mcp::server::CodeIndexSearchUnavailableV1 {
                            code_generation: None,
                            reason,
                            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                reason: "semantic_unavailable",
                            },
                        },
                    );
                }
            };
            if let Some(reason) = control.request_termination() {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                        reason,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: reason.as_str(),
                        },
                    },
                );
            }
            if !admission_provider.route_is_registered() {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                        reason:
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: "route_revoked",
                        },
                    },
                );
            }
            let terminal_scope = match project_open_owners::resolved_scope_for_project(
                &project_root,
                &project_id,
            ) {
                Ok(terminal_scope) if terminal_scope == scope => terminal_scope,
                _ => {
                    return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                        crate::mcp::server::CodeIndexSearchUnavailableV1 {
                            code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                            reason:
                                crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                reason: "scope_changed_before_publication",
                            },
                        },
                    );
                }
            };
            let terminal_admission = match admission_provider.admit_current(&terminal_scope) {
                Ok(admission) => admission,
                Err(error) => {
                    return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                        crate::mcp::server::CodeIndexSearchUnavailableV1 {
                            code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                            reason:
                                crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                reason: error.reason(),
                            },
                        },
                    );
                }
            };
            let terminal_authority = terminal_admission.search_authority();
            if terminal_authority != terminal_expected_authority
                || terminal_admission
                    .authorize(&terminal_scope, Some(&terminal_authority))
                    .is_err()
            {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                        reason:
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: "authorization_changed_before_publication",
                        },
                    },
                );
            }
            let (semantic, ordered_candidates, next_cursor, accepted_semantic_budget) =
                match &executed.semantic {
                code_index_scheduler::semantic_query_runtime::SemanticAugmentationOutcomeV1::Augmented {
                    composition,
                    cursor,
                    hydration_budget,
                    ..
                } => (
                    crate::mcp::server::CodeIndexSemanticStatusV1::Complete,
                    composition.ranked_candidates.clone(),
                    cursor.clone(),
                    Some(hydration_budget),
                ),
                code_index_scheduler::semantic_query_runtime::SemanticAugmentationOutcomeV1::Fallback {
                    abstention,
                    fallback,
                } => (
                    crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                        reason: code_index_scheduler::semantic_query_runtime::semantic_abstention_reason(
                            abstention,
                        ),
                    },
                    executed.pr9.authorized.fallback.ordered_candidates.clone(),
                    fallback.cursor.clone(),
                    None,
                ),
            };
            let Some(latest) = schedulers
                .generation_for(&terminal_scope, &executed.pr9.generation)
                .await
            else {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                        reason:
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: "generation_changed_before_hydration",
                        },
                    },
                );
            };
            let mut hydration_request = executed.pr9.sanitized.request().clone();
            let hydration_budget = code_index_search_hydration_budget(
                accepted_semantic_budget,
                &hydration_request.budget,
            );
            hydration_request.budget = hydration_budget;
            let authorize =
                |request: &tracedecay_domain::RetrievalRequest,
                 _candidate: &tracedecay_domain::RankedCandidate| {
                    use crate::query::retrieval::hydrate::HydrationAuthorizationV1;

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
                 _permit: &crate::query::retrieval::hydrate::HydrationWorkPermitV1| {
                    use crate::query::retrieval::hydrate::HydrationPreflightOutcomeV1;

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
                 _permit: &crate::query::retrieval::hydrate::HydrationWorkPermitV1| {
                    use crate::query::retrieval::hydrate::HydrationReadOutcomeV1;

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
                                crate::query::retrieval::hydrate::HydrationUnavailableV1::Internal,
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
            let hydrated = match crate::query::retrieval::hydrate::DeterministicLateHydration::new(
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
                    return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                        crate::mcp::server::CodeIndexSearchUnavailableV1 {
                            code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                            reason:
                                crate::mcp::server::CodeIndexSearchUnavailableReasonV1::Internal,
                            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                reason: "late_hydration_failed",
                            },
                        },
                    );
                }
            };
            let hydrated_prefix_len = hydrated.results.len();
            let mut display_by_anchor = HashMap::new();
            let mut hydrated_candidates = Vec::with_capacity(ordered_candidates.len());
            for result in hydrated.results {
                use crate::query::retrieval::hydrate::{
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
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                        reason,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: reason.as_str(),
                        },
                    },
                );
            }
            if !admission_provider.route_is_registered() {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                        reason:
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: "route_revoked_before_publication",
                        },
                    },
                );
            }
            let publication_scope = match project_open_owners::resolved_scope_for_project(
                &project_root,
                &project_id,
            ) {
                Ok(publication_scope) if publication_scope == terminal_scope => publication_scope,
                _ => {
                    return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                        crate::mcp::server::CodeIndexSearchUnavailableV1 {
                            code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                            reason:
                                crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                reason: "scope_changed_during_publication",
                            },
                        },
                    );
                }
            };
            let publication_admission = match admission_provider.admit_current(&publication_scope) {
                Ok(admission) => admission,
                Err(error) => {
                    return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                            crate::mcp::server::CodeIndexSearchUnavailableV1 {
                                code_generation: Some(
                                    executed.pr9.generation.as_str().to_owned(),
                                ),
                                reason:
                                    crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                                semantic:
                                    crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                        reason: error.reason(),
                                    },
                            },
                        );
                }
            };
            let publication_authority = publication_admission.search_authority();
            if publication_authority != terminal_expected_authority
                || publication_admission
                    .authorize(&publication_scope, Some(&publication_authority))
                    .is_err()
            {
                return crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                    crate::mcp::server::CodeIndexSearchUnavailableV1 {
                        code_generation: Some(executed.pr9.generation.as_str().to_owned()),
                        reason:
                            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                        semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                            reason: "authorization_changed_during_publication",
                        },
                    },
                );
            }
            crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(
                crate::mcp::server::CodeIndexSearchCompletedV1 {
                    code_generation: executed.pr9.generation.as_str().to_owned(),
                    ordered_candidates,
                    pr9_fallback: executed.pr9.authorized.fallback,
                    display_by_anchor,
                    semantic,
                    next_cursor,
                },
            )
        })
    })
}

mod authority;
mod branch_add;
mod branch_admin;
mod callable_code_authorization;
pub(crate) mod code_index_scheduler;
pub(crate) mod context_scout_lifecycle;
mod core_admission;
mod core_client;
mod core_doctor;
mod core_handshake;
mod core_hooks;
mod core_lifecycle;
mod core_logging;
mod core_proxy;
pub(crate) mod doctor_kernel;
pub(crate) mod hook_v2_replay;
pub(crate) mod pr9_authority_provider;
pub(crate) mod project_open_owners;
mod semantic_evaluation;
pub(crate) use core_admission::*;
pub use core_client::*;
pub(crate) use core_doctor::*;
pub use core_handshake::*;
pub use core_hooks::*;
pub(crate) use core_lifecycle::*;
pub use core_logging::*;
pub use core_proxy::*;
mod git_transactions;
#[cfg(unix)]
mod git_watch;
mod github_credential_lifecycle;
mod http_application;
pub mod lsp_gateway;
#[cfg(unix)]
mod memory_repair_scheduler;
mod pr9_mcp_admission;
#[cfg(unix)]
pub mod pr_autotrack;
mod profile_host_admission_replay;
pub(crate) mod profile_identity;
#[cfg(unix)]
mod scheduler;
mod service;
pub(crate) mod session_temporal_refresh_scheduler;
pub(crate) mod store_runtime;

/// Enables background maintenance only for long-lived daemon/MCP processes.
///
/// Session-store mounts retain the registered database authority for the
/// lifetime of each maintenance task. One-shot commands never enable it.
pub fn mark_process_long_lived_for_session_maintenance() {
    store_runtime::session_registry::mark_process_long_lived_for_session_maintenance();
}

const SEMANTIC_ARTIFACT_GC_PERIOD: Duration = Duration::from_hours(24);

struct SemanticArtifactGcMaintenanceTask(JoinHandle<()>);

impl Drop for SemanticArtifactGcMaintenanceTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn spawn_semantic_artifact_gc_maintenance() -> SemanticArtifactGcMaintenanceTask {
    SemanticArtifactGcMaintenanceTask(tokio::spawn(async {
        let mut interval = tokio::time::interval(SEMANTIC_ARTIFACT_GC_PERIOD);
        loop {
            interval.tick().await;
            let Some(owner) = crate::semantic_code::SemanticModelLifecycleOwnerV1::mounted_shared()
            else {
                continue;
            };
            let now_unix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if owner.run_daemon_artifact_gc(now_unix).is_err() {
                log_daemon_event(
                    "semantic_artifact_gc",
                    &[("outcome", "retry_next_interval".to_owned())],
                );
            }
        }
    }))
}

pub(crate) mod transport;
pub(crate) use service::invocation::{
    BoundedPr13HookOrchestratorV1, DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION,
    DaemonAdvisoryRuntimeRegistrar, DaemonAdvisoryRuntimeRegistrationError,
    DaemonConfigurationRuntimeRegistrar, DaemonContextScoutRuntimeRegistrar,
    DaemonContextScoutRuntimeRegistrationError, DaemonFeedbackRuntimeRegistrar,
    DaemonFeedbackRuntimeRegistrationError, DaemonInvocationOutcome, DaemonInvocationProblem,
    DaemonInvocationRequest, DaemonInvocationResponse, DaemonInvocationService,
    DaemonLspOwnerRegistrar, DaemonLspSessionAccess, DaemonPrimitiveRuntimeRegistrar,
    DaemonPrimitiveRuntimeRegistrationError, DaemonSemanticRuntimeRegistrar,
    DaemonSemanticRuntimeRegistrationError, Pr13AdvisoryCycleInvocationFutureV1,
    Pr13AdvisoryCycleInvocationOutcomeV1, Pr13AdvisoryCycleInvocationPortV1,
    Pr13AdvisoryCycleInvocationRequestV1, Pr13AdvisoryCycleTerminalV1,
    Pr13HookOrchestrationAdmissionV1, Pr13HookOrchestrationRequestV1,
    Pr13HookOrchestrationTriggerV1, admit_registered_pr13_hook_orchestration,
    daemon_operation_event_authority, parse_daemon_invocation_request,
};
pub use service::{
    DaemonServiceSpec, DaemonServiceState, QuiescedDaemonLifecycle, daemon_reachable,
    default_socket_path, enforce_forward_only_service_recovery, install_service,
    installed_service_socket_path, quiesce_installed_service_before_lease,
    refresh_installed_service, refresh_installed_service_under_lease,
    refresh_installed_service_under_lease_with_state, refresh_service,
    restore_installed_service_after_update, service_spec, service_status, socket_path_or_default,
    uninstall_service, verify_installed_service_quiesced_under_lease,
    wait_for_installed_service_state, with_exclusive_maintenance_window,
    with_quiesced_installed_service,
};

#[cfg(unix)]
pub async fn run_foreground(socket_path: PathBuf) -> Result<()> {
    run_foreground_unix(socket_path).await
}

#[cfg(not(unix))]
pub async fn run_foreground(_socket_path: PathBuf) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let requested = transport::default_loopback_endpoint();
    let _lifecycle_lease = crate::lifecycle_lease::acquire_shared_for_profile(
        &profile_root,
        "managed daemon database ownership",
    )?;
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &requested, binary_version())?;
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let (listener, endpoint) = BrokerListener::bind(authority.endpoint()).await?;
    authority.publish_endpoint(&endpoint)?;
    log_daemon_event("daemon_listening", &[("endpoint", endpoint.to_string())]);

    let store_administration =
        StoreAdministration::default().with_profile_identity(authority.profile_identity().clone());
    let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
    install_http_application_cold_resolver(
        &http_application_registry,
        store_administration.clone(),
    )?;
    let http_application_service = http_application::DaemonHttpApplicationService::bind(
        http_application_registry.clone(),
        authority.auth_token(),
    )
    .await?;
    authority.publish_http_application_endpoint(http_application_service.endpoint())?;
    log_daemon_event(
        "daemon_http_application_listening",
        &[("endpoint", http_application_service.endpoint().to_string())],
    );
    let _semantic_artifact_gc = spawn_semantic_artifact_gc_maintenance();

    let lifecycle = DaemonLifecycle::default();
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
    let invocation = DaemonInvocationState::default();
    invocation.configure_github_read_only_credentials(authority.profile_identity());
    let admission = DaemonClientAdmission::new(MAX_CONCURRENT_DAEMON_CLIENTS);
    let per_client_admission = DaemonPerClientAdmission::default();
    let mut clients: JoinSet<Result<()>> = JoinSet::new();
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?,
            completed = clients.join_next(), if !clients.is_empty() => {
                if let Some(Err(error)) = completed {
                    log_daemon_event("daemon_client", &[("outcome", error.to_string())]);
                }
                continue;
            },
            _ = tokio::signal::ctrl_c() => break,
        };
        let permit = match admission.try_admit() {
            DaemonClientAdmissionOutcome::Admitted(permit) => permit,
            DaemonClientAdmissionOutcome::Saturated(response) => {
                reject_saturated_daemon_client(stream, response).await;
                continue;
            }
        };
        let admission_class = permit.class();
        let auth_token = authority.auth_token().to_string();
        let client_lifecycle = lifecycle.clone();
        let store_administration = store_administration.clone();
        let project_open_gates = Arc::clone(&project_open_gates);
        let invocation = invocation.clone();
        let http_application_registry = http_application_registry.clone();
        let per_client_admission = per_client_admission.clone();
        clients.spawn(async move {
            let _permit = permit;
            Box::pin(serve_windows_broker_client_with_class_and_invocation(
                stream,
                &auth_token,
                &client_lifecycle,
                store_administration,
                project_open_gates,
                invocation,
                http_application_registry,
                per_client_admission,
                admission_class,
                #[cfg(test)]
                None,
            ))
            .await
        });
    }
    lifecycle.begin_draining();
    cancel_project_server_startup_ingests(&store_administration).await;
    let _ = timeout(
        DAEMON_TASK_ABORT_DEADLINE,
        http_application_service.shutdown(),
    )
    .await;
    shutdown_portable_project_open_tasks(project_open_gates.as_ref()).await;
    cancel_project_server_startup_ingests(&store_administration).await;
    invocation.shutdown().await;
    let in_flight_drained = timeout(DAEMON_CLIENT_DRAIN_DEADLINE, lifecycle.wait_for_idle())
        .await
        .is_ok();
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    store_administration.shutdown_host_admission_replay().await;
    if !in_flight_drained {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "client_drain_timeout".to_string()),
                (
                    "deadline_secs",
                    DAEMON_CLIENT_DRAIN_DEADLINE.as_secs().to_string(),
                ),
                (
                    "checkpoint",
                    "skipped_active_clients_were_aborted".to_string(),
                ),
            ],
        );
        return endpoint_cleanup;
    }
    shutdown_project_servers(&store_administration).await;
    endpoint_cleanup
}

#[cfg(unix)]
async fn run_foreground_unix(socket_path: PathBuf) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let endpoint = transport::DaemonEndpoint::Unix(socket_path);
    let _lifecycle = crate::lifecycle_lease::acquire_shared_for_profile(
        &profile_root,
        "managed daemon database ownership",
    )?;
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &endpoint, binary_version())?;
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let socket_path = match authority.endpoint() {
        transport::DaemonEndpoint::Unix(path) => path.clone(),
        transport::DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
    if let Some(parent) = socket_path.parent() {
        let parent_existed = parent.exists();
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to create socket directory '{}': {e}",
                parent.display()
            ),
        })?;
        if !parent_existed {
            set_owner_only_permissions(parent, 0o700)?;
        }
    }
    prepare_socket_path(&authority).await?;

    let (listener, bound_endpoint) = BrokerListener::bind(authority.endpoint()).await?;
    authority.publish_endpoint(&bound_endpoint)?;
    set_owner_only_permissions(&socket_path, 0o600)?;
    log_daemon_event(
        "daemon_listening",
        &[("endpoint", bound_endpoint.to_string())],
    );
    let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
    let engine = DaemonEngine::default()
        .with_profile_identity(authority.profile_identity().clone())
        .with_http_application_registry(http_application_registry.clone());
    install_http_application_cold_resolver(
        &http_application_registry,
        engine.store_administration.clone(),
    )?;
    let http_application_service = http_application::DaemonHttpApplicationService::bind(
        http_application_registry.clone(),
        authority.auth_token(),
    )
    .await?;
    authority.publish_http_application_endpoint(http_application_service.endpoint())?;
    log_daemon_event(
        "daemon_http_application_listening",
        &[("endpoint", http_application_service.endpoint().to_string())],
    );
    let _semantic_artifact_gc = spawn_semantic_artifact_gc_maintenance();
    // Install the git-metadata watcher (design D3/D5). The daemon has no single
    // project root, so it uses the default `[sync]` config plus env overrides.
    // When `auto_watch` is off the watcher is inert. The watcher shares the
    // engine's administration coordinator before it can spawn any writer.
    let git_watcher = git_watch::GitWatcher::new_with_administration(
        crate::config::SyncConfig::default().with_env_overrides(),
        engine.store_administration.clone(),
        profile_root.clone(),
    );
    if git_watcher.is_enabled() {
        let profile_database = engine
            .store_administration
            .registered_profile_database()
            .await?;
        git_watcher.spawn(profile_database).await;
    }
    // PR-branch auto-tracking runs independently of the metadata watcher: it is
    // gated per-project on `sync.auto_track_pr_branches` (default off), so this
    // loop is inert unless a project opts in.
    let pr_autotrack_task = pr_autotrack::spawn_with_administration(
        crate::global_db::global_db_path(),
        engine.store_administration.clone(),
    );
    let engine = engine
        .with_git_watcher(git_watcher)
        .with_pr_autotrack_task(pr_autotrack_task)
        .await;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let admission = DaemonClientAdmission::new(MAX_CONCURRENT_DAEMON_CLIENTS);
    let mut client_tasks: JoinSet<Result<()>> = JoinSet::new();

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?,
            completed = client_tasks.join_next(), if !client_tasks.is_empty() => {
                if let Some(completed) = completed {
                    log_client_task_result(completed);
                }
                continue;
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        };
        let permit = match admission.try_admit() {
            DaemonClientAdmissionOutcome::Admitted(permit) => permit,
            DaemonClientAdmissionOutcome::Saturated(response) => {
                reject_saturated_daemon_client(stream, response).await;
                continue;
            }
        };
        let admission_class = permit.class();
        let engine = engine.clone();
        let auth_token = authority.auth_token().to_string();
        let client: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<()>> + Send + 'static>,
        > = Box::pin(serve_authenticated_socket_client_with_class(
            stream,
            engine,
            auth_token,
            admission_class,
        ));
        client_tasks.spawn(async move {
            let _permit = permit;
            client.await
        });
    }
    engine.lifecycle.begin_draining();
    // Stop accepting and unlink the socket before draining so clients that
    // connect during shutdown get NotFound/ConnectionRefused (which they retry
    // via `connect_with_restart_grace`) instead of a queued connection that
    // will never be served.
    drop(listener);
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    let shutdown_completed = timeout(DAEMON_SHUTDOWN_DEADLINE, async {
        cancel_project_server_startup_ingests(&engine.store_administration).await;
        let _ = timeout(
            DAEMON_TASK_ABORT_DEADLINE,
            http_application_service.shutdown(),
        )
        .await;
        engine.shutdown_project_open_tasks().await;
        cancel_project_server_startup_ingests(&engine.store_administration).await;
        // Keep auxiliary process creation blocked until every scheduler and client
        // task is drained or abandoned. A killed app-server call may retry before
        // unwinding, so a shorter guard leaves a shutdown-time respawn race.
        let _codex_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
        // Stop automation before announcing shutdown or waiting for clients.
        // Scheduler tasks may be inside a synchronous auxiliary-agent call, so
        // shutdown also terminates their tracked process trees before joining.
        let (automation_stopped, memory_repair_stopped) = tokio::join!(
            timeout(
                DAEMON_TASK_ABORT_DEADLINE,
                engine.shutdown_automation_schedulers(),
            ),
            timeout(
                DAEMON_TASK_ABORT_DEADLINE,
                engine.shutdown_memory_repair_schedulers(),
            )
        );
        let automation_stopped = automation_stopped.is_ok();
        let memory_repair_stopped = memory_repair_stopped.is_ok();
        if !automation_stopped || !memory_repair_stopped {
            log_daemon_event(
                "daemon_shutdown",
                &[("outcome", "scheduler_lock_timeout".to_string())],
            );
        }
        log_daemon_event(
            "daemon_shutdown",
            &[("socket", socket_path.display().to_string())],
        );
        let in_flight_drained = timeout(
            DAEMON_CLIENT_DRAIN_DEADLINE,
            engine.lifecycle.wait_for_idle(),
        )
        .await
        .is_ok();
        // Once admitted requests are finished (or their bound elapsed), every
        // remaining client task is an idle socket reader or already-cancelled
        // request wrapper. Abort those immediately instead of making shutdown wait
        // for clients to close persistent connections themselves.
        client_tasks.abort_all();
        let clients_drained =
            drain_client_tasks(&mut client_tasks, DAEMON_TASK_ABORT_DEADLINE).await;
        // Client setup and in-flight requests may create schedulers or project
        // servers. Sweep owned background tasks only after all client work drains.
        let background_drained = timeout(
            DAEMON_TASK_ABORT_DEADLINE,
            engine.shutdown_background_tasks(),
        )
        .await
        .is_ok();
        if !in_flight_drained || !clients_drained {
            log_daemon_event(
                "daemon_shutdown",
                &[
                    ("outcome", "client_drain_timeout".to_string()),
                    (
                        "deadline_secs",
                        DAEMON_CLIENT_DRAIN_DEADLINE.as_secs().to_string(),
                    ),
                    (
                        "checkpoint",
                        "skipped_active_clients_were_aborted".to_string(),
                    ),
                ],
            );
        }
        if !background_drained {
            log_daemon_event(
                "daemon_shutdown",
                &[("outcome", "background_task_timeout".to_string())],
            );
        }
        // Graceful shutdown persists tokens-saved counters and checkpoints WALs
        // for every live project server sequentially; with many servers or large
        // WALs that can exceed systemd's stop timeout, which then sends `SIGKILL`
        // to the daemon. On timeout the shutdown future is dropped and we proceed
        // to exit: the remaining persistence is best-effort and the database WAL
        // keeps state crash-safe.
        let completed = timeout(DAEMON_SERVER_SHUTDOWN_DEADLINE, engine.shutdown_servers())
            .await
            .is_ok();
        if !completed {
            log_daemon_event(
                "daemon_shutdown",
                &[
                    ("outcome", "timeout".to_string()),
                    (
                        "deadline_secs",
                        DAEMON_SERVER_SHUTDOWN_DEADLINE.as_secs().to_string(),
                    ),
                ],
            );
        }
    })
    .await
    .is_ok();
    if !shutdown_completed {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "hard_backstop_timeout".to_string()),
                (
                    "deadline_secs",
                    DAEMON_SHUTDOWN_DEADLINE.as_secs().to_string(),
                ),
            ],
        );
    }
    endpoint_cleanup
}

#[cfg(unix)]
fn log_client_task_result(completed: std::result::Result<Result<()>, tokio::task::JoinError>) {
    let error = match completed {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error.to_string(),
        Err(error) if error.is_cancelled() => return,
        Err(error) => error.to_string(),
    };
    log_daemon_event(
        "daemon_client",
        &[("outcome", "error".to_string()), ("error", error)],
    );
}

#[cfg(unix)]
async fn drain_client_tasks(clients: &mut JoinSet<Result<()>>, deadline: Duration) -> bool {
    let drained = timeout(deadline, async {
        while let Some(completed) = clients.join_next().await {
            log_client_task_result(completed);
        }
    })
    .await
    .is_ok();
    if drained {
        return true;
    }

    clients.abort_all();
    let _ = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
        while let Some(completed) = clients.join_next().await {
            log_client_task_result(completed);
        }
    })
    .await;
    false
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path, mode: u32) -> Result<()> {
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to restrict permissions on '{}': {e}",
            path.display()
        ),
    })
}

#[cfg(unix)]
async fn prepare_socket_path(authority: &authority::DaemonAuthority) -> Result<()> {
    authority.ensure_current()?;
    let socket_path = match authority.endpoint() {
        transport::DaemonEndpoint::Unix(path) => path,
        transport::DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
    match UnixStream::connect(socket_path).await {
        Ok(_) => Err(TraceDecayError::Config {
            message: format!(
                "daemon socket '{}' is already in use",
                socket_path.display()
            ),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => std::fs::remove_file(socket_path).map_err(|remove_err| TraceDecayError::Config {
            message: format!(
                "failed to remove stale daemon socket '{}': {remove_err}",
                socket_path.display()
            ),
        }),
    }
}

/// Daemon-generation-local state for the closed invocation protocol.
///
/// The Unix and portable brokers share this state so an authenticated LSP
/// session remains daemon-owned across client connections until it is detached
/// or expires.
#[derive(Clone)]
struct DaemonInvocationState {
    lsp_session_registry: Arc<tokio::sync::Mutex<lsp_gateway::LspSessionRegistry>>,
    service: DaemonInvocationService,
    github_credential_lifecycle:
        github_credential_lifecycle::DaemonGitHubReadOnlyCredentialLifecycleV1,
    code_index_schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    pr9_authority_provider: pr9_authority_provider::DaemonPr9AuthorityProviderV1,
    semantic_projection_scheduler:
        crate::application::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1,
}

impl Default for DaemonInvocationState {
    fn default() -> Self {
        let code_index_schedulers =
            code_index_scheduler::CodeIndexSchedulerRegistryV1::new(MAX_CACHED_PROJECT_SERVERS);
        let service =
            DaemonInvocationService::with_code_index_schedulers(code_index_schedulers.clone());
        Self {
            lsp_session_registry: Arc::new(tokio::sync::Mutex::new(
                lsp_gateway::LspSessionRegistry::default(),
            )),
            service,
            github_credential_lifecycle:
                github_credential_lifecycle::DaemonGitHubReadOnlyCredentialLifecycleV1::default(),
            code_index_schedulers,
            pr9_authority_provider:
                pr9_authority_provider::DaemonPr9AuthorityProviderV1::default(),
            semantic_projection_scheduler:
                crate::application::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1::default(),
        }
    }
}

impl DaemonInvocationState {
    fn configure_github_read_only_credentials(
        &self,
        identity: &profile_identity::LocalProfileIdentityAuthorityV1,
    ) {
        self.github_credential_lifecycle.configure_profile(identity);
    }

    fn mount_github_read_only_credential_authority_for_project(
        &self,
        profile_id: &tracedecay_domain::UserProfileId,
        repository_owner: &str,
        repository_name: &str,
    ) -> crate::application::advisory::github_runtime::ProfileGitHubReadOnlyCredentialMountOutcomeV1
    {
        self.github_credential_lifecycle
            .mount(profile_id, repository_owner, repository_name)
    }

    fn advisory_runtime_registrar(&self) -> DaemonAdvisoryRuntimeRegistrar {
        DaemonAdvisoryRuntimeRegistrar::new(&self.service)
    }

    fn feedback_runtime_registrar(&self) -> DaemonFeedbackRuntimeRegistrar {
        DaemonFeedbackRuntimeRegistrar::new(&self.service)
    }

    fn context_scout_runtime_registrar(&self) -> DaemonContextScoutRuntimeRegistrar {
        DaemonContextScoutRuntimeRegistrar::new(&self.service)
    }

    fn primitive_runtime_registrar(&self) -> DaemonPrimitiveRuntimeRegistrar {
        DaemonPrimitiveRuntimeRegistrar::new(&self.service)
    }

    fn configuration_runtime_registrar(&self) -> DaemonConfigurationRuntimeRegistrar {
        DaemonConfigurationRuntimeRegistrar::new(&self.service)
    }

    fn semantic_runtime_registrar(&self) -> DaemonSemanticRuntimeRegistrar {
        DaemonSemanticRuntimeRegistrar::new(&self.service)
    }

    fn lsp_owner_registrar(&self) -> DaemonLspOwnerRegistrar {
        DaemonLspOwnerRegistrar::new(&self.service)
    }

    async fn mount_pr9_authority_for_project(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> std::result::Result<(), code_index_scheduler::pr9_runtime::Pr9RuntimeMountErrorV1> {
        code_index_scheduler::pr9_runtime::mount_pr9_query_authority_on_project_open(
            &self.code_index_schedulers,
            project_root,
            scope,
            &self.pr9_authority_provider,
        )
        .await
    }

    fn restore_initial_pr9_authority_for_project(
        &self,
        scope: tracedecay_application::ResolvedScope,
        state: crate::config::retrieval::RetrievalProfileStateV1,
    ) -> std::result::Result<
        pr9_authority_provider::Pr9AuthorityProviderStatusV1,
        pr9_authority_provider::Pr9AuthorityUpdateErrorV1,
    > {
        self.pr9_authority_provider
            .install_evaluated_initial_state(scope, state)
    }

    fn pr9_activation_registrar(
        &self,
        project_root: &Path,
    ) -> Arc<dyn crate::application::semantic_runtime::RetrievalProfileActivationObserverV1> {
        Arc::new(pr9_authority_provider::DaemonPr9ActivationRegistrarV1::new(
            self.pr9_authority_provider.clone(),
            self.code_index_schedulers.clone(),
            project_root.to_path_buf(),
        ))
    }

    async fn mount_code_index(
        &self,
        project_root: &Path,
        store_root: PathBuf,
        semantic_runtime: Option<&crate::semantic_code::DaemonSemanticRuntimeHandleV1>,
        semantic_database: Option<Arc<crate::db::Database>>,
        semantic_lifecycle: Option<Arc<crate::semantic_code::SemanticModelLifecycleOwnerV1>>,
        semantic_resources: Option<crate::config::SemanticResourceCeilings>,
    ) -> Result<()> {
        // Code-index identity is anchored on the project root's own git
        // repository (`IndexingIdentityV1::resolve` uses `gix::open` on the
        // root, no upward discovery). A non-git project has no code-index
        // identity by design: skip mounting instead of failing project open —
        // every non-code-index surface stays available.
        let git_control = project_root.join(".git");
        if !git_control.is_dir() && !git_control.is_file() {
            tracing::warn!(
                event = "code_index_mount",
                outcome = "skipped",
                project = %project_root.display(),
                reason = "missing project-root .git control path",
                "project root is not a git repository; code index disabled"
            );
            return Ok(());
        }
        let canonical_project_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let scoped_code_index_store_root = code_index_scheduler::scoped_code_index_store_root(
            &store_root,
            &canonical_project_root,
        );
        let semantic_schedule = semantic_runtime
            .zip(semantic_database)
            .zip(semantic_lifecycle)
            .zip(semantic_resources)
            .zip(code_index_scheduler::identity::worktree_id_for(project_root).ok())
            .map(
                |((((handle, database), lifecycle), resources), worktree_id)| {
                    crate::application::semantic_runtime::production_saved_generation_schedule_hook(
                        project_root.to_path_buf(),
                        scoped_code_index_store_root.clone(),
                        worktree_id,
                        handle.clone(),
                        database,
                        lifecycle,
                        resources,
                        self.semantic_projection_scheduler.clone(),
                    )
                },
            );
        self.code_index_schedulers
            .mount_worktree(project_root, store_root, semantic_schedule)
            .await
            .map(|_| ())
            .map_err(|error| TraceDecayError::Config {
                message: format!("code-index scheduler could not be mounted: {error}"),
            })
    }

    async fn shutdown(&self) {
        self.github_credential_lifecycle.shutdown();
        self.code_index_schedulers.shutdown().await;
        self.lsp_session_registry.lock().await.expire_at(u64::MAX);
        self.service.expire_all().await;
    }

    async fn invoke_for_project(
        &self,
        store_administration: &StoreAdministration,
        project_path: Option<&Path>,
        request: DaemonInvocationRequest,
    ) -> DaemonInvocationResponse {
        let request_project_path = request.requires_project().then_some(project_path).flatten();
        let root = request_project_path.and_then(admitted_lsp_root_for_project_path);
        let git_service = if invocation_is_git_operation(request.operation()) {
            git_service_for_project_path(store_administration, request_project_path).await
        } else {
            None
        };
        self.service
            .invoke(
                &self.lsp_session_registry,
                request_project_path,
                root,
                git_service,
                request,
            )
            .await
    }
}

#[derive(Clone)]
struct InProcessDaemonInvocationExecutor {
    invocation: DaemonInvocationState,
    store_administration: StoreAdministration,
    project_path: PathBuf,
}

impl InProcessDaemonInvocationExecutor {
    fn new(
        invocation: DaemonInvocationState,
        store_administration: StoreAdministration,
        project_path: PathBuf,
    ) -> Self {
        Self {
            invocation,
            store_administration,
            project_path,
        }
    }

    async fn invoke_once(&self, request: DaemonInvocationRequest) -> DaemonInvocationResponse {
        self.invocation
            .invoke_for_project(
                &self.store_administration,
                Some(&self.project_path),
                request,
            )
            .await
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for InProcessDaemonInvocationExecutor {
    fn invoke_controlled(
        &self,
        request: DaemonInvocationRequest,
        deadline: tracedecay_application::Deadline,
        cancellation: tracedecay_application::CancellationSignal,
        policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<DaemonInvocationResponse, crate::daemon_client::DaemonInvocationError>,
    > {
        Box::pin(async move {
            use tracedecay_application::CancellationStage;

            if cancellation.is_cancelled() {
                return Err(crate::daemon_client::DaemonInvocationError::Cancelled {
                    stage: CancellationStage::BeforeAdmission,
                });
            }
            let remaining = crate::daemon_client::deadline_remaining(&deadline).ok_or(
                crate::daemon_client::DaemonInvocationError::TimedOut {
                    stage: CancellationStage::BeforeAdmission,
                },
            )?;
            let executor = self.clone();
            tokio::spawn(async move {
                let stage = match policy {
                    crate::daemon_client::InvocationCancellationPolicy::ReadOnly => {
                        CancellationStage::DuringRead
                    }
                    crate::daemon_client::InvocationCancellationPolicy::AuthoritativeEffect => {
                        CancellationStage::EffectInFlight
                    }
                };
                if !policy.may_interrupt(stage) {
                    return Ok(executor.invoke_once(request).await);
                }
                let invocation = executor.invoke_once(request);
                tokio::pin!(invocation);
                let cancellation_wait = crate::daemon_client::wait_for_cancellation(cancellation);
                tokio::pin!(cancellation_wait);
                tokio::select! {
                    response = &mut invocation => Ok(response),
                    () = &mut cancellation_wait => {
                        Err(crate::daemon_client::DaemonInvocationError::Cancelled { stage })
                    }
                    () = tokio::time::sleep(remaining) => {
                        Err(crate::daemon_client::DaemonInvocationError::TimedOut { stage })
                    }
                }
            })
            .await
            .map_err(|_| crate::daemon_client::DaemonInvocationError::Unavailable)?
        })
    }

    fn observe_plan26_feedback(
        &self,
        subject_digest: tracedecay_domain::ManifestDigest,
        observed_at: tracedecay_domain::UtcMicros,
        event: crate::application::feedback::observations::Plan26FeedbackSourceEventV1,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<'_, Result<()>> {
        Box::pin(async move {
            let request_id = crate::request_identity::mint_global_request_id(
                crate::request_identity::GlobalRequestSurface::FeedbackObservation,
            )
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?;
            let response = self
                .invoke_once(DaemonInvocationRequest::feedback_observation(
                    request_id.as_str(),
                    subject_digest,
                    observed_at,
                    event,
                ))
                .await;
            if matches!(
                response.outcome,
                DaemonInvocationOutcome::ObservationAccepted
            ) {
                Ok(())
            } else {
                Err(TraceDecayError::Config {
                    message: "daemon did not accept the feedback observation".to_owned(),
                })
            }
        })
    }
}

fn invocation_is_git_operation(operation: service::invocation::DaemonInvocationOperation) -> bool {
    matches!(
        operation,
        service::invocation::DaemonInvocationOperation::GitStatus
            | service::invocation::DaemonInvocationOperation::GitDiff
            | service::invocation::DaemonInvocationOperation::GitHistory
            | service::invocation::DaemonInvocationOperation::GitBlame
            | service::invocation::DaemonInvocationOperation::GitHunks
            | service::invocation::DaemonInvocationOperation::GitPreview
            | service::invocation::DaemonInvocationOperation::GitApply
    )
}

#[cfg(unix)]
#[derive(Clone, Default)]
struct DaemonEngine {
    lifecycle: DaemonLifecycle,
    /// Closed post-handshake operations backed by daemon-owned session actors.
    /// Git and feedback remain unavailable until their authoritative request
    /// owners register daemon-minted handles; no client-side fallback exists.
    invocation: DaemonInvocationState,
    /// Project-scoped canonical application routers served by the daemon's
    /// standalone authenticated loopback HTTP listener.
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    /// Lightweight per-proxy leases keep one reconnecting client from
    /// consuming every bulk slot while preserving reserved control capacity.
    per_client_admission: DaemonPerClientAdmission,
    /// One coordinator owns the project-server registry, scheduler registry,
    /// and the writer gate that orders all mutations of either identity map.
    store_administration: StoreAdministration,
    /// Per-canonical-route gates plus a bounded, route-local warm-up task
    /// registry. Weak gates disappear after the last waiter; deterministic
    /// route failures remain only for their short retry backoff.
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    /// Per-logical-owner transition guards. Task-map locks are released before
    /// stale owners are awaited; this guard alone spans retirement so a
    /// concurrent activation or rekey cannot publish a replacement early.
    maintenance_transition_gates: Arc<tokio::sync::Mutex<MaintenanceTransitionGates>>,
    #[cfg(test)]
    project_open_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    memory_repair_start_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    automation_config_probe_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    automation_configured_override: Arc<AtomicBool>,
    #[cfg(test)]
    automation_scheduler_exit_barrier:
        Arc<tokio::sync::Mutex<Option<Arc<scheduler::AutomationSchedulerExitBarrier>>>>,
    #[cfg(test)]
    automation_scheduler_state_changed: Arc<tokio::sync::Notify>,
    /// Client versions whose skew was already logged. Proxy clients reconnect
    /// per request, so without this the mismatch would flood the daemon log.
    logged_client_version_skews: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Client processes already told to refresh their tool catalog during
    /// this daemon generation. The set is process-local by design: a daemon
    /// restart creates a new generation and permits one fresh notification.
    catalog_refresh_notified_clients: Arc<tokio::sync::Mutex<HashSet<CatalogRefreshClientKey>>>,
    /// Prevents capacity exhaustion from flooding the daemon log.
    catalog_refresh_saturation_logged: Arc<AtomicBool>,
    /// Git-metadata watcher (design D3/D5). Default-constructed inert; the real
    /// config-driven watcher is installed by `run_foreground_unix` via
    /// [`DaemonEngine::with_git_watcher`] before the accept loop starts.
    git_watcher: git_watch::GitWatcher,
    /// PR reconciliation task, retained so shutdown never leaves it writing.
    pr_autotrack_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

/// Retain one daemon-owned Git index transaction service for the project store
/// and reconcile any durable records before mutation owners become available.
/// Read-only core tools and edit previews do not depend on this service. The
/// service owns the store actor; constructing a second service for the same
/// database is rejected by the registry.
async fn ensure_git_index_transactions_for_mutation_owners(
    store_administration: &StoreAdministration,
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    project_root: &Path,
    project_id: Option<&str>,
) -> Result<()> {
    let Some(project_id) = project_id else {
        // Linked/anonymous project opens without a durable project id cannot
        // own index-mutation authority; skip rather than invent an identity.
        return Ok(());
    };
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("git index transaction project identity is invalid: {error}"),
        }
    })?;
    let Some(repository_root) = crate::worktree::git_worktree_root(project_root) else {
        // Non-Git projects remain valid TraceDecay projects. They advertise no
        // Git mutation authority and must not fail project-open admission.
        return Ok(());
    };
    let observed_at = tracedecay_domain::UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
        )
        .unwrap_or(i64::MAX),
    );
    store_administration
        .git_index_transaction_services()
        .ensure(session_db, repository_root, project_id, observed_at)
        .await
        .map(|_| ())
        .map_err(|error| TraceDecayError::Config {
            message: format!("git index transaction startup did not complete: {error}"),
        })
}

fn ensure_context_scout_owner_before_advertising(
    project: &crate::tracedecay::TraceDecay,
) -> Result<()> {
    if project.store_layout().identity.project_id.is_none() {
        return Ok(());
    }
    let owner = project
        .context_scout_owner()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project Context Scout owner did not start".to_owned(),
        })?;
    if matches!(
        owner.startup_outcome(),
        crate::agents::context_scout_v2::ContextScoutDurableStartupOutcomeV1::Unavailable
    ) {
        return Err(TraceDecayError::Config {
            message: "project Context Scout durable owner is unavailable".to_owned(),
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProjectServerKey {
    owner: StoreOwnerKey,
    scope_prefix: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct StoreOwnerKey {
    profile_root: PathBuf,
    global_db_path: PathBuf,
    project_id: Option<String>,
    store_root: PathBuf,
    graph_db_path: PathBuf,
}

/// A client route known before any project database is opened. This is the
/// cache/singleflight key; [`ProjectServerKey`] remains the post-open physical
/// owner key so linked aliases and branch DBs still converge correctly.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProjectRouteKey {
    profile_root: PathBuf,
    global_db_path: PathBuf,
    project_path: PathBuf,
    scope_prefix: Option<String>,
}

type ProjectOpenGate = tokio::sync::Mutex<()>;
#[derive(Default)]
struct ProjectOpenGates {
    gates: HashMap<ProjectRouteKey, std::sync::Weak<ProjectOpenGate>>,
    tasks: ProjectOpenTasks,
}
#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
type MaintenanceTransitionGate = tokio::sync::Mutex<()>;
#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
type MaintenanceTransitionGates =
    HashMap<MaintenanceTransitionKey, std::sync::Weak<MaintenanceTransitionGate>>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
struct MaintenanceTransitionKey {
    profile_root: PathBuf,
    project_id: Option<String>,
    scope_prefix: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
enum MaintenanceRekeyOutcome {
    Completed,
    Retiring,
}

/// Route-local project-open work. A route owns at most one task, and
/// deterministic configuration failures retain a short backoff record so a
/// reconnecting MCP host cannot repeatedly reopen the same rejected store.
#[derive(Clone, Default)]
struct ProjectOpenTasks {
    registry: Arc<tokio::sync::Mutex<ProjectOpenTaskRegistry>>,
}

#[derive(Default)]
struct ProjectOpenTaskRegistry {
    routes: HashMap<ProjectRouteKey, ProjectOpenTaskEntry>,
}

struct ProjectOpenTaskEntry {
    state: tokio::sync::watch::Receiver<ProjectOpenTaskState>,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Clone)]
enum ProjectOpenTaskState {
    Opening,
    Ready,
    Failed(ProjectOpenFailure),
}

#[derive(Clone)]
struct ProjectOpenFailure {
    message: String,
    retry_at: Option<Instant>,
}

enum ProjectOpenTaskClaim {
    InFlight(tokio::sync::watch::Receiver<ProjectOpenTaskState>),
    Failed(ProjectOpenFailure),
    Saturated,
}

/// Whether the authority audit failed because it could not read the database,
/// rather than because it judged what it read.
///
/// These are the only failures under that audit whose answer can differ on the
/// next open without anything being repaired.
fn is_database_read_failure(message: &str) -> bool {
    const DRIVER_FAILURES: [&str; 5] = [
        "database is locked",
        "database is busy",
        "disk I/O error",
        "unable to open database file",
        "interrupted",
    ];
    DRIVER_FAILURES
        .iter()
        .any(|failure| message.contains(failure))
}

/// How long a failed project-open route declines reopening, or `None` when the
/// failure may clear on its own.
fn project_open_retry_backoff(error: &TraceDecayError) -> Option<Duration> {
    match error {
        TraceDecayError::Config { message } => (message.contains("identity cutover conflict")
            || message.contains("ambiguous legacy profile stores")
            || message.contains("enrollment marker did not resolve a profile store"))
        .then_some(PROJECT_OPEN_FAILURE_RETRY_BACKOFF),
        // This audit's whole job is to read persisted rows and judge them, so
        // its verdict is a property of the stored data: a row rejected now is
        // rejected identically 250ms from now. Back off for the whole family
        // and name the exceptions, rather than listing the failures that
        // deserve a backoff — that ordering meant every newly surfaced
        // invariant message spun warm-up at the debounce cadence until someone
        // noticed the CPU. Decode failures and column-versus-JSON
        // disagreements both land here without being enumerated.
        TraceDecayError::Database { message, operation } => {
            if operation != "ensure global database authority invariants" {
                return None;
            }
            if is_database_read_failure(message) {
                return None;
            }
            // A migration still in flight can be what leaves these mutable.
            if message.contains("session temporal receipts or cursor keys are mutable") {
                return Some(PROJECT_OPEN_FAILURE_RETRY_BACKOFF);
            }
            Some(PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF)
        }
        _ => None,
    }
}

impl ProjectOpenFailure {
    fn from_error(error: &TraceDecayError) -> Self {
        // Operator-repairable authority rejections decline implicit repair.
        // Reopening before maintenance changes that state is not useful and
        // only multiplies daemon warm-up tasks.
        let retry_at = project_open_retry_backoff(error).map(|backoff| Instant::now() + backoff);
        Self {
            message: error.to_string(),
            retry_at,
        }
    }

    fn is_backed_off(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|retry_at| retry_at > now)
    }

    fn to_error(&self) -> TraceDecayError {
        let message = match self.retry_at {
            Some(retry_at) => format!(
                "{PROJECT_OPEN_FAILURE_RETRY_HINT}; retry after {} ms: {}",
                retry_at
                    .saturating_duration_since(Instant::now())
                    .as_millis(),
                self.message
            ),
            None => self.message.clone(),
        };
        TraceDecayError::Config { message }
    }
}

impl ProjectOpenTaskRegistry {
    fn prune(&mut self, now: Instant) {
        self.routes.retain(|_, entry| {
            let state = entry.state.borrow().clone();
            match state {
                ProjectOpenTaskState::Opening | ProjectOpenTaskState::Ready => {
                    !entry.task.is_finished()
                }
                ProjectOpenTaskState::Failed(failure) => {
                    !entry.task.is_finished() || failure.is_backed_off(now)
                }
            }
        });
    }
}

impl ProjectOpenTasks {
    #[cfg(test)]
    async fn start<OpenFuture>(
        &self,
        route: ProjectRouteKey,
        open: OpenFuture,
    ) -> ProjectOpenTaskClaim
    where
        OpenFuture: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        self.start_cancellable(route, |_| open).await
    }

    async fn start_cancellable<OpenOperation, OpenFuture>(
        &self,
        route: ProjectRouteKey,
        open: OpenOperation,
    ) -> ProjectOpenTaskClaim
    where
        OpenOperation: FnOnce(CancellationToken) -> OpenFuture + Send + 'static,
        OpenFuture: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let now = Instant::now();
        let mut registry = self.registry.lock().await;
        registry.prune(now);
        if let Some(entry) = registry.routes.get(&route) {
            return match entry.state.borrow().clone() {
                ProjectOpenTaskState::Failed(failure) => ProjectOpenTaskClaim::Failed(failure),
                ProjectOpenTaskState::Opening | ProjectOpenTaskState::Ready => {
                    ProjectOpenTaskClaim::InFlight(entry.state.clone())
                }
            };
        }
        if registry.routes.len() >= MAX_TRACKED_PROJECT_OPEN_TASKS {
            return ProjectOpenTaskClaim::Saturated;
        }

        let (updates, state) = tokio::sync::watch::channel(ProjectOpenTaskState::Opening);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            let state = match open(task_cancellation).await {
                Ok(()) => ProjectOpenTaskState::Ready,
                Err(error) => ProjectOpenTaskState::Failed(ProjectOpenFailure::from_error(&error)),
            };
            updates.send_replace(state);
        });
        registry.routes.insert(
            route,
            ProjectOpenTaskEntry {
                state: state.clone(),
                cancellation,
                task,
            },
        );
        ProjectOpenTaskClaim::InFlight(state)
    }

    async fn cached_failure(&self, route: &ProjectRouteKey) -> Option<ProjectOpenFailure> {
        let now = Instant::now();
        let mut registry = self.registry.lock().await;
        registry.prune(now);
        let entry = registry.routes.get(route)?;
        match entry.state.borrow().clone() {
            ProjectOpenTaskState::Failed(failure) if failure.is_backed_off(now) => Some(failure),
            ProjectOpenTaskState::Opening
            | ProjectOpenTaskState::Ready
            | ProjectOpenTaskState::Failed(_) => None,
        }
    }

    #[cfg(test)]
    async fn wait_for_completion(
        mut state: tokio::sync::watch::Receiver<ProjectOpenTaskState>,
    ) -> Result<()> {
        loop {
            let current = state.borrow().clone();
            match current {
                ProjectOpenTaskState::Opening => {
                    state.changed().await.map_err(|_| TraceDecayError::Config {
                        message: "project open task ended before reporting an outcome".to_string(),
                    })?;
                }
                ProjectOpenTaskState::Ready => return Ok(()),
                ProjectOpenTaskState::Failed(failure) => return Err(failure.to_error()),
            }
        }
    }

    async fn shutdown(&self) -> bool {
        self.shutdown_with_deadline(DAEMON_TASK_ABORT_DEADLINE, DAEMON_TASK_ABORT_DEADLINE)
            .await
    }

    async fn shutdown_with_deadline(
        &self,
        cooperative_deadline: Duration,
        post_abort_deadline: Duration,
    ) -> bool {
        let mut entries = {
            let mut registry = self.registry.lock().await;
            std::mem::take(&mut registry.routes)
        }
        .into_values()
        .collect::<Vec<_>>();
        for entry in &entries {
            entry.cancellation.cancel();
        }
        let cooperative_deadline = tokio::time::Instant::now() + cooperative_deadline;
        let mut drained = true;
        for entry in &mut entries {
            if tokio::time::timeout_at(cooperative_deadline, &mut entry.task)
                .await
                .is_err()
            {
                drained = false;
                entry.task.abort();
            }
        }
        if !drained {
            let post_abort_deadline = tokio::time::Instant::now() + post_abort_deadline;
            for entry in &mut entries {
                if entry.task.is_finished() {
                    continue;
                }
                let _ = tokio::time::timeout_at(post_abort_deadline, &mut entry.task).await;
            }
        }
        if !drained {
            log_daemon_event(
                "project_server_warmup",
                &[("outcome", "shutdown_abort_timeout".to_string())],
            );
        }
        drained
    }

    #[cfg(test)]
    async fn tracked_task_count(&self) -> usize {
        let mut registry = self.registry.lock().await;
        registry.prune(Instant::now());
        registry
            .routes
            .values()
            .filter(|entry| !entry.task.is_finished())
            .count()
    }

    #[cfg(test)]
    async fn tracked_route_count(&self) -> usize {
        let mut registry = self.registry.lock().await;
        registry.prune(Instant::now());
        registry.routes.len()
    }
}

/// Scope-specific MCP servers routed through one canonical physical DB owner.
/// `Database` performs the actual same-process handle sharing; this registry
/// keeps daemon cache aliases and branch-drift rekeys consistent with it.
struct DatabaseOwnerEntry<Server> {
    server: Server,
    last_used: Instant,
    ready: bool,
}

struct DatabaseOwnerRegistry<Server = Arc<crate::mcp::McpServer>> {
    servers: HashMap<ProjectServerKey, DatabaseOwnerEntry<Server>>,
    aliases: HashMap<ProjectRouteKey, ProjectServerKey>,
    synchronous_health: HashSet<StoreOwnerKey>,
}

impl<Server> Default for DatabaseOwnerRegistry<Server> {
    fn default() -> Self {
        Self {
            servers: HashMap::new(),
            aliases: HashMap::new(),
            synchronous_health: HashSet::new(),
        }
    }
}

impl<Server> DatabaseOwnerRegistry<Server> {
    fn get(&self, key: &ProjectServerKey) -> Option<&Server> {
        self.servers.get(key).map(|entry| &entry.server)
    }

    fn get_ready(&self, key: &ProjectServerKey) -> Option<&Server> {
        self.servers
            .get(key)
            .filter(|entry| entry.ready)
            .map(|entry| &entry.server)
    }

    fn insert(&mut self, key: ProjectServerKey, server: Server) {
        self.insert_at(key, server, Instant::now());
    }

    fn insert_at(&mut self, key: ProjectServerKey, server: Server, last_used: Instant) {
        self.servers.insert(
            key,
            DatabaseOwnerEntry {
                server,
                last_used,
                ready: true,
            },
        );
    }

    fn get_route(&self, route: &ProjectRouteKey) -> Option<(&ProjectServerKey, &Server)> {
        let key = self.aliases.get(route)?;
        let (key, entry) = self.servers.get_key_value(key)?;
        entry.ready.then_some((key, &entry.server))
    }

    fn get_route_and_touch(
        &mut self,
        route: &ProjectRouteKey,
    ) -> Option<(&ProjectServerKey, &Server)> {
        let key = self.aliases.get(route)?.clone();
        let entry = self.servers.get_mut(&key)?;
        if !entry.ready {
            return None;
        }
        entry.last_used = Instant::now();
        Some((self.aliases.get(route)?, &entry.server))
    }

    fn mark_ready(&mut self, key: &ProjectServerKey) -> bool {
        let Some(entry) = self.servers.get_mut(key) else {
            return false;
        };
        entry.ready = true;
        entry.last_used = Instant::now();
        true
    }

    fn replace_ready_if<F>(
        &mut self,
        key: &ProjectServerKey,
        replacement: Server,
        matches: F,
    ) -> bool
    where
        F: FnOnce(&Server) -> bool,
    {
        let Some(entry) = self.servers.get_mut(key) else {
            return false;
        };
        if !entry.ready || !matches(&entry.server) {
            return false;
        }
        entry.server = replacement;
        entry.last_used = Instant::now();
        true
    }

    fn remove(&mut self, key: &ProjectServerKey) -> Option<Server> {
        let entry = self.servers.remove(key)?;
        self.aliases.retain(|_, alias| alias != key);
        Some(entry.server)
    }

    fn remove_owner(&mut self, owner: &StoreOwnerKey) -> Vec<Server> {
        let keys = self
            .servers
            .keys()
            .filter(|key| &key.owner == owner)
            .cloned()
            .collect::<Vec<_>>();
        let mut removed = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(entry) = self.servers.remove(&key) {
                removed.push(entry.server);
            }
        }
        self.aliases.retain(|_, key| &key.owner != owner);
        removed
    }

    fn quarantine_and_remove_owner(&mut self, owner: &StoreOwnerKey) -> Vec<Server> {
        self.synchronous_health.insert(owner.clone());
        self.remove_owner(owner)
    }

    fn requires_synchronous_health(&self, owner: &StoreOwnerKey) -> bool {
        self.synchronous_health.contains(owner)
    }

    fn clear_synchronous_health(&mut self, owner: &StoreOwnerKey) {
        self.synchronous_health.remove(owner);
    }

    fn bind_route(&mut self, route: ProjectRouteKey, key: ProjectServerKey) {
        debug_assert!(self.servers.contains_key(&key));
        if let Some(entry) = self.servers.get_mut(&key) {
            entry.last_used = Instant::now();
        }
        self.aliases.insert(route, key);
    }

    fn insert_route(&mut self, route: ProjectRouteKey, key: ProjectServerKey, server: Server) {
        self.insert(key.clone(), server);
        self.bind_route(route, key);
    }

    fn insert_pending_route(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        server: Server,
    ) {
        self.servers.insert(
            key.clone(),
            DatabaseOwnerEntry {
                server,
                last_used: Instant::now(),
                ready: false,
            },
        );
        self.aliases.insert(route, key);
    }

    #[allow(dead_code)] // in-flight daemon route binding — staged
    fn bind_or_insert_route(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        candidate: Server,
    ) -> (Server, bool)
    where
        Server: Clone,
    {
        if let Some(existing) = self.get(&key).cloned() {
            self.bind_route(route, key);
            return (existing, false);
        }
        self.insert_route(route, key, candidate.clone());
        (candidate, true)
    }

    fn bind_or_insert_route_bounded<F>(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        candidate: Server,
        capacity: usize,
        mut is_leased: F,
    ) -> Option<(Server, bool)>
    where
        Server: Clone,
        F: FnMut(&Server) -> bool,
    {
        if let Some(existing) = self.servers.get_mut(&key) {
            if existing.ready {
                let server = existing.server.clone();
                self.bind_route(route, key);
                return Some((server, false));
            }
            existing.server = candidate.clone();
            existing.last_used = Instant::now();
            self.aliases.insert(route, key);
            return Some((candidate, true));
        }
        while self.servers.len() >= capacity {
            let evict = self
                .servers
                .iter()
                .filter(|(_, entry)| !is_leased(&entry.server))
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())?;
            self.servers.remove(&evict);
            self.aliases.retain(|_, key| key != &evict);
        }
        self.insert_pending_route(route, key, candidate.clone());
        Some((candidate, true))
    }

    fn rekey(&mut self, old: &ProjectServerKey, new: &ProjectServerKey) -> bool {
        if old == new {
            return true;
        }
        let Some(server) = self.servers.remove(old) else {
            return false;
        };
        if self.servers.contains_key(new) {
            self.aliases.retain(|_, key| key != old);
            return false;
        }
        self.servers.insert(new.clone(), server);
        for key in self.aliases.values_mut() {
            if key == old {
                *key = new.clone();
            }
        }
        true
    }

    fn values(&self) -> impl Iterator<Item = &Server> {
        self.servers.values().map(|entry| &entry.server)
    }

    fn keys(&self) -> impl Iterator<Item = &ProjectServerKey> {
        self.servers.keys()
    }
}

fn project_server_capacity_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "daemon project server capacity reached (capacity={MAX_CACHED_PROJECT_SERVERS}); retry after active clients finish"
        ),
    }
}

fn project_open_task_capacity_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "daemon project open task capacity reached (capacity={MAX_TRACKED_PROJECT_OPEN_TASKS}); retry shortly"
        ),
    }
}

fn project_open_cancellation_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: "daemon is draining during project warm-up".to_string(),
    }
}

fn project_open_cancellation_checkpoint(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(project_open_cancellation_error());
    }
    Ok(())
}

fn project_warming_error(project_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "TraceDecay project '{}' {PROJECT_WARMING_RETRY_HINT}",
            project_path.display(),
        ),
    }
}

impl StoreOwnerKey {
    fn from_paths(
        profile_root: &Path,
        global_db_path: &Path,
        project_id: Option<String>,
        store_root: &Path,
        graph_db_path: &Path,
    ) -> Result<Self> {
        Ok(Self {
            profile_root: authority::canonical_identity_path(profile_root)?,
            global_db_path: authority::canonical_identity_path(global_db_path)?,
            project_id,
            store_root: authority::canonical_identity_path(store_root)?,
            graph_db_path: authority::canonical_identity_path(graph_db_path)?,
        })
    }
}

impl ProjectRouteKey {
    fn from_handshake(project_path: &Path, handshake: &DaemonHandshake) -> Result<Self> {
        Ok(Self {
            profile_root: authority::canonical_identity_path(
                &handshake.client_identity.profile_root,
            )?,
            global_db_path: authority::canonical_identity_path(
                &handshake.client_identity.global_db_path,
            )?,
            project_path: authority::canonical_identity_path(project_path)?,
            scope_prefix: handshake.scope_prefix.clone(),
        })
    }
}

fn project_route_for_handshake(handshake: &DaemonHandshake) -> Result<(PathBuf, ProjectRouteKey)> {
    let Some(project_path) = handshake.project_path.as_ref() else {
        return Err(TraceDecayError::Config {
            message: "project server requested without project_path".to_string(),
        });
    };
    let canonical_project_path = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.clone());
    let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake)?;
    Ok((canonical_project_path, route))
}

async fn bind_authenticated_profile_identity(
    handshake: &mut DaemonHandshake,
    store_administration: &StoreAdministration,
) -> Result<StoreAdministration> {
    let profile_root = authority::canonical_identity_path(&handshake.client_identity.profile_root)?;
    let profile_identity = profile_identity::load_or_create(&profile_root)?;
    let scoped_administration = store_administration
        .clone()
        .with_profile_identity(profile_identity);
    let profile_database = scoped_administration.registered_profile_database().await?;
    let global_db_path = authority::canonical_identity_path(profile_database.db_path())?;
    let supplied_global_db_path =
        authority::canonical_identity_path(&handshake.client_identity.global_db_path)?;
    if supplied_global_db_path != global_db_path {
        return Err(TraceDecayError::Config {
            message: "daemon client global database does not match its registered profile runtime"
                .to_owned(),
        });
    }
    handshake.client_identity = DaemonClientIdentity {
        profile_root,
        global_db_path,
    };
    Ok(scoped_administration)
}

async fn project_open_gate(
    gates: &tokio::sync::Mutex<ProjectOpenGates>,
    route: &ProjectRouteKey,
) -> Arc<ProjectOpenGate> {
    let mut gates = gates.lock().await;
    if let Some(gate) = gates.gates.get(route).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(ProjectOpenGate::new(()));
    gates.gates.insert(route.clone(), Arc::downgrade(&gate));
    gate
}

async fn project_open_tasks(gates: &tokio::sync::Mutex<ProjectOpenGates>) -> ProjectOpenTasks {
    gates.lock().await.tasks.clone()
}

#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
async fn maintenance_transition_gate(
    gates: &tokio::sync::Mutex<MaintenanceTransitionGates>,
    key: &ProjectServerKey,
) -> Arc<MaintenanceTransitionGate> {
    let transition_key = MaintenanceTransitionKey {
        profile_root: key.owner.profile_root.clone(),
        project_id: key.owner.project_id.clone(),
        scope_prefix: key.scope_prefix.clone(),
    };
    let mut gates = gates.lock().await;
    if let Some(gate) = gates
        .get(&transition_key)
        .and_then(std::sync::Weak::upgrade)
    {
        return gate;
    }
    let gate = Arc::new(MaintenanceTransitionGate::new(()));
    gates.insert(transition_key, Arc::downgrade(&gate));
    gate
}

#[cfg(any(not(unix), test, feature = "test-transport"))]
fn portable_database_owner_reconciler(
    store_administration: StoreAdministration,
    current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
    route_registered: Arc<AtomicBool>,
    handshake: DaemonHandshake,
) -> crate::mcp::DatabaseOwnerReconciler {
    Arc::new(move |fresh| {
        let store_administration = store_administration.clone();
        let current_key = Arc::clone(&current_key);
        let route_registered = Arc::clone(&route_registered);
        let handshake = handshake.clone();
        Box::pin(async move {
            let transition = store_administration
                .with_writer(|| async {
                    if !route_registered.load(Ordering::Acquire) {
                        return None;
                    }
                    let new_key = match ProjectServerKey::from_open_project(&fresh, &handshake) {
                        Ok(key) => key,
                        Err(error) => {
                            eprintln!(
                                "[tracedecay] failed to rekey daemon database owner: {error}"
                            );
                            return None;
                        }
                    };
                    let mut current = current_key.lock().await;
                    if *current == new_key {
                        return None;
                    }
                    let old_key = current.clone();
                    let rekeyed = store_administration
                        .project_servers()
                        .lock()
                        .await
                        .rekey(&old_key, &new_key);
                    if !rekeyed {
                        route_registered.store(false, Ordering::Release);
                    }
                    *current = new_key.clone();
                    Some((old_key.owner, new_key.owner, rekeyed))
                })
                .await;
            let Some((old_owner, new_owner, rekeyed)) = transition else {
                return;
            };
            if rekeyed
                && new_owner.project_id.is_some()
                && let Ok(database) = store_administration
                    .registered_project_session_database(fresh.project_root(), fresh.store_layout())
                    .await
            {
                store_administration
                    .session_temporal_refresh_schedulers()
                    .rekey_project(&old_owner, new_owner, database)
                    .await;
            } else {
                store_administration
                    .session_temporal_refresh_schedulers()
                    .retire_project(&old_owner)
                    .await;
            }
        })
    })
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CatalogRefreshClientKey {
    client_identity: DaemonClientIdentity,
    client_instance_id: String,
}

#[cfg(unix)]
impl CatalogRefreshClientKey {
    fn from_handshake(handshake: &DaemonHandshake) -> Self {
        Self {
            client_identity: handshake.client_identity.clone(),
            client_instance_id: handshake.client_instance_id.clone(),
        }
    }
}

impl ProjectServerKey {
    fn from_open_project(
        cg: &crate::tracedecay::TraceDecay,
        handshake: &DaemonHandshake,
    ) -> Result<Self> {
        let layout = cg.store_layout();
        Ok(Self {
            owner: StoreOwnerKey::from_paths(
                &handshake.client_identity.profile_root,
                &handshake.client_identity.global_db_path,
                layout.identity.project_id.clone(),
                &layout.data_root,
                &cg.db_path(),
            )?,
            scope_prefix: handshake.scope_prefix.clone(),
        })
    }
}

fn build_http_application_router(project_id: &str, project_path: &Path) -> Result<axum::Router> {
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("daemon HTTP project identity is invalid: {error}"),
        }
    })?;
    let handshake =
        DaemonHandshake::for_current_client(Some(project_path.to_path_buf()), None, false, false)?;
    let client = crate::daemon_client::DaemonInvocationClient::for_current(handshake)?;
    crate::application_surface::http_application_router(
        client,
        daemon_operation_event_authority(),
        project_id.clone(),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("could not mount daemon HTTP application routes: {error}"),
    })
}

fn install_http_application_cold_resolver(
    registry: &http_application::DaemonHttpApplicationRegistry,
    store_administration: StoreAdministration,
) -> Result<()> {
    registry.install_resolver(move |project_id| {
        let store_administration = store_administration.clone();
        async move {
            let database = store_administration.registered_profile_database().await?;
            let Some(context) = database
                .project_registry_context_by_id(project_id.as_str())
                .await?
            else {
                return Ok(None);
            };
            if context.project.project_id != project_id.as_str() {
                return Err(TraceDecayError::Config {
                    message: "daemon HTTP project registry identity changed".to_owned(),
                });
            }
            let registered_root = PathBuf::from(&context.project.canonical_root);
            if !registered_root.is_absolute() {
                return Err(TraceDecayError::Config {
                    message: "daemon HTTP registered project root is not absolute".to_owned(),
                });
            }
            let canonical_root =
                registered_root
                    .canonicalize()
                    .map_err(|error| TraceDecayError::Config {
                        message: format!(
                            "daemon HTTP registered project root is unavailable: {error}"
                        ),
                    })?;
            if canonical_root != registered_root {
                return Err(TraceDecayError::Config {
                    message: "daemon HTTP registered project root is not canonical".to_owned(),
                });
            }
            build_http_application_router(project_id.as_str(), &canonical_root).map(Some)
        }
    })
}

async fn mount_http_application_router(
    registry: &http_application::DaemonHttpApplicationRegistry,
    project_id: &str,
    project_path: &Path,
) -> Result<()> {
    if !registry.is_active() {
        return Ok(());
    }
    let router = build_http_application_router(project_id, project_path)?;
    registry.mount(project_id, router).await
}

#[cfg(unix)]
impl DaemonEngine {
    fn with_profile_identity(
        mut self,
        profile_identity: profile_identity::LocalProfileIdentityAuthorityV1,
    ) -> Self {
        self.invocation
            .configure_github_read_only_credentials(&profile_identity);
        self.store_administration = self
            .store_administration
            .with_profile_identity(profile_identity);
        self
    }

    fn with_http_application_registry(
        mut self,
        registry: http_application::DaemonHttpApplicationRegistry,
    ) -> Self {
        self.http_application_registry = registry;
        self
    }

    /// Installs the config-driven git-metadata watcher on this engine. Called
    /// once by `run_foreground_unix` before the accept loop.
    fn with_git_watcher(mut self, watcher: git_watch::GitWatcher) -> Self {
        self.git_watcher = watcher;
        self
    }

    async fn with_pr_autotrack_task(self, task: JoinHandle<()>) -> Self {
        *self.pr_autotrack_task.lock().await = Some(task);
        self
    }

    async fn maintenance_transition_gate(
        &self,
        key: &ProjectServerKey,
    ) -> Arc<MaintenanceTransitionGate> {
        maintenance_transition_gate(&self.maintenance_transition_gates, key).await
    }

    /// Runs destructive branch administration before any project server is
    /// opened for the request, under the daemon-wide store administration gate.
    async fn execute_branch_admin(
        &self,
        handshake: &DaemonHandshake,
        action: crate::branch::BranchAdminAction,
    ) -> Result<crate::branch::BranchAdminReport> {
        self.store_administration
            .execute_branch_admin_for_handshake(handshake, action)
            .await
    }

    /// Returns the client version to log for this handshake, once per distinct
    /// skewed version; repeat connections from the same client return `None`.
    async fn client_version_skew_to_log(&self, handshake: &DaemonHandshake) -> Option<String> {
        let skew = client_version_skew(&handshake.client_version, binary_version())?;
        let mut logged = self.logged_client_version_skews.lock().await;
        logged.insert(skew.clone()).then_some(skew)
    }

    /// Logs a `daemon_version_skew` event when this handshake's client runs a
    /// different binary version, deduped per distinct client version.
    async fn log_client_version_skew(&self, handshake: &DaemonHandshake) {
        let Some(client_version) = self.client_version_skew_to_log(handshake).await else {
            return;
        };
        let hint = version_skew_action(binary_version(), &client_version).to_string();
        log_daemon_event(
            "daemon_version_skew",
            &[
                ("daemon_version", binary_version().to_string()),
                ("client_version", client_version),
                ("hint", hint),
            ],
        );
    }

    /// Claims the one catalog-refresh notification for this client in the
    /// current daemon generation. Only proxies that already advertised the
    /// capability are eligible. `initialize` and `tools/list` mark the client
    /// current without emitting because those requests already fetch the new
    /// generation's catalog.
    async fn claim_catalog_refresh(
        &self,
        handshake: &DaemonHandshake,
        request_line: &str,
    ) -> Option<CatalogRefreshClientKey> {
        if !valid_client_instance_id(&handshake.client_instance_id) {
            return None;
        }
        let request = serde_json::from_str::<JsonRpcRequest>(request_line).ok()?;
        if request.method == HOOK_EVENT_METHOD {
            return None;
        }
        let catalog_is_current = matches!(request.method.as_str(), "initialize" | "tools/list");
        if !catalog_is_current
            && (!handshake.tool_list_changed_capable || handshake.catalog_version.is_empty())
        {
            return None;
        }
        let key = CatalogRefreshClientKey::from_handshake(handshake);
        let mut notified_clients = self.catalog_refresh_notified_clients.lock().await;
        if notified_clients.contains(&key) {
            return None;
        }
        if notified_clients.len() >= MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION {
            drop(notified_clients);
            if !self
                .catalog_refresh_saturation_logged
                .swap(true, Ordering::Relaxed)
            {
                log_daemon_event(
                    "catalog_refresh",
                    &[
                        ("outcome", "skipped".to_string()),
                        ("reason", "client_capacity_reached".to_string()),
                        (
                            "capacity",
                            MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION.to_string(),
                        ),
                    ],
                );
            }
            return None;
        }
        notified_clients.insert(key.clone());
        drop(notified_clients);
        if catalog_is_current {
            return None;
        }
        Some(key)
    }

    async fn release_catalog_refresh(&self, key: CatalogRefreshClientKey) {
        self.catalog_refresh_notified_clients
            .lock()
            .await
            .remove(&key);
    }

    async fn project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        let cancellation = CancellationToken::new();
        self.project_server_until_cancelled(handshake, &cancellation)
            .await
    }

    async fn project_server_until_cancelled(
        &self,
        handshake: &DaemonHandshake,
        cancellation: &CancellationToken,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        if let Some(server) = self.cached_project_server(handshake).await? {
            return Ok(server);
        }

        let cached = self
            .store_administration
            .with_writer_until_cancelled(cancellation, || {
                self.open_project_server_until_cancelled(handshake, cancellation)
            })
            .await
            .ok_or_else(project_open_cancellation_error)??;
        let (_key, project_path, server, _inserted) = cached;
        project_open_cancellation_checkpoint(cancellation)?;
        Ok(self.activate_project_server(project_path, server).await)
    }

    async fn cached_project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Option<Arc<crate::mcp::McpServer>>> {
        let (project_path, route) = Self::project_route(handshake)?;
        let cached = {
            let mut servers = self.store_administration.project_servers().lock().await;
            servers
                .get_route_and_touch(&route)
                .map(|(_, server)| Arc::clone(server))
        };
        let Some(server) = cached else {
            return Ok(None);
        };
        self.ensure_registered_project_route(&project_path, handshake.allow_init)
            .await?;
        Ok(Some(
            self.activate_project_server(project_path, server).await,
        ))
    }

    async fn begin_project_open(
        &self,
        handshake: DaemonHandshake,
        initialize_request: Option<JsonRpcRequest>,
    ) -> Result<ProjectOpenTaskClaim> {
        let (project_path, route) = Self::project_route(&handshake)?;
        let tasks = project_open_tasks(&self.project_open_gates).await;
        let engine = self.clone();
        let open_handshake = handshake.clone();
        Ok(Box::pin(start_lifecycle_project_open(
            &tasks,
            self.lifecycle.clone(),
            route,
            project_path,
            initialize_request,
            move |cancellation| async move {
                engine
                    .project_server_until_cancelled(&open_handshake, &cancellation)
                    .await
            },
        ))
        .await)
    }

    /// Rejects ambient working directories before scheduling project warm-up.
    ///
    /// Host MCP clients may start from `$HOME` and include that directory in
    /// their handshake. Opening it as a project would perform graph and index
    /// work before session-store resolution eventually notices the missing
    /// enrollment. Registry alias and repository-identity lookups preserve
    /// linked-worktree routing without manufacturing path-derived authority.
    async fn ensure_registered_project_route(
        &self,
        project_path: &Path,
        allow_init: bool,
    ) -> Result<()> {
        ensure_registered_project_route(&self.store_administration, project_path, allow_init).await
    }

    async fn schedule_project_server_warmup(
        &self,
        handshake: DaemonHandshake,
        initialize_request: JsonRpcRequest,
    ) -> Result<()> {
        if self.cached_project_server(&handshake).await?.is_some() {
            return Ok(());
        }
        match Box::pin(self.begin_project_open(handshake, Some(initialize_request))).await? {
            ProjectOpenTaskClaim::InFlight(_) => Ok(()),
            ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
            ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
        }
    }

    async fn project_server_for_request(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        if let Some(server) = self.cached_project_server(handshake).await? {
            return Ok(server);
        }
        let (project_path, _) = Self::project_route(handshake)?;
        // Bound only the wait behind an unrelated writer. An uncontended open
        // is this request's own work and must run to completion.
        let contended = self.store_administration.writer_is_busy();
        let claim = Box::pin(self.begin_project_open(handshake.clone(), None)).await?;
        match claim {
            ProjectOpenTaskClaim::InFlight(mut state) => {
                let publication = async {
                    loop {
                        if let Some(server) = self.cached_project_server(handshake).await? {
                            return Ok(server);
                        }
                        let current = state.borrow().clone();
                        match current {
                            ProjectOpenTaskState::Opening => {
                                tokio::select! {
                                    changed = state.changed() => {
                                        changed.map_err(|_| TraceDecayError::Config {
                                            message: "project open task ended before reporting an outcome"
                                                .to_string(),
                                        })?;
                                    }
                                    () = tokio::time::sleep(Duration::from_millis(25)) => {}
                                }
                            }
                            ProjectOpenTaskState::Ready => {
                                return Err(TraceDecayError::Config {
                                    message: "project open completed without publishing a server"
                                        .to_string(),
                                });
                            }
                            ProjectOpenTaskState::Failed(failure) => {
                                return Err(failure.to_error());
                            }
                        }
                    }
                };
                if contended {
                    timeout(PROJECT_OPEN_REQUEST_DEADLINE, publication)
                        .await
                        .map_err(|_| project_warming_error(&project_path))?
                } else {
                    publication.await
                }
            }
            ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
            ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
        }
    }

    async fn cached_project_open_failure(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Option<ProjectOpenFailure>> {
        let (_, route) = Self::project_route(handshake)?;
        let tasks = project_open_tasks(&self.project_open_gates).await;
        Ok(tasks.cached_failure(&route).await)
    }

    async fn shutdown_project_open_tasks(&self) {
        project_open_tasks(&self.project_open_gates)
            .await
            .shutdown()
            .await;
    }

    /// Opens or resolves a project server while writer administration is held.
    /// Watcher and scheduler activation happen only after this returns so those
    /// components can acquire the same coordinator without recursive locking.
    async fn open_project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<(ProjectServerKey, PathBuf, Arc<crate::mcp::McpServer>, bool)> {
        let cancellation = CancellationToken::new();
        self.open_project_server_until_cancelled(handshake, &cancellation)
            .await
    }

    async fn open_project_server_until_cancelled(
        &self,
        handshake: &DaemonHandshake,
        cancellation: &CancellationToken,
    ) -> Result<(ProjectServerKey, PathBuf, Arc<crate::mcp::McpServer>, bool)> {
        let Some(project_path) = handshake.project_path.as_ref() else {
            return Err(TraceDecayError::Config {
                message: "project server requested without project_path".to_string(),
            });
        };
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        self.ensure_registered_project_route(&canonical_project_path, handshake.allow_init)
            .await?;
        let composition = production_project_server(
            &self.store_administration,
            self.project_open_gates.as_ref(),
            &self.invocation,
            &self.http_application_registry,
            &canonical_project_path,
            handshake,
            ProductionProjectCompositionRuntime::Unix(self.clone()),
            cancellation,
            #[cfg(test)]
            Some(&self.project_open_attempts),
        )
        .await?;
        if composition.inserted {
            self.spawn_project_maintenance_activation(
                composition.key.clone(),
                composition.canonical_project_path.clone(),
                handshake.clone(),
                Arc::clone(&composition.server),
            );
        }
        Ok((
            composition.key,
            composition.canonical_project_path,
            composition.server,
            composition.inserted,
        ))
    }

    fn project_route(handshake: &DaemonHandshake) -> Result<(PathBuf, ProjectRouteKey)> {
        project_route_for_handshake(handshake)
    }

    async fn activate_project_server(
        &self,
        project_path: PathBuf,
        server: Arc<crate::mcp::McpServer>,
    ) -> Arc<crate::mcp::McpServer> {
        // A freshly-handshaken project should be watched even on a cache hit
        // (the watcher may have started after this server was cached).
        self.git_watcher.ensure_watching(&project_path).await;
        server
    }

    fn spawn_project_maintenance_activation(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
        server: Arc<crate::mcp::McpServer>,
    ) {
        let repair_key = key.clone();
        let repair_project_path = project_path.clone();
        let repair_handshake = handshake.clone();
        let engine = self.clone();
        spawn_lifecycle_automation_scheduler_activation(self.lifecycle.clone(), async move {
            engine
                .activate_project_maintenance(repair_key, repair_project_path, repair_handshake)
                .await;
        });
        let engine = self.clone();
        spawn_lifecycle_automation_scheduler_activation(self.lifecycle.clone(), async move {
            let cg = server.cg().await;
            engine
                .activate_automation_scheduler_for_open_project(key, project_path, handshake, cg)
                .await;
        });
    }

    async fn activate_project_maintenance(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) {
        let transition = self.maintenance_transition_gate(&key).await;
        let _transition = transition.lock().await;
        self.store_administration
            .with_writer(|| async move {
                if self
                    .store_administration
                    .project_servers()
                    .lock()
                    .await
                    .get(&key)
                    .is_none()
                {
                    return;
                }
                self.start_memory_repair_scheduler(
                    key.clone(),
                    project_path.clone(),
                    handshake.clone(),
                )
                .await;
            })
            .await;
    }

    async fn rekey_project_maintenance(
        &self,
        old_key: &ProjectServerKey,
        new_key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
        acquire_new: bool,
    ) -> MaintenanceRekeyOutcome {
        let transition = self.maintenance_transition_gate(old_key).await;
        let _transition = transition.lock().await;
        let repair_retirement = self.retire_memory_repair_scheduler_locked(old_key).await;
        let automation_retirement = self.retire_automation_scheduler_locked(old_key).await;
        let retired = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
            if let Some(retirement) = repair_retirement {
                retirement.wait().await;
            }
            if let Some(retirement) = automation_retirement {
                retirement.wait().await;
            }
        })
        .await
        .is_ok();
        if !retired {
            log_daemon_event(
                "maintenance_rekey",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "retirement_timeout".to_string()),
                ],
            );
            return MaintenanceRekeyOutcome::Retiring;
        }
        if !acquire_new || !self.lifecycle.accepting() {
            return MaintenanceRekeyOutcome::Completed;
        }
        let repair_outcome = self
            .reconcile_memory_repair_scheduler_locked(
                new_key.clone(),
                project_path.clone(),
                handshake.clone(),
            )
            .await;
        let automation_outcome = self
            .reconcile_automation_scheduler_locked(new_key, project_path, handshake)
            .await;
        if matches!(
            repair_outcome,
            memory_repair_scheduler::MemoryRepairSchedulerReconcileOutcome::Retiring
        ) || matches!(
            automation_outcome,
            crate::dashboard::AutomationSchedulerReconcileOutcome::Retiring
        ) {
            MaintenanceRekeyOutcome::Retiring
        } else {
            MaintenanceRekeyOutcome::Completed
        }
    }

    fn database_owner_reconciler(
        &self,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        route_registered: Arc<AtomicBool>,
        handshake: DaemonHandshake,
    ) -> crate::mcp::DatabaseOwnerReconciler {
        let engine = self.clone();
        Arc::new(move |fresh| {
            let engine = engine.clone();
            let current_key = Arc::clone(&current_key);
            let current_project_path = Arc::clone(&current_project_path);
            let route_registered = Arc::clone(&route_registered);
            let handshake = handshake.clone();
            Box::pin(async move {
                let transition = engine
                    .store_administration
                    .with_writer(|| async {
                        if !route_registered.load(Ordering::Acquire) {
                            return None;
                        }
                        let new_key = match ProjectServerKey::from_open_project(&fresh, &handshake)
                        {
                            Ok(key) => key,
                            Err(error) => {
                                eprintln!(
                                    "[tracedecay] failed to rekey daemon database owner: {error}"
                                );
                                return None;
                            }
                        };
                        let mut current = current_key.lock().await;
                        if *current == new_key {
                            return None;
                        }
                        let old_key = current.clone();
                        let rekeyed = engine
                            .store_administration
                            .project_servers()
                            .lock()
                            .await
                            .rekey(&old_key, &new_key);
                        if !rekeyed {
                            route_registered.store(false, Ordering::Release);
                        }
                        let project_path = fresh.project_root().to_path_buf();
                        let new_session_db = match new_key.owner.project_id.as_deref() {
                            Some(_) => engine
                                .store_administration
                                .registered_project_session_database(
                                    fresh.project_root(),
                                    fresh.store_layout(),
                                )
                                .await
                                .ok(),
                            None => None,
                        };
                        *current_project_path.lock().await = project_path;
                        *current = new_key.clone();
                        Some((
                            old_key,
                            new_key,
                            new_session_db,
                            fresh.project_root().to_path_buf(),
                            rekeyed,
                        ))
                    })
                    .await;
                if let Some((old_key, new_key, new_session_db, project_path, acquire_new)) =
                    transition
                {
                    let old_owner = old_key.owner.clone();
                    let new_owner = new_key.owner.clone();
                    let outcome = engine
                        .rekey_project_maintenance(
                            &old_key,
                            new_key,
                            project_path,
                            handshake,
                            acquire_new,
                        )
                        .await;
                    if outcome == MaintenanceRekeyOutcome::Completed {
                        if acquire_new
                            && engine.lifecycle.accepting()
                            && let Some(new_session_db) = new_session_db
                        {
                            engine
                                .store_administration
                                .session_temporal_refresh_schedulers()
                                .rekey_project(&old_owner, new_owner, new_session_db)
                                .await;
                        } else {
                            engine
                                .store_administration
                                .session_temporal_refresh_schedulers()
                                .retire_project(&old_owner)
                                .await;
                        }
                    }
                }
            })
        })
    }

    async fn shutdown_background_tasks(&self) {
        self.shutdown_project_open_tasks().await;
        self.invocation.shutdown().await;
        self.store_administration
            .session_temporal_refresh_schedulers()
            .shutdown()
            .await;
        self.shutdown_automation_schedulers().await;
        self.shutdown_memory_repair_schedulers().await;
        self.store_administration
            .shutdown_retirement_reapers()
            .await;
        self.store_administration
            .shutdown_host_admission_replay()
            .await;

        self.git_watcher.shutdown().await;
        if let Some(handle) = self.pr_autotrack_task.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn shutdown_servers(&self) {
        shutdown_project_servers(&self.store_administration).await;
    }

    #[cfg(test)]
    async fn shutdown_all(&self) {
        self.lifecycle.begin_draining();
        self.shutdown_background_tasks().await;
        self.shutdown_servers().await;
    }
}

async fn cancel_project_server_startup_ingests(store_administration: &StoreAdministration) {
    let servers = {
        let registry = store_administration.project_servers().lock().await;
        let mut seen = HashSet::new();
        registry
            .values()
            .filter(|server| seen.insert(Arc::as_ptr(server) as usize))
            .cloned()
            .collect::<Vec<_>>()
    };
    for server in servers {
        server.cancel_startup_transcript_ingest();
    }
}

async fn shutdown_project_servers(store_administration: &StoreAdministration) {
    store_administration.join_project_server_retirements().await;
    let servers = detach_project_servers(store_administration).await;
    shutdown_detached_project_servers(servers).await;
}

async fn detach_project_servers(
    store_administration: &StoreAdministration,
) -> Vec<Arc<crate::mcp::McpServer>> {
    let servers: Vec<Arc<crate::mcp::McpServer>> = store_administration
        .with_writer(|| async {
            let mut registry = store_administration.project_servers().lock().await;
            let mut seen = HashSet::new();
            let servers = registry
                .values()
                .filter(|server| seen.insert(Arc::as_ptr(server) as usize))
                .cloned()
                .collect();
            // Servers retain daemon callbacks that clone StoreAdministration.
            // Remove the registry's side of that cycle before awaiting server
            // shutdown so every physical store runtime can be dropped.
            registry.servers.clear();
            registry.aliases.clear();
            servers
        })
        .await;
    servers
}

async fn shutdown_detached_project_servers(servers: Vec<Arc<crate::mcp::McpServer>>) {
    for server in servers {
        let graph = server.cg().await;
        hook_v2_replay::shutdown_hook_v2_replay_consumer(&graph.hook_store_layout().data_root)
            .await;
        drop(graph);
        server.shutdown().await;
    }
}

const PROJECT_SERVER_REQUEST_DRAIN_DEADLINE: Duration = Duration::from_secs(35);
const PROJECT_SERVER_ABORT_DRAIN_DEADLINE: Duration = Duration::from_secs(2);

async fn wait_for_project_server_request_drains(servers: &[Arc<crate::mcp::McpServer>]) {
    for server in servers {
        server.wait_for_project_server_request_drain().await;
    }
}

async fn retire_project_servers(
    servers: Vec<Arc<crate::mcp::McpServer>>,
    route_registered: Option<Arc<AtomicBool>>,
) {
    if tokio::time::timeout(
        PROJECT_SERVER_REQUEST_DRAIN_DEADLINE,
        wait_for_project_server_request_drains(&servers),
    )
    .await
    .is_err()
    {
        tracing::warn!(
            deadline_secs = PROJECT_SERVER_REQUEST_DRAIN_DEADLINE.as_secs(),
            server_count = servers.len(),
            "retired project requests exceeded their drain deadline; cancelling them"
        );
        for server in &servers {
            server.abort_project_server_requests();
        }
        if tokio::time::timeout(
            PROJECT_SERVER_ABORT_DRAIN_DEADLINE,
            wait_for_project_server_request_drains(&servers),
        )
        .await
        .is_err()
        {
            tracing::warn!(
                deadline_secs = PROJECT_SERVER_ABORT_DRAIN_DEADLINE.as_secs(),
                server_count = servers.len(),
                "cancelled project requests have not yielded; retaining safe shutdown ownership"
            );
            wait_for_project_server_request_drains(&servers).await;
        }
    }
    if let Some(route_registered) = route_registered {
        route_registered.store(false, Ordering::Release);
    }
    for server in servers {
        server.shutdown().await;
    }
}

async fn schedule_project_server_retirement(
    store_administration: &StoreAdministration,
    servers: Vec<Arc<crate::mcp::McpServer>>,
    route_registered: Option<Arc<AtomicBool>>,
) {
    let retirement = tokio::spawn(retire_project_servers(servers, route_registered));
    store_administration
        .track_project_server_retirement(retirement)
        .await;
}

/// Kick coalesced per-profile replay without awaiting a pass (handshake-safe).
async fn ensure_user_profile_host_admission_replay_for_identity(
    store_administration: &StoreAdministration,
    _client_identity: &DaemonClientIdentity,
) -> Result<()> {
    let user_session_db = match store_administration
        .registered_profile_session_database()
        .await
    {
        Ok(database) => database,
        Err(error) => {
            eprintln!(
                "[tracedecay] user-profile host admission disposition: authority_unavailable: {error}"
            );
            return Ok(());
        }
    };
    let Ok(state) = store_administration
        .host_admission_broker(&user_session_db)
        .await
    else {
        eprintln!("[tracedecay] user-profile host admission disposition: authority_unavailable");
        return Ok(());
    };
    if let Some(outcome) = state.unavailable_outcome() {
        eprintln!(
            "[tracedecay] user-profile host admission disposition: {}",
            outcome.reason_code.unwrap_or("spool_unavailable")
        );
    }
    // host_admission_broker already kicks the coalesced worker for user-sessions DBs.
    Ok(())
}

#[cfg(test)]
async fn replay_user_profile_host_admission_for_identity(
    store_administration: &StoreAdministration,
    client_identity: &DaemonClientIdentity,
) -> Result<()> {
    ensure_user_profile_host_admission_replay_for_identity(store_administration, client_identity)
        .await?;
    let Ok(broker_path) = authority::canonical_identity_path(
        &crate::sessions::user_sessions_db_path(&client_identity.profile_root),
    ) else {
        return Ok(());
    };
    let _ = store_administration
        .wait_user_profile_host_admission_replay_idle(&broker_path, Duration::from_secs(5))
        .await;
    Ok(())
}

#[cfg(all(unix, test))]
async fn serve_socket_client(stream: tokio::net::UnixStream, engine: DaemonEngine) -> Result<()> {
    Box::pin(serve_broker_socket_client(
        BrokerStream::Unix(stream),
        engine,
        None,
        DaemonClientAdmissionClass::General,
    ))
    .await
}

#[cfg(unix)]
#[allow(dead_code)] // in-flight authenticated socket serving — staged
async fn serve_authenticated_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: String,
) -> Result<()> {
    Box::pin(serve_authenticated_socket_client_with_class(
        stream,
        engine,
        auth_token,
        DaemonClientAdmissionClass::General,
    ))
    .await
}

#[cfg(unix)]
async fn serve_authenticated_socket_client_with_class(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: String,
    admission_class: DaemonClientAdmissionClass,
) -> Result<()> {
    Box::pin(serve_broker_socket_client(
        stream,
        engine,
        Some(auth_token),
        admission_class,
    ))
    .await
}

async fn apply_daemon_initialize_route(
    handshake: &mut DaemonHandshake,
    first_request_line: &str,
    store_administration: &StoreAdministration,
) -> Result<Option<InitializeRouteMetadata>> {
    if !handshake.allow_initialize_root_routing {
        return Ok(None);
    }
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) else {
        return Ok(None);
    };
    if request.method != "initialize" {
        return Ok(None);
    }
    let registry = store_administration.registered_profile_database().await?;
    let Some(route) =
        resolve_daemon_initialize_route(request.params.as_ref(), Some(&registry)).await?
    else {
        return Ok(None);
    };
    if handshake.project_path.as_deref() != Some(route.project_path.as_path()) {
        handshake.scope_prefix = None;
    }
    handshake.project_path = Some(route.project_path.clone());
    handshake.allow_init = route.allow_init;
    Ok(Some(route))
}

fn attach_initialize_route_metadata(
    response: &mut JsonRpcResponse,
    route: &InitializeRouteMetadata,
) {
    let Some(result) = response.result.as_mut() else {
        return;
    };
    result["_meta"]["tracedecayInitializeRoute"] = json!(route);
}

/// Returns `None` for project-dependent requests, `Some(None)` for handled
/// notifications, and `Some(Some(response))` for static MCP bootstrap calls.
fn daemon_bootstrap_response(
    request: &JsonRpcRequest,
    route: Option<&InitializeRouteMetadata>,
    project_node_count: Option<u64>,
) -> Option<Option<JsonRpcResponse>> {
    match classify_mcp_method(&request.method) {
        McpMethod::Initialize => Some(request.id.clone().map(|id| {
            let mut response = JsonRpcResponse::success(id, initialize_result(SERVER_INSTRUCTIONS));
            if let Some(route) = route {
                attach_initialize_route_metadata(&mut response, route);
            }
            response
        })),
        McpMethod::InitializedAck => Some(None),
        McpMethod::ToolsList => Some(request.id.clone().map(|id| {
            let budget = explore_call_budget(project_node_count.unwrap_or(0));
            let profile_id = tracedecay_tool_catalog::ProfileId::new(
                tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID,
            );
            let authority = default_catalog_discovery_authority();
            match (profile_id, authority) {
                (Ok(profile_id), Ok(authority)) => {
                    let definitions = match project_node_count {
                        Some(node_count) => get_catalog_filtered_tool_definitions_with_budget(
                            node_count,
                            budget,
                            &profile_id,
                            &authority,
                            &project_catalog_discovery_scope(),
                            ToolRegistryMode::HostAvailable,
                        ),
                        None => get_catalog_filtered_tool_definitions_with_warming_budget(
                            budget,
                            &profile_id,
                            &authority,
                            &project_catalog_discovery_scope(),
                            ToolRegistryMode::HostAvailable,
                        ),
                    };
                    match definitions {
                        Ok(tools) => JsonRpcResponse::success(id, json!({ "tools": tools })),
                        Err(_) => JsonRpcResponse::error(
                            id,
                            ErrorCode::InternalError,
                            "MCP catalog discovery unavailable".to_owned(),
                        ),
                    }
                }
                _ => JsonRpcResponse::error(
                    id,
                    ErrorCode::InternalError,
                    "MCP catalog discovery unavailable".to_owned(),
                ),
            }
        })),
        _ => None,
    }
}

async fn cached_project_node_count(
    store_administration: &StoreAdministration,
    handshake: &DaemonHandshake,
) -> Option<u64> {
    let project_path = handshake.project_path.as_ref()?;
    let canonical_project_path = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.clone());
    let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake).ok()?;
    let server = {
        let servers = store_administration.project_servers().lock().await;
        servers
            .get_route(&route)
            .map(|(_, server)| Arc::clone(server))
    }?;
    ensure_registered_project_route(
        store_administration,
        &canonical_project_path,
        handshake.allow_init,
    )
    .await
    .ok()?;
    server
        .cg()
        .await
        .get_stats()
        .await
        .ok()
        .map(|stats| stats.node_count)
}

async fn start_lifecycle_project_open<OpenOperation, OpenFuture>(
    tasks: &ProjectOpenTasks,
    lifecycle: DaemonLifecycle,
    route: ProjectRouteKey,
    project_path: PathBuf,
    initialize_request: Option<JsonRpcRequest>,
    open_project_server: OpenOperation,
) -> ProjectOpenTaskClaim
where
    OpenOperation: FnOnce(CancellationToken) -> OpenFuture + Send + 'static,
    OpenFuture: std::future::Future<Output = Result<Arc<crate::mcp::McpServer>>> + Send + 'static,
{
    if !lifecycle.accepting() {
        return ProjectOpenTaskClaim::Failed(ProjectOpenFailure {
            message: "daemon is draining before project warm-up".to_string(),
            retry_at: None,
        });
    }
    tasks
        .start_cancellable(route, move |cancellation| async move {
            let Some(activity) = lifecycle.try_enter() else {
                return Err(TraceDecayError::Config {
                    message: "daemon is draining before project warm-up".to_string(),
                });
            };
            let _activity = activity;
            // Once admitted, warm-up may be inside a schema migration. The
            // cancellation token is observed only at explicit boundaries around
            // those transactionally safe units; dropping this future on drain
            // would untrack the database owner and can interrupt SQLite
            // mid-statement. The lifecycle activity remains held until the task
            // reports its terminal outcome and shutdown explicitly joins it.
            let result = Box::pin(open_project_server(cancellation.clone())).await;
            match result {
                Ok(server) => {
                    project_open_cancellation_checkpoint(&cancellation)?;
                    if let Some(initialize_request) = initialize_request {
                        // Preserve the regular initialize side effect that records
                        // the negotiated MCP client name on the real server.
                        let initialize: std::pin::Pin<
                            Box<
                                dyn std::future::Future<Output = Option<JsonRpcResponse>>
                                    + Send
                                    + '_,
                            >,
                        > = Box::pin(server.handle_request(&initialize_request));
                        let _ = initialize.await;
                    }
                    Ok(())
                }
                Err(error) => {
                    if cancellation.is_cancelled() {
                        return Err(error);
                    }
                    log_daemon_event(
                        "project_server_warmup",
                        &[
                            ("outcome", "error".to_string()),
                            ("project", project_path.display().to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                    Err(error)
                }
            }
        })
        .await
}

#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
fn spawn_lifecycle_automation_scheduler_activation<ActivationFuture>(
    lifecycle: DaemonLifecycle,
    activation: ActivationFuture,
) where
    ActivationFuture: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(activity) = lifecycle.try_enter() else {
        return;
    };
    tokio::spawn(async move {
        let _activity = activity;
        tokio::select! {
            biased;
            () = lifecycle.wait_for_draining() => {}
            () = activation => {}
        }
    });
}

async fn ensure_registered_project_route(
    store_administration: &StoreAdministration,
    project_path: &Path,
    allow_init: bool,
) -> Result<()> {
    let registry = store_administration.registered_profile_database().await?;
    let context = match registry
        .project_registry_context_by_alias(project_path)
        .await?
    {
        Some(context) => Some(context),
        None => {
            let git_root = crate::worktree::git_worktree_root(project_path)
                .unwrap_or_else(|| project_path.to_path_buf());
            let git_common_dir = crate::worktree::git_common_dir(&git_root);
            registry
                .project_registry_context_by_identity(&git_root, git_common_dir.as_deref())
                .await?
        }
    };
    if context.is_none() {
        let project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.to_path_buf());
        let is_project_root = crate::worktree::git_worktree_root(&project_path)
            .is_none_or(|git_root| git_root == project_path);
        let owns_repository_identity =
            crate::worktree::repository_identity_root(&project_path).is_none();
        if allow_init && is_project_root && owns_repository_identity {
            return Ok(());
        }
        return Err(unenrolled_project_route_error(&project_path));
    }
    Ok(())
}

fn unenrolled_project_route_error(project_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "no TraceDecay index found at '{}': project is not enrolled in the authenticated \
             profile; run 'tracedecay init' first",
            project_path.display()
        ),
    }
}

#[cfg(any(not(unix), test))]
async fn portable_cached_project_server(
    store_administration: &StoreAdministration,
    canonical_project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<Option<Arc<crate::mcp::McpServer>>> {
    let route = ProjectRouteKey::from_handshake(canonical_project_path, handshake)?;
    let server = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch(&route)
            .map(|(_, server)| Arc::clone(server))
    };
    let Some(server) = server else {
        return Ok(None);
    };
    ensure_registered_project_route(
        store_administration,
        canonical_project_path,
        handshake.allow_init,
    )
    .await?;
    Ok(Some(server))
}

#[cfg(any(not(unix), test))]
// Cohesive route-open context; a params struct would only move the same ownership bundle.
#[allow(clippy::too_many_arguments)]
async fn begin_portable_project_open(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    handshake: DaemonHandshake,
    canonical_project_path: PathBuf,
    route: ProjectRouteKey,
    initialize_request: Option<JsonRpcRequest>,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> ProjectOpenTaskClaim {
    let tasks = project_open_tasks(project_open_gates.as_ref()).await;
    let open_project_path = canonical_project_path.clone();
    let open_gates = Arc::clone(&project_open_gates);
    Box::pin(start_lifecycle_project_open(
        &tasks,
        lifecycle,
        route,
        canonical_project_path,
        initialize_request,
        move |cancellation| async move {
            store_administration
                .with_writer_until_cancelled(&cancellation, || async {
                    production_project_server(
                        &store_administration,
                        open_gates.as_ref(),
                        &invocation,
                        &http_application_registry,
                        &open_project_path,
                        &handshake,
                        ProductionProjectCompositionRuntime::Portable {
                            semantic_auto_download: true,
                            startup_catch_up: true,
                        },
                        &cancellation,
                        #[cfg(test)]
                        project_open_attempts.as_ref(),
                    )
                    .await
                    .map(|composition| composition.server)
                })
                .await
                .ok_or_else(project_open_cancellation_error)?
        },
    ))
    .await
}

#[cfg(any(not(unix), test))]
async fn schedule_portable_project_server_warmup(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    handshake: DaemonHandshake,
    initialize_request: JsonRpcRequest,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    let (canonical_project_path, route) = project_route_for_handshake(&handshake)?;
    if portable_cached_project_server(&store_administration, &canonical_project_path, &handshake)
        .await?
        .is_some()
    {
        return Ok(());
    }
    match Box::pin(begin_portable_project_open(
        lifecycle,
        store_administration,
        project_open_gates,
        invocation,
        http_application_registry,
        handshake,
        canonical_project_path,
        route,
        Some(initialize_request),
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
    {
        ProjectOpenTaskClaim::InFlight(_) => Ok(()),
        ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
        ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
    }
}

#[cfg(any(not(unix), test))]
async fn portable_project_server_for_request(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    handshake: &DaemonHandshake,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<Arc<crate::mcp::McpServer>> {
    let (canonical_project_path, route) = project_route_for_handshake(handshake)?;
    if let Some(server) =
        portable_cached_project_server(&store_administration, &canonical_project_path, handshake)
            .await?
    {
        return Ok(server);
    }
    // Match the Unix path: only a request queued behind an unrelated writer
    // gets the retry deadline.
    let contended = store_administration.writer_is_busy();
    let claim = Box::pin(begin_portable_project_open(
        lifecycle,
        store_administration.clone(),
        project_open_gates,
        invocation,
        http_application_registry,
        handshake.clone(),
        canonical_project_path.clone(),
        route,
        None,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await;
    match claim {
        ProjectOpenTaskClaim::InFlight(mut state) => {
            let publication = async {
                loop {
                    if let Some(server) = portable_cached_project_server(
                        &store_administration,
                        &canonical_project_path,
                        handshake,
                    )
                    .await?
                    {
                        return Ok(server);
                    }
                    let current = state.borrow().clone();
                    match current {
                        ProjectOpenTaskState::Opening => {
                            tokio::select! {
                                changed = state.changed() => {
                                    changed.map_err(|_| TraceDecayError::Config {
                                        message: "project open task ended before reporting an outcome"
                                            .to_string(),
                                    })?;
                                }
                                () = tokio::time::sleep(Duration::from_millis(25)) => {}
                            }
                        }
                        ProjectOpenTaskState::Ready => {
                            return Err(TraceDecayError::Config {
                                message: "project open completed without publishing a server"
                                    .to_string(),
                            });
                        }
                        ProjectOpenTaskState::Failed(failure) => {
                            return Err(failure.to_error());
                        }
                    }
                }
            };
            if contended {
                timeout(PROJECT_OPEN_REQUEST_DEADLINE, publication)
                    .await
                    .map_err(|_| project_warming_error(&canonical_project_path))?
            } else {
                publication.await
            }
        }
        ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
        ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
    }
}

#[cfg(any(not(unix), test))]
async fn portable_cached_project_open_failure(
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
    handshake: &DaemonHandshake,
) -> Result<Option<ProjectOpenFailure>> {
    let (_, route) = project_route_for_handshake(handshake)?;
    let tasks = project_open_tasks(project_open_gates).await;
    Ok(tasks.cached_failure(&route).await)
}

#[cfg(not(unix))]
async fn shutdown_portable_project_open_tasks(
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
) {
    project_open_tasks(project_open_gates)
        .await
        .shutdown()
        .await;
}

#[derive(Clone)]
enum ProductionProjectCompositionRuntime {
    #[cfg(unix)]
    Unix(DaemonEngine),
    #[cfg(any(not(unix), test, feature = "test-transport"))]
    Portable {
        semantic_auto_download: bool,
        startup_catch_up: bool,
    },
}

impl ProductionProjectCompositionRuntime {
    fn database_owner_reconciler(
        &self,
        _store_administration: &StoreAdministration,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        _current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        route_registered: Arc<AtomicBool>,
        handshake: DaemonHandshake,
    ) -> crate::mcp::DatabaseOwnerReconciler {
        match self {
            #[cfg(unix)]
            Self::Unix(engine) => engine.database_owner_reconciler(
                current_key,
                _current_project_path,
                route_registered,
                handshake,
            ),
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable { .. } => portable_database_owner_reconciler(
                _store_administration.clone(),
                current_key,
                route_registered,
                handshake,
            ),
        }
    }

    fn automation_scheduler_reconciler(
        &self,
        _current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        _current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        _handshake: DaemonHandshake,
    ) -> Option<crate::dashboard::AutomationSchedulerReconciler> {
        match self {
            #[cfg(unix)]
            Self::Unix(engine) => Some(engine.automation_scheduler_reconciler(
                _current_key,
                _current_project_path,
                _handshake,
            )),
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable { .. } => None,
        }
    }

    const fn semantic_auto_download(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => true,
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable {
                semantic_auto_download,
                ..
            } => *semantic_auto_download,
        }
    }

    const fn startup_catch_up(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => true,
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable {
                startup_catch_up, ..
            } => *startup_catch_up,
        }
    }
}

struct ProductionProjectComposition {
    key: ProjectServerKey,
    canonical_project_path: PathBuf,
    server: Arc<crate::mcp::McpServer>,
    inserted: bool,
    semantic_auto_download_enabled: Option<bool>,
}

#[cfg(test)]
fn daemon_transcript_source_home(profile_root: &Path) -> Option<PathBuf> {
    profile_root.parent().map(Path::to_path_buf)
}

#[cfg(not(test))]
fn daemon_transcript_source_home(_profile_root: &Path) -> Option<PathBuf> {
    crate::sessions::home_dir()
}

async fn production_project_server(
    store_administration: &StoreAdministration,
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
    invocation: &DaemonInvocationState,
    http_application_registry: &http_application::DaemonHttpApplicationRegistry,
    canonical_project_path: &Path,
    handshake: &DaemonHandshake,
    runtime: ProductionProjectCompositionRuntime,
    cancellation: &CancellationToken,
    #[cfg(test)] project_open_attempts: Option<&Arc<AtomicUsize>>,
) -> Result<ProductionProjectComposition> {
    let project_open_started = Instant::now();
    project_open_cancellation_checkpoint(cancellation)?;
    ensure_registered_project_route(
        store_administration,
        canonical_project_path,
        handshake.allow_init,
    )
    .await?;
    let route = ProjectRouteKey::from_handshake(canonical_project_path, handshake)?;
    if let Some(server) = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch(&route)
            .map(|(key, server)| (key.clone(), Arc::clone(server)))
    } {
        return Ok(ProductionProjectComposition {
            key: server.0,
            canonical_project_path: canonical_project_path.to_path_buf(),
            server: server.1,
            inserted: false,
            semantic_auto_download_enabled: None,
        });
    }

    let gate = project_open_gate(project_open_gates, &route).await;
    let _singleflight = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(project_open_cancellation_error()),
        singleflight = gate.lock() => singleflight,
    };
    if let Some(server) = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch(&route)
            .map(|(key, server)| (key.clone(), Arc::clone(server)))
    } {
        return Ok(ProductionProjectComposition {
            key: server.0,
            canonical_project_path: canonical_project_path.to_path_buf(),
            server: server.1,
            inserted: false,
            semantic_auto_download_enabled: None,
        });
    }

    #[cfg(test)]
    if let Some(attempts) = project_open_attempts {
        attempts.fetch_add(1, Ordering::Relaxed);
    }
    let (initial_cg, initial_deferred_post_open_health) =
        Box::pin(open_project_for_handshake_with_health_mode(
            canonical_project_path,
            handshake,
            store_administration,
            true,
        ))
        .await?;
    let initial_key = ProjectServerKey::from_open_project(&initial_cg, handshake)?;
    let synchronous_post_open_health = store_administration
        .project_servers()
        .lock()
        .await
        .requires_synchronous_health(&initial_key.owner);
    let (cg, deferred_post_open_health, key) = if synchronous_post_open_health {
        drop(initial_deferred_post_open_health);
        initial_cg.close();
        let (validated_cg, validated_deferred_post_open_health) =
            Box::pin(open_project_for_handshake_with_health_mode(
                canonical_project_path,
                handshake,
                store_administration,
                false,
            ))
            .await?;
        let validated_key = ProjectServerKey::from_open_project(&validated_cg, handshake)?;
        if validated_key.owner == initial_key.owner {
            store_administration
                .project_servers()
                .lock()
                .await
                .clear_synchronous_health(&validated_key.owner);
        }
        (
            validated_cg,
            validated_deferred_post_open_health,
            validated_key,
        )
    } else {
        (initial_cg, initial_deferred_post_open_health, initial_key)
    };
    let cg = Arc::new(cg);
    log_daemon_event(
        "project_open_phase",
        &[
            ("project", canonical_project_path.display().to_string()),
            ("phase", "graph_admitted".to_owned()),
            (
                "elapsed_ms",
                project_open_started.elapsed().as_millis().to_string(),
            ),
        ],
    );
    project_open_cancellation_checkpoint(cancellation)?;
    ensure_context_scout_owner_before_advertising(&cg)?;
    cg.register_project_store_in_global_registry().await?;
    let code_index_store_root = cg.store_layout().data_root.join("code-index-v1");
    let runtime_configuration = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("authoritative runtime configuration unavailable: {error}"),
        })?;
    let semantic_config = &runtime_configuration.config.semantic;
    let semantic_resources = &semantic_config.resources;
    let semantic_runtime = crate::semantic_code::DaemonSemanticRuntimeHandleV1::new(
        semantic_resources.max_concurrent_sessions as usize,
        usize::try_from(semantic_resources.max_resident_bytes / 4096)
            .unwrap_or(usize::MAX)
            .max(semantic_resources.max_batch_size as usize),
        semantic_resources.max_resident_bytes,
    )
    .map_err(|_| TraceDecayError::Config {
        message: "semantic runtime resource ceilings are invalid".to_owned(),
    })?;
    let semantic_auto_download_enabled =
        semantic_config.auto_download && runtime.semantic_auto_download();
    let _ = crate::semantic_code::apply_config_and_queue_startup(
        semantic_config.selected_model.as_deref(),
        semantic_auto_download_enabled,
    );
    let semantic_database = cg.dashboard_database_guard();
    let project_database_is_read_only = cg.db().filesystem_is_read_only();
    let semantic_lifecycle = crate::semantic_code::shared_lifecycle_owner();
    let existing = {
        let mut servers = store_administration.project_servers().lock().await;
        let existing = servers.get_ready(&key).cloned();
        if existing.is_some() {
            servers.bind_route(route.clone(), key.clone());
        }
        existing
    };
    if let Some(existing) = existing {
        return Ok(ProductionProjectComposition {
            key,
            canonical_project_path: canonical_project_path.to_path_buf(),
            server: existing,
            inserted: false,
            semantic_auto_download_enabled: Some(semantic_auto_download_enabled),
        });
    }

    let current_key = Arc::new(tokio::sync::Mutex::new(key.clone()));
    let current_project_path = Arc::new(tokio::sync::Mutex::new(
        canonical_project_path.to_path_buf(),
    ));
    let route_registered = Arc::new(AtomicBool::new(true));
    let database_owner_reconciler = runtime.database_owner_reconciler(
        store_administration,
        Arc::clone(&current_key),
        Arc::clone(&current_project_path),
        Arc::clone(&route_registered),
        handshake.clone(),
    );
    let automation_scheduler_reconciler = runtime.automation_scheduler_reconciler(
        Arc::clone(&current_key),
        Arc::clone(&current_project_path),
        handshake.clone(),
    );
    let authoritative_project_id =
        key.owner
            .project_id
            .clone()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project session runtime requires an authoritative project identity"
                    .to_owned(),
            })?;
    let registered_profile_db = store_administration.registered_profile_database().await?;
    let registry_db = Arc::clone(&registered_profile_db);
    let profile_identity = store_administration.profile_identity()?.clone();
    let accounting_db =
        crate::global_db::global_accounting_enabled().then(|| Arc::clone(&registered_profile_db));
    // Route after-edit hooks into the code-index scheduler queue on the
    // portable broker path too (mirrors the Unix `open_project_server`).
    let code_index_schedulers = invocation.code_index_schedulers.clone();
    let code_index_hook_sink: crate::mcp::server::CodeIndexHookSink =
        Arc::new(move |root: PathBuf, rel_paths: Vec<String>| {
            let schedulers = code_index_schedulers.clone();
            Box::pin(async move { schedulers.notify_hook_paths(&root, &rel_paths).await })
        });
    let code_index_publication_identity: crate::mcp::server::CodeIndexPublicationIdentityResolver =
        Arc::new(invocation.code_index_schedulers.clone());
    let code_search_project_id =
        tracedecay_domain::ProjectId::new(authoritative_project_id.clone()).map_err(|error| {
            TraceDecayError::Config {
                message: format!("project search identity is invalid: {error}"),
            }
        })?;
    let code_search_scope =
        project_open_owners::resolved_scope_for_project(cg.project_root(), &code_search_project_id)
            .map_err(|error| TraceDecayError::Config {
                message: format!("project search scope is invalid: {error:?}"),
            })?;
    let code_search_admission = pr9_mcp_admission::admit_pr9_mcp_read(
        Some(&profile_identity),
        &code_search_project_id,
        &code_search_scope,
        Arc::clone(&route_registered),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("project search admission is unavailable: {error}"),
    })?;
    let code_search_authority = code_search_admission.search_authority();
    let read_admission_provider = pr9_mcp_admission::Pr9McpReadAdmissionProviderV1::new(
        profile_identity.clone(),
        code_search_project_id.clone(),
        Arc::clone(&route_registered),
    );
    // `load_settings` returns defaults as `Ok` when no settings file exists,
    // so an `Err` is an unreadable or unparsable file. Serving silent defaults
    // there would drop the user's `custom_adapters`; record the degradation on
    // the broker instead (same pattern as
    // `application::dashboard_diagnostics::open_diagnostic_broker`).
    let diagnostic_broker =
        match crate::diagnostics::lsp::settings::load_settings(&cg.store_layout().dashboard_root)
            .await
        {
            Ok(settings) => Arc::new(tokio::sync::Mutex::new(
                crate::application::dashboard_diagnostics::diagnostic_broker(
                    canonical_project_path.to_path_buf(),
                    settings,
                ),
            )),
            Err(error) => {
                tracing::warn!(
                    dashboard_root = %cg.store_layout().dashboard_root.display(),
                    error = %error,
                    "code diagnostics settings could not be loaded; serving defaults as degraded"
                );
                let mut broker = crate::application::dashboard_diagnostics::diagnostic_broker(
                    canonical_project_path.to_path_buf(),
                    crate::diagnostics::lsp::settings::CodeDiagnosticsSettings::default(),
                );
                broker.record_settings_unavailable(error.to_string());
                Arc::new(tokio::sync::Mutex::new(broker))
            }
        };
    let code_index_search_executor = code_index_search_executor(
        invocation.code_index_schedulers.clone(),
        code_search_project_id.clone(),
        read_admission_provider,
    );
    let dashboard_code_index_schedulers = invocation.code_index_schedulers.clone();
    let dashboard_code_index_freshness_reader:
        crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader =
        Arc::new(move |project_root| {
            let schedulers = dashboard_code_index_schedulers.clone();
            Box::pin(async move { schedulers.dashboard_freshness(&project_root).await })
        });
    let dashboard_feedback_status_reader = crate::dashboard::feedback_api::feedback_status_reader(
        invocation.feedback_runtime_registrar(),
    );
    let application_invocation_executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor> =
        Arc::new(InProcessDaemonInvocationExecutor::new(
            invocation.clone(),
            store_administration.clone(),
            canonical_project_path.to_path_buf(),
        ));
    let transcript_source_home = daemon_transcript_source_home(profile_identity.profile_root());
    let retained_graph_resolver = retained_project_graph_resolver(store_administration.clone());
    let mut core_context = crate::mcp::server::McpServerConstructionContext::daemon_owned_core(
        Arc::clone(&cg),
        handshake.scope_prefix.clone(),
        crate::mcp::server::McpServerDaemonCoreAuthority {
            profile_identity: profile_identity.clone(),
            transcript_source_home: transcript_source_home.clone(),
            accounting: accounting_db.clone(),
            registry: Arc::clone(&registry_db),
            database_owner_reconciler: Arc::clone(&database_owner_reconciler),
            project_routes: store_administration.project_routes(),
            writers: crate::mcp::server::McpServerWriters::daemon_owned(
                coordinated_dashboard_automation_writer(store_administration.clone()),
                coordinated_hook_branch_writer(store_administration.clone()),
                coordinated_background_refresh_writer(store_administration.clone()),
            ),
        },
    )
    .with_dashboard_code_index_freshness_reader(Arc::clone(&dashboard_code_index_freshness_reader))
    .with_dashboard_feedback_status_reader(Arc::clone(&dashboard_feedback_status_reader))
    .with_diagnostics_lsp(Arc::clone(&diagnostic_broker))
    .with_code_index_hook_sink(Arc::clone(&code_index_hook_sink))
    .with_code_index_publication_identity(Arc::clone(&code_index_publication_identity))
    .with_code_index_search_executor(Arc::clone(&code_index_search_executor))
    .with_code_index_search_authority(code_search_authority.clone())
    .with_project_server_live(Arc::clone(&route_registered))
    .with_application_invocation_executor(Arc::clone(&application_invocation_executor))
    .with_retained_project_graph_resolver(Arc::clone(&retained_graph_resolver));
    if let Some(reconciler) = automation_scheduler_reconciler.as_ref() {
        core_context = core_context.with_automation_scheduler_reconciler(Arc::clone(reconciler));
    }
    project_open_cancellation_checkpoint(cancellation)?;
    let mcp_construction_started = Instant::now();
    let core_candidate = crate::mcp::McpServer::new_with_context(core_context).await;
    log_daemon_event(
        "project_open_phase",
        &[
            ("project", canonical_project_path.display().to_string()),
            ("phase", "mcp_core_constructed".to_owned()),
            (
                "elapsed_ms",
                mcp_construction_started.elapsed().as_millis().to_string(),
            ),
        ],
    );
    if cancellation.is_cancelled() {
        core_candidate.cancel_startup_transcript_ingest();
        core_candidate.shutdown().await;
        return Err(project_open_cancellation_error());
    }
    let project_id = key
        .owner
        .project_id
        .clone()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open owners require an authoritative project identity".to_owned(),
        })?;
    let resolution = store_administration
        .project_servers()
        .lock()
        .await
        .bind_or_insert_route_bounded(
            route,
            key.clone(),
            core_candidate,
            MAX_CACHED_PROJECT_SERVERS,
            |server| Arc::strong_count(server) > 1,
        );
    let Some((mut resolved, inserted)) = resolution else {
        route_registered.store(false, Ordering::Release);
        return Err(project_server_capacity_error());
    };
    if !inserted {
        route_registered.store(false, Ordering::Release);
    } else {
        if cancellation.is_cancelled() {
            resolved.cancel_startup_transcript_ingest();
            return Err(project_open_cancellation_error());
        }
        if !project_database_is_read_only {
            project_open_owners::install_project_open_source_edit_preview_owner(
                resolved.as_ref(),
                Arc::clone(&cg),
                canonical_project_path,
                &project_id,
            )
            .await?;
        }
        // Publish the graph/search/diagnostic core before session admission.
        // Source-edit previews are available, while mutations fail closed as
        // warming until the full server has its transaction authority.
        {
            let mut servers = store_administration.project_servers().lock().await;
            if !servers.mark_ready(&key) {
                return Err(TraceDecayError::Config {
                    message: "project server disappeared before core publication completed"
                        .to_owned(),
                });
            }
        };
        log_daemon_event(
            "project_open_phase",
            &[
                ("project", canonical_project_path.display().to_string()),
                ("phase", "core_published".to_owned()),
                (
                    "elapsed_ms",
                    project_open_started.elapsed().as_millis().to_string(),
                ),
            ],
        );
        let quarantine_on_upgrade_failure = AtomicBool::new(false);
        let session_capabilities_published = AtomicBool::new(false);
        let full_upgrade: Result<Arc<crate::mcp::McpServer>> = async {
            if let Some(database) = deferred_post_open_health
                && let Err(error) = database.repair_fts_after_open().await
            {
                quarantine_on_upgrade_failure.store(true, Ordering::Release);
                return Err(error);
            }
            if *current_key.lock().await != key {
                return Err(TraceDecayError::Config {
                    message: "project changed branch during core capability admission".to_owned(),
                });
            }
            project_open_cancellation_checkpoint(cancellation)?;
            let project_sessions_started = Instant::now();
            let registered_project_session_db = store_administration
                .registered_project_session_database(cg.project_root(), cg.store_layout())
                .await?;
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "project_sessions_admitted".to_owned()),
                    (
                        "elapsed_ms",
                        project_sessions_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            let profile_sessions_started = Instant::now();
            let registered_user_session_db = store_administration
                .registered_profile_session_database()
                .await?;
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "profile_sessions_admitted".to_owned()),
                    (
                        "elapsed_ms",
                        profile_sessions_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            let session_db = Arc::clone(&registered_project_session_db);
            let user_session_db = Arc::clone(&registered_user_session_db);
            let host_admission_broker = store_administration
                .host_admission_broker(&session_db)
                .await?
                .broker()
                .cloned();
            let project_session_refresh_wake = store_administration
                .session_temporal_refresh_schedulers()
                .ensure_project(key.owner.clone(), Arc::clone(&session_db))
                .await;
            let user_session_refresh_wake = store_administration
                .session_temporal_refresh_schedulers()
                .ensure_profile(
                    user_session_db.db_path().to_path_buf(),
                    Arc::clone(&user_session_db),
                )
                .await;
            let doctor_report_reader = doctor_kernel::production_doctor_report_reader(
                canonical_project_path.to_path_buf(),
                code_search_project_id.clone(),
                cg.store_layout().clone(),
                cg.db().clone(),
                Arc::clone(&registry_db),
                Arc::clone(&user_session_db),
                Arc::clone(&session_db),
                profile_identity.profile_root().to_path_buf(),
                cg.get_config().sync.retention.clone(),
                invocation.code_index_schedulers.clone(),
                Arc::clone(&diagnostic_broker),
                invocation.feedback_runtime_registrar(),
            );
            let doctor_remediation_dispatcher =
                doctor_kernel::production_doctor_remediation_dispatcher(
                    doctor_kernel::ProductionDoctorRemediationOwnersV1 {
                        project_root: canonical_project_path.to_path_buf(),
                        project_id: code_search_project_id.clone(),
                        layout: cg.store_layout().clone(),
                        registry: Arc::clone(&registry_db),
                        profile_sessions: Arc::clone(&user_session_db),
                        project_sessions: Arc::clone(&session_db),
                        profile_root: profile_identity.profile_root().to_path_buf(),
                        config: cg.get_config().clone(),
                        global_retention: crate::user_config::UserConfig::load()
                            .automation
                            .retention,
                        store_administration: store_administration.clone(),
                        invocation: invocation.clone(),
                        code_index_store_root: code_index_store_root.clone(),
                        semantic_runtime: semantic_runtime.clone(),
                        semantic_database: Arc::clone(&semantic_database),
                        semantic_lifecycle: semantic_lifecycle.clone(),
                        semantic_resources: *semantic_resources,
                        route_registered: Arc::clone(&route_registered),
                    },
                    Arc::clone(&doctor_report_reader),
                );
            let mut full_context = crate::mcp::server::McpServerConstructionContext::daemon_owned(
                Arc::clone(&cg),
                handshake.scope_prefix.clone(),
                crate::mcp::server::McpServerDaemonAuthority {
                    profile_identity: profile_identity.clone(),
                    transcript_source_home,
                    databases: crate::mcp::server::McpServerDaemonDatabases {
                        accounting: accounting_db,
                        registry: registry_db,
                        project_sessions: session_db,
                        user_sessions: user_session_db,
                        registered_project_sessions: Arc::clone(&registered_project_session_db),
                        registered_user_sessions: registered_user_session_db,
                    },
                    host_admission_broker,
                    project_session_refresh_wake,
                    user_session_refresh_wake,
                    database_owner_reconciler,
                    project_routes: store_administration.project_routes(),
                    writers: crate::mcp::server::McpServerWriters::daemon_owned(
                        coordinated_dashboard_automation_writer(store_administration.clone()),
                        coordinated_hook_branch_writer(store_administration.clone()),
                        coordinated_background_refresh_writer(store_administration.clone()),
                    ),
                },
            )
            .with_dashboard_doctor_report_reader(doctor_report_reader)
            .with_dashboard_doctor_remediation_dispatcher(doctor_remediation_dispatcher)
            .with_dashboard_code_index_freshness_reader(dashboard_code_index_freshness_reader)
            .with_dashboard_feedback_status_reader(dashboard_feedback_status_reader)
            .with_diagnostics_lsp(diagnostic_broker)
            .with_code_index_hook_sink(code_index_hook_sink)
            .with_code_index_publication_identity(code_index_publication_identity)
            .with_code_index_search_executor(code_index_search_executor)
            .with_code_index_search_authority(code_search_authority)
            .with_project_server_live(Arc::clone(&route_registered))
            .with_application_invocation_executor(application_invocation_executor)
            .with_startup_catch_up_enabled(runtime.startup_catch_up())
            .with_retained_project_graph_resolver(retained_graph_resolver);
            if let Some(reconciler) = automation_scheduler_reconciler {
                full_context = full_context.with_automation_scheduler_reconciler(reconciler);
            }
            project_open_cancellation_checkpoint(cancellation)?;
            let full_construction_started = Instant::now();
            let full_candidate = crate::mcp::McpServer::new_with_context(full_context).await;
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "mcp_full_constructed".to_owned()),
                    (
                        "elapsed_ms",
                        full_construction_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            if *current_key.lock().await != key {
                full_candidate.cancel_startup_transcript_ingest();
                full_candidate.shutdown().await;
                return Err(TraceDecayError::Config {
                    message: "project changed branch during full capability admission".to_owned(),
                });
            }
            let upgraded = store_administration
                .project_servers()
                .lock()
                .await
                .replace_ready_if(&key, Arc::clone(&full_candidate), |current| {
                    Arc::ptr_eq(current, &resolved)
                });
            if !upgraded {
                full_candidate.cancel_startup_transcript_ingest();
                full_candidate.shutdown().await;
                return Err(TraceDecayError::Config {
                    message: "project server changed during session capability upgrade".to_owned(),
                });
            }
            session_capabilities_published.store(true, Ordering::Release);
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "session_capabilities_published".to_owned()),
                    (
                        "elapsed_ms",
                        project_open_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            let full_setup: Result<()> = async {
                let full_setup_started = Instant::now();
                let log_full_setup_phase = |phase: &'static str| {
                    log_daemon_event(
                        "project_open_phase",
                        &[
                            ("project", canonical_project_path.display().to_string()),
                            ("phase", phase.to_owned()),
                            (
                                "elapsed_ms",
                                full_setup_started.elapsed().as_millis().to_string(),
                            ),
                        ],
                    );
                };
                project_open_cancellation_checkpoint(cancellation)?;
                let source_edit_mutation_ready = if project_database_is_read_only {
                    None
                } else {
                    Some(
                        project_open_owners::install_project_open_source_edit_preview_owner(
                            full_candidate.as_ref(),
                            Arc::clone(&cg),
                            canonical_project_path,
                            &project_id,
                        )
                        .await?,
                    )
                };
                log_full_setup_phase("source_edit_preview_ready");
                ensure_git_index_transactions_for_mutation_owners(
                    store_administration,
                    Arc::clone(&registered_project_session_db),
                    canonical_project_path,
                    key.owner.project_id.as_deref(),
                )
                .await?;
                log_full_setup_phase("git_transactions_ready");
                let dependent_owners = if project_database_is_read_only {
                    None
                } else {
                    let state = project_open_owners::register_project_open_production_owners(
                        invocation,
                        store_administration.git_index_transaction_services(),
                        canonical_project_path,
                        &project_id,
                        full_candidate.as_ref(),
                        source_edit_mutation_ready
                            .expect("writable projects install source edit preview authority"),
                    )
                    .await?;
                    log_full_setup_phase("independent_owners_registered");
                    Some(state)
                };
                invocation
                    .mount_code_index(
                        canonical_project_path,
                        code_index_store_root,
                        Some(&semantic_runtime),
                        Some(semantic_database),
                        semantic_lifecycle,
                        Some(*semantic_resources),
                    )
                    .await?;
                log_full_setup_phase("code_index_mounted");
                project_open_cancellation_checkpoint(cancellation)?;
                match invocation
                    .semantic_runtime_registrar()
                    .register(canonical_project_path.to_path_buf(), semantic_runtime)
                    .await
                {
                    Ok(()) | Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered) => {}
                }
                log_full_setup_phase("semantic_runtime_registered");
                if let Some(dependent_owners) = dependent_owners {
                    project_open_owners::register_project_open_dependent_owners(
                        invocation,
                        canonical_project_path,
                        dependent_owners,
                    )
                    .await?;
                    log_full_setup_phase("production_owners_registered");
                    mount_http_application_router(
                        http_application_registry,
                        &project_id,
                        canonical_project_path,
                    )
                    .await?;
                    log_full_setup_phase("http_application_mounted");
                }
                Ok(())
            }
            .await;
            if let Err(error) = full_setup {
                return Err(error);
            }
            if *current_key.lock().await != key {
                return Err(TraceDecayError::Config {
                    message: "project changed branch during full capability admission".to_owned(),
                });
            }
            // The registry cutover prevents new core leases. Existing core
            // requests may finish while dependent owners warm, then the
            // displaced server is drained without closing the shared graph.
            resolved.revoke_project_server_responses();
            resolved.cancel_startup_transcript_ingest();
            schedule_project_server_retirement(
                store_administration,
                vec![Arc::clone(&resolved)],
                None,
            )
            .await;
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "full_published".to_owned()),
                    (
                        "elapsed_ms",
                        project_open_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            Ok(full_candidate)
        }
        .await;
        match full_upgrade {
            Ok(full_server) => resolved = full_server,
            Err(error) => {
                let failed_key = current_key.lock().await.clone();
                let mut removed = {
                    let mut servers = store_administration.project_servers().lock().await;
                    if quarantine_on_upgrade_failure.load(Ordering::Acquire) {
                        servers.quarantine_and_remove_owner(&failed_key.owner)
                    } else {
                        servers.remove_owner(&failed_key.owner)
                    }
                };
                if session_capabilities_published.load(Ordering::Acquire)
                    && removed.iter().all(|server| !Arc::ptr_eq(server, &resolved))
                {
                    removed.push(Arc::clone(&resolved));
                }
                for server in &removed {
                    server.revoke_project_server_responses();
                    server.cancel_startup_transcript_ingest();
                }
                debug_assert!(
                    !removed.is_empty(),
                    "failed core upgrade must retire its published owner"
                );
                // Request execution may itself need the owner writer held by
                // this open attempt. The tracked retirement starts draining
                // after this closure returns and releases that writer.
                schedule_project_server_retirement(
                    store_administration,
                    removed,
                    Some(Arc::clone(&route_registered)),
                )
                .await;
                return Err(error);
            }
        }
    }
    Ok(ProductionProjectComposition {
        key,
        canonical_project_path: canonical_project_path.to_path_buf(),
        server: resolved,
        inserted,
        semantic_auto_download_enabled: Some(semantic_auto_download_enabled),
    })
}

#[cfg(any(test, feature = "test-transport"))]
struct ProductionProjectHarnessResourcesV1 {
    store_administration: StoreAdministration,
    invocation: DaemonInvocationState,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    servers: HashMap<PathBuf, Arc<crate::mcp::McpServer>>,
    _database_scope: crate::db::DaemonDatabaseScope,
    _lifecycle_lease: crate::lifecycle_lease::LifecycleLease,
}

/// In-process owner for the same production project composition used by the
/// daemon. The caller supplies one isolated root containing both the profile
/// and every project; live profile paths are rejected before any store opens.
#[cfg(any(test, feature = "test-transport"))]
#[doc(hidden)]
pub struct ProductionProjectCompositionHarnessV1 {
    isolation_root: PathBuf,
    profile_root: PathBuf,
    semantic_auto_download_enabled: bool,
    resources: Option<ProductionProjectHarnessResourcesV1>,
}

#[cfg(any(test, feature = "test-transport"))]
impl ProductionProjectCompositionHarnessV1 {
    pub async fn open(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self> {
        let live_profile_root = crate::config::user_data_dir().filter(|path| path.exists());
        Self::open_with_live_profile_root(isolation_root, project_roots, live_profile_root).await
    }

    async fn open_with_live_profile_root(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
        live_profile_root: Option<PathBuf>,
    ) -> Result<Self> {
        std::fs::create_dir_all(isolation_root.as_ref()).map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "failed to create production-composition isolation root '{}': {error}",
                    isolation_root.as_ref().display()
                ),
            }
        })?;
        let isolation_root = std::fs::canonicalize(isolation_root.as_ref()).map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "failed to canonicalize production-composition isolation root '{}': {error}",
                    isolation_root.as_ref().display()
                ),
            }
        })?;
        if let Some(live_profile_root) =
            live_profile_root.and_then(|path| std::fs::canonicalize(path).ok())
        {
            let overlaps_live_profile = isolation_root == live_profile_root
                || isolation_root.starts_with(&live_profile_root)
                || live_profile_root.starts_with(&isolation_root);
            if overlaps_live_profile {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "production-composition isolation root '{}' overlaps live profile '{}'",
                        isolation_root.display(),
                        live_profile_root.display()
                    ),
                });
            }
        }

        let profile_root = isolation_root.join("profile");
        std::fs::create_dir_all(&profile_root).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to create isolated production-composition profile '{}': {error}",
                profile_root.display()
            ),
        })?;
        #[cfg(unix)]
        set_owner_only_permissions(&profile_root, 0o700)?;

        let project_roots = project_roots
            .into_iter()
            .map(|project_root| {
                std::fs::canonicalize(&project_root).map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to canonicalize production-composition project '{}': {error}",
                        project_root.display()
                    ),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if project_roots.is_empty() {
            return Err(TraceDecayError::Config {
                message: "production-composition harness requires at least one project".to_owned(),
            });
        }
        for project_root in &project_roots {
            if !project_root.starts_with(&isolation_root) || project_root.starts_with(&profile_root)
            {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "production-composition project '{}' must be inside isolation root '{}' and outside its profile",
                        project_root.display(),
                        isolation_root.display()
                    ),
                });
            }
        }

        let profile_identity = profile_identity::load_or_create(&profile_root)?;
        let lifecycle_lease = crate::lifecycle_lease::acquire_shared_for_profile(
            &profile_root,
            "in-process production composition",
        )?;
        let database_scope = crate::db::enter_daemon_database_scope(
            &profile_root,
            1,
            "in-process-production-composition",
        )?;
        let store_administration =
            StoreAdministration::default().with_profile_identity(profile_identity.clone());
        let invocation = DaemonInvocationState::default();
        invocation.configure_github_read_only_credentials(&profile_identity);
        let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
        install_http_application_cold_resolver(
            &http_application_registry,
            store_administration.clone(),
        )?;
        let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
        let client_identity = DaemonClientIdentity {
            profile_root: profile_root.clone(),
            global_db_path: profile_root.join("global.db"),
        };
        let mut servers = HashMap::new();
        let mut semantic_auto_download_enabled = false;

        for (index, project_root) in project_roots.into_iter().enumerate() {
            let handshake = DaemonHandshake {
                client_version: binary_version().to_owned(),
                client_instance_id: format!("production-composition-harness-{index}"),
                client_identity: client_identity.clone(),
                scope_prefix: None,
                project_path: Some(project_root.clone()),
                timings: false,
                allow_init: true,
                allow_initialize_root_routing: false,
                tool_list_changed_capable: false,
                catalog_version: String::new(),
            };
            let (canonical_project_path, _) = project_route_for_handshake(&handshake)?;
            let composition = store_administration
                .with_writer(|| async {
                    let cancellation = CancellationToken::new();
                    production_project_server(
                        &store_administration,
                        &project_open_gates,
                        &invocation,
                        &http_application_registry,
                        &canonical_project_path,
                        &handshake,
                        ProductionProjectCompositionRuntime::Portable {
                            semantic_auto_download: false,
                            startup_catch_up: false,
                        },
                        &cancellation,
                        #[cfg(test)]
                        None,
                    )
                    .await
                })
                .await?;
            wait_for_production_composition_code_index(
                &invocation,
                &composition.canonical_project_path,
            )
            .await?;
            semantic_auto_download_enabled |= composition
                .semantic_auto_download_enabled
                .ok_or_else(|| TraceDecayError::Config {
                    message: "production-composition harness reused an unobserved semantic runtime"
                        .to_owned(),
                })?;
            servers.insert(composition.canonical_project_path, composition.server);
        }

        Ok(Self {
            isolation_root,
            profile_root,
            semantic_auto_download_enabled,
            resources: Some(ProductionProjectHarnessResourcesV1 {
                store_administration,
                invocation,
                project_open_gates,
                servers,
                _database_scope: database_scope,
                _lifecycle_lease: lifecycle_lease,
            }),
        })
    }

    #[cfg(test)]
    async fn open_with_live_profile_root_for_test(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
        live_profile_root: PathBuf,
    ) -> Result<Self> {
        Self::open_with_live_profile_root(isolation_root, project_roots, Some(live_profile_root))
            .await
    }

    pub fn isolation_root(&self) -> &Path {
        &self.isolation_root
    }

    pub fn profile_root(&self) -> &Path {
        &self.profile_root
    }

    pub fn semantic_auto_download_enabled(&self) -> bool {
        self.semantic_auto_download_enabled
    }

    pub async fn read_profile_analytics_events(
        &self,
        query: &crate::global_db::AnalyticsEventQuery,
    ) -> Result<Vec<crate::global_db::AnalyticsEventRecord>> {
        let resources = self
            .resources
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "production-composition harness is shut down".to_owned(),
            })?;
        resources
            .store_administration
            .registered_profile_database()
            .await?
            .query_analytics_events(query)
            .await
            .map_err(|message| TraceDecayError::Database {
                message,
                operation: "read retained production profile analytics".to_owned(),
            })
    }

    pub fn server(&self, project_root: impl AsRef<Path>) -> Result<Arc<crate::mcp::McpServer>> {
        let canonical_project_path =
            std::fs::canonicalize(project_root.as_ref()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!(
                        "failed to canonicalize production-composition project '{}': {error}",
                        project_root.as_ref().display()
                    ),
                }
            })?;
        self.resources
            .as_ref()
            .and_then(|resources| resources.servers.get(&canonical_project_path))
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "project '{}' is not mounted in this production composition",
                    canonical_project_path.display()
                ),
            })
    }

    pub async fn project_data_root(&self, project_root: impl AsRef<Path>) -> Result<PathBuf> {
        Ok(self
            .server(project_root)?
            .cg()
            .await
            .store_layout()
            .data_root
            .clone())
    }

    pub async fn track_worktree_branch(
        &self,
        project_root: impl AsRef<Path>,
        worktree_root: impl AsRef<Path>,
        branch: &str,
    ) -> Result<crate::branch::BranchAddOutcome> {
        self.server(project_root)?
            .cg()
            .await
            .track_worktree_branch(worktree_root.as_ref(), branch)
            .await
    }

    pub async fn sync_tracked_worktree_branch(
        &self,
        project_root: impl AsRef<Path>,
        worktree_root: impl AsRef<Path>,
        branch: &str,
        query: &str,
    ) -> Result<(Option<String>, Option<String>, bool, bool)> {
        let graph = self.server(project_root)?.cg().await;
        let (database_path, _, _) = crate::tracedecay::TraceDecay::resolve_db_for_branch(
            graph.project_root(),
            &graph.store_layout().data_root,
            Some(branch),
        );
        let branch_graph = graph
            .sync_retained_worktree_branch(worktree_root.as_ref(), branch, &database_path)
            .await?;
        let contains_query = !branch_graph.search(query, 10).await?.is_empty();
        Ok((
            branch_graph.active_branch().map(str::to_owned),
            branch_graph.serving_branch().map(str::to_owned),
            branch_graph.is_fallback(),
            contains_query,
        ))
    }

    pub async fn call_tool(
        &self,
        project_root: impl AsRef<Path>,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<JsonRpcResponse> {
        let request = serde_json::from_value::<JsonRpcRequest>(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            },
        }))
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to construct production-composition tool request: {error}"),
        })?;
        self.server(project_root)?
            .handle_request(&request)
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "production-composition server returned no response for '{tool_name}'"
                ),
            })
    }

    pub async fn shutdown(mut self) {
        if let Some(resources) = self.resources.take() {
            shutdown_production_project_harness(resources).await;
        }
    }
}

#[cfg(any(test, feature = "test-transport"))]
async fn wait_for_production_composition_code_index(
    invocation: &DaemonInvocationState,
    project_root: &Path,
) -> Result<()> {
    timeout(Duration::from_secs(20), async {
        loop {
            if invocation
                .code_index_schedulers
                .latest_generation_id(project_root)
                .await
                .is_some()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| TraceDecayError::Config {
        message: format!(
            "production-composition code index did not publish for '{}'",
            project_root.display()
        ),
    })
}

#[cfg(any(test, feature = "test-transport"))]
impl Drop for ProductionProjectCompositionHarnessV1 {
    fn drop(&mut self) {
        let Some(resources) = self.resources.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(shutdown_production_project_harness(resources));
        }
    }
}

#[cfg(any(test, feature = "test-transport"))]
async fn shutdown_production_project_harness(mut resources: ProductionProjectHarnessResourcesV1) {
    resources
        .store_administration
        .join_project_server_retirements()
        .await;
    let servers = detach_project_servers(&resources.store_administration).await;
    resources.servers.clear();
    for server in &servers {
        server.ledger_writes_settled().await;
        server.shutdown_background_tasks().await;
    }
    resources
        .store_administration
        .session_temporal_refresh_schedulers()
        .shutdown()
        .await;
    resources
        .store_administration
        .shutdown_host_admission_replay()
        .await;
    resources.invocation.shutdown().await;
    shutdown_detached_project_servers(servers).await;
    drop(resources);
}

async fn write_routed_initialize_response(
    server: &crate::mcp::McpServer,
    transport: &mut impl McpTransport,
    first_request_line: &str,
    route: Option<&InitializeRouteMetadata>,
) -> Result<bool> {
    let Some(route) = route else {
        return Ok(false);
    };
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) else {
        return Ok(false);
    };
    if request.method != "initialize" {
        return Ok(false);
    }
    let Some(mut response) = server.handle_request(&request).await else {
        return Ok(false);
    };
    attach_initialize_route_metadata(&mut response, route);
    write_json_rpc_response(transport, &response).await?;
    Ok(true)
}

const MAX_PENDING_PROJECT_OPEN_LINES: usize = 64;

async fn await_project_owner_or_disconnect<T>(
    transport: &mut impl McpTransport,
    open: impl std::future::Future<Output = Result<T>>,
) -> Result<Option<(T, VecDeque<String>)>> {
    tokio::pin!(open);
    let mut pending_lines = VecDeque::new();
    loop {
        tokio::select! {
            result = &mut open => return result.map(|owner| Some((owner, pending_lines))),
            incoming = transport.read_line() => {
                let Some(line) = incoming? else {
                    return Ok(None);
                };
                if pending_lines.len() >= MAX_PENDING_PROJECT_OPEN_LINES {
                    return Err(TraceDecayError::Config {
                        message: "daemon client pipelined too many requests while the project owner was opening"
                            .to_owned(),
                    });
                }
                pending_lines.push_back(line);
            }
        }
    }
}

#[cfg(unix)]
async fn serve_broker_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: Option<String>,
    admission_class: DaemonClientAdmissionClass,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    if let Some(expected_token) = auth_token.as_deref() {
        let preface_line = tokio::select! {
            result = read_line_handling_wire_oversized(&mut transport) => result?,
            () = engine.lifecycle.wait_for_draining() => return Ok(()),
        };
        let Some(preface_line) = preface_line else {
            return Ok(());
        };
        let preface =
            DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            })?;
        if !preface.authenticate(expected_token) {
            return Err(TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            });
        }
    }
    let line = tokio::select! {
        result = read_line_handling_wire_oversized(&mut transport) => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(line) = line else {
        return Ok(());
    };
    let Some(setup_activity) = engine.lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = DaemonHandshake::from_line(&line)?;
    let store_administration =
        bind_authenticated_profile_identity(&mut handshake, &engine.store_administration).await?;
    let mut engine = engine;
    engine.store_administration = store_administration;
    let first_request_line = tokio::select! {
        result = read_line_handling_wire_oversized(&mut transport) => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(first_request_line) = first_request_line else {
        return Ok(());
    };
    let reserved_control_request = is_reserved_control_request(&first_request_line);
    if admission_class == DaemonClientAdmissionClass::ReservedControl && !reserved_control_request {
        drop(setup_activity);
        reject_reserved_bulk_request(
            &mut transport,
            &first_request_line,
            MAX_CONCURRENT_DAEMON_CLIENTS,
        )
        .await?;
        return Ok(());
    }
    let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
        match engine
            .per_client_admission
            .try_admit_request(&handshake, &first_request_line)
        {
            Ok(permit) => Some(permit),
            Err(response) => {
                drop(setup_activity);
                reject_admitted_request(&mut transport, &first_request_line, response).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    if let Some(request) = doctor_runtime_request(&first_request_line) {
        let report_ready = if request.doctor_report_requested() {
            engine
                .cached_project_server(&handshake)
                .await?
                .is_some_and(|server| server.doctor_report_ready())
        } else {
            false
        };
        if request.should_serve_from_core(report_ready) {
            drop(setup_activity);
            write_doctor_runtime_response(
                &mut transport,
                &handshake,
                &engine.store_administration,
                request,
            )
            .await?;
            return Ok(());
        }
    }
    engine.log_client_version_skew(&handshake).await;
    ensure_user_profile_host_admission_replay_for_identity(
        &engine.store_administration,
        &handshake.client_identity,
    )
    .await?;
    // Resolve initialize roots only after authentication and inside daemon
    // authority. The proxy process never opens the registry database.
    let initialize_route = apply_daemon_initialize_route(
        &mut handshake,
        &first_request_line,
        &engine.store_administration,
    )
    .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => engine.execute_branch_admin(&handshake, action).await,
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
        let response = match await_project_owner_or_disconnect(
            &mut transport,
            engine.project_server_for_request(&handshake),
        )
        .await
        {
            Ok(Some(_)) => {
                branch_add_response(&engine.store_administration, &handshake, &request).await
            }
            Ok(None) => return Ok(()),
            Err(error) => JsonRpcResponse::error(
                request.id.clone(),
                ErrorCode::InternalError,
                error.to_string(),
            ),
        };
        drop(setup_activity);
        write_json_rpc_response(&mut transport, &response).await?;
        return Ok(());
    }
    if let Some(invocation) = parse_daemon_invocation_request(&first_request_line) {
        let mut invocation = invocation;
        let mut owned_lsp_sessions = HashMap::new();
        let result = async {
            loop {
                let session_transition = invocation
                    .as_ref()
                    .ok()
                    .and_then(invocation_lsp_session_transition);
                let response = match invocation {
                    Ok(request) => execute_daemon_invocation(&engine, &handshake, request).await,
                    Err(response) => response,
                };
                update_connection_lsp_sessions(
                    &mut owned_lsp_sessions,
                    session_transition.as_ref(),
                    &response,
                );
                write_daemon_invocation_response(&mut transport, &response).await?;
                let next_line = tokio::select! {
                    result = read_line_handling_wire_oversized(&mut transport) => result?,
                    () = engine.lifecycle.wait_for_draining() => return Ok(()),
                };
                let Some(next_line) = next_line else {
                    return Ok(());
                };
                let Some(next_invocation) = parse_daemon_invocation_request(&next_line) else {
                    return Ok(());
                };
                invocation = next_invocation;
            }
        }
        .await;
        cleanup_connection_lsp_sessions(&engine.invocation, owned_lsp_sessions).await;
        return result;
    }
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
        let project_node_count =
            if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                if handshake.project_path.is_some() {
                    cached_project_node_count(&engine.store_administration, &handshake).await
                } else {
                    Some(0)
                }
            } else {
                None
            };
        if let Some(mut response) =
            daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
        {
            let project_open_error = if handshake.project_path.is_some()
                && matches!(
                    classify_mcp_method(&request.method),
                    McpMethod::Initialize | McpMethod::ToolsList
                ) {
                match engine.cached_project_open_failure(&handshake).await {
                    Ok(Some(failure)) => Some(failure.to_error()),
                    Ok(None)
                        if matches!(
                            classify_mcp_method(&request.method),
                            McpMethod::Initialize
                        ) =>
                    {
                        Box::pin(
                            engine
                                .schedule_project_server_warmup(handshake.clone(), request.clone()),
                        )
                        .await
                        .err()
                    }
                    Ok(None) => None,
                    Err(error) => Some(error),
                }
            } else {
                None
            };
            if let Some(error) = project_open_error {
                response = request
                    .id
                    .clone()
                    .map(|id| project_open_error_response(id, &error));
            }
            // Keep catalog-refresh bookkeeping consistent with the regular MCP
            // server path: initialize and tools/list mark this catalog current.
            if let Some(key) = engine
                .claim_catalog_refresh(&handshake, &first_request_line)
                .await
                && let Err(error) = write_tool_list_changed_notification(&mut transport).await
            {
                engine.release_catalog_refresh(key).await;
                return Err(error);
            }
            drop(setup_activity);
            if let Some(response) = response {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    let user_session_request = projectless_user_session_request(&first_request_line);
    let mut pending_project_open_lines = VecDeque::new();
    let server = if handshake.project_path.is_some() && !user_session_request {
        match await_project_owner_or_disconnect(
            &mut transport,
            engine.project_server_for_request(&handshake),
        )
        .await
        {
            Ok(Some((server, pending_lines))) => {
                pending_project_open_lines = pending_lines;
                Some(server)
            }
            Ok(None) => {
                drop(setup_activity);
                return Ok(());
            }
            Err(error) => {
                drop(setup_activity);
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    drop(setup_activity);
    if !engine.lifecycle.accepting() {
        return Ok(());
    }

    // The stdio proxy creates one daemon connection per request. The request
    // was peeked above so initialize-root routing happens before project open.
    if let Some(key) = engine
        .claim_catalog_refresh(&handshake, &first_request_line)
        .await
        && let Err(error) = write_tool_list_changed_notification(&mut transport).await
    {
        engine.release_catalog_refresh(key).await;
        return Err(error);
    }
    let initialize_handled = match server.as_deref() {
        Some(server) => {
            write_routed_initialize_response(
                server,
                &mut transport,
                &first_request_line,
                initialize_route.as_ref(),
            )
            .await?
        }
        None => false,
    };
    let mut transport = ReplayTransport::new(transport);
    if !initialize_handled {
        transport.push_replay(first_request_line)?;
    }
    for line in pending_project_open_lines {
        transport.push_replay(line)?;
    }

    if let Some(server) = server {
        Box::pin(server.run_daemon_connection_with_timings(
            &mut transport,
            handshake.timings,
            &engine.lifecycle,
        ))
        .await?;
    } else {
        serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            &engine.lifecycle,
            &engine.store_administration,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
async fn serve_windows_broker_client(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    Box::pin(serve_windows_broker_client_with_class(
        stream,
        auth_token,
        lifecycle,
        store_administration,
        project_open_gates,
        DaemonPerClientAdmission::default(),
        DaemonClientAdmissionClass::General,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
}

#[cfg(test)]
// Cohesive per-connection serving context; bundling into a params struct would churn every caller.
#[allow(clippy::too_many_arguments)]
async fn serve_windows_broker_client_with_class(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    per_client_admission: DaemonPerClientAdmission,
    admission_class: DaemonClientAdmissionClass,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    Box::pin(serve_windows_broker_client_with_class_and_invocation(
        stream,
        auth_token,
        lifecycle,
        store_administration,
        project_open_gates,
        DaemonInvocationState::default(),
        http_application::DaemonHttpApplicationRegistry::default(),
        per_client_admission,
        admission_class,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
}

#[cfg(any(not(unix), test))]
// The foreground portable broker supplies one daemon-generation invocation state.
#[allow(clippy::too_many_arguments)]
async fn serve_windows_broker_client_with_class_and_invocation(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    per_client_admission: DaemonPerClientAdmission,
    admission_class: DaemonClientAdmissionClass,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    let Some(preface_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let preface =
        DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        })?;
    if !preface.authenticate(auth_token) {
        return Err(TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        });
    }
    let Some(handshake_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let Some(setup_activity) = lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = DaemonHandshake::from_line(&handshake_line)?;
    let store_administration =
        bind_authenticated_profile_identity(&mut handshake, &store_administration).await?;
    let Some(first_request_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let reserved_control_request = is_reserved_control_request(&first_request_line);
    if admission_class == DaemonClientAdmissionClass::ReservedControl && !reserved_control_request {
        drop(setup_activity);
        reject_reserved_bulk_request(
            &mut transport,
            &first_request_line,
            MAX_CONCURRENT_DAEMON_CLIENTS,
        )
        .await?;
        return Ok(());
    }
    let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
        match per_client_admission.try_admit_request(&handshake, &first_request_line) {
            Ok(permit) => Some(permit),
            Err(response) => {
                drop(setup_activity);
                reject_admitted_request(&mut transport, &first_request_line, response).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    if let Some(request) = doctor_runtime_request(&first_request_line) {
        let report_ready = if request.doctor_report_requested() {
            let (canonical_project_path, _) = project_route_for_handshake(&handshake)?;
            portable_cached_project_server(
                &store_administration,
                &canonical_project_path,
                &handshake,
            )
            .await?
            .is_some_and(|server| server.doctor_report_ready())
        } else {
            false
        };
        if request.should_serve_from_core(report_ready) {
            drop(setup_activity);
            write_doctor_runtime_response(
                &mut transport,
                &handshake,
                &store_administration,
                request,
            )
            .await?;
            return Ok(());
        }
    }
    ensure_user_profile_host_admission_replay_for_identity(
        &store_administration,
        &handshake.client_identity,
    )
    .await?;
    let initialize_route =
        apply_daemon_initialize_route(&mut handshake, &first_request_line, &store_administration)
            .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => {
                store_administration
                    .execute_branch_admin_for_handshake(&handshake, action)
                    .await
            }
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
        let response = match await_project_owner_or_disconnect(
            &mut transport,
            portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                Arc::clone(&project_open_gates),
                invocation.clone(),
                http_application_registry.clone(),
                &handshake,
                #[cfg(test)]
                project_open_attempts.clone(),
            ),
        )
        .await
        {
            Ok(Some(_)) => branch_add_response(&store_administration, &handshake, &request).await,
            Ok(None) => return Ok(()),
            Err(error) => JsonRpcResponse::error(
                request.id.clone(),
                ErrorCode::InternalError,
                error.to_string(),
            ),
        };
        drop(setup_activity);
        write_json_rpc_response(&mut transport, &response).await?;
        return Ok(());
    }
    if let Some(invocation_request) = parse_daemon_invocation_request(&first_request_line) {
        let mut invocation_request = invocation_request;
        let mut owned_lsp_sessions = HashMap::new();
        let result = async {
            loop {
                let session_transition = invocation_request
                    .as_ref()
                    .ok()
                    .and_then(invocation_lsp_session_transition);
                let response = match invocation_request {
                    Ok(request) => {
                        execute_portable_daemon_invocation(
                            lifecycle.clone(),
                            store_administration.clone(),
                            Arc::clone(&project_open_gates),
                            &handshake,
                            &invocation,
                            http_application_registry.clone(),
                            request,
                            #[cfg(test)]
                            project_open_attempts.clone(),
                        )
                        .await
                    }
                    Err(response) => response,
                };
                update_connection_lsp_sessions(
                    &mut owned_lsp_sessions,
                    session_transition.as_ref(),
                    &response,
                );
                write_daemon_invocation_response(&mut transport, &response).await?;
                let next_line = tokio::select! {
                    result = read_line_handling_wire_oversized(&mut transport) => result?,
                    () = lifecycle.wait_for_draining() => return Ok(()),
                };
                let Some(next_line) = next_line else {
                    return Ok(());
                };
                let Some(next_invocation) = parse_daemon_invocation_request(&next_line) else {
                    return Ok(());
                };
                invocation_request = next_invocation;
            }
        }
        .await;
        cleanup_connection_lsp_sessions(&invocation, owned_lsp_sessions).await;
        return result;
    }
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
        let project_node_count =
            if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                if handshake.project_path.is_some() {
                    cached_project_node_count(&store_administration, &handshake).await
                } else {
                    Some(0)
                }
            } else {
                None
            };
        if let Some(mut response) =
            daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
        {
            let project_open_error = if handshake.project_path.is_some()
                && matches!(
                    classify_mcp_method(&request.method),
                    McpMethod::Initialize | McpMethod::ToolsList
                ) {
                match portable_cached_project_open_failure(project_open_gates.as_ref(), &handshake)
                    .await
                {
                    Ok(Some(failure)) => Some(failure.to_error()),
                    Ok(None)
                        if matches!(
                            classify_mcp_method(&request.method),
                            McpMethod::Initialize
                        ) =>
                    {
                        Box::pin(schedule_portable_project_server_warmup(
                            lifecycle.clone(),
                            store_administration.clone(),
                            Arc::clone(&project_open_gates),
                            invocation.clone(),
                            http_application_registry.clone(),
                            handshake.clone(),
                            request.clone(),
                            #[cfg(test)]
                            project_open_attempts.clone(),
                        ))
                        .await
                        .err()
                    }
                    Ok(None) => None,
                    Err(error) => Some(error),
                }
            } else {
                None
            };
            if let Some(error) = project_open_error {
                response = request
                    .id
                    .clone()
                    .map(|id| project_open_error_response(id, &error));
            }
            drop(setup_activity);
            if let Some(response) = response {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    let user_session_request = projectless_user_session_request(&first_request_line);
    if handshake.project_path.is_some() && !user_session_request {
        let server = match await_project_owner_or_disconnect(
            &mut transport,
            portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                Arc::clone(&project_open_gates),
                invocation.clone(),
                http_application_registry,
                &handshake,
                #[cfg(test)]
                project_open_attempts.clone(),
            ),
        )
        .await
        {
            Ok(Some(server)) => server,
            Ok(None) => {
                drop(setup_activity);
                return Ok(());
            }
            Err(error) => {
                drop(setup_activity);
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Ok(());
            }
        };
        drop(setup_activity);
        let initialize_handled = write_routed_initialize_response(
            &server.0,
            &mut transport,
            &first_request_line,
            initialize_route.as_ref(),
        )
        .await?;
        let mut transport = ReplayTransport::new(transport);
        if !initialize_handled {
            transport.push_replay(first_request_line)?;
        }
        for line in server.1 {
            transport.push_replay(line)?;
        }
        Box::pin(server.0.run_daemon_connection_with_timings(
            &mut transport,
            handshake.timings,
            lifecycle,
        ))
        .await?;
    } else {
        drop(setup_activity);
        let mut transport = ReplayTransport::new(transport);
        transport.push_replay(first_request_line)?;
        Box::pin(serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            lifecycle,
            &store_administration,
        ))
        .await?;
    }
    Ok(())
}

#[cfg(any(not(unix), test))]
async fn execute_portable_daemon_invocation(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    handshake: &DaemonHandshake,
    invocation: &DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    request: DaemonInvocationRequest,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> DaemonInvocationResponse {
    let request_id = request.request_id.clone();
    let git_operation = invocation_is_git_operation(request.operation());
    let mut project_path = None;
    if request.requires_project() {
        if Box::pin(portable_project_server_for_request(
            lifecycle,
            store_administration.clone(),
            project_open_gates,
            invocation.clone(),
            http_application_registry,
            handshake,
            #[cfg(test)]
            project_open_attempts,
        ))
        .await
        .is_err()
        {
            return DaemonInvocationResponse::problem(
                request_id,
                if git_operation {
                    DaemonInvocationProblem::NotFoundOrNotAuthorized
                } else {
                    DaemonInvocationProblem::Unavailable
                },
            );
        }
        let Ok((resolved_project_path, _)) = project_route_for_handshake(handshake) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if admitted_lsp_root_for_project_path(&resolved_project_path).is_none() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        project_path = Some(resolved_project_path);
    }
    invocation
        .invoke_for_project(&store_administration, project_path.as_deref(), request)
        .await
}

async fn git_service_for_project_path(
    store_administration: &StoreAdministration,
    project_path: Option<&Path>,
) -> Option<git_transactions::DaemonGitInvocationOwner> {
    let project_path = project_path?;
    let repository_root = crate::worktree::git_worktree_root(project_path)
        .unwrap_or_else(|| project_path.to_path_buf());
    store_administration
        .git_index_transaction_services()
        .for_repository_root(&repository_root)
        .await
        .ok()
        .flatten()
}

#[cfg(unix)]
async fn write_tool_list_changed_notification(transport: &mut impl McpTransport) -> Result<()> {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": TOOL_LIST_CHANGED_METHOD,
    });
    transport
        .write_line(&format!("{}\n", serde_json::to_string(&notification)?))
        .await?;
    transport.flush().await?;
    Ok(())
}

#[cfg(test)]
async fn open_project_for_handshake(
    project_path: &Path,
    handshake: &DaemonHandshake,
    store_administration: &StoreAdministration,
) -> Result<crate::tracedecay::TraceDecay> {
    let (cg, _) = open_project_for_handshake_with_health_mode(
        project_path,
        handshake,
        store_administration,
        false,
    )
    .await?;
    Ok(cg)
}

async fn open_project_for_handshake_with_health_mode(
    project_path: &Path,
    handshake: &DaemonHandshake,
    store_administration: &StoreAdministration,
    defer_post_open_health: bool,
) -> Result<(crate::tracedecay::TraceDecay, Option<crate::db::Database>)> {
    let open_options = handshake.open_options();
    let registry_database = store_administration.registered_profile_database().await?;
    let (store_layout, first_touch) =
        match crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
            project_path,
            &open_options,
            registry_database.as_ref(),
            true,
        )
        .await
        {
            Ok(layout) => (layout, false),
            // A brand-new project has no enrollment marker, registry match, or
            // legacy shard, so identity resolution fails closed. When the client
            // explicitly asked to initialize (first-touch `tracedecay init`),
            // mint a fresh path-derived identity and let the missing-index
            // fallback below bootstrap it. Existing-but-unresolvable stores
            // raise their own identity-cutover errors instead of this one and
            // still fail closed.
            Err(err) if handshake.allow_init && is_unregistered_identity_error(&err) => (
                crate::tracedecay::TraceDecay::resolve_first_touch_configuration_layout(
                    project_path,
                    &open_options,
                    registry_database.as_ref(),
                    true,
                )
                .await?,
                true,
            ),
            Err(err) if is_unregistered_identity_error(&err) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "no TraceDecay index found at '{}'; run 'tracedecay init' first",
                        project_path.display()
                    ),
                });
            }
            Err(err) => return Err(err),
        };
    let project_id =
        store_layout
            .identity
            .project_id
            .as_deref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "registered project open requires an authoritative project identity"
                    .to_owned(),
            })?;
    // First-touch enrollment: the daemon's registered session runtime resolves
    // a project's store through its on-disk enrollment marker, which a
    // never-seen project does not yet have. Persist it now — under the same
    // minted identity the layout carries — so the session store can mount
    // before init bootstraps the graph. This is the honest first enrollment
    // step, not a bypass: it only runs on the explicit allow_init first-touch
    // path, and a subsequent open resolves this same marker deterministically.
    if first_touch {
        let enrollment_root = crate::worktree::repository_identity_root(project_path)
            .unwrap_or_else(|| project_path.to_path_buf());
        crate::storage::write_enrollment_marker(
            &enrollment_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )?;
    }
    let configuration_database = store_administration
        .registered_project_session_database(project_path, &store_layout)
        .await?;
    let runtime_registry = store_administration.registered_runtime_registry().await?;
    let open_result = if defer_post_open_health {
        crate::tracedecay::TraceDecay::open_with_registered_configuration_deferred_post_open_health(
            project_path,
            open_options.clone(),
            store_layout.clone(),
            Arc::clone(&configuration_database),
            Arc::clone(&registry_database),
            Arc::clone(&runtime_registry),
        )
        .await
    } else {
        crate::tracedecay::TraceDecay::open_with_registered_configuration(
            project_path,
            open_options.clone(),
            store_layout.clone(),
            Arc::clone(&configuration_database),
            Arc::clone(&registry_database),
            Arc::clone(&runtime_registry),
        )
        .await
    };
    match open_result {
        Ok(cg) => {
            let deferred_post_open_health = defer_post_open_health.then(|| cg.db().clone());
            Ok((cg, deferred_post_open_health))
        }
        Err(open_err) if defer_post_open_health && is_readonly_database_error(&open_err) => {
            match crate::tracedecay::TraceDecay::open_read_only_with_registered_configuration(
                project_path,
                open_options,
                store_layout,
                configuration_database,
                registry_database,
                runtime_registry,
            )
            .await
            {
                Ok(cg) => {
                    cg.ensure_schema_current().await?;
                    Ok((cg, None))
                }
                Err(_) => Err(open_err),
            }
        }
        Err(open_err) if handshake.allow_init && is_missing_index_error(&open_err) => {
            // First-touch (or not-yet-indexed) bootstrap: create and index the
            // store under the daemon's authority. Surface the bootstrap error
            // itself on failure — the original "no index found" open error is a
            // misleading symptom that hides the real reason init could not
            // complete.
            crate::tracedecay::TraceDecay::init_and_index_with_registered_configuration(
                project_path,
                open_options,
                store_layout,
                configuration_database,
                registry_database,
                runtime_registry,
            )
            .await
            .map(|cg| (cg, None))
        }
        Err(open_err) => Err(open_err),
    }
}

/// Whether `err` is the specific fail-closed error raised when identity
/// resolution finds no enrollment marker, registry match, or legacy shard for a
/// project — i.e. a genuinely never-enrolled project. Conflicting or ambiguous
/// *existing* stores raise distinct identity-cutover errors and are excluded, so
/// first-touch bootstrap never masks a real conflict.
fn is_unregistered_identity_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Config { message }
            if message.contains(
                "registered configuration layout requires an enrolled or registry-resolved project identity"
            )
    )
}

fn is_missing_index_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Config { message }
            if message.contains("no TraceDecay index found")
                || message.contains("no TraceDecay database found")
                || message.contains("parent DB not found")
                || (message.contains("parent branch '") && message.contains("' has no DB"))
    )
}

fn is_readonly_database_error(err: &TraceDecayError) -> bool {
    if !err.is_database_error() {
        return false;
    }
    match err {
        TraceDecayError::Database { message, .. } => {
            message.to_ascii_lowercase().contains("readonly database")
        }
        #[allow(deprecated)]
        TraceDecayError::DatabaseOperation { source, .. } => source
            .to_string()
            .to_ascii_lowercase()
            .contains("readonly database"),
        _ => false,
    }
}

async fn write_project_open_error(
    transport: &mut impl McpTransport,
    request_line: &str,
    error: &TraceDecayError,
) -> Result<()> {
    let id = serde_json::from_str::<JsonRpcRequest>(request_line)
        .ok()
        .and_then(|request| request.id)
        .unwrap_or(serde_json::Value::Null);
    let response = project_open_error_response(id, error);
    write_json_rpc_response(transport, &response).await
}

fn project_open_error_response(id: serde_json::Value, error: &TraceDecayError) -> JsonRpcResponse {
    match error {
        TraceDecayError::Config { message }
            if message.contains(PROJECT_OPEN_FAILURE_RETRY_HINT) =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_route_open_backoff",
                    "retryable": true,
                    "retry_after_ms": PROJECT_OPEN_FAILURE_RETRY_BACKOFF.as_millis() as u64,
                })),
            )
        }
        TraceDecayError::Config { message }
            if message.starts_with("daemon project open task capacity reached") =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_open_task_capacity_reached",
                    "retryable": true,
                    "capacity": MAX_TRACKED_PROJECT_OPEN_TASKS,
                })),
            )
        }
        TraceDecayError::Config { message }
            if message.starts_with("daemon project server capacity reached") =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_server_capacity_reached",
                    "retryable": true,
                    "capacity": MAX_CACHED_PROJECT_SERVERS,
                })),
            )
        }
        _ => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
    }
}

async fn write_json_rpc_response(
    transport: &mut impl McpTransport,
    response: &crate::mcp::JsonRpcResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

async fn write_daemon_invocation_response(
    transport: &mut impl McpTransport,
    response: &DaemonInvocationResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

fn invocation_lsp_session_transition(
    request: &DaemonInvocationRequest,
) -> Option<service::invocation::DaemonLspSessionAccess> {
    match &request.payload {
        service::invocation::DaemonInvocationPayload::LspReconnect { session }
        | service::invocation::DaemonInvocationPayload::LspDetach { session } => {
            Some(session.clone())
        }
        _ => None,
    }
}

fn update_connection_lsp_sessions(
    sessions: &mut HashMap<String, service::invocation::DaemonLspSessionAccess>,
    transitioned: Option<&service::invocation::DaemonLspSessionAccess>,
    response: &DaemonInvocationResponse,
) {
    match &response.outcome {
        service::invocation::DaemonInvocationOutcome::LspOpened { session, .. } => {
            sessions.insert(session.session_id.clone(), session.clone());
        }
        service::invocation::DaemonInvocationOutcome::LspReconnected { session } => {
            sessions.insert(session.session_id.clone(), session.clone());
        }
        service::invocation::DaemonInvocationOutcome::LspDetached => {
            if let Some(detached) = transitioned {
                sessions.remove(&detached.session_id);
            }
        }
        _ => {}
    }
}

async fn cleanup_connection_lsp_sessions(
    invocation: &DaemonInvocationState,
    sessions: HashMap<String, service::invocation::DaemonLspSessionAccess>,
) {
    for session in sessions.into_values() {
        invocation
            .service
            .disconnect_lsp_session(&invocation.lsp_session_registry, session)
            .await;
    }
}

fn admitted_lsp_root_for_project_path(project_path: &Path) -> Option<lsp_gateway::AdmittedRoot> {
    url::Url::from_file_path(project_path)
        .ok()
        .map(|uri| lsp_gateway::AdmittedRoot::new(uri.to_string()))
}

#[cfg(unix)]
async fn execute_daemon_invocation(
    engine: &DaemonEngine,
    handshake: &DaemonHandshake,
    request: DaemonInvocationRequest,
) -> DaemonInvocationResponse {
    let request_id = request.request_id.clone();
    let git_operation = invocation_is_git_operation(request.operation());
    let mut project_path = None;
    if request.requires_project() {
        if engine.project_server_for_request(handshake).await.is_err() {
            return DaemonInvocationResponse::problem(
                request_id,
                if git_operation {
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized
                } else {
                    service::invocation::DaemonInvocationProblem::Unavailable
                },
            );
        }
        let Ok((resolved_project_path, _)) = DaemonEngine::project_route(handshake) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if admitted_lsp_root_for_project_path(&resolved_project_path).is_none() {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        }
        project_path = Some(resolved_project_path);
    }
    engine
        .invocation
        .invoke_for_project(
            &engine.store_administration,
            project_path.as_deref(),
            request,
        )
        .await
}

/// Read one newline-delimited frame. Oversized input gets a typed non-durable
/// rejection and returns `Ok(None)` without retaining payload bytes.
async fn read_line_handling_wire_oversized(
    transport: &mut impl McpTransport,
) -> Result<Option<String>> {
    match transport.read_line().await {
        Ok(line) => Ok(line),
        Err(error) if crate::application::host_admission::is_wire_oversized_io_error(&error) => {
            let _ = crate::mcp::transport::write_wire_oversized_rejection(transport, &error).await;
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

async fn serve_projectless_client(
    transport: &mut impl McpTransport,
    client_identity: &DaemonClientIdentity,
    lifecycle: &DaemonLifecycle,
    store_administration: &StoreAdministration,
) -> Result<()> {
    loop {
        let line = tokio::select! {
            result = read_line_handling_wire_oversized(transport) => result?,
            () = lifecycle.wait_for_draining() => break,
        };
        let Some(line) = line else {
            break;
        };
        let Some(_activity) = lifecycle.try_enter() else {
            break;
        };
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => {
                projectless_response(&request, client_identity, store_administration).await
            }
            Err(e) => Some(JsonRpcResponse::error(
                json!(null),
                ErrorCode::ParseError,
                format!("Parse error: {e}"),
            )),
        };
        if let Some(response) = response {
            write_json_rpc_response(transport, &response).await?;
        }
        if !lifecycle.accepting() {
            break;
        }
    }
    Ok(())
}

async fn projectless_response(
    request: &crate::mcp::JsonRpcRequest,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> Option<crate::mcp::JsonRpcResponse> {
    let id = request.id.clone()?;
    match request.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {
                        "listChanged": true
                    }
                },
                "serverInfo": {
                    "name": "tracedecay",
                    "version": crate::version::build_version()
                }
            }),
        )),
        "tools/call" => Some(
            projectless_tools_call_response(
                id,
                request.params.as_ref(),
                client_identity,
                store_administration,
            )
            .await,
        ),
        "ping" | "logging/setLevel" => Some(JsonRpcResponse::success(id, json!({}))),
        _ => Some(JsonRpcResponse::error(
            id,
            ErrorCode::MethodNotFound,
            format!("Method not found: {}", request.method),
        )),
    }
}

async fn projectless_tools_call_response(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> crate::mcp::JsonRpcResponse {
    let (tool_name, arguments) = match projectless_tool_call(params) {
        Ok(tool_call) => tool_call,
        Err(message) => {
            return JsonRpcResponse::error(id, ErrorCode::InvalidParams, message.to_string());
        }
    };
    if tool_name == "tracedecay_admin_project" {
        #[derive(serde::Deserialize)]
        #[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
        enum ProjectlessAdminProjectAction {
            AutomationReconcile {
                scope: crate::dashboard::AutomationReconcileScope,
            },
        }

        let request = match serde_json::from_value::<ProjectlessAdminProjectAction>(arguments) {
            Ok(request) => request,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    ErrorCode::InvalidParams,
                    format!("invalid projectless tracedecay_admin_project arguments: {error}"),
                );
            }
        };
        let ProjectlessAdminProjectAction::AutomationReconcile { scope } = request;
        if scope != crate::dashboard::AutomationReconcileScope::Profile {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "project-scoped automation reconciliation requires a project path".to_string(),
            );
        }
        let outcomes = match store_administration
            .reconcile_cached_automation_for_profile(&client_identity.profile_root)
            .await
        {
            Ok(outcomes) => outcomes,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let report = crate::dashboard::ProfileAutomationReconcileReport {
            scope,
            cached_owners: outcomes.len(),
            outcomes,
            uncached_projects:
                crate::dashboard::UncachedProjectReconcileOutcome::DeferredUntilProjectStartup,
        };
        return JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string())
                }]
            }),
        );
    }
    if tool_name == "tracedecay_hook_runtime" {
        let global_db = match store_administration.registered_profile_database().await {
            Ok(global_db) => global_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let session_runtime_registry =
            match store_administration.registered_runtime_registry().await {
                Ok(registry) => registry,
                Err(error) => {
                    return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
                }
            };
        let user_session_db = match store_administration
            .registered_profile_session_database()
            .await
        {
            Ok(database) => database,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let profile_identity = match store_administration.profile_identity() {
            Ok(identity) => identity,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let host_admission_state = match store_administration
            .host_admission_broker(&user_session_db)
            .await
        {
            Ok(state) => state,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let host_admission_broker = match &host_admission_state {
            branch_admin::HostAdmissionBrokerState::Available(broker) => Ok(broker),
            branch_admin::HostAdmissionBrokerState::Unavailable(outcome) => Err(*outcome),
        };
        let refresh_wake = store_administration
            .session_temporal_refresh_schedulers()
            .ensure_profile(
                user_session_db.db_path().to_path_buf(),
                Arc::clone(&user_session_db),
            )
            .await;
        return match crate::mcp::tools::handle_projectless_hook_runtime(
            arguments,
            &client_identity.profile_root,
            session_runtime_registry,
            global_db.as_ref(),
            crate::mcp::tools::SessionAuthorities::new(None, Some(&user_session_db))
                .with_profile_identity(Some(profile_identity))
                .with_registered_databases(None, Some(user_session_db.as_ref())),
            host_admission_broker,
        )
        .await
        {
            Ok(result) => {
                refresh_wake.wake();
                JsonRpcResponse::success(id, result.value)
            }
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    if tool_name == "tracedecay_admin_cli" {
        let global_db = match store_administration.registered_profile_database().await {
            Ok(global_db) => global_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let accounting_db = match store_administration.registered_profile_database().await {
            Ok(database) => database,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        return match crate::mcp::tools::handle_projectless_admin_cli(
            arguments,
            &global_db,
            crate::global_db::global_accounting_enabled().then_some(accounting_db.as_ref()),
            &client_identity.profile_root,
        )
        .await
        {
            Ok(result) => JsonRpcResponse::success(id, result.value),
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    if tool_name.starts_with("tracedecay_lcm_") || tool_name == "tracedecay_message_search" {
        return projectless_user_lcm_tools_call_response(
            id,
            tool_name,
            arguments,
            client_identity,
            store_administration,
        )
        .await;
    }
    if matches!(
        tool_name,
        "tracedecay_fact_store" | "tracedecay_fact_feedback" | "tracedecay_memory_status"
    ) {
        if arguments
            .get("memory_scope")
            .and_then(serde_json::Value::as_str)
            != Some("user")
        {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "projectless memory dispatch requires memory_scope=user".to_string(),
            );
        }
        let runtime_registry = match store_administration.retained_runtime_registry().await {
            Ok(registry) => registry,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        return match crate::mcp::tools::handle_user_memory_tool(
            tool_name,
            arguments,
            runtime_registry.as_ref(),
            &client_identity.profile_root,
        )
        .await
        {
            Ok(result) => JsonRpcResponse::success(id, result.value),
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    JsonRpcResponse::error(
        id,
        ErrorCode::InternalError,
        format!("{tool_name} requires an initialized code project"),
    )
}

async fn projectless_user_lcm_tools_call_response(
    id: serde_json::Value,
    tool_name: &str,
    arguments: serde_json::Value,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> crate::mcp::JsonRpcResponse {
    if arguments
        .get("storage_scope")
        .and_then(serde_json::Value::as_str)
        != Some("user")
    {
        return JsonRpcResponse::error(
            id,
            ErrorCode::InvalidParams,
            "projectless LCM dispatch requires storage_scope=user".to_string(),
        );
    }
    let user_session_db = match store_administration
        .registered_profile_session_database()
        .await
    {
        Ok(database) => database,
        Err(error) => {
            return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
        }
    };
    let profile_identity = match store_administration.profile_identity() {
        Ok(identity) => identity,
        Err(error) => {
            return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
        }
    };
    let refresh_wake = store_administration
        .session_temporal_refresh_schedulers()
        .ensure_profile(
            user_session_db.db_path().to_path_buf(),
            Arc::clone(&user_session_db),
        )
        .await;
    if tool_name == "tracedecay_message_search" {
        // Joining retained temporal projection is part of reopening the mounted
        // profile store. It does not ingest provider history or widen scope.
        let _ = refresh_wake
            .wake_and_wait_until_idle(std::time::Duration::from_secs(5))
            .await;
    }
    let retrieval_calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let retrieval_service = crate::mcp::server::DaemonSessionRetrievalRoot::profile()
        .and_then(|root| root.with_profile_runtime_shard(profile_identity))
        .and_then(|root| {
            crate::mcp::server::DaemonSessionRetrievalService::new_registered(
                Arc::clone(&user_session_db),
                Arc::clone(&user_session_db),
                root,
                Arc::clone(&retrieval_calls),
                Some(refresh_wake.clone()),
            )
        })
        .map(|service| {
            Arc::new(service) as Arc<dyn crate::mcp::tools::SessionRetrievalServicePort>
        });
    let result = crate::mcp::tools::handle_user_lcm_tool_with_retained_authority(
        tool_name,
        arguments.clone(),
        &client_identity.profile_root,
        &user_session_db,
        retrieval_service.as_deref(),
    )
    .await;
    match result {
        Ok(result) => {
            if tool_name == "tracedecay_lcm_preflight"
                && arguments
                    .get("transcript_projection")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            {
                let _ = refresh_wake
                    .wake_and_wait_until_idle(std::time::Duration::from_secs(5))
                    .await;
            } else if matches!(
                tool_name,
                "tracedecay_lcm_preflight"
                    | "tracedecay_lcm_compress"
                    | "tracedecay_lcm_session_boundary"
            ) {
                refresh_wake.wake();
            }
            JsonRpcResponse::success(id, result.value)
        }
        Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
    }
}

fn projectless_tool_call(
    params: Option<&serde_json::Value>,
) -> std::result::Result<(&str, serde_json::Value), &'static str> {
    let Some(params) = params else {
        return Err("missing params for tools/call");
    };
    let Some(tool_name) = params.get("name").and_then(|v| v.as_str()) else {
        return Err("missing 'name' in tools/call params");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Ok((tool_name, arguments))
}

fn projectless_user_session_request(request_line: &str) -> bool {
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(request_line.trim()) else {
        return false;
    };
    if request.method != "tools/call" {
        return false;
    }
    let Ok((tool_name, arguments)) = projectless_tool_call(request.params.as_ref()) else {
        return false;
    };
    (tool_name.starts_with("tracedecay_lcm_") || tool_name == "tracedecay_message_search")
        && arguments
            .get("storage_scope")
            .and_then(serde_json::Value::as_str)
            == Some("user")
}

struct BrokerStreamTransport {
    reader: tokio::io::BufReader<tokio::io::ReadHalf<BrokerStream>>,
    writer: tokio::io::WriteHalf<BrokerStream>,
}

impl BrokerStreamTransport {
    fn new(stream: BrokerStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: tokio::io::BufReader::new(reader),
            writer,
        }
    }
}

impl crate::mcp::McpTransport for BrokerStreamTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        crate::application::host_admission::read_bounded_mcp_line(&mut self.reader).await
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.writer.write_all(line.as_bytes()).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush().await
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod http_application_tests;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod wire_bound_tests {
    use super::{BrokerStreamTransport, read_line_handling_wire_oversized};
    use crate::application::host_admission::{WIRE_RECORD_TOO_LARGE, is_wire_oversized_io_error};
    use crate::mcp::McpTransport;
    use tokio::io::AsyncWriteExt;

    use super::transport::{BrokerListener, BrokerStream, default_loopback_endpoint};

    #[tokio::test]
    async fn broker_transport_streams_hostile_frame_and_typed_rejection_has_no_payload() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");

        let client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut server_transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            let mut client = client;
            // Stream hostile bytes without pre-building a MAX+1 String in the
            // product reader path; allocate only a small chunk buffer here.
            let chunk = vec![b'w'; 8192];
            let mut remaining =
                crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES + 64 * 1024;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client.write_all(&chunk[..n]).await.expect("write");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("newline");
            client.flush().await.expect("flush");
        });

        let err = server_transport.read_line().await.expect_err("oversized");
        assert!(is_wire_oversized_io_error(&err));
        assert_eq!(err.to_string(), WIRE_RECORD_TOO_LARGE);
        // Reason code is `wire_record_too_large` (contains 'w'); assert the
        // hostile fill pattern itself is not echoed.
        assert!(!err.to_string().contains("wwww"));
        writer.await.expect("writer");
    }

    #[tokio::test]
    async fn broker_transport_accepts_exact_cap_and_recovers_next_frame_after_oversize() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");

        let client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut server_transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            let mut client = client;
            let chunk = vec![b'a'; 8192];
            let mut remaining = crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client.write_all(&chunk[..n]).await.expect("write exact");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("exact newline");

            let chunk = vec![b'z'; 8192];
            let mut remaining = crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES + 1;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client
                    .write_all(&chunk[..n])
                    .await
                    .expect("write oversized");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("oversized newline");
            client
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n")
                .await
                .expect("next frame");
            client.flush().await.expect("flush");
        });

        assert_eq!(
            server_transport
                .read_line()
                .await
                .expect("exact accepted")
                .expect("exact line")
                .len(),
            crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
        );
        let error = server_transport
            .read_line()
            .await
            .expect_err("one over rejected");
        assert!(is_wire_oversized_io_error(&error));
        assert_eq!(
            server_transport
                .read_line()
                .await
                .expect("next read")
                .as_deref(),
            Some(r#"{"jsonrpc":"2.0","method":"ping"}"#)
        );
        writer.await.expect("writer");
    }

    #[tokio::test]
    async fn read_line_handling_writes_typed_rejection_without_payload_bytes() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");

        let mut client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut server_transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            let prefix =
                br#"{"jsonrpc":"2.0","id":"daemon-7","method":"tools/call","params":{"payload":""#;
            client.write_all(prefix).await.expect("prefix");
            let chunk = vec![b'q'; 4096];
            let mut remaining = crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
                + 32 * 1024
                - prefix.len();
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client.write_all(&chunk[..n]).await.expect("write");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("newline");
            client.flush().await.expect("flush");
            client
        });

        let outcome = read_line_handling_wire_oversized(&mut server_transport)
            .await
            .expect("typed handling");
        assert!(outcome.is_none());

        let mut client = writer.await.expect("writer");
        let mut response = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut client, &mut buf)
                .await
                .expect("read rejection");
            if n == 0 {
                break;
            }
            response.extend_from_slice(&buf[..n]);
            if response.contains(&b'\n') {
                break;
            }
        }
        let response: serde_json::Value =
            serde_json::from_slice(&response).expect("JSON-RPC rejection");
        assert_eq!(response["id"], serde_json::json!("daemon-7"));
        assert_eq!(response["error"]["code"], serde_json::json!(-32600));
        assert_eq!(
            response["error"]["message"],
            serde_json::json!(WIRE_RECORD_TOO_LARGE)
        );
        assert!(!response.to_string().contains('q'));
    }
}
