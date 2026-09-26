use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::ProfileStorageAction;
use tracedecay_global_db::profile_registry_maintenance::remove_store_directory;
use tracedecay_runtime_core::lifecycle_lease::{
    ExclusiveLeaseAttempt, try_acquire_exclusive_for_profile,
};
use tracedecay_runtime_core::text::format_bytes;

#[hotpath::measure(label = "cli.profile_storage.dispatch", future = true)]
pub(crate) async fn handle_profile_storage_action(
    action: ProfileStorageAction,
    assume_yes: bool,
) -> tracedecay_domain::errors::Result<()> {
    match action {
        ProfileStorageAction::StorageReport {
            profile_root,
            project_id,
            project_root,
            json,
        } => handle_storage_report(profile_root, project_id, project_root, json).await,
        ProfileStorageAction::ResetProjectStore {
            project_root,
            project_id,
        } => handle_reset_project_store(project_root, project_id, assume_yes),
    }
}

/// Scoped operator recovery for a project graph store whose open failed with
/// the typed `ResetRequired` state (an incompatible version or shape). Only the
/// refused graph database and its WAL/SHM sidecars are deleted; the store
/// directory, session archive, and provider transcripts are preserved, so the
/// next daemon open recreates the graph at the canonical schema and re-ingests
/// from those durable inputs. A store already at the canonical schema is
/// refused untouched, this command cannot be used to wipe a healthy store.
/// With `--project-root`, the checkout's retired `.tracedecay/` layout is
/// deleted too, after the store verification succeeds.
fn handle_reset_project_store(
    project_root: Option<String>,
    project_id: Option<String>,
    assume_yes: bool,
) -> tracedecay_domain::errors::Result<()> {
    let profile_root = tracedecay_runtime_core::storage::default_profile_root()?;
    reset_project_store(&profile_root, project_root, project_id, assume_yes)
}

fn reset_project_store(
    profile_root: &Path,
    project_root: Option<String>,
    project_id: Option<String>,
    assume_yes: bool,
) -> tracedecay_domain::errors::Result<()> {
    let (project_id, retired_checkout) = match (project_root, project_id) {
        (Some(root), None) => {
            let root = PathBuf::from(root);
            let layout =
                tracedecay_runtime_core::storage::resolve_layout_for_current_profile(&root)?;
            let project_id = layout.identity.project_id.ok_or_else(|| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "project root '{}' resolves no authoritative project identity",
                        root.display()
                    ),
                }
            })?;
            (
                project_id,
                tracedecay_runtime_core::storage::retired_checkout_layout_dir(&profile_root, &root),
            )
        }
        (None, Some(project_id)) => {
            tracedecay_runtime_core::storage::validate_project_id(&project_id).map_err(
                |message| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("invalid --project-id: {message}"),
                },
            )?;
            (project_id, None)
        }
        _ => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "exactly one of --project-root or --project-id is required".to_owned(),
            });
        }
    };
    if !assume_yes {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "resetting the refused project graph store for '{project_id}' deletes its \
                 code graph and project memory facts; sessions re-ingest from the preserved \
                 transcripts at the next open. Re-run with --yes to confirm"
            ),
        });
    }
    let lifecycle_lease =
        match try_acquire_exclusive_for_profile(profile_root, "reset-project-store")? {
            ExclusiveLeaseAttempt::Acquired(lease) => lease,
            ExclusiveLeaseAttempt::Busy {
                owner_operation: Some(owner),
            } => {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "cannot reset the project store for '{project_id}' while {owner} is \
                         active; retry after it finishes"
                    ),
                });
            }
            // The daemon holds a shared lease for its whole lifetime and owns every
            // store handle, so the reset can only run with it stopped.
            ExclusiveLeaseAttempt::Busy {
                owner_operation: None,
            } => {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "cannot reset the project store for '{project_id}' while the TraceDecay \
                         daemon holds the profile; run `tracedecay daemon stop`, re-run this \
                         command, then `tracedecay daemon start`"
                    ),
                });
            }
        };
    let _database_scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
        &lifecycle_lease,
        profile_root,
        "reset-project-store",
    )?;
    let outcome =
        reset_refused_project_graph_store(profile_root, &project_id, retired_checkout.is_some())?;
    if let Some(dir) = &retired_checkout {
        remove_store_directory(dir)?;
        println!(
            "removed the retired checkout-local layout {}",
            dir.display()
        );
    }
    if let Some(reset_graph_db) = &outcome.reset_graph_db {
        println!(
            "reset the refused graph database in the project store for '{project_id}' \
             (fresh v{} at the next open)",
            outcome.canonical_schema_version
        );
        println!(
            "  removed {} (was schema v{})",
            reset_graph_db.path.display(),
            reset_graph_db.previous_schema_version
        );
    }
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
    reset_graph_db: Option<ResetGraphDb>,
    canonical_schema_version: u32,
}

