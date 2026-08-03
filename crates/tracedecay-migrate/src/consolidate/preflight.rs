use std::fs::{self, File, OpenOptions};

use fs2::FileExt;

use super::*;

pub(super) struct StoreLocks {
    files: Vec<File>,
}

impl Drop for StoreLocks {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = FileExt::unlock(file);
        }
    }
}

pub(super) fn ensure_profile_offline(
    options: &ConsolidationOptions,
    daemon_reachable: bool,
) -> Result<()> {
    if crate::config::user_data_dir().is_some_and(|root| same_path(&root, &options.profile_root))
        && daemon_reachable
    {
        return Err(config_error(
            "profile shard consolidation is offline-only, including its dry-run; stop the TraceDecay daemon and all MCP/CLI writers, then retry",
        ));
    }
    Ok(())
}

#[cfg(not(test))]
pub(super) fn ensure_no_open_store_holders(database_paths: &[PathBuf]) -> Result<()> {
    evaluate_holder_scan(crate::open_store_holders::scan(database_paths).map_err(io_error)?)
}

#[cfg(test)]
pub(super) fn ensure_no_open_store_holders(_database_paths: &[PathBuf]) -> Result<()> {
    // Unit tests share the host with unrelated processes whose /proc entries
    // may be unreadable. Keep production discovery fail-closed and exercise
    // its result handling deterministically below.
    evaluate_holder_scan(crate::open_store_holders::OpenStoreHolderScan::Supported(
        Vec::new(),
    ))
}

fn evaluate_holder_scan(scan: crate::open_store_holders::OpenStoreHolderScan) -> Result<()> {
    match scan {
        crate::open_store_holders::OpenStoreHolderScan::Supported(holders)
            if holders.is_empty() =>
        {
            Ok(())
        }
        crate::open_store_holders::OpenStoreHolderScan::Supported(holders) => {
            let mut details = String::new();
            for holder in holders {
                let version = holder.version.as_deref().unwrap_or("version unknown");
                let executable = holder.executable.as_deref().map_or_else(
                    || "executable unknown".to_string(),
                    |path| path.display().to_string(),
                );
                let paths = holder
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                details.push_str(&format!(
                    "\n- pid {}: {} [{}; {}]; open: {}",
                    holder.pid,
                    truncate_command(&holder.command),
                    version,
                    executable,
                    paths
                ));
            }
            Err(config_error(format!(
                "profile shard consolidation requires every input store handle to be closed; restart the listed agent hosts and retry (TraceDecay never terminates them automatically):{details}"
            )))
        }
        crate::open_store_holders::OpenStoreHolderScan::Unsupported { reason } => {
            Err(config_error(format!(
                "profile shard consolidation cannot prove every input store handle is closed: {reason}; run consolidation on a host with open-store process discovery"
            )))
        }
    }
}

fn truncate_command(command: &str) -> String {
    const LIMIT: usize = 200;
    let mut chars = command.chars();
    let prefix = chars.by_ref().take(LIMIT).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

pub(super) fn acquire_store_locks(
    source: &StoreLayout,
    target: &StoreLayout,
) -> Result<StoreLocks> {
    let mut paths = vec![
        source.sync_lock_path.clone(),
        source.branch_add_lock_path.clone(),
        target.sync_lock_path.clone(),
        target.branch_add_lock_path.clone(),
    ];
    paths.sort();
    paths.dedup();
    let mut files = Vec::new();
    for path in paths {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(io_error)?;
        file.try_lock_exclusive().map_err(|error| {
            config_error(format!(
                "store is busy at '{}': {error}; stop MCP/CLI writers and retry",
                path.display()
            ))
        })?;
        files.push(file);
    }
    Ok(StoreLocks { files })
}

pub(super) fn preflight_disk_space(resolved: &ResolvedPlan) -> Result<()> {
    const LEDGER_HEADROOM: u64 = 1024 * 1024;
    let source_backup = resolved
        .report
        .backup_root
        .join(&resolved.report.source.project_id);
    let target_backup = resolved
        .report
        .backup_root
        .join(&resolved.report.target.project_id);
    let backup_bytes =
        additional_copy_bytes(&resolved.source_layout.data_root, &source_backup)?.saturating_add(
            additional_copy_bytes(&resolved.target_layout.data_root, &target_backup)?,
        );
    let destination_ceiling = resolved
        .report
        .source
        .bytes
        .saturating_add(resolved.report.target.bytes);
    let existing_destination = if resolved.report.destination_data_root.is_dir() {
        tree_stats(&resolved.report.destination_data_root)?.1
    } else {
        0
    };
    let destination_bytes = destination_ceiling.saturating_sub(existing_destination);
    let required = backup_bytes
        .saturating_add(destination_bytes)
        .saturating_add(LEDGER_HEADROOM);
    let profile_root = resolved
        .report
        .destination_data_root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| config_error("destination shard has no profile root"))?;
    let available = fs2::available_space(profile_root).map_err(io_error)?;
    if required > available {
        return Err(config_error(format!(
            "insufficient profile space before consolidation: required {required} bytes \
             (backups {backup_bytes}, destination upper bound {destination_bytes}, ledger {LEDGER_HEADROOM}; \
             scratch fallback already reserved {}), available {available} at '{}'; no ledger or backup was changed",
            resolved.evidence.sessions.copied_bytes(),
            profile_root.display()
        )));
    }
    Ok(())
}

fn additional_copy_bytes(source: &Path, target: &Path) -> Result<u64> {
    let mut bytes = 0_u64;
    for (relative, path) in relative_file_map(source)? {
        if !target.join(relative).exists() {
            bytes = bytes.saturating_add(fs::metadata(path).map_err(io_error)?.len());
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{evaluate_holder_scan, truncate_command};
    use crate::open_store_holders::OpenStoreHolderScan;

    #[test]
    fn holder_command_is_bounded_on_character_boundaries() {
        let command = "x".repeat(205);
        assert_eq!(truncate_command(&command).len(), 203);
        assert!(truncate_command("trace decay").ends_with("decay"));
    }

    #[test]
    fn unsupported_holder_discovery_never_silently_weakens_offline_safety() {
        let error = evaluate_holder_scan(OpenStoreHolderScan::Unsupported {
            reason: "synthetic unsupported host".to_string(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("cannot prove"), "{error}");
        assert!(
            error.to_string().contains("synthetic unsupported host"),
            "{error}"
        );
    }
}
