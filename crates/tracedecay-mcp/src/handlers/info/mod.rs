//! Portable project-info and file-inspection tool handlers.
//!
//! Each tool owns a sibling module; this module holds the constants shared by
//! more than one sibling, and the re-exports the handler dispatcher calls.

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
