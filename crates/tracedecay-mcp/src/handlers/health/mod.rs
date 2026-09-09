//! Portable code-health report handlers.

mod dsm;
mod reports;
mod runtime;
mod test_map;

pub use dsm::handle_dsm;
pub use reports::{handle_dependency_depth, handle_gini, handle_health};
pub use runtime::{collect_database_snapshot, handle_runtime};
pub use test_map::{handle_test_map, handle_test_risk};

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::tools::render::{self, Md};
use crate::{
    ToolResult, effective_path, generic_tool_result, rendered_tool_result, unique_file_paths,
};
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
