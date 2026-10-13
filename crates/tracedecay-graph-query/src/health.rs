//! Structural health analysis algorithms.
//!
//! Provides file-level DAG construction, Gini coefficient computation,
//! Tarjan's SCC-based acyclicity scoring, dependency depth analysis,
//! modularity estimation, and composite health scoring.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasher;

use super::queries::{GraphQueryManager, VerifiedHealthInputsV1};
use super::scc::tarjan_scc;
use tracedecay_domain::errors::Result;

#[derive(Clone, Debug)]
pub struct VerifiedHealthSnapshotV1 {
    pub quality_signal: u32,
    pub files_analyzed: usize,
    pub acyclicity: f64,
    pub depth: f64,
    pub equality: f64,
    pub redundancy: f64,
    pub modularity: f64,
    pub coverage_discipline: f64,
    pub gini: f64,
    pub edges_in_cycles: usize,
    pub total_edges: usize,
    pub max_chain: usize,
    pub ideal_chain: usize,
    pub complexity_files: usize,
    /// Symbols whose complexity counters were left out of the per-file
    /// complexity because their bounded walk did not cover the body.
    pub incomplete_complexity_symbols: usize,
    pub modularity_components: usize,
    pub dead_count: usize,
    pub total_fns: usize,
    pub skip_coverage_count: usize,
}

impl VerifiedHealthSnapshotV1 {
    /// Why `dimension` measured nothing in this scope, or `None` when its
    /// score is a measurement.
    pub fn no_evidence(&self, dimension: HealthDimension) -> Option<NoEvidence> {
        dimension.no_evidence(dimension.denominator(self), self.files_analyzed as u64)
    }
}

/// One scored dimension of the health panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthDimension {
    Acyclicity,
    CoverageDiscipline,
    Depth,
    Equality,
    Modularity,
    Redundancy,
}

impl HealthDimension {
    pub const ALL: [Self; 6] = [
        Self::Acyclicity,
        Self::CoverageDiscipline,
        Self::Depth,
        Self::Equality,
        Self::Modularity,
        Self::Redundancy,
    ];

    /// The dimension's key in the panel, the report, and persisted
    /// health-delta points.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Acyclicity => "acyclicity",
            Self::CoverageDiscipline => "coverage_discipline",
            Self::Depth => "depth",
            Self::Equality => "equality",
            Self::Modularity => "modularity",
            Self::Redundancy => "redundancy",
        }
    }

    pub const fn score(self, snapshot: &VerifiedHealthSnapshotV1) -> f64 {
        match self {
            Self::Acyclicity => snapshot.acyclicity,
            Self::CoverageDiscipline => snapshot.coverage_discipline,
            Self::Depth => snapshot.depth,
            Self::Equality => snapshot.equality,
            Self::Modularity => snapshot.modularity,
            Self::Redundancy => snapshot.redundancy,
        }
    }

    /// The count the score is measured over, as health-delta points persist
    /// it; `None` when that count is zero.
    pub fn denominator(self, snapshot: &VerifiedHealthSnapshotV1) -> Option<u64> {
        let count = match self {
            Self::Acyclicity => snapshot.total_edges,
            Self::Depth => snapshot.max_chain,
            Self::Equality => snapshot.complexity_files,
            Self::Modularity => snapshot.modularity_components,
            Self::Redundancy | Self::CoverageDiscipline => snapshot.total_fns,
        } as u64;
        (count > 0).then_some(count)
    }

    /// The one evidence rule. It reads only values a persisted health-delta
    /// point carries, so a stored point and a live snapshot are judged alike.
    pub fn no_evidence(self, denominator: Option<u64>, files_analyzed: u64) -> Option<NoEvidence> {
        if files_analyzed == 0 {
            return Some(NoEvidence::NoFiles);
        }
        match self {
            Self::Acyclicity => denominator
                .is_none()
                .then_some(NoEvidence::NoDependencyEdges),
            Self::Depth => denominator
                .is_none()
                .then_some(NoEvidence::NoDependencyChain),
            Self::Equality => {
                (denominator.unwrap_or(0) < 2).then_some(NoEvidence::FewerThanTwoComplexityFiles)
            }
            Self::Modularity if files_analyzed < 2 => Some(NoEvidence::FewerThanTwoFiles),
            Self::Modularity => denominator.is_none().then_some(NoEvidence::NoComponents),
            Self::Redundancy | Self::CoverageDiscipline => {
                denominator.is_none().then_some(NoEvidence::NoFunctions)
            }
        }
    }
}

