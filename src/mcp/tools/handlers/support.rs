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

/// Key under which context handlers stash analytics that must reach the server
/// but never the client. [`rendered_tool_result`] is the one place it is lifted
/// back out, so no handler has to remember to strip it.
pub(super) const CONTEXT_MEMORY_ANALYTICS_KEY: &str = "context_memory_analytics";

/// The single wrapper every MCP tool handler returns through.
///
/// Lifts internal analytics out of `value` so they travel beside the result
/// instead of inside the client payload, renders the default-format (markdown)
/// body with `md`, and records `touched_files`. The `format:"json"` path is
/// unaffected — [`render::finalize`] serializes `value` compactly there.
pub(super) fn rendered_tool_result<F: FnOnce() -> String>(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
    touched_files: Vec<String>,
    md: F,
) -> ToolResult {
    let internal_analytics = value.get(CONTEXT_MEMORY_ANALYTICS_KEY).cloned();
    let public_value = internal_analytics
        .as_ref()
        .and_then(|_| public_value_without_internal_context_memory_analytics(value));
    let value = public_value.as_ref().unwrap_or(value);
    let text = render::finalize(project_root, args, value, md);
    let result = text_tool_result(&text, touched_files);
    if let Some(internal_analytics) = internal_analytics {
        result.with_internal_analytics(internal_analytics)
    } else {
        result
    }
}

fn public_value_without_internal_context_memory_analytics(value: &Value) -> Option<Value> {
    let mut value = value.clone();
    take_internal_context_memory_analytics(&mut value).map(|_| value)
}

pub(super) fn take_internal_context_memory_analytics(value: &mut Value) -> Option<Value> {
    value.as_object_mut()?.remove(CONTEXT_MEMORY_ANALYTICS_KEY)
}

pub(super) fn text_tool_result(text: &str, touched_files: Vec<String>) -> ToolResult {
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    )
}

/// [`rendered_tool_result`] for handlers that touch no files.
pub(super) fn tool_json_with_md<F: FnOnce() -> String>(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
    md: F,
) -> ToolResult {
    rendered_tool_result(project_root, args, value, Vec::new(), md)
}

/// [`rendered_tool_result`] for handlers that don't need a custom markdown
/// renderer — the default body is [`render::generic_md`] over the same value.
pub(super) fn generic_tool_result(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
    touched_files: Vec<String>,
) -> ToolResult {
    rendered_tool_result(project_root, args, value, touched_files, || {
        render::generic_md(value)
    })
}

/// [`generic_tool_result`] for handlers that touch no files.
pub(super) fn tool_json(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    generic_tool_result(project_root, args, value, Vec::new())
}

/// Rejects tool arguments that are not a JSON object.
///
/// The argument value comes straight off the wire (an MCP client, the
/// `tracedecay tool --args` CLI, or an internal dispatch probe), so a scalar
/// or array is caller error, not a broken invariant — asserting it would
/// panic the daemon's client task and the caller would see only a dropped
/// connection.
pub(crate) fn require_object_args(args: &Value, tool_name: &str) -> Result<()> {
    if args.is_object() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!("invalid arguments: {tool_name} expects a JSON object"),
    })
}

/// Rejects a zero result limit with a typed error.
///
/// Handlers clamp a caller-supplied limit with `min(max)`, which leaves an
/// explicit `"limit": 0` intact, so zero is caller input rather than an
/// invariant the handler can assume away.
pub(crate) fn require_positive_limit(limit: usize, tool_name: &str) -> Result<()> {
    if limit == 0 {
        return Err(TraceDecayError::Config {
            message: format!("invalid parameter: {tool_name} requires limit to be at least 1"),
        });
    }
    Ok(())
}

/// Extracts the `node_id` parameter from tool arguments, accepting `id` as a
/// fallback alias. LLMs occasionally shorten `node_id` to `id`; this avoids a
/// confusing error when that happens.
///
/// A present-but-blank value is rejected here rather than forwarded to the
/// graph traversal layer; every handler that takes a node id shares this one
/// guard, so the failure is a typed argument error naming the parameter.
pub(super) fn require_node_id(args: &Value) -> Result<&str> {
    let node_id = args
        .get("node_id")
        .or_else(|| args.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: node_id".to_string(),
        })?;
    if node_id.trim().is_empty() {
        return Err(TraceDecayError::Config {
            message: "invalid parameter: node_id must not be empty".to_string(),
        });
    }
    Ok(node_id)
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
    match resolve_project_registry_context(db, project_id, project_path).await? {
        ProjectSelectorResolution::Resolved(context) => Ok(Some(context)),
        ProjectSelectorResolution::Unresolved => {
            Err(unresolved_project_selector_error(project_id, project_path))
        }
        ProjectSelectorResolution::Ambiguous { candidates } => Err(
            ambiguous_project_selector_error(project_id, project_path, &candidates),
        ),
    }
}

