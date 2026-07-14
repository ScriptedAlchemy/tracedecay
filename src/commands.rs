use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use crate::Spinner;
use crate::cli::{BranchAction, MemoryAction, MigrateAction};
use crate::global;
use tracedecay::tracedecay::TraceDecay;

pub(crate) async fn daemon_tool_json(
    project_path: Option<&std::path::Path>,
    tool_name: &str,
    arguments: serde_json::Value,
) -> tracedecay::errors::Result<serde_json::Value> {
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        project_path.map(std::path::Path::to_path_buf),
        None,
        false,
        false,
    )?;
    let result = tracedecay::daemon::call_default_tool(&handshake, tool_name, arguments).await?;
    let blocks = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no content blocks"),
        })?;
    for text in blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
    {
        if let Ok(value) = serde_json::from_str(text) {
            return Ok(value);
        }
    }
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!("daemon tool {tool_name} returned no JSON payload"),
    })
}

pub(crate) async fn handle_memory_action(action: MemoryAction) -> tracedecay::errors::Result<()> {
    match action {
        MemoryAction::Status { .. } => unreachable!("memory status is handled in main.rs dispatch"),
        MemoryAction::Curate {
            apply,
            llm,
            llm_ops,
            max_clusters,
            min_confidence,
            path,
        } => {
            let project_path = tracedecay::config::resolve_path_with_discovery(path);
            let llm_ops_value = match llm_ops {
                Some(source) => Some(read_llm_ops_payload(&source)?),
                None => None,
            };
            let report = daemon_tool_json(
                Some(&project_path),
                "tracedecay_admin_project",
                serde_json::json!({
                    "action": "memory_curate",
                    "apply": apply,
                    "llm": llm,
                    "llm_ops": llm_ops_value,
                    "max_clusters": max_clusters,
                    "min_confidence": min_confidence,
                }),
            )
            .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
        }
    }
    Ok(())
}

/// Reads the `--llm-ops` payload from a file path or stdin (`-`).
fn read_llm_ops_payload(source: &str) -> tracedecay::errors::Result<serde_json::Value> {
    let text = if source == "-" {
        let mut buf = String::new();
        io::stdin().lock().read_to_string(&mut buf).map_err(|e| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to read --llm-ops from stdin: {e}"),
            }
        })?;
        buf
    } else {
        std::fs::read_to_string(source).map_err(|e| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to read --llm-ops file {source}: {e}"),
            }
        })?
    };
    serde_json::from_str(&text).map_err(|e| tracedecay::errors::TraceDecayError::Config {
        message: format!("--llm-ops payload is not valid JSON: {e}"),
    })
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
                let manifest_path = manifest.map(PathBuf::from).unwrap_or_else(|| {
                    PathBuf::from(&profile_root)
                        .join("migration-inventory")
                        .join(format!("{migration_id}.json"))
                });
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
                    let project_root =
                        project
                            .map(PathBuf::from)
                            .unwrap_or(std::env::current_dir().map_err(|e| {
                                tracedecay::errors::TraceDecayError::Config {
                                    message: format!("could not determine current directory: {e}"),
                                }
                            })?);
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

