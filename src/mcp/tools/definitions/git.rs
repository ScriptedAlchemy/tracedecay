//! Git/diff/branch tool definitions.

use serde_json::json;

use super::def;
use crate::mcp::tools::ToolDefinition;

pub(super) fn def_affected() -> ToolDefinition {
    def(
        "tracedecay_affected",
        "Affected Tests",
        "Which tests to run, run affected tests, tests impacted by a change. Find test files affected by changed source files via dependency graph traversal.",
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of changed file paths to analyze"
                },
                "depth": {
                    "type": "number",
                    "description": "Maximum dependency traversal depth (default: 5)"
                },
                "filter": {
                    "type": "string",
                    "description": "Custom glob pattern for test files (default: common test patterns)"
                }
            },
            "required": ["files"]
        }),
    )
}

pub(super) fn def_diff_context() -> ToolDefinition {
    def(
        "tracedecay_diff_context",
        "Diff Context",
        "Given changed file paths, return semantic context: which symbols were modified, what depends on them, and affected tests.",
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of changed file paths"
                },
                "depth": {
                    "type": "number",
                    "description": "Maximum impact traversal depth (default: 2)"
                }
            },
            "required": ["files"]
        }),
    )
}

pub(super) fn def_changelog() -> ToolDefinition {
    def(
        "tracedecay_changelog",
        "Changelog",
        "git log, git history, git blame, diff between refs. Generate a semantic diff/changelog between two git refs, categorizing symbols as added, removed, or modified.",
        json!({
            "type": "object",
            "properties": {
                "from_ref": {
                    "type": "string",
                    "description": "Starting git ref (commit, branch, tag)"
                },
                "to_ref": {
                    "type": "string",
                    "description": "Ending git ref (commit, branch, tag)"
                }
            },
            "required": ["from_ref", "to_ref"]
        }),
    )
}

pub(super) fn def_commit_context() -> ToolDefinition {
    def(
        "tracedecay_commit_context",
        "Commit Context",
        "git diff, git status, git log style, staged changes for a commit message. Semantic summary of uncommitted changes for drafting a commit message. Returns changed symbols, file roles, and recent commit style.",
        json!({
            "type": "object",
            "properties": {
                "staged_only": {
                    "type": "boolean",
                    "description": "If true, only analyze staged changes (default: false = all uncommitted changes)"
                }
            }
        }),
    )
}

pub(super) fn def_pr_context() -> ToolDefinition {
    def(
        "tracedecay_pr_context",
        "PR Context",
        "Semantic summary of changes between two git refs for drafting a pull request description.",
        json!({
            "type": "object",
            "properties": {
                "base_ref": {
                    "type": "string",
                    "description": "Base branch or ref to compare against (default: detected repository default branch)"
                },
                "head_ref": {
                    "type": "string",
                    "description": "Head branch or ref (default: 'HEAD')"
                }
            }
        }),
    )
}

pub(super) fn def_branch_search() -> ToolDefinition {
    def(
        "tracedecay_branch_search",
        "Cross-Branch Search",
        "Search the exact snapshot selected by another branch or ref in the project graph.",
        json!({
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Existing local branch or ref to search"
                },
                "query": {
                    "type": "string",
                    "description": "Search query string to match against symbol names"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 10)"
                }
            },
            "required": ["branch", "query"]
        }),
    )
}

pub(super) fn def_branch_diff() -> ToolDefinition {
    def(
        "tracedecay_branch_diff",
        "Branch Diff",
        "Compare the code graphs of two branches. Shows symbols added, removed, and changed (signature differs) between base and head.",
        json!({
            "type": "object",
            "properties": {
                "base": {
                    "type": "string",
                    "description": "Exact base branch or ref (e.g. 'main')"
                },
                "head": {
                    "type": "string",
                    "description": "Head branch name (e.g. 'feature/foo'). Defaults to the current branch."
                },
                "file": {
                    "type": "string",
                    "description": "Optional file path filter — only show diffs for symbols in this file"
                },
                "kind": {
                    "type": "string",
                    "description": "Optional kind filter — only show diffs for this symbol kind (e.g. 'function', 'struct')"
                }
            },
            "required": ["base"]
        }),
    )
}

pub(super) fn def_branch_list() -> ToolDefinition {
    def(
        "tracedecay_branch_list",
        "List Branch Snapshots",
        "List local branches and their exact commit snapshots in the project graph.",
        json!({
            "type": "object",
            "properties": {}
        }),
    )
}
