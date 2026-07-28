//! Shared helpers for MCP tool handlers.
//!
//! Keep this module free of tool dispatch logic. Handler modules use it for
//! argument normalization, scope filtering, and registered-project selection.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use super::super::ToolResult;
use super::super::render;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{CodeProjectRecord, ProjectRegistryContext, RegisteredGlobalDb};

/// Trimmed, non-empty string argument by key, or `None` when absent, non-string,
/// or blank after trimming.
pub(super) fn string_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Builds a `Config` error from a message, for argument-validation failures.
pub(super) fn argument_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

/// Wraps a JSON payload in a text `ToolResult`, rendering the default-format
/// (markdown) body with a caller-supplied closure. The `format:"json"` path is
/// unaffected — [`render::finalize`] serializes `value` compactly there.
pub(super) fn tool_json_with_md<F: FnOnce() -> String>(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
    md: F,
) -> ToolResult {
    let text = render::finalize(project_root, args, value, md);
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        Vec::new(),
    )
}

/// Wraps a JSON payload in a text `ToolResult`, rendering the default markdown
/// body via [`render::generic_md`]. Convenience wrapper around
/// [`tool_json_with_md`] for handlers that don't need a custom markdown
/// renderer.
pub(super) fn tool_json(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    tool_json_with_md(project_root, args, value, || render::generic_md(value))
}

/// Extracts the `node_id` parameter from tool arguments, accepting `id` as a
/// fallback alias. LLMs occasionally shorten `node_id` to `id`; this avoids a
/// confusing error when that happens.
pub(super) fn require_node_id(args: &Value) -> Result<&str> {
    args.get("node_id")
        .or_else(|| args.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: node_id".to_string(),
        })
}

/// Returns the user-provided `path` argument, falling back to the scope
/// prefix when the argument is absent. This makes listing tools
/// automatically scoped to the subdirectory the server was launched from.
pub(super) fn effective_path<'a>(
    args: &'a Value,
    scope_prefix: Option<&'a str>,
) -> Option<&'a str> {
    args.get("path").and_then(|v| v.as_str()).or(scope_prefix)
}

/// Returns string elements from an optional JSON array argument.
pub(super) fn string_array_values(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Filters a Vec of items by file path prefix when a scope is active.
/// Returns the vec unchanged when `scope_prefix` is `None`.
pub(super) fn filter_by_scope<T, F>(
    items: Vec<T>,
    scope_prefix: Option<&str>,
    get_path: F,
) -> Vec<T>
where
    F: Fn(&T) -> &str,
{
    items
        .into_iter()
        .filter(|item| crate::path_scope::path_matches_scope(get_path(item), scope_prefix))
        .collect()
}

/// Deduplicates an iterator of file path strings into a `Vec<String>`.
pub(super) fn unique_file_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for p in paths {
        if seen.insert(p) {
            result.push(p.to_string());
        }
    }
    result
}

pub(super) fn safe_profile_relpath(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(TraceDecayError::Config {
            message: format!("registry artifact path is not a safe profile-relative path: {value}"),
        });
    }
    Ok(path)
}

pub(super) fn profile_root_for_global_db(
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<PathBuf> {
    if let Some(global_db) = global_db {
        return global_db
            .db_path()
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| TraceDecayError::Config {
                message: "could not resolve tracedecay profile root".to_string(),
            });
    }
    Err(TraceDecayError::Config {
        message: "client project registry is unavailable for selector resolution".to_string(),
    })
}

pub(super) fn project_selector_present(args: &Value, top_level_path_keys: &[&str]) -> bool {
    args.get("project_selector").is_some()
        || args.get("project_id").is_some()
        || top_level_path_keys
            .iter()
            .any(|key| args.get(*key).is_some())
}

