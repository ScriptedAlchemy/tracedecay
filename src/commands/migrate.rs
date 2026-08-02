use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cli::MigrateAction;

async fn build_migration_inventory(
    options: tracedecay::migrate::inventory::MigrationInventoryOptions,
) -> tracedecay::errors::Result<tracedecay::migrate::inventory::MigrationInventory> {
    let daemon_available = tracedecay::daemon::daemon_reachable();

    if daemon_available {
        return brokered_migration_inventory(&options).await;
    }

    match tracedecay::migrate::inventory::build_inventory(options.clone()).await {
        Ok(report) => Ok(report),
        Err(offline_error) => {
            if tracedecay::daemon::daemon_reachable() {
                return brokered_migration_inventory(&options).await;
            }
            Err(offline_error)
        }
    }
}

async fn brokered_migration_inventory(
    options: &tracedecay::migrate::inventory::MigrationInventoryOptions,
) -> tracedecay::errors::Result<tracedecay::migrate::inventory::MigrationInventory> {
    let value = super::daemon::daemon_tool_json(
        None,
        "tracedecay_admin_cli",
        serde_json::json!({
            "action": "migration_inventory",
            "roots": &options.roots,
            "follow_symlinks": options.follow_symlinks,
            "include_all_registered": options.include_all_registered,
            "verify_integrity": matches!(
                options.integrity,
                tracedecay::migrate::inventory::InventoryIntegrityMode::Full
            ),
        }),
    )
    .await?;
    serde_json::from_value(value).map_err(Into::into)
}

pub(crate) async fn handle_migrate_action(action: MigrateAction) -> tracedecay::errors::Result<()> {
    match action {
        MigrateAction::Consolidate {
            project,
            source_project_id,
            target_project_id,
            profile_root,
            apply,
            confirm_token,
            json,
        } => {
            handle_migrate_consolidate(
                project,
                source_project_id,
                target_project_id,
                profile_root,
                apply,
                confirm_token,
                json,
            )
            .await
        }
        MigrateAction::Plan {
            roots,
            include_all_registered,
            follow_symlinks,
            verify_integrity,
            manifest,
            save,
            profile_root,
            project_id,
            json,
        } => {
            handle_migrate_plan(
                roots,
                include_all_registered,
                follow_symlinks,
                verify_integrity,
                manifest,
                save,
                profile_root,
                project_id,
                json,
            )
            .await
        }
        MigrateAction::Export {
            from_profile: _,
            project,
            project_id,
            to,
        } => handle_migrate_export(project, project_id, to),
        MigrateAction::Apply {
            manifest,
            confirm_token,
        } => handle_migrate_apply(manifest, confirm_token).await,
        MigrateAction::Verify { manifest, json } => handle_migrate_verify(manifest, json),
        MigrateAction::Reconstruct {
            profile_root,
            apply,
            json,
        } => handle_migrate_reconstruct(profile_root, apply, json).await,
        MigrateAction::RegistryGc {
            prefix,
            apply,
            json,
        } => handle_migrate_registry_gc(prefix, apply, json).await,
        MigrateAction::StorageReport {
            profile_root,
            project_id,
            project_root,
            json,
        } => handle_migrate_storage_report(profile_root, project_id, project_root, json).await,
        MigrateAction::BackupProfile { to, backup_id } => {
            handle_migrate_backup_profile(to, backup_id)
        }
        MigrateAction::RehearseProfileBackup { backup, restore } => {
            handle_migrate_rehearse_profile_backup(backup, restore)
        }
        MigrateAction::Rollback {
            manifest,
            confirm_token,
        } => handle_migrate_rollback(manifest, confirm_token),
        MigrateAction::CleanupSources {
            manifest,
            confirm_token,
        } => handle_migrate_cleanup_sources(manifest, confirm_token),
    }
}

async fn handle_migrate_consolidate(
    project: String,
    source_project_id: String,
    target_project_id: String,
    profile_root: Option<String>,
    apply: bool,
    confirm_token: Option<String>,
    json: bool,
) -> tracedecay::errors::Result<()> {
    let profile_root = profile_root.map_or_else(
        || {
            tracedecay::config::user_data_dir().ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "could not determine TraceDecay profile root".to_string(),
                }
            })
        },
        |value| Ok(PathBuf::from(value)),
    )?;
    let options = tracedecay::migrate::consolidate::ConsolidationOptions {
        project_root: PathBuf::from(project),
        profile_root,
        source_project_id,
        target_project_id,
    };
    let report = if apply {
        let token = confirm_token.ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "--confirm-token is required with --apply".to_string(),
        })?;
        tracedecay::migrate::consolidate::apply(&options, &token).await?
    } else {
        tracedecay::migrate::consolidate::plan(&options).await?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Migration: {}", report.migration_id);
        println!("State: {:?}", report.state);
        println!(
            "Source: {} ({})",
            report.source.project_id,
            report.source.data_root.display()
        );
        println!(
            "Target: {} ({})",
            report.target.project_id,
            report.target.data_root.display()
        );
        println!(
            "Destination: {} ({})",
            report.destination_project_id,
            report.destination_data_root.display()
        );
        println!("Backups: {}", report.backup_root.display());
        println!("Ledger: {}", report.ledger_path.display());
        if report.dry_run {
            println!("Confirmation token: {}", report.confirmation_token);
            println!("No files changed.");
        }
    }
    Ok(())
}

