// Rust guideline compliant 2025-10-17
use std::collections::{HashMap, HashSet, VecDeque};

use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

/// A path through the graph: a sequence of nodes, each paired with the
/// optional edge used to reach it (the first node has `None`).
pub type GraphPath = Vec<(Node, Option<Edge>)>;

/// Rejects a blank node identifier with a typed error.
///
/// Every entry point below is reachable straight from MCP/CLI tool
/// arguments (`tracedecay_impact --args '{"node_id":""}'`). Argument shape is
/// caller input, not an internal invariant, so it is validated, not asserted:
/// a panic here unwinds the daemon's client task and the caller sees only
/// "daemon closed the connection" with no indication of which argument was
/// wrong.
fn require_traversal_id(value: &str, operation: &str, parameter: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(TraceDecayError::Config {
            message: format!("{operation} requires a non-empty {parameter}"),
        });
    }
    Ok(())
}

/// Rejects a zero traversal depth with a typed error.
///
/// Tool handlers clamp a caller-supplied depth with `min(max)`, which leaves
/// `0` intact, so an explicit `{"max_depth": 0}` reaches these functions
/// directly.
fn require_positive_depth(depth: u64, operation: &str, parameter: &str) -> Result<()> {
    if depth == 0 {
        return Err(TraceDecayError::Config {
            message: format!("{operation} requires {parameter} to be at least 1"),
        });
    }
    Ok(())
}

/// Performs graph traversal operations on the code graph.
pub struct GraphTraverser<'a> {
    db: &'a Database,
}

impl<'a> GraphTraverser<'a> {
    /// Creates a new `GraphTraverser` backed by the given database.
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Performs a breadth-first traversal starting from `start_id`.
    ///
    /// Respects the traversal options including max depth, edge kind filter,
    /// node kind filter, direction, and result limit. Returns a `Subgraph`
    /// containing the discovered nodes and the edges used to reach them.
    pub async fn traverse_bfs(&self, start_id: &str, opts: &TraversalOptions) -> Result<Subgraph> {
        require_traversal_id(start_id, "traverse_bfs", "start_id")?;
        require_positive_depth(u64::from(opts.max_depth), "traverse_bfs", "max_depth")?;
        let mut visited: HashSet<String> = HashSet::new();
        let mut result_nodes: Vec<Node> = Vec::new();
        let mut result_edges: Vec<Edge> = Vec::new();
        let mut roots: Vec<String> = Vec::new();

        // Queue holds (node_id, current_depth).
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();

        // Optionally include the start node.
        if let Some(start_node) = self.db.get_node_by_id(start_id).await? {
            visited.insert(start_id.to_string());
            if opts.include_start && Self::node_matches_filter(&start_node, opts) {
                roots.push(start_id.to_string());
                result_nodes.push(start_node);
            }
            queue.push_back((start_id.to_string(), 0));
        } else {
            return Ok(Subgraph {
                nodes: Vec::new(),
                edges: Vec::new(),
                roots: Vec::new(),
            });
        }

        let edge_filter = opts.edge_kinds.as_deref().unwrap_or(&[]);

        // Level-batched BFS. The queue is FIFO with monotonically
        // non-decreasing depth, and every entry pushed while processing a
        // level carries `depth + 1`, so the queue always holds exactly one
        // depth group at a time. We drain that whole frontier, fetch its
        // edges, neighbor nodes, and container children in ONE bulk call each
        // (instead of one round-trip per frontier node), then replay the
        // original per-node visit logic against the prefetched maps. Visit
        // order, the synthesized `Contains` edges, dedup, depth bounds, and
        // the `limit` early-out are all preserved exactly — only the number
        // of DB round-trips changes (O(visited) -> O(depth)).
        'outer: while !queue.is_empty() {
            let level = Self::drain_level(&mut queue);
            let depth = level[0].1;
            if depth >= opts.max_depth {
                continue;
            }
            if result_nodes.len() >= opts.limit as usize {
                break;
            }

            let level_ids: Vec<String> = level.iter().map(|(id, _)| id.clone()).collect();

            // One (or two, for `Both`) bulk edge queries for the whole frontier.
            let edges_by_node = self
                .bulk_edges_by_node(&level_ids, edge_filter, &opts.direction)
                .await?;

            // Every neighbor id reachable from the frontier, in first-seen
            // order, fetched with a single `get_nodes_by_ids`.
            let mut neighbor_ids: Vec<String> = Vec::new();
            let mut seen_neighbor: HashSet<String> = HashSet::new();
            for (current_id, _) in &level {
                if let Some(edges) = edges_by_node.get(current_id) {
                    for edge in edges {
                        let nid = Self::neighbor_id(edge, current_id, &opts.direction);
                        if seen_neighbor.insert(nid.clone()) {
                            neighbor_ids.push(nid);
                        }
                    }
                }
            }
            let neighbor_nodes = self.db.get_nodes_by_ids(&neighbor_ids).await?;
            let neighbor_map: HashMap<String, Node> = neighbor_nodes
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect();

            // For incoming traversals, prefetch children of every container
            // neighbor in one query so the `Contains` synthesis below is not a
            // per-node round-trip. Fetching a superset (containers that later
            // get filtered/skipped) is harmless.
            let children_by_parent = if opts.direction == TraversalDirection::Incoming {
                let container_ids: Vec<String> = neighbor_ids
                    .iter()
                    .filter(|id| {
                        neighbor_map
                            .get(*id)
                            .is_some_and(|n| is_container_kind(&n.kind))
                    })
                    .cloned()
                    .collect();
                Self::group_children(self.db.get_children_of_bulk(&container_ids).await?)
            } else {
                HashMap::new()
            };

            for (current_id, depth) in &level {
                if result_nodes.len() >= opts.limit as usize {
                    break 'outer;
                }
                let Some(edges) = edges_by_node.get(current_id) else {
                    continue;
                };

                for edge in edges {
                    let neighbor_id = Self::neighbor_id(edge, current_id, &opts.direction);

                    if visited.contains(&neighbor_id) {
                        continue;
                    }

                    let Some(neighbor_node) = neighbor_map.get(&neighbor_id) else {
                        continue;
                    };

                    visited.insert(neighbor_id.clone());

                    if Self::node_matches_filter(neighbor_node, opts) {
                        if opts.direction == TraversalDirection::Incoming
                            && is_container_kind(&neighbor_node.kind)
                        {
                            // Children are now queried via parent_id, not via
                            // outgoing Contains edges (denormalized in v9).
                            // Synthesize Contains-shaped Edge values so callers
                            // that inspect `result_edges` see the same shape.
                            if let Some(children) = children_by_parent.get(&neighbor_id) {
                                for child in children {
                                    if !visited.contains(&child.id) {
                                        visited.insert(child.id.clone());
                                        result_edges.push(crate::types::Edge {
                                            source: neighbor_id.clone(),
                                            target: child.id.clone(),
                                            kind: EdgeKind::Contains,
                                            line: None,
                                        });
                                        queue.push_back((child.id.clone(), depth + 1));
                                    }
                                }
                            }
                        }

                        result_nodes.push(neighbor_node.clone());
                        result_edges.push(edge.clone());
                        queue.push_back((neighbor_id, depth + 1));

                        if result_nodes.len() >= opts.limit as usize {
                            break 'outer;
                        }
                    } else {
                        result_edges.push(edge.clone());
                        queue.push_back((neighbor_id, depth + 1));
                    }
                }
            }
        }

