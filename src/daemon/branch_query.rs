//! Daemon-owned branch search and diff execution.
//!
//! The resolver re-proves one registered project and branch graph scope,
//! narrows source authorization to the exact repository/worktree/ref scope,
//! and opens the read-only graph through the retained runtime registry. Query
//! execution then uses the canonical [`GraphRuntimePort`] adapter only.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracedecay_application::{
    BRANCH_DIFF_CAPABILITY_ID_V1, BRANCH_SEARCH_CAPABILITY_ID_V1, BranchChangedSymbolV1,
    BranchDiffRequestV1, BranchDiffResultV1, BranchDiffSummaryV1, BranchDiffSymbolV1,
    BranchGraphGenerationV1, BranchQueryControlsV1, BranchQueryFuture, BranchQueryOutcomeV1,
    BranchQueryPartialReasonV1, BranchQueryPort, BranchQueryRequestV1, BranchQueryResultV1,
    BranchQueryUnavailableReasonV1, BranchSearchMatchV1, BranchSearchRequestV1,
    BranchSearchResultV1, BranchSnapshotIdentityV1, ResolvedScope,
};
use tracedecay_domain::{
    CanonicalGitRefNameV1, CommitId, ProjectId, RefId, UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::CapabilityId;
use tracedecay_usecases::tracedecay::GraphRuntimePort;

use super::project_open_owners::{
    daemon_owned_project_source_access_at, resolved_scope_for_project,
};
use crate::global_db::{GraphScopeRecord, RegisteredGlobalDb};
use crate::tracedecay::TraceDecay;
use crate::types::Node;

const BRANCH_QUERY_DEFAULT_DEADLINE_MICROS: i64 = 30_000_000;
const BRANCH_GRAPH_GENERATION_DOMAIN_V1: &str = "tracedecay.daemon.branch-graph-generation.v1";

type BranchGraphFuture<'a, T> = Pin<Box<dyn Future<Output = crate::errors::Result<T>> + Send + 'a>>;
type BranchMarkerFuture<'a> = Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>;
type BranchResolutionFuture<'a> =
    Pin<Box<dyn Future<Output = BranchResolutionOutcome> + Send + 'a>>;
type BranchRevalidationFuture<'a> =
    Pin<Box<dyn Future<Output = BranchRevalidationOutcome> + Send + 'a>>;
type ComparisonTargetsFuture<'a> = Pin<
    Box<dyn Future<Output = Result<(String, String), BranchQueryUnavailableReasonV1>> + Send + 'a>,
>;

trait BranchGraphReadPort: Send + Sync {
    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> BranchGraphFuture<'a, Vec<BranchSearchMatchV1>>;

    fn all_nodes(&self) -> BranchGraphFuture<'_, Vec<BranchGraphSymbol>>;

    fn source_commit(&self) -> BranchMarkerFuture<'_>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BranchGraphSymbol {
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
    signature: Option<String>,
}

#[derive(Clone)]
struct TraceDecayBranchGraph {
    graph: Arc<TraceDecay>,
}

impl BranchGraphReadPort for TraceDecayBranchGraph {
    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> BranchGraphFuture<'a, Vec<BranchSearchMatchV1>> {
        Box::pin(async move {
            GraphRuntimePort::search(self.graph.as_ref(), query, limit)
                .await
                .map(|results| {
                    results
                        .into_iter()
                        .map(|result| BranchSearchMatchV1 {
                            id: result.node.id,
                            name: result.node.name,
                            kind: result.node.kind.as_str().to_owned(),
                            file: result.node.file_path,
                            line: result.node.start_line,
                            signature: result.node.signature,
                            score: result.score,
                        })
                        .collect()
                })
        })
    }

    fn all_nodes(&self) -> BranchGraphFuture<'_, Vec<BranchGraphSymbol>> {
        Box::pin(async move {
            let nodes = GraphRuntimePort::get_all_nodes(self.graph.as_ref()).await?;
            let mut symbols = Vec::with_capacity(nodes.len());
            for node in nodes {
                symbols.push(BranchGraphSymbol::from(node));
                if symbols.len() % 256 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            Ok(symbols)
        })
    }

    fn source_commit(&self) -> BranchMarkerFuture<'_> {
        GraphRuntimePort::last_synced_commit(self.graph.as_ref())
    }
}

