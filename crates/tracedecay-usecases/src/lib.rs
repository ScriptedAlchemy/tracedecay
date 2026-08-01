//! Typed product use cases shared by CLI, MCP, HTTP, hooks, and daemon adapters.

pub mod advisory;
pub mod anchor_resolution;
pub mod api_migration;
pub mod code_index;
// Moved down from the root binary's `src/config/`: see `config/mod.rs` and
// SEAMS.md — root must delete its copies and re-export from here.
pub mod config;
pub mod configuration;
pub mod context;
pub mod dashboard_diagnostics;
pub mod doctor_remediation;
pub mod edit;
// Widened from `pub(crate)`: the root shim re-exports this crate, and root
// adapters (`src/mcp`, `src/daemon`, `src/store`) publish onto the event lane.
pub mod event_lane;
pub mod evidence_assembly;
// Widened from `pub(crate)`: `tracedecay-global-db` reaches the runtime
// external-source store through the root shim (see that crate's SEAMS.md).
pub mod external_source_store;
pub mod feedback;
pub mod git_reads;
pub mod host_admission;
pub mod lsp_runtime;
pub mod memory;
pub mod observability;
pub mod observation;
pub mod operation_stream;
pub mod primitives;
pub(crate) mod retrieval_anchor_store;
pub mod semantic_runtime;
pub mod session;
pub mod settings_control;
pub mod source_authorization;

pub use source_authorization::{
    ProjectSourceAccessDenial, ProjectSourceAccessOutcome, ProjectSourceAccessSnapshot,
    project_source_access_snapshot_for_request,
};
