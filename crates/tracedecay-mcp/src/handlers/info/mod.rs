//! Portable project-info and file-inspection tool handlers.
//!
//! Each tool owns a sibling module; this module holds the shared markdown
//! enrichment helpers used by more than one sibling, and the re-exports the
//! handler dispatcher calls.

mod body;
mod config;
mod files;
mod outline;
mod port_order;
mod port_status;
mod read;
mod registry;
mod remote_status;
mod signature_search;
mod simplify_scan;
mod todos;
mod type_hierarchy;
mod verified;

pub use body::{extract_lines, handle_body};
pub use config::handle_config;
pub use files::handle_files;
pub use outline::handle_outline;
pub use port_order::handle_port_order;
pub use port_status::handle_port_status;
pub use read::handle_read;
pub use registry::{handle_project_context, handle_project_list, handle_project_search};
pub use remote_status::handle_remote_status;
pub use signature_search::handle_signature_search;
pub use simplify_scan::handle_simplify_scan;
pub use todos::handle_todos;
pub use type_hierarchy::handle_type_hierarchy;

use std::path::Path;

use crate::tools::render::Md;
use serde_json::Value;
use tracedecay_graph_query::context::markdown_sections::{
    SectionEnrichment, is_markdown_file, section_summary_lines,
};
use tracedecay_runtime_core::tracedecay::current_timestamp;

/// Adds the section lane — title, truncated preview, full-body retrieval
/// handle, line span, and parsed section structure — to every markdown section
/// symbol in a `{"symbols": [...]}` container.
///
/// This is an enrichment of a surface that already answered: a file that cannot
/// be read, or a container with no symbol array, leaves the payload exactly as
/// it was rather than failing the outline or read that carries it.
pub(super) fn enrich_markdown_sections(
    project_root: &Path,
    absolute_path: &Path,
    display_file: &str,
    container: &mut Value,
) {
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
    let Ok(source) = tracedecay_runtime_core::sync::read_source_file(absolute_path) else {
        return;
    };
    SectionEnrichment::new(Some(project_root), current_timestamp())
        .enrich_symbol_array(symbols, &source);
}

/// Emits one symbol's markdown-section lane under its outline/read bullet.
///
/// The summary lines themselves are composed in
/// `tracedecay-usecases::context::markdown_sections`; this adapter only owns
/// the markdown builder and the two-space bullet continuation indent.
pub(super) fn render_section_md(md: &mut Md, section: Option<&Value>) {
    let Some(section) = section else {
        return;
    };
    for line in section_summary_lines(section) {
        md.line(&format!("  {line}"));
    }
}

/// Default node kinds for port comparisons.
pub(super) const PORT_DEFAULT_KINDS: &[&str] = &[
    "function",
    "method",
    "class",
    "struct",
    "interface",
    "trait",
    "enum",
    "module",
];