pub(crate) async fn handle_branch_action(action: BranchAction) -> tracedecay::errors::Result<()> {
    use tracedecay::branch;
    use tracedecay::branch_meta;

    match action {
        BranchAction::List { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let status = daemon_tool_json(
                Some(&project_path),
                "tracedecay_status",
                serde_json::json!({ "format": "json" }),
            )
            .await?;
            let diagnostics = status.get("branch_diagnostics").ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "daemon status omitted branch diagnostics".to_string(),
                }
            })?;
            if !diagnostics
                .get("tracking_enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                eprintln!("No branch tracking configured. Run `tracedecay branch add` to start.");
                return Ok(());
            }
            eprintln!(
                "Default branch: {}",
                diagnostics
                    .get("default_branch")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("<unknown>")
            );
            eprintln!(
                "Current branch: {}",
                diagnostics
                    .get("current_branch")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("<detached HEAD>")
            );
            if let Some(serving) = diagnostics
                .get("serving_branch")
                .and_then(serde_json::Value::as_str)
            {
                let suffix = if diagnostics
                    .get("is_fallback")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    " (fallback)"
                } else {
                    ""
                };
                eprintln!("Serving branch: {serving}{suffix}");
            }
            if diagnostics
                .get("branch_drifted")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                eprintln!(
                    "Opened branch: {}",
                    diagnostics
                        .get("open_active_branch")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("<detached HEAD>")
                );
            }
            eprintln!();
            for branch in diagnostics
                .get("branches")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                let db_exists = branch
                    .get("db_exists")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let size = if db_exists {
                    tracedecay::display::format_bytes(
                        branch
                            .get("size_bytes")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                    )
                } else {
                    "missing".to_string()
                };
                let parent = branch
                    .get("parent")
                    .and_then(serde_json::Value::as_str)
                    .map(|p| format!(" (from {p})"))
                    .unwrap_or_default();
                let last_synced_at = branch
                    .get("last_synced_at")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("never");
                let synced = branch_meta::format_timestamp(last_synced_at);
                let mut flags = Vec::new();
                if branch
                    .get("is_default")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    flags.push("default");
                }
                if branch
                    .get("is_current")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    flags.push("current");
                }
                if branch
                    .get("is_serving")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    flags.push("serving");
                }
                if !db_exists {
                    flags.push("missing-db");
                }
                let flags = if flags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", flags.join(", "))
                };
                eprintln!(
                    "  {}{} — {}{}, synced {}",
                    branch
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("<unknown>"),
                    flags,
                    size,
                    parent,
                    synced
                );
            }
            if let Some(warnings) = diagnostics
                .get("warnings")
                .and_then(serde_json::Value::as_array)
                .filter(|warnings| !warnings.is_empty())
            {
                eprintln!();
                for warning in warnings.iter().filter_map(serde_json::Value::as_str) {
                    eprintln!("warning: {warning}");
                }
            }
        }
        BranchAction::Add { name, path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let branch_name = match name {
                Some(n) => n,
                None => branch::current_branch(&project_path).ok_or_else(|| {
                    tracedecay::errors::TraceDecayError::Config {
                        message:
                            "cannot detect current branch (detached HEAD?). Specify a branch name."
                                .to_string(),
                    }
                })?,
            };

            let spinner = Spinner::new();
            spinner.set_message("syncing changes");
            match TraceDecay::add_branch_tracking(&project_path, &branch_name).await? {
                branch::BranchAddOutcome::NotIndexed => {
                    spinner.done("no TraceDecay index found; run `tracedecay init` first");
                }
                branch::BranchAddOutcome::AlreadyTracked => {
                    spinner.done(&format!("Branch '{branch_name}' is already tracked."));
                }
                branch::BranchAddOutcome::Added => {
                    spinner.done(&format!("branch '{branch_name}' tracked"));
                }
                branch::BranchAddOutcome::Deferred => {
                    spinner.done(&format!(
                        "branch '{branch_name}' tracked; sync deferred because another process is active"
                    ));
                }
            }
        }
        BranchAction::Remove { name, path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let tracedecay_dir = resolve_branch_data_root(&project_path).await;
            let Some(mut meta) = branch_meta::load_branch_meta(&tracedecay_dir) else {
                eprintln!("No branch tracking configured.");
                return Ok(());
            };
            if name == meta.default_branch {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: format!("cannot remove default branch '{name}'"),
                });
            }
            if let Some(entry) = meta.remove_branch(&name) {
                let db_path = tracedecay_dir.join(&entry.db_file);
                if db_path.exists() {
                    std::fs::remove_file(&db_path)?;
                    // Also remove WAL/SHM sidecar files
                    let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
                    let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
                }
                branch_meta::save_branch_meta(&tracedecay_dir, &meta)?;
                eprintln!("\x1b[32m✔\x1b[0m Branch '{name}' removed.");
            } else {
                eprintln!("Branch '{name}' is not tracked.");
            }
        }
        BranchAction::Removeall { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let tracedecay_dir = resolve_branch_data_root(&project_path).await;
            let Some(mut meta) = branch_meta::load_branch_meta(&tracedecay_dir) else {
                eprintln!("No branch tracking configured.");
                return Ok(());
            };
            let removed = meta.remove_all_branches();
            if removed.is_empty() {
                eprintln!("No non-default branches to remove.");
            } else {
                for (name, entry) in &removed {
                    let db_path = tracedecay_dir.join(&entry.db_file);
                    if db_path.exists() {
                        std::fs::remove_file(&db_path)?;
                        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
                        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
                    }
                    eprintln!("  removed '{name}'");
                }
                branch_meta::save_branch_meta(&tracedecay_dir, &meta)?;
                eprintln!(
                    "\x1b[32m✔\x1b[0m Removed {} branch(es). Only '{}' remains.",
                    removed.len(),
                    meta.default_branch
                );
            }
        }
        BranchAction::Gc { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let tracedecay_dir = resolve_branch_data_root(&project_path).await;
            let Some(mut meta) = branch_meta::load_branch_meta(&tracedecay_dir) else {
                eprintln!("No branch tracking configured.");
                return Ok(());
            };

            // Find branches in metadata that no longer exist in git
            let stale: Vec<String> = meta
                .branches
                .keys()
                .filter(|name| *name != &meta.default_branch)
                .filter(|name| {
                    let ref_path = project_path.join(format!(".git/refs/heads/{name}"));
                    let packed = project_path.join(".git/packed-refs");
                    let suffix = format!("refs/heads/{name}");
                    let in_packed = packed.exists()
                        && std::fs::read_to_string(&packed)
                            .map(|c| c.lines().any(|line| line.ends_with(&suffix)))
                            .unwrap_or(false);
                    !ref_path.exists() && !in_packed
                })
                .cloned()
                .collect();

            if stale.is_empty() {
                eprintln!("No stale branches to clean up.");
            } else {
                for name in &stale {
                    if let Some(entry) = meta.remove_branch(name) {
                        let db_path = tracedecay_dir.join(&entry.db_file);
                        if db_path.exists() {
                            std::fs::remove_file(&db_path)?;
                            let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
                            let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
                        }
                        eprintln!("  removed '{name}'");
                    }
                }
                branch_meta::save_branch_meta(&tracedecay_dir, &meta)?;
                eprintln!(
                    "\x1b[32m✔\x1b[0m Cleaned up {} stale branch(es).",
                    stale.len()
                );
            }
        }
        BranchAction::Autotrack { action } => {
            handle_branch_autotrack_action(action).await?;
        }
    }
    Ok(())
}