async fn handle_migrate_plan(
    roots: Vec<String>,
    include_all_registered: bool,
    follow_symlinks: bool,
    verify_integrity: bool,
    manifest: Option<String>,
    save: bool,
    profile_root: Option<String>,
    project_id: Option<String>,
    json: bool,
) -> tracedecay::errors::Result<()> {
    let cwd =
        std::env::current_dir().map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not determine current directory: {error}"),
        })?;
    let scan_roots = if roots.is_empty() {
        vec![cwd]
    } else {
        roots
            .into_iter()
            .map(PathBuf::from)
            .map(|root| {
                let absolute = if root.is_absolute() {
                    root
                } else {
                    cwd.join(root)
                };
                absolute.canonicalize().unwrap_or(absolute)
            })
            .collect()
    };
    let saves_manifest = manifest.is_some() || save;
    let integrity = if verify_integrity || saves_manifest {
        tracedecay::migrate::inventory::InventoryIntegrityMode::Full
    } else {
        tracedecay::migrate::inventory::InventoryIntegrityMode::MetadataOnly
    };
    let report =
        build_migration_inventory(tracedecay::migrate::inventory::MigrationInventoryOptions {
            roots: scan_roots,
            follow_symlinks,
            include_all_registered,
            integrity,
            ..tracedecay::migrate::inventory::MigrationInventoryOptions::default()
        })
        .await?;
    if saves_manifest {
        let migration_id = format!("mig_{}", tracedecay::tracedecay::current_timestamp());
        let profile_root =
            profile_root.ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: "--profile-root is required when saving a manifest".to_string(),
            })?;
        let project_id = project_id.ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "--project-id is required when saving a manifest".to_string(),
        })?;
        let manifest_path = manifest.map_or_else(
            || {
                PathBuf::from(&profile_root)
                    .join("migration-inventory")
                    .join(format!("{migration_id}.json"))
            },
            PathBuf::from,
        );
        let confirmation_token = format!("confirm-{migration_id}");
        let manifest = tracedecay::migrate::manifest::build_plan_manifest(
            report,
            tracedecay::migrate::manifest::MigrationPlanOptions {
                manifest_path,
                migration_id,
                tracedecay_version: env!("CARGO_PKG_VERSION").to_string(),
                created_at_unix: tracedecay::tracedecay::current_timestamp(),
                confirmation_token,
                target_profile_root: PathBuf::from(profile_root),
                project_id,
            },
        )
        .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })?;
        tracedecay::migrate::manifest::save_manifest(&manifest)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        } else {
            println!(
                "migration manifest: {} ({} artifact(s))",
                manifest.protocol.manifest_path.display(),
                manifest.artifacts.len()
            );
            println!("confirmation token: {}", manifest.confirmation_token);
        }
    } else if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "migration inventory: {} store(s), {} skipped path(s)",
            report.stores.len(),
            report.skipped.len()
        );
        for store in &report.stores {
            if store.statuses != [tracedecay::migrate::inventory::StoreStatus::Ok] {
                println!("store {}: {:?}", store.data_dir.display(), store.statuses);
            }
        }
        if let Some(global) = report.global_db {
            println!(
                "global db: {} (projects: {}, sessions: {}, integrity: {:?})",
                global.path.display(),
                global.project_count,
                global.session_count,
                global.integrity
            );
            for warning in global.warnings {
                println!("global db warning: {warning}");
            }
        }
    }
    Ok(())
}

