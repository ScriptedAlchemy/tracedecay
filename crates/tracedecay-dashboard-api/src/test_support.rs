//! Fixtures for states built without a project runtime.

use std::path::Path;

use tracedecay_automation_runtime::automation::host_io::{
    HostIo, ManagedSkillExportReport, PluginFile,
};
use tracedecay_domain::errors::Result;

/// Installs the registered global/session schema into the kernel's fail-closed
/// port for this crate's test process.
///
/// `Database::publish_test_runtime` materialises a profile-scoped sidecar shard
/// that the kernel initialises through
/// `tracedecay_runtime_core::ports::registered_schema`. That port fails closed
/// until the real schema, owned by `tracedecay-global-db`, is registered.
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

/// A host I/O bundle with no agent hosts behind it: writes land on disk
/// plainly, export sweeps report nothing, and the managed-agent bundle is
/// empty.
pub(crate) fn fixture_host_io() -> HostIo {
    fn export_to_agents(_: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        Vec::new()
    }

    fn export_to_agent_hosts(_: &Path, _: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        Vec::new()
    }

    fn write_text(path: &Path, contents: &str) -> Result<()> {
        Ok(std::fs::write(path, contents)?)
    }

    fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
        Ok(std::fs::write(path, serde_json::to_vec_pretty(value)?)?)
    }

    fn remove_host_file(path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }

    fn codex_agent_files() -> &'static [PluginFile] {
        &[]
    }

    HostIo {
        export_to_agents,
        export_to_agent_hosts,
        write_text,
        write_json,
        remove_host_file,
        codex_agent_files,
    }
}
