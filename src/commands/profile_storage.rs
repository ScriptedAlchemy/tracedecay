use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cli::ProfileStorageAction;
use tracedecay::display::format_bytes;

pub(crate) async fn handle_profile_storage_action(
    action: ProfileStorageAction,
    assume_yes: bool,
) -> tracedecay::errors::Result<()> {
    match action {
        ProfileStorageAction::StorageReport {
            profile_root,
            project_id,
            project_root,
            json,
        } => handle_storage_report(profile_root, project_id, project_root, json).await,
        ProfileStorageAction::BackupProfile { to, backup_id } => {
            handle_backup_profile(to, backup_id)
        }
        ProfileStorageAction::RehearseProfileBackup { backup, restore } => {
            handle_rehearse_profile_backup(backup, restore)
        }
        ProfileStorageAction::ResetAuthority { authority, db } => {
            handle_reset_authority(authority, db, assume_yes)
        }
        ProfileStorageAction::ResetProjectStore {
            project_root,
            project_id,
        } => handle_reset_project_store(project_root, project_id, assume_yes),
    }
}

/// Scoped operator recovery for a project graph store whose open failed with
/// the typed `ResetRequired` state (an incompatible `user_version`). Only the
/// refused graph database and its WAL/SHM sidecars are deleted; the store
/// directory, session archive, and provider transcripts are preserved, so the
/// next daemon open recreates the graph at the canonical schema and re-ingests
/// from those durable inputs. A store already at the canonical schema is
/// refused untouched — this command cannot be used to wipe a healthy store.
fn handle_reset_project_store(
    project_root: Option<String>,
    project_id: Option<String>,
    assume_yes: bool,
) -> tracedecay::errors::Result<()> {
    let profile_root = tracedecay::storage::default_profile_root()?;
    let project_id = match (project_root, project_id) {
        (Some(root), None) => {
            let root = PathBuf::from(root);
            let layout = tracedecay::storage::resolve_layout_for_current_profile(&root)?;
            layout.identity.project_id.ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "project root '{}' resolves no authoritative project identity",
                        root.display()
                    ),
                }
            })?
        }
        (None, Some(project_id)) => {
            tracedecay::storage::validate_project_id(&project_id).map_err(|message| {
                tracedecay::errors::TraceDecayError::Config {
                    message: format!("invalid --project-id: {message}"),
                }
            })?;
            project_id
        }
        _ => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "exactly one of --project-root or --project-id is required".to_owned(),
            });
        }
    };
    if !assume_yes {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "resetting the refused project graph store for '{project_id}' deletes its \
                 code graph and project memory facts; sessions re-ingest from the preserved \
                 transcripts at the next open. Re-run with --yes to confirm"
            ),
        });
    }
    let lifecycle_lease = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "reset-project-store",
    )?;
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle_lease,
        &profile_root,
        "reset-project-store",
    )?;
    let outcome = reset_refused_project_graph_store(&profile_root, &project_id)?;
    println!(
        "reset the refused project graph store for '{project_id}' \
         (schema v{} -> fresh v{} at the next open)",
        outcome.previous_schema_version, outcome.canonical_schema_version
    );
    println!("  removed {}", outcome.graph_db_path.display());
    println!(
        "  preserved the store directory, session archive, and provider transcripts \
         under {}",
        outcome.data_root.display()
    );
    println!(
        "run `tracedecay init <project root>` (or any daemon-brokered open) to recreate \
         the graph at the canonical schema; sessions re-ingest from the preserved \
         transcripts"
    );
    Ok(())
}

#[derive(Debug)]
struct ResetProjectGraphStoreOutcome {
    data_root: PathBuf,
    graph_db_path: PathBuf,
    previous_schema_version: i64,
    canonical_schema_version: u32,
}

