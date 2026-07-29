#[cfg(unix)]
use std::path::{Path, PathBuf};

use crate::Spinner;
use crate::cli::BranchAction;

use super::daemon::daemon_tool_json;

fn branch_list_rpc_args() -> serde_json::Value {
    serde_json::json!({
        "format": "json",
        "include_branch_diagnostics": true,
        "include_storage_health": false,
        "include_session_ingest": false,
        "include_staleness": false,
    })
}

pub(crate) async fn handle_branch_action(action: BranchAction) -> tracedecay::errors::Result<()> {
    use tracedecay::branch;
    use tracedecay::branch_meta;

    match action {
        BranchAction::List { path } => {
            let resolved =
                super::scope::resolve_project_scope(tracedecay::config::resolve_path(path)).await?;
            let status = daemon_tool_json(
                Some(&resolved.project_path),
                "tracedecay_status",
                branch_list_rpc_args(),
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
            let resolved =
                super::scope::resolve_project_scope(tracedecay::config::resolve_path(path)).await?;
            let branch_name = match name {
                Some(n) => n,
                None => branch::current_branch(&resolved.project_path).ok_or_else(|| {
                    tracedecay::errors::TraceDecayError::Config {
                        message:
                            "cannot detect current branch (detached HEAD?). Specify a branch name."
                                .to_string(),
                    }
                })?,
            };

            let spinner = Spinner::new();
            spinner.set_message("syncing changes");
            let response = daemon_tool_json(
                Some(&resolved.project_path),
                "tracedecay_admin_branch_add",
                serde_json::json!({ "branch": branch_name }),
            )
            .await?;
            match parse_daemon_branch_add_outcome(&response)? {
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
            let resolved =
                super::scope::resolve_project_scope(tracedecay::config::resolve_path(path)).await?;
            let response = daemon_tool_json(
                Some(&resolved.project_path),
                "tracedecay_admin_branch",
                serde_json::json!({ "action": "remove", "branch": name }),
            )
            .await?;
            let report = parse_daemon_branch_admin_report(&response)?;
            match report.outcome {
                branch::BranchAdminOutcome::NoTracking => {
                    eprintln!("No branch tracking configured.");
                }
                branch::BranchAdminOutcome::NotTracked => {
                    eprintln!("Branch '{name}' is not tracked.");
                }
                branch::BranchAdminOutcome::Removed => {
                    eprintln!("\x1b[32m✔\x1b[0m Branch '{name}' removed.");
                }
                branch::BranchAdminOutcome::NoChanges => {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: "daemon branch remove returned no_changes".to_string(),
                    });
                }
            }
        }
        BranchAction::Removeall { path } => {
            let resolved =
                super::scope::resolve_project_scope(tracedecay::config::resolve_path(path)).await?;
            let response = daemon_tool_json(
                Some(&resolved.project_path),
                "tracedecay_admin_branch",
                serde_json::json!({ "action": "remove_all" }),
            )
            .await?;
            let report = parse_daemon_branch_admin_report(&response)?;
            match report.outcome {
                branch::BranchAdminOutcome::NoTracking => {
                    eprintln!("No branch tracking configured.");
                }
                branch::BranchAdminOutcome::NoChanges => {
                    eprintln!("No non-default branches to remove.");
                }
                branch::BranchAdminOutcome::Removed => {
                    for name in &report.removed_branches {
                        eprintln!("  removed '{name}'");
                    }
                    eprintln!(
                        "\x1b[32m✔\x1b[0m Removed {} branch(es). Only '{}' remains.",
                        report.removed_branches.len(),
                        report.default_branch.as_deref().unwrap_or("<unknown>")
                    );
                }
                branch::BranchAdminOutcome::NotTracked => {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: "daemon branch remove_all returned not_tracked".to_string(),
                    });
                }
            }
        }
        BranchAction::Gc { path } => {
            let resolved =
                super::scope::resolve_project_scope(tracedecay::config::resolve_path(path)).await?;
            let response = daemon_tool_json(
                Some(&resolved.project_path),
                "tracedecay_admin_branch",
                serde_json::json!({ "action": "gc" }),
            )
            .await?;
            let report = parse_daemon_branch_admin_report(&response)?;
            if report.outcome == branch::BranchAdminOutcome::Removed {
                for name in &report.removed_branches {
                    eprintln!("  removed '{name}'");
                }
                for path in &report.removed_orphan_dbs {
                    eprintln!("  removed orphan '{}'", path.display());
                }
                eprintln!(
                    "\x1b[32m✔\x1b[0m Cleaned up {} stale branch(es) and {} orphan database(s).",
                    report.removed_branches.len(),
                    report.removed_orphan_dbs.len()
                );
            } else {
                eprintln!("No stale branches or orphan databases to clean up.");
            }
        }
        BranchAction::Autotrack { action } => {
            handle_branch_autotrack_action(action).await?;
        }
    }
    Ok(())
}

fn parse_daemon_branch_admin_report(
    response: &serde_json::Value,
) -> tracedecay::errors::Result<tracedecay::branch::BranchAdminReport> {
    serde_json::from_value(response.clone()).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("invalid daemon branch administration response: {error}"),
        }
    })
}

