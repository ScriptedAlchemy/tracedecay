//! Daemon-side implementation of the dashboard code-graph read port.
//!
//! [`DashboardGraphReadAdapter`] is the only implementation of
//! `tracedecay_application::DashboardGraphReadPortV1`: HTTP adapters receive
//! complete bounded read models and never a database path, connection, or
//! graph store handle. The adapter serves topology rows from the canonical
//! code-graph reads of the retained project graph, and binds every read to a
//! verified generation published through the daemon's mounted Grafeo
//! publication/snapshot authority (`ProjectGraphRuntimePortV1`) — the same
//! publish-on-read projection pattern the Work and Git-evidence topologies
//! use. The published projection is a content-addressed watermark over the
//! served topology, so identical topology re-verifies the same generation and
//! any topology change publishes (and reports) a new one.

mod queries;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::Value;
use tracedecay_application::{
    DashboardGraphEdgeV1, DashboardGraphKindCountV1, DashboardGraphLanguageCountV1,
    DashboardGraphLargestFileV1, DashboardGraphNeighborsV1, DashboardGraphNodeV1,
    DashboardGraphOverviewV1, DashboardGraphPathV1, DashboardGraphReadErrorV1,
    DashboardGraphReadFutureV1, DashboardGraphReadOperationV1, DashboardGraphReadPayloadV1,
    DashboardGraphReadPortV1, DashboardGraphReadRequestV1, DashboardGraphReadV1,
    DashboardGraphSearchV1, DashboardGraphSpanV1, DashboardGraphSubgraphV1, DashboardGraphTotalsV1,
    ResolvedScope, VerifiedDashboardGraphGenerationV1,
};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSemanticEdgeV1, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::code_intelligence::{FileRecord, GraphStats};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RelationEdgeKindV1, SymbolOccurrenceId,
    canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphGenerationId,
    GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, GraphProperty, GraphPropertyName, GraphWatermark, SourceGeneration,
};

use crate::global_db::{ProjectGraphRuntimePortV1, RegisteredGlobalDb};
use crate::tracedecay::TraceDecay;

/// Safety cap on the BFS visited set for path reads.
const PATH_VISITED_CAP: usize = 20_000;

/// Cap on the cached top-degree pool: the default subgraph's candidate pool
/// is at most `node_limit * 2 = 500`, and the overview needs the top 12.
const DEGREE_POOL_CAP: i64 = 500;

/// Cap on edges fetched among the default-mode candidate pool before the
/// per-response edge cap is applied.
const DEFAULT_POOL_EDGE_CAP: i64 = 4_000;

/// Shared journey namespace on the daemon's project graph registry.
const DASHBOARD_GRAPH_NAMESPACE: &str = "project";
const DASHBOARD_GRAPH_PROJECTION: &str = "code-dashboard";
const DASHBOARD_GRAPH_PROJECTOR_REVISION_V1: &str = "code-dashboard-projector.v1";
const PROJECTION_RECORD_PROPERTY: &str = "projection-record";
const GENERATION_DIGEST_DOMAIN: &str = "tracedecay.dashboard-code-graph-generation.v1";

/// Content watermark of the topology actually served by this read.
///
/// Node edits, edge churn (ids are AUTOINCREMENT and never reused), and file
/// inventory changes each move at least one component, so the published
/// generation identity follows the served topology.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TopologyWatermark {
    nodes: i64,
    edges: i64,
    files: i64,
    max_edge_id: i64,
    last_node_update: i64,
}

impl TopologyWatermark {
    fn canonical_text(&self) -> String {
        format!(
            "nodes:{};edges:{};files:{};max-edge:{};last-node-update:{}",
            self.nodes, self.edges, self.files, self.max_edge_id, self.last_node_update
        )
    }
}

/// Cached whole-graph degree aggregation feeding the overview's
/// `top_connected` and the default subgraph's candidate pool, rebuilt when
/// the edge fingerprint moves.
struct DegreeSummary {
    fingerprint: (i64, i64),
    /// Top [`DEGREE_POOL_CAP`] `(node_id, degree)` rows, ordered by degree
    /// descending then qualified name ascending (zero-degree nodes included).
    pool: Vec<(String, i64)>,
    /// Overview `top_connected` rows (top 12 by degree, hydrated node rows).
    top_connected: Vec<DashboardGraphNodeV1>,
}

/// Daemon-owned dashboard graph read authority over one retained project
/// graph and its registered project-sessions Grafeo mount.
pub struct DashboardGraphReadAdapter {
    graph_database: Arc<crate::db::Database>,
    graph_runtime: Arc<dyn ProjectGraphRuntimePortV1>,
    scope: ResolvedScope,
    profile_id: tracedecay_domain::UserProfileId,
    degree_cache: tokio::sync::Mutex<Option<Arc<DegreeSummary>>>,
    /// Root the daemon's scheduler registry keys the retained interactive
    /// code graph store by.
    project_root: std::path::PathBuf,
    /// Daemon-owned per-request resolver of the retained interactive code
    /// graph store. `None` (direct servers, fixtures without an activation)
    /// is the typed unavailable interactive graph: adjacency reads answer
    /// their unavailable envelope instead of falling back to relational rows.
    interactive_graph: Option<crate::mcp::server::DashboardGraphInteractiveResolver>,
}

impl DashboardGraphReadAdapter {
    /// Composes the read authority for one retained project graph.
    ///
    /// `None` is the typed absent state: an unregistered project identity, an
    /// unresolvable exact scope, or a project-sessions authority without its
    /// bound project graph runtime cannot serve verified dashboard graph
    /// reads, and the composition keeps `graph_read_authority` empty so every
    /// route answers its typed unavailable envelope.
    pub fn for_project(
        cg: &TraceDecay,
        project_database: &RegisteredGlobalDb,
        interactive_graph: Option<crate::mcp::server::DashboardGraphInteractiveResolver>,
    ) -> Option<Self> {
        let project_id = ProjectId::new(cg.store_layout().identity.project_id.clone()?).ok()?;
        let scope = crate::application::context::RegisteredScopeResolver::resolve(
            cg.project_root(),
            cg.project_root(),
            &project_id,
        )
        .ok()?;
        let graph_runtime = project_database.project_graph_runtime()?.clone();
        let profile_id = project_database.binding().shard_id.profile_id.clone();
        Some(Self {
            graph_database: cg.dashboard_database_guard(),
            graph_runtime,
            scope,
            profile_id,
            degree_cache: tokio::sync::Mutex::new(None),
            project_root: cg.project_root().to_path_buf(),
            interactive_graph,
        })
    }

