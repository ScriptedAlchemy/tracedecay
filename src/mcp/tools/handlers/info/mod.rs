//! Project-info, registry, and file-inspection tool handlers.
//!
//! Each tool owns a sibling module; this module holds only the shared imports
//! (which the siblings pick up through `use super::*`), the two helpers used by
//! more than one sibling, and the re-exports the handler dispatcher calls.

mod body;
mod config;
mod files;
mod outline;
mod port_order;
mod port_status;
mod read;
mod registry;
mod signature_search;
mod simplify_scan;
mod status;
mod todos;
mod type_hierarchy;

pub(super) use body::{extract_lines, handle_body};
pub(super) use config::handle_config;
pub(super) use files::handle_files;
pub(super) use outline::handle_outline;
pub(super) use port_order::handle_port_order;
pub(super) use port_status::handle_port_status;
pub(super) use read::handle_read;
pub(super) use registry::{handle_project_context, handle_project_list, handle_project_search};
pub(super) use signature_search::handle_signature_search;
pub(super) use simplify_scan::handle_simplify_scan;
pub(super) use status::{handle_active_project, handle_admin_sync, handle_status};
pub(super) use todos::handle_todos;
pub(super) use type_hierarchy::handle_type_hierarchy;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use crate::context::read_modes::{LineRange, ReadMode};
use crate::context::source_read::{SourceReadRequest, read_source, resolve_indexed_source_file};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{RegisteredGlobalDb, SessionIngestHealth};
use crate::path_tree::format_compact_annotated_path_list;
use crate::project_registry::{ProjectRegistryView, render_project_registry_view};
use crate::storage::{ProjectPath, StorageMode, StoreKind};
use crate::tracedecay::{BranchDiagnostics, TraceDecay};
use crate::types::{FileRecord, NodeKind, Visibility};

use super::super::ToolResult;
use super::super::definitions;
use super::super::render::{self, Md};
use super::dependency_hints;
use super::project_registry::{
    ProjectRegistryContextCommand, ProjectRegistryContextOutcome, ProjectRegistryListingCommand,
    ProjectRegistryListingOutcome, ProjectRegistryListingScope, ProjectRegistryReadPort,
    ProjectRegistrySelector, list_registered_projects, read_registered_project_context,
};
use super::support::{
    effective_path, filter_by_scope, generic_tool_result, is_explicit_project_path_selector,
    rendered_tool_result, require_node_id, require_object_args, unique_file_paths,
};

fn display_path(path: &std::path::Path) -> String {
    path.display().to_string()
}

/// Default node kinds for port comparisons.
const PORT_DEFAULT_KINDS: &[&str] = &[
    "function",
    "method",
    "class",
    "struct",
    "interface",
    "trait",
    "enum",
    "module",
];
