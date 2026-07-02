//! Read-side query surface: search ranking plus thin delegation to the
//! graph query/traversal layers.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::TraceDecayConfig;
use crate::context::ContextBuilder;
use crate::errors::Result;
use crate::graph::{GraphQueryManager, GraphTraverser};
use crate::types::*;

use super::TraceDecay;

impl TraceDecay {
    /// Searches for nodes matching the given query string.
    ///
    /// Over-fetches from the FTS layer and re-ranks results so that symbol
    /// definitions (functions, structs, traits, etc.) sort above mere
    /// references (`use`, `module`, annotation usages) that happen to share
    /// the same name. BM25 alone does not distinguish kinds, so a `use foo`
    /// statement could outrank the actual `pub fn foo()` definition.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let overfetch = limit.saturating_mul(3).max(30);
        let trimmed_query = query.trim();
        let mut raw = self.db.search_nodes(query, overfetch).await?;

        // FTS/BM25 can bury exact symbol definitions below many short import
        // rows. On Sonium, `LinearOperator` had dozens of `use ...LinearOperator`
        // rows in the top FTS window while the actual trait definition was
        // outside `overfetch`, so the kind tier below never saw it. Seed the
        // candidate set with exact `name = query` hits first, then dedup.
        if !trimmed_query.is_empty() {
            let mut exact_names = vec![trimmed_query.to_string()];
            if let Some(short) = trimmed_query.rsplit("::").next() {
                if short != trimmed_query && !short.is_empty() {
                    exact_names.push(short.to_string());
                }
            }
            let exact = self
                .db
                .search_nodes_by_exact_name(&exact_names, overfetch)
                .await?;
            raw.extend(
                exact
                    .into_iter()
                    .map(|node| SearchResult { node, score: 0.0 }),
            );
        }

        let mut seen = HashSet::new();
        let mut ranked: Vec<SearchResult> = raw
            .into_iter()
            .filter(|r| seen.insert(r.node.id.clone()))
            .map(|mut r| {
                r.score += kind_rank_bonus(&r.node.kind);
                // Exact-name match boost: when the node's `name` equals the
                // query verbatim, surface it ahead of partial / qualified-name
                // matches. Without this, searching for a trait like
                // `LinearOperator` could be outranked by a `Method` whose
                // qualified name happens to contain `LinearOperator` (e.g.
                // a method declared inside the trait body), or by a `Field`
                // that shares the same simple name.
                if !trimmed_query.is_empty() && r.node.name == trimmed_query {
                    r.score += 10.0;
                }
                r
            })
            .collect();
        // Sort by kind tier first (definitions > references), then score
        // descending. Tier-first avoids any chance that a `use` re-export
        // (kind tier = `Use`) outscores a real definition because BM25
        // happened to weight the short re-export row highly. Score is the
        // secondary key so within a tier we still respect BM25.
        ranked.sort_by(|a, b| {
            kind_tier(&a.node.kind)
                .cmp(&kind_tier(&b.node.kind))
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        ranked.truncate(limit);
        Ok(ranked)
    }

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
        qm.find_dead_code(kinds, include_public).await
    }

    /// Returns all nodes for a given file, ordered by start line.
    pub async fn get_nodes_by_file(&self, file_path: &str) -> Result<Vec<Node>> {
        self.db.get_nodes_by_file(file_path).await
    }