fn handle_migrate_export(
    project: Option<String>,
    project_id: Option<String>,
    to: String,
) -> tracedecay::errors::Result<()> {
    let project_id =
        match project_id {
            Some(project_id) => project_id,
            None => {
                let project_root = project.map_or(
                    std::env::current_dir().map_err(|e| {
                        tracedecay::errors::TraceDecayError::Config {
                            message: format!("could not determine current directory: {e}"),
                        }
                    })?,
                    PathBuf::from,
                );
                let marker = tracedecay::storage::read_enrollment_marker(&project_root)?
                    .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                        message: format!(
                            "project '{}' is not enrolled in profile-sharded storage",
                            project_root.display()
                        ),
                    })?;
                marker.project_id
            }
        };
    let profile_root = tracedecay::storage::default_profile_root()?;
    let target_dir = PathBuf::from(to);
    let report =
        tracedecay::daemon::with_quiesced_installed_service("profile store export", |lifecycle| {
            tracedecay::migrate::manifest::export_profile_store_with_lease(
                &profile_root,
                &project_id,
                &target_dir,
                lifecycle,
            )
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: err.to_string(),
            })
        })?;
    println!(
        "migration export: {} artifact(s) from {} to {}",
        report.artifact_count,
        report.source_data_root.display(),
        report.target_dir.display()
    );
    Ok(())
}

async fn handle_migrate_apply(
    manifest: String,
    confirm_token: String,
) -> tracedecay::errors::Result<()> {
    let mut manifest = tracedecay::migrate::manifest::load_manifest(manifest)?;
    if manifest.confirmation_token != confirm_token {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "confirmation token does not match migration manifest".to_string(),
        });
    }
    let target_profile_root = manifest.destination.profile_root.clone().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "migration manifest has no destination profile_root".to_string(),
        }
    })?;
    let _lifecycle_lease = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &target_profile_root,
        "legacy store migration",
    )?;
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &_lifecycle_lease,
        &target_profile_root,
        "legacy store migration",
    )?;
    let apply_report =
        tracedecay::migrate::manifest::apply_migration_manifest_with_destination_lease(
            &mut manifest,
            &_lifecycle_lease,
        )
        .await
        .map_err(|err| tracedecay::errors::TraceDecayError::Config {
            message: err.to_string(),
        })?;
    let verify_report = tracedecay::migrate::manifest::verify_migration_manifest(&manifest);
    if !verify_report.cutover_ready {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "migration staging did not reach cutover-ready state: {} missing target(s), {} issue(s)",
                verify_report.missing_targets,
                verify_report.issues.len()
            ),
        });
    }
    let migration_runtime =
        tracedecay::migrate::registry::MigrationRegistryRuntime::open(&apply_report.profile_root)
            .await?;
    let registry_report = migration_runtime
        .apply_single_reconstruction(&verify_report.registry_reconstruction)
        .await
        .map_err(|issues| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "failed to apply registry reconstruction: {}",
                issues.join("; ")
            ),
        })?;
    tracedecay::storage::write_enrollment_marker(
        &apply_report.project_root,
        &tracedecay::storage::EnrollmentMarker {
            project_id: apply_report.project_id.clone(),
            storage_mode: tracedecay::storage::StorageMode::ProfileSharded,
        },
    )?;
    if let Err(err) = tracedecay::migrate::manifest::finalize_migration_apply(&mut manifest) {
        let _ = tracedecay::storage::remove_enrollment_marker(
            &apply_report.project_root,
            &apply_report.project_id,
        );
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: err.to_string(),
        });
    }
    tracedecay::migrate::manifest::save_manifest(&manifest)?;
    println!(
        "migration apply: {} artifact(s), {} registry project(s), {} alias(es)",
        apply_report.artifact_count, registry_report.projects, registry_report.aliases
    );
    Ok(())
}

fn handle_migrate_verify(manifest: String, json: bool) -> tracedecay::errors::Result<()> {
    let manifest = tracedecay::migrate::manifest::load_manifest(manifest)?;
    let report = tracedecay::migrate::manifest::verify_migration_manifest(&manifest);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "migration verify: {} artifact(s), {} planned target(s), {} missing target(s)",
            report.artifact_count, report.planned_targets, report.missing_targets
        );
        println!(
            "registry reconstruction: {} plan(s), {} store manifest(s), {} issue(s)",
            report.registry_plan_count,
            report.store_manifest_count,
            report.issues.len()
        );
        println!(
            "cutover ready: {}",
            if report.cutover_ready { "yes" } else { "no" }
        );
        println!(
            "apply supported: {}",
            if report.apply_supported { "yes" } else { "no" }
        );
    }
    Ok(())
}