/// Why a dimension's score is the formula's value for an empty measurement
/// rather than a measurement of the scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoEvidence {
    NoFiles,
    NoDependencyEdges,
    NoDependencyChain,
    FewerThanTwoComplexityFiles,
    FewerThanTwoFiles,
    NoComponents,
    NoFunctions,
}

impl NoEvidence {
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoFiles => "0 files in scope",
            Self::NoDependencyEdges => "no dependency edges",
            Self::NoDependencyChain => "no dependency chain",
            Self::FewerThanTwoComplexityFiles => "fewer than 2 files with measured complexity",
            Self::FewerThanTwoFiles => "fewer than 2 files",
            Self::NoComponents => "no components after hub removal",
            Self::NoFunctions => "no functions",
        }
    }
}

#[tracing::instrument(name = "usecases.graph.health_snapshot", level = "trace", skip_all)]
pub async fn compute_verified_health_snapshot(
    graph: &GraphQueryManager<'_>,
    path_prefix: Option<&str>,
) -> Result<VerifiedHealthSnapshotV1> {
    let inputs = tracing::Instrument::instrument(
        graph.health_inputs(path_prefix),
        tracing::trace_span!("usecases.graph.health.inputs"),
    )
    .await?;
    Ok(health_snapshot_from_inputs(&inputs))
}

pub fn health_snapshot_from_inputs(inputs: &VerifiedHealthInputsV1) -> VerifiedHealthSnapshotV1 {
    let adjacency = &inputs.adjacency;
    let files_analyzed = adjacency.len();
    let total_edges = adjacency.values().map(HashSet::len).sum();
    let (acyclicity, edges_in_cycles) = acyclicity_score(adjacency);
    let depth_result = dependency_depth(adjacency, 1);
    let depth = depth_score(depth_result.max_depth, depth_result.ideal_depth);
    let aggregates = &inputs.aggregates;
    let complexity_values = aggregates
        .iter()
        .map(|aggregate| aggregate.complexity)
        .collect::<Vec<_>>();
    let complexity_files = complexity_values.len();
    let incomplete_complexity_symbols = aggregates
        .iter()
        .map(|aggregate| aggregate.incomplete_complexity_symbols)
        .sum::<usize>();
    let total_fns = aggregates
        .iter()
        .map(|aggregate| aggregate.function_methods)
        .sum::<usize>();
    let skip_coverage_count = aggregates
        .iter()
        .map(|aggregate| aggregate.skipped_function_methods)
        .sum::<usize>();
    let dead_count = aggregates
        .iter()
        .map(|aggregate| aggregate.dead_function_methods)
        .sum::<usize>();
    let gini = gini_coefficient(&complexity_values);
    let equality = (1.0 - gini).clamp(0.0, 1.0);
    let redundancy = if total_fns == 0 {
        1.0
    } else {
        (1.0 - dead_count as f64 / total_fns as f64).clamp(0.0, 1.0)
    };
    let (modularity, modularity_components) = modularity_score(adjacency);
    let coverage_discipline = if total_fns == 0 {
        1.0
    } else {
        (1.0 - skip_coverage_count as f64 / total_fns as f64).clamp(0.0, 1.0)
    };
    let quality_signal = compute_composite_health(&HealthDimensions {
        acyclicity,
        depth,
        equality,
        redundancy,
        modularity,
        coverage_discipline,
    });
    VerifiedHealthSnapshotV1 {
        quality_signal,
        files_analyzed,
        acyclicity,
        depth,
        equality,
        redundancy,
        modularity,
        coverage_discipline,
        gini,
        edges_in_cycles,
        total_edges,
        max_chain: depth_result.max_depth,
        ideal_chain: depth_result.ideal_depth,
        complexity_files,
        incomplete_complexity_symbols,
        modularity_components,
        dead_count,
        total_fns,
        skip_coverage_count,
    }
}

/// Computes the Gini coefficient for a slice of non-negative values.
/// Returns 0.0 for empty slices, single-element slices, or all-zero slices.
/// Result is in \[0.0, 1.0\] where 0.0 = perfect equality.
#[tracing::instrument(name = "usecases.graph.gini", level = "trace", skip_all)]
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

