//! Structural health analysis algorithms.
//!
//! Provides file-level DAG construction, Gini coefficient computation,
//! Tarjan's SCC-based acyclicity scoring, dependency depth analysis,
//! modularity estimation, and composite health scoring.

pub mod delta;
pub mod snapshot;
pub mod test_risk;

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasher;

use super::scc::tarjan_scc;

// The four structural-health value types are the shared authority in
// `tracedecay-usecases`; re-export them so root callers keep a single type
// identity. The `delta`, `snapshot`, and `test_risk` submodules and the
// algorithm functions below have no usecases counterpart and remain root-owned.
pub use tracedecay_usecases::graph::health::{
    DepthChain, DepthResult, DsmCluster, HealthDimensions,
};

// ---------------------------------------------------------------------------
// Task 2: Gini Coefficient
// ---------------------------------------------------------------------------

/// Computes the Gini coefficient for a slice of non-negative values.
/// Returns 0.0 for empty slices, single-element slices, or all-zero slices.
/// Result is in \[0.0, 1.0\] where 0.0 = perfect equality.
pub fn gini_coefficient(values: &[f64]) -> f64 {
    if values.len() <= 1 {
        return 0.0;
    }

    let sum: f64 = values.iter().sum();
    if sum == 0.0 {
        return 0.0;
    }

    let n = values.len() as f64;
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // G = (2 * Σ(i * x_i)) / (n * Σ(x_i)) - (n + 1) / n  (i is 1-indexed)
    let weighted_sum: f64 = sorted
        .iter()
        .enumerate()
        .map(|(idx, &x)| (idx as f64 + 1.0) * x)
        .sum();

    (2.0 * weighted_sum) / (n * sum) - (n + 1.0) / n
}

/// Returns a human-readable label for a Gini coefficient value.
/// - <0.20  → "low inequality (healthy)"
/// - <0.40  → "moderate inequality"
/// - <0.60  → "high inequality"
/// - >=0.60 → "extreme inequality (god files likely)"
pub fn gini_label(gini: f64) -> &'static str {
    if gini < 0.20 {
        "low inequality (healthy)"
    } else if gini < 0.40 {
        "moderate inequality"
    } else if gini < 0.60 {
        "high inequality"
    } else {
        "extreme inequality (god files likely)"
    }
}

// ---------------------------------------------------------------------------
// Task 3: Tarjan's SCC / Acyclicity Score
// ---------------------------------------------------------------------------

/// Computes the acyclicity score for a directed graph.
/// Uses Tarjan's SCC algorithm. Score = 1.0 - (`edges_in_nontrivial_SCCs` / `total_edges`).
/// Returns (score, `number_of_edges_in_cycles`).
pub fn acyclicity_score<S1: BuildHasher, S2: BuildHasher>(
    adj: &HashMap<String, HashSet<String, S2>, S1>,
) -> (f64, usize) {
    let total_edges: usize = adj.values().map(HashSet::len).sum();

    if total_edges == 0 {
        return (1.0, 0);
    }

    let sccs = tarjan_scc(adj);

    // Build a set of nodes in nontrivial SCCs (size > 1)
    let mut in_cycle: HashSet<&str> = HashSet::new();
    for scc in &sccs {
        if scc.len() > 1 {
            for node in scc {
                in_cycle.insert(node.as_str());
            }
        }
    }

    // Count edges where both endpoints are in nontrivial SCCs
    let edges_in_cycles: usize = adj
        .iter()
        .filter(|(src, _)| in_cycle.contains(src.as_str()))
        .map(|(_src, targets)| {
            targets
                .iter()
                .filter(|tgt| in_cycle.contains(tgt.as_str()))
                .count()
        })
        .sum();

    let score = 1.0 - (edges_in_cycles as f64 / total_edges as f64);
    (score, edges_in_cycles)
}

// ---------------------------------------------------------------------------
// Task 4: Dependency Depth
// ---------------------------------------------------------------------------

