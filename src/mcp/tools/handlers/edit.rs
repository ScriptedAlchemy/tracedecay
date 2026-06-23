//! File editing tool handlers: `str_replace`, `multi_str_replace`, `insert_at`,
//! `ast_grep_rewrite`.

use serde::Serialize;
use serde_json::{json, Value};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;

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

fn required_array<'a>(args: &'a Value, name: &str) -> Result<&'a Vec<Value>> {
    args.get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| missing_required_param(name))
}

fn text_tool_result<T: Serialize>(result: &T, touched_files: Vec<String>) -> ToolResult {
    ToolResult {
        value: json!({
            "content": [{ "type": "text", "text": serde_json::to_string(result).unwrap_or_default() }]
        }),
        touched_files,
    }
}

pub(super) async fn handle_str_replace(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let old_str = required_str(&args, "old_str")?;
    let new_str = required_str(&args, "new_str")?;

    let result = cg.str_replace(path, old_str, new_str).await?;
    let touched_files = vec![result.file_path.clone()];
    Ok(text_tool_result(&result, touched_files))
}

pub(super) async fn handle_multi_str_replace(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let replacements = required_array(&args, "replacements")?;

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

    let result = cg.multi_str_replace(path, &parsed_replacements).await?;
    let touched_files = vec![result.file_path.clone()];
    Ok(text_tool_result(&result, touched_files))
}

pub(super) async fn handle_insert_at(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let anchor = required_str(&args, "anchor")?;
    let content = required_str(&args, "content")?;

    let before = args
        .get("before")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let result = cg.insert_at(path, anchor, content, before).await?;
    let touched_files = vec![result.file_path.clone()];
    Ok(text_tool_result(&result, touched_files))
}

pub(super) async fn handle_replace_symbol(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let symbol = required_str(&args, "symbol")?;
    let new_source = required_str(&args, "new_source")?;

    let result = cg.replace_symbol(symbol, new_source).await?;
    let touched_files = if result.success {
        vec![result.file_path.clone()]
    } else {
        vec![]
    };
    Ok(text_tool_result(&result, touched_files))
}

pub(super) async fn handle_insert_at_symbol(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let symbol = required_str(&args, "symbol")?;
    let content = required_str(&args, "content")?;
    let position = args
        .get("position")
        .and_then(|v| v.as_str())
        .unwrap_or("after");

    let result = cg.insert_at_symbol(symbol, content, position).await?;
    let touched_files = if result.success {
        vec![result.file_path.clone()]
    } else {
        vec![]
    };
    Ok(text_tool_result(&result, touched_files))
}

pub(super) async fn handle_ast_grep_rewrite(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let pattern = required_str(&args, "pattern")?;
    let rewrite = required_str(&args, "rewrite")?;

    let result = cg.ast_grep_rewrite(path, pattern, rewrite).await?;
    let touched_files = if result.success {
        vec![result.file_path.clone()]
    } else {
        vec![]
    };
    Ok(text_tool_result(&result, touched_files))
}