    /// Opens a generation-pinned interactive reader over the retained code
    /// graph projection store. Every absence is the typed unavailable
    /// envelope: an unmounted resolver, an incomplete activation, and a
    /// stale or mismatched generation each refuse rather than falling back
    /// to relational adjacency.
    async fn interactive_reader(
        &self,
    ) -> Result<CodeGraphInteractiveReader, DashboardGraphReadErrorV1> {
        let resolver = self.interactive_graph.as_ref().ok_or_else(|| {
            unavailable("interactive code graph reads require the daemon-owned scheduler bridge")
        })?;
        let store = resolver(self.project_root.clone())
            .await
            .ok_or_else(|| unavailable("code graph projection has not completed activation"))?;
        let generation = store.generation().clone();
        store
            .interactive_reader_with_cancellation(&generation, Arc::new(UnsignalledRead))
            .map_err(|error| unavailable(error.to_string()))
    }

    async fn read_inner(
        &self,
        request: DashboardGraphReadRequestV1,
    ) -> Result<DashboardGraphReadV1, DashboardGraphReadErrorV1> {
        request
            .scope
            .validate()
            .map_err(|error| DashboardGraphReadErrorV1::InvalidRequest {
                detail: error.to_string(),
            })?;
        verify_scope(&self.scope, &request.scope)?;
        let payload = match request.operation {
            DashboardGraphReadOperationV1::Overview => {
                DashboardGraphReadPayloadV1::Overview(self.overview().await?)
            }
            DashboardGraphReadOperationV1::Search {
                query,
                limit,
                offset,
            } => DashboardGraphReadPayloadV1::Search(self.search(&query, limit, offset).await?),
            DashboardGraphReadOperationV1::Node { node_id } => {
                DashboardGraphReadPayloadV1::Node(self.node(&node_id).await?)
            }
            DashboardGraphReadOperationV1::Neighbors { node_id, limit } => {
                DashboardGraphReadPayloadV1::Neighbors(self.neighbors(&node_id, limit).await?)
            }
            DashboardGraphReadOperationV1::Subgraph {
                node_id,
                query,
                node_limit,
                edge_limit,
            } => DashboardGraphReadPayloadV1::Subgraph(
                self.subgraph(node_id, &query, node_limit, edge_limit)
                    .await?,
            ),
            DashboardGraphReadOperationV1::Path {
                from,
                to,
                max_depth,
            } => DashboardGraphReadPayloadV1::Path(self.path(&from, &to, max_depth).await?),
        };
        let generation = self.published_generation().await?;
        Ok(DashboardGraphReadV1 {
            generation,
            payload,
        })
    }

