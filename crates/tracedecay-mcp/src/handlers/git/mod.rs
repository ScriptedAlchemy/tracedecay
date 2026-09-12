//! Git-backed tool handlers.
//!
//! `shell` owns every `git` subprocess call; the other siblings turn its output
//! into tool payloads. This module holds the shared imports (siblings pick them
//! up through `use super::*`), the two shapes `shell` returns, and the argument
//! helpers used across siblings.
//!
//! Every authority the family reads arrives through [`McpToolContext`]: the
//! admitted project route, the caller's deadline and cancellation, the
//! registered project session store that authenticates PR-context cursors,
//! and the daemon-owned code-index executors. Nothing here opens a store,
//! resolves a project, or mints an authorization for itself.

mod affected;
mod branch;
mod context;
mod dispatch;
mod pr_context_cursor;
mod shell;
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod test_support;

pub use affected::handle_affected;
pub use branch::{handle_branch_diff, handle_branch_list, handle_branch_search};
pub use context::{
    handle_changelog, handle_commit_context, handle_diff_context, handle_pr_context,
};
pub use dispatch::dispatch_tool;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};

use super::support::{generic_tool_result, require_object_args, unique_file_paths};
use crate::ToolResult;
use crate::tool_context::McpToolContext;
use tracedecay_domain::errors::{Result, TraceDecayError};

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
struct GitFileChange {
    path: String,
    status: &'static str,
}

struct GitPrComparison {
    base_oid: String,
    head_oid: String,
    merge_base: String,
    changes: Vec<GitFileChange>,
    commits: Vec<Value>,
}

fn git_error_result(
    ctx: &McpToolContext<'_>,
    args: &Value,
    operation: &str,
    message: &str,
) -> ToolResult {
    let output = json!({
        "error": {
            "kind": "git",
            "operation": operation,
            "message": message,
        }
    });
    generic_tool_result(Some(ctx.project_root()), args, &output, vec![])
        .with_semantic_error(true)
        .with_failure_message(message)
}

/// Typed result returned when a git-dispatched tool exhausts the dispatch
/// deadline the daemon carried into `dispatch_git_tools`.
///
/// Git tree walks, revwalks, diffs, and the branch-add index build are
/// unbounded on pathological or diverged inputs. When the carried deadline
/// elapses the caller must receive the same shaped, semantic error every other
/// git failure surfaces — never a bare hang or a panic.
pub fn git_dispatch_deadline_result(ctx: &McpToolContext<'_>, tool_name: &str) -> ToolResult {
    let message =
        format!("git tool '{tool_name}' exceeded its dispatch deadline and was cancelled");
    git_error_result(ctx, &json!({ "tool": tool_name }), "deadline", &message)
}

fn require_string_array_arg(args: &Value, name: &str) -> Result<Vec<String>> {
    args.get(name)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                .collect()
        })
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("missing required parameter: {name} (array of strings)"),
        })
}

fn clamped_depth_arg(args: &Value, name: &str, default: usize, max: usize) -> usize {
    args.get(name)
        .and_then(serde_json::Value::as_u64)
        .map_or(default, |v| v.min(max as u64) as usize)
}

fn matches_test_file(
    path: &str,
    custom_glob: Option<&glob::Pattern>,
    files_with_inline_tests: &HashSet<String>,
) -> bool {
    if let Some(glob) = custom_glob {
        glob.matches(path)
    } else {
        tracedecay_code_index::is_test_file(path) || files_with_inline_tests.contains(path)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::test_support::{branched_repository, fixture_context, fixture_project};
    use super::*;

    /// A terminal graph failure must reach the caller as an error. Degrading
    /// it into a partial PR context would publish git evidence while claiming
    /// the graph enrichment simply found nothing.
    #[tokio::test]
    async fn pr_context_propagates_terminal_graph_failures() {
        let repo = branched_repository();
        let project = fixture_project(repo.path());
        let ctx = fixture_context(&project);
        let terminal_errors = [
            tracedecay_graph_query::map_code_graph_read_runtime_error(
                tracedecay_graph_query::CodeGraphReadError::Cancelled,
            ),
            tracedecay_graph_query::map_code_graph_read_runtime_error(
                tracedecay_graph_query::CodeGraphReadError::Denied,
            ),
            tracedecay_graph_query::map_code_graph_read_runtime_error(
                tracedecay_graph_query::CodeGraphReadError::Corrupt {
                    detail: "corrupt projection".to_owned(),
                },
            ),
            tracedecay_graph_query::map_code_graph_read_runtime_error(
                tracedecay_graph_query::CodeGraphReadError::ResetRequired {
                    detail: "generation reset required".to_owned(),
                },
            ),
            tracedecay_graph_query::map_code_graph_read_runtime_error(
                tracedecay_graph_query::CodeGraphReadError::InvalidRequest {
                    detail: "invalid graph request".to_owned(),
                },
            ),
            TraceDecayError::Config {
                message: "graph configuration is invalid".to_owned(),
            },
        ];

        for error in terminal_errors {
            let detail = error.to_string();
            let result = context::handle_pr_context(
                &ctx,
                async move { Err::<tracedecay_graph_query::VerifiedGraphQuery, _>(error) },
                json!({"base_ref": "main", "head_ref": "HEAD", "format": "json"}),
            )
            .await;
            assert!(
                result.is_err(),
                "terminal graph failure must not become partial success: {detail}"
            );
        }
    }

    /// An elapsed dispatch deadline surfaces as the same shaped semantic git
    /// failure every other git error uses, so a caller never sees a bare hang.
    #[test]
    fn an_elapsed_dispatch_deadline_is_a_typed_semantic_failure() {
        let project = fixture_project(std::path::Path::new("/unread"));
        let result =
            git_dispatch_deadline_result(&fixture_context(&project), "tracedecay_pr_context");

        assert_eq!(result.semantic_error(), Some(true));
        let message = result.failure_message().unwrap_or_default();
        assert!(message.contains("tracedecay_pr_context"), "got {message:?}");
        assert!(message.contains("dispatch deadline"), "got {message:?}");
    }
}
