//! First-class CLI bindings for the catalogued Git read operations.
//!
//! These commands build the small reviewed request bodies accepted by the Git
//! application surface. They never accept an arbitrary Git command or open a
//! repository outside the daemon's admitted scope.

use serde_json::{Value, json};

use crate::cli::{GitAction, GitDiffScopeArg, GitProjectArgs};
use crate::{resolve_cli_project_root, tool_command::dispatch_catalogued_cli_operation};

pub(crate) async fn handle_git_action(action: GitAction) -> tracedecay::errors::Result<()> {
    match action {
        GitAction::Status { project } => {
            dispatch_git_read(
                tracedecay::application_surface::ApplicationSurfaceOperation::GitStatus,
                json!({}),
                project,
            )
            .await
        }
        GitAction::Diff {
            scope,
            base,
            head,
            project,
        } => {
            let payload = git_diff_payload(scope, base, head)?;
            dispatch_git_read(
                tracedecay::application_surface::ApplicationSurfaceOperation::GitDiff,
                payload,
                project,
            )
            .await
        }
        GitAction::History {
            count,
            path,
            follow,
            first_parent,
            project,
        } => {
            let payload = git_history_payload(count, path, follow, first_parent)?;
            dispatch_git_read(
                tracedecay::application_surface::ApplicationSurfaceOperation::GitHistory,
                payload,
                project,
            )
            .await
        }
        GitAction::Blame {
            path,
            follow_renames,
            project,
        } => {
            dispatch_git_read(
                tracedecay::application_surface::ApplicationSurfaceOperation::GitBlame,
                json!({
                    "path": path,
                    "follow_renames": follow_renames,
                }),
                project,
            )
            .await
        }
        GitAction::Hunks { scope, project } => {
            let scope = git_hunk_scope(scope)?;
            dispatch_git_read(
                tracedecay::application_surface::ApplicationSurfaceOperation::GitHunks,
                json!({ "scope": scope }),
                project,
            )
            .await
        }
    }
}

async fn dispatch_git_read(
    operation: tracedecay::application_surface::ApplicationSurfaceOperation,
    payload: Value,
    project: GitProjectArgs,
) -> tracedecay::errors::Result<()> {
    let GitProjectArgs {
        project,
        project_id,
        project_path,
        json,
    } = project;
    let project = resolve_cli_project_root(project, project_id, project_path).await?;
    dispatch_catalogued_cli_operation(operation, payload, Some(project), json).await
}

fn git_diff_payload(
    scope: GitDiffScopeArg,
    base: Option<String>,
    head: Option<String>,
) -> tracedecay::errors::Result<Value> {
    match scope {
        GitDiffScopeArg::WorkingTree => no_range_payload("working_tree", base, head),
        GitDiffScopeArg::Staged => no_range_payload("staged", base, head),
        GitDiffScopeArg::CommitRange => {
            let base = required_commit_range_bound("base", base)?;
            let head = required_commit_range_bound("head", head)?;
            Ok(json!({
                "scope": "commit_range",
                "base": base,
                "head": head,
            }))
        }
    }
}

fn git_hunk_scope(scope: GitDiffScopeArg) -> tracedecay::errors::Result<&'static str> {
    match scope {
        GitDiffScopeArg::WorkingTree => Ok("working_tree"),
        GitDiffScopeArg::Staged => Ok("staged"),
        GitDiffScopeArg::CommitRange => Err(config_error(
            "git hunks accepts only working-tree or staged scope; commit-range diffs cannot mint applicable hunk references",
        )),
    }
}

fn git_history_payload(
    count: u32,
    path: Option<String>,
    follow: bool,
    first_parent: bool,
) -> tracedecay::errors::Result<Value> {
    if follow && path.is_none() {
        return Err(config_error("--follow requires --path"));
    }
    let mut payload = json!({
        "count": count,
        "follow": follow,
        "first_parent": first_parent,
    });
    if let Some(path) = path
        && let Some(fields) = payload.as_object_mut()
    {
        fields.insert("path".to_owned(), Value::String(path));
    }
    Ok(payload)
}

fn no_range_payload(
    scope: &'static str,
    base: Option<String>,
    head: Option<String>,
) -> tracedecay::errors::Result<Value> {
    if base.is_some() || head.is_some() {
        return Err(config_error(
            "--base and --head are valid only with --scope commit-range",
        ));
    }
    Ok(json!({ "scope": scope }))
}

fn required_commit_range_bound(
    name: &str,
    value: Option<String>,
) -> tracedecay::errors::Result<String> {
    value
        .filter(|value| !value.trim().is_empty() && value.trim() == value)
        .ok_or_else(|| config_error(&format!("--{name} is required with --scope commit-range")))
}

fn config_error(message: &str) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use super::{git_diff_payload, git_history_payload, git_hunk_scope};
    use crate::cli::{Cli, Commands, GitAction, GitDiffScopeArg};

    #[test]
    fn git_diff_cli_accepts_an_exact_commit_range_then_builds_its_typed_request() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "git",
            "diff",
            "--scope",
            "commit-range",
            "--base",
            "a1b2c3d4",
            "--head",
            "e5f6a7b8",
            "--json",
        ])
        .expect("first-class git diff syntax");
        let Some(Commands::Git {
            action:
                GitAction::Diff {
                    scope,
                    base,
                    head,
                    project,
                },
        }) = cli.command
        else {
            panic!("expected the typed git diff command");
        };

        assert!(project.json);
        assert_eq!(
            git_diff_payload(scope, base, head).expect("typed commit range"),
            json!({
                "scope": "commit_range",
                "base": "a1b2c3d4",
                "head": "e5f6a7b8",
            })
        );
    }

    #[test]
    fn commit_range_diff_requires_both_exact_bounds() {
        let error = git_diff_payload(GitDiffScopeArg::CommitRange, Some("a".to_string()), None)
            .expect_err("head is mandatory for an exact commit range");

        assert!(error.to_string().contains("--head is required"));
    }

    #[test]
    fn hunks_reject_history_ranges() {
        let error = git_hunk_scope(GitDiffScopeArg::CommitRange)
            .expect_err("history ranges cannot produce applicable index hunks");

        assert!(
            error
                .to_string()
                .contains("cannot mint applicable hunk references")
        );
    }

    #[test]
    fn history_follow_requires_an_explicit_path() {
        let error = git_history_payload(100, None, true, false)
            .expect_err("Git cannot follow renames without a selected path");

        assert!(error.to_string().contains("--follow requires --path"));
    }

    #[test]
    fn history_omits_an_absent_optional_path_from_its_reviewed_request() {
        assert_eq!(
            git_history_payload(1, None, false, false).expect("default history request"),
            json!({
                "count": 1,
                "follow": false,
                "first_parent": false,
            })
        );
    }

    #[test]
    fn history_preserves_a_requested_file_path_in_its_reviewed_request() {
        assert_eq!(
            git_history_payload(1, Some("src/lib.rs".to_owned()), false, false)
                .expect("path-filtered history request"),
            json!({
                "count": 1,
                "path": "src/lib.rs",
                "follow": false,
                "first_parent": false,
            })
        );
    }
}
