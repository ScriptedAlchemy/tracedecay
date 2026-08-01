//! Code-health tool handlers.
//!
//! One sibling module per report. This module holds the shared imports (which
//! siblings pick up through `use super::*`) plus the health snapshot itself —
//! the one value every sibling is built on.

mod delta;
mod dsm;
mod reports;
mod runtime;
mod session;
mod test_map;

pub(crate) use delta::compute_health_delta_result;
pub(super) use dsm::handle_dsm;
pub(super) use reports::{handle_dependency_depth, handle_gini, handle_health};
pub(super) use runtime::handle_runtime;
pub(super) use session::{handle_session_end, handle_session_start};
pub(super) use test_map::{handle_test_map, handle_test_risk};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_application::retrieval::{
    HealthDeltaCoverageV1, HealthDeltaCurrentnessV1, HealthDeltaPointV1, HealthDeltaResult,
    HealthDeltaScopeV1, HealthDimensionDeltaV1, HealthDimensionPointV1,
};
use tracedecay_application::{
    ObservabilityApplicationV1, ObservabilityHorizonV1, ObservabilityQueryV1,
};
use tracedecay_domain::{
    CoverageStateV1, HealthDimensionObservedV1, HealthSnapshotObservedV1, ManifestDigest,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityRetentionClassV1,
    ObservabilityTerminalResultV1, UtcMicros, canonical_sha256,
};

use crate::application::observability::RegisteredObservabilityPortV1;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::graph::health::{
    HealthDimensions, acyclicity_score, compute_composite_health, dependency_depth, depth_score,
    dsm_clusters, gini_coefficient, gini_label, modularity_score,
};
use crate::graph::queries::GraphQueryManager;
use crate::tracedecay::TraceDecay;
use crate::types::NodeKind;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::{
    effective_path, generic_tool_result, rendered_tool_result, unique_file_paths,
};

/// Coarse human label for a modularity score in [0,1].
fn modularity_label(score: f64) -> &'static str {
    if score >= 0.75 {
        "high"
    } else if score >= 0.5 {
        "moderate"
    } else {
        "low"
    }
}

struct HealthSnapshot {
    quality_signal: u32,
    files_analyzed: usize,
    acyclicity: f64,
    depth: f64,
    equality: f64,
    redundancy: f64,
    modularity: f64,
    coverage_discipline: f64,
    /// Raw signals retained for `details=true` (#82).
    gini: f64,
    edges_in_cycles: usize,
    total_edges: usize,
    max_chain: usize,
    ideal_chain: usize,
    complexity_files: usize,
    modularity_components: usize,
    dead_count: usize,
    total_fns: usize,
    skip_coverage_count: usize,
}

/// Computes all 5 health dimensions and the composite signal for a given scope.
async fn compute_health_snapshot(
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

    let all_nodes = cg.get_all_nodes().await?;
    let nodes: Vec<_> = all_nodes
        .iter()
        .filter(|n| crate::path_scope::path_matches_scope(&n.file_path, path_prefix))
        .collect();

    let mut per_file_complexity: HashMap<String, f64> = HashMap::new();
    for n in &nodes {
        let c = f64::from(n.branches) * 2.0
            + f64::from(n.loops) * 2.0
            + f64::from(n.max_nesting) * 3.0
            + f64::from(n.end_line.saturating_sub(n.start_line) + 1);
        *per_file_complexity
            .entry(n.file_path.clone())
            .or_insert(0.0) += c;
    }
    let complexity_values: Vec<f64> = per_file_complexity.values().copied().collect();
    let complexity_files = complexity_values.len();
    let gini = gini_coefficient(&complexity_values);
    let equality = (1.0 - gini).clamp(0.0, 1.0);

    let dead = cg
        .find_dead_code(&[NodeKind::Function, NodeKind::Method], false)
        .await?;
    let dead_in_scope = dead
        .iter()
        .filter(|n| crate::path_scope::path_matches_scope(&n.file_path, path_prefix));
    let dead_count = dead_in_scope.count();
    let total_fns = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .count();
    let redundancy = if total_fns == 0 {
        1.0
    } else {
        (1.0 - dead_count as f64 / total_fns as f64).clamp(0.0, 1.0)
    };

    let (modularity, modularity_components) = modularity_score(&adj);

    // coverage_discipline: penalise overuse of skip-test-coverage annotations.
    let skip_coverage = cg.get_skip_test_coverage_node_ids().await?;
    let skipped_in_scope = nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Function | NodeKind::Method) && skip_coverage.contains(&n.id)
        })
        .count();
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
