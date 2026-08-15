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
mod verified;

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
use crate::types::NodeKind;

use self::verified::{
    INFO_RELATION_LIMIT, all_symbols, end_line, indexed_files, info_graph_error,
    required_file_path, required_metadata, required_symbol_parts, symbols_in_dir,
};

use super::super::ToolResult;
use super::super::definitions;
use super::super::render::{self, Md};
use super::project_registry::{
    ProjectRegistryContextCommand, ProjectRegistryContextOutcome, ProjectRegistryListingCommand,
    ProjectRegistryListingOutcome, ProjectRegistryListingScope, ProjectRegistryReadPort,
    ProjectRegistrySelector, list_registered_projects, read_registered_project_context,
};
use super::support::{
    effective_path, generic_tool_result, is_explicit_project_path_selector, rendered_tool_result,
    require_node_id, require_object_args, unique_file_paths,
};

fn display_path(path: &std::path::Path) -> String {
    path.display().to_string()
}

/// Adds the section lane — title, truncated preview, full-body retrieval
/// handle, line span, and parsed section structure — to every markdown section
/// symbol in a `{"symbols": [...]}` container.
///
/// This is an enrichment of a surface that already answered: a file that cannot
/// be read, or a container with no symbol array, leaves the payload exactly as
/// it was rather than failing the outline or read that carries it.
fn enrich_markdown_sections(
    project_root: &Path,
    absolute_path: &Path,
    display_file: &str,
    container: &mut Value,
) {
    use crate::context::markdown_sections::{SectionEnrichment, is_markdown_file};

    if !is_markdown_file(display_file) {
        return;
    }
    let Some(symbols) = container
        .get_mut("symbols")
        .and_then(Value::as_array_mut)
        .filter(|symbols| !symbols.is_empty())
    else {
        return;
    };
    let Ok(source) = crate::sync::read_source_file(absolute_path) else {
        return;
    };
    SectionEnrichment::new(Some(project_root), crate::tracedecay::current_timestamp())
        .enrich_symbol_array(symbols, &source);
}

/// Emits one symbol's markdown-section lane under its outline/read bullet.
///
/// The summary lines themselves are composed in
/// `tracedecay-usecases::context::markdown_sections`; this adapter only owns
/// the markdown builder and the two-space bullet continuation indent.
fn render_section_md(md: &mut Md, section: Option<&Value>) {
    let Some(section) = section else {
        return;
    };
    for line in crate::context::markdown_sections::section_summary_lines(section) {
        md.line(&format!("  {line}"));
    }
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