async fn handle_migrate_reconstruct(
    profile_root: String,
    apply: bool,
    json: bool,
) -> tracedecay::errors::Result<()> {
    let profile_root = PathBuf::from(profile_root);
    if apply {
        let projects_root = profile_root.join("projects");
        std::fs::read_dir(&projects_root).map_err(|error| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "could not read profile projects directory '{}': {error}",
                    projects_root.display()
                ),
            }
        })?;
    }
    let _lifecycle_lease = apply
        .then(|| {
            tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
                &profile_root,
                "registry reconstruction",
            )
        })
        .transpose()?;
    let _database_scope = _lifecycle_lease
        .as_ref()
        .map(|lifecycle_lease| {
            tracedecay::db::enter_maintenance_database_scope(
                lifecycle_lease,
                &profile_root,
                "registry reconstruction",
            )
        })
        .transpose()?;
    let report = tracedecay::migrate::registry::scan_profile_store_manifests(
        &profile_root,
        tracedecay::tracedecay::current_timestamp(),
    );
    if apply {
        let mut blockers = report.issues.clone();
        blockers.extend(
            report
                .plans
                .iter()
                .filter(|plan| {
                    plan.status
                        == tracedecay::migrate::registry::RegistryReconstructionStatus::Blocked
                })
                .map(|plan| {
                    format!(
                        "blocked manifest '{}': {}",
                        plan.manifest_path.display(),
                        plan.status_reason.as_deref().unwrap_or("not eligible")
                    )
                }),
        );
        if !blockers.is_empty() {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "failed to preflight registry reconstruction: {}",
                    blockers.join("; ")
                ),
            });
        }
        let migration_runtime =
            tracedecay::migrate::registry::MigrationRegistryRuntime::open(&profile_root).await?;
        let applied = migration_runtime
            .apply_reconstruction(&report)
            .await
            .map_err(|issues| tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "failed to apply registry reconstruction: {}",
                    issues.join("; ")
                ),
            })?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "dry_run": report,
                    "applied": applied,
                }))?
            );
        } else {
            println!(
                "registry reconstruction applied: {} project(s), {} alias(es), {} store(s), {} graph scope(s), {} artifact(s)",
                applied.projects,
                applied.aliases,
                applied.stores,
                applied.graph_scopes,
                applied.artifacts
            );
        }
    } else if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        use tracedecay::migrate::registry::RegistryReconstructionStatus;
        let eligible = report.status_count(RegistryReconstructionStatus::Eligible);
        let blocked = report.status_count(RegistryReconstructionStatus::Blocked);
        let stale = report.status_count(RegistryReconstructionStatus::Stale);
        let retired = report.status_count(RegistryReconstructionStatus::Retired);
        println!(
            "registry reconstruction: {} eligible, {} blocked, {} stale, {} retired, {} issue(s)",
            eligible,
            blocked,
            stale,
            retired,
            report.issues.len()
        );
        println!(
            "apply supported: {} (atomic batch; skips stale/retired, inserts eligible missing rows only, fails on blocked/invalid/conflict)",
            if blocked == 0 && report.issues.is_empty() {
                "yes"
            } else {
                "no"
            }
        );
    }
    Ok(())
}

async fn handle_migrate_registry_gc(
    prefix: Option<String>,
    apply: bool,
    json: bool,
) -> tracedecay::errors::Result<()> {
    let report = registry_gc(prefix, apply).await?;
    print_registry_gc_report(report, json)
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
async fn handle_migrate_storage_report(
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
                tracedecay::retention::storage_report::build_project_storage_report(
                    &profile_root,
                    project_id,
                    project_root,
                )
            }
            (None, None) => {
                tracedecay::retention::storage_report::build_storage_report(&profile_root).await
            }
            _ => unreachable!("clap requires project id and root together"),
        };
        match offline {
            Ok(report) => report,
            Err(_) if daemon_owns_profile && tracedecay::daemon::daemon_reachable() => {
                brokered_storage_report(project_id.as_deref(), project_root.as_deref()).await?
            }
            Err(error) => return Err(error),
        }
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

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn handle_migrate_rollback(
    manifest: String,
    confirm_token: String,
) -> tracedecay::errors::Result<()> {
    let mut manifest = tracedecay::migrate::manifest::load_manifest(manifest)?;
    if manifest.confirmation_token != confirm_token {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "confirmation token does not match migration manifest".to_string(),
        });
    }
    let rollback_report = tracedecay::migrate::manifest::rollback_migration_manifest(&mut manifest)
        .map_err(|err| tracedecay::errors::TraceDecayError::Config {
            message: err.to_string(),
        })?;
    tracedecay::migrate::manifest::save_manifest(&manifest)?;
    println!(
        "migration rollback: {} artifact(s)",
        rollback_report.artifact_count
    );
    Ok(())
}

