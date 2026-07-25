//! Typed product use cases shared by CLI, MCP, HTTP, hooks, and daemon adapters.

pub mod advisory;
pub mod anchor_resolution;
pub mod code_diagnostics_control;
pub mod code_index;
pub mod configuration;
pub mod context;
pub mod edit;
pub mod evidence_assembly;
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