    /// Publishes the served topology's watermark projection through the
    /// mounted Grafeo authority and returns its verified generation identity.
    async fn published_generation(
        &self,
    ) -> Result<VerifiedDashboardGraphGenerationV1, DashboardGraphReadErrorV1> {
        let watermark = self.watermark().await?;
        let watermark_text = watermark.canonical_text();
        let digest = canonical_sha256(&(
            GENERATION_DIGEST_DOMAIN,
            &watermark_text,
            DASHBOARD_GRAPH_PROJECTOR_REVISION_V1,
        ))
        .map_err(|error| DashboardGraphReadErrorV1::Corrupt {
            detail: format!("dashboard graph generation digest failed: {error}"),
        })?;
        let generation = GraphGenerationId::new(format!("code-dashboard:{}", digest.as_str()))
            .map_err(map_graph_error)?;
        let identity = GraphProjectionIdentity::new(
            GraphNamespace::new(DASHBOARD_GRAPH_NAMESPACE).map_err(map_graph_error)?,
            GraphProjectionId::new(DASHBOARD_GRAPH_PROJECTION).map_err(map_graph_error)?,
        );
        let entity = GraphEntity::new(
            GraphEntityId::new("projection:code-dashboard").map_err(map_graph_error)?,
            BTreeSet::new(),
            BTreeMap::from([(
                GraphPropertyName::new(PROJECTION_RECORD_PROPERTY).map_err(map_graph_error)?,
                GraphProperty::String(watermark_text.clone()),
            )]),
        )
        .map_err(map_graph_error)?;
        let manifest = GraphGenerationManifest::new(
            identity,
            generation.clone(),
            SourceGeneration::new(&watermark_text).map_err(map_graph_error)?,
            GraphWatermark::new(&watermark_text).map_err(map_graph_error)?,
            Vec::new(),
            vec![entity],
            Vec::new(),
        )
        .map_err(map_graph_error)?;
        let idempotency_key = GraphIdempotencyKey::new(format!("publish:{}", generation.as_str()))
            .map_err(map_graph_error)?;
        let snapshot = self
            .graph_runtime
            .publish_verified_manifest(&manifest, idempotency_key, Arc::new(AtomicBool::new(false)))
            .map_err(map_graph_error)?;
        let recovered_state_digest =
            ManifestDigest::new(snapshot.verified_head().recovered_digest.as_str().to_owned())
                .map_err(|error| DashboardGraphReadErrorV1::Corrupt {
                    detail: format!(
                        "verified dashboard graph generation carries a non-canonical recovered digest: {error}"
                    ),
                })?;
        let code_generation_id = CodeGenerationId::new(format!(
            "generation.code-dashboard.{}",
            digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(|error| DashboardGraphReadErrorV1::Corrupt {
            detail: format!("dashboard graph generation identity is invalid: {error}"),
        })?;
        Ok(VerifiedDashboardGraphGenerationV1 {
            project_id: self.scope.project_id.clone(),
            profile_id: self.profile_id.clone(),
            repository_id: self.scope.repository_id.clone(),
            worktree_id: self.scope.worktree_id.clone(),
            code_generation_id,
            graph_generation_id: snapshot.generation().as_str().to_owned(),
            projection_id: snapshot.projection().to_string(),
            recovered_state_digest,
        })
    }

    async fn watermark(&self) -> Result<TopologyWatermark, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        Ok(TopologyWatermark {
            nodes: queries::total_nodes(&conn).await.map_err(unavailable)?,
            edges: queries::total_edges(&conn).await.map_err(unavailable)?,
            files: queries::total_files(&conn).await.map_err(unavailable)?,
            max_edge_id: queries::max_edge_id(&conn).await.map_err(unavailable)?,
            last_node_update: queries::last_node_update(&conn)
                .await
                .map_err(unavailable)?,
        })
    }

    async fn degree_summary(&self) -> Result<Arc<DegreeSummary>, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        let fingerprint = (
            queries::total_edges(&conn).await.map_err(unavailable)?,
            queries::max_edge_id(&conn).await.map_err(unavailable)?,
        );
        // Held across the rebuild so concurrent requests share one aggregation.
        let mut guard = self.degree_cache.lock().await;
        if let Some(existing) = guard.as_ref()
            && existing.fingerprint == fingerprint
        {
            return Ok(Arc::clone(existing));
        }
        let pool = queries::degree_pool_rows(&conn, DEGREE_POOL_CAP)
            .await
            .map_err(unavailable)?
            .iter()
            .filter_map(|row| {
                row.get("id")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_owned(), i64_field(row, "degree")))
            })
            .collect();
        let top_connected = queries::top_connected_rows(&conn)
            .await
            .map_err(unavailable)?
            .into_iter()
            .map(decode_node)
            .collect::<Result<Vec<_>, _>>()?;
        let summary = Arc::new(DegreeSummary {
            fingerprint,
            pool,
            top_connected,
        });
        *guard = Some(Arc::clone(&summary));
        Ok(summary)
    }

    async fn overview(&self) -> Result<DashboardGraphOverviewV1, DashboardGraphReadErrorV1> {
        let (stats, files, summary) = tokio::join!(
            self.graph_database.get_stats(),
            self.graph_database.get_all_files(),
            self.degree_summary(),
        );
        let stats = stats.map_err(|error| unavailable(error.to_string()))?;
        let files = files.map_err(|error| unavailable(error.to_string()))?;
        let summary = summary?;
        Ok(overview_read_model(
            &stats,
            &files,
            summary.top_connected.clone(),
        ))
    }

    async fn search(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> Result<DashboardGraphSearchV1, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        let total = queries::search_total(&conn, query)
            .await
            .map_err(unavailable)?;
        let rows = queries::search_rows(&conn, query, limit, offset)
            .await
            .map_err(unavailable)?;
        let results = self.hydrate_nodes(rows).await?;
        Ok(DashboardGraphSearchV1 {
            query: query.to_owned(),
            limit,
            offset,
            total,
            results,
        })
    }

    async fn node(
        &self,
        node_id: &str,
    ) -> Result<Option<DashboardGraphNodeV1>, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        let Some(row) = queries::node_row(&conn, node_id)
            .await
            .map_err(unavailable)?
        else {
            return Ok(None);
        };
        Ok(self.hydrate_nodes(vec![row]).await?.into_iter().next())
    }

    /// Serves the neighborhood of one node from the verified code graph
    /// projection. Node identity (the focus row and neighbor hydration) stays
    /// on the relational node index until the cutover's identity value swap;
    /// adjacency — callers, callees, incident edges, per-kind counts, and
    /// neighbor degrees — is read exclusively from the generation-pinned
    /// interactive reader.
    async fn neighbors(
        &self,
        node_id: &str,
        limit: i64,
    ) -> Result<Option<DashboardGraphNeighborsV1>, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        let Some(focus_row) = queries::node_row(&conn, node_id)
            .await
            .map_err(unavailable)?
        else {
            return Ok(None);
        };
        let qualified_name = match str_field(&focus_row, "qualified_name") {
            "" => str_field(&focus_row, "name"),
            name => name,
        }
        .to_owned();
        if qualified_name.is_empty() {
            return Err(unavailable(
                "dashboard graph node row carries no qualified name to resolve \
                 against the published code graph generation",
            ));
        }
        let focus_kind = str_field(&focus_row, "kind").to_owned();
        let max_relations = usize::try_from(limit)
            .ok()
            .filter(|limit| *limit > 0)
            .ok_or_else(|| DashboardGraphReadErrorV1::InvalidRequest {
                detail: format!("neighbor limit must be positive, got {limit}"),
            })?;
        let reader = self.interactive_reader().await?;
        let neighborhood = tokio::task::spawn_blocking(move || {
            interactive_neighborhood(&reader, &qualified_name, &focus_kind, max_relations)
        })
        .await
        .map_err(|error| {
            unavailable(format!(
                "interactive adjacency read did not complete: {error}"
            ))
        })??;

        // Neighbor hydration: map projection occurrences back onto relational
        // node rows by qualified name so the served id-space stays coherent
        // with the not-yet-cut Search/Node operations. A neighbor the node
        // index does not know is still served, keyed by its occurrence — the
        // projection is the adjacency authority, not the relational rows.
        let mut names: BTreeSet<String> = BTreeSet::new();
        for edge in neighborhood
            .callers
            .iter()
            .chain(neighborhood.callees.iter())
        {
            if let Some(metadata) = edge.neighbor.metadata.as_ref() {
                names.insert(metadata.qualified_name.clone());
            }
        }
        let name_list: Vec<String> = names.into_iter().collect();
        // Hydration is keyed on the (qualified name, kind) pair, never on the
        // qualified name alone: a qualified name repeats BY CONSTRUCTION in
        // this projection, which is exactly why the same-name occurrence index
        // exists. Every row that shares a key is retained so an ambiguous key
        // can be refused; picking one would silently serve a different
        // symbol's metadata and wire id under the requested name.
        let mut rows_by_identity: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
        for row in queries::node_rows_by_qualified_names(&conn, &name_list)
            .await
            .map_err(unavailable)?
        {
            let key = (
                str_field(&row, "qualified_name").to_owned(),
                str_field(&row, "kind").to_owned(),
            );
            rows_by_identity.entry(key).or_default().push(row);
        }
        let mut nodes_by_occurrence: BTreeMap<SymbolOccurrenceId, DashboardGraphNodeV1> =
            BTreeMap::new();
        for edge in neighborhood
            .callers
            .iter()
            .chain(neighborhood.callees.iter())
        {
            if nodes_by_occurrence.contains_key(&edge.neighbor.occurrence) {
                continue;
            }
            let mut node = neighbor_node(&edge.neighbor, &rows_by_identity)?;
            node.degree = Some(
                neighborhood
                    .degrees
                    .get(&edge.neighbor.occurrence)
                    .copied()
                    .unwrap_or(0),
            );
            nodes_by_occurrence.insert(edge.neighbor.occurrence.clone(), node);
        }
        // The caller/callee node lists keep their pre-cutover semantics:
        // call edges only. The incident-edge list spans every semantic kind.
        let hydrate = |edges: &[CodeGraphSemanticEdgeV1]| {
            edges
                .iter()
                .filter(|edge| edge.edge.kind == RelationEdgeKindV1::Calls)
                .map(|edge| {
                    let mut node = nodes_by_occurrence
                        .get(&edge.neighbor.occurrence)
                        .cloned()
                        .ok_or_else(|| DashboardGraphReadErrorV1::Corrupt {
                            detail: "hydrated neighborhood lost a neighbor node".to_owned(),
                        })?;
                    node.edge_kind = Some(relation_kind_str(edge.edge.kind).to_owned());
                    node.edge_line = None;
                    Ok(node)
                })
                .collect::<Result<Vec<_>, DashboardGraphReadErrorV1>>()
        };
        let callers = hydrate(&neighborhood.callers)?;
        let callees = hydrate(&neighborhood.callees)?;
        let mut edges = Vec::with_capacity(neighborhood.callers.len() + neighborhood.callees.len());
        for edge in &neighborhood.callers {
            let node = nodes_by_occurrence
                .get(&edge.neighbor.occurrence)
                .ok_or_else(|| DashboardGraphReadErrorV1::Corrupt {
                    detail: "hydrated neighborhood lost a caller node".to_owned(),
                })?;
            edges.push(DashboardGraphEdgeV1 {
                source: node.id.clone(),
                target: node_id.to_owned(),
                kind: relation_kind_str(edge.edge.kind).to_owned(),
                line: None,
                source_name: node.name.clone(),
                target_name: None,
            });
        }
        for edge in &neighborhood.callees {
            let node = nodes_by_occurrence
                .get(&edge.neighbor.occurrence)
                .ok_or_else(|| DashboardGraphReadErrorV1::Corrupt {
                    detail: "hydrated neighborhood lost a callee node".to_owned(),
                })?;
            edges.push(DashboardGraphEdgeV1 {
                source: node_id.to_owned(),
                target: node.id.clone(),
                kind: relation_kind_str(edge.edge.kind).to_owned(),
                line: None,
                source_name: None,
                target_name: node.name.clone(),
            });
        }
        let edges_by_kind = neighborhood
            .edges_by_kind
            .iter()
            .map(|(kind, count)| DashboardGraphKindCountV1 {
                kind: relation_kind_str(*kind).to_owned(),
                count: i64::try_from(*count).unwrap_or(i64::MAX),
            })
            .collect();
        Ok(Some(DashboardGraphNeighborsV1 {
            node_id: node_id.to_owned(),
            callers,
            callees,
            edges,
            edges_by_kind,
        }))
    }

    async fn subgraph(
        &self,
        node_id: Option<String>,
        query: &str,
        node_limit: i64,
        edge_limit: i64,
    ) -> Result<DashboardGraphSubgraphV1, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        let seed_id = match node_id.filter(|id| !id.trim().is_empty()) {
            Some(id) => Some(id),
            None if !query.is_empty() => {
                let Some(id) = queries::first_node_for_query(&conn, query)
                    .await
                    .map_err(unavailable)?
                else {
                    // Explicit query with no hit: an empty payload, not the
                    // default slice, so a failed search reads as "no match".
                    return Ok(DashboardGraphSubgraphV1 {
                        seed_id: None,
                        mode: "seeded".to_owned(),
                        nodes: Vec::new(),
                        edges: Vec::new(),
                        nodes_capped: false,
                        edges_capped: false,
                    });
                };
                Some(id)
            }
            None => None,
        };
        let Some(seed_id) = seed_id else {
            return self.default_subgraph(node_limit, edge_limit).await;
        };

        let candidate_rows = queries::subgraph_candidate_rows(&conn, &seed_id)
            .await
            .map_err(unavailable)?;
        let mut all_ids = Vec::new();
        let mut seen = BTreeSet::new();
        for row in candidate_rows {
            if let Some(id) = row.get("id").and_then(Value::as_str)
                && seen.insert(id.to_owned())
            {
                all_ids.push(id.to_owned());
            }
        }
        let selected_ids: Vec<String> = all_ids
            .iter()
            .take(usize::try_from(node_limit).unwrap_or(0))
            .cloned()
            .collect();
        let nodes = self.nodes_by_ids(&selected_ids).await?;
        let edge_rows = queries::edge_rows_for_ids(&conn, &selected_ids, edge_limit + 1)
            .await
            .map_err(unavailable)?;
        let edges_capped = edge_rows.len() > usize::try_from(edge_limit).unwrap_or(0);
        let edges = edge_rows
            .into_iter()
            .take(usize::try_from(edge_limit).unwrap_or(0))
            .map(decode_edge)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DashboardGraphSubgraphV1 {
            seed_id: Some(seed_id),
            mode: "seeded".to_owned(),
            nodes_capped: all_ids.len() > usize::try_from(node_limit).unwrap_or(0),
            edges_capped,
            nodes,
            edges,
        })
    }

    /// Seedless "project overview" slice: the most-connected symbols plus the
    /// edges among them. Selection grows greedily by adjacency over a
    /// top-degree candidate pool; isolated nodes fill leftover capacity.
    async fn default_subgraph(
        &self,
        node_limit: i64,
        edge_limit: i64,
    ) -> Result<DashboardGraphSubgraphV1, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        let pool_limit = usize::try_from((node_limit * 2).min(DEGREE_POOL_CAP)).unwrap_or(0);
        let summary = self.degree_summary().await?;

        let mut pool_ids = Vec::new();
        let mut degrees: BTreeMap<String, i64> = BTreeMap::new();
        for (id, degree) in summary.pool.iter().take(pool_limit) {
            pool_ids.push(id.clone());
            degrees.insert(id.clone(), *degree);
        }

        let pool_edge_rows = queries::edge_rows_for_ids(&conn, &pool_ids, DEFAULT_POOL_EDGE_CAP)
            .await
            .map_err(unavailable)?;
        let pool_edges = pool_edge_rows
            .into_iter()
            .map(decode_edge)
            .collect::<Result<Vec<_>, _>>()?;

        // Adjacency over the pool: node id -> indices of touching edges
        // (self-loops don't make a node "connected" for selection purposes).
        let mut adjacency: HashMap<&str, Vec<usize>> = HashMap::new();
        for (idx, edge) in pool_edges.iter().enumerate() {
            if edge.source == edge.target {
                continue;
            }
            adjacency.entry(edge.source.as_str()).or_default().push(idx);
            adjacency.entry(edge.target.as_str()).or_default().push(idx);
        }

        let budget = usize::try_from(node_limit).unwrap_or(0);
        let mut selected: Vec<String> = Vec::new();
        let mut selected_set: HashSet<&str> = HashSet::new();
        // Edges recorded while growing the selection; emitted first so the
        // edge cap cannot leave a selected node without any visible edge.
        let mut connecting_edges: Vec<usize> = Vec::new();
        while selected.len() < budget {
            let mut adjacent_pick: Option<(&str, usize)> = None;
            let mut seed_pick: Option<&str> = None;
            for id in pool_ids.iter().map(String::as_str) {
                if selected_set.contains(id) {
                    continue;
                }
                let Some(edge_idxs) = adjacency.get(id) else {
                    continue;
                };
                let touching = edge_idxs.iter().copied().find(|&idx| {
                    let edge = &pool_edges[idx];
                    let other = if edge.source == id {
                        edge.target.as_str()
                    } else {
                        edge.source.as_str()
                    };
                    selected_set.contains(other)
                });
                if let Some(idx) = touching {
                    adjacent_pick = Some((id, idx));
                    break;
                }
                if seed_pick.is_none() {
                    seed_pick = Some(id);
                }
            }
            let Some(id) = adjacent_pick.map(|(id, _)| id).or(seed_pick) else {
                break;
            };
            if let Some((_, edge_idx)) = adjacent_pick {
                connecting_edges.push(edge_idx);
            }
            selected.push(id.to_owned());
            selected_set.insert(id);
        }
        if selected.len() < budget {
            for id in &pool_ids {
                if selected.len() >= budget {
                    break;
                }
                if !selected_set.contains(id.as_str()) {
                    selected.push(id.clone());
                    selected_set.insert(id);
                }
            }
        }

        let mut edge_order = connecting_edges;
        let used: HashSet<usize> = edge_order.iter().copied().collect();
        for (idx, edge) in pool_edges.iter().enumerate() {
            if used.contains(&idx) {
                continue;
            }
            if selected_set.contains(edge.source.as_str())
                && selected_set.contains(edge.target.as_str())
            {
                edge_order.push(idx);
            }
        }
        let edges_capped = edge_order.len() > usize::try_from(edge_limit).unwrap_or(0);
        let edges: Vec<DashboardGraphEdgeV1> = edge_order
            .into_iter()
            .take(usize::try_from(edge_limit).unwrap_or(0))
            .map(|idx| pool_edges[idx].clone())
            .collect();

        let nodes = self.nodes_by_ids(&selected).await?;
        let total_nodes = queries::total_nodes(&conn).await.map_err(unavailable)?;
        Ok(DashboardGraphSubgraphV1 {
            seed_id: None,
            mode: "default".to_owned(),
            nodes_capped: total_nodes > i64::try_from(selected.len()).unwrap_or(i64::MAX),
            edges_capped,
            nodes,
            edges,
        })
    }

    /// Undirected shortest path between two nodes via breadth-first search
    /// over the edges table, with a capped visited set.
    async fn path(
        &self,
        from: &str,
        to: &str,
        max_depth: i64,
    ) -> Result<DashboardGraphPathV1, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        let empty = DashboardGraphPathV1 {
            from: from.to_owned(),
            to: to.to_owned(),
            found: false,
            path: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        };
        if from.is_empty() || to.is_empty() {
            return Ok(empty);
        }

        // child -> (parent, edge row) back-pointers for path reconstruction.
        let mut parents: HashMap<String, (String, DashboardGraphEdgeV1)> = HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(from.to_owned());
        let mut frontier = vec![from.to_owned()];
        let mut found = from == to;

        'search: for _ in 0..max_depth {
            if found || frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for chunk in frontier.chunks(400) {
                let rows = queries::frontier_edge_rows(&conn, chunk)
                    .await
                    .map_err(unavailable)?;
                for row in rows {
                    let edge = decode_edge(row)?;
                    let (known, discovered) = if visited.contains(&edge.source)
                        && !visited.contains(&edge.target)
                    {
                        (edge.source.clone(), edge.target.clone())
                    } else if visited.contains(&edge.target) && !visited.contains(&edge.source) {
                        (edge.target.clone(), edge.source.clone())
                    } else {
                        continue;
                    };
                    visited.insert(discovered.clone());
                    parents.insert(discovered.clone(), (known, edge));
                    if discovered == to {
                        found = true;
                        break 'search;
                    }
                    next.push(discovered);
                    if visited.len() > PATH_VISITED_CAP {
                        break 'search;
                    }
                }
            }
            frontier = next;
        }

        if !found {
            return Ok(empty);
        }

        let mut path_ids = vec![to.to_owned()];
        let mut path_edges = Vec::new();
        let mut cursor = to.to_owned();
        while cursor != from {
            let Some((parent, edge)) = parents.get(&cursor) else {
                break;
            };
            path_edges.push(edge.clone());
            cursor = parent.clone();
            path_ids.push(cursor.clone());
        }
        path_ids.reverse();
        path_edges.reverse();

        let nodes = self.nodes_by_ids(&path_ids).await?;
        Ok(DashboardGraphPathV1 {
            from: from.to_owned(),
            to: to.to_owned(),
            found: true,
            path: path_ids,
            nodes,
            edges: path_edges,
        })
    }

    async fn nodes_by_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<DashboardGraphNodeV1>, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        let rows = queries::node_rows_by_ids(&conn, ids)
            .await
            .map_err(unavailable)?;
        self.hydrate_nodes(rows).await
    }

    /// Decodes node rows and attaches total degrees plus the span object —
    /// the same hydration every pre-cutover dashboard graph read performed.
    async fn hydrate_nodes(
        &self,
        rows: Vec<Value>,
    ) -> Result<Vec<DashboardGraphNodeV1>, DashboardGraphReadErrorV1> {
        let conn = self.graph_database.engine_conn();
        let mut nodes = rows
            .into_iter()
            .map(decode_node)
            .collect::<Result<Vec<_>, _>>()?;
        let ids: Vec<String> = nodes.iter().map(|node| node.id.clone()).collect();
        let mut degrees: BTreeMap<String, i64> = BTreeMap::new();
        for row in queries::degree_rows_for_ids(&conn, &ids)
            .await
            .map_err(unavailable)?
        {
            if let (Some(id), Some(degree)) = (
                row.get("node_id").and_then(Value::as_str),
                row.get("degree").and_then(Value::as_i64),
            ) {
                degrees.insert(id.to_owned(), degree);
            }
        }
        for node in &mut nodes {
            node.degree = Some(degrees.get(&node.id).copied().unwrap_or(0));
        }
        Ok(nodes)
    }
}

