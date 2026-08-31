//! Project-info handlers that still depend on composition-root authorities.
//!
//! Portable file-inspection tools live in `tracedecay_mcp::handlers::info`.

mod config;
mod registry;
mod remote_status;
mod simplify_scan;
mod status;

pub(super) use config::handle_config;
pub(super) use registry::{handle_project_context, handle_project_list, handle_project_search};
pub(super) use remote_status::handle_remote_status;
pub(super) use simplify_scan::handle_simplify_scan;
pub(crate) use status::graph_statistics_value;
pub(super) use status::{handle_active_project, handle_admin_sync, handle_status};

use std::path::Path;

use serde_json::{Value, json};

use crate::tracedecay::{BranchDiagnostics, TraceDecay};
use tracedecay_application::ProjectRegistryView;
use tracedecay_dashboard_api::project_registry::render_project_registry_view;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::{RegisteredGlobalDb, SessionIngestHealth};
use tracedecay_runtime_core::storage::{ProjectPath, StorageMode, StoreKind};

use super::support::{
    generic_tool_result, is_explicit_project_path_selector, rendered_tool_result,
};
use tracedecay_application::{
    ProjectRegistryContextCommand, ProjectRegistryContextOutcome, ProjectRegistryListingCommand,
    ProjectRegistryListingOutcome, ProjectRegistryListingScope, ProjectRegistryReadPort,
    ProjectRegistrySelector, list_registered_projects, read_registered_project_context,
};
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::tools::render::{self, Md};

fn display_path(path: &std::path::Path) -> String {
    path.display().to_string()
}

fn info_graph_error(reason_code: &str, detail: &str) -> TraceDecayError {
    TraceDecayError::project_route(reason_code, false, detail)
}
