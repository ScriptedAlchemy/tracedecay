//! Read-only branch snapshot inspection and PR autotrack configuration.

use std::path::{Path, PathBuf};

use crate::cli::BranchAction;

pub(crate) async fn handle_branch_action(action: BranchAction) -> tracedecay::errors::Result<()> {
    match action {
        BranchAction::List { path } => {
            let resolved =
                super::scope::resolve_project_scope(tracedecay::config::resolve_path(path)).await?;
            print_branch_snapshots(&resolved.project_path)?;
        }
        BranchAction::Autotrack { action } => {
            handle_branch_autotrack_action(action).await?;
        }
    }
    Ok(())
}

fn print_branch_snapshots(project_path: &Path) -> tracedecay::errors::Result<()> {
    let snapshots =
        tracedecay::branch::local_branch_snapshots(project_path).map_err(|message| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "failed to list git branch snapshots for '{}': {message}",
                    project_path.display()
                ),
            }
        })?;
    let current = tracedecay::branch::current_branch(project_path);
    for snapshot in snapshots {
        let marker = if current.as_deref() == Some(snapshot.name.as_str()) {
            '*'
        } else {
            ' '
        };
        eprintln!("{marker} {} {}", snapshot.name, snapshot.commit);
    }
    Ok(())
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
                    eprintln!("Tracked PR refs: none");
                } else {
                    eprintln!("Tracked PR refs:");
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
                "\x1b[32m✔\x1b[0m PR auto-tracking disabled. The daemon tears down managed PR worktrees and refs on its next poll cycle."
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