impl DashboardGraphReadPortV1 for DashboardGraphReadAdapter {
    fn read(&self, request: DashboardGraphReadRequestV1) -> DashboardGraphReadFutureV1<'_> {
        Box::pin(self.read_inner(request))
    }
}

/// A read for a foreign exact scope is concealed behind the typed denial —
/// the adapter serves exactly one registered project/repository/worktree.
fn verify_scope(
    own: &ResolvedScope,
    requested: &ResolvedScope,
) -> Result<(), DashboardGraphReadErrorV1> {
    if own.project_id == requested.project_id
        && own.repository_id == requested.repository_id
        && own.worktree_id == requested.worktree_id
    {
        Ok(())
    } else {
        Err(DashboardGraphReadErrorV1::Denied)
    }
}

/// Candidate cap when resolving a focus row's qualified name against the
/// projection catalog; more same-name overloads than this means the name is
/// not a usable interactive key.
const NEIGHBOR_RESOLVE_CANDIDATES: usize = 16;

/// The HTTP read path carries no per-request cancellation signal; the store
/// lifecycle cancellation the reader was assembled with still applies.
struct UnsignalledRead;

impl GraphCancellation for UnsignalledRead {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Adjacency bundle for one focus symbol, read entirely from the verified
/// code graph projection.
struct InteractiveNeighborhoodV1 {
    callers: Vec<CodeGraphSemanticEdgeV1>,
    callees: Vec<CodeGraphSemanticEdgeV1>,
    edges_by_kind: Vec<(RelationEdgeKindV1, u64)>,
    degrees: BTreeMap<SymbolOccurrenceId, i64>,
}

fn interactive_neighborhood(
    reader: &CodeGraphInteractiveReader,
    qualified_name: &str,
    kind: &str,
    max_relations: usize,
) -> Result<InteractiveNeighborhoodV1, DashboardGraphReadErrorV1> {
    let cancellation: Arc<dyn GraphCancellation> = Arc::new(UnsignalledRead);
    let candidates = reader
        .resolve_qualified_name(
            qualified_name,
            None,
            NEIGHBOR_RESOLVE_CANDIDATES,
            Arc::clone(&cancellation),
        )
        .map_err(|error| unavailable(error.to_string()))?;
    let focus = candidates
        .iter()
        .find(|candidate| {
            candidate
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.kind == kind)
        })
        .or_else(|| candidates.first())
        .map(|candidate| candidate.occurrence.clone())
        .ok_or_else(|| {
            unavailable(format!(
                "symbol {qualified_name:?} is not present in the published code graph generation"
            ))
        })?;
    let seeds = [focus.clone()];
    let callers = single_seed_batch(
        reader
            .callers(&seeds, &[], max_relations, Arc::clone(&cancellation))
            .map_err(|error| unavailable(error.to_string()))?,
    )?;
    let callees = single_seed_batch(
        reader
            .callees(&seeds, &[], max_relations, Arc::clone(&cancellation))
            .map_err(|error| unavailable(error.to_string()))?,
    )?;
    let counts = reader
        .edge_kind_counts(&focus, Arc::clone(&cancellation))
        .map_err(|error| unavailable(error.to_string()))?;
    let mut merged: BTreeMap<RelationEdgeKindV1, u64> = counts.outgoing;
    for (kind, count) in counts.incoming {
        *merged.entry(kind).or_default() += count;
    }
    let mut occurrences: BTreeSet<SymbolOccurrenceId> = BTreeSet::new();
    for edge in callers.iter().chain(callees.iter()) {
        occurrences.insert(edge.neighbor.occurrence.clone());
    }
    let occurrence_list: Vec<SymbolOccurrenceId> = occurrences.into_iter().collect();
    let mut degrees: BTreeMap<SymbolOccurrenceId, i64> = BTreeMap::new();
    if !occurrence_list.is_empty() {
        for entry in reader
            .degrees(&occurrence_list, cancellation)
            .map_err(|error| unavailable(error.to_string()))?
        {
            let total = entry.outgoing.saturating_add(entry.incoming);
            degrees.insert(entry.occurrence, i64::try_from(total).unwrap_or(i64::MAX));
        }
    }
    Ok(InteractiveNeighborhoodV1 {
        callers,
        callees,
        edges_by_kind: merged.into_iter().collect(),
        degrees,
    })
}

