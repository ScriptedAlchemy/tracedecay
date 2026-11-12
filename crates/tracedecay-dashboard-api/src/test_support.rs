//! Dashboard fixtures and process-isolated runtime checks.

use std::path::Path;

use tracedecay_automation_runtime::automation::host_io::{
    HostIo, ManagedSkillExportReport, PluginFile,
};
use tracedecay_domain::errors::Result;

/// Storage-wait counters belong to the process. Run their measurements alone
/// so another test's fixture setup cannot change the observed delta.
#[allow(clippy::expect_used)]
pub(crate) fn alone_with_storage_wait_counters() -> bool {
    const CHILD: &str = "TRACEDECAY_STORAGE_WAIT_TEST_CHILD";
    let thread = std::thread::current();
    let test = thread.name().expect("libtest names its test thread");
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([test, "--exact", "--test-threads=1", "--nocapture"])
        .env(CHILD, test)
        .output()
        .expect("run the storage-wait test in its own process");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test} failed in its child process:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

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