/// The three outcomes a project selector can have. "No single registered
/// project" is not one state: a selector that matches several registered
/// projects is ambiguous and must say so, because reporting it as unresolved
/// sends the caller looking for a registration that already exists.
enum ProjectSelectorResolution {
    Resolved(ProjectRegistryContext),
    Unresolved,
    Ambiguous { candidates: Vec<String> },
}

impl From<Option<ProjectRegistryContext>> for ProjectSelectorResolution {
    fn from(context: Option<ProjectRegistryContext>) -> Self {
        context.map_or(Self::Unresolved, Self::Resolved)
    }
}

async fn resolve_project_registry_context(
    db: &RegisteredGlobalDb,
    project_id: Option<&str>,
    project_path: Option<&str>,
) -> Result<ProjectSelectorResolution> {
    if let Some(project_id) = project_id {
        return Ok(db.project_registry_context_by_id(project_id).await?.into());
    }
    let Some(project_path) = project_path else {
        return Ok(ProjectSelectorResolution::Unresolved);
    };
    let selector_path = Path::new(project_path);
    if let Some(store) = db
        .try_resolve_project_store_record_by_alias(selector_path)
        .await?
    {
        return Ok(db
            .project_registry_context_by_id(&store.project_id)
            .await?
            .into());
    }
    if is_explicit_project_path_selector(project_path) {
        // A registered project needs no registered *store instance* row to be
        // selectable: its registry identity is what names the project. Resolve
        // the project itself so a project registered before its store instance
        // was recorded still selects, instead of reporting an unregistered
        // project.
        if let Some(context) = db.project_registry_context_by_alias(selector_path).await? {
            return sole_claimant_of_its_root(db, context).await;
        }
        let git_common_dir = crate::worktree::git_common_dir(selector_path);
        if let Some(context) = db
            .project_registry_context_by_identity(selector_path, git_common_dir.as_deref())
            .await?
        {
            return sole_claimant_of_its_root(db, context).await;
        }
        let canonical_path = selector_path
            .canonicalize()
            .unwrap_or_else(|_| selector_path.to_path_buf());
        for parent in canonical_path.ancestors().skip(1) {
            if let Some(context) = db.project_registry_context_by_alias(parent).await? {
                return sole_claimant_of_its_root(db, context).await;
            }
        }
    }
    let Some(basename) = bare_project_name(project_path) else {
        return Ok(ProjectSelectorResolution::Unresolved);
    };
    unique_project_basename_context(db, basename).await
}

/// Keeps a path-resolved context only while one registered project claims its
/// canonical root.
///
/// A path maps to exactly one row in the alias table, so registering a second
/// project at the same root silently rebinds that path. Serving whichever
/// project the alias last named would answer for an arbitrary one, so a root
/// several projects claim is reported as ambiguous instead.
async fn sole_claimant_of_its_root(
    db: &RegisteredGlobalDb,
    context: ProjectRegistryContext,
) -> Result<ProjectSelectorResolution> {
    let canonical_root = context.project.canonical_root.clone();
    let mut claimants = Vec::new();
    for project in db
        .try_search_code_projects(&canonical_root, usize::MAX)
        .await?
    {
        if project.canonical_root == canonical_root && !claimants.contains(&project.project_id) {
            claimants.push(project.project_id);
        }
    }
    if claimants.len() > 1 {
        claimants.sort();
        return Ok(ProjectSelectorResolution::Ambiguous {
            candidates: claimants,
        });
    }
    Ok(ProjectSelectorResolution::Resolved(context))
}

async fn unique_project_basename_context(
    db: &RegisteredGlobalDb,
    basename: &str,
) -> Result<ProjectSelectorResolution> {
    let mut matching_ids = Vec::new();
    for project in db.try_search_code_projects(basename, usize::MAX).await? {
        if !project_basename_matches(&project, basename)
            || matching_ids.contains(&project.project_id)
        {
            continue;
        }
        matching_ids.push(project.project_id);
    }
    if matching_ids.len() > 1 {
        matching_ids.sort();
        return Ok(ProjectSelectorResolution::Ambiguous {
            candidates: matching_ids,
        });
    }
    let Some(project_id) = matching_ids.into_iter().next() else {
        return Ok(ProjectSelectorResolution::Unresolved);
    };
    Ok(db.project_registry_context_by_id(&project_id).await?.into())
}