    /// Returns every node in the database.
    pub async fn get_all_nodes(&self) -> Result<Vec<Node>> {
        self.db.get_all_nodes().await
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

    /// Returns node kind distribution, optionally filtered by path prefix.
    pub async fn get_node_distribution(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<(String, String, u64)>> {
        self.db.get_node_distribution(path_prefix).await
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

    /// Returns the `#[derive(...)]` names attached to the given node.
    ///
    /// The graph's `DerivesMacro` edges are unreliable here: the resolver
    /// fuzzy-binds std-trait names like `Debug` to nonsense nodes (a `Debug`
    /// enum variant in an unrelated test fixture) and the resulting unique
    /// constraint on `(source, target, kind, line)` collapses multiple
    /// distinct derives on the same type onto a single edge — so a struct
    /// that derives `Debug, Clone, PartialEq, Eq, Hash` may surface only one
    /// of them. Instead we re-read the lines between `attrs_start_line` and
    /// `start_line` of the node, which the extractor already promises to
    /// cover the leading attribute block, and parse `#[derive(...)]`
    /// attributes directly. Bounded file I/O — one read per call.
    pub async fn get_derives_for_node(&self, node_id: &str) -> Result<Vec<String>> {
        let Some(node) = self.db.get_node_by_id(node_id).await? else {
            return Ok(Vec::new());
        };
        let file_path = self.project_root().join(&node.file_path);
        let Ok(content) = std::fs::read_to_string(&file_path) else {
            return Ok(Vec::new());
        };
        Ok(parse_derives_in_attr_block(
            &content,
            node.attrs_start_line,
            node.start_line,
        ))
    }

    /// Finds the most specific (smallest-span) node whose source range
    /// contains the given `(file, line)` location.
    ///
    /// Returns `None` when no indexed node covers the location — typically
    /// because the file isn't indexed, or the line is in a region the
    /// extractor didn't capture (e.g. inside a `use` block or top-of-file
    /// comment). Lines are 1-based to match `rustc` / `clippy` output;
    /// `Node.start_line` / `end_line` are 0-based internally so we subtract
    /// before comparing.
    ///
    /// Implementation loads every node in the file (cached at the index
    /// layer) and picks the smallest containing span. At the typical ~50
    /// nodes per file this is faster than a custom range-query and stays
    /// honest about overlap (impl blocks contain methods, etc.).
    pub async fn node_at_location(&self, file: &str, line_1based: u32) -> Result<Option<Node>> {
        if line_1based == 0 {
            return Ok(None);
        }
        let zero_based = line_1based - 1;
        let normalized = normalize_lookup_path(self.project_root(), file);
        let mut nodes = self.db.get_nodes_by_file(&normalized).await?;
        nodes.retain(|n| n.start_line <= zero_based && n.end_line >= zero_based);
        // Prefer the smallest containing span — that's the most specific
        // owner of the source location.
        nodes.sort_by_key(|n| (n.end_line - n.start_line, n.start_line));
        Ok(nodes.into_iter().next())
    }

    /// Returns the indexed size in bytes for a file path, or `0` if unknown.
    /// Used to estimate the token cost of expanding a file in responses.
    pub async fn get_file_size_bytes(&self, path: &str) -> u64 {
        match self.db.get_file(path).await {
            Ok(Some(rec)) => rec.size,
            _ => 0,
        }
    }

    /// Returns `impl` blocks matching the given trait and/or implementing type.
    ///
    /// Both filters are optional:
    /// - With only `trait_name`: every impl of that trait, regardless of the
    ///   implementing type.
    /// - With only `type_name`: every impl block for that type (trait impls
    ///   and inherent impls).
    /// - With both: the intersection.
    /// - With neither: every `impl` node in the graph (use sparingly).
    ///
    /// Each result carries the impl node plus, when available, the resolved
    /// trait node it implements. Matching uses substring containment on the
    /// trait/type names so callers can pass either short or qualified names.
    pub async fn get_impls(
        &self,
        trait_name: Option<&str>,
        type_name: Option<&str>,
    ) -> Result<Vec<(Node, Option<Node>)>> {
        use crate::types::EdgeKind;

        // Candidate impl blocks.
        let mut impls = self.db.get_nodes_by_kind(NodeKind::Impl).await?;

        // Filter by implementing type if requested. The impl node's `name`
        // field holds the type identifier (e.g. "MyType" for `impl Foo for MyType`).
        if let Some(type_q) = type_name {
            impls.retain(|n| node_name_matches(n, type_q));
        }

        // Gather Implements edges per impl, then batch-fetch every trait node
        // in one `get_nodes_by_ids` call to avoid an N+1 across impl blocks.
        let mut per_impl_trait_id: Vec<Option<String>> = Vec::with_capacity(impls.len());
        let mut trait_target_ids: Vec<String> = Vec::new();
        for impl_node in &impls {
            let edges = self
                .db
                .get_outgoing_edges(&impl_node.id, &[EdgeKind::Implements])
                .await
                .unwrap_or_default();
            let target = edges.into_iter().next().map(|e| e.target);
            if let Some(ref t) = target {
                trait_target_ids.push(t.clone());
            }
            per_impl_trait_id.push(target);
        }
        let trait_nodes = if trait_target_ids.is_empty() {
            Vec::new()
        } else {
            self.db.get_nodes_by_ids(&trait_target_ids).await?
        };
        let trait_map: std::collections::HashMap<String, Node> =
            trait_nodes.into_iter().map(|n| (n.id.clone(), n)).collect();

        let mut out: Vec<(Node, Option<Node>)> = Vec::with_capacity(impls.len());
        for (impl_node, trait_id) in impls.into_iter().zip(per_impl_trait_id) {
            let trait_node = trait_id.and_then(|id| trait_map.get(&id).cloned());

            // Trait filter: drop inherent impls when a trait was requested.
            if let Some(trait_q) = trait_name {
                let matched = trait_node
                    .as_ref()
                    .is_some_and(|t| node_name_matches(t, trait_q));
                if !matched {
                    continue;
                }
            }

            out.push((impl_node, trait_node));
        }
        Ok(out)
    }

    /// Resolves a trait method node to the concrete method nodes that satisfy
    /// it across every `impl` block of the enclosing trait.
    ///
    /// Returns an empty vec when the input is not a method whose parent (via
    /// `Contains`) is a trait. Used by `tracedecay_callees` to surface concrete
    /// dispatch targets in addition to the trait method itself.
    pub async fn get_trait_dispatch_targets(&self, method: &Node) -> Result<Vec<Node>> {
        use crate::types::EdgeKind;

        // Only method-kind nodes can be trait methods.
        if !matches!(method.kind, NodeKind::Method | NodeKind::Function) {
            return Ok(Vec::new());
        }

        // Find the trait that contains this method. parent_id points at
        // the enclosing scope after v9; verify it's actually a Trait.
        let Some(parent_id) = method.parent_id.as_deref() else {
            return Ok(Vec::new());
        };
        let Some(trait_node) = self.db.get_node_by_id(parent_id).await? else {
            return Ok(Vec::new());
        };
        if trait_node.kind != NodeKind::Trait {
            return Ok(Vec::new());
        }

        // Find every impl block of that trait.
        let impl_edges = self
            .db
            .get_incoming_edges(&trait_node.id, &[EdgeKind::Implements])
            .await?;
        let impl_ids: Vec<String> = impl_edges.into_iter().map(|e| e.source).collect();
        if impl_ids.is_empty() {
            return Ok(Vec::new());
        }

        // For each impl block, surface the method whose name matches the
        // trait method. Multiple impls may share names with unrelated nodes,
        // so we filter by both kind and name.
        let mut targets = Vec::new();
        for impl_id in impl_ids {
            let candidates = self.db.get_children_of(&impl_id).await?;
            for n in candidates {
                if matches!(n.kind, NodeKind::Method | NodeKind::Function) && n.name == method.name
                {
                    targets.push(n);
                }
            }
        }
        Ok(targets)
    }

    /// Returns file paths that depend on the given file.
    pub async fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>> {
        let qm = GraphQueryManager::new(&self.db);
        qm.get_file_dependents(file_path).await
    }

    /// Returns a map of file path to approximate token count (size / 4).
    pub async fn get_file_token_map(&self) -> Result<HashMap<String, u64>> {
        let files = self.db.get_all_files().await?;
        Ok(files.into_iter().map(|f| (f.path, f.size / 4)).collect())
    }

    /// Returns the persisted tokens-saved counter.
    pub async fn get_tokens_saved(&self) -> Result<u64> {
        match self.db.get_metadata("tokens_saved").await? {
            Some(v) => Ok(v.parse::<u64>().unwrap_or(0)),
            None => Ok(0),
        }
    }

    /// Persists the tokens-saved counter to the database.
    pub async fn set_tokens_saved(&self, value: u64) -> Result<()> {
        self.db
            .set_metadata("tokens_saved", &value.to_string())
            .await
    }

    /// Returns the resettable project-local token counter.
    ///
    /// This is separate from the main `tokens_saved` counter and can be
    /// independently reset via [`Self::reset_local_counter`].
    pub async fn get_local_counter(&self) -> Result<u64> {
        match self.db.get_metadata("local_counter").await? {
            Some(v) => Ok(v.parse::<u64>().unwrap_or(0)),
            None => Ok(0),
        }
    }

    /// Resets the project-local token counter to zero.
    pub async fn reset_local_counter(&self) -> Result<()> {
        self.db.set_metadata("local_counter", "0").await
    }

    /// Increments the project-local token counter by the given amount.
    pub async fn add_local_counter(&self, delta: u64) -> Result<()> {
        let current = self.get_local_counter().await?;
        self.db
            .set_metadata("local_counter", &(current + delta).to_string())
            .await
    }

    /// Returns all nodes under a directory prefix filtered by kinds.
    pub async fn get_nodes_by_dir(&self, dir: &str, kinds: &[NodeKind]) -> Result<Vec<Node>> {
        self.db.get_nodes_by_dir(dir, kinds).await
    }

    /// Returns edges where both source and target are in the given node ID set.
    pub async fn get_internal_edges(&self, node_ids: &[String]) -> Result<Vec<Edge>> {
        self.db.get_internal_edges(node_ids).await
    }

    /// Checkpoints the WAL and closes the database connection.
    pub async fn checkpoint(&self) -> Result<()> {
        self.db.checkpoint().await
    }

    /// Consumes the code graph and closes the database connection.
    pub fn close(self) {
        self.db.close();
    }

    /// Runs VACUUM and ANALYZE to reclaim disk space and update planner stats.
    pub async fn optimize(&self) -> Result<()> {
        self.db.optimize().await
    }

    /// Returns a reference to the current configuration.
    pub fn get_config(&self) -> &TraceDecayConfig {
        &self.config
    }

    /// Returns the project root path.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}

/// Search-result rank bonus applied per node kind, so symbol *definitions*
/// outrank mere *references* (use statements, annotation usages, modules)
/// that BM25 may otherwise score equally. Tuned so a definition with a
/// slightly worse BM25 score still surfaces above its imports.
///
/// Exhaustive match by design: when a new `NodeKind` variant is added the
/// compiler will force a re-tune here rather than silently defaulting it to
/// `0.0`, matching the project rule "crash hard if there is an unknown
/// value".
/// Coarse ranking tier used as the primary sort key in `search`. Lower
/// numbers sort first. The tiers separate "real definitions" (functions,
/// types, traits, …) from "references" (`use`, `module`, annotation usage)
/// so a re-export can never beat the thing it re-exports, no matter what
/// BM25 produces for the row.
fn kind_tier(kind: &NodeKind) -> u8 {
    match kind {
        // Tier 0: callable definitions and type definitions — the
        // "what is this?" answers a user usually wants when searching by
        // symbol name.
        NodeKind::Function
        | NodeKind::Method
        | NodeKind::StructMethod
        | NodeKind::Constructor
        | NodeKind::AbstractMethod
        | NodeKind::ArrowFunction
        | NodeKind::Procedure
        | NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Trait
        | NodeKind::Class
        | NodeKind::InnerClass
        | NodeKind::Interface
        | NodeKind::InterfaceType
        | NodeKind::Record
        | NodeKind::CaseClass
        | NodeKind::DataClass
        | NodeKind::SealedClass
        | NodeKind::TypeAlias
        | NodeKind::Union
        | NodeKind::Typedef
        | NodeKind::Mixin
        | NodeKind::Extension
        | NodeKind::Delegate
        | NodeKind::Template
        | NodeKind::PascalRecord
        | NodeKind::ScalaObject
        | NodeKind::KotlinObject
        | NodeKind::CompanionObject
        | NodeKind::Annotation
        | NodeKind::Event => 0,
        // Proto definitions (feature-gated)
        #[cfg(feature = "lang-protobuf")]
        NodeKind::ProtoMessage | NodeKind::ProtoService | NodeKind::ProtoRpc => 0,
        // Tier 1: impl blocks — between definitions and references.
        NodeKind::Impl => 1,
        // Tier 2: values, macros, members of types.
        NodeKind::Const
        | NodeKind::Static
        | NodeKind::Macro
        | NodeKind::PreprocessorDef
        | NodeKind::EnumVariant
        | NodeKind::Field
        | NodeKind::ValField
        | NodeKind::VarField
        | NodeKind::Property
        | NodeKind::CSharpProperty
        | NodeKind::StructTag
        | NodeKind::InitBlock
        | NodeKind::Export => 2,
        // Tier 3: containers (module, namespace, …) — usually not the
        // answer to "find symbol".
        NodeKind::Module
        | NodeKind::Package
        | NodeKind::Namespace
        | NodeKind::ScalaPackage
        | NodeKind::GoPackage
        | NodeKind::KotlinPackage
        | NodeKind::PascalUnit
        | NodeKind::Library
        | NodeKind::File
        | NodeKind::GenericParam
        | NodeKind::PascalProgram => 3,
        // Tier 4: pure references / annotations — always rank last.
        NodeKind::Use | NodeKind::Include | NodeKind::AnnotationUsage | NodeKind::Decorator => 4,
    }
}

fn kind_rank_bonus(kind: &NodeKind) -> f64 {
    match kind {
        // Callable definitions
        NodeKind::Function
        | NodeKind::Method
        | NodeKind::StructMethod
        | NodeKind::Constructor
        | NodeKind::AbstractMethod
        | NodeKind::ArrowFunction
        | NodeKind::Procedure => 3.0,
        // Type definitions
        NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Trait
        | NodeKind::Class
        | NodeKind::InnerClass
        | NodeKind::Interface
        | NodeKind::InterfaceType
        | NodeKind::Record
        | NodeKind::CaseClass
        | NodeKind::DataClass
        | NodeKind::SealedClass
        | NodeKind::TypeAlias
        | NodeKind::Union
        | NodeKind::Typedef
        | NodeKind::Mixin
        | NodeKind::Extension
        | NodeKind::Delegate
        | NodeKind::Template
        | NodeKind::PascalRecord
        | NodeKind::ScalaObject
        | NodeKind::KotlinObject
        | NodeKind::CompanionObject
        | NodeKind::Annotation
        | NodeKind::Event => 2.5,
        // Proto definitions
        #[cfg(feature = "lang-protobuf")]
        NodeKind::ProtoMessage | NodeKind::ProtoService | NodeKind::ProtoRpc => 2.5,
        // Impl blocks (between defs and refs)
        NodeKind::Impl => 2.0,
        // Values, macros, preprocessor defs
        NodeKind::Const
        | NodeKind::Static
        | NodeKind::Macro
        | NodeKind::PreprocessorDef
        | NodeKind::EnumVariant => 1.0,
        // Members of types
        NodeKind::Field
        | NodeKind::ValField
        | NodeKind::VarField
        | NodeKind::Property
        | NodeKind::CSharpProperty
        | NodeKind::StructTag
        | NodeKind::InitBlock
        | NodeKind::Export => 0.5,
        // File / generic-parameter — neutral
        NodeKind::File | NodeKind::GenericParam | NodeKind::PascalProgram => 0.0,
        // References & containers — push below definitions
        NodeKind::Use | NodeKind::Include => -3.0,
        NodeKind::AnnotationUsage | NodeKind::Decorator => -2.0,
        NodeKind::Module
        | NodeKind::Package
        | NodeKind::Namespace
        | NodeKind::ScalaPackage
        | NodeKind::GoPackage
        | NodeKind::KotlinPackage
        | NodeKind::PascalUnit
        | NodeKind::Library => -1.5,
    }
}

/// Parses every `#[derive(A, B, C)]` attribute appearing in `content`
/// between (0-based, inclusive) `start_line` and `end_line`. Multiple
/// derive attributes stack — `#[derive(Debug)]` and `#[derive(Clone)]` on
/// the same item both contribute. The returned list is de-duplicated and
/// preserves source order (Debug before Clone if that's how they're
/// written).
fn parse_derives_in_attr_block(content: &str, start_line: u32, end_line: u32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let lines: Vec<&str> = content.lines().collect();
    let start = start_line as usize;
    let end = (end_line as usize).min(lines.len().saturating_sub(1));
    if start >= lines.len() {
        return out;
    }
    // Join the attribute block into a single string so multi-line
    // `#[derive(\n  Debug,\n  Clone,\n)]` (rustfmt's split form for long
    // derive lists) is handled uniformly with the single-line variant.
    let block = lines[start..=end].join("\n");
    let mut search_from = 0usize;
    while let Some(start_idx) = block[search_from..].find("#[derive(") {
        let abs_start = search_from + start_idx + "#[derive(".len();
        let Some(close_offset) = block[abs_start..].find(')') else {
            break;
        };
        let inner = &block[abs_start..abs_start + close_offset];
        for name in inner.split(',') {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            // Strip the path prefix on fully-qualified derives so callers
            // see `Serialize` not `serde::Serialize`. Matches the convention
            // the static derive table uses.
            let short = name.rsplit("::").next().unwrap_or(name).to_string();
            if seen.insert(short.clone()) {
                out.push(short);
            }
        }
        search_from = abs_start + close_offset + 1;
    }
    out
}

/// Normalises an external file path (typically from a `cargo check` /
/// `cargo clippy` diagnostic span) into the project-relative,
/// forward-slash form the index stores. Handles three real-world shapes:
///
/// - Absolute paths (cargo emits them when `--manifest-path` points at a
///   project root that differs from `cwd`): strip the `project_root`
///   prefix so `/abs/path/to/project/src/lib.rs` becomes `src/lib.rs`.
/// - Backslash paths (Windows cargo): convert `\` → `/`.
/// - Already-relative forward-slash paths: pass through unchanged.
///
/// Falls back to returning the input verbatim if no transformation
/// applies — `get_nodes_by_file` will then handle "no such file" the
/// same way it always does.
fn normalize_lookup_path(project_root: &std::path::Path, raw: &str) -> String {
    let forward = raw.replace('\\', "/");
    let path = std::path::Path::new(&forward);
    if path.is_absolute() {
        // Try canonicalising both sides; canonicalisation handles
        // symlinks, `..` segments, and trailing slashes uniformly. If
        // either fails (file doesn't exist on disk, project root
        // moved), fall back to a raw prefix strip.
        if let (Ok(abs), Ok(root)) = (path.canonicalize(), project_root.canonicalize()) {
            if let Ok(rel) = abs.strip_prefix(&root) {
                return rel.to_string_lossy().replace('\\', "/");
            }
        }
        let root_str = project_root.to_string_lossy();
        if let Some(rel) = forward.strip_prefix(root_str.as_ref()) {
            return rel.trim_start_matches('/').to_string();
        }
    }
    forward
}

/// True when the user-supplied query matches either the node's short `name`
/// or its `qualified_name`. Matching is exact on the short name and substring
/// on the qualified name, so callers can pass either form for the impl/trait
/// filter on `tracedecay_impls`.
fn node_name_matches(node: &Node, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    node.name == query || node.qualified_name == query || node.qualified_name.contains(query)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod derive_parse_tests {
    use super::parse_derives_in_attr_block;

    #[test]
    fn parses_single_derive_block() {
        let src = "\
#[derive(Debug, Clone, PartialEq)]
pub struct Foo;
";
        let derives = parse_derives_in_attr_block(src, 0, 1);
        assert_eq!(derives, vec!["Debug", "Clone", "PartialEq"]);
    }

    #[test]
    fn stacks_multiple_derive_attributes() {
        let src = "\
#[derive(Debug)]
#[derive(Clone, Hash)]
pub enum K {}
";
        let derives = parse_derives_in_attr_block(src, 0, 2);
        assert_eq!(derives, vec!["Debug", "Clone", "Hash"]);
    }

    #[test]
    fn strips_path_prefix_on_qualified_derive() {
        let src = "#[derive(serde::Serialize, Debug)]\npub struct S;\n";
        let derives = parse_derives_in_attr_block(src, 0, 1);
        assert_eq!(derives, vec!["Serialize", "Debug"]);
    }

    #[test]
    fn ignores_non_derive_attributes() {
        let src = "\
#[cfg(feature = \"foo\")]
#[serde(rename = \"x\")]
#[derive(Debug)]
pub struct S;
";
        let derives = parse_derives_in_attr_block(src, 0, 3);
        assert_eq!(derives, vec!["Debug"]);
    }

    #[test]
    fn deduplicates_repeated_derives() {
        let src = "#[derive(Debug, Debug, Clone)]\npub struct S;\n";
        let derives = parse_derives_in_attr_block(src, 0, 1);
        assert_eq!(derives, vec!["Debug", "Clone"]);
    }

    /// Regression: rustfmt splits long derive lists across lines:
    ///   `#[derive(\n    Debug,\n    Clone,\n    PartialEq,\n)]`
    /// The previous line-bounded parser dropped all of these because it
    /// only matched `#[derive(...)]` when the closing `)` was on the
    /// same line. Production codebases with realistic-sized derive
    /// lists were getting empty `derives` output.
    #[test]
    fn parses_multiline_derive_attribute() {
        let src = "\
#[derive(
    Debug,
    Clone,
    PartialEq,
)]
pub struct Wide;
";
        let derives = parse_derives_in_attr_block(src, 0, 5);
        assert_eq!(derives, vec!["Debug", "Clone", "PartialEq"]);
    }

    #[test]
    fn parses_multiline_derive_mixed_with_single_line() {
        let src = "\
#[derive(Debug)]
#[derive(
    Clone,
    Hash,
)]
pub struct M;
";
        let derives = parse_derives_in_attr_block(src, 0, 5);
        assert_eq!(derives, vec!["Debug", "Clone", "Hash"]);
    }
}
