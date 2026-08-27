//! Shortest path operator for finding paths between nodes.
//!
//! This operator computes shortest paths between source and target nodes
//! using BFS for unweighted graphs.

use super::{Operator, OperatorResult};
use crate::execution::chunk::DataChunkBuilder;
use crate::graph::Direction;
use crate::graph::GraphStoreSearch;
use grafeo_common::types::{LogicalType, NodeId, Value};
use grafeo_common::utils::hash::FxHashMap;
use std::collections::VecDeque;
use std::sync::Arc;

/// Operator that finds shortest paths between source and target nodes.
///
/// For each input row containing source and target nodes, this operator
/// computes the shortest path and outputs the path as a value.
pub struct ShortestPathOperator {
    /// The graph store.
    store: Arc<dyn GraphStoreSearch>,
    /// Input operator providing source/target node pairs.
    input: Box<dyn Operator>,
    /// Column index of the source node.
    source_column: usize,
    /// Column index of the target node.
    target_column: usize,
    /// Edge type filter (empty means all types).
    edge_types: Vec<String>,
    /// Direction of edge traversal.
    direction: Direction,
    /// Whether to find all shortest paths (vs. just one).
    all_paths: bool,
    /// Whether the operator has been exhausted.
    exhausted: bool,
}

impl ShortestPathOperator {
    /// Creates a new shortest path operator.
    pub fn new(
        store: Arc<dyn GraphStoreSearch>,
        input: Box<dyn Operator>,
        source_column: usize,
        target_column: usize,
        edge_types: Vec<String>,
        direction: Direction,
    ) -> Self {
        Self {
            store,
            input,
            source_column,
            target_column,
            edge_types,
            direction,
            all_paths: false,
            exhausted: false,
        }
    }

    /// Sets whether to find all shortest paths.
    pub fn with_all_paths(mut self, all_paths: bool) -> Self {
        self.all_paths = all_paths;
        self
    }