/// Whether a selector names a path rather than a bare project name. This is
/// pure syntax: it decides whether a selector may fall back to Git identity,
/// and never consults the registry.
pub(super) fn is_explicit_project_path_selector(selector: &str) -> bool {
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

fn selector_label(project_id: Option<&str>, project_path: Option<&str>) -> String {
    project_id
        .map(|value| format!("project_id={value}"))
        .or_else(|| project_path.map(|value| format!("project_path={value}")))
        .unwrap_or_else(|| "empty selector".to_string())
}

fn unresolved_project_selector_error(
    project_id: Option<&str>,
    project_path: Option<&str>,
) -> TraceDecayError {
    let selector = selector_label(project_id, project_path);
    TraceDecayError::Config {
        message: format!(
            "registered project not found for selector ({selector}); run tracedecay_project_search to find the registered project_id or full project_path"
        ),
    }
}

/// An ambiguous selector still resolved no single project, so it keeps the
/// unresolved wording callers and hosts match on, and adds which registrations
/// collided so the caller can disambiguate instead of re-searching.
fn ambiguous_project_selector_error(
    project_id: Option<&str>,
    project_path: Option<&str>,
    candidates: &[String],
) -> TraceDecayError {
    let selector = selector_label(project_id, project_path);
    TraceDecayError::Config {
        message: format!(
            "registered project not found for selector ({selector}): the selector is ambiguous across {} registered projects ({}); pass project_id or the full project_path",
            candidates.len(),
            candidates.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::mcp::tools::render;

    use super::{
        CONTEXT_MEMORY_ANALYTICS_KEY, generic_tool_result, is_explicit_project_path_selector,
        rendered_tool_result, require_node_id, string_array_values,
    };

    /// `generic_tool_result` must stay a pure spelling of the closure form it
    /// replaced at every call site — same bytes on both output formats, and the
    /// same internal-analytics lifting.
    #[test]
    fn generic_tool_result_matches_the_explicit_generic_md_closure() {
        let mut value = json!({
            "count": 2,
            "items": [{"name": "alpha", "file": "src/a.rs"}, {"name": "beta", "file": "src/b.rs"}],
        });
        // Exercise the internal-analytics lifting branch too.
        value[CONTEXT_MEMORY_ANALYTICS_KEY] = json!({"matches": 1});
        let touched = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];

        for args in [
            json!({}),
            json!({"format": "markdown"}),
            json!({"format": "json"}),
        ] {
            let expected = rendered_tool_result(None, &args, &value, touched.clone(), || {
                render::generic_md(&value)
            });
            let actual = generic_tool_result(None, &args, &value, touched.clone());

            assert_eq!(actual.value, expected.value, "payload differs for {args}");
            assert_eq!(
                actual.touched_files, expected.touched_files,
                "touched files differ for {args}"
            );
            assert_eq!(
                actual.internal_analytics(),
                expected.internal_analytics(),
                "internal analytics differ for {args}"
            );
        }
    }

    /// Handlers that used to build their own text envelope — `render::finalize`
    /// then a hand-written `{"content":[{"type":"text",...}]}` — now go through
    /// `rendered_tool_result`. That is the same envelope for any payload without
    /// the internal-analytics key, which is every payload those handlers build.
    #[test]
    fn rendered_tool_result_matches_a_hand_built_text_envelope() {
        let value = json!({"passed": 0, "failed": 1, "results": [], "note": "nothing ran"});
        let touched = vec!["src/a.rs".to_string()];

        for args in [
            json!({}),
            json!({"format": "markdown"}),
            json!({"format": "json"}),
        ] {
            let text = render::finalize(None, &args, &value, || render::generic_md(&value));
            let expected = super::text_tool_result(&text, touched.clone());
            let actual = generic_tool_result(None, &args, &value, touched.clone());

            assert_eq!(actual.value, expected.value, "payload differs for {args}");
            assert_eq!(
                actual.touched_files, expected.touched_files,
                "touched files differ for {args}"
            );
            assert!(actual.internal_analytics().is_none(), "for {args}");
        }
    }

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

    /// Every node-id handler shares this one guard, so a blank value must be
    /// rejected here — naming the offending parameter — rather than reaching
    /// graph traversal.
    #[test]
    fn require_node_id_rejects_blank_values() {
        for args in [
            json!({"node_id": ""}),
            json!({"node_id": "   "}),
            json!({"node_id": "\t\n"}),
            json!({"id": ""}),
        ] {
            let error = require_node_id(&args).expect_err(&format!("blank node id: {args}"));
            let message = error.to_string();
            assert!(
                message.contains("node_id must not be empty"),
                "unexpected message for {args}: {message}"
            );
        }
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
