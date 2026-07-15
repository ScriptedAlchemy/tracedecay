//! File editing tool handlers: `str_replace`, `multi_str_replace`, `insert_at`,
//! `ast_grep_rewrite`.

use serde::Serialize;
use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;
use crate::types::{AstGrepResult, EditResult, InsertResult, MultiEditResult};

use super::super::ToolResult;
use super::super::render;

fn missing_required_param(name: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("missing required parameter: {name}"),
    }
}

fn required_str<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| missing_required_param(name))
}

fn required_array<'a>(args: &'a Value, name: &str) -> Result<&'a [Value]> {
    args.get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| missing_required_param(name))
}

/// Common shape shared by every edit-tool result payload: a handler-known
/// `success` flag plus a human-readable `message` describing the outcome
/// (e.g. "`old_str` not found in file" / "`old_str` matches 3 times"). Lets
/// [`text_tool_result`] attach a structural semantic-error marker (and its
/// reason) to the [`ToolResult`] instead of the dispatcher having to guess
/// outcome from rendered — possibly markdown, not JSON — response text.
trait EditOutcome {
    fn success(&self) -> bool;
    fn message(&self) -> &str;
    fn file_path(&self) -> &str;
}

impl EditOutcome for EditResult {
    fn success(&self) -> bool {
        self.success
    }
    fn message(&self) -> &str {
        &self.message
    }
    fn file_path(&self) -> &str {
        &self.file_path
    }
}

impl EditOutcome for MultiEditResult {
    fn success(&self) -> bool {
        self.success
    }
    fn message(&self) -> &str {
        &self.message
    }
    fn file_path(&self) -> &str {
        &self.file_path
    }
}

impl EditOutcome for InsertResult {
    fn success(&self) -> bool {
        self.success
    }
    fn message(&self) -> &str {
        &self.message
    }
    fn file_path(&self) -> &str {
        &self.file_path
    }
}

impl EditOutcome for AstGrepResult {
    fn success(&self) -> bool {
        self.success
    }
    fn message(&self) -> &str {
        &self.message
    }
    fn file_path(&self) -> &str {
        &self.file_path
    }
}

/// Reads the shared `dry_run` edit flag (default `false`): when set, an edit
/// primitive validates and computes the resulting content but writes nothing,
/// returning a preview diff instead.
fn dry_run_arg(args: &Value) -> bool {
    args.get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Reads the shared `verify` edit flag (default `false`): when set, a real
/// (non-dry-run) successful edit re-runs file-scoped diagnostics and attaches a
/// compact verdict to the result. Off by default to keep edits fast; compound
/// refactor tools are expected to default it on.
fn verify_arg(args: &Value) -> bool {
    args.get("verify").and_then(Value::as_bool).unwrap_or(false)
}

/// Files considered "touched" for downstream bookkeeping. A dry run writes
/// nothing and a failure changes nothing, so only a real successful edit
/// reports its file.
fn edit_touched_files(result: &impl EditOutcome, dry_run: bool) -> Vec<String> {
    if result.success() && !dry_run {
        vec![result.file_path().to_string()]
    } else {
        vec![]
    }
}

/// Post-edit verification loop. Runs file-scoped diagnostics over the edited
/// file and returns a compact verdict (`clean` / `errors` with the first few
/// error messages). Returns `None` if diagnostics could not run — verification
/// is best-effort and never fails an edit that already applied.
async fn run_edit_verification(cg: &TraceDecay, file_path: &str) -> Option<Value> {
    let scope = crate::diagnostics::Scope::File {
        path: file_path.to_string(),
    };
    let diagnostics = crate::diagnostics::run_all(cg.project_root(), &scope)
        .await
        .ok()?;

    let mut error_count = 0usize;
    let mut warning_count = 0usize;
    let mut first_errors: Vec<Value> = Vec::new();
    for diag in &diagnostics {
        if diag.file != file_path {
            continue;
        }
        match diag.level.as_str() {
            "error" => {
                error_count += 1;
                if first_errors.len() < 3 {
                    first_errors.push(json!({
                        "line": diag.line_start,
                        "code": diag.code,
                        "message": diag.message,
                    }));
                }
            }
            "warning" => warning_count += 1,
            _ => {}
        }
    }

    Some(json!({
        "verdict": if error_count == 0 { "clean" } else { "errors" },
        "error_count": error_count,
        "warning_count": warning_count,
        "first_errors": first_errors,
    }))
}

async fn text_tool_result<T: Serialize + EditOutcome>(
    cg: &TraceDecay,
    args: &Value,
    result: &T,
    touched_files: Vec<String>,
    dry_run: bool,
    verify: bool,
) -> ToolResult {
    let success = result.success();
    let mut value = serde_json::to_value(result).unwrap_or_default();

    // Verification only makes sense for a real (written) successful edit: a dry
    // run changed nothing on disk, and a failure left the file as-is.
    if verify
        && !dry_run
        && success
        && let Some(verdict) = run_edit_verification(cg, result.file_path()).await
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert("verification".to_string(), verdict);
    }

    let text = render::finalize(Some(cg.project_root()), args, &value, || {
        render::generic_md(&value)
    });
    let tool_result = ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    )
    .with_semantic_error(!success);
    if success {
        tool_result
    } else {
        tool_result.with_failure_message(result.message())
    }
}