/// One-seed adjacency batches must come back with exactly one batch.
fn single_seed_batch(
    mut batches: Vec<Vec<CodeGraphSemanticEdgeV1>>,
) -> Result<Vec<CodeGraphSemanticEdgeV1>, DashboardGraphReadErrorV1> {
    if batches.len() != 1 {
        return Err(DashboardGraphReadErrorV1::Corrupt {
            detail: format!(
                "interactive adjacency returned {} batches for one seed",
                batches.len()
            ),
        });
    }
    Ok(batches.remove(0))
}

/// Hydrates one projection neighbor. Prefers the relational node row (same
/// id-space as the not-yet-cut Search/Node operations); a symbol the node
/// index does not know is served as projection truth keyed by its
/// occurrence, never dropped.
fn neighbor_node(
    summary: &CodeGraphSymbolSummaryV1,
    rows_by_identity: &BTreeMap<(String, String), Vec<Value>>,
) -> Result<DashboardGraphNodeV1, DashboardGraphReadErrorV1> {
    if let Some(metadata) = summary.metadata.as_ref() {
        let key = (metadata.qualified_name.clone(), metadata.kind.clone());
        match rows_by_identity.get(&key).map(Vec::as_slice) {
            Some([row]) => return decode_node(row.clone()),
            Some(rows) if rows.len() > 1 => {
                // More than one relational row answers this projected symbol's
                // (qualified name, kind). The projection distinguishes them by
                // file identity, but the node table is keyed by path and the
                // two are not joinable, so no correct row can be chosen here.
                // Refuse rather than pick: a silently-picked row serves the
                // wrong symbol's wire id. Resolving this needs the pending
                // `nodes.symbol_occurrence_id` column, which supersedes the
                // qualified-name key with a direct occurrence join.
                return Err(DashboardGraphReadErrorV1::Corrupt {
                    detail: format!(
                        "the node index holds {} rows for qualified name {:?} of kind {:?}; \
                         the qualified-name key cannot identify one and the direct \
                         occurrence join is not yet available",
                        rows.len(),
                        metadata.qualified_name,
                        metadata.kind,
                    ),
                });
            }
            _ => {}
        }
    }
    let metadata = summary.metadata.as_ref();
    Ok(DashboardGraphNodeV1 {
        id: summary.occurrence.as_str().to_owned(),
        kind: metadata.map(|m| m.kind.clone()).unwrap_or_default(),
        name: metadata.map(|m| {
            m.qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(m.qualified_name.as_str())
                .to_owned()
        }),
        qualified_name: metadata.map(|m| m.qualified_name.clone()),
        file_path: None,
        start_line: None,
        end_line: None,
        start_column: None,
        end_column: None,
        attrs_start_line: None,
        doc: None,
        signature: None,
        visibility: None,
        is_async: None,
        branches: None,
        loops: None,
        returns: None,
        max_nesting: None,
        unsafe_blocks: None,
        unchecked_calls: None,
        assertions: None,
        updated_at: None,
        parent_id: None,
        degree: None,
        span: None,
        edge_kind: None,
        edge_line: None,
    })
}