        Ok(Subgraph {
            nodes: result_nodes,
            edges: result_edges,
            roots,
        })
    }

    /// Performs a depth-first traversal starting from `start_id`.
    ///
    /// Respects the traversal options including max depth, edge kind filter,
    /// node kind filter, direction, and result limit. Returns a `Subgraph`
    /// containing the discovered nodes and edges.
    ///
    /// Uses an iterative approach with an explicit stack to avoid async
    /// recursion issues.
    pub async fn traverse_dfs(&self, start_id: &str, opts: &TraversalOptions) -> Result<Subgraph> {
        require_traversal_id(start_id, "traverse_dfs", "start_id")?;
        require_positive_depth(u64::from(opts.max_depth), "traverse_dfs", "max_depth")?;
        let mut visited: HashSet<String> = HashSet::new();
        let mut result_nodes: Vec<Node> = Vec::new();
        let mut result_edges: Vec<Edge> = Vec::new();
        let mut roots: Vec<String> = Vec::new();

        if let Some(start_node) = self.db.get_node_by_id(start_id).await? {
            visited.insert(start_id.to_string());
            if opts.include_start && Self::node_matches_filter(&start_node, opts) {
                roots.push(start_id.to_string());
                result_nodes.push(start_node);
            }
        } else {
            return Ok(Subgraph {
                nodes: Vec::new(),
                edges: Vec::new(),
                roots: Vec::new(),
            });
        }

        let edge_filter = opts.edge_kinds.as_deref().unwrap_or(&[]);

        // Iterative DFS using an explicit stack of (node_id, depth).
        let mut stack: Vec<(String, u32)> = vec![(start_id.to_string(), 0)];

        while let Some((current_id, depth)) = stack.pop() {
            if depth >= opts.max_depth {
                continue;
            }

            if result_nodes.len() >= opts.limit as usize {
                break;
            }

            let edges = self
                .get_edges_for_direction(&current_id, edge_filter, &opts.direction)
                .await?;

            let neighbor_ids: Vec<String> = edges
                .iter()
                .map(|edge| Self::neighbor_id(edge, &current_id, &opts.direction))
                .filter(|id| !visited.contains(id))
                .collect();

            if neighbor_ids.is_empty() {
                continue;
            }

            let neighbor_nodes = self.db.get_nodes_by_ids(&neighbor_ids).await?;
            let neighbor_map: std::collections::HashMap<String, Node> = neighbor_nodes
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect();

            for edge in edges {
                let neighbor_id = Self::neighbor_id(&edge, &current_id, &opts.direction);

                if visited.contains(&neighbor_id) {
                    continue;
                }

                let Some(neighbor_node) = neighbor_map.get(&neighbor_id) else {
                    continue;
                };

                visited.insert(neighbor_id.clone());

                if Self::node_matches_filter(neighbor_node, opts) {
                    result_nodes.push(neighbor_node.clone());
                    result_edges.push(edge.clone());
                    stack.push((neighbor_id, depth + 1));

                    if result_nodes.len() >= opts.limit as usize {
                        break;
                    }
                } else {
                    result_edges.push(edge.clone());
                    stack.push((neighbor_id, depth + 1));
                }
            }
        }

        Ok(Subgraph {
            nodes: result_nodes,
            edges: result_edges,
            roots,
        })
    }

    /// Gets all nodes that call the given node, up to `max_depth` levels.
    ///
    /// Follows incoming `Calls` edges to find callers transitively.
    pub async fn get_callers(&self, node_id: &str, max_depth: usize) -> Result<Vec<(Node, Edge)>> {
        require_traversal_id(node_id, "get_callers", "node_id")?;
        require_positive_depth(max_depth as u64, "get_callers", "max_depth")?;
        let mut results: Vec<(Node, Edge)> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(node_id.to_string());

        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((node_id.to_string(), 0));

        // Level-batched: one `get_incoming_edges_bulk` + one `get_nodes_by_ids`
        // for the whole frontier per depth, instead of a pair of round-trips
        // per frontier node. Visit order and dedup match the per-node walk.
        while !queue.is_empty() {
            let level = Self::drain_level(&mut queue);
            let depth = level[0].1;
            if depth >= max_depth {
                continue;
            }

            let level_ids: Vec<String> = level.iter().map(|(id, _)| id.clone()).collect();
            let edges_by_target = Self::group_by(
                self.db
                    .get_incoming_edges_bulk(&level_ids, &[EdgeKind::Calls])
                    .await?,
                |e| e.target.clone(),
            );

            let mut caller_ids: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for (current_id, _) in &level {
                if let Some(edges) = edges_by_target.get(current_id) {
                    for edge in edges {
                        if seen.insert(edge.source.clone()) {
                            caller_ids.push(edge.source.clone());
                        }
                    }
                }
            }
            let caller_map: HashMap<String, Node> = self
                .db
                .get_nodes_by_ids(&caller_ids)
                .await?
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect();

            for (current_id, depth) in &level {
                let Some(edges) = edges_by_target.get(current_id) else {
                    continue;
                };
                for edge in edges {
                    let caller_id = &edge.source;
                    if visited.contains(caller_id) {
                        continue;
                    }
                    if let Some(caller_node) = caller_map.get(caller_id) {
                        visited.insert(caller_id.clone());
                        queue.push_back((caller_id.clone(), depth + 1));
                        results.push((caller_node.clone(), edge.clone()));
                    }
                }
            }
        }

        Ok(results)
    }

    /// Gets all nodes that the given node calls, up to `max_depth` levels.
    ///
    /// Follows outgoing `Calls` edges to find callees transitively.
    pub async fn get_callees(&self, node_id: &str, max_depth: usize) -> Result<Vec<(Node, Edge)>> {
        require_traversal_id(node_id, "get_callees", "node_id")?;
        require_positive_depth(max_depth as u64, "get_callees", "max_depth")?;
        let mut results: Vec<(Node, Edge)> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(node_id.to_string());

        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((node_id.to_string(), 0));

        // Level-batched: one `get_outgoing_edges_bulk` + one `get_nodes_by_ids`
        // for the whole frontier per depth. Visit order and dedup match the
        // per-node walk.
        while !queue.is_empty() {
            let level = Self::drain_level(&mut queue);
            let depth = level[0].1;
            if depth >= max_depth {
                continue;
            }

            let level_ids: Vec<String> = level.iter().map(|(id, _)| id.clone()).collect();
            let edges_by_source = Self::group_by(
                self.db
                    .get_outgoing_edges_bulk(&level_ids, &[EdgeKind::Calls])
                    .await?,
                |e| e.source.clone(),
            );

            let mut callee_ids: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for (current_id, _) in &level {
                if let Some(edges) = edges_by_source.get(current_id) {
                    for edge in edges {
                        if seen.insert(edge.target.clone()) {
                            callee_ids.push(edge.target.clone());
                        }
                    }
                }
            }
            let callee_map: HashMap<String, Node> = self
                .db
                .get_nodes_by_ids(&callee_ids)
                .await?
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect();

            for (current_id, depth) in &level {
                let Some(edges) = edges_by_source.get(current_id) else {
                    continue;
                };
                for edge in edges {
                    let callee_id = &edge.target;
                    if visited.contains(callee_id) {
                        continue;
                    }
                    if let Some(callee_node) = callee_map.get(callee_id) {
                        visited.insert(callee_id.clone());
                        queue.push_back((callee_id.clone(), depth + 1));
                        results.push((callee_node.clone(), edge.clone()));
                    }
                }
            }
        }

        Ok(results)
    }

    /// Computes the impact radius of a node: all nodes that directly or
    /// indirectly reference or call this node.
    ///
    /// Performs a BFS over incoming edges of all kinds up to `max_depth`.
    pub async fn get_impact_radius(&self, node_id: &str, max_depth: usize) -> Result<Subgraph> {
        require_traversal_id(node_id, "get_impact_radius", "node_id")?;
        require_positive_depth(max_depth as u64, "get_impact_radius", "max_depth")?;
        let opts = TraversalOptions {
            max_depth: max_depth as u32,
            edge_kinds: None,
            node_kinds: None,
            direction: TraversalDirection::Incoming,
            limit: u32::MAX,
            include_start: true,
        };
        self.traverse_bfs(node_id, &opts).await
    }

    /// Same as `get_impact_radius` but seeded from many nodes with a shared
    /// `visited` set. Avoids the quadratic re-traversal that happens when
    /// callers loop `get_impact_radius` per modified symbol — diamond
    /// dependencies (one downstream node reachable from many sources) get
    /// walked once instead of N times.
    ///
    /// Returns every reachable node, including the seeds themselves.
    pub async fn get_impact_radius_multi(
        &self,
        seed_ids: &[String],
        max_depth: usize,
    ) -> Result<Vec<Node>> {
        require_positive_depth(max_depth as u64, "get_impact_radius_multi", "max_depth")?;
        if seed_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut visited: HashSet<String> = seed_ids.iter().cloned().collect();
        let seed_nodes = self.db.get_nodes_by_ids(seed_ids).await?;
        let mut result_nodes: Vec<Node> = seed_nodes;
        let mut queue: VecDeque<(String, usize)> =
            seed_ids.iter().map(|id| (id.clone(), 0usize)).collect();

        // Level-batched: one `get_incoming_edges_bulk` + one `get_nodes_by_ids`
        // per depth for the whole frontier. The reachable node set is
        // identical to the per-node walk; ordering of the returned `Vec` is,
        // as before, unspecified (it follows `get_nodes_by_ids`).
        while !queue.is_empty() {
            let level = Self::drain_level(&mut queue);
            let depth = level[0].1;
            if depth >= max_depth {
                continue;
            }

            let level_ids: Vec<String> = level.iter().map(|(id, _)| id.clone()).collect();
            let edges_by_target = Self::group_by(
                self.db.get_incoming_edges_bulk(&level_ids, &[]).await?,
                |e| e.target.clone(),
            );

            let mut new_ids: Vec<String> = Vec::new();
            for (current_id, _) in &level {
                if let Some(edges) = edges_by_target.get(current_id) {
                    for edge in edges {
                        if visited.insert(edge.source.clone()) {
                            new_ids.push(edge.source.clone());
                        }
                    }
                }
            }
            if new_ids.is_empty() {
                continue;
            }
            let child_depth = depth + 1;
            for node in self.db.get_nodes_by_ids(&new_ids).await? {
                queue.push_back((node.id.clone(), child_depth));
                result_nodes.push(node);
            }
        }

        Ok(result_nodes)
    }

    /// Builds a bidirectional call graph around a node.
    ///
    /// Combines BFS over outgoing `Calls` edges (callees) and BFS over
    /// incoming `Calls` edges (callers) up to the specified `depth`.
    pub async fn get_call_graph(&self, node_id: &str, depth: usize) -> Result<Subgraph> {
        require_traversal_id(node_id, "get_call_graph", "node_id")?;
        require_positive_depth(depth as u64, "get_call_graph", "depth")?;
        // Outgoing (callees)
        let outgoing_opts = TraversalOptions {
            max_depth: depth as u32,
            edge_kinds: Some(vec![EdgeKind::Calls]),
            node_kinds: None,
            direction: TraversalDirection::Outgoing,
            limit: u32::MAX,
            include_start: true,
        };
        let outgoing_sub = self.traverse_bfs(node_id, &outgoing_opts).await?;

        // Incoming (callers)
        let incoming_opts = TraversalOptions {
            max_depth: depth as u32,
            edge_kinds: Some(vec![EdgeKind::Calls]),
            node_kinds: None,
            direction: TraversalDirection::Incoming,
            limit: u32::MAX,
            include_start: false,
        };
        let incoming_sub = self.traverse_bfs(node_id, &incoming_opts).await?;

        // Merge the two subgraphs, deduplicating nodes by ID.
        let mut seen_nodes: HashSet<String> = HashSet::new();
        let mut nodes: Vec<Node> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();
        let roots = outgoing_sub.roots;

        for node in outgoing_sub.nodes {
            if seen_nodes.insert(node.id.clone()) {
                nodes.push(node);
            }
        }
        for node in incoming_sub.nodes {
            if seen_nodes.insert(node.id.clone()) {
                nodes.push(node);
            }
        }

        // Deduplicate edges by (source, target, kind).
        let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();
        for edge in outgoing_sub.edges.into_iter().chain(incoming_sub.edges) {
            let key = (
                edge.source.clone(),
                edge.target.clone(),
                edge.kind.as_str().to_string(),
            );
            if seen_edges.insert(key) {
                edges.push(edge);
            }
        }

        Ok(Subgraph {
            nodes,
            edges,
            roots,
        })
    }

    /// Discovers the type hierarchy around a node by following `Implements` edges.
    ///
    /// Follows both outgoing (traits this node implements) and incoming
    /// (nodes that implement this trait) `Implements` edges.
    pub async fn get_type_hierarchy(&self, node_id: &str) -> Result<Subgraph> {
        require_traversal_id(node_id, "get_type_hierarchy", "node_id")?;
        let opts = TraversalOptions {
            max_depth: 10,
            edge_kinds: Some(vec![EdgeKind::Implements]),
            node_kinds: None,
            direction: TraversalDirection::Both,
            limit: u32::MAX,
            include_start: true,
        };
        self.traverse_bfs(node_id, &opts).await
    }

    /// Finds the shortest path between two nodes using BFS.
    ///
    /// If `edge_kinds` is empty, all edge types are followed. Returns `None`
    /// if no path exists. The returned path includes the start and end nodes
    /// with the edges connecting them.
    pub async fn find_path(
        &self,
        from_id: &str,
        to_id: &str,
        edge_kinds: &[EdgeKind],
    ) -> Result<Option<GraphPath>> {
        require_traversal_id(from_id, "find_path", "from_id")?;
        require_traversal_id(to_id, "find_path", "to_id")?;
        if from_id == to_id {
            if let Some(node) = self.db.get_node_by_id(from_id).await? {
                return Ok(Some(vec![(node, None)]));
            }
            return Ok(None);
        }

        // BFS: track parent info for path reconstruction.
        // parent_map: child_id -> (parent_id, edge_used)
        let mut parent_map: std::collections::HashMap<String, (String, Edge)> =
            std::collections::HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        visited.insert(from_id.to_string());
        queue.push_back(from_id.to_string());

        let mut found = false;

        while let Some(current_id) = queue.pop_front() {
            // Get outgoing edges.
            let outgoing = self.db.get_outgoing_edges(&current_id, edge_kinds).await?;
            for edge in outgoing {
                let neighbor = edge.target.clone();
                if !visited.contains(&neighbor) {
                    visited.insert(neighbor.clone());
                    let is_target = neighbor == to_id;
                    parent_map.insert(neighbor.clone(), (current_id.clone(), edge));

                    if is_target {
                        found = true;
                        break;
                    }
                    queue.push_back(neighbor);
                }
            }

            if found {
                break;
            }

            // Also get incoming edges (traverse bidirectionally for path finding).
            let incoming = self.db.get_incoming_edges(&current_id, edge_kinds).await?;
            for edge in incoming {
                let neighbor = edge.source.clone();
                if !visited.contains(&neighbor) {
                    visited.insert(neighbor.clone());
                    let is_target = neighbor == to_id;
                    parent_map.insert(neighbor.clone(), (current_id.clone(), edge));

                    if is_target {
                        found = true;
                        break;
                    }
                    queue.push_back(neighbor);
                }
            }

            if found {
                break;
            }
        }

        if !found {
            return Ok(None);
        }

        // Reconstruct path from to_id back to from_id.
        let mut path_ids: Vec<(String, Option<Edge>)> = Vec::new();
        let mut current = to_id.to_string();
        while current != from_id {
            if let Some((parent, edge)) = parent_map.remove(&current) {
                path_ids.push((current, Some(edge)));
                current = parent;
            } else {
                return Ok(None);
            }
        }
        path_ids.push((from_id.to_string(), None));
        path_ids.reverse();

        // Resolve node IDs to actual Node objects.
        let mut path: Vec<(Node, Option<Edge>)> = Vec::new();
        for (id, edge) in path_ids {
            if let Some(node) = self.db.get_node_by_id(&id).await? {
                path.push((node, edge));
            }
        }

        Ok(Some(path))
    }

    /// Finds the shortest *directed* path from `from_id` to `to_id`, following
    /// only outgoing edges of the given kinds. `max_depth` bounds the BFS so a
    /// runaway call graph can't OOM us. Returns `None` if no path exists.
    ///
    /// Use this for "call chain from A to B" semantics: BFS expands only along
    /// edges where A is the source, which models actual flow of execution. The
    /// older `find_path` does a bidirectional walk that's right for "are these
    /// connected at all" but wrong for directed-chain queries.
    pub async fn find_path_directed(
        &self,
        from_id: &str,
        to_id: &str,
        edge_kinds: &[EdgeKind],
        max_depth: usize,
    ) -> Result<Option<GraphPath>> {
        require_traversal_id(from_id, "find_path_directed", "from_id")?;
        require_traversal_id(to_id, "find_path_directed", "to_id")?;
        if from_id == to_id {
            if let Some(node) = self.db.get_node_by_id(from_id).await? {
                return Ok(Some(vec![(node, None)]));
            }
            return Ok(None);
        }

        let mut parent_map: HashMap<String, (String, Edge)> = HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        visited.insert(from_id.to_string());
        queue.push_back((from_id.to_string(), 0));

        // Level-batched directed BFS: one `get_outgoing_edges_bulk` per depth
        // for the whole frontier. Because BFS visits a node the first time it
        // is reached and that first-reach ordering is unchanged (same frontier
        // order, same per-source edge order), `parent_map` — and therefore the
        // reconstructed shortest path — is byte-identical to the per-node walk.
        let mut found = false;
        'outer: while !queue.is_empty() {
            let level = Self::drain_level(&mut queue);
            let depth = level[0].1;
            if depth >= max_depth {
                continue;
            }

            let level_ids: Vec<String> = level.iter().map(|(id, _)| id.clone()).collect();
            let edges_by_source = Self::group_by(
                self.db
                    .get_outgoing_edges_bulk(&level_ids, edge_kinds)
                    .await?,
                |e| e.source.clone(),
            );

            for (current_id, depth) in &level {
                let Some(outgoing) = edges_by_source.get(current_id) else {
                    continue;
                };
                for edge in outgoing {
                    let neighbor = edge.target.clone();
                    if visited.contains(&neighbor) {
                        continue;
                    }
                    visited.insert(neighbor.clone());
                    let is_target = neighbor == to_id;
                    parent_map.insert(neighbor.clone(), (current_id.clone(), edge.clone()));
                    if is_target {
                        found = true;
                        break 'outer;
                    }
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        if !found {
            return Ok(None);
        }

        let mut path_ids: Vec<(String, Option<Edge>)> = Vec::new();
        let mut current = to_id.to_string();
        while current != from_id {
            if let Some((parent, edge)) = parent_map.remove(&current) {
                path_ids.push((current, Some(edge)));
                current = parent;
            } else {
                return Ok(None);
            }
        }
        path_ids.push((from_id.to_string(), None));
        path_ids.reverse();

        let mut path: Vec<(Node, Option<Edge>)> = Vec::new();
        for (id, edge) in path_ids {
            if let Some(node) = self.db.get_node_by_id(&id).await? {
                path.push((node, edge));
            }
        }
        Ok(Some(path))
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Removes and returns every queue entry that shares the front entry's
    /// depth — i.e. the entire current BFS frontier.
    ///
    /// Relies on the level-batched invariant that the queue only ever holds
    /// one depth group at a time (every entry pushed while a level is being
    /// processed carries `parent_depth + 1`, and all lower-depth entries are
    /// drained before any are pushed). Returns an empty vec for an empty queue.
    fn drain_level<D: Copy + PartialEq>(queue: &mut VecDeque<(String, D)>) -> Vec<(String, D)> {
        let depth = match queue.front() {
            Some(&(_, d)) => d,
            None => return Vec::new(),
        };
        let mut level = Vec::new();
        while let Some(&(_, d)) = queue.front() {
            if d != depth {
                break;
            }
            level.push(queue.pop_front().expect("front peeked above"));
        }
        level
    }

    /// Fetches every edge touching the whole frontier in one bulk query per
    /// direction and groups them by the *current* node they belong to, so the
    /// per-node BFS body can look up its edges with zero extra round-trips.
    ///
    /// The per-node edge order matches `get_edges_for_direction`: for a fixed
    /// current node the bulk query returns rows in the same index order as the
    /// single-node query, and for `Both` all outgoing edges precede all
    /// incoming edges (outgoing bulk is drained before incoming bulk).
    async fn bulk_edges_by_node(
        &self,
        ids: &[String],
        edge_kinds: &[EdgeKind],
        direction: &TraversalDirection,
    ) -> Result<HashMap<String, Vec<Edge>>> {
        let mut map: HashMap<String, Vec<Edge>> = HashMap::new();
        match direction {
            TraversalDirection::Outgoing => {
                for edge in self.db.get_outgoing_edges_bulk(ids, edge_kinds).await? {
                    map.entry(edge.source.clone()).or_default().push(edge);
                }
            }
            TraversalDirection::Incoming => {
                for edge in self.db.get_incoming_edges_bulk(ids, edge_kinds).await? {
                    map.entry(edge.target.clone()).or_default().push(edge);
                }
            }
            TraversalDirection::Both => {
                for edge in self.db.get_outgoing_edges_bulk(ids, edge_kinds).await? {
                    map.entry(edge.source.clone()).or_default().push(edge);
                }
                for edge in self.db.get_incoming_edges_bulk(ids, edge_kinds).await? {
                    map.entry(edge.target.clone()).or_default().push(edge);
                }
            }
        }
        Ok(map)
    }

    /// Groups edges by a caller-chosen key (`source` or `target`), preserving
    /// encounter order within each group.
    fn group_by(edges: Vec<Edge>, key: impl Fn(&Edge) -> String) -> HashMap<String, Vec<Edge>> {
        let mut map: HashMap<String, Vec<Edge>> = HashMap::new();
        for edge in edges {
            map.entry(key(&edge)).or_default().push(edge);
        }
        map
    }

    /// Groups bulk-fetched children by their `parent_id`. Because
    /// `get_children_of_bulk` orders rows by `(parent_id, start_line)`, each
    /// group is `start_line`-ordered — identical to `get_children_of`.
    fn group_children(children: Vec<Node>) -> HashMap<String, Vec<Node>> {
        let mut map: HashMap<String, Vec<Node>> = HashMap::new();
        for child in children {
            if let Some(parent_id) = child.parent_id.clone() {
                map.entry(parent_id).or_default().push(child);
            }
        }
        map
    }

    /// Gets edges from the database according to the traversal direction.
    async fn get_edges_for_direction(
        &self,
        node_id: &str,
        edge_kinds: &[EdgeKind],
        direction: &TraversalDirection,
    ) -> Result<Vec<Edge>> {
        match direction {
            TraversalDirection::Outgoing => self.db.get_outgoing_edges(node_id, edge_kinds).await,
            TraversalDirection::Incoming => self.db.get_incoming_edges(node_id, edge_kinds).await,
            TraversalDirection::Both => {
                let mut edges = self.db.get_outgoing_edges(node_id, edge_kinds).await?;
                edges.extend(self.db.get_incoming_edges(node_id, edge_kinds).await?);
                Ok(edges)
            }
        }
    }

    /// Returns the neighbor node ID from an edge, depending on direction.
    ///
    /// For outgoing: the neighbor is `edge.target`.
    /// For incoming: the neighbor is `edge.source`.
    /// For both: whichever end is not `current_id`.
    fn neighbor_id(edge: &Edge, current_id: &str, direction: &TraversalDirection) -> String {
        match direction {
            TraversalDirection::Outgoing => edge.target.clone(),
            TraversalDirection::Incoming => edge.source.clone(),
            TraversalDirection::Both => {
                if edge.source == current_id {
                    edge.target.clone()
                } else {
                    edge.source.clone()
                }
            }
        }
    }

    /// Checks whether a node passes the optional `node_kinds` filter.
    fn node_matches_filter(node: &Node, opts: &TraversalOptions) -> bool {
        if let Some(ref kinds) = opts.node_kinds
            && !kinds.is_empty()
        {
            return kinds.contains(&node.kind);
        }
        true
    }
}

/// Returns true if a node kind is a container that can hold child symbols.
fn is_container_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Interface
            | NodeKind::Module
            | NodeKind::Impl
            | NodeKind::Enum
    )
}

