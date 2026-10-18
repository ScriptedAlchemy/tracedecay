#!/usr/bin/env python3
"""Apply the reviewed scheduler patch to its exact source revision."""
from pathlib import Path
import subprocess

path = Path("crates/tracedecay-automation-runtime/src/automation/scheduler.rs")
expected = "fe48bf99d09aefad536042f0da48cfcc928d406c"
actual = subprocess.check_output(["git", "hash-object", str(path)], text=True).strip()
if actual != expected:
    raise SystemExit(f"scheduler source changed: expected {expected}, found {actual}")
source = path.read_text()
start = source.index('#[tracing::instrument(name = "automation.scheduler.save_control"')
end = source.index("\nimpl AutomationTaskLock {", start)
replacement = '''/// Success acknowledges a complete file and its platform durability barriers.
/// A failure after replacement can leave the complete new state visible; it
/// must remain an error rather than attempting an unsafe rollback.
#[tracing::instrument(name = "automation.scheduler.save_control", level = "trace", skip_all)]
pub async fn save_scheduler_control(
    dashboard_root: &Path,
    control: &AutomationSchedulerControl,
) -> Result<()> {
    let path = scheduler_control_path(dashboard_root);
    let bytes = serde_json::to_vec_pretty(control).map_err(|e| TraceDecayError::Config {
        message: format!("failed to encode automation scheduler control: {e}"),
    })?;
    let write_root = dashboard_root.to_path_buf();
    crate::automation::blocking_fs(move || publish_scheduler_control(&write_root, &bytes))
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to durably publish automation scheduler control '{}': {e}",
                path.display()
            ),
        })
}

fn publish_scheduler_control(dashboard_root: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // Resolve platform aliases such as macOS /var before private-store path
    // validation. Relative callers retain their ordinary working-directory base.
    let absolute = std::path::absolute(dashboard_root)?;
    let directory =
        tracedecay_runtime_core::path_safety::canonicalize_path_or_existing_parent(&absolute);
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(&directory)?;
    let destination = scheduler_control_path(&directory);
    tracedecay_private_fs::framed_log::with_owned_temp_publish(
        &destination,
        "scheduler-control",
        |temporary, destination| {
            tracedecay_private_fs::framed_log::set_owner_private_file_mode(temporary)?;
            tracedecay_runtime_core::db::DatabaseAuthority::replace_file_atomically(
                temporary,
                destination,
                "automation scheduler control",
            )
            .map_err(std::io::Error::other)
        },
        |output| output.write_all(bytes),
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
    )
}
'''
source = source[:start] + replacement + source[end:]
source = source.rstrip() + '''

#[cfg(test)]
#[path = "scheduler_control_tests.rs"]
mod control_publication_tests;
'''
path.write_text(source)