/// Computes longest dependency chains. Breaks cycles via Tarjan's SCC
/// (collapses each SCC to a single node), then runs topo sort + DP.
pub fn dependency_depth<S1: BuildHasher, S2: BuildHasher>(
    adj: &HashMap<String, HashSet<String, S2>, S1>,
    limit: usize,
) -> DepthResult {
    // Collect all nodes
    let mut all_nodes: HashSet<String> = adj.keys().cloned().collect();
    for targets in adj.values() {
        all_nodes.extend(targets.iter().cloned());
    }
    let file_count = all_nodes.len();

    if file_count == 0 {
        return DepthResult {
            max_depth: 0,
            ideal_depth: 0,
            chains: Vec::new(),
        };
    }

    let ideal_depth = if file_count <= 1 {
        0
    } else {
        (file_count as f64).log2().ceil() as usize
    };

    // Step 1: Run Tarjan's SCC, map each node to its SCC index
    let sccs = tarjan_scc(adj);
    let mut node_to_scc: HashMap<String, usize> = HashMap::new();
    for (idx, scc) in sccs.iter().enumerate() {
        for node in scc {
            node_to_scc.insert(node.clone(), idx);
        }
    }

    // Step 2: Build DAG over SCC indices
    let scc_count = sccs.len();
    let mut scc_adj: HashMap<usize, HashSet<usize>> = HashMap::new();
    for (src, targets) in adj {
        let src_scc = node_to_scc[src];
        for tgt in targets {
            let tgt_scc = node_to_scc[tgt];
            if src_scc != tgt_scc {
                scc_adj.entry(src_scc).or_default().insert(tgt_scc);
            }
        }
    }

    // Step 3: Kahn's algorithm for topological sort
    let mut in_degree = vec![0usize; scc_count];
    for targets in scc_adj.values() {
        for &tgt in targets {
            in_degree[tgt] += 1;
        }
    }

    let mut queue: VecDeque<usize> = (0..scc_count).filter(|&i| in_degree[i] == 0).collect();

    let mut topo_order: Vec<usize> = Vec::new();
    while let Some(node) = queue.pop_front() {
        topo_order.push(node);
        if let Some(neighbors) = scc_adj.get(&node) {
            for &nb in neighbors {
                in_degree[nb] -= 1;
                if in_degree[nb] == 0 {
                    queue.push_back(nb);
                }
            }
        }
    }

    // Step 4: DP for longest path with predecessor tracking
    let mut dist = vec![0usize; scc_count];
    let mut pred = vec![usize::MAX; scc_count];

    for &u in &topo_order {
        if let Some(neighbors) = scc_adj.get(&u) {
            for &v in neighbors {
                if dist[u] + 1 > dist[v] {
                    dist[v] = dist[u] + 1;
                    pred[v] = u;
                }
            }
        }
    }

    // Step 5: Reconstruct chains (use first node of each SCC as representative)
    let mut max_depth = 0;
    let mut results: Vec<DepthChain> = Vec::new();

    for scc_idx in 0..scc_count {
        let depth = dist[scc_idx];
        if depth > max_depth {
            max_depth = depth;
        }

        if results.len() < limit {
            // Reconstruct the chain by walking predecessors
            let mut chain_sccs: Vec<usize> = Vec::new();
            let mut cur = scc_idx;
            loop {
                chain_sccs.push(cur);
                let p = pred[cur];
                if p == usize::MAX {
                    break;
                }
                cur = p;
            }
            chain_sccs.reverse();

            // Map SCC indices back to representative file names
            let chain: Vec<String> = chain_sccs.iter().map(|&si| sccs[si][0].clone()).collect();

            let mut scc_files = sccs[scc_idx].clone();
            scc_files.sort();
            let representative = scc_files[0].clone();
            results.push(DepthChain {
                file: representative,
                scc_files,
                depth,
                chain,
            });
        }
    }

    // Sort by depth descending for convenience
    results.sort_by_key(|ch| std::cmp::Reverse(ch.depth));

    DepthResult {
        max_depth,
        ideal_depth,
        chains: results,
    }
}

/// Groups the file adjacency by parent directory and orders clusters by
/// cross-boundary coupling, then cluster size. This is the shared authority for
/// both the MCP DSM tool and dashboard graph strata.
pub fn dsm_clusters<AdjHasher, EdgeHasher>(
    adj: &HashMap<String, HashSet<String, EdgeHasher>, AdjHasher>,
) -> Vec<DsmCluster>
where
    AdjHasher: BuildHasher,
    EdgeHasher: BuildHasher,
{
    let mut dir_to_files: HashMap<String, Vec<String>> = HashMap::new();
    for file in adj.keys() {
        let directory = file
            .rfind('/')
            .map_or_else(|| ".".to_string(), |index| file[..index].to_string());
        dir_to_files
            .entry(directory)
            .or_default()
            .push(file.clone());
    }

    let mut clusters: Vec<DsmCluster> = dir_to_files
        .into_iter()
        .map(|(directory, files)| {
            let file_set: HashSet<&str> = files.iter().map(String::as_str).collect();
            let mut internal_edges = 0;
            let mut outgoing_edges = 0;
            let mut incoming_edges = 0;
            for file in &files {
                if let Some(targets) = adj.get(file) {
                    for target in targets {
                        if file_set.contains(target.as_str()) {
                            internal_edges += 1;
                        } else {
                            outgoing_edges += 1;
                        }
                    }
                }
                for (source, targets) in adj {
                    if !file_set.contains(source.as_str()) && targets.contains(file) {
                        incoming_edges += 1;
                    }
                }
            }
            DsmCluster {
                directory,
                file_count: files.len(),
                internal_edges,
                outgoing_edges,
                incoming_edges,
            }
        })
        .collect();
    clusters.sort_by(|left, right| {
        right
            .boundary_edges()
            .cmp(&left.boundary_edges())
            .then_with(|| right.file_count.cmp(&left.file_count))
            .then_with(|| left.directory.cmp(&right.directory))
    });
    clusters
}