#[cfg(test)]
mod batching_tests {
    //! Correctness + performance coverage for the level-batched BFS core.
    //!
    //! Each traversal is checked against a *reference* implementation that
    //! reproduces the pre-batching per-frontier-node walk (one DB round-trip
    //! per node). Both run against the same fixture graph, and the batched
    //! result must be byte-identical — same nodes, same edges, same order —
    //! proving the round-trip reduction did not change semantics.

    use super::*;
    use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

    async fn new_db() -> (Database, tempfile::TempDir) {
        // The runtime-core store registry fails closed until the root crate
        // installs its schema builder; this is idempotent.
        crate::daemon::store_runtime::register_registered_schema_installer();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "traversal batching test").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        (db, temp)
    }

    fn func(id: &str, line: u32) -> Node {
        node(id, line, NodeKind::Function, None)
    }

    fn node(id: &str, line: u32, kind: NodeKind, parent_id: Option<&str>) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: id.to_string(),
            qualified_name: id.to_string(),
            file_path: "src/lib.rs".to_string(),
            start_line: line,
            attrs_start_line: line,
            end_line: line + 1,
            start_column: 0,
            end_column: 0,
            signature: None,
            docstring: None,
            visibility: Visibility::default(),
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 0,
            parent_id: parent_id.map(str::to_string),
        }
    }

    fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
        Edge {
            source: source.to_string(),
            target: target.to_string(),
            kind,
            line: None,
        }
    }

    /// Builds a graph exercising fan-in, fan-out, a chain, and a cycle.
    ///
    /// * chain:   c0 -> c1 -> c2 -> c3            (Calls)
    /// * fan-in:  in0..in4 -> hub                 (Calls) — wide callers frontier
    /// * fan-out: spread -> out0..out4            (Calls) — wide callees frontier
    /// * cycle:   p -> q -> r -> p                (Calls)
    async fn build_call_fixture(db: &Database) {
        let mut nodes = vec![
            func("c0", 1),
            func("c1", 2),
            func("c2", 3),
            func("c3", 4),
            func("hub", 5),
            func("spread", 6),
            func("p", 7),
            func("q", 8),
            func("r", 9),
        ];
        let mut edges = vec![
            edge("c0", "c1", EdgeKind::Calls),
            edge("c1", "c2", EdgeKind::Calls),
            edge("c2", "c3", EdgeKind::Calls),
            edge("p", "q", EdgeKind::Calls),
            edge("q", "r", EdgeKind::Calls),
            edge("r", "p", EdgeKind::Calls),
        ];
        for i in 0..5 {
            let caller = format!("in{i}");
            nodes.push(func(&caller, 20 + i));
            edges.push(edge(&caller, "hub", EdgeKind::Calls));
            let callee = format!("out{i}");
            nodes.push(func(&callee, 40 + i));
            edges.push(edge("spread", &callee, EdgeKind::Calls));
        }
        db.insert_nodes(&nodes).await.unwrap();
        db.insert_edges(&edges).await.unwrap();
    }

    // ------------------------------------------------------------------
    // Reference (pre-batching) implementations: one round-trip per node.
    // ------------------------------------------------------------------

    async fn ref_callers(db: &Database, node_id: &str, max_depth: usize) -> Vec<(Node, Edge)> {
        let mut results = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(node_id.to_string());
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((node_id.to_string(), 0));
        while let Some((current_id, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let edges = db
                .get_incoming_edges(&current_id, &[EdgeKind::Calls])
                .await
                .unwrap();
            let caller_ids: Vec<String> = edges
                .iter()
                .map(|e| e.source.clone())
                .filter(|id| !visited.contains(id))
                .collect();
            if caller_ids.is_empty() {
                continue;
            }
            let caller_map: HashMap<String, Node> = db
                .get_nodes_by_ids(&caller_ids)
                .await
                .unwrap()
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect();
            for edge in edges {
                let caller_id = &edge.source;
                if visited.contains(caller_id) {
                    continue;
                }
                if let Some(caller_node) = caller_map.get(caller_id) {
                    visited.insert(caller_id.clone());
                    queue.push_back((caller_id.clone(), depth + 1));
                    results.push((caller_node.clone(), edge));
                }
            }
        }
        results
    }

    async fn ref_callees(db: &Database, node_id: &str, max_depth: usize) -> Vec<(Node, Edge)> {
        let mut results = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(node_id.to_string());
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((node_id.to_string(), 0));
        while let Some((current_id, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let edges = db
                .get_outgoing_edges(&current_id, &[EdgeKind::Calls])
                .await
                .unwrap();
            let callee_ids: Vec<String> = edges
                .iter()
                .map(|e| e.target.clone())
                .filter(|id| !visited.contains(id))
                .collect();
            if callee_ids.is_empty() {
                continue;
            }
            let callee_map: HashMap<String, Node> = db
                .get_nodes_by_ids(&callee_ids)
                .await
                .unwrap()
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect();
            for edge in edges {
                let callee_id = &edge.target;
                if visited.contains(callee_id) {
                    continue;
                }
                if let Some(callee_node) = callee_map.get(callee_id) {
                    visited.insert(callee_id.clone());
                    queue.push_back((callee_id.clone(), depth + 1));
                    results.push((callee_node.clone(), edge));
                }
            }
        }
        results
    }

    /// Reference `traverse_bfs`, mirroring the pre-batching per-node walk
    /// including the incoming-container `Contains` synthesis.
    async fn ref_traverse_bfs(db: &Database, start_id: &str, opts: &TraversalOptions) -> Subgraph {
        let mut visited: HashSet<String> = HashSet::new();
        let mut result_nodes: Vec<Node> = Vec::new();
        let mut result_edges: Vec<Edge> = Vec::new();
        let mut roots: Vec<String> = Vec::new();
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();
        if let Some(start_node) = db.get_node_by_id(start_id).await.unwrap() {
            visited.insert(start_id.to_string());
            if opts.include_start && GraphTraverser::node_matches_filter(&start_node, opts) {
                roots.push(start_id.to_string());
                result_nodes.push(start_node);
            }
            queue.push_back((start_id.to_string(), 0));
        } else {
            return Subgraph::default();
        }
        let edge_filter = opts.edge_kinds.as_deref().unwrap_or(&[]);
        while let Some((current_id, depth)) = queue.pop_front() {
            if depth >= opts.max_depth {
                continue;
            }
            if result_nodes.len() >= opts.limit as usize {
                break;
            }
            let edges = match &opts.direction {
                TraversalDirection::Outgoing => db
                    .get_outgoing_edges(&current_id, edge_filter)
                    .await
                    .unwrap(),
                TraversalDirection::Incoming => db
                    .get_incoming_edges(&current_id, edge_filter)
                    .await
                    .unwrap(),
                TraversalDirection::Both => {
                    let mut e = db
                        .get_outgoing_edges(&current_id, edge_filter)
                        .await
                        .unwrap();
                    e.extend(
                        db.get_incoming_edges(&current_id, edge_filter)
                            .await
                            .unwrap(),
                    );
                    e
                }
            };
            let neighbor_ids: Vec<String> = edges
                .iter()
                .map(|e| GraphTraverser::neighbor_id(e, &current_id, &opts.direction))
                .filter(|id| !visited.contains(id))
                .collect();
            if neighbor_ids.is_empty() {
                continue;
            }
            let neighbor_map: HashMap<String, Node> = db
                .get_nodes_by_ids(&neighbor_ids)
                .await
                .unwrap()
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect();
            for edge in edges {
                let neighbor_id = GraphTraverser::neighbor_id(&edge, &current_id, &opts.direction);
                if visited.contains(&neighbor_id) {
                    continue;
                }
                let Some(neighbor_node) = neighbor_map.get(&neighbor_id) else {
                    continue;
                };
                visited.insert(neighbor_id.clone());
                if GraphTraverser::node_matches_filter(neighbor_node, opts) {
                    if opts.direction == TraversalDirection::Incoming
                        && is_container_kind(&neighbor_node.kind)
                    {
                        let children = db.get_children_of(&neighbor_id).await.unwrap();
                        for child in children {
                            if !visited.contains(&child.id) {
                                visited.insert(child.id.clone());
                                result_edges.push(Edge {
                                    source: neighbor_id.clone(),
                                    target: child.id.clone(),
                                    kind: EdgeKind::Contains,
                                    line: None,
                                });
                                queue.push_back((child.id, depth + 1));
                            }
                        }
                    }
                    result_nodes.push(neighbor_node.clone());
                    result_edges.push(edge.clone());
                    queue.push_back((neighbor_id, depth + 1));
                    if result_nodes.len() >= opts.limit as usize {
                        break;
                    }
                } else {
                    result_edges.push(edge.clone());
                    queue.push_back((neighbor_id, depth + 1));
                }
            }
        }
        Subgraph {
            nodes: result_nodes,
            edges: result_edges,
            roots,
        }
    }

    async fn ref_find_path_directed(
        db: &Database,
        from_id: &str,
        to_id: &str,
        edge_kinds: &[EdgeKind],
        max_depth: usize,
    ) -> Option<GraphPath> {
        if from_id == to_id {
            return db
                .get_node_by_id(from_id)
                .await
                .unwrap()
                .map(|n| vec![(n, None)]);
        }
        let mut parent_map: HashMap<String, (String, Edge)> = HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        visited.insert(from_id.to_string());
        queue.push_back((from_id.to_string(), 0));
        let mut found = false;
        while let Some((current_id, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let outgoing = db
                .get_outgoing_edges(&current_id, edge_kinds)
                .await
                .unwrap();
            for edge in outgoing {
                let neighbor = edge.target.clone();
                if visited.contains(&neighbor) {
                    continue;
                }
                visited.insert(neighbor.clone());
                let is_target = neighbor == to_id;
                parent_map.insert(neighbor.clone(), (current_id.clone(), edge));
                if is_target {
                    found = true;
                    break;
                }
                queue.push_back((neighbor, depth + 1));
            }
            if found {
                break;
            }
        }
        if !found {
            return None;
        }
        let mut path_ids: Vec<(String, Option<Edge>)> = Vec::new();
        let mut current = to_id.to_string();
        while current != from_id {
            let (parent, edge) = parent_map.remove(&current)?;
            path_ids.push((current, Some(edge)));
            current = parent;
        }
        path_ids.push((from_id.to_string(), None));
        path_ids.reverse();
        let mut path: Vec<(Node, Option<Edge>)> = Vec::new();
        for (id, edge) in path_ids {
            if let Some(node) = db.get_node_by_id(&id).await.unwrap() {
                path.push((node, edge));
            }
        }
        Some(path)
    }

    fn ids(pairs: &[(Node, Edge)]) -> Vec<String> {
        pairs.iter().map(|(n, _)| n.id.clone()).collect()
    }

    #[tokio::test]
    async fn callers_identical_to_reference_fanin_and_cycle() {
        let (db, _tmp) = new_db().await;
        build_call_fixture(&db).await;
        let traverser = GraphTraverser::new(&db);

        for target in ["hub", "c3", "p", "q"] {
            for depth in [1usize, 3, 8] {
                let expected = ref_callers(&db, target, depth).await;
                let actual = traverser.get_callers(target, depth).await.unwrap();
                assert_eq!(
                    actual, expected,
                    "get_callers({target}, {depth}) diverged from reference"
                );
            }
        }
        // Sanity: hub really is a wide fan-in.
        assert_eq!(
            ids(&traverser.get_callers("hub", 1).await.unwrap()).len(),
            5
        );
    }

    #[tokio::test]
    async fn callees_identical_to_reference_fanout_and_chain() {
        let (db, _tmp) = new_db().await;
        build_call_fixture(&db).await;
        let traverser = GraphTraverser::new(&db);

        for start in ["spread", "c0", "p", "r"] {
            for depth in [1usize, 3, 8] {
                let expected = ref_callees(&db, start, depth).await;
                let actual = traverser.get_callees(start, depth).await.unwrap();
                assert_eq!(
                    actual, expected,
                    "get_callees({start}, {depth}) diverged from reference"
                );
            }
        }
        assert_eq!(
            ids(&traverser.get_callees("spread", 1).await.unwrap()).len(),
            5
        );
    }

    #[tokio::test]
    async fn impact_identical_to_reference_including_container_children() {
        let (db, _tmp) = new_db().await;
        // endpoint <- S (a Struct container) via a Uses edge; S owns two
        // methods, so incoming BFS from endpoint must synthesize Contains
        // edges for m1, m2 in start_line order.
        let nodes = vec![
            func("endpoint", 1),
            node("S", 10, NodeKind::Struct, None),
            node("m2", 12, NodeKind::Method, Some("S")),
            node("m1", 11, NodeKind::Method, Some("S")),
            func("far", 30),
        ];
        let edges = vec![
            edge("S", "endpoint", EdgeKind::Uses),
            edge("far", "S", EdgeKind::Calls),
        ];
        db.insert_nodes(&nodes).await.unwrap();
        db.insert_edges(&edges).await.unwrap();
        let traverser = GraphTraverser::new(&db);

        for depth in [1usize, 2, 4] {
            let opts = TraversalOptions {
                max_depth: depth as u32,
                edge_kinds: None,
                node_kinds: None,
                direction: TraversalDirection::Incoming,
                limit: u32::MAX,
                include_start: true,
            };
            let expected = ref_traverse_bfs(&db, "endpoint", &opts).await;
            let actual = traverser
                .get_impact_radius("endpoint", depth)
                .await
                .unwrap();
            assert_eq!(
                actual.nodes, expected.nodes,
                "impact nodes diverged @depth {depth}"
            );
            assert_eq!(
                actual.edges, expected.edges,
                "impact edges diverged @depth {depth}"
            );
            assert_eq!(
                actual.roots, expected.roots,
                "impact roots diverged @depth {depth}"
            );
        }
        // The Contains synthesis actually fired.
        let sub = traverser.get_impact_radius("endpoint", 4).await.unwrap();
        assert!(
            sub.edges
                .iter()
                .any(|e| e.kind == EdgeKind::Contains && e.source == "S" && e.target == "m1"),
            "expected synthesized Contains S->m1"
        );
    }

    #[tokio::test]
    async fn impact_respects_limit_identical_to_reference() {
        let (db, _tmp) = new_db().await;
        build_call_fixture(&db).await;
        let traverser = GraphTraverser::new(&db);
        // A bounded limit must truncate at exactly the same node/edge as the
        // reference per-node walk.
        for limit in [1u32, 2, 3] {
            let opts = TraversalOptions {
                max_depth: 5,
                edge_kinds: Some(vec![EdgeKind::Calls]),
                node_kinds: None,
                direction: TraversalDirection::Incoming,
                limit,
                include_start: true,
            };
            let expected = ref_traverse_bfs(&db, "hub", &opts).await;
            let actual = traverser.traverse_bfs("hub", &opts).await.unwrap();
            assert_eq!(
                actual.nodes, expected.nodes,
                "limited nodes diverged @limit {limit}"
            );
            assert_eq!(
                actual.edges, expected.edges,
                "limited edges diverged @limit {limit}"
            );
        }
    }

    #[tokio::test]
    async fn call_chain_identical_to_reference() {
        let (db, _tmp) = new_db().await;
        build_call_fixture(&db).await;
        let traverser = GraphTraverser::new(&db);
        let kinds = [EdgeKind::Calls];
        for (from, to) in [("c0", "c3"), ("p", "r"), ("c0", "hub"), ("spread", "out3")] {
            let expected = ref_find_path_directed(&db, from, to, &kinds, 10).await;
            let actual = traverser
                .find_path_directed(from, to, &kinds, 10)
                .await
                .unwrap();
            let proj = |p: &Option<GraphPath>| {
                p.as_ref().map(|path| {
                    path.iter()
                        .map(|(n, e)| (n.id.clone(), e.clone()))
                        .collect::<Vec<_>>()
                })
            };
            assert_eq!(
                proj(&actual),
                proj(&expected),
                "call_chain {from}->{to} diverged from reference"
            );
        }
    }

    #[tokio::test]
    async fn wide_frontier_batches_and_matches_reference() {
        // 200 callers of one hub: the pathological wide frontier. The batched
        // walk issues 1 edge query + 1 node query per level; the reference
        // issues one pair PER caller. Assert identical output and report the
        // round-trip reduction and wall-clock delta.
        let (db, _tmp) = new_db().await;
        const FANIN: usize = 200;
        let mut nodes = vec![func("hub", 1)];
        let mut edges = Vec::new();
        for i in 0..FANIN {
            let id = format!("caller{i:03}");
            nodes.push(func(&id, 100 + i as u32));
            edges.push(edge(&id, "hub", EdgeKind::Calls));
        }
        db.insert_nodes(&nodes).await.unwrap();
        db.insert_edges(&edges).await.unwrap();
        let traverser = GraphTraverser::new(&db);

        let t0 = std::time::Instant::now();
        let expected = ref_callers(&db, "hub", 3).await;
        let ref_ms = t0.elapsed().as_secs_f64() * 1e3;

        let t1 = std::time::Instant::now();
        let actual = traverser.get_callers("hub", 3).await.unwrap();
        let new_ms = t1.elapsed().as_secs_f64() * 1e3;

        assert_eq!(
            actual, expected,
            "wide-frontier callers diverged from reference"
        );
        assert_eq!(actual.len(), FANIN);

        // Round-trips over the two BFS levels that do work (depth 0 expands the
        // hub's frontier of FANIN callers; depth 1 finds no further callers).
        let ref_round_trips = 1 + 2 * FANIN; // seed pair-per-node walk
        let new_round_trips = 2 * 2; // (edges+nodes) x 2 populated levels
        eprintln!(
            "wide fan-in ({FANIN}): reference {ref_ms:.1}ms (~{ref_round_trips} round-trips) \
             vs batched {new_ms:.1}ms (~{new_round_trips} round-trips)"
        );
    }
}