/// Reads or mutates the project-scoped `sync.auto_track_pr_branches` setting and
/// reports the daemon's PR-autotrack status for a project.
async fn handle_branch_autotrack_action(
    action: crate::cli::BranchAutotrackAction,
) -> tracedecay::errors::Result<()> {
    use crate::cli::BranchAutotrackAction;
    use tracedecay::config::{
        MIN_AUTO_TRACK_PR_POLL_SECS, load_config_with_identity, save_config_with_identity,
    };

    match action {
        BranchAutotrackAction::Status { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let config = load_config_with_identity(&project_path).await?;
            let sync = &config.sync;
            eprintln!(
                "PR auto-tracking: {}",
                if sync.auto_track_pr_branches {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            eprintln!(
                "Poll interval: {}s (effective {}s)",
                sync.auto_track_pr_poll_secs,
                sync.effective_auto_track_pr_poll_secs()
            );
            #[cfg(unix)]
            {
                let data_root = resolve_branch_data_root(&project_path).await;
                let managed = tracedecay::daemon::pr_autotrack::managed_summary(&data_root);
                if managed.is_empty() {
                    eprintln!("Tracked PR branches: none");
                } else {
                    eprintln!("Tracked PR branches:");
                    for entry in managed {
                        eprintln!(
                            "  {} — PR #{} (head {})",
                            entry.branch, entry.pr, entry.head_branch
                        );
                    }
                }
            }
        }
        BranchAutotrackAction::Enable { poll_secs, path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let mut config = load_config_with_identity(&project_path).await?;
            config.sync.auto_track_pr_branches = true;
            if let Some(secs) = poll_secs {
                config.sync.auto_track_pr_poll_secs = secs.max(MIN_AUTO_TRACK_PR_POLL_SECS);
            }
            save_config_with_identity(&project_path, &config).await?;
            eprintln!(
                "\x1b[32m✔\x1b[0m PR auto-tracking enabled (poll every {}s). Restart the daemon (`tracedecay daemon restart`) to apply.",
                config.sync.effective_auto_track_pr_poll_secs()
            );
        }
        BranchAutotrackAction::Disable { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let mut config = load_config_with_identity(&project_path).await?;
            config.sync.auto_track_pr_branches = false;
            save_config_with_identity(&project_path, &config).await?;
            eprintln!(
                "\x1b[32m✔\x1b[0m PR auto-tracking disabled. The daemon tears down any managed PR worktrees, refs, synthetic branches and stores on its next poll cycle."
            );
        }
    }
    Ok(())
}

async fn resolve_branch_data_root(project_path: &Path) -> PathBuf {
    fallback_branch_data_root(project_path)
}

fn fallback_branch_data_root(project_path: &Path) -> PathBuf {
    tracedecay::storage::resolve_layout_for_current_profile(project_path)
        .map(|layout| layout.data_root)
        .unwrap_or_else(|_| tracedecay::config::get_tracedecay_dir(project_path))
}

/// Handles the `wipe` and `wipe --all` commands.
pub(crate) async fn handle_wipe(all: bool) -> tracedecay::errors::Result<()> {
    use std::fs;
    let profile_root = tracedecay::storage::default_profile_root()?;
    let lifecycle_lease =
        tracedecay::lifecycle_lease::acquire_exclusive_for_profile(&profile_root, "wipe")?;
    let _database_scope =
        tracedecay::db::enter_maintenance_database_scope(&lifecycle_lease, &profile_root, "wipe")?;
    let home_tracedecay = Some(profile_root);

    let project_paths = global::gather_target_projects(all, &home_tracedecay).await;
    let gdb = tracedecay::global_db::GlobalDb::try_open().await?;
    let mut targets = Vec::new();
    for path in &project_paths {
        let location = global::classify_project_storage_with_registry(
            path,
            gdb.as_ref(),
            home_tracedecay.as_deref(),
        )
        .await;
        if location.status.is_live() {
            targets.push(location);
        }
    }

    if !all && targets.is_empty() {
        eprintln!("No tracedecay projects found in current folder, parents, or children.");
        return Ok(());
    }

    global::print_flash_warning(all, &targets);

    eprint!("Type \x1b[1;32mgo!\x1b[0m to confirm (anything else aborts): ");
    io::stderr().flush().ok();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).map_err(|e| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to read stdin: {e}"),
        }
    })?;
    if answer.trim() != "go!" {
        eprintln!("\x1b[33mAborted — nothing was wiped.\x1b[0m");
        return Ok(());
    }

    let mut removed = 0usize;
    let mut errors = 0usize;
    let mut wiped_paths: Vec<PathBuf> = Vec::new();

    for location in &targets {
        if !location.data_root.exists() {
            continue;
        }
        match fs::remove_dir_all(&location.data_root) {
            Ok(()) => {
                removed += 1;
                wiped_paths.push(location.project_root.clone());
                eprintln!(
                    "  \x1b[32m✔\x1b[0m removed {}",
                    location.data_root.display()
                );
                if let Some(marker_root) = &location.marker_root {
                    let _ = fs::remove_dir_all(marker_root);
                }
            }
            Err(e) => {
                errors += 1;
                eprintln!("  \x1b[31m✗\x1b[0m {} ({e})", location.data_root.display());
            }
        }
    }

    drop(gdb);

    if all {
        if let Some(global_dir) = home_tracedecay.as_ref() {
            for ext in ["db", "db-wal", "db-shm"] {
                let p = global_dir.join(format!("global.{ext}"));
                let _ = fs::remove_file(&p);
            }
            eprintln!(
                "  \x1b[32m✔\x1b[0m emptied global DB at {}/global.db",
                global_dir.display()
            );
        }
    } else if !wiped_paths.is_empty() {
        if let Some(gdb) = tracedecay::global_db::GlobalDb::try_open().await? {
            let path_strs: Vec<String> = wiped_paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            gdb.delete_projects(&path_strs).await;
        }
    }

    eprintln!();
    let suffix = if errors > 0 {
        format!(" ({errors} error(s))")
    } else {
        String::new()
    };
    eprintln!("\x1b[32mWiped {removed} project(s){suffix}.\x1b[0m");
    Ok(())
}

