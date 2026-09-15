use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::global;

use super::daemon::daemon_tool_json;

const PROFILE_SQLITE_DATABASES: [&str; 3] = ["global.db", "user-sessions.db", "user-memory.db"];
const PROFILE_DATABASE_PATHS: [&str; 8] = [
    "projects",
    "stores",
    "remote",
    "user-sessions.grafeo",
    "user-sessions.grafeo.wal",
    "user-memory.grafeo",
    "user-memory.grafeo.wal",
    ".user-sessions.db.host-admission",
];

fn sqlite_family_member(database: &Path, suffix: &str) -> PathBuf {
    let mut member = database.as_os_str().to_os_string();
    member.push(suffix);
    member.into()
}

fn wipe_io(
    operation: &str,
    path: &Path,
    error: &std::io::Error,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("failed to {operation} '{}': {error}", path.display()),
    }
}

fn validate_complete_wipe_profile_root(
    profile_root: &Path,
    user_home: Option<&Path>,
) -> tracedecay_domain::errors::Result<()> {
    if !profile_root.is_absolute() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "complete profile wipe requires an absolute profile root, got '{}'",
                profile_root.display()
            ),
        });
    }
    let metadata = std::fs::symlink_metadata(profile_root)
        .map_err(|error| wipe_io("inspect complete-wipe profile root", profile_root, &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "complete profile wipe root '{}' must be a regular directory, not a symlink",
                profile_root.display()
            ),
        });
    }
    let canonical = profile_root.canonicalize().map_err(|error| {
        wipe_io(
            "canonicalize complete-wipe profile root",
            profile_root,
            &error,
        )
    })?;
    if canonical != profile_root || canonical.parent().is_none() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "complete profile wipe root '{}' must be an exact canonical non-filesystem-root directory",
                profile_root.display()
            ),
        });
    }
    if let Some(user_home) = user_home {
        let canonical_home = user_home
            .canonicalize()
            .map_err(|error| wipe_io("canonicalize user home", user_home, &error))?;
        if canonical_home.starts_with(&canonical) {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "complete profile wipe root '{}' must not be the user home or one of its ancestors",
                    profile_root.display()
                ),
            });
        }
    }
    Ok(())
}

fn remove_fixed_profile_path(
    profile_root: &Path,
    name: &str,
) -> tracedecay_domain::errors::Result<bool> {
    use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_directory};

    let path = profile_root.join(name);
    tracedecay_runtime_core::storage::reject_symlink_components(
        &path,
        "profile database wipe target",
    )
    .map_err(|error| wipe_io("validate wipe target", &path, &error))?;
    let removed = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => {
            // `remove_dir_all` does not follow directory symlinks. The exact
            // fixed child and every existing parent were also rejected above
            // when symlinked, while the exclusive lifecycle lease prevents a
            // cooperating TraceDecay writer from racing this private profile.
            std::fs::remove_dir_all(&path)
                .map_err(|error| wipe_io("remove profile database directory", &path, &error))?;
            true
        }
        Ok(metadata) if metadata.is_file() => {
            tracedecay_runtime_core::storage::PrivateStoreIo::remove_file_durable(&path)
                .map_err(|error| wipe_io("remove profile database file", &path, &error))?
        }
        Ok(_) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "profile database wipe target '{}' is not a regular file or directory",
                    path.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(wipe_io("inspect profile database path", &path, &error)),
    };
    sync_directory(profile_root, DirectorySyncPolicy::Strict)
        .map_err(|error| wipe_io("sync profile after database removal", profile_root, &error))?;
    Ok(removed)
}

fn verify_wipe_path_absent(path: &Path) -> tracedecay_domain::errors::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "wipe did not remove expected namespace entry '{}'",
                path.display()
            ),
        }),
        Err(error) => Err(wipe_io("verify wipe path namespace absence", path, &error)),
    }
}

fn remove_local_wipe_directory(path: &Path) -> tracedecay_domain::errors::Result<bool> {
    use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_directory};

    tracedecay_runtime_core::storage::reject_symlink_components(path, "local wipe target")
        .map_err(|error| wipe_io("validate local wipe target", path, &error))?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(path)
                .map_err(|error| wipe_io("remove local wipe directory", path, &error))?;
            let parent = path.parent().ok_or_else(|| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("local wipe target '{}' has no parent", path.display()),
                }
            })?;
            sync_directory(parent, DirectorySyncPolicy::Strict)
                .map_err(|error| wipe_io("sync local wipe parent", parent, &error))?;
            verify_wipe_path_absent(path)?;
            Ok(true)
        }
        Ok(_) => Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "local wipe target '{}' is not a regular directory",
                path.display()
            ),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(wipe_io("inspect local wipe target", path, &error)),
    }
}