#[derive(Debug)]
struct ResetGraphDb {
    path: PathBuf,
    previous_schema_version: i64,
}

/// Verifies one graph database through the daemon's canonical version and
/// exact-shape authorities. A file that is not a SQLite database is a typed
/// error, never a deletion candidate.
fn verified_graph_db_schema(
    graph_db_path: &Path,
) -> tracedecay_domain::errors::Result<(i64, bool)> {
    let has_header = tracedecay_runtime_core::storage::has_sqlite_database_header(graph_db_path)
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "could not verify the store header at {}: {error}",
                graph_db_path.display()
            ),
        })?;
    if !has_header {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "{} is not a SQLite database; the scoped reset covers only stores refused \
                 for an incompatible schema version",
                graph_db_path.display()
            ),
        });
    }
    let connection = rusqlite::Connection::open_with_flags(
        graph_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(
        |error| tracedecay_domain::errors::TraceDecayError::Database {
            operation: "open project graph store for reset verification".to_string(),
            message: error.to_string(),
        },
    )?;
    let schema_version = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(
            |error| tracedecay_domain::errors::TraceDecayError::Database {
                operation: "read project graph store schema version".to_string(),
                message: error.to_string(),
            },
        )?;
    let exact_final_shape =
        if schema_version != i64::from(tracedecay_runtime_core::db::migrations::SCHEMA_VERSION) {
            false
        } else {
            match tracedecay_runtime_core::db::migrations::verify_admissible_final_shape_rusqlite(
                &connection,
            ) {
                Ok(()) => true,
                Err(tracedecay_domain::errors::TraceDecayError::ResetRequired { .. }) => false,
                Err(error) => return Err(error),
            }
        };
    Ok((schema_version, exact_final_shape))
}

/// Verifies the project graph database under `profile_root` and deletes it
/// with its WAL/SHM sidecars only when refused (a real SQLite database whose
/// version or exact relational shape this binary does not accept). A database
/// already at the canonical schema and exact shape is a typed error, so this
/// cannot wipe a healthy store. When `other_reset_pending` names another
/// refused shape the caller resets, nothing refused here is not an error.
fn reset_refused_project_graph_store(
    profile_root: &Path,
    project_id: &str,
    other_reset_pending: bool,
) -> tracedecay_domain::errors::Result<ResetProjectGraphStoreOutcome> {
    let data_root =
        tracedecay_runtime_core::storage::profile_sharded_data_root(profile_root, project_id);
    let graph_db_path = data_root.join(tracedecay_runtime_core::config::DB_FILENAME);
    let canonical_schema_version = tracedecay_runtime_core::db::migrations::SCHEMA_VERSION;
    let nothing_refused = ResetProjectGraphStoreOutcome {
        data_root: data_root.clone(),
        reset_graph_db: None,
        canonical_schema_version,
    };
    if !graph_db_path.is_file() && other_reset_pending {
        return Ok(nothing_refused);
    }
    if !graph_db_path.is_file() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "no project graph store exists at {}; nothing to reset",
                graph_db_path.display()
            ),
        });
    }
    let (previous_schema_version, exact_final_shape) = verified_graph_db_schema(&graph_db_path)?;
    if exact_final_shape && other_reset_pending {
        return Ok(nothing_refused);
    }
    if exact_final_shape {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "the graph database in the project store at {} is already at the \
                 canonical schema v{canonical_schema_version}; nothing is refused and \
                 nothing was reset",
                data_root.display()
            ),
        });
    }
    for sidecar_suffix in ["", "-wal", "-shm"] {
        let mut path = graph_db_path.clone().into_os_string();
        path.push(sidecar_suffix);
        let path = PathBuf::from(path);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("failed to remove {}: {error}", path.display()),
                });
            }
        }
    }
    Ok(ResetProjectGraphStoreOutcome {
        data_root,
        reset_graph_db: Some(ResetGraphDb {
            path: graph_db_path,
            previous_schema_version,
        }),
        canonical_schema_version,
    })
}

async fn brokered_storage_report(
    project_id: Option<&str>,
    project_root: Option<&Path>,
) -> tracedecay_domain::errors::Result<
    tracedecay_maintenance::retention::storage_report::StorageReport,