/// Handles the `list` and `list --all` commands.
pub(crate) async fn handle_list(all: bool) -> tracedecay::errors::Result<()> {
    use tracedecay::display::format_token_count;

    let home_tracedecay = tracedecay::config::user_data_dir();
    let project_paths = global::gather_target_projects(all, &home_tracedecay).await;

    if !all && project_paths.is_empty() {
        println!("No tracedecay projects found in current folder, parents, or children.");
        return Ok(());
    }

    let token_result = daemon_tool_json(
        None,
        "tracedecay_admin_cli",
        serde_json::json!({
            "action": "registry_project_tokens",
            "project_args": &project_paths,
        }),
    )
    .await?;
    let token_rows = token_result
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rows: Vec<ListRow> = Vec::with_capacity(project_paths.len());
    let mut total_size: u64 = 0;
    let mut total_tokens: u64 = 0;

    for path in &project_paths {
        let location =
            global::classify_project_storage_with_registry(path, None, home_tracedecay.as_deref())
                .await;
        let has_data = location.data_root.exists();
        let size = if has_data {
            global::tracedecay_dir_size(&location.data_root)
        } else {
            0
        };
        let project_key = tracedecay::global_db::GlobalDb::canonical_project_key(path);
        let tokens = token_rows
            .iter()
            .find(|row| {
                row.get("project")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| {
                        tracedecay::global_db::GlobalDb::canonical_project_key(Path::new(value))
                            == project_key
                    })
            })
            .and_then(|row| row.get("tokens"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        total_size = total_size.saturating_add(size);
        total_tokens = total_tokens.saturating_add(tokens);
        rows.push(ListRow {
            path: path.clone(),
            status_label: location.status.label(),
            has_data,
            size,
            tokens,
        });
    }

    if all {
        append_orphan_manifest_rows(&mut rows, &project_paths, home_tracedecay.as_deref());
    }

    if rows.is_empty() {
        println!("No tracedecay projects tracked in the global DB.");
        return Ok(());
    }

    total_size = rows.iter().map(|row| row.size).sum();
    total_tokens = rows.iter().map(|row| row.tokens).sum();

    rows.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.path.cmp(&b.path)));

    let path_w = rows
        .iter()
        .map(|r| {
            format!("{} [{}]", r.path.display(), r.status_label)
                .chars()
                .count()
        })
        .max()
        .unwrap_or(0);

    println!("Found {} tracedecay project(s):", rows.len());
    println!();
    for r in &rows {
        let path_str = format!("{} [{}]", r.path.display(), r.status_label);
        let pad = path_w.saturating_sub(path_str.chars().count());
        let size_str = if r.has_data {
            tracedecay::display::format_bytes(r.size)
        } else {
            "—".to_string()
        };
        let tokens_str = if r.tokens == 0 {
            "—".to_string()
        } else {
            format_token_count(r.tokens)
        };
        println!(
            "  {path_str}{pad}  {size:>10}  {tokens:>10} tokens",
            pad = " ".repeat(pad),
            size = size_str,
            tokens = tokens_str
        );
    }
    println!();
    let total_tokens_str = if total_tokens == 0 {
        "—".to_string()
    } else {
        format_token_count(total_tokens)
    };
    println!(
        "Total: {} on disk · {} tokens saved",
        tracedecay::display::format_bytes(total_size),
        total_tokens_str
    );
    Ok(())
}

