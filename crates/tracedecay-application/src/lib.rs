//! Typed product use cases shared by CLI, MCP, HTTP, hooks, and daemon adapters.
//!
//! This top-of-stack orchestration layer composes the ports and types from
//! `tracedecay-contracts` with the workspace's runtime, storage, session,
//! configuration, indexing, and semantic owners. The contracts crate never
//! depends on this crate.
//!
//! Host adapters, the dashboard API, and the root binary depend on this crate,
//! so their seams are resolved through port inversion rather than reverse
//! dependency edges.
//!
//! ## Composition-root ports
//!
//! Graph reads use the Grafeo-backed
//! `tracedecay_global_db::VerifiedGraphRuntimePortV1` (defined in
//! `tracedecay_runtime_core::shard_runtime`) rather than a
//! parallel port owned here.
//! Source editing is its own vertical slice in `tracedecay-source-edit`:
//! planning, preview capture, journal, rollback, and reconciliation live
//! there, behind ports that crate defines; this crate carries no source-edit
//! runtime state.
//! - [`tracedecay_configuration::PinnedRuntimeConfigurationCachePort`], installed
//!   via [`tracedecay_configuration::install_pinned_runtime_configuration_cache`]
//!   by the composition root, which owns opening durable configuration.
//!   Configuration
//!   value/persistence contracts live in `tracedecay-configuration` (re-exported
//!   from `tracedecay_global_db::configuration::contracts`), not duplicated here.
//!   [`config::retrieval`] stays in this crate because it is production-load-bearing
//!   on search-eval.
//! - Transport-independent response handles live in
//!   `tracedecay_session_memory::response_handles`; MCP adapters should call
//!   that module rather than keep a parallel handle store.
//!
//! ## Packaging
//!
//! `publish = false`. `semantic_runtime` reaches search-quality fixtures via
//! `include_str!` outside this package's root (repo-root `tests/fixtures/`);
//! workspace builds resolve it, but a standalone package build would not.

/// Installs the registered global/session schema into the kernel's fail-closed
/// port for this crate's test process.
///
/// `Database::publish_test_runtime` materialises a profile-scoped sidecar shard
/// that the kernel initialises through
/// `tracedecay_runtime_core::ports::registered_schema`. That port fails closed
/// until the real schema — owned by `tracedecay-global-db` — is registered.
/// Production wires it from the daemon composition root; this crate's test
/// target reuses the identical installer through its `test-helpers`
/// dev-dependency. Idempotent: the port keeps the first registration, so every
/// fixture entry point can call it unconditionally.
///
/// Fixtures built on `tracedecay_global_db::tests::harness` register the
/// installer themselves; only fixtures that reach `publish_test_runtime`
/// directly need this call.
#[cfg(test)]
pub(crate) fn register_test_schema_installer() {
    tracedecay_global_db::register_registered_schema_installer();
}

pub mod advisory;
pub mod code_index;
pub mod config;
pub mod dashboard_diagnostics;
pub mod delivery;
pub mod diagnose;
pub mod diagnostics_publication;
pub mod diagnostics_query;
pub mod diagnostics_store;
// Public because `tracedecay-global-db` reaches the runtime external-source
// store through the root shim.
pub mod analytics_bridge;
pub mod feedback;
pub mod git_intelligence;
pub mod git_query;
pub mod git_reads;
pub mod graph_health_delta;
mod hotpath_observe;
pub mod lsp_runtime;
mod lsp_support;
pub mod native_integration;
pub mod observability;
pub mod operation_stream;
pub mod pr_tracking;
pub mod primitives;
pub mod project_adoption;
pub mod project_open_authorization;
pub mod semantic_runtime;
pub mod settings_control;
pub mod source_authorization;
pub mod stack_coordinator;
pub mod store;
pub mod tracedecay;
pub mod work;

pub use lsp_support::analyzer_runtime_config_error;
pub use source_authorization::{
    CallableCodeAuthorizationSourcePort, CurrentCallableCodeAccessFuture,
    ProjectSourceAccessDenial, ProjectSourceAccessOutcome, ProjectSourceAccessSnapshot,
    ProjectSourceAccessSnapshotPort, project_source_access_snapshot_for_request,
};