/// Verifies the project graph store under `profile_root` is genuinely refused
/// — a real SQLite database stamped with a schema version this binary does
/// not create — and deletes exactly that database and its WAL/SHM sidecars.
/// A store already at the canonical schema (or a file that is not a SQLite
/// database) is refused untouched, so this cannot wipe a healthy store.
fn reset_refused_project_graph_store(
    profile_root: &Path,
    project_id: &str,
) -> tracedecay::errors::Result<ResetProjectGraphStoreOutcome> {
    let data_root = tracedecay::storage::profile_sharded_data_root(profile_root, project_id);
    let graph_db_path = data_root.join(tracedecay::config::db_filename(&data_root));
    if !graph_db_path.is_file() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "no project graph store exists at {}; nothing to reset",
                graph_db_path.display()
            ),
        });
    }
    let has_header =
        tracedecay::storage::has_sqlite_database_header(&graph_db_path).map_err(|error| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "could not verify the store header at {}: {error}",
                    graph_db_path.display()
                ),
            }
        })?;
    if !has_header {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "{} is not a SQLite database; the scoped reset covers only stores refused \
                 for an incompatible schema version",
                graph_db_path.display()
            ),
        });
    }
    let connection = rusqlite::Connection::open_with_flags(
        &graph_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| tracedecay::errors::TraceDecayError::Database {
        operation: "open project graph store for reset verification".to_string(),
        message: error.to_string(),
    })?;
    let previous_schema_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| tracedecay::errors::TraceDecayError::Database {
            operation: "read project graph store schema version".to_string(),
            message: error.to_string(),
        })?;
    drop(connection);
    let canonical_schema_version = tracedecay::db::migrations::SCHEMA_VERSION;
    if previous_schema_version == i64::from(canonical_schema_version) {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "the project graph store at {} is already at the canonical schema \
                 v{canonical_schema_version}; it is not refused and nothing was reset",
                graph_db_path.display()
            ),
        });
    }
    for sidecar_suffix in ["", "-wal", "-shm"] {
        let path = if sidecar_suffix.is_empty() {
            graph_db_path.clone()
        } else {
            let mut file_name = graph_db_path.file_name().unwrap_or_default().to_os_string();
            file_name.push(sidecar_suffix);
            graph_db_path.with_file_name(file_name)
        };
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: format!("failed to remove {}: {error}", path.display()),
                });
            }
        }
    }
    Ok(ResetProjectGraphStoreOutcome {
        data_root,
        graph_db_path,
        previous_schema_version,
        canonical_schema_version,
    })
}

/// Scoped operator recovery for a store whose open failed with the typed
/// `ResetRequired` state. The daemon cannot open a refused store, so the
/// reset runs offline under the profile's exclusive maintenance lease; the
/// next daemon open recreates the authority at the canonical schema and its
/// content re-derives from the preserved transcripts.
fn handle_reset_authority(
    authority: String,
    db: Option<String>,
    assume_yes: bool,
) -> tracedecay::errors::Result<()> {
    if authority != tracedecay_global_db::observation::OBSERVATION_AUTHORITY {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "no scoped reset exists for authority '{authority}'; the only \
                 scoped-resettable authority is '{}'",
                tracedecay_global_db::observation::OBSERVATION_AUTHORITY
            ),
        });
    }
    if !assume_yes {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "resetting the '{authority}' authority drops its refused tables and \
                 clears their recoverable derivations; re-run with --yes to confirm"
            ),
        });
    }
    let profile_root = tracedecay::storage::default_profile_root()?;
    let lifecycle_lease = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "reset-authority",
    )?;
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle_lease,
        &profile_root,
        "reset-authority",
    )?;
    let db_path = match db {
        Some(path) => PathBuf::from(path),
        None => tracedecay_sessions::runtime::user_sessions_db_path(&profile_root),
    };
    if !db_path.is_file() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "no sessions store exists at {}; nothing to reset",
                db_path.display()
            ),
        });
    }
    let mut connection = rusqlite::Connection::open(&db_path).map_err(|error| {
        tracedecay::errors::TraceDecayError::Database {
            operation: "open sessions store for authority reset".to_string(),
            message: error.to_string(),
        }
    })?;
    let report =
        tracedecay_global_db::observation::reset_refused_observation_authority(&mut connection)?;
    println!(
        "reset the refused '{authority}' authority in {}",
        db_path.display()
    );
    for table in &report.reset_tables {
        println!("  recreated {table} empty at the canonical schema");
    }
    println!(
        "  cleared {} recoverable session_messages row(s)",
        report.cleared_session_message_rows
    );
    println!(
        "the authority content re-derives from the preserved transcripts at the \
         next daemon open"
    );
    Ok(())
}