#[derive(Debug)]
struct ListRow {
    path: std::path::PathBuf,
    status_label: &'static str,
    has_data: bool,
    size: u64,
    tokens: u64,
}

fn append_orphan_manifest_rows(
    rows: &mut Vec<ListRow>,
    project_paths: &[std::path::PathBuf],
    profile_root: Option<&Path>,
) {
    let Some(profile_root) = profile_root else {
        return;
    };
    let registered: std::collections::HashSet<String> = project_paths
        .iter()
        .map(|path| tracedecay::global_db::GlobalDb::canonical_project_key(path))
        .collect();
    let report = tracedecay::migrate::registry::scan_profile_store_manifests(
        profile_root,
        tracedecay::tracedecay::current_timestamp(),
    );
    for plan in report.plans {
        if plan.status != tracedecay::migrate::registry::RegistryReconstructionStatus::Eligible {
            continue;
        }
        let key =
            tracedecay::global_db::GlobalDb::canonical_project_key(&plan.project.project_root);
        if registered.contains(&key) {
            continue;
        }
        let data_root = profile_root.join(&plan.store.store_relpath);
        let has_data = data_root.exists();
        let size = if has_data {
            global::tracedecay_dir_size(&data_root)
        } else {
            0
        };
        rows.push(ListRow {
            path: plan.project.project_root,
            status_label: "orphan manifest-reconstructable",
            has_data,
            size,
            tokens: 0,
        });
    }
}

