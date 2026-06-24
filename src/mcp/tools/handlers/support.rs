//! Shared helpers for MCP tool handlers.
//!
//! Keep this module free of tool dispatch logic. Handler modules use it for
//! argument normalization, scope filtering, and registered-project selection.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::errors::{Result, TraceDecayError};
use crate::global_db::{GlobalDb, ProjectRegistryContext};

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
    match scope_prefix {
        Some(prefix) => {
            let with_slash = if prefix.ends_with('/') {
                prefix.to_string()
            } else {
                format!("{prefix}/")
            };
            items
                .into_iter()
                .filter(|item| {
                    let p = get_path(item);
                    p.starts_with(&with_slash) || p == prefix
                })
                .collect()
        }
        None => items,
    }
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

fn global_db_profile_root() -> Result<PathBuf> {
    crate::storage::default_profile_root()
}

pub(super) fn profile_root_for_global_db(
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
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
    if !allow_default_registry_fallback {
        return Err(TraceDecayError::Config {
            message: "client project registry is unavailable for selector resolution".to_string(),
        });
    }
    global_db_profile_root()
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
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
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

    let owned_db;
    let db = match global_db {
        Some(db) => db,
        None if allow_default_registry_fallback => {
            owned_db = GlobalDb::open()
                .await
                .ok_or_else(|| TraceDecayError::Config {
                    message:
                        "could not open tracedecay project registry; run tracedecay init first"
                            .to_string(),
                })?;
            &owned_db
        }
        None => {
            return Err(TraceDecayError::Config {
                message: "client project registry is unavailable for selector resolution"
                    .to_string(),
            });
        }
    };
    let context = if let Some(project_id) = project_id {
        db.project_registry_context_by_id(project_id).await
    } else if let Some(project_path) = project_path {
        db.project_registry_context_by_alias(Path::new(project_path))
            .await
    } else {
        return Ok(None);
    };

    context
        .ok_or_else(|| TraceDecayError::Config {
            message: "registered project not found for selector".to_string(),
        })
        .map(Some)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{require_node_id, string_array_values};

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
}