impl From<Node> for BranchGraphSymbol {
    fn from(node: Node) -> Self {
        Self {
            name: node.name,
            qualified_name: node.qualified_name,
            kind: node.kind.as_str().to_owned(),
            file: node.file_path,
            line: node.start_line,
            signature: node.signature,
        }
    }
}

#[derive(Clone)]
struct ResolvedBranchSnapshot {
    identity: BranchSnapshotIdentityV1,
    registered_scope: GraphScopeRecord,
    graph: Arc<dyn BranchGraphReadPort>,
}

enum BranchResolutionOutcome {
    Resolved(ResolvedBranchSnapshot),
    Denied,
    Unavailable(BranchQueryUnavailableReasonV1),
}

#[derive(Clone, Copy)]
enum BranchRevalidationOutcome {
    Current,
    Denied,
    Unavailable(BranchQueryUnavailableReasonV1),
}

trait BranchSnapshotResolver: Send + Sync {
    fn comparison_targets<'a>(
        &'a self,
        request: &'a BranchDiffRequestV1,
    ) -> ComparisonTargetsFuture<'a>;

    fn resolve<'a>(
        &'a self,
        branch: &'a str,
        capability: &'static str,
    ) -> BranchResolutionFuture<'a>;

    fn revalidate<'a>(
        &'a self,
        snapshot: &'a ResolvedBranchSnapshot,
        capability: &'static str,
    ) -> BranchRevalidationFuture<'a>;
}

struct RegisteredBranchSnapshotResolver {
    active_graph: Arc<TraceDecay>,
    registry: Arc<RegisteredGlobalDb>,
    project_scope: ResolvedScope,
    route_registered: Arc<AtomicBool>,
}

impl RegisteredBranchSnapshotResolver {
    async fn registry_context(
        &self,
    ) -> Result<crate::global_db::ProjectRegistryContext, BranchQueryUnavailableReasonV1> {
        if !self.route_registered.load(Ordering::Acquire) {
            return Err(BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable);
        }
        let by_id = self
            .registry
            .project_registry_context_by_id(self.project_scope.project_id.as_str())
            .await
            .map_err(|_| BranchQueryUnavailableReasonV1::RegistryUnavailable)?
            .ok_or(BranchQueryUnavailableReasonV1::ProjectUnavailable)?;
        let git_common_dir = crate::worktree::git_common_dir(self.active_graph.project_root());
        let by_identity = self
            .registry
            .project_registry_context_by_identity(
                self.active_graph.project_root(),
                git_common_dir.as_deref(),
            )
            .await
            .map_err(|_| BranchQueryUnavailableReasonV1::RegistryUnavailable)?
            .ok_or(BranchQueryUnavailableReasonV1::ProjectUnavailable)?;
        if by_id.project.project_id != by_identity.project.project_id
            || by_id.project.project_id != self.project_scope.project_id.as_str()
        {
            return Err(BranchQueryUnavailableReasonV1::ProjectUnavailable);
        }
        Ok(by_id)
    }

    fn branch_scope(&self, branch: &str) -> Result<ResolvedScope, BranchQueryUnavailableReasonV1> {
        let reference = CanonicalGitRefNameV1::new(format!("refs/heads/{branch}"))
            .and_then(|reference| RefId::new(reference.as_str().to_owned()))
            .map_err(|_| BranchQueryUnavailableReasonV1::BranchUnavailable)?;
        ResolvedScope::new(
            self.project_scope.project_id.clone(),
            self.project_scope.repository_id.clone(),
            self.project_scope.worktree_id.clone(),
            Some(reference),
        )
        .map_err(|_| BranchQueryUnavailableReasonV1::BranchUnavailable)
    }

    async fn authorize(
        &self,
        scope: &ResolvedScope,
        capability: &'static str,
    ) -> Result<bool, BranchQueryUnavailableReasonV1> {
        let observed_at = tracedecay_application::now_micros();
        let configuration = self
            .active_graph
            .configuration_runtime()
            .client()
            .current()
            .await
            .map_err(|_| BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable)?;
        let access = match daemon_owned_project_source_access_at(
            scope,
            self.active_graph.project_root(),
            &configuration,
            observed_at,
        ) {
            Ok(access) => access,
            Err(_) => {
                return Err(BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable);
            }
        };
        let capability = CapabilityId::new(capability)
            .map_err(|_| BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable)?;
        Ok(access.effective_capabilities.contains(&capability)
            && observed_at < access.grant_expires_at)
    }