/// True when the global DB has zero registered projects (or can't be opened
/// at all) — i.e. the user has not run `tracedecay init` anywhere yet.
async fn is_fresh_install() -> bool {
    daemon_tool_json(
        None,
        "tracedecay_admin_cli",
        serde_json::json!({ "action": "registry_empty" }),
    )
    .await
    .ok()
    .and_then(|value| value.get("empty").and_then(serde_json::Value::as_bool))
    .unwrap_or(false)
}

/// When invoked with no subcommand, offer to create the index if none exists.
pub(crate) async fn handle_no_command() -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(None);
    if TraceDecay::has_initialized_store(&project_path).await {
        // Already initialized — show help via clap
        let _ = <crate::cli::Cli as clap::CommandFactory>::command().print_help();
        eprintln!();
        return Ok(());
    }
    if is_fresh_install().await {
        eprintln!("\x1b[1;36mWelcome to tracedecay!\x1b[0m");
        eprintln!(
            "Looks like a new installation. To get started, run \x1b[1mtracedecay init\x1b[0m \
             in your project root."
        );
        eprintln!();
    }
    if !io::stdin().is_terminal() {
        eprintln!(
            "No TraceDecay index found at '{}'. Non-interactive: skipping index creation (run `tracedecay init`).",
            project_path.display()
        );
        return Ok(());
    }
    eprint!(
        "No TraceDecay index found at '{}'. Create one now? [Y/n] ",
        project_path.display()
    );
    io::stderr().flush().ok();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).map_err(|e| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to read stdin: {}", e),
        }
    })?;
    let answer = answer.trim();
    if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
        handle_init(
            Some(project_path.to_string_lossy().into_owned()),
            Vec::new(),
            Vec::new(),
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn handle_init(
    path: Option<String>,
    skip_folders: Vec<String>,
    include_folders: Vec<String>,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(path);
    if !skip_folders.is_empty() || !include_folders.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "brokered init does not yet support --skip-folders/--include-folders; configure tracedecay.toml first".to_string(),
        });
    }
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(project_path.clone()),
        None,
        false,
        true,
    )?;
    tracedecay::daemon::call_default_tool(
        &handshake,
        "tracedecay_status",
        serde_json::json!({"format": "json"}),
    )
    .await?;
    eprintln!(
        "initialized and indexed {} via daemon",
        project_path.display()
    );
    Ok(())
}

pub(crate) async fn handle_sync(
    path: Option<String>,
    force: bool,
    skip_folders: Vec<String>,
    include_folders: Vec<String>,
    doctor: bool,
    verbose: bool,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path_with_discovery(path);
    if !skip_folders.is_empty() || !include_folders.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "brokered sync does not yet support --skip-folders/--include-folders; update tracedecay.toml first".to_string(),
        });
    }
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(project_path.clone()),
        None,
        false,
        false,
    )?;
    let result = tracedecay::daemon::call_default_tool(
        &handshake,
        "tracedecay_admin_sync",
        serde_json::json!({"force": force}),
    )
    .await?;
    if verbose {
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    }
    eprintln!("sync completed via daemon for {}", project_path.display());
    if doctor {
        tracedecay::doctor::run_doctor(None).await?;
    }
    Ok(())
}

pub(crate) fn handle_upload_counter(enable: bool) {
    let mut config = tracedecay::user_config::UserConfig::load();
    config.upload_enabled = enable;
    match config.save_with_recovery() {
        Ok(Some(backup)) => eprintln!(
            "note: corrupt config.toml backed up to {} before regenerating",
            backup.display()
        ),
        Ok(None) => {}
        Err(err) => eprintln!("warning: could not save tracedecay config: {err}"),
    }
    if enable {
        eprintln!("Worldwide counter upload enabled.");
    } else {
        eprintln!(
            "Worldwide counter upload disabled. You can re-enable with `tracedecay enable-upload-counter`."
        );
    }
}

pub(crate) async fn handle_gitignore(
    path: Option<String>,
    action: Option<String>,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(path);
    let mut config = tracedecay::config::load_config_with_identity(&project_path).await?;
    match action.as_deref() {
        Some("on") => {
            config.git_ignore = true;
            tracedecay::config::save_config_with_identity(&project_path, &config).await?;
            eprintln!("gitignore enabled — .gitignore rules will be respected during indexing.");
            eprintln!("Run `tracedecay sync` to re-index with the new setting.");
        }
        Some("off") => {
            config.git_ignore = false;
            tracedecay::config::save_config_with_identity(&project_path, &config).await?;
            eprintln!("gitignore disabled — .gitignore rules will be ignored during indexing.");
            eprintln!("Run `tracedecay sync` to re-index with the new setting.");
        }
        Some(other) => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!("unknown action '{other}': expected 'on' or 'off'"),
            });
        }
        None => {
            let status = if config.git_ignore { "on" } else { "off" };
            eprintln!("gitignore: {status}");
        }
    }
    Ok(())
}

