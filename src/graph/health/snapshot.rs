//! The scoped health snapshot — the one value every health surface is built on.
//!
//! This lives below the MCP handler tree so the tool handlers and the root
//! engine's downward ports can both read it without either depending on the
//! other. The computation is byte-for-byte the pre-move handler code.

use std::collections::HashSet;

use crate::errors::Result;
use crate::graph::queries::GraphQueryManager;
use crate::tracedecay::TraceDecay;

use super::{
    HealthDimensions, acyclicity_score, compute_composite_health, dependency_depth, depth_score,
    gini_coefficient, modularity_score,
};

pub(crate) struct HealthSnapshot {
    pub(crate) quality_signal: u32,
    pub(crate) files_analyzed: usize,
    pub(crate) acyclicity: f64,
    pub(crate) depth: f64,
    pub(crate) equality: f64,
    pub(crate) redundancy: f64,
    pub(crate) modularity: f64,
    pub(crate) coverage_discipline: f64,
    /// Raw signals retained for `details=true` (#82).
    pub(crate) gini: f64,
    pub(crate) edges_in_cycles: usize,
    pub(crate) total_edges: usize,
    pub(crate) max_chain: usize,
    pub(crate) ideal_chain: usize,
    pub(crate) complexity_files: usize,
    pub(crate) modularity_components: usize,
    pub(crate) dead_count: usize,
    pub(crate) total_fns: usize,
    pub(crate) skip_coverage_count: usize,
}

/// The six named dimensions of a snapshot, in their canonical report order.
pub(crate) fn session_dimension_values(snap: &HealthSnapshot) -> [(&'static str, f64); 6] {
    [
        ("acyclicity", snap.acyclicity),
        ("depth", snap.depth),
        ("equality", snap.equality),
        ("redundancy", snap.redundancy),
        ("modularity", snap.modularity),
        ("coverage_discipline", snap.coverage_discipline),
    ]
}

/// Computes all 5 health dimensions and the composite signal for a given scope.
pub(crate) async fn compute_health_snapshot(
    cg: &TraceDecay,
    path_prefix: Option<&str>,
) -> Result<HealthSnapshot> {
    let adj = GraphQueryManager::new(cg.db())
        .build_file_adjacency(path_prefix)
        .await?;
    let files_analyzed = adj.len();
    let total_edges = adj.values().map(HashSet::len).sum();

    let (acyclicity, edges_in_cycles) = acyclicity_score(&adj);
    let depth_result = dependency_depth(&adj, 1);
    let depth = depth_score(depth_result.max_depth, depth_result.ideal_depth);

    // Per-file aggregates — weighted complexity, function/method count,
    // skip-test-coverage count, and dead function/method count — are folded
    // inside SQLite in one keyset-paged `GROUP BY` scan instead of
    // materializing the whole node table in the process. Filtering grouped
    // rows by scope is byte-identical to filtering nodes before folding,
    // because every node in a file shares that file's path.
    let file_aggregates = cg.db().health_file_aggregates().await?;
    let mut complexity_values: Vec<f64> = Vec::with_capacity(file_aggregates.len());
    let mut total_fns = 0usize;
    let mut skipped_in_scope = 0usize;
    let mut dead_count = 0usize;
    for agg in &file_aggregates {
        if crate::path_scope::path_matches_scope(&agg.file_path, path_prefix) {
            complexity_values.push(agg.complexity);
            total_fns += agg.function_methods;
            skipped_in_scope += agg.skipped_function_methods;
            dead_count += agg.dead_function_methods;
        }
    }
    let complexity_files = complexity_values.len();
    let gini = gini_coefficient(&complexity_values);
    let equality = (1.0 - gini).clamp(0.0, 1.0);

    let redundancy = if total_fns == 0 {
        1.0
    } else {
        (1.0 - dead_count as f64 / total_fns as f64).clamp(0.0, 1.0)
    };

    let (modularity, modularity_components) = modularity_score(&adj);

    // coverage_discipline: penalise overuse of skip-test-coverage annotations.
    let coverage_discipline = if total_fns == 0 {
        1.0
    } else {
        (1.0 - skipped_in_scope as f64 / total_fns as f64).clamp(0.0, 1.0)
    };

    let dims = HealthDimensions {
        acyclicity,
        depth,
        equality,
        redundancy,
        modularity,
        coverage_discipline,
    };
    let quality_signal = compute_composite_health(&dims);

    Ok(HealthSnapshot {
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
        modularity_components,
        dead_count,
        total_fns,
        skip_coverage_count: skipped_in_scope,
    })
}