    fn exact_graph_scope(
        context: &crate::global_db::ProjectRegistryContext,
        branch: &str,
    ) -> Result<GraphScopeRecord, BranchQueryUnavailableReasonV1> {
        let mut matches = context
            .stores
            .iter()
            .filter(|store| store.store.project_id == context.project.project_id)
            .flat_map(|store| {
                store
                    .graph_scopes
                    .iter()
                    .filter(|scope| scope.store_id == store.store.store_id)
            })
            .filter(|scope| scope.project_id == context.project.project_id && scope.writable)
            .filter(|scope| scope.branch_name == branch);
        let Some(scope) = matches.next().cloned() else {
            return Err(BranchQueryUnavailableReasonV1::BranchUnavailable);
        };
        if matches.next().is_some() {
            return Err(BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable);
        }
        Ok(scope)
    }

    async fn open_graph(
        &self,
        branch: &str,
        registered_scope: &GraphScopeRecord,
    ) -> Result<Arc<TraceDecay>, BranchQueryUnavailableReasonV1> {
        let graph = TraceDecay::open_branch_with_registered_configuration(
            self.active_graph.project_root(),
            branch,
            self.active_graph.open_options(),
            self.active_graph.store_layout().clone(),
            self.active_graph
                .configuration_runtime()
                .registered_database(),
            self.active_graph.profile_database().clone(),
            self.active_graph.store_runtime_registry().clone(),
        )
        .await
        .map(Arc::new)
        .map_err(|_| BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable)?;
        let profile_root = self
            .active_graph
            .profile_database()
            .db_path()
            .parent()
            .ok_or(BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable)?;
        if profile_root.join(&registered_scope.db_relpath) != graph.db_path() {
            return Err(BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable);
        }
        Ok(graph)
    }

    fn generation(
        scope: &GraphScopeRecord,
        source_commit: String,
    ) -> Result<BranchGraphGenerationV1, BranchQueryUnavailableReasonV1> {
        if scope.graph_scope_id.is_empty() || scope.graph_scope_id.chars().any(char::is_control) {
            return Err(BranchQueryUnavailableReasonV1::GenerationUnavailable);
        }
        let source_commit = CommitId::new(source_commit)
            .map_err(|_| BranchQueryUnavailableReasonV1::GenerationUnavailable)?;
        let recorded_sync_at = match scope.last_synced_at {
            Some(seconds) if seconds >= 0 => {
                Some(UtcMicros(seconds.checked_mul(1_000_000).ok_or(
                    BranchQueryUnavailableReasonV1::GenerationUnavailable,
                )?))
            }
            Some(_) => return Err(BranchQueryUnavailableReasonV1::GenerationUnavailable),
            None => None,
        };
        let generation_digest = canonical_sha256(&(
            BRANCH_GRAPH_GENERATION_DOMAIN_V1,
            &scope.graph_scope_id,
            &scope.project_id,
            &scope.store_id,
            &scope.branch_name,
            &scope.db_relpath,
            scope.last_synced_at,
            &source_commit,
        ))
        .map_err(|_| BranchQueryUnavailableReasonV1::GenerationUnavailable)?;
        Ok(BranchGraphGenerationV1 {
            graph_scope_id: scope.graph_scope_id.clone(),
            source_commit,
            recorded_sync_at,
            generation_digest,
        })
    }

    async fn current_generation(
        graph: &dyn BranchGraphReadPort,
        scope: &GraphScopeRecord,
    ) -> Result<BranchGraphGenerationV1, BranchQueryUnavailableReasonV1> {
        let source_commit = graph
            .source_commit()
            .await
            .filter(|commit| !commit.is_empty())
            .ok_or(BranchQueryUnavailableReasonV1::GenerationUnavailable)?;
        Self::generation(scope, source_commit)
    }
}