    /// Finds the shortest path between source and target using BFS.
    /// Returns the path length (number of edges).
    fn find_shortest_path(&self, source: NodeId, target: NodeId) -> Option<i64> {
        if source == target {
            return Some(0);
        }

        let mut visited: FxHashMap<NodeId, i64> = FxHashMap::default();
        let mut queue: VecDeque<(NodeId, i64)> = VecDeque::new();

        visited.insert(source, 0);
        queue.push_back((source, 0));

        while let Some((current, depth)) = queue.pop_front() {
            // Get neighbors based on direction
            let neighbors = self.get_neighbors(current);

            for neighbor in neighbors {
                if neighbor == target {
                    return Some(depth + 1);
                }

                if !visited.contains_key(&neighbor) {
                    visited.insert(neighbor, depth + 1);
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        None // No path found
    }

    /// Finds all shortest paths between source and target using BFS.
    /// Returns a vector of path lengths (all will be the same minimum length).
    /// For allShortestPaths, we return the count of paths with minimum length.
    fn find_all_shortest_paths(&self, source: NodeId, target: NodeId) -> Vec<i64> {
        if source == target {
            return vec![0];
        }

        // BFS that tracks number of paths to each node at each depth
        let mut distances: FxHashMap<NodeId, i64> = FxHashMap::default();
        let mut path_counts: FxHashMap<NodeId, usize> = FxHashMap::default();
        let mut queue: VecDeque<NodeId> = VecDeque::new();

        distances.insert(source, 0);
        path_counts.insert(source, 1);
        queue.push_back(source);

        let mut target_depth: Option<i64> = None;
        let mut target_path_count = 0;

        while let Some(current) = queue.pop_front() {
            let current_depth = *distances
                .get(&current)
                .expect("BFS: node dequeued has distance");
            let current_paths = *path_counts
                .get(&current)
                .expect("BFS: node dequeued has path count");

            // If we've found target and we're past its depth, stop
            if let Some(td) = target_depth
                && current_depth >= td
            {
                continue;
            }

            for neighbor in self.get_neighbors(current) {
                let new_depth = current_depth + 1;

                if neighbor == target {
                    // Found target
                    if target_depth.is_none() {
                        target_depth = Some(new_depth);
                        target_path_count = current_paths;
                    } else if Some(new_depth) == target_depth {
                        target_path_count += current_paths;
                    }
                    continue;
                }

                // If not visited or same depth (for counting all paths)
                if let Some(&existing_depth) = distances.get(&neighbor) {
                    if existing_depth == new_depth {
                        // Same depth, add to path count
                        *path_counts
                            .get_mut(&neighbor)
                            .expect("BFS: neighbor has path count at same depth") += current_paths;
                    }
                    // If existing_depth < new_depth, skip (already processed at shorter distance)
                } else {
                    // New node
                    distances.insert(neighbor, new_depth);
                    path_counts.insert(neighbor, current_paths);
                    queue.push_back(neighbor);
                }
            }
        }

        // Return one entry per path
        if let Some(depth) = target_depth {
            vec![depth; target_path_count]
        } else {
            vec![]
        }
    }

    /// Gets neighbors of a node in a specific direction, respecting edge type filter.
    ///
    /// This is the direction-parameterized variant used by bidirectional BFS
    /// to traverse the forward and backward frontiers independently.
    fn get_neighbors_directed(&self, node: NodeId, direction: Direction) -> Vec<NodeId> {
        self.store
            .edges_from(node, direction)
            .into_iter()
            .filter(|(_target, edge_id)| {
                if self.edge_types.is_empty() {
                    true
                } else if let Some(actual_type) = self.store.edge_type(*edge_id) {
                    self.edge_types
                        .iter()
                        .any(|t| actual_type.as_str().eq_ignore_ascii_case(t.as_str()))
                } else {
                    false
                }
            })
            .map(|(target, _)| target)
            .collect()
    }

    /// Gets neighbors of a node respecting edge type filter and direction.
    fn get_neighbors(&self, node: NodeId) -> Vec<NodeId> {
        self.get_neighbors_directed(node, self.direction)
    }

    /// Finds shortest path using bidirectional BFS.
    ///
    /// Maintains forward and backward frontiers, alternating expansion of the
    /// smaller one. When a node is found in both visited sets, the shortest
    /// path is `forward_depth + backward_depth`. This reduces the search space
    /// from O(b^d) to O(b^(d/2)) where b is the branching factor and d is
    /// the path length.
    ///
    /// Falls back to unidirectional BFS if backward adjacency is unavailable.
    fn find_shortest_path_bidirectional(&self, source: NodeId, target: NodeId) -> Option<i64> {
        if source == target {
            return Some(0);
        }

        // Fall back to unidirectional if backward adjacency is unavailable
        if !self.store.has_backward_adjacency() {
            return self.find_shortest_path(source, target);
        }

        let reverse_dir = self.direction.reverse();

        // Forward BFS state
        let mut forward_visited: FxHashMap<NodeId, i64> = FxHashMap::default();
        let mut forward_queue: VecDeque<(NodeId, i64)> = VecDeque::new();
        forward_visited.insert(source, 0);
        forward_queue.push_back((source, 0));

        // Backward BFS state
        let mut backward_visited: FxHashMap<NodeId, i64> = FxHashMap::default();
        let mut backward_queue: VecDeque<(NodeId, i64)> = VecDeque::new();
        backward_visited.insert(target, 0);
        backward_queue.push_back((target, 0));

        // Best known path length (upper bound)
        let mut best: Option<i64> = None;

        loop {
            // Decide which frontier to expand, or stop
            let expand_forward = match (forward_queue.front(), backward_queue.front()) {
                (Some(_), Some(_)) => forward_queue.len() <= backward_queue.len(),
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };

            if expand_forward {
                let Some((current, depth)) = forward_queue.pop_front() else {
                    break;
                };

                // If this depth alone exceeds best, this frontier is exhausted
                if let Some(b) = best
                    && depth + 1 > b
                {
                    // Clear the queue; no further expansion can improve
                    forward_queue.clear();
                    continue;
                }

                for neighbor in self.get_neighbors_directed(current, self.direction) {
                    let new_depth = depth + 1;

                    // Check if backward frontier already visited this node
                    if let Some(&backward_depth) = backward_visited.get(&neighbor) {
                        let total = new_depth + backward_depth;
                        best = Some(best.map_or(total, |b: i64| b.min(total)));
                    }

                    if !forward_visited.contains_key(&neighbor) {
                        forward_visited.insert(neighbor, new_depth);
                        if best.is_none_or(|b| new_depth < b) {
                            forward_queue.push_back((neighbor, new_depth));
                        }
                    }
                }
            } else {
                let Some((current, depth)) = backward_queue.pop_front() else {
                    break;
                };

                if let Some(b) = best
                    && depth + 1 > b
                {
                    backward_queue.clear();
                    continue;
                }

                for neighbor in self.get_neighbors_directed(current, reverse_dir) {
                    let new_depth = depth + 1;

                    // Check if forward frontier already visited this node
                    if let Some(&forward_depth) = forward_visited.get(&neighbor) {
                        let total = forward_depth + new_depth;
                        best = Some(best.map_or(total, |b: i64| b.min(total)));
                    }

                    if !backward_visited.contains_key(&neighbor) {
                        backward_visited.insert(neighbor, new_depth);
                        if best.is_none_or(|b| new_depth < b) {
                            backward_queue.push_back((neighbor, new_depth));
                        }
                    }
                }
            }
        }

        best
    }
}

impl Operator for ShortestPathOperator {
    fn next(&mut self) -> OperatorResult {
        if self.exhausted {
            return Ok(None);
        }

        // Get input chunk
        let Some(input_chunk) = self.input.next()? else {
            self.exhausted = true;
            return Ok(None);
        };

        // Build output: input columns + path length
        let num_input_cols = input_chunk.column_count();
        let mut output_schema: Vec<LogicalType> = (0..num_input_cols)
            .map(|i| {
                input_chunk
                    .column(i)
                    .map_or(LogicalType::Any, |c| c.data_type().clone())
            })
            .collect();
        output_schema.push(LogicalType::Any); // Path column (stores length as int)

        // For allShortestPaths, we may need more rows than input
        let initial_capacity = if self.all_paths {
            input_chunk.row_count() * 4 // Estimate 4x for multiple paths
        } else {
            input_chunk.row_count()
        };
        let mut builder = DataChunkBuilder::with_capacity(&output_schema, initial_capacity);

        for row in input_chunk.selected_indices() {
            // Get source and target nodes
            let source = input_chunk
                .column(self.source_column)
                .and_then(|c| c.get_node_id(row));
            let target = input_chunk
                .column(self.target_column)
                .and_then(|c| c.get_node_id(row));

            // Compute shortest path(s)
            let path_lengths: Vec<Option<i64>> = match (source, target) {
                (Some(s), Some(t)) => {
                    if self.all_paths {
                        let paths = self.find_all_shortest_paths(s, t);
                        if paths.is_empty() {
                            vec![None] // No path found, still output one row with null
                        } else {
                            paths.into_iter().map(Some).collect()
                        }
                    } else {
                        // Use bidirectional BFS when possible (single shortest path)
                        vec![self.find_shortest_path_bidirectional(s, t)]
                    }
                }
                _ => vec![None],
            };

            // Output one row per path
            for path_length in path_lengths {
                // Copy input columns
                for col_idx in 0..num_input_cols {
                    if let Some(in_col) = input_chunk.column(col_idx)
                        && let Some(out_col) = builder.column_mut(col_idx)
                    {
                        if let Some(node_id) = in_col.get_node_id(row) {
                            out_col.push_node_id(node_id);
                        } else if let Some(edge_id) = in_col.get_edge_id(row) {
                            out_col.push_edge_id(edge_id);
                        } else if let Some(value) = in_col.get_value(row) {
                            out_col.push_value(value);
                        } else {
                            out_col.push_value(Value::Null);
                        }
                    }
                }

                // Add path length column
                if let Some(out_col) = builder.column_mut(num_input_cols) {
                    match path_length {
                        Some(len) => out_col.push_value(Value::Int64(len)),
                        None => out_col.push_value(Value::Null),
                    }
                }

                builder.advance_row();
            }
        }

        let chunk = builder.finish();
        if chunk.row_count() > 0 {
            Ok(Some(chunk))
        } else {
            self.exhausted = true;
            Ok(None)
        }
    }

    fn reset(&mut self) {
        self.input.reset();
        self.exhausted = false;
    }

    fn name(&self) -> &'static str {
        "ShortestPath"
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any + Send> {
        self
    }
}

#[cfg(all(test, feature = "lpg"))]
mod tests {
    use super::*;
    use crate::graph::lpg::LpgStore;

    /// A mock operator that returns a single chunk with source/target node pairs.
    struct MockPairOperator {
        pairs: Vec<(NodeId, NodeId)>,
        exhausted: bool,
    }

    impl MockPairOperator {
        fn new(pairs: Vec<(NodeId, NodeId)>) -> Self {
            Self {
                pairs,
                exhausted: false,
            }
        }
    }

    impl Operator for MockPairOperator {
        fn next(&mut self) -> OperatorResult {
            if self.exhausted || self.pairs.is_empty() {
                return Ok(None);
            }
            self.exhausted = true;

            let schema = vec![LogicalType::Node, LogicalType::Node];
            let mut builder = DataChunkBuilder::with_capacity(&schema, self.pairs.len());

            for (source, target) in &self.pairs {
                builder.column_mut(0).unwrap().push_node_id(*source);
                builder.column_mut(1).unwrap().push_node_id(*target);
                builder.advance_row();
            }

            Ok(Some(builder.finish()))
        }

        fn reset(&mut self) {
            self.exhausted = false;
        }

        fn name(&self) -> &'static str {
            "MockPair"
        }

        fn into_any(self: Box<Self>) -> Box<dyn std::any::Any + Send> {
            self
        }
    }

    #[test]
    fn test_find_shortest_path_direct() {
        let store = Arc::new(LpgStore::new().unwrap());

        // a -> b (1 hop)
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        store.create_edge(a, b, "KNOWS");

        let input = Box::new(MockPairOperator::new(vec![(a, b)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0, // source column
            1, // target column
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        assert_eq!(chunk.row_count(), 1);

        // Path length should be 1
        let path_col = chunk.column(2).unwrap();
        let path_len = path_col.get_value(0).unwrap();
        assert_eq!(path_len, Value::Int64(1));
    }

    #[test]
    fn test_find_shortest_path_same_node() {
        let store = Arc::new(LpgStore::new().unwrap());
        let a = store.create_node(&["Node"]);

        let input = Box::new(MockPairOperator::new(vec![(a, a)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        assert_eq!(chunk.row_count(), 1);

        // Path length should be 0 (same node)
        let path_col = chunk.column(2).unwrap();
        let path_len = path_col.get_value(0).unwrap();
        assert_eq!(path_len, Value::Int64(0));
    }

    #[test]
    fn test_find_shortest_path_two_hops() {
        let store = Arc::new(LpgStore::new().unwrap());

        // a -> b -> c (2 hops from a to c)
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        let c = store.create_node(&["Node"]);
        store.create_edge(a, b, "KNOWS");
        store.create_edge(b, c, "KNOWS");

        let input = Box::new(MockPairOperator::new(vec![(a, c)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        assert_eq!(chunk.row_count(), 1);

        let path_col = chunk.column(2).unwrap();
        let path_len = path_col.get_value(0).unwrap();
        assert_eq!(path_len, Value::Int64(2));
    }

    #[test]
    fn test_find_shortest_path_no_path() {
        let store = Arc::new(LpgStore::new().unwrap());

        // a and b are disconnected
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);

        let input = Box::new(MockPairOperator::new(vec![(a, b)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        assert_eq!(chunk.row_count(), 1);

        // Path length should be null (no path)
        let path_col = chunk.column(2).unwrap();
        let path_len = path_col.get_value(0).unwrap();
        assert_eq!(path_len, Value::Null);
    }

    #[test]
    fn test_find_shortest_path_prefers_shorter() {
        let store = Arc::new(LpgStore::new().unwrap());

        // Create two paths: a -> d (1 hop) and a -> b -> c -> d (3 hops)
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        let c = store.create_node(&["Node"]);
        let d = store.create_node(&["Node"]);

        // Long path
        store.create_edge(a, b, "KNOWS");
        store.create_edge(b, c, "KNOWS");
        store.create_edge(c, d, "KNOWS");

        // Short path (direct)
        store.create_edge(a, d, "KNOWS");

        let input = Box::new(MockPairOperator::new(vec![(a, d)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        let path_col = chunk.column(2).unwrap();
        let path_len = path_col.get_value(0).unwrap();
        assert_eq!(path_len, Value::Int64(1)); // Should find direct path
    }

    #[test]
    fn test_find_shortest_path_with_edge_type_filter() {
        let store = Arc::new(LpgStore::new().unwrap());

        // a -KNOWS-> b -LIKES-> c
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        let c = store.create_node(&["Node"]);
        store.create_edge(a, b, "KNOWS");
        store.create_edge(b, c, "LIKES");

        // Path with KNOWS filter should only reach b, not c
        let input = Box::new(MockPairOperator::new(vec![(a, c)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec!["KNOWS".to_string()],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        let path_col = chunk.column(2).unwrap();
        let path_len = path_col.get_value(0).unwrap();
        assert_eq!(path_len, Value::Null); // Can't reach c via KNOWS only
    }

    #[test]
    fn test_all_shortest_paths_single_path() {
        let store = Arc::new(LpgStore::new().unwrap());

        // a -> b (single path)
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        store.create_edge(a, b, "KNOWS");

        let input = Box::new(MockPairOperator::new(vec![(a, b)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        )
        .with_all_paths(true);

        let chunk = op.next().unwrap().unwrap();
        assert_eq!(chunk.row_count(), 1); // Only one path exists
    }

    #[test]
    fn test_all_shortest_paths_multiple_paths() {
        let store = Arc::new(LpgStore::new().unwrap());

        // Create diamond: a -> b -> d and a -> c -> d (two paths of length 2)
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        let c = store.create_node(&["Node"]);
        let d = store.create_node(&["Node"]);

        store.create_edge(a, b, "KNOWS");
        store.create_edge(a, c, "KNOWS");
        store.create_edge(b, d, "KNOWS");
        store.create_edge(c, d, "KNOWS");

        let input = Box::new(MockPairOperator::new(vec![(a, d)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        )
        .with_all_paths(true);

        let chunk = op.next().unwrap().unwrap();
        // Should return 2 rows (two paths of length 2)
        assert_eq!(chunk.row_count(), 2);

        // Both should have length 2
        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Int64(2));
        assert_eq!(path_col.get_value(1).unwrap(), Value::Int64(2));
    }

    #[test]
    fn test_multiple_pairs_in_chunk() {
        let store = Arc::new(LpgStore::new().unwrap());

        // Create: a -> b, c -> d
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        let c = store.create_node(&["Node"]);
        let d = store.create_node(&["Node"]);

        store.create_edge(a, b, "KNOWS");
        store.create_edge(c, d, "KNOWS");

        // Test multiple pairs at once
        let input = Box::new(MockPairOperator::new(vec![(a, b), (c, d), (a, d)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        assert_eq!(chunk.row_count(), 3);

        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Int64(1)); // a->b = 1
        assert_eq!(path_col.get_value(1).unwrap(), Value::Int64(1)); // c->d = 1
        assert_eq!(path_col.get_value(2).unwrap(), Value::Null); // a->d = no path
    }

    #[test]
    fn test_operator_reset() {
        let store = Arc::new(LpgStore::new().unwrap());
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        store.create_edge(a, b, "KNOWS");

        let input = Box::new(MockPairOperator::new(vec![(a, b)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        // First iteration
        let chunk = op.next().unwrap();
        assert!(chunk.is_some());
        let chunk = op.next().unwrap();
        assert!(chunk.is_none());

        // After reset
        op.reset();
        let chunk = op.next().unwrap();
        assert!(chunk.is_some());
    }

    #[test]
    fn test_operator_name() {
        let store = Arc::new(LpgStore::new().unwrap());
        let input = Box::new(MockPairOperator::new(vec![]));
        let op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        assert_eq!(op.name(), "ShortestPath");
    }

    #[test]
    fn test_empty_input() {
        let store = Arc::new(LpgStore::new().unwrap());
        let input = Box::new(MockPairOperator::new(vec![]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        // Empty input should return None
        let chunk = op.next().unwrap();
        assert!(chunk.is_none());
    }

    #[test]
    fn test_all_shortest_paths_no_path() {
        let store = Arc::new(LpgStore::new().unwrap());

        // Disconnected nodes
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);

        let input = Box::new(MockPairOperator::new(vec![(a, b)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        )
        .with_all_paths(true);

        let chunk = op.next().unwrap().unwrap();
        assert_eq!(chunk.row_count(), 1); // Still returns one row with null

        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Null);
    }

    #[test]
    fn test_all_shortest_paths_same_node() {
        let store = Arc::new(LpgStore::new().unwrap());
        let a = store.create_node(&["Node"]);

        let input = Box::new(MockPairOperator::new(vec![(a, a)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        )
        .with_all_paths(true);

        let chunk = op.next().unwrap().unwrap();
        assert_eq!(chunk.row_count(), 1);

        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Int64(0));
    }

    // === Bidirectional BFS Tests ===

    #[test]
    fn test_bidirectional_bfs_long_chain() {
        let store = Arc::new(LpgStore::new().unwrap());

        // Chain: n0 -> n1 -> n2 -> ... -> n9 (9 hops)
        let nodes: Vec<NodeId> = (0..10).map(|_| store.create_node(&["Node"])).collect();
        for i in 0..9 {
            store.create_edge(nodes[i], nodes[i + 1], "NEXT");
        }

        let input = Box::new(MockPairOperator::new(vec![(nodes[0], nodes[9])]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Int64(9));
    }

    #[test]
    fn test_bidirectional_bfs_diamond() {
        let store = Arc::new(LpgStore::new().unwrap());

        // Diamond: a -> b -> d, a -> c -> d (two paths of length 2)
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        let c = store.create_node(&["Node"]);
        let d = store.create_node(&["Node"]);

        store.create_edge(a, b, "KNOWS");
        store.create_edge(a, c, "KNOWS");
        store.create_edge(b, d, "KNOWS");
        store.create_edge(c, d, "KNOWS");

        let input = Box::new(MockPairOperator::new(vec![(a, d)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Int64(2));
    }

    #[test]
    fn test_bidirectional_bfs_no_path() {
        let store = Arc::new(LpgStore::new().unwrap());

        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        // No edges between a and b

        let input = Box::new(MockPairOperator::new(vec![(a, b)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Null);
    }

    #[test]
    fn test_bidirectional_bfs_same_node() {
        let store = Arc::new(LpgStore::new().unwrap());
        let a = store.create_node(&["Node"]);

        let input = Box::new(MockPairOperator::new(vec![(a, a)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Int64(0));
    }

    #[test]
    fn test_bidirectional_bfs_prefers_shorter() {
        let store = Arc::new(LpgStore::new().unwrap());

        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        let c = store.create_node(&["Node"]);
        let d = store.create_node(&["Node"]);

        // Long path: a -> b -> c -> d (3 hops)
        store.create_edge(a, b, "KNOWS");
        store.create_edge(b, c, "KNOWS");
        store.create_edge(c, d, "KNOWS");
        // Short path: a -> d (1 hop)
        store.create_edge(a, d, "KNOWS");

        let input = Box::new(MockPairOperator::new(vec![(a, d)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Int64(1));
    }

    #[test]
    fn test_bidirectional_bfs_with_edge_type_filter() {
        let store = Arc::new(LpgStore::new().unwrap());

        // a -KNOWS-> b -LIKES-> c
        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        let c = store.create_node(&["Node"]);
        store.create_edge(a, b, "KNOWS");
        store.create_edge(b, c, "LIKES");

        // Only KNOWS edges: a can reach b but not c
        let input = Box::new(MockPairOperator::new(vec![(a, c)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec!["KNOWS".to_string()],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Null);
    }

    #[test]
    fn test_bidirectional_bfs_has_backward_adjacency() {
        // Default store has backward adjacency enabled
        let store = Arc::new(LpgStore::new().unwrap());
        assert!(store.has_backward_adjacency());

        let a = store.create_node(&["Node"]);
        let b = store.create_node(&["Node"]);
        let c = store.create_node(&["Node"]);
        store.create_edge(a, b, "KNOWS");
        store.create_edge(b, c, "KNOWS");

        // Bidirectional BFS should work and find shortest path
        let input = Box::new(MockPairOperator::new(vec![(a, c)]));
        let mut op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );

        let chunk = op.next().unwrap().unwrap();
        let path_col = chunk.column(2).unwrap();
        assert_eq!(path_col.get_value(0).unwrap(), Value::Int64(2));
    }

    #[test]
    fn test_shortest_path_into_any() {
        let store = Arc::new(LpgStore::new().unwrap());
        let input = Box::new(MockPairOperator::new(vec![]));
        let op = ShortestPathOperator::new(
            store.clone() as Arc<dyn GraphStoreSearch>,
            input,
            0,
            1,
            vec![],
            Direction::Outgoing,
        );
        let any = Box::new(op).into_any();
        assert!(any.downcast::<ShortestPathOperator>().is_ok());
    }
}
