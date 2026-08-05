//! Daemon-owned branch search and diff execution.
//!
//! The resolver re-proves one registered project and branch graph scope,
//! narrows source authorization to the exact repository/worktree/ref scope,
//! and opens the read-only graph through the retained runtime registry. Query
//! execution then uses the canonical [`GraphRuntimePort`] adapter only.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_application::{
    BRANCH_DIFF_CAPABILITY_ID_V1, BRANCH_SEARCH_CAPABILITY_ID_V1, BranchAuthorizationEpochV1,
    BranchDiffRequestV1, BranchGraphGenerationV1, BranchQueryPort, BranchQueryStaleReasonV1,
    BranchQueryUnavailableReasonV1, BranchSearchMatchV1, BranchSnapshotIdentityV1, ResolvedScope,
};
use tracedecay_domain::{
    CanonicalGitRefNameV1, GitOidV1, ManifestDigest, ProjectId, RefId, RepositoryId,
    SignedCursorKeyRefV1, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_temporal_query::ports::SessionCursorAuthenticator;
use tracedecay_tool_catalog::CapabilityId;
use tracedecay_usecases::{
    source_authorization::ProjectSourceAccessSnapshot, tracedecay::GraphRuntimePort,
};

use super::project_open_owners::{
    daemon_owned_project_source_access_at, resolved_scope_for_project,
};
use crate::global_db::{GraphScopeRecord, RegisteredGlobalDb};
use crate::tracedecay::TraceDecay;
use crate::types::Node;

const BRANCH_QUERY_DEFAULT_DEADLINE_MICROS: i64 = 30_000_000;
const BRANCH_GRAPH_GENERATION_DOMAIN_V1: &str = "tracedecay.daemon.branch-graph-generation.v1";
const BRANCH_QUERY_BINDING_DOMAIN_V1: &str = "tracedecay.daemon.branch-query-binding.v1";
const BRANCH_AUTHORIZATION_EPOCH_DOMAIN_V1: &str =
    "tracedecay.daemon.branch-authorization-epoch.v1";
const BRANCH_SEARCH_CANDIDATE_LIMIT: usize = 500;

type BranchGraphFuture<'a, T> = Pin<Box<dyn Future<Output = crate::errors::Result<T>> + Send + 'a>>;
type BranchMarkerFuture<'a> = Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>;
type BranchSourceFuture<'a> =
    Pin<Box<dyn Future<Output = Option<crate::branch_meta::BranchGraphSourceV1>> + Send + 'a>>;
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

    fn published_source(&self) -> BranchSourceFuture<'_>;
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
struct BranchGraphSymbol {
    node_digest: ManifestDigest,
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
                let node_digest = canonical_sha256(&("tracedecay.branch-graph-node.v1", &node))
                    .map_err(|error| crate::errors::TraceDecayError::Database {
                        operation: "digest branch graph node".to_owned(),
                        message: error.to_string(),
                    })?;
                symbols.push(BranchGraphSymbol::new(node, node_digest));
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

    fn published_source(&self) -> BranchSourceFuture<'_> {
        Box::pin(async move {
            let encoded = GraphRuntimePort::db(self.graph.as_ref())
                .get_metadata(crate::tracedecay::BRANCH_QUERY_GRAPH_SOURCE_KEY)
                .await
                .ok()
                .flatten()?;
            serde_json::from_str(&encoded).ok()
        })
    }
}