fn wipe_complete_profile_database_state(
    profile_root: &Path,
) -> tracedecay_domain::errors::Result<usize> {
    let mut removed = 0usize;
    for name in PROFILE_DATABASE_PATHS {
        removed += usize::from(remove_fixed_profile_path(profile_root, name)?);
    }
    for database in PROFILE_SQLITE_DATABASES {
        let database = profile_root.join(database);
        for suffix in ["-wal", "-shm", "-journal", ""] {
            let member = sqlite_family_member(&database, suffix);
            removed += usize::from(
                tracedecay_runtime_core::storage::PrivateStoreIo::remove_file_durable(&member)
                    .map_err(|error| {
                        wipe_io("remove profile SQLite family member", &member, &error)
                    })?,
            );
        }
    }
    for name in PROFILE_DATABASE_PATHS {
        verify_wipe_path_absent(&profile_root.join(name))?;
    }
    for database in PROFILE_SQLITE_DATABASES {
        let database = profile_root.join(database);
        for suffix in ["-wal", "-shm", "-journal", ""] {
            verify_wipe_path_absent(&sqlite_family_member(&database, suffix))?;
        }
    }
    Ok(removed)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod wipe_safety_tests {
    use super::*;

    #[test]
    fn complete_wipe_accepts_an_exact_canonical_profile_directory() {
        let profile = tempfile::TempDir::new().expect("create temporary profile");
        let profile = profile
            .path()
            .canonicalize()
            .expect("canonicalize temporary profile");
        let home = profile
            .parent()
            .expect("temporary profile has a parent")
            .to_path_buf();

        validate_complete_wipe_profile_root(&profile, Some(&home))
            .expect("exact private profile root is safe to wipe");
    }

    #[test]
    fn complete_wipe_rejects_relative_user_home_and_filesystem_roots() {
        let profile = tempfile::TempDir::new().expect("create temporary profile");
        let profile = profile
            .path()
            .canonicalize()
            .expect("canonicalize temporary profile");
        let filesystem_root = profile
            .ancestors()
            .last()
            .expect("absolute path has a filesystem root");

        for (candidate, home) in [
            (Path::new("relative-profile"), None),
            (profile.as_path(), Some(profile.as_path())),
            (
                profile.parent().expect("temporary profile has a parent"),
                Some(profile.as_path()),
            ),
            (filesystem_root, None),
        ] {
            assert!(
                validate_complete_wipe_profile_root(candidate, home).is_err(),
                "dangerous complete-wipe root was admitted: {}",
                candidate.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn complete_wipe_rejects_a_leaf_symlink() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::TempDir::new().expect("create temporary parent");
        let profile = parent.path().join("profile");
        let profile_link = parent.path().join("profile-link");
        let dangling = parent.path().join("dangling");
        std::fs::create_dir(&profile).expect("create real profile");
        symlink(&profile, &profile_link).expect("create profile symlink");
        symlink(parent.path().join("missing"), &dangling).expect("create dangling symlink");

        assert!(validate_complete_wipe_profile_root(&profile_link, None).is_err());
        assert!(
            verify_wipe_path_absent(&dangling).is_err(),
            "a dangling symlink remains a namespace entry"
        );
    }
}

/// Handles the `wipe` and `wipe --all` commands.
///
/// `assume_yes` carries the global `--yes` confirmation flag. It is the same
/// non-interactive acceptance every other destructive first-party command
/// takes, so a scripted caller no longer has to feed `go!` through a pipe on
/// stdin to reach the wipe.
#[hotpath::measure(label = "cli.wipe.run", future = true)]
pub(crate) async fn handle_wipe(
    all: bool,
    assume_yes: bool,
) -> tracedecay_domain::errors::Result<()> {
    handle_wipe_inner(all, assume_yes).await
}

fn handle_wipe_inner(
    all: bool,
    assume_yes: bool,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = tracedecay_domain::errors::Result<()>> + Send + 'static>,
> {
    // Erase the deeply nested wipe future before it reaches the measured
    // wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let profile_root = tracedecay_runtime_core::storage::default_profile_root()?;
        let home_tracedecay = Some(profile_root.clone());
        if all {
            validate_complete_wipe_profile_root(
                &profile_root,
                tracedecay::agents::home_dir().as_deref(),
            )?;
        }
        let lifecycle_lease =
            tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
                &profile_root,
                "wipe",
            )?;
        let _database_scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
            &lifecycle_lease,
            &profile_root,
            "wipe",
        )?;
        let registry = if all {
            None
        } else {
            tracedecay::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime::try_open_existing(&profile_root)
            .await?
        };

        // A complete wipe is deliberately schema-independent: the databases may
        // be corrupt or newer than this binary, and opening the state we are about
        // to destroy would make the recovery command unavailable. Local wipes keep
        // their registry-backed target classification and ledger cleanup.
        let project_paths = if all {
            Vec::new()
        } else {
            global::gather_target_projects(false, &home_tracedecay).await?
        };
        let mut targets = Vec::new();
        for path in &project_paths {
            let location = global::classify_project_storage_with_registry(
                path,
                registry.as_ref(),
                home_tracedecay.as_deref(),
            )
            .await?;
            // A prior partial wipe may have removed the profile shard while a
            // marker deletion failed. Keep that marker-backed target selectable so
            // the same command can finish the cleanup without deleting its registry
            // retry authority on the failed attempt.
            if location.status.is_live() || location.marker_root.is_some() {
                targets.push(location);
            }
        }

        if !all && targets.is_empty() {
            eprintln!("No tracedecay projects found in current folder, parents, or children.");
            return Ok(());
        }

        global::print_flash_warning(all, &targets);

        if assume_yes {
            eprintln!("\x1b[33m--yes supplied — proceeding without the interactive prompt.\x1b[0m");
        } else {
            eprint!("Type \x1b[1;32mgo!\x1b[0m to confirm (anything else aborts): ");
            io::stderr().flush().ok();
            let mut answer = String::new();
            io::stdin().lock().read_line(&mut answer).map_err(|e| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("failed to read stdin: {e}"),
                }
            })?;
            if answer.trim() != "go!" {
                eprintln!("\x1b[33mAborted — nothing was wiped.\x1b[0m");
                if !io::stdin().is_terminal() {
                    eprintln!("(no terminal on stdin — pass --yes to confirm a scripted wipe)");
                }
                return Ok(());
            }
        }

        if all {
            let removed = wipe_complete_profile_database_state(&profile_root)?;
            eprintln!();
            eprintln!(
                "\x1b[32mWiped complete profile database state ({removed} filesystem entries).\x1b[0m"
            );
            return Ok(());
        }

        let mut removed = 0usize;
        let mut failures = Vec::new();
        let mut wiped_paths: Vec<PathBuf> = Vec::new();
        let mut marker_cleanup = Vec::new();

        for location in &targets {
            match remove_local_wipe_directory(&location.data_root) {
                Ok(_) => {
                    wiped_paths.push(location.project_root.clone());
                    marker_cleanup.push(location);
                }
                Err(error) => {
                    eprintln!(
                        "  \x1b[31m✗\x1b[0m {} ({error})",
                        location.data_root.display()
                    );
                    failures.push(format!("{} ({error})", location.data_root.display()));
                }
            }
        }

        // Keep repository markers intact until the registry transaction succeeds.
        // If it fails after a shard was removed, the marker remains the durable
        // local discovery authority for a retry.
        if !wiped_paths.is_empty() {
            if let Some(registry) = registry.as_ref() {
                registry.delete_project_paths(&wiped_paths).await?;
            }
        }

        for location in marker_cleanup {
            if let Some(marker_root) = &location.marker_root
                && let Err(error) = remove_local_wipe_directory(marker_root)
            {
                eprintln!("  \x1b[31m✗\x1b[0m {} ({error})", marker_root.display());
                failures.push(format!("{} ({error})", marker_root.display()));
                continue;
            }
            removed += 1;
            eprintln!(
                "  \x1b[32m✔\x1b[0m wiped {}",
                location.project_root.display()
            );
        }

        if !failures.is_empty() {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "local wipe failed for {} selected target(s): {}",
                    failures.len(),
                    failures.join("; ")
                ),
            });
        }

        eprintln!();
        eprintln!("\x1b[32mWiped {removed} project(s).\x1b[0m");
        Ok(())
    })
}

