//! Git/diff/branch tool definitions.

use serde_json::Value;

use super::def;
use crate::ToolDefinition;

pub(super) fn def_affected(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_affected",
        "Affected Tests",
        "Which tests to run, run affected tests, tests impacted by a change. Find test files affected by changed source files via dependency graph traversal.",
        input_schema,
    )
}

pub(super) fn def_diff_context(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_diff_context",
        "Diff Context",
        "Given changed file paths, return semantic context: which symbols were modified, what depends on them, and affected tests.",
        input_schema,
    )
}

pub(super) fn def_changelog(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_changelog",
        "Changelog",
        "git log, git history, git blame, diff between refs. Generate a semantic diff/changelog between two git refs, categorizing symbols as added, removed, or modified.",
        input_schema,
    )
}

pub(super) fn def_commit_context(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_commit_context",
        "Commit Context",
        "git diff, git status, git log style, staged changes for a commit message. Semantic summary of uncommitted changes for drafting a commit message. Returns changed symbols, file roles, and recent commit style.",
        input_schema,
    )
}

pub(super) fn def_pr_context(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_pr_context",
        "PR Context",
        "Semantic summary of changes between two git refs for drafting a pull request description.",
        input_schema,
    )
}

pub(super) fn def_branch_search(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_branch_search",
        "Cross-Branch Search",
        "Search the immutable code-index generation sealed for a local branch's exact current commit.",
        input_schema,
    )
}

pub(super) fn def_branch_diff(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_branch_diff",
        "Branch Diff",
        "Compare immutable code-index generations sealed for two local branches' exact commits.",
        input_schema,
    )
}

pub(super) fn def_branch_list(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_branch_list",
        "List Tracked Branches",
        "List a bounded snapshot of exact local branch refs and their current commit identities.",
        input_schema,
    )
}
