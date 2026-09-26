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
pub use files::compute_files;
pub(crate) use files::render_files_md;
pub use port_order::compute_port_order;
pub use port_status::compute_port_status;
pub use registry::{handle_project_context, handle_project_list, handle_project_search};
pub use remote_status::handle_remote_status;
pub use status::{
    graph_statistics_value, handle_active_project, handle_status, readiness_wait_outcome,
    status_readiness_wait,
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
