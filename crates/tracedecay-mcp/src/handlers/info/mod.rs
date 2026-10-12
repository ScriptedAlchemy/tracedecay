//! Portable project-info and file-inspection tool handlers.
//!
//! Each tool owns a sibling module; this module holds the constants shared by
//! more than one sibling, and the re-exports the handler dispatcher calls.

use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::errors::{Result, TraceDecayError};

mod config;
mod files;
mod port_order;
mod port_status;
mod registry;
mod remote_status;
mod status;
mod todos;
mod verified;

pub use config::compute_config;
pub use files::{compute_files, render_files_md};
pub use port_order::compute_port_order;
pub use port_status::compute_port_status;
pub use registry::compute_registry_read;
pub(crate) use registry::render_registry_listing_md;
pub use remote_status::read_remote_status;
pub(crate) use status::render_status_md;
pub use status::{
    compute_active_project, compute_status, graph_statistics_value, readiness_wait_outcome,
};
pub use todos::compute_todos;

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

/// The supported node kinds a port request names, or the defaults when it
/// names none. Unsupported names are skipped; an empty result is refused.
fn port_kinds(requested: Option<&[String]>) -> Result<Vec<NodeKind>> {
    let kinds: Vec<NodeKind> = match requested {
        Some(names) => names.iter().filter_map(|s| NodeKind::from_str(s)).collect(),
        None => PORT_DEFAULT_KINDS
            .iter()
            .filter_map(|s| NodeKind::from_str(s))
            .collect(),
    };
    if kinds.is_empty() {
        return Err(TraceDecayError::Config {
            message: "invalid parameter: kinds must contain at least one supported node kind"
                .to_owned(),
        });
    }
    Ok(kinds)
}
