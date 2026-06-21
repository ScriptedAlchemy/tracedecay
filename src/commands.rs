use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use crate::cli::{BranchAction, MemoryAction, MigrateAction};
use crate::global;
use crate::Spinner;
use tracedecay::tracedecay::TraceDecay;

pub(crate) async fn handle_memory_action(action: MemoryAction) -> tracedecay::errors::Result<()> {
    use tracedecay::dashboard::memory_curate::{run_memory_curate, MemoryCurateOptions};

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
            let cg = crate::serve::ensure_initialized(&project_path).await?;
            let llm_ops_value = match llm_ops {
                Some(source) => Some(read_llm_ops_payload(&source)?),
                None => None,
            };
            let options = MemoryCurateOptions {
                apply,
                llm,
                llm_ops: llm_ops_value,
                max_clusters: max_clusters.clamp(1, 50),
                min_confidence: min_confidence.clamp(0.0, 1.0),
            };
            let report = run_memory_curate(&cg, &options).await?;
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
            from_profile,
            project,
            project_id,
            to,
        } => {
            if !from_profile {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "migrate export currently supports only --from-profile".to_string(),
                });
            }
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
            let global_db = tracedecay::global_db::GlobalDb::open()
                .await
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: "could not open global DB for migrate apply".to_string(),
                })?;
            let registry_report =
                tracedecay::migrate::registry::apply_registry_reconstruction_report(
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
            let report = tracedecay::migrate::registry::scan_profile_store_manifests(
                &PathBuf::from(profile_root),
                tracedecay::tracedecay::current_timestamp(),
            );
            if apply {
                let global_db = tracedecay::global_db::GlobalDb::open()
                    .await
                    .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                        message: "could not open global DB for registry reconstruction".to_string(),
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
                println!(
                    "registry reconstruction: {} plan(s), {} issue(s)",
                    report.plans.len(),
                    report.issues.len()
                );
                println!("apply supported: yes (re-run with --apply after review)");
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
            let tracedecay_dir = resolve_branch_data_root(&project_path);
            let Some(_meta) = branch_meta::load_branch_meta(&tracedecay_dir) else {
                eprintln!("No branch tracking configured. Run `tracedecay branch add` to start.");
                return Ok(());
            };
            let diagnostics = TraceDecay::project_branch_diagnostics(&project_path);
            eprintln!(
                "Default branch: {}",
                diagnostics.default_branch.as_deref().unwrap_or("<unknown>")
            );
            eprintln!(
                "Current branch: {}",
                diagnostics
                    .current_branch
                    .as_deref()
                    .unwrap_or("<detached HEAD>")
            );
            if let Some(serving) = diagnostics.serving_branch.as_deref() {
                let suffix = if diagnostics.is_fallback {
                    " (fallback)"
                } else {
                    ""
                };
                eprintln!("Serving branch: {serving}{suffix}");
            }
            if diagnostics.branch_drifted {
                eprintln!(
                    "Opened branch: {}",
                    diagnostics
                        .open_active_branch
                        .as_deref()
                        .unwrap_or("<detached HEAD>")
                );
            }
            eprintln!();
            for branch in &diagnostics.branches {
                let size = if branch.db_exists {
                    tracedecay::display::format_bytes(branch.size_bytes)
                } else {
                    "missing".to_string()
                };
                let parent = branch
                    .parent
                    .as_deref()
                    .map(|p| format!(" (from {p})"))
                    .unwrap_or_default();
                let synced = branch_meta::format_timestamp(&branch.last_synced_at);
                let mut flags = Vec::new();
                if branch.is_default {
                    flags.push("default");
                }
                if branch.is_current {
                    flags.push("current");
                }
                if branch.is_serving {
                    flags.push("serving");
                }
                if !branch.db_exists {
                    flags.push("missing-db");
                }
                let flags = if flags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", flags.join(", "))
                };
                eprintln!(
                    "  {}{} — {}{}, synced {}",
                    branch.name, flags, size, parent, synced
                );
            }
            if !diagnostics.warnings.is_empty() {
                eprintln!();
                for warning in diagnostics.warnings {
                    eprintln!("warning: {warning}");
                }
            }
        }
        BranchAction::Add { name, path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let tracedecay_dir = resolve_branch_data_root(&project_path);

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

            // Load or bootstrap metadata
            let mut meta = branch_meta::load_branch_meta(&tracedecay_dir).unwrap_or_else(|| {
                let default = branch::detect_default_branch(&project_path)
                    .unwrap_or_else(|| "main".to_string());
                branch_meta::BranchMeta::new_for_dir(&tracedecay_dir, &default)
            });

            if meta.is_tracked(&branch_name) {
                eprintln!("Branch '{branch_name}' is already tracked.");
                return Ok(());
            }

            // Find parent DB to copy from
            let parent = branch::find_nearest_tracked_ancestor(&project_path, &branch_name, &meta)
                .unwrap_or_else(|| meta.default_branch.clone());
            let parent_db = branch::resolve_branch_db_path(&tracedecay_dir, &parent, &meta)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!("parent branch '{parent}' has no DB"),
                })?;
            if !parent_db.exists() {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: format!("parent DB not found at '{}'", parent_db.display()),
                });
            }

            // Copy DB
            let sanitized = branch::sanitize_branch_name(&branch_name);
            let branches_dir = branch_meta::ensure_branches_dir(&tracedecay_dir)?;
            let new_db_path = branches_dir.join(format!("{sanitized}.db"));
            let spinner = Spinner::new();
            spinner.set_message(&format!("copying DB from '{parent}'"));
            std::fs::copy(&parent_db, &new_db_path)?;

            // Save metadata BEFORE open() so it resolves the new branch to its DB
            let db_file = format!("branches/{sanitized}.db");
            meta.add_branch(&branch_name, &db_file, &parent);
            branch_meta::save_branch_meta(&tracedecay_dir, &meta)?;

            // Run incremental sync (hash-based delta) against the new branch DB
            spinner.set_message("syncing changes");
            let cg = TraceDecay::open(&project_path).await?;
            let result = cg.sync().await?;

            // Update sync timestamp after successful sync
            if let Some(mut meta) = branch_meta::load_branch_meta(&tracedecay_dir) {
                meta.touch_synced(&branch_name);
                let _ = branch_meta::save_branch_meta(&tracedecay_dir, &meta);
            }

            let skipped_msg = if result.skipped_paths.is_empty() {
                String::new()
            } else {
                format!(", {} skipped", result.skipped_paths.len())
            };
            spinner.done(&format!(
                "branch '{branch_name}' tracked — {} added, {} modified, {} removed{skipped_msg}",
                result.files_added, result.files_modified, result.files_removed
            ));
            if !result.skipped_paths.is_empty() {
                eprintln!();
                eprintln!(
                    "\x1b[33mSkipped ({}) — files found but not readable:\x1b[0m",
                    result.skipped_paths.len()
                );
                for (path, reason) in &result.skipped_paths {
                    eprintln!("  ! {path}: {reason}");
                }
            }
        }
        BranchAction::Remove { name, path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let tracedecay_dir = resolve_branch_data_root(&project_path);
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
            let tracedecay_dir = resolve_branch_data_root(&project_path);
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
            let tracedecay_dir = resolve_branch_data_root(&project_path);
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
    }
    Ok(())
}