impl BranchSnapshotResolver for RegisteredBranchSnapshotResolver {
    fn comparison_targets<'a>(
        &'a self,
        request: &'a BranchDiffRequestV1,
    ) -> ComparisonTargetsFuture<'a> {
        Box::pin(async move {
            let context = self.registry_context().await?;
            let base = request
                .base
                .clone()
                .or(context.project.default_branch)
                .ok_or(BranchQueryUnavailableReasonV1::BranchUnavailable)?;
            let head = request
                .head
                .clone()
                .or_else(|| {
                    self.project_scope
                        .reference
                        .as_ref()
                        .and_then(|reference| reference.as_str().strip_prefix("refs/heads/"))
                        .map(str::to_owned)
                })
                .ok_or(BranchQueryUnavailableReasonV1::BranchUnavailable)?;
            Ok((base, head))
        })
    }

    fn resolve<'a>(
        &'a self,
        branch: &'a str,
        capability: &'static str,
    ) -> BranchResolutionFuture<'a> {
        Box::pin(async move {
            let context = match self.registry_context().await {
                Ok(context) => context,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            let scope = match self.branch_scope(branch) {
                Ok(scope) => scope,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            match self.authorize(&scope, capability).await {
                Ok(true) => {}
                Ok(false) => return BranchResolutionOutcome::Denied,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            }
            let registered_scope = match Self::exact_graph_scope(&context, branch) {
                Ok(scope) => scope,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            let graph = match self.open_graph(branch, &registered_scope).await {
                Ok(graph) => graph,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            let graph: Arc<dyn BranchGraphReadPort> = Arc::new(TraceDecayBranchGraph { graph });
            let generation = match Self::current_generation(graph.as_ref(), &registered_scope).await
            {
                Ok(generation) => generation,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            let Some(reference) = scope.reference.clone() else {
                return BranchResolutionOutcome::Unavailable(
                    BranchQueryUnavailableReasonV1::BranchUnavailable,
                );
            };
            BranchResolutionOutcome::Resolved(ResolvedBranchSnapshot {
                identity: BranchSnapshotIdentityV1 {
                    project_id: scope.project_id,
                    repository_id: scope.repository_id,
                    worktree_id: scope.worktree_id,
                    reference,
                    scope_digest: scope.scope_digest,
                    generation,
                },
                registered_scope,
                graph,
            })
        })
    }

    fn revalidate<'a>(
        &'a self,
        snapshot: &'a ResolvedBranchSnapshot,
        capability: &'static str,
    ) -> BranchRevalidationFuture<'a> {
        Box::pin(async move {
            let Some(branch) = snapshot
                .identity
                .reference
                .as_str()
                .strip_prefix("refs/heads/")
            else {
                return BranchRevalidationOutcome::Unavailable(
                    BranchQueryUnavailableReasonV1::BranchUnavailable,
                );
            };
            let context = match self.registry_context().await {
                Ok(context) => context,
                Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
            };
            let scope = match self.branch_scope(branch) {
                Ok(scope) => scope,
                Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
            };
            match self.authorize(&scope, capability).await {
                Ok(true) => {}
                Ok(false) => return BranchRevalidationOutcome::Denied,
                Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
            }
            let registered_scope = match Self::exact_graph_scope(&context, branch) {
                Ok(scope) => scope,
                Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
            };
            let generation =
                match Self::current_generation(snapshot.graph.as_ref(), &registered_scope).await {
                    Ok(generation) => generation,
                    Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
                };
            if registered_scope != snapshot.registered_scope
                || scope.scope_digest != snapshot.identity.scope_digest
                || generation != snapshot.identity.generation
            {
                return BranchRevalidationOutcome::Unavailable(
                    BranchQueryUnavailableReasonV1::GenerationDrift,
                );
            }
            BranchRevalidationOutcome::Current
        })
    }
}

pub(super) fn daemon_branch_query_port(
    active_graph: Arc<TraceDecay>,
    registry: Arc<RegisteredGlobalDb>,
    route_registered: Arc<AtomicBool>,
) -> Result<Arc<dyn BranchQueryPort>, crate::errors::TraceDecayError> {
    let project_id = active_graph
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: "branch query authority requires an authoritative project identity".to_owned(),
        })
        .and_then(|project_id| {
            ProjectId::new(project_id.to_owned()).map_err(|error| {
                crate::errors::TraceDecayError::Config {
                    message: format!("branch query project identity is invalid: {error}"),
                }
            })
        })?;
    let project_scope = resolved_scope_for_project(active_graph.project_root(), &project_id)
        .map_err(|error| crate::errors::TraceDecayError::Config {
            message: format!("branch query project scope is invalid: {error}"),
        })?;
    Ok(Arc::new(DaemonBranchQueryExecutor {
        resolver: Arc::new(RegisteredBranchSnapshotResolver {
            active_graph,
            registry,
            project_scope,
            route_registered,
        }),
    }))
}