/// Canonical relation kinds share the relational edge-kind vocabulary for
/// the kinds both sides define, so the served wire strings are stable across
/// the adjacency cutover.
fn relation_kind_str(kind: RelationEdgeKindV1) -> &'static str {
    match kind {
        RelationEdgeKindV1::Calls => "calls",
        RelationEdgeKindV1::Uses => "uses",
        RelationEdgeKindV1::TypeOf => "type_of",
        RelationEdgeKindV1::Contains => "contains",
        RelationEdgeKindV1::Implements => "implements",
        RelationEdgeKindV1::Extends => "extends",
        RelationEdgeKindV1::Annotates => "annotates",
    }
}

fn unavailable(detail: impl Into<String>) -> DashboardGraphReadErrorV1 {
    DashboardGraphReadErrorV1::Unavailable {
        detail: detail.into(),
    }
}

fn map_graph_error(error: GraphDbError) -> DashboardGraphReadErrorV1 {
    match error {
        GraphDbError::Cancelled => DashboardGraphReadErrorV1::Cancelled,
        GraphDbError::DeadlineExceeded => DashboardGraphReadErrorV1::TimedOut,
        GraphDbError::InvalidRequest { message } => {
            DashboardGraphReadErrorV1::InvalidRequest { detail: message }
        }
        corrupt @ (GraphDbError::Corrupt { .. }
        | GraphDbError::ProjectionMismatch { .. }
        | GraphDbError::GenerationMismatch { .. }
        | GraphDbError::ResetRequired { .. }) => DashboardGraphReadErrorV1::Corrupt {
            detail: corrupt.to_string(),
        },
        other => DashboardGraphReadErrorV1::Unavailable {
            detail: other.to_string(),
        },
    }
}