async fn brokered_storage_report(
    project_id: Option<&str>,
    project_root: Option<&Path>,
) -> tracedecay::errors::Result<tracedecay::retention::storage_report::StorageReport> {
    const PAGE_LIMIT: usize = 8;
    const MAX_PAGES: usize = 4096;

    let mut report = tracedecay::retention::storage_report::StorageReport::default();
    let mut cursor = None;
    for _ in 0..MAX_PAGES {
        let request = super::daemon::daemon_tool_json(
            None,
            "tracedecay_admin_cli",
            serde_json::json!({
                "action": "storage_report",
                "project_id": project_id,
                "project_root": project_root,
                "cursor": cursor,
                "limit": PAGE_LIMIT,
            }),
        );
        let value = tokio::time::timeout(Duration::from_secs(10), request)
            .await
            .map_err(|_| tracedecay::errors::TraceDecayError::Config {
                message: "daemon storage report authority timed out after 10 seconds".to_string(),
            })??;
        let page: tracedecay::retention::storage_report::StorageReport =
            serde_json::from_value(value)?;
        merge_storage_report_page(&mut report, page);
        if report.coverage.state
            == tracedecay::retention::storage_report::StorageReportCoverageState::Complete
        {
            return Ok(report);
        }
        cursor = report.coverage.next_cursor.clone();
        if cursor.is_none() {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "partial daemon storage report omitted its continuation cursor".to_owned(),
            });
        }
    }
    Ok(report)
}

fn merge_storage_report_page(
    report: &mut tracedecay::retention::storage_report::StorageReport,
    page: tracedecay::retention::storage_report::StorageReport,
) {
    if report.profile_root.is_empty() {
        report.profile_root = page.profile_root;
    }
    report.stores.extend(page.stores);
    report
        .code_generation_retention
        .extend(page.code_generation_retention);
    report
        .code_generation_retention_availability
        .extend(page.code_generation_retention_availability);
    report.unregistered_dir_count = report
        .unregistered_dir_count
        .saturating_add(page.unregistered_dir_count);
    report.unregistered_bytes = report
        .unregistered_bytes
        .saturating_add(page.unregistered_bytes);
    report.global_db_bytes = report.global_db_bytes.max(page.global_db_bytes);
    report.coverage = page.coverage;
}

