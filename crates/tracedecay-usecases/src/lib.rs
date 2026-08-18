//! Typed product use cases shared by CLI, MCP, HTTP, hooks, and daemon adapters.
//!
//! This is the top-of-stack use-case orchestration layer — what the root
//! binary's `src/application/` tree became. `tracedecay-application` is a
//! different, bottom-of-stack ports-and-contracts crate that only shares the
//! word; see that crate's `lib.rs` for why the split-era plan briefly
//! conflated them. This crate depends on `tracedecay-application` (never the
//! reverse) along with `tracedecay-runtime-core`, `tracedecay-sessions`,
//! `tracedecay-global-db`, `tracedecay-semantic`, and 14 other workspace
//! crates; every edge is proven acyclic with `cargo tree -e normal -p <dep> |
//! grep tracedecay-usecases` returning no matches. It deliberately does not
//! depend on `tracedecay-agent-hosts`, `tracedecay-dashboard-api`, or the
//! root binary crate — `tracedecay-dashboard-api` depends on this crate, so
//! an edge back would be a Cargo cycle, and root-owned seams (`mcp`, `daemon`,
//! `hooks`, `automation`, `dashboard`) must be resolved by port inversion,
//! never by adding a dependency here.
//!
//! ## Composition-root ports
//!
//! Graph reads now go through the Grafeo-backed
//! `tracedecay_global_db::VerifiedGraphRuntimePortV1` (defined in
//! `tracedecay-runtime-core::store_runtime::verified_graph`), not a port
//! owned by this crate — that landed after the one-shot crate split, when the
//! SQLite graph authority was replaced by the embedded Grafeo runtime.
//! Source-edit preview/apply still route through this crate's own task-local
//! plan authority: [`tracedecay::capture_source_edit_plan`],
//! [`tracedecay::apply_source_edit_plan`], and
//! [`tracedecay::capture_planned_source_edit`]. Callers should use these
//! rather than create a second root-owned plan.
//! - [`config::RuntimeConfigurationAuthorityPort`], installed via
//!   [`config::install_runtime_configuration_authority`] before opening any
//!   configuration-backed use case. Configuration value/persistence contracts
//!   are re-exported from `tracedecay_global_db::configuration::contracts`
//!   through [`configuration::ports`]/[`configuration::types`], not
//!   duplicated here.
//! - Transport-independent response handles live in
//!   [`response_handles`]; MCP adapters should call that module rather than
//!   keep a parallel handle store.
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
    tracedecay_global_db::register_test_schema_installer();
}

pub mod advisory;
pub mod anchor_resolution;
pub mod code_index;
// Moved down from the root binary's `src/config/`; see `config/mod.rs`.
pub mod config;
pub mod configuration;
pub mod context;
pub mod dashboard_diagnostics;
// Moved down from the root binary (`src/diagnose.rs`,
// `src/diagnostics_store.rs`, `src/diagnostics_publication.rs`,
// `src/diagnostics_query.rs`): their whole closure is the runtime kernel plus
// `tracedecay-domain`/`tracedecay-store`.
pub mod delivery;
pub mod diagnose;
pub mod diagnostics_publication;
pub mod diagnostics_query;
pub mod diagnostics_store;
pub mod edit;
// Widened from `pub(crate)`: the root shim re-exports this crate, and root
// adapters (`src/mcp`, `src/daemon`, `src/store`) publish onto the event lane.
pub mod event_lane;
// Widened from `pub(crate)`: `tracedecay-global-db` reaches the runtime
// external-source store through the root shim.
pub mod external_source_store;
pub mod feedback;
// Moved down from the root binary. `analytics_bridge` kept only its durable
// hook-JSONL importer; `git_intelligence`/`git_query` are the native git
// adapter and its read engine; `graph`/`retention`/`request_identity`/
// `user_config` had kernel-only closures.
pub mod analytics_bridge;
pub mod git_intelligence;
pub mod git_query;
pub mod git_reads;
pub mod graph;
pub mod host_admission;
pub mod lsp_runtime;
mod lsp_support;
pub mod memory;
pub mod native_integration;
pub mod observability;
pub mod observation;
pub mod operation_stream;
pub mod primitives;
pub mod provider_pricing;
pub mod provider_usage;
pub mod request_identity;
pub mod response_handles;
pub mod retention;
pub mod semantic_runtime;
pub mod session;
pub mod settings_control;
pub mod source_authorization;
pub mod stack_coordinator;
pub mod store;
pub mod tracedecay;
pub mod user_config;

pub use source_authorization::{
    ProjectSourceAccessDenial, ProjectSourceAccessOutcome, ProjectSourceAccessSnapshot,
    project_source_access_snapshot_for_request,
};