pub(crate) async fn handle_bench(
    queries: Option<String>,
    json: bool,
    path: Option<String>,
    max_nodes: usize,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(path);
    let queries_toml = queries
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to read query file: {error}"),
        })?;
    let result = daemon_tool_json(
        Some(&project_path),
        "tracedecay_admin_project",
        serde_json::json!({
            "action": "bench",
            "queries_toml": queries_toml,
            "json": json,
            "max_nodes": max_nodes,
        }),
    )
    .await?;
    let output = result
        .get("output")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "daemon bench response omitted output".to_string(),
        })?;
    print!("{output}");
    Ok(())
}

/// Convert raw tokens-saved into a USD estimate using Sonnet input pricing.
/// Sonnet is the default agent target; output-token savings are not relevant
/// for retrieval savings.
///
/// Pure table lookup: callers that want up-to-date prices must run
/// `pricing::refresh_if_stale()` once beforehand (see [`handle_gain`]).
/// Keeping the refresh out of this function avoids a network fetch per call
/// (it used to fire for every history row and for every unit test process).
pub(crate) fn estimate_dollars_saved(saved_tokens: u64) -> f64 {
    use tracedecay::accounting::pricing;
    let price = pricing::lookup("claude-sonnet-4")
        .map(|p| p.input_per_mtok)
        .unwrap_or(3.0);
    (saved_tokens as f64) * price / 1_000_000.0
}

pub async fn handle_gain(
    all: bool,
    history: bool,
    range: &str,
    json_output: bool,
) -> tracedecay::errors::Result<()> {
    tracedecay::accounting::pricing::refresh_if_stale();
    let since = tracedecay::accounting::metrics::parse_range(range);
    let project_filter: Option<String> = if all {
        None
    } else {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    };

    let result = daemon_tool_json(
        None,
        "tracedecay_admin_cli",
        serde_json::json!({
            "action": "gain_query",
            "project_arg": project_filter,
            "since": since as i64,
            "history": history,
        }),
    )
    .await?;
    if history {
        let rows = result
            .get("history")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .map(|row| tracedecay::global_db::SavingsDay {
                day: row
                    .get("day")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0),
                saved_tokens: row
                    .get("saved_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                calls: row
                    .get("calls")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            })
            .collect::<Vec<_>>();
        if json_output {
            let arr: Vec<_> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "day": r.day,
                        "saved_tokens": r.saved_tokens,
                        "calls": r.calls,
                        "usd": estimate_dollars_saved(r.saved_tokens),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_default());
        } else {
            tracedecay::display::print_gain_history(&rows, estimate_dollars_saved);
        }
        return Ok(());
    }

    let saved_tokens = result
        .get("saved_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let calls = result
        .get("calls")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let usd = estimate_dollars_saved(saved_tokens);

    if json_output {
        let out = serde_json::json!({
            "range": range,
            "project": project_filter.clone().unwrap_or_else(|| "ALL".to_string()),
            "saved_tokens": saved_tokens,
            "calls": calls,
            "usd": usd,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        tracedecay::display::print_gain_total(
            project_filter.as_deref().unwrap_or("ALL projects"),
            range,
            saved_tokens,
            calls,
            usd,
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod gain_tests {
    use super::estimate_dollars_saved;

    #[test]
    fn dollars_uses_sonnet_input_price_by_default() {
        // 1_000_000 tokens × $3 / MTok = $3.00 (Sonnet input price)
        let usd = estimate_dollars_saved(1_000_000);
        assert!((usd - 3.0).abs() < 0.01, "expected ~$3.00, got ${usd}");
    }

    #[test]
    fn dollars_handles_small_counts() {
        // 1_000 tokens × $3 / MTok = $0.003
        let usd = estimate_dollars_saved(1_000);
        assert!((usd - 0.003).abs() < 0.001);
    }

    #[test]
    fn dollars_zero_for_zero_tokens() {
        assert_eq!(estimate_dollars_saved(0), 0.0);
    }
}