fn resolve_branch_data_root(project_path: &Path) -> PathBuf {
    tracedecay::storage::resolve_layout_for_current_profile(project_path)
        .map(|layout| layout.data_root)
        .unwrap_or_else(|_| tracedecay::config::get_tracedecay_dir(project_path))
}

/// Handles the `wipe` and `wipe --all` commands.
pub(crate) async fn handle_wipe(all: bool) -> tracedecay::errors::Result<()> {
    use std::fs;
    let home_tracedecay = tracedecay::config::user_data_dir();

    let project_paths = global::gather_target_projects(all, &home_tracedecay).await;
    let gdb = tracedecay::global_db::GlobalDb::open().await;
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
        if let Some(gdb) = tracedecay::global_db::GlobalDb::open().await {
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

    let gdb = tracedecay::global_db::GlobalDb::open().await;
    let mut rows: Vec<ListRow> = Vec::with_capacity(project_paths.len());
    let mut total_size: u64 = 0;
    let mut total_tokens: u64 = 0;

    for path in &project_paths {
        let location = global::classify_project_storage_with_registry(
            path,
            gdb.as_ref(),
            home_tracedecay.as_deref(),
        )
        .await;
        let has_data = location.data_root.exists();
        let size = if has_data {
            global::tracedecay_dir_size(&location.data_root)
        } else {
            0
        };
        let tokens = match &gdb {
            Some(db) => db.get_project_tokens(path).await,
            None => 0,
        };
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
    match tracedecay::global_db::GlobalDb::open().await {
        Some(gdb) => gdb.list_project_paths().await.is_empty(),
        None => true,
    }
}

/// When invoked with no subcommand, offer to create the index if none exists.
pub(crate) async fn handle_no_command() -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(None);
    if TraceDecay::is_initialized(&project_path) {
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
        init_and_index(&project_path, &[], false).await?;
    }
    Ok(())
}

pub(crate) async fn handle_init(
    path: Option<String>,
    skip_folders: Vec<String>,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(path);
    if TraceDecay::is_initialized(&project_path) {
        eprintln!(
            "\x1b[31merror:\x1b[0m TraceDecay is already initialized at '{}'.\n\
             Use \x1b[1mtracedecay sync\x1b[0m to update the index, or \
             \x1b[1mtracedecay sync --force\x1b[0m to rebuild it.",
            project_path.display()
        );
        std::process::exit(1);
    }

    let version_handle = std::thread::spawn(tracedecay::cloud::fetch_latest_version);
    init_and_index(&project_path, &skip_folders, false).await?;
    maybe_print_parallel_update_notice(version_handle);
    Ok(())
}

pub(crate) async fn handle_sync(
    path: Option<String>,
    force: bool,
    skip_folders: Vec<String>,
    doctor: bool,
    verbose: bool,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path_with_discovery(path);
    if !TraceDecay::is_initialized(&project_path) {
        eprintln!(
            "\x1b[31merror:\x1b[0m no TraceDecay index found at '{}'.\n\
             Run \x1b[1mtracedecay init\x1b[0m to create one first.",
            project_path.display()
        );
        std::process::exit(1);
    }
    if project_path.join(".codegraph").is_dir() {
        eprintln!(
            "warning: found legacy .codegraph/ directory at '{}'. \
             tracedecay now uses .tracedecay/ — the old directory can be safely deleted.",
            project_path.display()
        );
    }

    let version_handle = std::thread::spawn(tracedecay::cloud::fetch_latest_version);

    if force {
        init_and_index(&project_path, &skip_folders, verbose).await?;
    } else {
        let mut cg = TraceDecay::open(&project_path).await?;
        cg.add_skip_folders(&skip_folders);
        let spinner = Spinner::new();
        let sync_start = std::time::Instant::now();
        let result = cg
            .sync_with_progress_verbose(
                |current, total, detail| {
                    if current == 0 {
                        spinner.set_message(detail);
                    } else {
                        let elapsed = sync_start.elapsed().as_secs_f64();
                        let eta = if current > 1 {
                            let per_file = elapsed / (current - 1) as f64;
                            let remaining = per_file * (total - current) as f64;
                            if remaining >= 1.0 {
                                format!(" (ETA: {remaining:.0}s)")
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        };
                        spinner.set_message(&format!("[{current}/{total}] syncing {detail}{eta}"));
                    }
                },
                |msg| {
                    if verbose {
                        eprintln!("  \x1b[2m[verbose]\x1b[0m {msg}");
                    }
                },
            )
            .await?;
        let skipped_msg = if result.skipped_paths.is_empty() {
            String::new()
        } else {
            format!(", {} skipped", result.skipped_paths.len())
        };
        spinner.done(&format!(
            "sync done — {} added, {} modified, {} removed{skipped_msg} in {}ms",
            result.files_added, result.files_modified, result.files_removed, result.duration_ms
        ));
        if !result.skipped_paths.is_empty() {
            eprintln!();
            eprintln!(
                "\x1b[33mSkipped ({}) — files found but not readable:\x1b[0m",
                result.skipped_paths.len()
            );
            for (path, reason) in &result.skipped_paths {
                eprintln!("  ! {path}: {reason}");
            }
        }
        if doctor {
            print_sync_doctor(&result);
        }
        global::update_global_db(&cg).await;
    }

    maybe_print_parallel_update_notice(version_handle);
    Ok(())
}

pub(crate) fn handle_upload_counter(enable: bool) {
    let mut config = tracedecay::user_config::UserConfig::load();
    config.upload_enabled = enable;
    config.save();
    if enable {
        eprintln!("Worldwide counter upload enabled.");
    } else {
        eprintln!(
            "Worldwide counter upload disabled. You can re-enable with `tracedecay enable-upload-counter`."
        );
    }
}

pub(crate) fn handle_gitignore(
    path: Option<String>,
    action: Option<String>,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(path);
    let mut config = tracedecay::config::load_config(&project_path)?;
    match action.as_deref() {
        Some("on") => {
            config.git_ignore = true;
            tracedecay::config::save_config(&project_path, &config)?;
            eprintln!("gitignore enabled — .gitignore rules will be respected during indexing.");
            eprintln!("Run `tracedecay sync` to re-index with the new setting.");
        }
        Some("off") => {
            config.git_ignore = false;
            tracedecay::config::save_config(&project_path, &config)?;
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
    let cg = crate::serve::ensure_initialized(&project_path).await?;

    let opts = tracedecay::bench::BenchOptions {
        format: if json {
            tracedecay::bench::OutputFormat::Json
        } else {
            tracedecay::bench::OutputFormat::Markdown
        },
        max_nodes,
    };

    let report = match queries {
        Some(path) => tracedecay::bench::run_bench(&cg, std::path::Path::new(&path), opts).await?,
        None => {
            tracedecay::bench::run_bench_with_toml(
                &cg,
                tracedecay::bench::DEFAULT_QUERIES_TOML,
                opts,
            )
            .await?
        }
    };

    if json {
        println!("{}", tracedecay::bench::format_report_json(&report));
    } else {
        print!("{}", tracedecay::bench::format_report_console(&report));
    }
    Ok(())
}

fn maybe_print_parallel_update_notice(version_handle: std::thread::JoinHandle<Option<String>>) {
    if let Ok(Some(latest)) = version_handle.join() {
        let current_version = env!("CARGO_PKG_VERSION");
        let now = crate::current_unix_timestamp();
        let mut config = tracedecay::user_config::UserConfig::load();
        config.cached_latest_version = latest.clone();
        config.last_version_check_at = now;
        config.save_if_exists();
        if tracedecay::cloud::is_newer_version(current_version, &latest)
            && now - config.last_version_warning_at >= 900
        {
            eprintln!(
                "\n\x1b[33mUpdate available: v{} → v{}\x1b[0m\n  Run: \x1b[1mtracedecay upgrade\x1b[0m",
                current_version, latest
            );
            config.last_version_warning_at = now;
            config.save_if_exists();
        }
    }
}

/// Initializes a new project (if needed) and runs a full index.
pub(crate) async fn init_and_index(
    project_path: &Path,
    skip_folders: &[String],
    verbose: bool,
) -> tracedecay::errors::Result<TraceDecay> {
    debug_assert!(
        project_path.is_dir(),
        "init_and_index: project_path is not a directory"
    );
    debug_assert!(
        project_path.is_absolute(),
        "init_and_index: project_path must be absolute"
    );
    let mut cg = if TraceDecay::is_initialized(project_path) {
        TraceDecay::open(project_path).await?
    } else {
        let cg = TraceDecay::init(project_path).await?;
        eprintln!("Initialized TraceDecay at {}", project_path.display());
        let data_dir_name = tracedecay::config::get_tracedecay_dir(project_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(tracedecay::config::TRACEDECAY_DIR)
            .to_string();
        // Offer to add the resolved data directory to .gitignore if needed.
        if !tracedecay::config::is_in_gitignore(project_path) {
            if io::stdin().is_terminal() {
                eprint!("Add {data_dir_name} to .gitignore? [Y/n] ");
                io::stderr().flush().ok();
                let mut answer = String::new();
                if io::stdin().lock().read_line(&mut answer).is_ok() {
                    let answer = answer.trim();
                    if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
                        tracedecay::config::add_to_gitignore(project_path);
                        eprintln!("Added {data_dir_name} to .gitignore");
                    }
                }
            } else {
                eprintln!(
                    "Non-interactive: skipped adding {data_dir_name} to .gitignore (run interactively to opt in)."
                );
            }
        }
        cg
    };
    cg.add_skip_folders(skip_folders);
    let spinner = Spinner::new();
    let index_start = std::time::Instant::now();
    let result = cg
        .index_all_with_progress_verbose(
            |current, total, file| {
                let elapsed = index_start.elapsed().as_secs_f64();
                let eta = if current > 1 {
                    let per_file = elapsed / (current - 1) as f64;
                    let remaining = per_file * (total - current) as f64;
                    if remaining >= 1.0 {
                        format!(" (ETA: {remaining:.0}s)")
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                spinner.set_message(&format!("[{current}/{total}] indexing {file}{eta}"));
            },
            |msg| {
                if verbose {
                    eprintln!("  \x1b[2m[verbose]\x1b[0m {msg}");
                }
            },
        )
        .await?;
    spinner.done(&format!(
        "indexing done — {} files, {} nodes, {} edges in {}ms",
        result.file_count, result.node_count, result.edge_count, result.duration_ms
    ));
    global::update_global_db(&cg).await;
    Ok(cg)
}

/// Convert raw tokens-saved into a USD estimate using Sonnet input pricing.
/// Sonnet is the default agent target; output-token savings are not relevant
/// for retrieval savings.
pub(crate) fn estimate_dollars_saved(saved_tokens: u64) -> f64 {
    use tracedecay::accounting::pricing;
    pricing::refresh_if_stale();
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
    let gdb = match tracedecay::global_db::GlobalDb::open().await {
        Some(db) => db,
        None => {
            eprintln!("Could not open the global database (~/.tracedecay/global.db).");
            return Ok(());
        }
    };

    let since = tracedecay::accounting::metrics::parse_range(range);
    let project_filter: Option<String> = if all {
        None
    } else {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    };

    if history {
        let rows = gdb
            .savings_history(project_filter.as_deref(), since as i64)
            .await;
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

    let total = gdb
        .sum_savings(project_filter.as_deref(), since as i64)
        .await;
    let usd = estimate_dollars_saved(total.saved_tokens);

    if json_output {
        let out = serde_json::json!({
            "range": range,
            "project": project_filter.clone().unwrap_or_else(|| "ALL".to_string()),
            "saved_tokens": total.saved_tokens,
            "calls": total.calls,
            "usd": usd,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        tracedecay::display::print_gain_total(
            project_filter.as_deref().unwrap_or("ALL projects"),
            range,
            total.saved_tokens,
            total.calls,
            usd,
        );
    }
    Ok(())
}

/// Print the `--doctor` report after an incremental sync.
pub(crate) fn print_sync_doctor(result: &tracedecay::tracedecay::SyncResult) {
    let has_changes = !result.added_paths.is_empty()
        || !result.modified_paths.is_empty()
        || !result.removed_paths.is_empty();
    if !has_changes {
        eprintln!("\n\x1b[2mNo files changed.\x1b[0m");
        return;
    }
    eprintln!();
    if !result.added_paths.is_empty() {
        eprintln!("\x1b[32mAdded ({}):\x1b[0m", result.added_paths.len());
        for p in &result.added_paths {
            eprintln!("  + {p}");
        }
    }
    if !result.modified_paths.is_empty() {
        eprintln!("\x1b[33mModified ({}):\x1b[0m", result.modified_paths.len());
        for p in &result.modified_paths {
            eprintln!("  ~ {p}");
        }
    }
    if !result.removed_paths.is_empty() {
        eprintln!("\x1b[31mRemoved ({}):\x1b[0m", result.removed_paths.len());
        for p in &result.removed_paths {
            eprintln!("  - {p}");
        }
    }
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