/// Read-only per-store size / free-page-ratio / unregistered-directory report
/// (plan 38 §7). The active profile routes through the daemon's retained
/// authority; explicit offline profiles retain the bounded read-only path.
async fn handle_storage_report(
    profile_root: Option<String>,
    project_id: Option<String>,
    project_root: Option<String>,
    json: bool,
) -> tracedecay::errors::Result<()> {
    let default_profile_root = tracedecay::storage::default_profile_root()?;
    let profile_root = match profile_root {
        Some(path) => PathBuf::from(path),
        None => default_profile_root.clone(),
    };
    let daemon_owns_profile = profile_root == default_profile_root;
    let project_root = project_root.map(PathBuf::from);
    let report = if daemon_owns_profile && tracedecay::daemon::daemon_reachable() {
        brokered_storage_report(project_id.as_deref(), project_root.as_deref()).await?
    } else {
        let offline = match (&project_id, &project_root) {
            (Some(project_id), Some(project_root)) => {
                // Offline runs have no mounted code graph, so vector liveness
                // is unprovable and the retention dry run reports unavailable.
                tracedecay::retention::storage_report::build_project_storage_report(
                    &profile_root,
                    project_id,
                    project_root,
                    None,
                )
            }
            (None, None) => {
                tracedecay::retention::storage_report::build_storage_report(&profile_root).await
            }
            _ => unreachable!("clap requires project id and root together"),
        };
        offline?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("storage report: {}", report.profile_root);
    let profile_total = report.profile_total_size();
    // A partial total is a floor, not the profile size; say which families are
    // missing rather than printing a number that reads as complete.
    match profile_total.state {
        tracedecay::retention::storage_report::ProfileTotalCoverageStateV1::Complete => {
            println!(
                "  profile total: {} bytes",
                format_bytes(profile_total.accounted_bytes)
            );
        }
        tracedecay::retention::storage_report::ProfileTotalCoverageStateV1::Partial => {
            println!(
                "  profile total: at least {} bytes (incomplete)",
                format_bytes(profile_total.accounted_bytes)
            );
            for family in &profile_total.excluded_families {
                println!("    not sized: {family}");
            }
        }
    }
    println!(
        "  global.db: {} bytes",
        format_bytes(report.global_db_bytes)
    );
    println!("  registered stores: {}", report.stores.len());
    for store in &report.stores {
        // Free pages are unsampled when the store was busy or unreadable; say
        // so rather than printing a zero that reads as "no bloat".
        let free = match (store.free_bytes, store.free_page_ratio) {
            (Some(free_bytes), Some(ratio)) => format!(
                "{} free ({:.1}% free pages)",
                format_bytes(free_bytes),
                ratio * 100.0
            ),
            _ => "free pages not sampled (store busy or unreadable)".to_string(),
        };
        println!(
            "    {} ({}): {} total, {free}",
            store.project_id,
            store.canonical_root,
            format_bytes(store.total_bytes),
        );
    }
    for retention in &report.code_generation_retention {
        println!(
            "  code-index retention dry run for {}: active {} ({}), rollback floor {}",
            retention.project_id,
            retention.active_generation_id,
            retention.active_generation_file,
            retention.rollback_floor
        );
        println!(
            "    superseded: {} generation(s), {} bytes ({})",
            retention.superseded_generation_count,
            retention.superseded_generation_bytes,
            format_bytes(retention.superseded_generation_bytes)
        );
        println!(
            "    would delete: {} generation(s), {} bytes ({})",
            retention.collectable_generation_count,
            retention.collectable_generation_bytes,
            format_bytes(retention.collectable_generation_bytes)
        );
        for generation in &retention.collectable_generations {
            println!(
                "      {}/code-generations-v1/{} ({} bytes, sealed_at_micros={})",
                retention.store_root,
                generation.generation_file,
                generation.size_bytes,
                generation.sealed_at_micros
            );
        }
    }
    for availability in &report.code_generation_retention_availability {
        if availability.state
            == tracedecay::retention::storage_report::StorageReportAvailabilityState::Unavailable
        {
            println!(
                "  code-index retention unavailable for {}: {}",
                availability.project_id,
                availability.reason.as_deref().unwrap_or("unspecified")
            );
        }
    }
    println!(
        "  unregistered directories: {} ({})",
        report.unregistered_dir_count,
        format_bytes(report.unregistered_bytes)
    );
    if report.unregistered_dir_count > 0 {
        println!(
            "  run the daemon's automatic sweep, or `tracedecay tool tracedecay_admin_cli` \
             orphan-store collection, to reclaim unregistered directories"
        );
    }
    if report.coverage.state
        == tracedecay::retention::storage_report::StorageReportCoverageState::Partial
    {
        println!(
            "  coverage: partial; resume with cursor {}",
            report
                .coverage
                .next_cursor
                .as_deref()
                .unwrap_or("<missing>")
        );
    }
    Ok(())
}

fn handle_backup_profile(destination: String, backup_id: String) -> tracedecay::errors::Result<()> {
    let profile_root = tracedecay::storage::default_profile_root()?;
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("system clock is before Unix epoch: {error}"),
        })?
        .as_secs()
        .try_into()
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock exceeds supported backup timestamp range".to_owned(),
        })?;
    let backup = tracedecay::daemon::with_quiesced_installed_service(
        "complete profile backup",
        |lifecycle| {
            tracedecay::profile_backup::create_complete_profile_backup(
                &profile_root,
                Path::new(&destination),
                &backup_id,
                created_at,
                lifecycle,
            )
            .map_err(|error| tracedecay::errors::TraceDecayError::Config {
                message: error.to_string(),
            })
        },
    )?;
    println!(
        "complete profile backup created and verified: {}",
        backup.display()
    );
    Ok(())
}

