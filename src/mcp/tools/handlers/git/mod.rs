//! Git-backed tool handlers.
//!
//! `shell` owns every `git` subprocess call; the other siblings turn its output
//! into tool payloads. This module holds the shared imports (siblings pick them
//! up through `use super::*`), the two shapes `shell` returns, and the argument
//! helpers used across siblings.

mod affected;
mod branch;
mod context;
mod shell;

pub(crate) use affected::collect_affected_test_files;
pub(super) use affected::handle_affected;
pub(super) use branch::{
    handle_admin_branch_add, handle_branch_diff, handle_branch_list, handle_branch_search,
};
pub(super) use context::{
    handle_changelog, handle_commit_context, handle_diff_context, handle_pr_context,
};

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};

use super::super::ToolResult;
use super::support::{generic_tool_result, require_object_args, unique_file_paths};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitFileChange {
    path: String,
    status: &'static str,
}

struct GitPrComparison {
    merge_base: String,
    changes: Vec<GitFileChange>,
    commits: Vec<Value>,
}

fn git_error_result(cg: &TraceDecay, args: &Value, operation: &str, message: &str) -> ToolResult {
    let output = json!({
        "error": {
            "kind": "git",
            "operation": operation,
            "message": message,
        }
    });
    generic_tool_result(Some(cg.project_root()), args, &output, vec![])
        .with_semantic_error(true)
        .with_failure_message(message)
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
        crate::tracedecay::is_test_file(path) || files_with_inline_tests.contains(path)
    }
}