fn parse_daemon_branch_add_outcome(
    response: &serde_json::Value,
) -> tracedecay::errors::Result<tracedecay::branch::BranchAddOutcome> {
    match response.get("outcome").and_then(serde_json::Value::as_str) {
        Some("not_indexed") => Ok(tracedecay::branch::BranchAddOutcome::NotIndexed),
        Some("already_tracked") => Ok(tracedecay::branch::BranchAddOutcome::AlreadyTracked),
        Some("added") => Ok(tracedecay::branch::BranchAddOutcome::Added),
        Some("deferred") => Ok(tracedecay::branch::BranchAddOutcome::Deferred),
        Some(outcome) => Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon branch add returned unknown outcome: {outcome}"),
        }),
        None => Err(tracedecay::errors::TraceDecayError::Config {
            message: "daemon branch add response omitted outcome".to_string(),
        }),
    }
}

/// Reads or mutates the project-scoped `sync.auto_track_pr_branches` setting and
/// reports the daemon's PR-autotrack status for a project.
async fn handle_branch_autotrack_action(
    action: crate::cli::BranchAutotrackAction,
) -> tracedecay::errors::Result<()> {
    use crate::cli::BranchAutotrackAction;
    use tracedecay::config::MIN_AUTO_TRACK_PR_POLL_SECS;

    match action {
        BranchAutotrackAction::Status { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let configuration = tracedecay::config::cached_runtime_configuration(&project_path)?;
            let sync = &configuration.config.sync;
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
                let data_root = resolve_branch_data_root(&project_path).await?;
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
            let current = tracedecay::config::cached_runtime_configuration(&project_path)?;
            let mut config = current.config.clone();
            config.sync.auto_track_pr_branches = true;
            if let Some(secs) = poll_secs {
                config.sync.auto_track_pr_poll_secs = secs.max(MIN_AUTO_TRACK_PR_POLL_SECS);
            }
            let updated =
                tracedecay::config::mutate_pinned_runtime_configuration(&current, config).await?;
            eprintln!(
                "\x1b[32m✔\x1b[0m PR auto-tracking enabled (poll every {}s). Restart the daemon (`tracedecay daemon restart`) to apply.",
                updated.config.sync.effective_auto_track_pr_poll_secs()
            );
        }
        BranchAutotrackAction::Disable { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let current = tracedecay::config::cached_runtime_configuration(&project_path)?;
            let mut config = current.config.clone();
            config.sync.auto_track_pr_branches = false;
            tracedecay::config::mutate_pinned_runtime_configuration(&current, config).await?;
            eprintln!(
                "\x1b[32m✔\x1b[0m PR auto-tracking disabled. The daemon tears down any managed PR worktrees, refs, synthetic branches and stores on its next poll cycle."
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn resolve_branch_data_root(project_path: &Path) -> tracedecay::errors::Result<PathBuf> {
    Ok(
        tracedecay::tracedecay::TraceDecay::resolve_store_layout_for_identity(project_path)
            .await?
            .data_root,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        branch_list_rpc_args, parse_daemon_branch_add_outcome, parse_daemon_branch_admin_report,
    };

    #[test]
    fn branch_list_requests_expensive_diagnostics_explicitly() {
        assert_eq!(
            branch_list_rpc_args(),
            serde_json::json!({
                "format": "json",
                "include_branch_diagnostics": true,
                "include_storage_health": false,
                "include_session_ingest": false,
                "include_staleness": false,
            })
        );
    }

    #[test]
    fn daemon_branch_add_outcomes_are_strictly_decoded() {
        for (name, expected) in [
            (
                "not_indexed",
                tracedecay::branch::BranchAddOutcome::NotIndexed,
            ),
            (
                "already_tracked",
                tracedecay::branch::BranchAddOutcome::AlreadyTracked,
            ),
            ("added", tracedecay::branch::BranchAddOutcome::Added),
            ("deferred", tracedecay::branch::BranchAddOutcome::Deferred),
        ] {
            assert_eq!(
                parse_daemon_branch_add_outcome(&serde_json::json!({ "outcome": name }))
                    .expect("known daemon outcome"),
                expected,
            );
        }
    }

    #[test]
    fn daemon_branch_add_response_must_include_known_outcome() {
        let missing = parse_daemon_branch_add_outcome(&serde_json::json!({}))
            .expect_err("missing outcome must fail closed");
        assert!(missing.to_string().contains("omitted outcome"));

        let unknown = parse_daemon_branch_add_outcome(&serde_json::json!({ "outcome": "other" }))
            .expect_err("unknown outcome must fail closed");
        assert!(unknown.to_string().contains("unknown outcome"));
    }

    #[test]
    fn daemon_branch_admin_report_is_strictly_typed() {
        let report = parse_daemon_branch_admin_report(&serde_json::json!({
            "outcome": "removed",
            "removed_branches": ["feature/a"],
            "removed_orphan_dbs": ["branches/orphan.db"],
            "default_branch": "main"
        }))
        .expect("valid branch admin response");
        assert_eq!(
            report.outcome,
            tracedecay::branch::BranchAdminOutcome::Removed
        );
        assert_eq!(report.removed_branches, vec!["feature/a"]);
        assert_eq!(
            report.removed_orphan_dbs,
            vec![std::path::PathBuf::from("branches/orphan.db")]
        );
        assert_eq!(report.default_branch.as_deref(), Some("main"));
    }

    #[test]
    fn daemon_branch_admin_report_rejects_unknown_or_missing_outcome() {
        for response in [
            serde_json::json!({}),
            serde_json::json!({ "outcome": "surprise" }),
        ] {
            let error = parse_daemon_branch_admin_report(&response)
                .expect_err("malformed branch admin response must fail closed");
            assert!(
                error
                    .to_string()
                    .contains("invalid daemon branch administration response")
            );
        }
    }
}
