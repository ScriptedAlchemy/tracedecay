//! Transactional session-store merge and durable migration ledger.

use tracedecay_application::DirectorySyncPolicy;

use super::*;
use crate::root_seam::global_db::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
use tracedecay_runtime_core::db::engine::Value;
use tracedecay_runtime_core::sqlite_read_snapshot::SnapshotConnection;

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

pub(super) struct MergeSnapshotRequest<'a> {
    pub(super) source: Option<&'a SnapshotConnection>,
    pub(super) source_path: &'a Path,
    pub(super) target_path: &'a Path,
    pub(super) target_project: &'a Path,
    pub(super) target_project_id: &'a str,
    pub(super) fingerprint: &'a str,
    pub(super) source_schema_version: i64,
    pub(super) initial_rows_copied: u64,
    pub(super) fail_after_table: Option<&'a str>,
}

struct CreatedPayloads {
    paths: Vec<PathBuf>,
    armed: bool,
}

impl CreatedPayloads {
    fn armed() -> Self {
        Self {
            paths: Vec::new(),
            armed: true,
        }
    }

    fn paths_mut(&mut self) -> &mut Vec<PathBuf> {
        &mut self.paths
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CreatedPayloads {
    fn drop(&mut self) {
        if self.armed {
            remove_created_payloads(&self.paths);
        }
    }
}

pub(super) async fn merge_snapshot(
    db: &RegisteredGlobalDb,
    request: MergeSnapshotRequest<'_>,
) -> Result<MergeOutcome, String> {
    let source_path = request.source_path;
    let target_path = request.target_path;
    let target_project = request.target_project;
    let target_project_id = request.target_project_id;
    let fingerprint = request.fingerprint;
    let source_schema_version = request.source_schema_version;
    let existing_marker = read_migration_marker(target_path, fingerprint, target_project_id)?;
    let mut created_payloads = CreatedPayloads::armed();
    if let Some(source) = request.source {
        copy_external_payload_files(
            source,
            source_path,
            target_path,
            created_payloads.paths_mut(),
        )
        .await?;
    }
    let repaired_payloads = !created_payloads.paths.is_empty();
    let transaction = db
        .begin_write_transaction()
        .await
        .map_err(|error| format!("could not begin target migration: {error}"))?;
    let mut outcome = merge_snapshot_in_transaction(&request, &transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("could not commit target migration: {error}"))?;
    created_payloads.disarm();
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
    if let Some(marker) = existing_marker
        && outcome.rows_copied == 0
        && !repaired_payloads
    {
        outcome.already_migrated = true;
        outcome.rows_copied = marker.rows_copied;
    }
    Ok(outcome)
}

async fn merge_snapshot_in_transaction(
    request: &MergeSnapshotRequest<'_>,
    target: &RegisteredGlobalDbWriteTransaction<'_>,
) -> Result<MergeOutcome, String> {
    let target_project = request.target_project;
    let fail_after_table = request.fail_after_table;
    let mut rows_copied = request.initial_rows_copied;
    let source = request.source;
    let Some(source) = source else {
        return Ok(MergeOutcome {
            already_migrated: false,
            rows_copied,
        });
    };
    let project = RegisteredGlobalDb::canonical_project_key(target_project);
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

// Windows does not support opening a directory with ordinary `File::open`, so
// the shared implementation is a no-op there and the durable file sync
// performed before the rename is the strongest portable guarantee.
fn sync_directory(dir: &Path) -> Result<(), String> {
    tracedecay_application::sync_directory(dir, DirectorySyncPolicy::Strict)
        .map_err(|error| format!("could not sync migration ledger directory: {error}"))
}

fn fail_after(table: &str, requested: Option<&str>) -> Result<(), String> {
    if requested == Some(table) {
        Err(format!("injected migration failure after {table}"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_removes_armed_payloads() {
        let temp = tempfile::tempdir().unwrap();
        let payload = temp.path().join("copied-payload");
        fs::write(&payload, b"payload").unwrap();
        let task_payload = payload.clone();
        let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut created = CreatedPayloads::armed();
            created.paths_mut().push(task_payload);
            armed_tx.send(()).unwrap();
            std::future::pending::<()>().await;
            created.disarm();
        });

        armed_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!payload.exists());
    }
}