/// Score = min(1.0, ideal\_depth / max\_depth). Shallower is better.
/// Returns 1.0 when `max_depth == 0`.
pub fn depth_score(max_depth: usize, ideal_depth: usize) -> f64 {
    if max_depth == 0 {
        return 1.0;
    }
    (ideal_depth as f64 / max_depth as f64).min(1.0)
}

// ---------------------------------------------------------------------------
// Task 5: Modularity Score
// ---------------------------------------------------------------------------

/// Estimates modularity by removing hub nodes and counting connected components.
/// Hub nodes = files with (fan\_in + fan\_out) > mean + 2\*stddev.
/// Score = 1.0 - (1.0 / component\_count), clamped to \[0, 1\].
/// Returns (score, component\_count\_after\_hub\_removal).
pub fn modularity_score<S1: BuildHasher, S2: BuildHasher>(
    adj: &HashMap<String, HashSet<String, S2>, S1>,
) -> (f64, usize) {
    if adj.is_empty() {
        return (1.0, 0);
    }

    // Collect all nodes
    let mut all_nodes: HashSet<String> = adj.keys().cloned().collect();
    for targets in adj.values() {
        all_nodes.extend(targets.iter().cloned());
    }

    if all_nodes.is_empty() {
        return (1.0, 0);
    }

    // Build undirected connectivity count per node (fan_in + fan_out)
    let mut connectivity: HashMap<&str, usize> = HashMap::new();
    for node in &all_nodes {
        connectivity.insert(node.as_str(), 0);
    }
    for (src, targets) in adj {
        *connectivity.entry(src.as_str()).or_insert(0) += targets.len();
        for tgt in targets {
            *connectivity.entry(tgt.as_str()).or_insert(0) += 1;
        }
    }

    // Compute mean and stddev
    let n = connectivity.len() as f64;
    let values: Vec<f64> = connectivity.values().map(|&v| v as f64).collect();
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / n;
    let stddev = variance.sqrt();
    let threshold = mean + 2.0 * stddev;

    // Identify hub nodes
    let hubs: HashSet<&str> = connectivity
        .iter()
        .filter(|&(_, &v)| v as f64 > threshold)
        .map(|(&k, _)| k)
        .collect();

    // Build undirected graph without hubs
    let non_hub_nodes: Vec<&str> = all_nodes
        .iter()
        .map(String::as_str)
        .filter(|n| !hubs.contains(n))
        .collect();

    if non_hub_nodes.is_empty() {
        return (1.0, 0);
    }

    let mut undirected: HashMap<&str, HashSet<&str>> = HashMap::new();
    for &node in &non_hub_nodes {
        undirected.entry(node).or_default();
    }
    for (src, targets) in adj {
        if hubs.contains(src.as_str()) {
            continue;
        }
        for tgt in targets {
            if hubs.contains(tgt.as_str()) {
                continue;
            }
            undirected
                .entry(src.as_str())
                .or_default()
                .insert(tgt.as_str());
            undirected
                .entry(tgt.as_str())
                .or_default()
                .insert(src.as_str());
        }
    }

    // Count connected components via BFS
    let mut visited: HashSet<&str> = HashSet::new();
    let mut components = 0;

    for &start in &non_hub_nodes {
        if visited.contains(start) {
            continue;
        }
        components += 1;
        let mut queue = VecDeque::new();
        queue.push_back(start);
        visited.insert(start);
        while let Some(curr) = queue.pop_front() {
            if let Some(neighbors) = undirected.get(curr) {
                for &nb in neighbors {
                    if !visited.contains(nb) {
                        visited.insert(nb);
                        queue.push_back(nb);
                    }
                }
            }
        }
    }

    let score = (1.0 - 1.0 / components as f64).clamp(0.0, 1.0);
    (score, components)
}

// ---------------------------------------------------------------------------
// Task 6: Composite Health Score
// ---------------------------------------------------------------------------

/// Computes quality signal (0–10000) from geometric mean of all five dimensions.
/// Formula: `(product of all 5).powf(1.0/5.0) * 10000.0`, rounded.
/// Zero in any dimension → 0.
/// A low-weight multiplicative penalty for `coverage_discipline` reduces
/// the score by up to 10% when skip-test-coverage is overused.
pub fn compute_composite_health(dims: &HealthDimensions) -> u32 {
    let product = dims.acyclicity * dims.depth * dims.equality * dims.redundancy * dims.modularity;

    if product <= 0.0 {
        return 0;
    }

    let base = (product.powf(1.0 / 5.0) * 10_000.0).round();
    // Low-weight penalty: skip-test-coverage overuse reduces score by up to 2%.
    let penalized = base * (0.98 + 0.02 * dims.coverage_discipline);
    penalized.round() as u32
}