/// Computes the acyclicity score for a directed graph.
/// Uses Tarjan's SCC algorithm. Score = 1.0 - (`edges_in_nontrivial_SCCs` / `total_edges`).
/// Returns (score, `number_of_edges_in_cycles`).
#[tracing::instrument(name = "usecases.graph.acyclicity", level = "trace", skip_all)]
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

/// A chain entry representing a file and the longest dependency chain reaching it.
pub struct DepthChain {
    pub file: String,
    /// Every file in this chain entry's strongly connected component. Files in
    /// one SCC share the same collapsed-DAG depth.
    pub scc_files: Vec<String>,
    pub depth: usize,
    pub chain: Vec<String>,
}

/// Result of the dependency depth analysis.
pub struct DepthResult {
    pub max_depth: usize,
    /// `ceil(log2(file_count))`
    pub ideal_depth: usize,
    pub chains: Vec<DepthChain>,
}

/// Computes longest dependency chains. Breaks cycles via Tarjan's SCC
/// (collapses each SCC to a single node), then runs topo sort + DP.
#[tracing::instrument(name = "usecases.graph.dependency_depth", level = "trace", skip_all)]
pub fn dependency_depth<S1: BuildHasher, S2: BuildHasher>(
    adj: &HashMap<String, HashSet<String, S2>, S1>,
    limit: usize,
) -> DepthResult {
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

    let sccs = tarjan_scc(adj);
    let mut node_to_scc: HashMap<String, usize> = HashMap::new();
    for (idx, scc) in sccs.iter().enumerate() {
        for node in scc {
            node_to_scc.insert(node.clone(), idx);
        }
    }

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

    // Rank every SCC before applying the response limit. Reconstructing
    // only the selected chains keeps small-limit callers proportional to their
    // requested result count.
    let max_depth = dist.iter().copied().max().unwrap_or(0);
    let mut ranked_sccs = (0..scc_count).collect::<Vec<_>>();
    ranked_sccs.sort_unstable_by(|left, right| {
        dist[*right]
            .cmp(&dist[*left])
            .then_with(|| sccs[*left].iter().min().cmp(&sccs[*right].iter().min()))
    });
    ranked_sccs.truncate(limit);

    let mut results: Vec<DepthChain> = Vec::with_capacity(ranked_sccs.len());
    for scc_idx in ranked_sccs {
        let depth = dist[scc_idx];

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

    DepthResult {
        max_depth,
        ideal_depth,
        chains: results,
    }
}

/// One directory cluster in the Design Structure Matrix ordering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DsmCluster {
    pub directory: String,
    pub file_count: usize,
    pub internal_edges: usize,
    pub outgoing_edges: usize,
    pub incoming_edges: usize,
}

impl DsmCluster {
    #[must_use]
    pub const fn boundary_edges(&self) -> usize {
        self.outgoing_edges + self.incoming_edges
    }
}

/// Groups the file adjacency by parent directory and orders clusters by
/// cross-boundary coupling, then cluster size. This is the shared authority for
/// both the MCP DSM tool and dashboard graph strata.
#[tracing::instrument(name = "usecases.graph.dsm_clusters", level = "trace", skip_all)]
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

    let mut clusters = Vec::with_capacity(dir_to_files.len());
    let mut file_to_cluster = HashMap::with_capacity(adj.len());
    for (directory, files) in &dir_to_files {
        let cluster_index = clusters.len();
        file_to_cluster.extend(files.iter().map(|file| (file.as_str(), cluster_index)));
        clusters.push(DsmCluster {
            directory: directory.clone(),
            file_count: files.len(),
            internal_edges: 0,
            outgoing_edges: 0,
            incoming_edges: 0,
        });
    }
    for (source, targets) in adj {
        let source_cluster = file_to_cluster[source.as_str()];
        for target in targets {
            match file_to_cluster.get(target.as_str()).copied() {
                Some(target_cluster) if target_cluster == source_cluster => {
                    clusters[source_cluster].internal_edges += 1;
                }
                Some(target_cluster) => {
                    clusters[source_cluster].outgoing_edges += 1;
                    clusters[target_cluster].incoming_edges += 1;
                }
                None => {
                    clusters[source_cluster].outgoing_edges += 1;
                }
            }
        }
    }
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

