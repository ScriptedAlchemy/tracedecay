//! Portable code-health report handlers.

mod dsm;
mod reports;
mod runtime;
mod test_map;

pub use dsm::{compute_dsm, render_dsm_md};
pub use reports::{compute_dependency_depth, compute_gini, compute_health};
pub use runtime::{collect_database_snapshot, handle_runtime};
pub use test_map::{compute_test_map, compute_test_risk};

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    DependencyDepthChainV1, DependencyDepthResultV1, DependencyDepthSurfaceRequestV1, DsmClusterV1,
    DsmMatrixV1, DsmResultV1, DsmShapeV1, DsmStatsV1, DsmSurfaceRequestV1, GiniMetricV1,
    GiniOutlierV1, GiniResultV1, GiniScopeV1, GiniSurfaceRequestV1, HealthAcyclicityV1,
    HealthCoverageDisciplineV1, HealthDepthV1, HealthDimensionsV1, HealthEqualityV1,
    HealthModularityV1, HealthRedundancyV1, HealthResultV1, HealthSurfaceRequestV1,
    HealthWeightsV1, TestMapResultV1, TestMapSourceCoverageV1, TestMapSurfaceRequestV1,
    TestMapTestV1, TestMapUncoveredV1, TestRiskSurfaceRequestV1,
};

use crate::handlers::graph::graph_tool_completion;
use crate::handlers::support::decode_primitive_request;
use crate::tools::render::{self, Md};
use crate::unique_file_paths;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_graph_query::health::{
    dependency_depth, depth_score, dsm_clusters, gini_coefficient, gini_label,
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