fn i64_field(row: &Value, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn str_field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

fn decode_node(row: Value) -> Result<DashboardGraphNodeV1, DashboardGraphReadErrorV1> {
    let mut node: DashboardGraphNodeV1 =
        serde_json::from_value(row).map_err(|error| DashboardGraphReadErrorV1::Corrupt {
            detail: format!("dashboard graph node row is invalid: {error}"),
        })?;
    node.span = Some(DashboardGraphSpanV1 {
        start_line: node.start_line.unwrap_or(0),
        end_line: node.end_line.unwrap_or(0),
        start_column: node.start_column.unwrap_or(0),
        end_column: node.end_column.unwrap_or(0),
        attrs_start_line: node.attrs_start_line.unwrap_or(0),
    });
    Ok(node)
}

fn decode_edge(row: Value) -> Result<DashboardGraphEdgeV1, DashboardGraphReadErrorV1> {
    serde_json::from_value(row).map_err(|error| DashboardGraphReadErrorV1::Corrupt {
        detail: format!("dashboard graph edge row is invalid: {error}"),
    })
}

fn language_for_path(path: &str) -> &'static str {
    let Some((_, ext)) = path.rsplit_once('.') else {
        return "unknown";
    };
    match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "scala" | "sc" => "scala",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "zig" => "zig",
        "sh" | "bash" | "zsh" => "shell",
        "md" | "mdx" => "markdown",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sql" => "sql",
        "html" | "css" => "web",
        _ => "other",
    }
}

