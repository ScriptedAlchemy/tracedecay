//! Agent host integrations (`agents`) for TraceDecay.
//!
//! Self-improvement automation lives in `tracedecay-automation-runtime`.
//! Host installers call that crate for skill-target installation; automation
//! calls back through `tracedecay_automation_runtime::automation::host_io`
//! (registered by [`register_automation_host_io`]).
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
    tracedecay_global_db::register_test_schema_installer();
    register_automation_host_io();
}

pub mod agents;
pub mod hooks;
pub mod native_integration;
pub mod ports;
pub mod product_version;
pub mod shell;
pub mod task_classifier;
pub mod tool_name;

/// The automation runtime whose host-io ports [`register_automation_host_io`]
/// binds. Exposed so downstream test processes (including the runtime crate's
/// own unit tests, which compile as a separate crate copy) can reach the
/// registered production surface instead of re-implementing it.
pub use tracedecay_automation_runtime as automation_runtime;

pub use product_version::PRODUCT_VERSION;

// Kernel shims. `tracedecay-runtime-core` owns the substrate these two
// subsystems were extracted alongside; aliasing the kernel modules into this
// crate's root keeps every historical `crate::<module>::…` path in the moved
// code resolving verbatim, exactly as the root crate's `src/<module>.rs` shims
// do on the other side of the split.
pub(crate) use tracedecay_domain::errors;
pub(crate) use tracedecay_runtime_core::{config, db, storage};

/// Registers the host-install surface automation used to call on `agents`.
///
/// Idempotent. The composition root and this crate's test installer both call
/// it so managed-skill export, host-config writes, and plugin-bundle files
/// keep working after the automation split.
pub fn register_automation_host_io() {
    use std::path::{Path, PathBuf};

    use tracedecay_automation_runtime::automation::host_io::{
        HostIoRegistration, ManagedSkillExportReport, PluginFile,
    };

    fn export_to_agents(home: &Path, profile_root: &Path) -> Vec<ManagedSkillExportReport> {
        crate::agents::export_managed_skills_to_agents(home, profile_root)
            .into_iter()
            .map(|report| ManagedSkillExportReport {
                agent: report.agent,
                exports: report.exports,
                error: report.error,
            })
            .collect()
    }

    fn export_to_agent_hosts(
        home: &Path,
        project_root: &Path,
        profile_root: &Path,
    ) -> Vec<ManagedSkillExportReport> {
        crate::agents::export_managed_skills_to_agent_hosts(home, project_root, profile_root)
            .into_iter()
            .map(|report| ManagedSkillExportReport {
                agent: report.agent,
                exports: report.exports,
                error: report.error,
            })
            .collect()
    }

    fn codex_agent_files() -> &'static [PluginFile] {
        use std::sync::OnceLock;
        static FILES: OnceLock<Vec<PluginFile>> = OnceLock::new();
        FILES
            .get_or_init(|| {
                crate::agents::plugin_bundle::codex_agent_files()
                    .iter()
                    .map(|file| PluginFile {
                        relative: file.relative,
                        contents: file.contents,
                    })
                    .collect()
            })
            .as_slice()
    }

    fn with_write_intents(root: PathBuf, effect: &mut dyn FnMut()) {
        crate::agents::with_host_config_write_intents(root, effect);
    }

    tracedecay_automation_runtime::automation::host_io::register(HostIoRegistration {
        export_to_agents,
        export_to_agent_hosts,
        write_text: crate::agents::safe_write_text_file,
        write_json: crate::agents::safe_write_json_file,
        remove_host_file: crate::agents::safe_remove_host_file,
        codex_agent_files,
        with_write_intents,
    });
}