pub(super) async fn handle_str_replace(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let old_str = required_str(&args, "old_str")?;
    let new_str = required_str(&args, "new_str")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    let result = cg.str_replace(path, old_str, new_str, dry_run).await?;
    let touched_files = edit_touched_files(&result, dry_run);
    Ok(text_tool_result(cg, &args, &result, touched_files, dry_run, verify).await)
}

pub(super) async fn handle_multi_str_replace(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let replacements = required_array(&args, "replacements")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    let parsed_replacements: Vec<(&str, &str)> = replacements
        .iter()
        .filter_map(|pair| {
            let arr = pair.as_array()?;
            if arr.len() != 2 {
                return None;
            }
            let old = arr[0].as_str()?;
            let new = arr[1].as_str()?;
            Some((old, new))
        })
        .collect();

    if parsed_replacements.len() != replacements.len() {
        return Err(TraceDecayError::Config {
            message: "each replacement must be an array of exactly 2 strings".to_string(),
        });
    }

    let result = cg
        .multi_str_replace(path, &parsed_replacements, dry_run)
        .await?;
    let touched_files = edit_touched_files(&result, dry_run);
    Ok(text_tool_result(cg, &args, &result, touched_files, dry_run, verify).await)
}

pub(super) async fn handle_insert_at(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let anchor = required_str(&args, "anchor")?;
    let content = required_str(&args, "content")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    let before = args.get("before").and_then(Value::as_bool).unwrap_or(false);

    let result = cg.insert_at(path, anchor, content, before, dry_run).await?;
    let touched_files = edit_touched_files(&result, dry_run);
    Ok(text_tool_result(cg, &args, &result, touched_files, dry_run, verify).await)
}

pub(super) async fn handle_replace_symbol(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let symbol = required_str(&args, "symbol")?;
    let new_source = required_str(&args, "new_source")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    let result = cg.replace_symbol(symbol, new_source, dry_run).await?;
    let touched_files = edit_touched_files(&result, dry_run);
    Ok(text_tool_result(cg, &args, &result, touched_files, dry_run, verify).await)
}

pub(super) async fn handle_insert_at_symbol(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let symbol = required_str(&args, "symbol")?;
    let content = required_str(&args, "content")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);
    let position = args
        .get("position")
        .and_then(|v| v.as_str())
        .unwrap_or("after");

    let result = cg
        .insert_at_symbol(symbol, content, position, dry_run)
        .await?;
    let touched_files = edit_touched_files(&result, dry_run);
    Ok(text_tool_result(cg, &args, &result, touched_files, dry_run, verify).await)
}

pub(super) async fn handle_move_symbol(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let symbol = required_str(&args, "symbol")?;
    let dest_file = required_str(&args, "dest_file")?;
    // The impact report is the product; applying is opt-in.
    let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(true);
    let update_references = args
        .get("update_references")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let result = cg
        .move_symbol(symbol, dest_file, dry_run, update_references)
        .await?;

    let success = result.success;
    let touched_files = if success && !dry_run {
        vec![result.source_file.clone(), result.dest_file.clone()]
    } else {
        vec![]
    };
    let value = serde_json::to_value(&result).unwrap_or_default();
    let text = render::finalize(Some(cg.project_root()), &args, &value, || {
        move_result_md(&result)
    });
    let tool_result = ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    )
    .with_semantic_error(!success);
    Ok(if success {
        tool_result
    } else {
        tool_result.with_failure_message(&result.message)
    })
}

/// Human-readable markdown for a move result: the outcome line, applied
/// imports, the impact report (the centerpiece), and the preview diff.
fn move_result_md(result: &crate::types::MoveResult) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let verb = if result.dry_run {
        "Would move"
    } else {
        "Moved"
    };
    let _ = writeln!(
        out,
        "## {verb} `{}`\n\n{} → {}\n\n{}",
        result.symbol, result.source_file, result.dest_file, result.message
    );
    if !result.applied_imports.is_empty() {
        out.push_str("\n### Auto-inserted imports (destination)\n");
        for imp in &result.applied_imports {
            let _ = writeln!(out, "- `{}`", imp.trim());
        }
    }
    out.push_str("\n### Impact\n");
    if result.impact.is_empty() {
        out.push_str("Clean move — no references, dependencies, or module concerns detected.\n");
    } else {
        for hint in &result.impact {
            let loc = hint
                .line
                .map_or_else(|| hint.file.clone(), |l| format!("{}:{}", hint.file, l));
            let _ = writeln!(out, "- **{}** ({}) — {}", hint.kind, loc, hint.detail);
            if let Some(sug) = &hint.suggestion {
                let _ = writeln!(out, "  - suggestion: {sug}");
            }
        }
    }
    if let Some(diff) = &result.diff {
        let _ = write!(out, "\n### Preview diff\n```diff\n{diff}\n```\n");
    }
    out
}

pub(super) async fn handle_ast_grep_rewrite(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let pattern = required_str(&args, "pattern")?;
    let rewrite = required_str(&args, "rewrite")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    let result = cg.ast_grep_rewrite(path, pattern, rewrite, dry_run).await?;
    let touched_files = edit_touched_files(&result, dry_run);
    Ok(text_tool_result(cg, &args, &result, touched_files, dry_run, verify).await)
}