fn handle_rehearse_profile_backup(
    backup: String,
    restore: String,
) -> tracedecay::errors::Result<()> {
    let manifest = tracedecay::profile_backup::rehearse_complete_profile_backup(
        Path::new(&backup),
        Path::new(&restore),
    )
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: error.to_string(),
    })?;
    println!(
        "complete profile backup rehearsed: {} entries restored to {}",
        manifest.entries.len(),
        restore
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod reset_project_store_tests {
    use super::*;

    fn write_store_with_user_version(
        profile_root: &Path,
        project_id: &str,
        version: u32,
    ) -> PathBuf {
        let data_root = tracedecay::storage::profile_sharded_data_root(profile_root, project_id);
        std::fs::create_dir_all(&data_root).expect("store dir");
        let db_path = data_root.join(tracedecay::config::db_filename(&data_root));
        let connection = rusqlite::Connection::open(&db_path).expect("create store");
        connection
            .execute_batch(&format!(
                "PRAGMA user_version = {version}; CREATE TABLE anchor (id INTEGER);"
            ))
            .expect("stamp store");
        drop(connection);
        db_path
    }

    #[test]
    fn refused_old_schema_store_is_reset_and_transcript_inputs_survive() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let db_path = write_store_with_user_version(&profile_root, "proj_refused_v18", 18);
        // WAL/SHM sidecars and durable transcript inputs share the store dir.
        let wal_path = db_path.with_file_name("tracedecay.db-wal");
        std::fs::write(&wal_path, b"wal").unwrap();
        let data_root = db_path.parent().unwrap().to_path_buf();
        let sessions_path = data_root.join("sessions.db");
        std::fs::write(&sessions_path, b"session archive").unwrap();

        let outcome = reset_refused_project_graph_store(&profile_root, "proj_refused_v18").unwrap();

        assert_eq!(outcome.previous_schema_version, 18);
        assert!(!db_path.exists(), "refused graph database must be removed");
        assert!(!wal_path.exists(), "WAL sidecar must be removed");
        assert!(
            sessions_path.exists(),
            "the session archive is a durable re-ingest input and must survive"
        );
        assert!(
            data_root.exists(),
            "the store directory itself must survive"
        );
    }

    #[test]
    fn canonical_schema_store_is_refused_untouched() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let db_path = write_store_with_user_version(
            &profile_root,
            "proj_canonical",
            tracedecay::db::migrations::SCHEMA_VERSION,
        );

        let error = reset_refused_project_graph_store(&profile_root, "proj_canonical").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("already at the canonical schema"),
            "unexpected refusal: {error}"
        );
        assert!(db_path.exists(), "a healthy store must never be deleted");
    }

    #[test]
    fn non_sqlite_file_is_refused_untouched() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let data_root =
            tracedecay::storage::profile_sharded_data_root(&profile_root, "proj_not_sqlite");
        std::fs::create_dir_all(&data_root).unwrap();
        let db_path = data_root.join(tracedecay::config::db_filename(&data_root));
        std::fs::write(&db_path, b"not a database").unwrap();

        let error =
            reset_refused_project_graph_store(&profile_root, "proj_not_sqlite").unwrap_err();

        assert!(
            error.to_string().contains("is not a SQLite database"),
            "unexpected refusal: {error}"
        );
        assert!(
            db_path.exists(),
            "an unrecognized file must never be deleted"
        );
    }

    #[test]
    fn missing_store_is_a_typed_nothing_to_reset() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");

        let error = reset_refused_project_graph_store(&profile_root, "proj_absent").unwrap_err();

        assert!(
            error.to_string().contains("nothing to reset"),
            "unexpected refusal: {error}"
        );
    }
}