struct DaemonBranchQueryExecutor {
    resolver: Arc<dyn BranchSnapshotResolver>,
}

impl BranchQueryPort for DaemonBranchQueryExecutor {
    fn execute<'a>(
        &'a self,
        request: BranchQueryRequestV1,
        controls: BranchQueryControlsV1,
    ) -> BranchQueryFuture<'a> {
        Box::pin(async move {
            if request.validate().is_err() {
                return BranchQueryOutcomeV1::Unavailable {
                    reason: BranchQueryUnavailableReasonV1::InvalidRequest,
                };
            }
            let control = QueryControl::new(controls);
            if let Some(terminal) = control.terminal() {
                return terminal.outcome();
            }
            match request {
                BranchQueryRequestV1::Search(request) => {
                    self.execute_search(request, &control).await
                }
                BranchQueryRequestV1::Diff(request) => self.execute_diff(request, &control).await,
            }
        })
    }
}

impl DaemonBranchQueryExecutor {
    async fn execute_search(
        &self,
        request: BranchSearchRequestV1,
        control: &QueryControl,
    ) -> BranchQueryOutcomeV1 {
        let snapshot = match controlled(
            self.resolver
                .resolve(&request.branch, BRANCH_SEARCH_CAPABILITY_ID_V1),
            control,
        )
        .await
        {
            Controlled::Value(BranchResolutionOutcome::Resolved(snapshot)) => snapshot,
            Controlled::Value(BranchResolutionOutcome::Denied) => {
                return BranchQueryOutcomeV1::Denied;
            }
            Controlled::Value(BranchResolutionOutcome::Unavailable(reason)) => {
                return BranchQueryOutcomeV1::Unavailable { reason };
            }
            Controlled::Terminal(terminal) => return terminal.outcome(),
        };
        let mut items = match controlled(
            snapshot
                .graph
                .search(&request.query, request.limit as usize + 1),
            control,
        )
        .await
        {
            Controlled::Value(Ok(items)) => items,
            Controlled::Value(Err(_)) => {
                return BranchQueryOutcomeV1::Unavailable {
                    reason: BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable,
                };
            }
            Controlled::Terminal(terminal) => return terminal.outcome(),
        };
        match controlled(
            self.resolver
                .revalidate(&snapshot, BRANCH_SEARCH_CAPABILITY_ID_V1),
            control,
        )
        .await
        {
            Controlled::Value(BranchRevalidationOutcome::Current) => {}
            Controlled::Value(BranchRevalidationOutcome::Denied) => {
                return BranchQueryOutcomeV1::Denied;
            }
            Controlled::Value(BranchRevalidationOutcome::Unavailable(reason)) => {
                return BranchQueryOutcomeV1::Unavailable { reason };
            }
            Controlled::Terminal(terminal) => return terminal.outcome(),
        }
        if let Some(terminal) = control.terminal() {
            return terminal.outcome();
        }
        let truncated = items.len() > request.limit as usize;
        items.truncate(request.limit as usize);
        let result = BranchQueryResultV1::Search(BranchSearchResultV1 {
            snapshot: snapshot.identity,
            items,
        });
        if truncated {
            BranchQueryOutcomeV1::Partial {
                result,
                reason: BranchQueryPartialReasonV1::ResultLimitReached,
            }
        } else {
            BranchQueryOutcomeV1::Complete { result }
        }
    }

