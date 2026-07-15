use std::path::{Path, PathBuf};

use crate::cli::MigrateAction;
use crate::global;

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
                let token =
                    confirm_token.ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
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
        }
        MigrateAction::Plan {
            roots,
            include_all_registered,
            follow_symlinks,
            manifest,
            save,
            profile_root,
            project_id,
            json,
        } => {
            let scan_roots = if roots.is_empty() {
                vec![std::env::current_dir().map_err(|e| {
                    tracedecay::errors::TraceDecayError::Config {
                        message: format!("could not determine current directory: {e}"),
                    }
                })?]
            } else {
                roots.into_iter().map(PathBuf::from).collect()
            };
            let report = tracedecay::migrate::inventory::build_inventory(
                tracedecay::migrate::inventory::MigrationInventoryOptions {
                    roots: scan_roots,
                    follow_symlinks,
                    include_all_registered,
                    ..tracedecay::migrate::inventory::MigrationInventoryOptions::default()
                },
            )
            .await?;
            if manifest.is_some() || save {
                let migration_id = format!("mig_{}", tracedecay::tracedecay::current_timestamp());
                let profile_root =
                    profile_root.ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                        message: "--profile-root is required when saving a manifest".to_string(),
                    })?;
                let project_id =
                    project_id.ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
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
                if let Some(global) = report.global_db {
                    println!(
                        "global db: {} (projects: {}, sessions: {})",
                        global.path.display(),
                        global.project_count,
                        global.session_count
                    );
                }
            }
        }
        MigrateAction::Export {
            from_profile: _,
            project,
            project_id,
            to,
        } => {
            let project_id = match project_id {
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
            let report = tracedecay::migrate::manifest::export_profile_store(
                &profile_root,
                &project_id,
                &PathBuf::from(to),
            )
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: err.to_string(),
            })?;
            println!(
                "migration export: {} artifact(s) from {} to {}",
                report.artifact_count,
                report.source_data_root.display(),
                report.target_dir.display()
            );
        }
        MigrateAction::Apply {
            manifest,
            confirm_token,
        } => {
            let mut manifest = tracedecay::migrate::manifest::load_manifest(manifest)?;
            if manifest.confirmation_token != confirm_token {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "confirmation token does not match migration manifest".to_string(),
                });
            }
            let target_profile_root =
                manifest.destination.profile_root.clone().ok_or_else(|| {
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
            let apply_report = tracedecay::migrate::manifest::apply_migration_manifest(
                &mut manifest,
            )
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
            let global_db = tracedecay::global_db::GlobalDb::try_open_at(
                &apply_report.profile_root.join("global.db"),
            )
            .await?
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: "could not open global DB for migrate apply".to_string(),
            })?;
            let registry_report =
                tracedecay::migrate::registry::apply_single_registry_reconstruction_report(
                    &global_db,
                    &verify_report.registry_reconstruction,
                )
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
            if let Err(err) = tracedecay::migrate::manifest::finalize_migration_apply(&mut manifest)
            {
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
        }
        MigrateAction::Verify { manifest, json } => {
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
        }
        MigrateAction::Reconstruct {
            profile_root,
            apply,
            json,
        } => {
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
                let global_db =
                    tracedecay::global_db::GlobalDb::try_open_at(&profile_root.join("global.db"))
                        .await?
                        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                            message: "could not open global DB for registry reconstruction"
                                .to_string(),
                        })?;
                let applied = tracedecay::migrate::registry::apply_registry_reconstruction_report(
                    &global_db, &report,
                )
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
        }
        MigrateAction::RegistryGc {
            prefix,
            apply,
            json,
        } => {
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
            let global_db =
                tracedecay::global_db::GlobalDb::try_open_at(&profile_root.join("global.db"))
                    .await?
                    .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                        message: "could not open global DB for registry cleanup".to_string(),
                    })?;
            let projects = global_db.list_code_projects(usize::MAX).await;
            let prefixes: Vec<PathBuf> = prefix.iter().map(PathBuf::from).collect();
            let stale = tracedecay::migrate::registry::stale_code_projects(
                &projects,
                &prefixes,
                tracedecay::migrate::registry::StaleRootScope::CanonicalRootMissing,
            );
            let mut stale_storage_projects = Vec::new();
            for project_path in global_db.list_project_paths().await {
                let path = Path::new(&project_path);
                if !prefixes.is_empty() && !prefixes.iter().any(|prefix| path.starts_with(prefix)) {
                    continue;
                }
                let location = global::classify_project_storage_with_registry(
                    path,
                    Some(&global_db),
                    Some(&profile_root),
                )
                .await;
                if location.status == global::ProjectStorageStatus::Stale {
                    stale_storage_projects.push(project_path);
                }
            }
            let (deleted_code_projects, deleted_storage_projects) = if apply {
                let project_ids: Vec<String> = stale
                    .iter()
                    .map(|project| project.project_id.clone())
                    .collect();
                (
                    global_db.delete_code_projects(&project_ids).await,
                    global_db.delete_projects(&stale_storage_projects).await,
                )
            } else {
                (0, 0)
            };
            let candidate_paths = stale
                .iter()
                .map(|project| {
                    tracedecay::global_db::GlobalDb::canonical_project_key(Path::new(
                        &project.canonical_root,
                    ))
                })
                .chain(stale_storage_projects.iter().map(|path| {
                    tracedecay::global_db::GlobalDb::canonical_project_key(Path::new(path))
                }))
                .collect::<std::collections::BTreeSet<_>>();
            let candidate_count = candidate_paths.len();
            let metadata_candidate_count = stale.len() + stale_storage_projects.len();
            let deleted_count = deleted_code_projects + deleted_storage_projects;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "apply": apply,
                        "prefix": prefix,
                        "candidate_count": candidate_count,
                        "metadata_candidate_count": metadata_candidate_count,
                        "code_project_candidate_count": stale.len(),
                        "storage_project_candidate_count": stale_storage_projects.len(),
                        "deleted_count": deleted_count,
                        "deleted_code_project_count": deleted_code_projects,
                        "deleted_storage_project_count": deleted_storage_projects,
                        "candidates": stale,
                        "storage_project_candidates": stale_storage_projects,
                    }))?
                );
            } else {
                println!(
                    "registry-gc: {} stale project(s){}",
                    candidate_count,
                    if apply { " selected" } else { " found" }
                );
                if apply {
                    println!(
                        "metadata rows deleted: {deleted_count} ({deleted_code_projects} identity, {deleted_storage_projects} storage)"
                    );
                } else {
                    println!("dry run: re-run with --apply to delete registry metadata");
                }
                for project_path in candidate_paths.iter().take(20) {
                    println!("{project_path}");
                }
                if candidate_count > 20 {
                    println!("... {} more", candidate_count - 20);
                }
            }
        }
        MigrateAction::Rollback {
            manifest,
            confirm_token,
        } => {
            let mut manifest = tracedecay::migrate::manifest::load_manifest(manifest)?;
            if manifest.confirmation_token != confirm_token {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "confirmation token does not match migration manifest".to_string(),
                });
            }
            let rollback_report = tracedecay::migrate::manifest::rollback_migration_manifest(
                &mut manifest,
            )
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: err.to_string(),
            })?;
            tracedecay::migrate::manifest::save_manifest(&manifest)?;
            println!(
                "migration rollback: {} artifact(s)",
                rollback_report.artifact_count
            );
        }
        MigrateAction::CleanupSources {
            manifest,
            confirm_token,
        } => {
            let manifest = tracedecay::migrate::manifest::load_manifest(manifest)?;
            if manifest.confirmation_token != confirm_token {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "confirmation token does not match migration manifest".to_string(),
                });
            }
            let cleanup_report = tracedecay::migrate::manifest::cleanup_migration_sources(
                &manifest,
            )
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: err.to_string(),
            })?;
            println!(
                "migration cleanup-sources: {} source artifact(s) removed",
                cleanup_report.removed_artifacts
            );
        }
    }
    Ok(())
}
