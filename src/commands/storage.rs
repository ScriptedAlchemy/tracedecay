use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::global;

use super::daemon::daemon_tool_json;

fn strict_registered_project_paths(
    paths: Vec<PathBuf>,
) -> tracedecay::errors::Result<Vec<PathBuf>> {
    paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| strict_registered_project_path(path, index))
        .collect()
}

fn strict_registered_project_path(
    project_root: PathBuf,
    index: usize,
) -> tracedecay::errors::Result<PathBuf> {
    if project_root.as_os_str().is_empty() || !project_root.is_absolute() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "global registry project at index {index} has an invalid root: {project_root:?}"
            ),
        });
    }
    Ok(project_root)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod wipe_target_tests {
    use super::*;

    #[test]
    fn registered_project_path_preserves_an_absolute_root() {
        let root = std::env::current_dir().expect("test process must have a working directory");
        let path = strict_registered_project_path(root.clone(), 0)
            .expect("absolute registry roots are valid");
        assert_eq!(path, root);
    }

    #[test]
    fn registered_project_path_rejects_malformed_roots() {
        for root in ["", "relative/project"] {
            let error = strict_registered_project_path(PathBuf::from(root), 7)
                .expect_err("a malformed registry root must abort wipe discovery");
            assert!(
                error
                    .to_string()
                    .contains("global registry project at index 7"),
                "unexpected error: {error}"
            );
        }
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn registered_project_paths_preserve_non_unicode_roots() {
        let dir = tempfile::TempDir::new().expect("create temporary profile");
        #[cfg(unix)]
        let (first, second) = {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt as _;

            (
                dir.path().join(OsString::from_vec(vec![b'p', 0x80])),
                dir.path().join(OsString::from_vec(vec![b'p', 0x81])),
            )
        };
        #[cfg(windows)]
        let (first, second) = {
            use std::ffi::OsString;
            use std::os::windows::ffi::OsStringExt as _;

            (
                dir.path()
                    .join(OsString::from_wide(&[u16::from(b'p'), 0xd800])),
                dir.path()
                    .join(OsString::from_wide(&[u16::from(b'p'), 0xd801])),
            )
        };

        let runtime = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::profile(
            dir.path(),
        )
        .await
        .expect("open global database");
        runtime
            .upsert_code_project("proj_first", &first, None, None, None)
            .await
            .expect("register first project");
        runtime
            .upsert_code_project("proj_second", &second, None, None, None)
            .await
            .expect("register second project");

        let paths = strict_registered_project_paths(
            runtime
                .registered_project_paths_for_test()
                .await
                .expect("read registered project paths"),
        )
        .expect("list registered paths");
        assert!(paths.contains(&first));
        assert!(paths.contains(&second));
    }
}

/// Handles the `wipe` and `wipe --all` commands.
///
/// `assume_yes` carries the global `--yes` confirmation flag. It is the same
/// non-interactive acceptance every other destructive first-party command
/// takes, so a scripted caller no longer has to feed `go!` through a pipe on
/// stdin to reach the wipe.
pub(crate) async fn handle_wipe(all: bool, assume_yes: bool) -> tracedecay::errors::Result<()> {
    use std::fs;
    let profile_root = tracedecay::storage::default_profile_root()?;
    let home_tracedecay = Some(profile_root.clone());
    let lifecycle_lease =
        tracedecay::lifecycle_lease::acquire_exclusive_for_profile(&profile_root, "wipe")?;
    let _database_scope =
        tracedecay::db::enter_maintenance_database_scope(&lifecycle_lease, &profile_root, "wipe")?;
    let registry =
        tracedecay::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime::try_open_existing(&profile_root)
            .await?;

    // `--all` must enumerate the registry only after exclusive maintenance
    // ownership is active. A strict local query avoids both the daemon
    // discovery-to-lease race and any failure-to-empty fallback before global.db
    // is eligible for removal.
    let project_paths = if all {
        match registry.as_ref() {
            Some(registry) => {
                strict_registered_project_paths(registry.registered_project_paths().await?)?
            }
            None => Vec::new(),
        }
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
        if location.status.is_live() {
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
            tracedecay::errors::TraceDecayError::Config {
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

    if all {
        drop(registry);
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
        if let Some(registry) = registry.as_ref() {
            registry.delete_project_paths(&wiped_paths).await?;
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
    let mut total_size: u64 = 0;
    let mut total_tokens: u64 = 0;

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
        total_size = total_size.saturating_add(size);
        total_tokens = total_tokens.saturating_add(tokens.unwrap_or(0));
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
    total_tokens = rows.iter().filter_map(|row| row.tokens).sum();

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
        tracedecay::display::format_bytes(total_size),
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
    let report = tracedecay::global_db::registry_maintenance::inspect_profile_store_orphans(
        profile_root,
        tracedecay::tracedecay::current_timestamp(),
    );
    for plan in report.plans {
        if plan.status
            != tracedecay::global_db::registry_maintenance::RegistryOrphanRelinkStatus::Eligible
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
