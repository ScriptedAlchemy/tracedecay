//! Transactional session-store merge and durable migration ledger.

use super::*;

pub(super) struct MergeOutcome {
    pub(super) already_migrated: bool,
    pub(super) rows_copied: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct MigrationMarker {
    schema_version: u32,
    source_fingerprint: String,
    source_db_path: PathBuf,
    target_project_path: PathBuf,
    #[serde(default)]
    target_project_id: String,
    #[serde(default)]
    target_db_path: PathBuf,
    source_lcm_schema_version: i64,
    rows_copied: u64,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn merge_snapshot(
    source: Option<&Connection>,
    source_path: &Path,
    target: &Connection,
    target_path: &Path,
    target_project: &Path,
    target_project_id: &str,
    fingerprint: &str,
    source_schema_version: i64,
    initial_rows_copied: u64,
    fail_after_table: Option<&str>,
) -> Result<MergeOutcome, String> {
    let existing_marker = read_migration_marker(target_path, fingerprint, target_project_id)?;
    target
        .execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|error| format!("could not begin target migration: {error}"))?;
    let mut created_payloads = Vec::new();
    let result = merge_snapshot_in_transaction(
        source,
        source_path,
        target,
        target_path,
        target_project,
        initial_rows_copied,
        fail_after_table,
        &mut created_payloads,
    )
    .await;
    match result {
        Ok(mut outcome) => {
            let repaired_payloads = !created_payloads.is_empty();
            if let Err(error) = target.execute("COMMIT", ()).await {
                let _ = target.execute("ROLLBACK", ()).await;
                remove_created_payloads(&created_payloads);
                return Err(format!("could not commit target migration: {error}"));
            }
            let prior_rows_copied = existing_marker
                .as_ref()
                .map_or(0, |marker| marker.rows_copied);
            write_migration_marker(
                target_path,
                &MigrationMarker {
                    schema_version: 2,
                    source_fingerprint: fingerprint.to_string(),
                    source_db_path: source_path.to_path_buf(),
                    target_project_path: target_project.to_path_buf(),
                    target_project_id: target_project_id.to_string(),
                    target_db_path: canonical_marker_target_path(target_path)?,
                    source_lcm_schema_version: source_schema_version,
                    rows_copied: prior_rows_copied.saturating_add(outcome.rows_copied),
                },
            )?;
            if let Some(marker) = existing_marker {
                if outcome.rows_copied == 0 && !repaired_payloads {
                    outcome.already_migrated = true;
                    outcome.rows_copied = marker.rows_copied;
                }
            }
            Ok(outcome)
        }
        Err(error) => {
            let _ = target.execute("ROLLBACK", ()).await;
            remove_created_payloads(&created_payloads);
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn merge_snapshot_in_transaction(
    source: Option<&Connection>,
    source_path: &Path,
    target: &Connection,
    target_path: &Path,
    target_project: &Path,
    initial_rows_copied: u64,
    fail_after_table: Option<&str>,
    created_payloads: &mut Vec<PathBuf>,
) -> Result<MergeOutcome, String> {
    let mut rows_copied = initial_rows_copied;
    let Some(source) = source else {
        return Ok(MergeOutcome {
            already_migrated: false,
            rows_copied,
        });
    };
    copy_external_payload_files(source, source_path, target_path, created_payloads).await?;
    let project = canonical_project_key(target_project);
    rows_copied += copy_table(source, target, "sessions", &[], |columns, values| {
        for (column, value) in columns.iter().zip(values.iter_mut()) {
            if column == "project_path" || column == "project_key" {
                *value = Value::Text(project.clone());
            }
        }
        Ok(())
    })
    .await?;
    fail_after("sessions", fail_after_table)?;

    rows_copied += copy_table(source, target, "session_messages", &[], |_, _| Ok(())).await?;
    fail_after("session_messages", fail_after_table)?;

    rows_copied += copy_table(source, target, "lcm_external_payloads", &[], |_, _| Ok(())).await?;
    fail_after("lcm_external_payloads", fail_after_table)?;

    let (raw_rows, raw_id_map) = copy_raw_messages(source, target).await?;
    rows_copied += raw_rows;
    fail_after("lcm_raw_messages", fail_after_table)?;

    rows_copied += copy_table(source, target, "lcm_summary_nodes", &[], |_, _| Ok(())).await?;
    fail_after("lcm_summary_nodes", fail_after_table)?;

    rows_copied += copy_table(
        source,
        target,
        "lcm_summary_sources",
        &[],
        |columns, values| remap_summary_source(columns, values, &raw_id_map),
    )
    .await?;
    fail_after("lcm_summary_sources", fail_after_table)?;

    rows_copied += copy_table(
        source,
        target,
        "lcm_lifecycle_state",
        &[],
        |columns, values| {
            remap_store_id_columns(
                columns,
                values,
                &raw_id_map,
                &[
                    "current_frontier_store_id",
                    "last_finalized_frontier_store_id",
                ],
            )
        },
    )
    .await?;
    fail_after("lcm_lifecycle_state", fail_after_table)?;

    rows_copied += copy_table(
        source,
        target,
        "lcm_maintenance_debt",
        &[],
        |columns, values| {
            remap_store_id_columns(
                columns,
                values,
                &raw_id_map,
                &["from_store_id", "to_store_id"],
            )
        },
    )
    .await?;
    fail_after("lcm_maintenance_debt", fail_after_table)?;

    Ok(MergeOutcome {
        already_migrated: false,
        rows_copied,
    })
}

fn marker_path(target_db_path: &Path, fingerprint: &str) -> Result<PathBuf, String> {
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid legacy-store fingerprint".to_string());
    }
    let data_root = target_db_path
        .parent()
        .ok_or_else(|| "target session DB has no parent directory".to_string())?;
    Ok(data_root
        .join(LEDGER_DIR)
        .join(format!("{fingerprint}.json")))
}

fn read_migration_marker(
    target_db_path: &Path,
    fingerprint: &str,
    target_project_id: &str,
) -> Result<Option<MigrationMarker>, String> {
    let path = marker_path(target_db_path, fingerprint)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not read migration marker '{}': {error}",
                path.display()
            ));
        }
    };
    let marker: MigrationMarker = serde_json::from_slice(&bytes)
        .map_err(|error| format!("migration marker '{}' is invalid: {error}", path.display()))?;
    if marker.source_fingerprint != fingerprint || !matches!(marker.schema_version, 1 | 2) {
        return Err(format!(
            "migration marker '{}' has an unsupported identity",
            path.display()
        ));
    }
    if marker.schema_version == 2 {
        let expected_target = canonical_marker_target_path(target_db_path)?;
        if marker.target_project_id != target_project_id || marker.target_db_path != expected_target
        {
            return Err(format!(
                "migration marker '{}' targets a different project store",
                path.display()
            ));
        }
    }
    Ok(Some(marker))
}

