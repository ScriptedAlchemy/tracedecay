//! Agent host integrations (`agents`) for TraceDecay.
//!
//! Self-improvement automation lives in `tracedecay-automation-runtime`.
//! Host installers call that crate for skill-target installation and hand it
//! the [`host_io`] bundle it writes host-owned files through.
//!
//! ## Registered ports
//!
//! A handful of root-owned runtimes cannot become a dependency edge (Hook
//! daemon/identity composition, the Codex app-server backend, and the
//! registered database's canonical project key). Those are
//! [`crate::ports`] slots the root fills at startup (`src/runtime_ports.rs`).
//! The MCP tool catalog is no longer among them: `tracedecay-mcp` owns it and
//! sits below this crate, so installers read it directly and an unavailable
//! catalog is a typed error rather than an empty allowlist.
//!
//! A crate-local `cargo check` passing is not evidence the production
//! composition root is wired — check the registration call sites too.
//!
//! ## Packaging
//!
//! `publish = false`. Several `include_str!`/`include_bytes!` sites (plugin
//! bundle generation, the Hermes dashboard wrapper, packaged-host-event and
//! transcript-golden fixtures) reach up through `../../../../…` into
//! repository-root `plugin/`, `dashboard/hermes-wrapper/`, and
//! `tests/fixtures/`, outside this crate's package root. `cargo package
//! --list` succeeds because it only enumerates this crate's own tracked
//! files; a real standalone package build would still fail to resolve those
//! includes. Accepted as-is because the crate is workspace-internal only.

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

use tracedecay_automation_runtime::automation::host_io::HostIo;

pub mod agents;
pub mod hooks;
pub mod native_integration;
pub mod ports;
pub mod product_version;
pub mod shell;
pub mod task_classifier;
pub mod tool_name;

pub use product_version::PRODUCT_VERSION;

/// The host-install surface automation borrows from this crate.
///
/// One `Copy` bundle of the managed-skill export sweeps, host-config writes,
/// and plugin-bundle files that `tracedecay-automation-runtime` cannot depend
/// on directly. Installers pass it to every automation entry point that
/// writes host-owned files; nothing is registered process-wide. The report
/// and plugin-file types are the runtime's own, so the production functions
/// are handed over directly.
pub fn host_io() -> HostIo {
    HostIo {
        export_to_agents: crate::agents::export_managed_skills_to_agents,
        export_to_agent_hosts: crate::agents::export_managed_skills_to_agent_hosts,
        write_text: crate::agents::safe_write_text_file,
        write_json: crate::agents::safe_write_json_file,
        remove_host_file: crate::agents::safe_remove_host_file,
        codex_agent_files: crate::agents::plugin_bundle::codex_agent_files,
    }
}