    async fn execute_diff(
        &self,
        request: BranchDiffRequestV1,
        control: &QueryControl,
    ) -> BranchQueryOutcomeV1 {
        let (base, head) =
            match controlled(self.resolver.comparison_targets(&request), control).await {
                Controlled::Value(Ok(targets)) => targets,
                Controlled::Value(Err(reason)) => {
                    return BranchQueryOutcomeV1::Unavailable { reason };
                }
                Controlled::Terminal(terminal) => return terminal.outcome(),
            };
        let base_snapshot = match controlled(
            self.resolver.resolve(&base, BRANCH_DIFF_CAPABILITY_ID_V1),
            control,
        )
        .await
        {
            Controlled::Value(BranchResolutionOutcome::Resolved(snapshot)) => snapshot,
            Controlled::Value(BranchResolutionOutcome::Denied) => {
                return BranchQueryOutcomeV1::Denied;
            }
            Controlled::Value(BranchResolutionOutcome::Unavailable(reason)) => {
                return BranchQueryOutcomeV1::Unavailable { reason };
            }
            Controlled::Terminal(terminal) => return terminal.outcome(),
        };
        let head_snapshot = if base == head {
            base_snapshot.clone()
        } else {
            match controlled(
                self.resolver.resolve(&head, BRANCH_DIFF_CAPABILITY_ID_V1),
                control,
            )
            .await
            {
                Controlled::Value(BranchResolutionOutcome::Resolved(snapshot)) => snapshot,
                Controlled::Value(BranchResolutionOutcome::Denied) => {
                    return BranchQueryOutcomeV1::Denied;
                }
                Controlled::Value(BranchResolutionOutcome::Unavailable(reason)) => {
                    return BranchQueryOutcomeV1::Unavailable { reason };
                }
                Controlled::Terminal(terminal) => return terminal.outcome(),
            }
        };
        let (base_nodes, head_nodes) = if base == head {
            (Vec::new(), Vec::new())
        } else {
            let base_nodes = match controlled(base_snapshot.graph.all_nodes(), control).await {
                Controlled::Value(Ok(nodes)) => nodes,
                Controlled::Value(Err(_)) => {
                    return BranchQueryOutcomeV1::Unavailable {
                        reason: BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable,
                    };
                }
                Controlled::Terminal(terminal) => return terminal.outcome(),
            };
            let head_nodes = match controlled(head_snapshot.graph.all_nodes(), control).await {
                Controlled::Value(Ok(nodes)) => nodes,
                Controlled::Value(Err(_)) => {
                    return BranchQueryOutcomeV1::Unavailable {
                        reason: BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable,
                    };
                }
                Controlled::Terminal(terminal) => return terminal.outcome(),
            };
            (base_nodes, head_nodes)
        };
        for snapshot in [&base_snapshot, &head_snapshot] {
            match controlled(
                self.resolver
                    .revalidate(snapshot, BRANCH_DIFF_CAPABILITY_ID_V1),
                control,
            )
            .await
            {
                Controlled::Value(BranchRevalidationOutcome::Current) => {}
                Controlled::Value(BranchRevalidationOutcome::Denied) => {
                    return BranchQueryOutcomeV1::Denied;
                }
                Controlled::Value(BranchRevalidationOutcome::Unavailable(reason)) => {
                    return BranchQueryOutcomeV1::Unavailable { reason };
                }
                Controlled::Terminal(terminal) => return terminal.outcome(),
            }
        }
        let (added, removed, changed) = match diff_symbols(
            base_nodes,
            head_nodes,
            request.file.as_deref(),
            request.kind.as_deref(),
            control,
        ) {
            Ok(diff) => diff,
            Err(terminal) => return terminal.outcome(),
        };
        if let Some(terminal) = control.terminal() {
            return terminal.outcome();
        }
        let result = BranchQueryResultV1::Diff(BranchDiffResultV1 {
            base: base_snapshot.identity,
            head: head_snapshot.identity,
            note: (base == head).then(|| format!("base and head are the same branch: '{base}'")),
            summary: BranchDiffSummaryV1 {
                added: added.len() as u64,
                removed: removed.len() as u64,
                changed: changed.len() as u64,
            },
            added,
            removed,
            changed,
        });
        BranchQueryOutcomeV1::Complete { result }
    }
}

fn diff_symbols(
    base_nodes: Vec<BranchGraphSymbol>,
    head_nodes: Vec<BranchGraphSymbol>,
    file_filter: Option<&str>,
    kind_filter: Option<&str>,
    control: &QueryControl,
) -> Result<
    (
        Vec<BranchDiffSymbolV1>,
        Vec<BranchDiffSymbolV1>,
        Vec<BranchChangedSymbolV1>,
    ),
    QueryTerminal,
