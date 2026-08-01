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
// Moved down from the root binary (`src/diagnose.rs`,
// `src/diagnostics_store.rs`, `src/diagnostics_publication.rs`,
// `src/diagnostics_query.rs`): their whole closure is the runtime kernel plus
// `tracedecay-domain`/`tracedecay-store`. See SEAMS.md.
pub mod diagnose;
pub mod diagnostics_publication;
pub mod diagnostics_query;
pub mod diagnostics_store;
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
// Moved down from the root binary. `analytics_bridge` kept only its durable
// hook-JSONL importer; `git_intelligence`/`git_query` are the native git
// adapter and its read engine; `graph`/`retention`/`request_identity`/
// `user_config` had kernel-only closures. See SEAMS.md.
pub mod analytics_bridge;
pub mod application_surface;
pub mod git_intelligence;
pub mod git_query;
pub mod git_reads;
pub mod graph;
pub mod host_admission;
pub mod lsp_runtime;
mod lsp_support;
pub mod memory;
pub mod observability;
pub mod observation;
pub mod operation_stream;
pub mod primitives;
// The TTL'd remote-JSON cache mechanism shared by the two model-pricing
// tables (root `accounting::pricing` and the dashboard's `savings_pricing`).
// Both crates already depend on this one, so it is the shared home that costs
// no new dependency edge.
pub mod remote_json_cache;
pub mod request_identity;
pub mod response_handles;
pub mod retention;
pub(crate) mod retrieval_anchor_store;
pub mod semantic_runtime;
pub mod session;
pub mod settings_control;
pub mod source_authorization;
pub mod store;
pub mod tracedecay;
pub mod user_config;

pub use source_authorization::{
    ProjectSourceAccessDenial, ProjectSourceAccessOutcome, ProjectSourceAccessSnapshot,
    project_source_access_snapshot_for_request,
};