> {
    const PAGE_LIMIT: usize = 8;
    const MAX_PAGES: usize = 4096;

    let mut report = tracedecay_maintenance::retention::storage_report::StorageReport::default();
    let mut cursor = None;
    for _ in 0..MAX_PAGES {
        // One entry per page: the call count exposes how many pages a report
        // walked, which is what makes a slow storage report diagnosable.
        let request = hotpath::future!(
            super::daemon::daemon_tool_json(
                None,
                "tracedecay_admin_cli",
                serde_json::json!({
                    "action": "storage_report",
                    "project_id": project_id,
                    "project_root": project_root,
                    "cursor": cursor,
                    "limit": PAGE_LIMIT,
                }),
            ),
            label = "cli.profile_storage.report_page"
        );
        let value = tokio::time::timeout(Duration::from_secs(10), request)
            .await
            .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon storage report authority timed out after 10 seconds".to_string(),
            })??;
        let page: tracedecay_maintenance::retention::storage_report::StorageReport =
            serde_json::from_value(value)?;
        merge_storage_report_page(&mut report, page);
        if report.coverage.state
            == tracedecay_maintenance::retention::storage_report::StorageReportCoverageState::Complete
        {
            return Ok(report);
        }
        cursor = report.coverage.next_cursor.clone();
        if cursor.is_none() {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "partial daemon storage report omitted its continuation cursor".to_owned(),
            });
        }
    }
    Ok(report)
}