> {
    let admitted = |symbol: &BranchGraphSymbol| {
        file_filter.is_none_or(|filter| symbol.file == filter || symbol.file.starts_with(filter))
            && kind_filter.is_none_or(|filter| symbol.kind == filter)
    };
    let mut base = BTreeMap::new();
    for symbol in base_nodes {
        if let Some(terminal) = control.terminal() {
            return Err(terminal);
        }
        if admitted(&symbol) {
            base.insert((symbol.file.clone(), symbol.qualified_name.clone()), symbol);
        }
    }
    let mut head = BTreeMap::new();
    for symbol in head_nodes {
        if let Some(terminal) = control.terminal() {
            return Err(terminal);
        }
        if admitted(&symbol) {
            head.insert((symbol.file.clone(), symbol.qualified_name.clone()), symbol);
        }
    }
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (identity, symbol) in &head {
        if let Some(terminal) = control.terminal() {
            return Err(terminal);
        }
        match base.get(identity) {
            None => added.push(diff_symbol(symbol)),
            Some(base_symbol) if base_symbol.signature != symbol.signature => {
                changed.push(BranchChangedSymbolV1 {
                    name: symbol.name.clone(),
                    qualified_name: symbol.qualified_name.clone(),
                    kind: symbol.kind.clone(),
                    file: symbol.file.clone(),
                    line: symbol.line,
                    base_signature: base_symbol.signature.clone(),
                    head_signature: symbol.signature.clone(),
                });
            }
            Some(_) => {}
        }
    }
    for (identity, symbol) in &base {
        if let Some(terminal) = control.terminal() {
            return Err(terminal);
        }
        if !head.contains_key(identity) {
            removed.push(diff_symbol(symbol));
        }
    }
    Ok((added, removed, changed))
}

fn diff_symbol(symbol: &BranchGraphSymbol) -> BranchDiffSymbolV1 {
    BranchDiffSymbolV1 {
        name: symbol.name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        kind: symbol.kind.clone(),
        file: symbol.file.clone(),
        line: symbol.line,
        signature: symbol.signature.clone(),
    }
}

#[derive(Clone, Copy)]
enum QueryTerminal {
    Cancelled,
    TimedOut,
}

impl QueryTerminal {
    const fn outcome(self) -> BranchQueryOutcomeV1 {
        match self {
            Self::Cancelled => BranchQueryOutcomeV1::Cancelled,
            Self::TimedOut => BranchQueryOutcomeV1::TimedOut,
        }
    }
}

struct QueryControl {
    deadline: tracedecay_application::Deadline,
    cancellation: Option<tracedecay_application::CancellationSignal>,
}

impl QueryControl {
    fn new(controls: BranchQueryControlsV1) -> Self {
        let now = tracedecay_application::now_micros();
        Self {
            deadline: controls
                .deadline
                .unwrap_or(tracedecay_application::Deadline {
                    expires_at: UtcMicros(
                        now.0.saturating_add(BRANCH_QUERY_DEFAULT_DEADLINE_MICROS),
                    ),
                }),
            cancellation: controls.cancellation,
        }
    }

    fn terminal(&self) -> Option<QueryTerminal> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(tracedecay_application::CancellationSignal::is_cancelled)
        {
            Some(QueryTerminal::Cancelled)
        } else if self
            .deadline
            .is_elapsed_at(tracedecay_application::now_micros())
        {
            Some(QueryTerminal::TimedOut)
        } else {
            None
        }
    }

    fn remaining(&self) -> Duration {
        let remaining = self
            .deadline
            .expires_at
            .0
            .saturating_sub(tracedecay_application::now_micros().0)
            .max(0) as u64;
        Duration::from_micros(remaining)
    }
}

enum Controlled<T> {
    Value(T),
    Terminal(QueryTerminal),
}

async fn controlled<T>(future: impl Future<Output = T>, control: &QueryControl) -> Controlled<T> {
    if let Some(terminal) = control.terminal() {
        return Controlled::Terminal(terminal);
    }
    let future = future;
    tokio::pin!(future);
    let deadline = tokio::time::sleep(control.remaining());
    tokio::pin!(deadline);
    if control.cancellation.is_none() {
        return tokio::select! {
            value = &mut future => Controlled::Value(value),
            () = &mut deadline => Controlled::Terminal(QueryTerminal::TimedOut),
        };
    }
    let mut cancellation_poll = tokio::time::interval(Duration::from_millis(10));
    cancellation_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            value = &mut future => return Controlled::Value(value),
            () = &mut deadline => return Controlled::Terminal(QueryTerminal::TimedOut),
            _ = cancellation_poll.tick() => {
                if control.cancellation.as_ref().is_some_and(
                    tracedecay_application::CancellationSignal::is_cancelled,
                ) {
                    return Controlled::Terminal(QueryTerminal::Cancelled);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "branch_query_tests.rs"]
mod tests;