fn canonical_marker_target_path(target_db_path: &Path) -> Result<PathBuf, String> {
    target_db_path.canonicalize().map_err(|error| {
        format!(
            "could not canonicalize target session DB '{}': {error}",
            target_db_path.display()
        )
    })
}

fn write_migration_marker(target_db_path: &Path, marker: &MigrationMarker) -> Result<(), String> {
    let mut marker = marker.clone();
    let path = marker_path(target_db_path, &marker.source_fingerprint)?;
    let dir = path
        .parent()
        .ok_or_else(|| "migration marker has no parent directory".to_string())?;
    fs::create_dir_all(dir)
        .map_err(|error| format!("could not create migration ledger directory: {error}"))?;
    let metadata = fs::symlink_metadata(dir)
        .map_err(|error| format!("could not inspect migration ledger directory: {error}"))?;
    if !metadata.file_type().is_dir() {
        return Err("migration ledger path is not a regular directory".to_string());
    }
    let replace_existing_marker = if path.exists() {
        let existing = read_migration_marker(
            target_db_path,
            &marker.source_fingerprint,
            &marker.target_project_id,
        )?
        .ok_or_else(|| "migration marker disappeared during validation".to_string())?;
        marker.rows_copied = marker.rows_copied.max(existing.rows_copied);
        if existing == marker {
            return Ok(());
        }
        true
    } else {
        false
    };
    let bytes = serde_json::to_vec_pretty(&marker)
        .map_err(|error| format!("could not encode migration marker: {error}"))?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        marker.source_fingerprint,
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| format!("could not create migration marker: {error}"))?;
    let persisted = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not persist migration marker: {error}"));
    drop(file);
    if let Err(error) = persisted {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if replace_existing_marker {
        replace_migration_marker(&temp, &path)?;
    } else {
        match fs::hard_link(&temp, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                read_migration_marker(
                    target_db_path,
                    &marker.source_fingerprint,
                    &marker.target_project_id,
                )?;
            }
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(format!("could not publish migration marker: {error}"));
            }
        }
    }
    let _ = fs::remove_file(&temp);
    sync_directory(dir)?;
    Ok(())
}

#[cfg(unix)]
fn replace_migration_marker(temp: &Path, path: &Path) -> Result<(), String> {
    fs::rename(temp, path).map_err(|error| format!("could not upgrade migration marker: {error}"))
}

#[cfg(not(unix))]
fn replace_migration_marker(temp: &Path, path: &Path) -> Result<(), String> {
    fs::remove_file(path)
        .map_err(|error| format!("could not retire legacy migration marker: {error}"))?;
    fs::rename(temp, path).map_err(|error| format!("could not upgrade migration marker: {error}"))
}

#[cfg(unix)]
fn sync_directory(dir: &Path) -> Result<(), String> {
    fs::File::open(dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("could not sync migration ledger directory: {error}"))
}

// Windows does not support opening a directory with ordinary `File::open`,
// so the durable file sync above is the strongest portable guarantee there.
#[cfg(not(unix))]
fn sync_directory(_dir: &Path) -> Result<(), String> {
    Ok(())
}

fn fail_after(table: &str, requested: Option<&str>) -> Result<(), String> {
    if requested == Some(table) {
        Err(format!("injected migration failure after {table}"))
    } else {
        Ok(())
    }
}