pub(super) async fn project_registry_context(
    args: &Value,
    top_level_path_keys: &[&str],
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<Option<ProjectRegistryContext>> {
    let selector_present = project_selector_present(args, top_level_path_keys);
    let selector = args
        .get("project_selector")
        .map(|value| {
            value.as_object().ok_or_else(|| TraceDecayError::Config {
                message: "project_selector must be an object".to_string(),
            })
        })
        .transpose()?;
    let project_id = selector
        .and_then(|selector| selector.get("project_id"))
        .or_else(|| args.get("project_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let project_path = selector
        .and_then(|selector| {
            selector
                .get("path")
                .or_else(|| selector.get("project_path"))
        })
        .or_else(|| top_level_path_keys.iter().find_map(|key| args.get(*key)))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if project_id.is_none() && project_path.is_none() {
        if selector_present {
            return Err(TraceDecayError::Config {
                message: "project selector must include project_id or project_path".to_string(),
            });
        }
        return Ok(None);
    }

    let db = match global_db {
        Some(db) => db,
        None => {
            return Err(TraceDecayError::Config {
                message: "client project registry is unavailable for selector resolution"
                    .to_string(),
            });
        }
    };
    let context = resolve_project_registry_context(db, project_id, project_path).await?;

    context
        .ok_or_else(|| unresolved_project_selector_error(project_id, project_path))
        .map(Some)
}

async fn resolve_project_registry_context(
    db: &RegisteredGlobalDb,
    project_id: Option<&str>,
    project_path: Option<&str>,
) -> Result<Option<ProjectRegistryContext>> {
    if let Some(project_id) = project_id {
        return db.project_registry_context_by_id(project_id).await;
    }
    let Some(project_path) = project_path else {
        return Ok(None);
    };
    let selector_path = Path::new(project_path);
    if let Some(store) = db
        .try_resolve_project_store_record_by_alias(selector_path)
        .await?
    {
        return db.project_registry_context_by_id(&store.project_id).await;
    }
    if is_explicit_project_path_selector(project_path) {
        let git_common_dir = crate::worktree::git_common_dir(selector_path);
        if let Some(resolution) = db
            .resolve_project_store_by_identity(selector_path, git_common_dir.as_deref())
            .await?
        {
            return db
                .project_registry_context_by_id(&resolution.project.project_id)
                .await;
        }
        let canonical_path = selector_path
            .canonicalize()
            .unwrap_or_else(|_| selector_path.to_path_buf());
        for parent in canonical_path.ancestors().skip(1) {
            if let Some(store) = db
                .try_resolve_project_store_record_by_alias(parent)
                .await?
            {
                return db.project_registry_context_by_id(&store.project_id).await;
            }
        }
    }
    let Some(basename) = bare_project_name(project_path) else {
        return Ok(None);
    };
    unique_project_basename_context(db, basename).await
}

async fn unique_project_basename_context(
    db: &RegisteredGlobalDb,
    basename: &str,
) -> Result<Option<ProjectRegistryContext>> {
    let mut matching_ids = Vec::new();
    for project in db.try_search_code_projects(basename, usize::MAX).await? {
        if !project_basename_matches(&project, basename)
            || matching_ids.contains(&project.project_id)
        {
            continue;
        }
        matching_ids.push(project.project_id);
        if matching_ids.len() > 1 {
            return Ok(None);
        }
    }
    let Some(project_id) = matching_ids.into_iter().next() else {
        return Ok(None);
    };
    db.project_registry_context_by_id(&project_id).await
}

fn is_explicit_project_path_selector(selector: &str) -> bool {
    let selector = selector.trim();
    !selector.is_empty()
        && (Path::new(selector).is_absolute()
            || selector == "."
            || selector == ".."
            || selector.contains('/')
            || selector.contains('\\'))
}

fn bare_project_name(value: &str) -> Option<&str> {
    let mut components = Path::new(value).components();
    let first = components.next()?;
    if components.next().is_some() {
        return None;
    }
    match first {
        Component::Normal(name) => name.to_str().filter(|name| !name.is_empty()),
        _ => None,
    }
}

fn project_basename_matches(project: &CodeProjectRecord, basename: &str) -> bool {
    [
        project.display_root.as_str(),
        project.canonical_root.as_str(),
    ]
    .into_iter()
    .filter_map(|root| Path::new(root).file_name())
    .any(|name| name == basename)
}

fn unresolved_project_selector_error(
    project_id: Option<&str>,
    project_path: Option<&str>,
) -> TraceDecayError {
    let selector = project_id
        .map(|value| format!("project_id={value}"))
        .or_else(|| project_path.map(|value| format!("project_path={value}")))
        .unwrap_or_else(|| "empty selector".to_string());
    TraceDecayError::Config {
        message: format!(
            "registered project not found for selector ({selector}); run tracedecay_project_search to find the registered project_id or full project_path"
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{is_explicit_project_path_selector, require_node_id, string_array_values};

    #[test]
    fn test_require_node_id_canonical() {
        let args = json!({"node_id": "fn:abc123"});
        assert!(matches!(require_node_id(&args), Ok("fn:abc123")));
    }

    #[test]
    fn test_require_node_id_alias() {
        let args = json!({"id": "trait:def456"});
        assert!(matches!(require_node_id(&args), Ok("trait:def456")));
    }

    #[test]
    fn test_require_node_id_prefers_canonical() {
        let args = json!({"node_id": "fn:canonical", "id": "fn:alias"});
        assert!(matches!(require_node_id(&args), Ok("fn:canonical")));
    }

    #[test]
    fn test_require_node_id_missing() {
        let args = json!({"query": "something"});
        assert!(require_node_id(&args).is_err());
    }

    #[test]
    fn test_string_array_values_keeps_only_string_items() {
        let args = json!({
            "values": ["alpha", 7, null, "beta"],
            "not_array": "alpha"
        });

        assert_eq!(
            string_array_values(&args, "values"),
            vec!["alpha".to_string(), "beta".to_string()]
        );
        assert!(string_array_values(&args, "missing").is_empty());
        assert!(string_array_values(&args, "not_array").is_empty());
    }

    #[test]
    fn explicit_project_path_detection_is_syntax_only() {
        assert!(is_explicit_project_path_selector("/workspace/project"));
        assert!(is_explicit_project_path_selector("team/project"));
        assert!(is_explicit_project_path_selector("."));
        assert!(!is_explicit_project_path_selector("project"));
    }
}