/// Estimates modularity by removing hub nodes and counting connected components.
/// Hub nodes = files with (fan\_in + fan\_out) > mean + 2\*stddev.
/// Score = 1.0 - (1.0 / component\_count), clamped to \[0, 1\].
/// Returns (score, component\_count\_after\_hub\_removal).
#[tracing::instrument(name = "usecases.graph.modularity", level = "trace", skip_all)]
pub fn modularity_score<S1: BuildHasher, S2: BuildHasher>(
    adj: &HashMap<String, HashSet<String, S2>, S1>,
) -> (f64, usize) {
    if adj.is_empty() {
        return (1.0, 0);
    }

    let mut all_nodes: HashSet<String> = adj.keys().cloned().collect();
    for targets in adj.values() {
        all_nodes.extend(targets.iter().cloned());
    }

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

    let n = connectivity.len() as f64;
    let values: Vec<f64> = connectivity.values().map(|&v| v as f64).collect();
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / n;
    let stddev = variance.sqrt();
    let threshold = mean + 2.0 * stddev;

    let hubs: HashSet<&str> = connectivity
        .iter()
        .filter(|&(_, &v)| v as f64 > threshold)
        .map(|(&k, _)| k)
        .collect();

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

/// All five health dimensions, each in \[0.0, 1.0\].
#[derive(Debug, Clone)]
pub struct HealthDimensions {
    pub acyclicity: f64,
    pub depth: f64,
    pub equality: f64,
    pub redundancy: f64,
    pub modularity: f64,
    /// Penalty for overuse of `/// skip-test-coverage` annotations.
    /// 1.0 = no skips, decays towards 0.0 as skip ratio increases.
    pub coverage_discipline: f64,
}

/// Measured dimensions, worst score first with ties broken by name, then
/// `no evidence: name (reason), ...` for the dimensions the scope gave
/// nothing to measure.
pub fn health_panel(snapshot: &VerifiedHealthSnapshotV1) -> String {
    if snapshot.files_analyzed == 0 {
        return format!("no evidence: {}", NoEvidence::NoFiles.label());
    }
    let mut measured = Vec::new();
    let mut unmeasured = Vec::new();
    for dimension in HealthDimension::ALL {
        match snapshot.no_evidence(dimension) {
            None => measured.push(dimension),
            Some(reason) => unmeasured.push(format!("{} ({})", dimension.name(), reason.label())),
        }
    }
    measured.sort_by(|left, right| {
        left.score(snapshot)
            .total_cmp(&right.score(snapshot))
            .then_with(|| left.name().cmp(right.name()))
    });
    let mut sections = Vec::new();
    if !measured.is_empty() {
        sections.push(
            measured
                .iter()
                .map(|dimension| dimension.name())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if !unmeasured.is_empty() {
        sections.push(format!("no evidence: {}", unmeasured.join(", ")));
    }
    sections.join("; ")
}

/// Computes quality signal (0–10000) from geometric mean of all five dimensions.
/// Formula: `(product of all 5).powf(1.0/5.0) * 10000.0`, rounded.
/// Zero in any dimension → 0.
/// A low-weight multiplicative penalty for `coverage_discipline` reduces
/// the score by up to 2% when skip-test-coverage is overused.
pub fn compute_composite_health(dims: &HealthDimensions) -> u32 {
    let product = dims.acyclicity * dims.depth * dims.equality * dims.redundancy * dims.modularity;

    if product <= 0.0 {
        return 0;
    }

    let base = (product.powf(1.0 / 5.0) * 10_000.0).round();
    let penalized = base * (0.98 + 0.02 * dims.coverage_discipline);
    penalized.round() as u32
}

#[cfg(test)]
mod panel_tests {
    use std::collections::{HashMap, HashSet};

    use super::{HealthDimension, NoEvidence, health_panel, health_snapshot_from_inputs};
    use crate::queries::{VerifiedHealthFileAggregateV1, VerifiedHealthInputsV1};

    fn scope(
        files: &[(&str, &[&str])],
        aggregates: &[(&str, f64, usize, usize)],
    ) -> VerifiedHealthInputsV1 {
        VerifiedHealthInputsV1 {
            adjacency: files
                .iter()
                .map(|(file, targets)| {
                    (
                        (*file).to_owned(),
                        targets.iter().map(|target| (*target).to_owned()).collect(),
                    )
                })
                .collect::<HashMap<_, HashSet<_>>>(),
            aggregates: aggregates
                .iter()
                .map(
                    |(file, complexity, functions, dead)| VerifiedHealthFileAggregateV1 {
                        file_path: (*file).to_owned(),
                        complexity: *complexity,
                        function_methods: *functions,
                        dead_function_methods: *dead,
                        ..VerifiedHealthFileAggregateV1::default()
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn each_dimension_needs_its_own_evidence() {
        use HealthDimension::{
            Acyclicity, CoverageDiscipline, Depth, Equality, Modularity, Redundancy,
        };
        use NoEvidence::{
            FewerThanTwoComplexityFiles, FewerThanTwoFiles, NoComponents, NoDependencyChain,
            NoDependencyEdges, NoFiles, NoFunctions,
        };

        for dimension in HealthDimension::ALL {
            assert_eq!(
                dimension.no_evidence(Some(5), 0),
                Some(NoFiles),
                "{dimension:?}"
            );
        }
        let cases = [
            (Acyclicity, None, 3, Some(NoDependencyEdges)),
            (Acyclicity, Some(1), 3, None),
            (Depth, None, 3, Some(NoDependencyChain)),
            (Depth, Some(1), 3, None),
            (Equality, None, 3, Some(FewerThanTwoComplexityFiles)),
            (Equality, Some(1), 3, Some(FewerThanTwoComplexityFiles)),
            (Equality, Some(2), 3, None),
            (Modularity, Some(1), 1, Some(FewerThanTwoFiles)),
            (Modularity, None, 3, Some(NoComponents)),
            (Modularity, Some(1), 2, None),
            (Redundancy, None, 3, Some(NoFunctions)),
            (Redundancy, Some(1), 3, None),
            (CoverageDiscipline, None, 3, Some(NoFunctions)),
            (CoverageDiscipline, Some(1), 3, None),
        ];
        for (dimension, denominator, files, expected) in cases {
            assert_eq!(
                dimension.no_evidence(denominator, files),
                expected,
                "{dimension:?} over denominator {denominator:?} and {files} files"
            );
        }
    }

    #[test]
    fn a_fully_measured_scope_lists_every_dimension_worst_first() {
        let snapshot = health_snapshot_from_inputs(&scope(
            &[("a.rs", &["b.rs"]), ("b.rs", &[]), ("c.rs", &[])],
            &[("a.rs", 2.0, 2, 1), ("b.rs", 4.0, 2, 0)],
        ));

        assert_eq!(
            health_panel(&snapshot),
            "modularity, redundancy, equality, acyclicity, coverage_discipline, depth"
        );
    }

    #[test]
    fn heterogeneous_scopes_name_each_dimension_without_evidence() {
        let cases: [(&str, VerifiedHealthInputsV1, &str); 5] = [
            (
                "functions without edges",
                scope(
                    &[("a.rs", &[]), ("b.rs", &[])],
                    &[("a.rs", 1.0, 1, 0), ("b.rs", 1.0, 1, 0)],
                ),
                "modularity, coverage_discipline, equality, redundancy; no evidence: acyclicity (no dependency edges), depth (no dependency chain)",
            ),
            (
                "edges without functions",
                scope(&[("a.rs", &["b.rs"]), ("b.rs", &[]), ("c.rs", &[])], &[]),
                "modularity, acyclicity, depth; no evidence: coverage_discipline (no functions), equality (fewer than 2 files with measured complexity), redundancy (no functions)",
            ),
            (
                "a cycle without a chain",
                scope(&[("a.rs", &["b.rs"]), ("b.rs", &["a.rs"])], &[]),
                "acyclicity, modularity; no evidence: coverage_discipline (no functions), depth (no dependency chain), equality (fewer than 2 files with measured complexity), redundancy (no functions)",
            ),
            (
                "one file without functions",
                scope(&[("a.rs", &[])], &[]),
                "no evidence: acyclicity (no dependency edges), coverage_discipline (no functions), depth (no dependency chain), equality (fewer than 2 files with measured complexity), modularity (fewer than 2 files), redundancy (no functions)",
            ),
            ("no files", scope(&[], &[]), "no evidence: 0 files in scope"),
        ];
        for (name, inputs, panel) in cases {
            assert_eq!(
                health_panel(&health_snapshot_from_inputs(&inputs)),
                panel,
                "{name}"
            );
        }
    }

    #[test]
    fn dimensions_without_evidence_still_enter_the_signal_at_their_formula_value() {
        let edges_without_functions = health_snapshot_from_inputs(&scope(
            &[("a.rs", &["b.rs"]), ("b.rs", &[]), ("c.rs", &[])],
            &[],
        ));
        let empty = health_snapshot_from_inputs(&scope(&[], &[]));

        assert_eq!(edges_without_functions.quality_signal, 8706);
        assert_eq!(empty.quality_signal, 10_000);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::BuildHasherDefault;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Clone)]
    struct CountingBuildHasher {
        hashes: Arc<AtomicUsize>,
    }

    struct CountingHasher {
        hashes: Arc<AtomicUsize>,
        state: u64,
    }

    impl BuildHasher for CountingBuildHasher {
        type Hasher = CountingHasher;

        fn build_hasher(&self) -> Self::Hasher {
            CountingHasher {
                hashes: Arc::clone(&self.hashes),
                state: 0xcbf2_9ce4_8422_2325,
            }
        }
    }

    impl Hasher for CountingHasher {
        fn finish(&self) -> u64 {
            self.state
        }

        fn write(&mut self, bytes: &[u8]) {
            self.hashes.fetch_add(1, Ordering::Relaxed);
            for byte in bytes {
                self.state ^= u64::from(*byte);
                self.state = self.state.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }

    #[test]
    fn dsm_clusters_reads_each_adjacency_edge_once() {
        const FILES: usize = 100;

        let hashes = Arc::new(AtomicUsize::new(0));
        let hasher = CountingBuildHasher {
            hashes: Arc::clone(&hashes),
        };
        let mut adjacency = HashMap::new();
        for index in 0..FILES {
            let source = format!("cluster_{index:03}/source.rs");
            let target = format!("cluster_{:03}/source.rs", (index + 1) % FILES);
            let mut targets = HashSet::with_hasher(hasher.clone());
            targets.insert(target);
            adjacency.insert(source, targets);
        }
        hashes.store(0, Ordering::Relaxed);

        let clusters = dsm_clusters(&adjacency);

        assert_eq!(clusters.len(), FILES);
        assert!(
            clusters.iter().all(|cluster| {
                cluster.internal_edges == 0
                    && cluster.outgoing_edges == 1
                    && cluster.incoming_edges == 1
            }),
            "ring topology must retain one incoming and outgoing edge per directory"
        );
        let hash_lookups = hashes.load(Ordering::Relaxed);
        assert!(
            hash_lookups <= FILES * 4,
            "DSM clustering hashed adjacency probes {hash_lookups} times for {FILES} edges"
        );
    }

    #[test]
    fn skip_test_coverage_overuse_costs_at_most_two_percent_of_the_signal() {
        let perfect = HealthDimensions {
            acyclicity: 1.0,
            depth: 1.0,
            equality: 1.0,
            redundancy: 1.0,
            modularity: 1.0,
            coverage_discipline: 1.0,
        };
        let every_function_skipped = HealthDimensions {
            coverage_discipline: 0.0,
            ..perfect.clone()
        };
        let half_skipped = HealthDimensions {
            coverage_discipline: 0.5,
            ..perfect.clone()
        };

        assert_eq!(compute_composite_health(&perfect), 10_000);
        assert_eq!(compute_composite_health(&half_skipped), 9_900);
        assert_eq!(compute_composite_health(&every_function_skipped), 9_800);
    }

    #[test]
    fn dependency_depth_limits_after_ranking_all_components() {
        let mut adjacency =
            HashMap::<String, HashSet<String>, BuildHasherDefault<DefaultHasher>>::default();
        adjacency.insert("isolated.rs".into(), HashSet::new());
        adjacency.insert("root.rs".into(), HashSet::from(["middle.rs".into()]));
        adjacency.insert("middle.rs".into(), HashSet::from(["deepest.rs".into()]));
        adjacency.insert("deepest.rs".into(), HashSet::new());

        let components = tarjan_scc(&adjacency);
        let deepest_component = components
            .iter()
            .position(|component| component.iter().any(|file| file == "deepest.rs"))
            .expect("deepest component");
        assert!(
            deepest_component > 0,
            "fixture must place the deepest SCC beyond the former limit"
        );

        let result = dependency_depth(&adjacency, 1);

        assert_eq!(result.max_depth, 2);
        assert_eq!(result.chains.len(), 1);
        assert_eq!(result.chains[0].depth, result.max_depth);
        assert_eq!(result.chains[0].file, "deepest.rs");
        assert_eq!(
            result.chains[0].chain,
            ["root.rs", "middle.rs", "deepest.rs"]
        );
    }
}