/// Handles the `list` and `list --all` commands.
#[hotpath::measure(label = "cli.list.run", future = true)]
pub(crate) async fn handle_list(all: bool) -> tracedecay_domain::errors::Result<()> {
    handle_list_inner(all).await
}

fn handle_list_inner(
    all: bool,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = tracedecay_domain::errors::Result<()>> + Send + 'static>,
> {
    // Erase the deeply nested list future before it reaches the measured
    // wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        use tracedecay_runtime_core::text::format_token_count;

        let home_tracedecay = tracedecay::config::user_data_dir();
        let project_paths = global::gather_target_projects(all, &home_tracedecay).await?;

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
        let mut token_errors: Vec<String> = Vec::new();

        for path in &project_paths {
            let mut location = global::classify_project_storage(path);
            if location.status == global::ProjectStorageStatus::Stale
                && let Some(profile_root) = home_tracedecay.as_deref()
            {
                let context = daemon_tool_json(
                    None,
                    "tracedecay_admin_cli",
                    serde_json::json!({
                        "action": "registry_context",
                        "project_arg": path,
                    }),
                )
                .await?;
                if let Some(store) = context
                    .get("stores")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| entry.get("store"))
                    .find(|store| {
                        store.get("store_kind").and_then(serde_json::Value::as_str)
                            == Some("code_project")
                    })
                    && let Some(registry_location) =
                        global::classify_registry_storage_value(path, profile_root, store)
                {
                    location = registry_location;
                }
            }
            let has_data = location.data_root.exists();
            let size = if has_data {
                global::tracedecay_dir_size(&location.data_root)
            } else {
                0
            };
            let project_key =
            tracedecay::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime::canonical_project_key(path);
            let token_row = token_rows.iter().find(|row| {
            row.get("project")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| {
                    tracedecay::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime::canonical_project_key(
                        Path::new(value),
                    ) == project_key
                })
        });
            // `None` is a total this run could not read, which is not the same
            // answer as a project that has saved nothing.
            let tokens = token_row
                .and_then(|row| row.get("tokens"))
                .and_then(serde_json::Value::as_u64);
            if tokens.is_none() {
                let reason = token_row
                    .and_then(|row| row.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("no token total reported for this project");
                token_errors.push(format!("{}: {reason}", path.display()));
            }
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

        let total_size: u64 = rows.iter().map(|row| row.size).sum();
        let total_tokens: u64 = rows.iter().filter_map(|row| row.tokens).sum();

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
                tracedecay_runtime_core::text::format_bytes(r.size)
            } else {
                "—".to_string()
            };
            let tokens_str = match r.tokens {
                None => "unavailable".to_string(),
                Some(0) => "—".to_string(),
                Some(tokens) => format_token_count(tokens),
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
        let unreadable = rows.iter().filter(|row| row.tokens.is_none()).count();
        let total_suffix = if unreadable == 0 {
            String::new()
        } else {
            format!(" (excludes {unreadable} project(s) with unavailable totals)")
        };
        println!(
            "Total: {} on disk · {} tokens saved{}",
            tracedecay_runtime_core::text::format_bytes(total_size),
            total_tokens_str,
            total_suffix
        );
        if !token_errors.is_empty() {
            eprintln!();
            eprintln!(
                "Token totals could not be read for {} project(s):",
                token_errors.len()
            );
            for error in &token_errors {
                eprintln!("  {error}");
            }
        }
        Ok(())
    })
}

#[derive(Debug)]
struct ListRow {
    path: std::path::PathBuf,
    status_label: &'static str,
    has_data: bool,
    size: u64,
    /// `None` when this run could not read the project's saved-token total.
    tokens: Option<u64>,
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
        .map(|path| {
            tracedecay::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime::canonical_project_key(path)
        })
        .collect();
    let report = tracedecay_global_db::registry_maintenance::inspect_profile_store_orphans(
        profile_root,
        tracedecay::tracedecay::current_timestamp(),
    );
    for plan in report.plans {
        if plan.status
            != tracedecay_global_db::registry_maintenance::RegistryOrphanRelinkStatus::Eligible
        {
            continue;
        }
        let key = tracedecay::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime::canonical_project_key(
            &plan.project.project_root,
        );
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
            tokens: Some(0),
        });
    }
}