impl BranchGraphSymbol {
    fn new(node: Node, node_digest: ManifestDigest) -> Self {
        Self {
            node_digest,
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
    authorization: BranchAuthorizationEpochV1,
    worktree_root: PathBuf,
    symbols: Vec<BranchGraphSymbol>,
    graph: Arc<dyn BranchGraphReadPort>,
}

enum BranchResolutionOutcome {
    Resolved(ResolvedBranchSnapshot),
    Denied,
    Stale(BranchQueryStaleReasonV1),
    Unavailable(BranchQueryUnavailableReasonV1),
}

#[derive(Clone, Copy)]
enum BranchRevalidationOutcome {
    Current,
    Denied,
    Stale(BranchQueryStaleReasonV1),
    Unavailable(BranchQueryUnavailableReasonV1),
}

enum BranchGenerationOutcome {
    Current(BranchGraphGenerationV1),
    Drift,
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
    fn authorization_epoch(
        access: &ProjectSourceAccessSnapshot,
    ) -> Result<BranchAuthorizationEpochV1, BranchQueryUnavailableReasonV1> {
        let access_digest = canonical_sha256(&(
            BRANCH_AUTHORIZATION_EPOCH_DOMAIN_V1,
            &access.scope,
            &access.requester,
            &access.binding,
            &access.configuration_revision,
            &access.configuration_digest,
            &access.configuration_provenance_digest,
            &access.effective_capabilities,
        ))
        .map_err(|_| BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable)?;
        Ok(BranchAuthorizationEpochV1 {
            configuration_revision: access.configuration_revision.clone(),
            configuration_digest: access.configuration_digest.clone(),
            configuration_provenance_digest: access.configuration_provenance_digest.clone(),
            access_digest,
        })
    }

    fn authorization_epoch_is_current(
        frozen: &BranchAuthorizationEpochV1,
        access: &ProjectSourceAccessSnapshot,
    ) -> Result<bool, BranchQueryUnavailableReasonV1> {
        Ok(Self::authorization_epoch(access)? == *frozen)
    }

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

    fn branch_scope(
        &self,
        branch: &str,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
    ) -> Result<ResolvedScope, BranchQueryUnavailableReasonV1> {
        let reference = CanonicalGitRefNameV1::new(format!("refs/heads/{branch}"))
            .and_then(|reference| RefId::new(reference.as_str().to_owned()))
            .map_err(|_| BranchQueryUnavailableReasonV1::BranchUnavailable)?;
        ResolvedScope::new(
            self.project_scope.project_id.clone(),
            repository_id,
            worktree_id,
            Some(reference),
        )
        .map_err(|_| BranchQueryUnavailableReasonV1::BranchUnavailable)
    }

    async fn authorize(
        &self,
        scope: &ResolvedScope,
        worktree_root: &Path,
        capability: &'static str,
    ) -> Result<ProjectSourceAccessSnapshot, BranchQueryUnavailableReasonV1> {
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
            worktree_root,
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
        if access.effective_capabilities.contains(&capability)
            && observed_at < access.grant_expires_at
        {
            Ok(access)
        } else {
            Err(BranchQueryUnavailableReasonV1::ProjectUnavailable)
        }
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
        worktree_root: &Path,
        branch: &str,
        registered_scope: &GraphScopeRecord,
    ) -> Result<Arc<TraceDecay>, BranchQueryUnavailableReasonV1> {
        let graph = TraceDecay::open_branch_with_registered_configuration(
            worktree_root,
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

    fn graph_source(
        &self,
        branch: &str,
        _registered_scope: &GraphScopeRecord,
    ) -> Result<(crate::branch_meta::BranchGraphSourceV1, PathBuf), BranchQueryUnavailableReasonV1>
    {
        let meta =
            crate::branch_meta::load_branch_meta(&self.active_graph.store_layout().data_root)
                .ok_or(BranchQueryUnavailableReasonV1::GenerationUnavailable)?;
        let entry = meta
            .branches
            .get(branch)
            .ok_or(BranchQueryUnavailableReasonV1::BranchUnavailable)?;
        let source = entry
            .graph_source
            .clone()
            .ok_or(BranchQueryUnavailableReasonV1::GenerationUnavailable)?;
        let worktree_root = PathBuf::from(&source.worktree_root)
            .canonicalize()
            .map_err(|_| BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable)?;
        let expected_repository =
            crate::daemon::code_index_scheduler::identity::repository_id_for(&worktree_root)
                .map_err(|_| BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable)?;
        let expected_worktree =
            crate::daemon::code_index_scheduler::identity::worktree_id_for(&worktree_root)
                .map_err(|_| BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable)?;
        GitOidV1::new(source.source_oid.clone())
            .map_err(|_| BranchQueryUnavailableReasonV1::GenerationUnavailable)?;
        if source.project_id != self.project_scope.project_id.as_str()
            || source.repository_id != expected_repository.as_str()
            || source.worktree_id != expected_worktree.as_str()
            || source.reference != format!("refs/heads/{branch}")
            || crate::worktree::git_common_dir(&worktree_root)
                != crate::worktree::git_common_dir(self.active_graph.project_root())
        {
            return Err(BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable);
        }
        Ok((source, worktree_root))
    }

    fn live_ref_oid(&self, reference: &str) -> Result<GitOidV1, BranchQueryUnavailableReasonV1> {
        let repo = gix::open(self.active_graph.project_root())
            .map_err(|_| BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable)?;
        let mut reference = repo
            .find_reference(reference)
            .map_err(|_| BranchQueryUnavailableReasonV1::BranchUnavailable)?;
        let oid = reference
            .peel_to_id_in_place()
            .map_err(|_| BranchQueryUnavailableReasonV1::BranchUnavailable)?;
        let oid_hex = oid.to_hex().to_string();
        repo.find_object(oid.detach())
            .map_err(|_| BranchQueryUnavailableReasonV1::BranchUnavailable)?;
        GitOidV1::new(oid_hex).map_err(|_| BranchQueryUnavailableReasonV1::GenerationUnavailable)
    }

    fn content_digest(
        symbols: &[BranchGraphSymbol],
    ) -> Result<ManifestDigest, BranchQueryUnavailableReasonV1> {
        let mut symbols = symbols.to_vec();
        symbols.sort_by(|left, right| {
            left.node_digest
                .cmp(&right.node_digest)
                .then_with(|| left.file.cmp(&right.file))
                .then_with(|| left.qualified_name.cmp(&right.qualified_name))
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.signature.cmp(&right.signature))
        });
        canonical_sha256(&("tracedecay.branch-graph-content.v1", symbols))
            .map_err(|_| BranchQueryUnavailableReasonV1::GenerationUnavailable)
    }

    fn generation(
        scope: &GraphScopeRecord,
        source_oid: GitOidV1,
        content_digest: ManifestDigest,
    ) -> Result<BranchGraphGenerationV1, BranchQueryUnavailableReasonV1> {
        if scope.graph_scope_id.is_empty() || scope.graph_scope_id.chars().any(char::is_control) {
            return Err(BranchQueryUnavailableReasonV1::GenerationUnavailable);
        }
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
            &source_oid,
            &content_digest,
        ))
        .map_err(|_| BranchQueryUnavailableReasonV1::GenerationUnavailable)?;
        Ok(BranchGraphGenerationV1 {
            graph_scope_id: scope.graph_scope_id.clone(),
            source_oid,
            content_digest,
            recorded_sync_at,
            generation_digest,
        })
    }

    async fn current_generation(
        graph: &dyn BranchGraphReadPort,
        scope: &GraphScopeRecord,
        source: &crate::branch_meta::BranchGraphSourceV1,
        symbols: &[BranchGraphSymbol],
    ) -> BranchGenerationOutcome {
        match graph.published_source().await {
            Some(published) if published == *source => {}
            Some(_) => return BranchGenerationOutcome::Drift,
            None => {
                return BranchGenerationOutcome::Unavailable(
                    BranchQueryUnavailableReasonV1::GenerationUnavailable,
                );
            }
        }
        let Some(graph_source_oid) = graph
            .source_commit()
            .await
            .filter(|commit| !commit.is_empty())
        else {
            return BranchGenerationOutcome::Unavailable(
                BranchQueryUnavailableReasonV1::GenerationUnavailable,
            );
        };
        let Ok(source_oid) = GitOidV1::new(source.source_oid.clone()) else {
            return BranchGenerationOutcome::Unavailable(
                BranchQueryUnavailableReasonV1::GenerationUnavailable,
            );
        };
        if graph_source_oid != source_oid.as_str() {
            return BranchGenerationOutcome::Drift;
        }
        let content_digest = match Self::content_digest(symbols) {
            Ok(digest) => digest,
            Err(reason) => return BranchGenerationOutcome::Unavailable(reason),
        };
        match Self::generation(scope, source_oid, content_digest) {
            Ok(generation) => BranchGenerationOutcome::Current(generation),
            Err(reason) => BranchGenerationOutcome::Unavailable(reason),
        }
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
            let registered_scope = match Self::exact_graph_scope(&context, branch) {
                Ok(scope) => scope,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            let (source, worktree_root) = match self.graph_source(branch, &registered_scope) {
                Ok(source) => source,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            let repository_id = match RepositoryId::new(source.repository_id.clone()) {
                Ok(id) => id,
                Err(_) => {
                    return BranchResolutionOutcome::Unavailable(
                        BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable,
                    );
                }
            };
            let worktree_id = match WorktreeId::new(source.worktree_id.clone()) {
                Ok(id) => id,
                Err(_) => {
                    return BranchResolutionOutcome::Unavailable(
                        BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable,
                    );
                }
            };
            let scope = match self.branch_scope(branch, repository_id, worktree_id) {
                Ok(scope) => scope,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            let authorization = match self.authorize(&scope, &worktree_root, capability).await {
                Ok(access) => access,
                Err(BranchQueryUnavailableReasonV1::ProjectUnavailable) => {
                    return BranchResolutionOutcome::Denied;
                }
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            let live_oid = match self.live_ref_oid(&source.reference) {
                Ok(oid) => oid,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            if live_oid.as_str() != source.source_oid {
                return BranchResolutionOutcome::Stale(BranchQueryStaleReasonV1::ReferenceMoved);
            }
            let graph = match self
                .open_graph(&worktree_root, branch, &registered_scope)
                .await
            {
                Ok(graph) => graph,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            let graph: Arc<dyn BranchGraphReadPort> = Arc::new(TraceDecayBranchGraph { graph });
            let symbols = match graph.all_nodes().await {
                Ok(symbols) => symbols,
                Err(_) => {
                    return BranchResolutionOutcome::Unavailable(
                        BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable,
                    );
                }
            };
            let generation = match Self::current_generation(
                graph.as_ref(),
                &registered_scope,
                &source,
                &symbols,
            )
            .await
            {
                BranchGenerationOutcome::Current(generation) => generation,
                BranchGenerationOutcome::Drift => {
                    return BranchResolutionOutcome::Stale(
                        BranchQueryStaleReasonV1::GraphGenerationChanged,
                    );
                }
                BranchGenerationOutcome::Unavailable(reason) => {
                    return BranchResolutionOutcome::Unavailable(reason);
                }
            };
            let Some(reference) = scope.reference.clone() else {
                return BranchResolutionOutcome::Unavailable(
                    BranchQueryUnavailableReasonV1::BranchUnavailable,
                );
            };
            let authorization_epoch = match Self::authorization_epoch(&authorization) {
                Ok(epoch) => epoch,
                Err(reason) => return BranchResolutionOutcome::Unavailable(reason),
            };
            BranchResolutionOutcome::Resolved(ResolvedBranchSnapshot {
                identity: BranchSnapshotIdentityV1 {
                    project_id: scope.project_id,
                    repository_id: scope.repository_id,
                    worktree_id: scope.worktree_id,
                    reference,
                    scope_digest: scope.scope_digest,
                    authorization: authorization_epoch.clone(),
                    generation,
                },
                registered_scope,
                authorization: authorization_epoch,
                worktree_root,
                symbols,
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
            let repository_id = snapshot.identity.repository_id.clone();
            let worktree_id = snapshot.identity.worktree_id.clone();
            let scope = match self.branch_scope(branch, repository_id, worktree_id) {
                Ok(scope) => scope,
                Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
            };
            let authorization = match self
                .authorize(&scope, &snapshot.worktree_root, capability)
                .await
            {
                Ok(access) => access,
                Err(BranchQueryUnavailableReasonV1::ProjectUnavailable) => {
                    return BranchRevalidationOutcome::Denied;
                }
                Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
            };
            let authorization_is_current =
                match Self::authorization_epoch_is_current(&snapshot.authorization, &authorization)
                {
                    Ok(current) => current,
                    Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
                };
            if !authorization_is_current {
                return BranchRevalidationOutcome::Stale(
                    BranchQueryStaleReasonV1::AuthorizationEpochChanged,
                );
            }
            let registered_scope = match Self::exact_graph_scope(&context, branch) {
                Ok(scope) => scope,
                Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
            };
            let (source, worktree_root) = match self.graph_source(branch, &registered_scope) {
                Ok(source) => source,
                Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
            };
            if worktree_root != snapshot.worktree_root {
                return BranchRevalidationOutcome::Stale(
                    BranchQueryStaleReasonV1::GraphGenerationChanged,
                );
            }
            let live_oid = match self.live_ref_oid(&source.reference) {
                Ok(oid) => oid,
                Err(reason) => return BranchRevalidationOutcome::Unavailable(reason),
            };
            if live_oid != snapshot.identity.generation.source_oid {
                return BranchRevalidationOutcome::Stale(BranchQueryStaleReasonV1::ReferenceMoved);
            }
            let symbols = match snapshot.graph.all_nodes().await {
                Ok(symbols) => symbols,
                Err(_) => {
                    return BranchRevalidationOutcome::Unavailable(
                        BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable,
                    );
                }
            };
            let generation = match Self::current_generation(
                snapshot.graph.as_ref(),
                &registered_scope,
                &source,
                &symbols,
            )
            .await
            {
                BranchGenerationOutcome::Current(generation) => generation,
                BranchGenerationOutcome::Drift => {
                    return BranchRevalidationOutcome::Stale(
                        BranchQueryStaleReasonV1::GraphGenerationChanged,
                    );
                }
                BranchGenerationOutcome::Unavailable(reason) => {
                    return BranchRevalidationOutcome::Unavailable(reason);
                }
            };
            if registered_scope != snapshot.registered_scope
                || scope.scope_digest != snapshot.identity.scope_digest
                || generation != snapshot.identity.generation
            {
                return BranchRevalidationOutcome::Stale(
                    BranchQueryStaleReasonV1::GraphGenerationChanged,
                );
            }
            BranchRevalidationOutcome::Current
        })
    }
}

pub(super) async fn daemon_branch_query_port(
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
    let cursor_keys = active_graph
        .configuration_runtime()
        .registered_database()
        .load_session_cursor_key_provider_result()
        .await
        .map_err(|error| crate::errors::TraceDecayError::Config {
            message: format!("branch query cursor authority is unavailable: {error}"),
        })?;
    Ok(Arc::new(DaemonBranchQueryExecutor {
        resolver: Arc::new(RegisteredBranchSnapshotResolver {
            active_graph,
            registry,
            project_scope,
            route_registered,
        }),
        cursor_key: cursor_keys.active_key_ref().clone(),
        cursor_authenticator: Arc::new(cursor_keys),
    }))
}

struct DaemonBranchQueryExecutor {
    resolver: Arc<dyn BranchSnapshotResolver>,
    cursor_key: SignedCursorKeyRefV1,
    cursor_authenticator: Arc<dyn SessionCursorAuthenticator>,
}

#[path = "branch_query/execution.rs"]
mod execution;

#[cfg(test)]
#[path = "branch_query_tests.rs"]
mod tests;
