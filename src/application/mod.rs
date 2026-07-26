//! Typed product use cases shared by CLI, MCP, HTTP, hooks, and daemon adapters.

pub mod advisory;
pub mod anchor_resolution;
pub mod api_migration;
pub mod code_index;
pub mod configuration;
pub mod context;
pub mod dashboard_diagnostics;
pub mod doctor_remediation;
pub mod edit;
pub(crate) mod event_lane;
pub mod evidence_assembly;
pub(crate) mod external_source_store;
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