fn saturating_count(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

fn kind_counts(counts: &HashMap<String, u64>) -> Vec<DashboardGraphKindCountV1> {
    let mut entries: Vec<_> = counts.iter().collect();
    entries.sort_by(|(left_label, left_count), (right_label, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_label.cmp(right_label))
    });
    entries
        .into_iter()
        .map(|(kind, count)| DashboardGraphKindCountV1 {
            kind: kind.clone(),
            count: saturating_count(*count),
        })
        .collect()
}

fn files_by_language(files: &[FileRecord]) -> Vec<DashboardGraphLanguageCountV1> {
    let mut counts: BTreeMap<&'static str, i64> = BTreeMap::new();
    for file in files {
        *counts.entry(language_for_path(&file.path)).or_insert(0) += 1;
    }
    let mut rows: Vec<DashboardGraphLanguageCountV1> = counts
        .into_iter()
        .map(|(language, count)| DashboardGraphLanguageCountV1 {
            language: language.to_owned(),
            count,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.language.cmp(&b.language))
    });
    rows
}

fn largest_files(files: &[FileRecord]) -> Vec<DashboardGraphLargestFileV1> {
    let mut files: Vec<_> = files.iter().collect();
    files.sort_by(|left, right| {
        right
            .node_count
            .cmp(&left.node_count)
            .then_with(|| left.path.cmp(&right.path))
    });
    files
        .into_iter()
        .take(12)
        .map(|file| DashboardGraphLargestFileV1 {
            path: file.path.clone(),
            node_count: i64::from(file.node_count),
            size: file.size,
        })
        .collect()
}

fn overview_read_model(
    stats: &GraphStats,
    files: &[FileRecord],
    top_connected: Vec<DashboardGraphNodeV1>,
) -> DashboardGraphOverviewV1 {
    DashboardGraphOverviewV1 {
        totals: DashboardGraphTotalsV1 {
            nodes: stats.node_count,
            edges: stats.edge_count,
            files: stats.file_count,
        },
        nodes_by_kind: kind_counts(&stats.nodes_by_kind),
        edges_by_kind: kind_counts(&stats.edges_by_kind),
        files_by_language: files_by_language(files),
        largest_files: largest_files(files),
        top_connected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{RepositoryId, WorktreeId};

    fn scope(project: &str, repository: &str, worktree: &str) -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new(project).expect("project id"),
            RepositoryId::new(repository).expect("repository id"),
            WorktreeId::new(worktree).expect("worktree id"),
            None,
        )
        .expect("resolved scope")
    }

    #[test]
    fn foreign_scope_reads_are_denied_not_aliased() {
        let own = scope(
            "project.dash-graph",
            "repository.dash-graph",
            "worktree.dash-graph",
        );

        let foreign_project = scope(
            "project.other",
            "repository.dash-graph",
            "worktree.dash-graph",
        );
        let foreign_repository = scope(
            "project.dash-graph",
            "repository.other",
            "worktree.dash-graph",
        );
        let foreign_worktree = scope(
            "project.dash-graph",
            "repository.dash-graph",
            "worktree.other",
        );

        assert!(verify_scope(&own, &own.clone()).is_ok());
        for foreign in [foreign_project, foreign_repository, foreign_worktree] {
            assert_eq!(
                verify_scope(&own, &foreign),
                Err(DashboardGraphReadErrorV1::Denied),
                "a foreign exact scope must be concealed behind the typed denial"
            );
        }
    }

    #[test]
    fn graph_store_failures_map_to_their_typed_read_states() {
        assert_eq!(
            map_graph_error(GraphDbError::Cancelled),
            DashboardGraphReadErrorV1::Cancelled
        );
        assert_eq!(
            map_graph_error(GraphDbError::DeadlineExceeded),
            DashboardGraphReadErrorV1::TimedOut
        );
        assert!(matches!(
            map_graph_error(GraphDbError::invalid("bad identifier")),
            DashboardGraphReadErrorV1::InvalidRequest { .. }
        ));
        assert!(matches!(
            map_graph_error(GraphDbError::Corrupt {
                message: "digest mismatch".to_owned()
            }),
            DashboardGraphReadErrorV1::Corrupt { .. }
        ));
        assert!(matches!(
            map_graph_error(GraphDbError::Closed),
            DashboardGraphReadErrorV1::Unavailable { .. }
        ));
    }

    #[test]
    fn topology_watermark_is_content_addressed_and_deterministic() {
        let watermark = TopologyWatermark {
            nodes: 4,
            edges: 3,
            files: 2,
            max_edge_id: 7,
            last_node_update: 1_700_000_000,
        };
        let same = watermark.clone();
        let moved = TopologyWatermark {
            max_edge_id: 8,
            ..watermark.clone()
        };

        assert_eq!(watermark.canonical_text(), same.canonical_text());
        assert_ne!(
            watermark.canonical_text(),
            moved.canonical_text(),
            "any topology movement must publish (and report) a new generation"
        );
    }
}
