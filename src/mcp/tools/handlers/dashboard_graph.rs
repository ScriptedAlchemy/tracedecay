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

mod interactive;
mod queries;
mod read_models;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::Value;
use tracedecay_application::{
    DashboardGraphEdgeV1, DashboardGraphKindCountV1, DashboardGraphNeighborsV1,
    DashboardGraphNodeV1, DashboardGraphOverviewV1, DashboardGraphPathV1,
    DashboardGraphReadErrorV1, DashboardGraphReadFutureV1, DashboardGraphReadOperationV1,
    DashboardGraphReadPayloadV1, DashboardGraphReadPortV1, DashboardGraphReadRequestV1,
    DashboardGraphReadV1, DashboardGraphSearchV1, DashboardGraphSubgraphV1, ResolvedScope,
    VerifiedDashboardGraphGenerationV1,
};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSemanticEdgeV1,
};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RelationEdgeKindV1, SymbolOccurrenceId,
    canonical_sha256,
};
use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
    GraphWatermark, SourceGeneration,
};

use crate::global_db::{ProjectGraphRuntimePortV1, RegisteredGlobalDb};
use crate::tracedecay::TraceDecay;
use interactive::{UnsignalledRead, interactive_neighborhood, neighbor_node, verify_scope};
use read_models::{
    decode_edge, decode_node, i64_field, map_graph_error, overview_read_model, relation_kind_str,
    str_field, unavailable,
};

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