fn handle_migrate_cleanup_sources(
    manifest: String,
    confirm_token: String,
) -> tracedecay::errors::Result<()> {
    let manifest = tracedecay::migrate::manifest::load_manifest(manifest)?;
    if manifest.confirmation_token != confirm_token {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "confirmation token does not match migration manifest".to_string(),
        });
    }
    let cleanup_report = tracedecay::migrate::manifest::cleanup_migration_sources(&manifest)
        .map_err(|err| tracedecay::errors::TraceDecayError::Config {
            message: err.to_string(),
        })?;
    println!(
        "migration cleanup-sources: {} source artifact(s) removed",
        cleanup_report.removed_artifacts
    );
    Ok(())
}

fn handle_migrate_backup_profile(
    destination: String,
    backup_id: String,
) -> tracedecay::errors::Result<()> {
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
            tracedecay::migrate::profile_backup::create_complete_profile_backup(
                &profile_root,
                Path::new(&destination),
                &backup_id,
                created_at,
                lifecycle,
            )
            .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })
        },
    )?;
    println!(
        "complete profile backup created and verified: {}",
        backup.display()
    );
    Ok(())
}

fn handle_migrate_rehearse_profile_backup(
    backup: String,
    restore: String,
) -> tracedecay::errors::Result<()> {
    let manifest = tracedecay::migrate::profile_backup::rehearse_complete_profile_backup(
        Path::new(&backup),
        Path::new(&restore),
    )
    .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })?;
    println!(
        "complete profile backup rehearsed: {} entries restored to {}",
        manifest.entries.len(),
        restore
    );
    Ok(())
}

async fn registry_gc(
    prefix: Option<String>,
    apply: bool,
) -> tracedecay::errors::Result<serde_json::Value> {
    let daemon_available = tracedecay::daemon::daemon_reachable();

    if daemon_available {
        return brokered_registry_gc(prefix, apply).await;
    }

    match offline_registry_gc(prefix.clone(), apply).await {
        Ok(report) => Ok(report),
        Err(offline_error) => {
            if tracedecay::daemon::daemon_reachable() {
                return brokered_registry_gc(prefix, apply).await;
            }
            Err(offline_error)
        }
    }
}

async fn brokered_registry_gc(
    prefix: Option<String>,
    apply: bool,
) -> tracedecay::errors::Result<serde_json::Value> {
    let cwd =
        std::env::current_dir().map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to determine current directory for registry cleanup: {error}"),
        })?;
    let project_root = tracedecay::config::discover_project_root(&cwd);
    super::daemon::daemon_tool_json(
        project_root.as_deref(),
        "tracedecay_admin_cli",
        serde_json::json!({
            "action": "registry_gc",
            "prefix": prefix,
            "apply": apply,
        }),
    )
    .await
}

async fn offline_registry_gc(
    prefix: Option<String>,
    apply: bool,
) -> tracedecay::errors::Result<serde_json::Value> {
    let profile_root = tracedecay::storage::default_profile_root()?;
    let lifecycle_lease = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "registry cleanup",
    )?;
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle_lease,
        &profile_root,
        "registry cleanup",
    )?;
    let migration_runtime =
        tracedecay::migrate::registry::MigrationRegistryRuntime::try_open_existing(&profile_root)
            .await?
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: "could not open global DB for registry cleanup".to_string(),
            })?;
    let report = migration_runtime
        .registry_gc(&profile_root, prefix, apply)
        .await?;
    serde_json::to_value(report).map_err(Into::into)
}

#[derive(serde::Deserialize)]
struct RegistryGcDisplay {
    apply: bool,
    candidate_count: usize,
    deleted_count: usize,
    deleted_code_project_count: usize,
    deleted_storage_project_count: usize,
    #[serde(default)]
    protected_code_project_count: usize,
    candidate_paths: Vec<String>,
}

fn print_registry_gc_report(
    report: serde_json::Value,
    json: bool,
) -> tracedecay::errors::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let display: RegistryGcDisplay = serde_json::from_value(report)?;
    println!(
        "registry-gc: {} stale project(s){}",
        display.candidate_count,
        if display.apply { " selected" } else { " found" }
    );
    if display.apply {
        println!(
            "metadata rows deleted: {} ({} identity, {} storage)",
            display.deleted_count,
            display.deleted_code_project_count,
            display.deleted_storage_project_count
        );
    } else {
        println!("dry run: re-run with --apply to delete registry metadata");
    }
    if display.protected_code_project_count > 0 {
        println!(
            "retained-store authorities protected: {}",
            display.protected_code_project_count
        );
    }
    for project_path in display.candidate_paths.iter().take(20) {
        println!("{project_path}");
    }
    if display.candidate_count > 20 {
        println!("... {} more", display.candidate_count - 20);
    }
    Ok(())
}
