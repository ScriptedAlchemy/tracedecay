//! Git-backed graph-tool computations.
//!
//! `shell` owns every `git` subprocess call; the other siblings turn its output
//! into typed tool results. This module holds the shared imports (siblings pick
//! them up through `use super::*`), the comparison shape `shell` returns, the
//! typed git refusal, and the semantic failure message each refusal renders
//! with.
//!
//! Every authority the family reads arrives through [`McpToolContext`]: the
//! admitted project route, the caller's deadline and cancellation, the
//! registered project session store that authenticates PR-context cursors,
//! and the daemon-owned code-index executors. Nothing here opens a store,
//! resolves a project, or mints an authorization for itself.

mod affected;
mod branch;
mod context;
mod pr_context_cursor;
mod shell;
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod test_support;

pub use affected::compute_affected;
pub use branch::{compute_branch_diff, compute_branch_list, compute_branch_search};
pub use context::{
    compute_changelog, compute_commit_context, compute_diff_context, compute_pr_context,
};

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    BranchDiffResultV1, BranchListResultV1, BranchSearchResultV1, ChangelogResultV1,
    CommitContextResultV1, GitCommitSubjectV1, GitFileChangeStatusV1, GitFileChangeV1,
    GitFileRoleV1, GitToolErrorKindV1, GitToolErrorV1, GitToolFailureV1, GitToolOperationV1,
    PrContextResultV1,
};

use super::graph::graph_tool_completion;
use super::support::{decode_primitive_request, unique_file_paths};
use crate::tool_context::McpToolContext;
use tracedecay_domain::errors::{Result, TraceDecayError};

struct GitPrComparison {
    base_oid: String,
    head_oid: String,
    merge_base: String,
    changes: Vec<GitFileChangeV1>,
    commits: Vec<GitCommitSubjectV1>,
}

fn git_failure(operation: GitToolOperationV1, message: String) -> GitToolFailureV1 {
    GitToolFailureV1 {
        error: GitToolErrorV1 {
            kind: GitToolErrorKindV1::Git,
            operation,
            message,
        },
    }
}

/// The failure message a git-context result renders as a semantic tool
/// error, or `None` when the result is an answer.
pub fn git_tool_failure_message(result: &GraphToolResultV1) -> Option<String> {
    match result {
        GraphToolResultV1::Changelog(ChangelogResultV1::GitFailure(failure))
        | GraphToolResultV1::CommitContext(CommitContextResultV1::GitFailure(failure))
        | GraphToolResultV1::PrContext(PrContextResultV1::GitFailure(failure)) => {
            Some(failure.error.message.clone())
        }
        GraphToolResultV1::BranchList(BranchListResultV1::Unavailable(_)) => {
            Some("local branch snapshots are unavailable".to_owned())
        }
        GraphToolResultV1::BranchSearch(BranchSearchResultV1::ReferenceUnavailable(
            unavailable,
        )) => Some(format!(
            "branch '{}' does not resolve to a local commit",
            unavailable.branch
        )),
        GraphToolResultV1::BranchDiff(BranchDiffResultV1::ReferenceUnavailable(unavailable)) => {
            Some(format!(
                "branch '{}' does not resolve to a local commit",
                unavailable.base_or_head
            ))
        }
        GraphToolResultV1::BranchSearch(BranchSearchResultV1::SearchUnavailable(unavailable)) => {
            Some(format!(
                "branch '{}' search is unavailable for commit {}: {}",
                unavailable.branch, unavailable.source_revision, unavailable.reason
            ))
        }
        GraphToolResultV1::BranchDiff(BranchDiffResultV1::DiffUnavailable(unavailable)) => {
            Some(format!(
                "branch diff {}..{} is unavailable: {}",
                unavailable.base, unavailable.head, unavailable.reason
            ))
        }
        _ => None,
    }
}

/// Applies a caller's depth to its default and the family's traversal bound.
fn clamped_depth(depth: Option<u32>, default: usize, max: usize) -> usize {
    depth.map_or(default, |depth| {
        usize::try_from(depth).map_or(max, |depth| depth.min(max))
    })
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
    use serde_json::json;

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
            let result = context::compute_pr_context(
                &ctx,
                async move { Err::<tracedecay_graph_query::VerifiedGraphQuery, _>(error) },
                json!({"base_ref": "main", "head_ref": "HEAD"}),
            )
            .await;
            assert!(
                result.is_err(),
                "terminal graph failure must not become partial success: {detail}"
            );
        }
    }
}