fn merge_storage_report_page(
    report: &mut tracedecay_maintenance::retention::storage_report::StorageReport,
    page: tracedecay_maintenance::retention::storage_report::StorageReport,
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
) -> tracedecay_domain::errors::Result<()> {
    let default_profile_root = tracedecay_runtime_core::storage::default_profile_root()?;
    let profile_root = match profile_root {
        Some(path) => PathBuf::from(path),
        None => default_profile_root.clone(),
    };
    let daemon_owns_profile = profile_root == default_profile_root;
    let project_root = project_root.map(PathBuf::from);
    let report = if daemon_owns_profile && tracedecay_daemon_control::daemon_reachable() {
        brokered_storage_report(project_id.as_deref(), project_root.as_deref()).await?
    } else {
        let offline = match (&project_id, &project_root) {
            (Some(project_id), Some(project_root)) => {
                // Offline runs have no mounted code graph, so vector liveness
                // is unprovable and the retention dry run reports unavailable.
                tracedecay_maintenance::retention::storage_report::build_project_storage_report(
                    &profile_root,
                    project_id,
                    project_root,
                    None,
                )
            }
            (None, None) => {
                tracedecay_maintenance::retention::storage_report::build_storage_report(
                    &profile_root,
                )
                .await
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
        tracedecay_maintenance::retention::storage_report::ProfileTotalCoverageStateV1::Complete => {
            println!(
                "  profile total: {} bytes",
                format_bytes(profile_total.accounted_bytes)
            );
        }
        tracedecay_maintenance::retention::storage_report::ProfileTotalCoverageStateV1::Partial => {
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
            "  code-index retention dry run for {}: active {} ({})",
            retention.project_id,
            retention
                .active_generation_id
                .as_deref()
                .unwrap_or("<unpublished store>"),
            retention
                .active_generation_file
                .as_deref()
                .unwrap_or("<no pointer>"),
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
            == tracedecay_maintenance::retention::storage_report::StorageReportAvailabilityState::Unavailable
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
        == tracedecay_maintenance::retention::storage_report::StorageReportCoverageState::Partial
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod reset_project_store_tests {
    use super::*;

    fn write_graph_db_with_user_version(db_path: &Path, version: u32) {
        std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("db dir");
        let connection = rusqlite::Connection::open(db_path).expect("create store");
        connection
            .execute_batch(&format!(
                "PRAGMA user_version = {version}; CREATE TABLE anchor (id INTEGER);"
            ))
            .expect("stamp store");
        drop(connection);
    }

    fn write_store_with_user_version(
        profile_root: &Path,
        project_id: &str,
        version: u32,
    ) -> PathBuf {
        let data_root =
            tracedecay_runtime_core::storage::profile_sharded_data_root(profile_root, project_id);
        let db_path = data_root.join(tracedecay_runtime_core::config::DB_FILENAME);
        write_graph_db_with_user_version(&db_path, version);
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

        let outcome =
            reset_refused_project_graph_store(&profile_root, "proj_refused_v18", false).unwrap();

        assert_eq!(
            outcome
                .reset_graph_db
                .as_ref()
                .unwrap()
                .previous_schema_version,
            18
        );
        assert_eq!(outcome.reset_graph_db.as_ref().unwrap().path, db_path);
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

    #[tokio::test]
    async fn same_version_incompatible_store_is_reset_while_exact_store_survives() {
        tracedecay::register_runtime_ports().expect("runtime port registration");
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let project_root = temp.path().join("healthy-project");
        std::fs::create_dir_all(&project_root).unwrap();
        let lifecycle = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
            &profile_root,
            "profile storage exact-shape fixture",
        )
        .unwrap();
        let database_scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
            &lifecycle,
            &profile_root,
            "profile storage exact-shape fixture",
        )
        .unwrap();
        let graph = tracedecay_project::project::TraceDecay::init_with_exclusive_maintenance(
            &project_root,
            tracedecay_project::project::TraceDecayOpenOptions {
                profile_root: Some(profile_root.clone()),
                global_db_path: Some(profile_root.join("global.db")),
            },
            &lifecycle,
        )
        .await
        .unwrap();
        let healthy_project_id = graph
            .store_layout()
            .identity
            .project_id
            .clone()
            .expect("profile-sharded project id");
        let healthy_db = graph.db_path();
        drop(graph);
        drop(database_scope);
        drop(lifecycle);

        let incompatible_project_id = "proj_same_version_missing_table";
        let incompatible_root = tracedecay_runtime_core::storage::profile_sharded_data_root(
            &profile_root,
            incompatible_project_id,
        );
        std::fs::create_dir_all(&incompatible_root).unwrap();
        let incompatible_db = incompatible_root.join(tracedecay_runtime_core::config::DB_FILENAME);
        let source = rusqlite::Connection::open_with_flags(
            &healthy_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut destination = rusqlite::Connection::open(&incompatible_db).unwrap();
        rusqlite::backup::Backup::new(&source, &mut destination)
            .unwrap()
            .run_to_completion(64, Duration::from_millis(1), None)
            .unwrap();
        drop(destination);
        drop(source);
        let connection = rusqlite::Connection::open(&incompatible_db).unwrap();
        connection
            .execute_batch("DROP TABLE diagnostic_generation_publications")
            .unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version,
            i64::from(tracedecay_runtime_core::db::migrations::SCHEMA_VERSION)
        );
        drop(connection);

        let outcome =
            reset_refused_project_graph_store(&profile_root, incompatible_project_id, false)
                .unwrap();
        assert_eq!(
            outcome.reset_graph_db.as_ref().unwrap().path,
            incompatible_db
        );
        assert!(
            !incompatible_db.exists(),
            "same-version store missing a required table must be reset"
        );

        let error = reset_refused_project_graph_store(&profile_root, &healthy_project_id, false)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("already at the canonical schema"),
            "unexpected refusal: {error}"
        );
        assert!(healthy_db.exists(), "a healthy store must never be deleted");
    }

    #[test]
    fn non_sqlite_file_is_refused_untouched() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let data_root = tracedecay_runtime_core::storage::profile_sharded_data_root(
            &profile_root,
            "proj_not_sqlite",
        );
        std::fs::create_dir_all(&data_root).unwrap();
        let db_path = data_root.join(tracedecay_runtime_core::config::DB_FILENAME);
        std::fs::write(&db_path, b"not a database").unwrap();

        let error =
            reset_refused_project_graph_store(&profile_root, "proj_not_sqlite", false).unwrap_err();

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

        let error =
            reset_refused_project_graph_store(&profile_root, "proj_absent", false).unwrap_err();

        assert!(
            error.to_string().contains("nothing to reset"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn reset_while_the_daemon_holds_the_profile_names_daemon_stop() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let daemon = tracedecay_runtime_core::lifecycle_lease::acquire_shared_for_profile(
            &profile_root,
            "daemon",
        )
        .unwrap();

        let refused =
            reset_project_store(&profile_root, None, Some("proj_absent".to_owned()), true)
                .unwrap_err();

        assert_eq!(
            refused.to_string(),
            "config error: cannot reset the project store for 'proj_absent' while the \
             TraceDecay daemon holds the profile; run `tracedecay daemon stop`, re-run this \
             command, then `tracedecay daemon start`"
        );
        drop(daemon);
        let admitted =
            reset_project_store(&profile_root, None, Some("proj_absent".to_owned()), true)
                .unwrap_err();
        assert_eq!(
            admitted.to_string(),
            format!(
                "config error: no project graph store exists at {}; nothing to reset",
                tracedecay_runtime_core::storage::profile_sharded_data_root(
                    &profile_root,
                    "proj_absent"
                )
                .join(tracedecay_runtime_core::config::DB_FILENAME)
                .display()
            )
        );
    }

    /// A retired checkout layout is itself a refused shape, so a project with
    /// no graph store yet still resets instead of reporting nothing to do.
    #[test]
    fn pending_retired_checkout_reset_admits_a_store_with_nothing_refused() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");

        let outcome =
            reset_refused_project_graph_store(&profile_root, "proj_absent", true).unwrap();

        assert!(outcome.reset_graph_db.is_none());
    }
}
