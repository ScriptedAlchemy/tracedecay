use std::collections::{HashMap, HashSet};

use crate::context::ContextBuilder;
use crate::errors::Result;
use crate::graph::{GraphQueryManager, GraphTraverser};
use crate::tracedecay::TraceDecay;
use crate::types::*;

impl TraceDecay {
    /// Returns aggregate statistics about the code graph.
    pub async fn get_stats(&self) -> Result<GraphStats> {
        self.db.get_stats().await
    }

    /// Retrieves a single node by its unique ID.
    pub async fn get_node(&self, id: &str) -> Result<Option<Node>> {
        self.db.get_node_by_id(id).await
    }

    /// Returns all nodes that transitively call the given node, up to `max_depth`.
    pub async fn get_callers(&self, node_id: &str, max_depth: usize) -> Result<Vec<(Node, Edge)>> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_callers(node_id, max_depth).await
    }

    /// Returns all nodes that the given node transitively calls, up to `max_depth`.
    pub async fn get_callees(&self, node_id: &str, max_depth: usize) -> Result<Vec<(Node, Edge)>> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_callees(node_id, max_depth).await
    }

    /// Computes the impact radius: all nodes that directly or indirectly
    /// depend on the given node, up to `max_depth`.
    pub async fn get_impact_radius(&self, node_id: &str, max_depth: usize) -> Result<Subgraph> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_impact_radius(node_id, max_depth).await
    }

    /// Same as `get_impact_radius` but multi-source: takes many seed node
    /// IDs and walks the union of their impact radii with a single shared
    /// `visited` set, so each downstream node is traversed at most once.
    pub async fn get_impact_radius_multi(
        &self,
        seed_ids: &[String],
        max_depth: usize,
    ) -> Result<Vec<Node>> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_impact_radius_multi(seed_ids, max_depth).await
    }

    /// Finds the shortest directed call chain from `from_id` to `to_id`,
    /// following only outgoing `Calls` edges. Returns `None` if no chain
    /// exists within `max_depth` hops.
    pub async fn get_call_chain(
        &self,
        from_id: &str,
        to_id: &str,
        max_depth: usize,
    ) -> Result<Option<crate::graph::traversal::GraphPath>> {
        let traverser = GraphTraverser::new(&self.db);
        traverser
            .find_path_directed(from_id, to_id, &[crate::types::EdgeKind::Calls], max_depth)
            .await
    }

    /// Builds a bidirectional call graph around a node.
    pub async fn get_call_graph(&self, node_id: &str, depth: usize) -> Result<Subgraph> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_call_graph(node_id, depth).await
    }

    /// Finds potentially dead code (nodes with no incoming edges).
    ///
    /// When `include_public` is `false` (the default), `pub` items are
    /// excluded — they may be referenced by code outside the indexed
    /// scope. Pass `true` to also surface pub items with zero indexed
    /// callers (useful for workspace-internal audits).
    pub async fn find_dead_code(
        &self,
        kinds: &[NodeKind],
        include_public: bool,
    ) -> Result<Vec<Node>> {
        let qm = GraphQueryManager::new(&self.db);
        qm.find_dead_code(kinds, include_public, None).await
    }

    /// Returns a bounded dead-code page for interactive tools.
    pub async fn find_dead_code_bounded(
        &self,
        kinds: &[NodeKind],
        include_public: bool,
        limit: usize,
    ) -> Result<Vec<Node>> {
        let qm = GraphQueryManager::new(&self.db);
        qm.find_dead_code(kinds, include_public, Some(limit)).await
    }

    /// Returns all nodes for a given file, ordered by start line.
    pub async fn get_nodes_by_file(&self, file_path: &str) -> Result<Vec<Node>> {
        self.db.get_nodes_by_file(file_path).await
    }

    /// Returns every node in the database.
    pub async fn get_all_nodes(&self) -> Result<Vec<Node>> {
        self.db.get_all_nodes().await
    }

    /// Returns the distinct file paths holding at least one node of `kind`, in
    /// path order after `after_path`, so a whole-repository walk over one kind
    /// can page instead of loading the graph.
    pub async fn file_paths_with_nodes_of_kind(
        &self,
        kind: NodeKind,
        after_path: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>> {
        self.db
            .file_paths_with_nodes_of_kind(kind, after_path, limit)
            .await
    }

    /// Returns incoming edges to a target node.
    pub async fn get_incoming_edges(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.db.get_incoming_edges(node_id, &[]).await
    }

    /// Returns the subset of `candidate_ids` that have a `#[test]` annotation.
    pub async fn get_test_annotated_node_ids(
        &self,
        candidate_ids: &[String],
    ) -> Result<HashSet<String>> {
        self.db.get_test_annotated_node_ids(candidate_ids).await
    }

    /// Returns all file paths containing at least one `#[test]`-annotated function.
    pub async fn get_files_with_test_annotations(&self) -> Result<HashSet<String>> {
        self.db.get_files_with_test_annotations().await
    }

    /// Returns all node IDs marked with `/// skip-test-coverage`.
    pub async fn get_skip_test_coverage_node_ids(&self) -> Result<HashSet<String>> {
        self.db.get_skip_test_coverage_node_ids().await
    }

    /// Returns incoming edges for many target nodes in one round-trip.
    /// Empty `kinds` matches every edge kind.
    pub async fn get_incoming_edges_bulk(
        &self,
        target_ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        self.db.get_incoming_edges_bulk(target_ids, kinds).await
    }

    /// Returns outgoing edges for many source nodes in one round-trip.
    /// Empty `kinds` matches every edge kind.
    pub async fn get_outgoing_edges_bulk(
        &self,
        source_ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        self.db.get_outgoing_edges_bulk(source_ids, kinds).await
    }

    /// Returns all nodes whose `qualified_name` matches `qname`.
    /// Cross-run lookup independent of the content-hash node IDs.
    pub async fn get_nodes_by_qualified_name(&self, qname: &str) -> Result<Vec<Node>> {
        self.db.get_nodes_by_qualified_name(qname).await
    }

    /// Exact bare-name lookup using `idx_nodes_name`. No relevance scoring,
    /// no fuzzy matching — for that, use [`search`](Self::search).
    pub async fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        self.db.get_nodes_by_name(name).await
    }

    /// Returns outgoing edges from a source node.
    pub async fn get_outgoing_edges(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.db.get_outgoing_edges(node_id, &[]).await
    }

    /// Returns every edge in the database.
    pub async fn get_all_edges(&self) -> Result<Vec<Edge>> {
        self.db.get_all_edges().await
    }

    /// Returns nodes ranked by edge count for a given edge kind and direction,
    /// optionally filtered by node kind.
    pub async fn get_ranked_nodes_by_edge_kind(
        &self,
        edge_kind: &EdgeKind,
        node_kind: Option<&NodeKind>,
        incoming: bool,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64)>> {
        self.db
            .get_ranked_nodes_by_edge_kind(edge_kind, node_kind, incoming, path_prefix, limit)
            .await
    }

    /// Returns nodes ranked by total incoming and outgoing connectivity.
    pub async fn get_hotspot_nodes(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64, u64)>> {
        self.db.get_hotspot_nodes(path_prefix, limit).await
    }

    /// Returns nodes ranked by line span, optionally filtered by node kind and path.
    pub async fn get_largest_nodes(
        &self,
        node_kind: Option<&NodeKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u32)>> {
        self.db
            .get_largest_nodes(node_kind, path_prefix, limit)
            .await
    }

    /// Returns files ranked by coupling (fan-in or fan-out).
    pub async fn get_file_coupling(
        &self,
        fan_in: bool,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, u64)>> {
        self.db.get_file_coupling(fan_in, path_prefix, limit).await
    }

    /// Returns classes/interfaces ranked by inheritance depth via extends chains.
    pub async fn get_inheritance_depth(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64)>> {
        self.db.get_inheritance_depth(path_prefix, limit).await
    }

    /// Returns node kind distribution for the highest-node-count files,
    /// optionally filtered by path prefix.
    pub async fn get_node_distribution(
        &self,
        path_prefix: Option<&str>,
        file_limit: u32,
    ) -> Result<Vec<(String, String, u64)>> {
        self.db.get_node_distribution(path_prefix, file_limit).await
    }

    /// Returns whole-scope node counts per kind.
    pub async fn get_node_kind_totals(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<(String, u64)>> {
        self.db.get_node_kind_totals(path_prefix).await
    }

    /// Returns the number of files in the distribution scope.
    pub async fn count_distribution_files(&self, path_prefix: Option<&str>) -> Result<u64> {
        self.db.count_distribution_files(path_prefix).await
    }

    /// Returns calls edges as (`source_id`, `target_id`) pairs for cycle detection.
    pub async fn get_call_edges(&self, path_prefix: Option<&str>) -> Result<Vec<(String, String)>> {
        self.db.get_call_edges(path_prefix).await
    }

    /// Returns calls edges as (`source_id`, `target_id`, `line`) tuples.
    pub async fn get_call_edges_with_lines(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<(String, String, Option<u32>)>> {
        self.db.get_call_edges_with_lines(path_prefix).await
    }

    /// Returns functions/methods ranked by composite complexity score.
    pub async fn get_complexity_ranked(
        &self,
        node_kind: Option<&NodeKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u32, u64, u64, u64)>> {
        self.db
            .get_complexity_ranked(node_kind, path_prefix, limit)
            .await
    }

    /// Returns public symbols missing docstrings.
    pub async fn get_undocumented_public_symbols(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Node>> {
        self.db
            .get_undocumented_public_symbols(path_prefix, limit)
            .await
    }

    /// Returns classes ranked by member count (methods + fields).
    pub async fn get_god_classes(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64, u64, u64)>> {
        self.db.get_god_classes(path_prefix, limit).await
    }

    /// Detects circular dependencies at the file level.
    pub async fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>> {
        let qm = GraphQueryManager::new(&self.db);
        qm.find_circular_dependencies().await
    }

    /// Builds an AI-ready context for a given task description.
    pub async fn build_context(
        &self,
        task: &str,
        options: &BuildContextOptions,
    ) -> Result<TaskContext> {
        let builder = ContextBuilder::new(&self.db, &self.project_root);
        builder.build_context(task, options).await
    }

    /// Returns all indexed file records.
    pub async fn get_all_files(&self) -> Result<Vec<FileRecord>> {
        self.db.get_all_files().await
    }

    /// Returns all indexed logical file paths without full record metadata.
    pub async fn get_all_file_paths(&self) -> Result<Vec<String>> {
        self.db.get_all_file_paths().await
    }

    /// Returns file paths that depend on the given file.
    pub async fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>> {
        let qm = GraphQueryManager::new(&self.db);
        qm.get_file_dependents(file_path).await
    }

    /// Returns a map of file path to approximate token count (size / 4).
    ///
    /// Delegates to the DB keyset-paged reader so callers never force an
    /// unbounded full-`FileRecord` materialization on startup or refresh.
    pub async fn get_file_token_map(&self) -> Result<HashMap<String, u64>> {
        self.db.get_file_token_map().await
    }

    /// Returns all nodes under a directory prefix filtered by kinds.
    pub async fn get_nodes_by_dir(&self, dir: &str, kinds: &[NodeKind]) -> Result<Vec<Node>> {
        self.db.get_nodes_by_dir(dir, kinds).await
    }

    /// Returns edges where both source and target are in the given node ID set.
    pub async fn get_internal_edges(&self, node_ids: &[String]) -> Result<Vec<Edge>> {
        self.db.get_internal_edges(node_ids).await
    }
}
