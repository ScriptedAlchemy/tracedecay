//! Code-health tool handlers.
//!
//! One sibling module per report. This module holds the shared imports (which
//! siblings pick up through `use super::*`) plus the health snapshot itself —
//! the one value every sibling is built on.

mod dsm;
mod reports;
mod runtime;
mod session;
mod test_map;

pub(super) use dsm::handle_dsm;
pub(super) use reports::{handle_dependency_depth, handle_gini, handle_health};
pub(super) use runtime::handle_runtime;
pub(super) use session::{handle_session_end, handle_session_start};
pub(super) use test_map::{handle_test_map, handle_test_risk};

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::graph::health::delta::compute_health_delta_result;
use crate::graph::health::snapshot::{
    HealthSnapshot, compute_health_snapshot, session_dimension_values,
};
use crate::graph::health::{
    dependency_depth, depth_score, dsm_clusters, gini_coefficient, gini_label,
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
