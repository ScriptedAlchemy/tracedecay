//! `tracedecay_project_list`, `tracedecay_project_search`, and `tracedecay_project_context` over the profile project registry.

use super::*;

fn bounded_limit(args: &Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .map_or(default, |value| value.clamp(1, max))
}

fn project_registry_result(cg: &TraceDecay, args: &Value, payload: &Value) -> ToolResult {
    render_registry_result(Some(cg.project_root()), args, payload)
}

fn registry_result(args: &Value, payload: &Value) -> ToolResult {
    render_registry_result(None, args, payload)
}

fn render_registry_result(root: Option<&Path>, args: &Value, payload: &Value) -> ToolResult {
    rendered_tool_result(root, args, payload, vec![], || {
        if payload.get("project_tree").is_some() {
            let view = serde_json::from_value::<ProjectRegistryView>(json!({
                "summary": payload.get("summary").cloned().unwrap_or_else(|| json!({})),
                "project_tree": payload.get("project_tree").cloned().unwrap_or_else(|| json!([])),
            }));
            if let Ok(view) = view {
                let title = payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("registered projects");
                return render_project_registry_view(title, &view);
            }
        }
        render::generic_md(payload)
    })
}

fn registry_missing_payload() -> Value {
    json!({
        "status": "not_found",
        "message": "project registry is not present for this profile",
        "projects": [],
    })
}

/// Zeroed summary/tree keys for the missing-registry branch, mirroring the
/// ok-shape's `summary`/`project_tree` so callers get a stable payload shape
/// regardless of whether the registry is present (see
/// `src/dashboard/projects.rs`'s missing-registry branch).
fn empty_registry_view_payload(title: &str) -> (Value, Value, Value) {
    (
        json!(title),
        json!({
            "project_count": 0,
            "repo_count": 0,
            "truncated": false,
        }),
        json!([]),
    )
}

/// Handles `tracedecay_project_list` tool calls.
pub(crate) async fn handle_project_list(
    cg: &TraceDecay,
    args: Value,
    registry: Option<&dyn ProjectRegistryReadPort>,
) -> Result<ToolResult> {
    let limit = bounded_limit(&args, 25, 100);
    let outcome = list_registered_projects(
        registry,
        ProjectRegistryListingCommand {
            active_project_root: cg.project_root().to_path_buf(),
            scope: ProjectRegistryListingScope::All,
            limit,
        },
    )
    .await?;
    Ok(registry_listing_result(
        &args,
        "registered projects",
        None,
        limit,
        outcome,
    ))
}

/// Handles `tracedecay_project_search` tool calls.
pub(crate) async fn handle_project_search(
    cg: &TraceDecay,
    args: Value,
    registry: Option<&dyn ProjectRegistryReadPort>,
) -> Result<ToolResult> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: query".to_string(),
        })?
        .to_owned();
    let limit = bounded_limit(&args, 10, 50);
    let outcome = list_registered_projects(
        registry,
        ProjectRegistryListingCommand {
            active_project_root: cg.project_root().to_path_buf(),
            scope: ProjectRegistryListingScope::Matching {
                query: query.clone(),
            },
            limit,
        },
    )
    .await?;
    Ok(registry_listing_result(
        &args,
        &format!("projects matching \"{query}\""),
        Some(&query),
        limit,
        outcome,
    ))
}

/// Renders a listing outcome, keeping the missing-registry state a stable
/// `not_found` payload with the same summary/tree keys as the `ok` shape.
fn registry_listing_result(
    args: &Value,
    title: &str,
    query: Option<&str>,
    limit: usize,
    outcome: ProjectRegistryListingOutcome,
) -> ToolResult {
    match outcome {
        ProjectRegistryListingOutcome::RegistryUnavailable => {
            let mut payload = registry_missing_payload();
            let (title, summary, project_tree) = empty_registry_view_payload(title);
            payload["title"] = title;
            payload["summary"] = summary;
            payload["project_tree"] = project_tree;
            if let Some(query) = query {
                payload["query"] = json!(query);
            }
            payload["limit"] = json!(limit);
            payload["truncated"] = json!(false);
            registry_result(args, &payload)
        }
        ProjectRegistryListingOutcome::Listing(listing) => {
            let mut payload = json!({
                "status": "ok",
                "title": title,
                "registry_path": display_path(&listing.registry_path),
                "limit": limit,
                "truncated": listing.truncated,
                "summary": listing.view.summary,
                "project_tree": listing.view.project_tree,
                "projects": listing.projects,
            });
            if let Some(query) = query {
                payload["query"] = json!(query);
            }
            registry_result(args, &payload)
        }
    }
}

fn project_context_selector(cg: &TraceDecay, args: &Value) -> ProjectRegistrySelector {
    if let Some(project_id) = args.get("project_id").and_then(Value::as_str) {
        return ProjectRegistrySelector::ProjectId(project_id.to_owned());
    }
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return ProjectRegistrySelector::Path {
            path: cg.project_root().to_path_buf(),
            allow_git_identity: true,
        };
    };
    let path = Path::new(path);
    let allow_git_identity =
        path.is_absolute() && is_explicit_project_path_selector(path.to_string_lossy().as_ref());
    ProjectRegistrySelector::Path {
        path: path.to_path_buf(),
        allow_git_identity,
    }
}

/// Handles `tracedecay_project_context` tool calls.
pub(crate) async fn handle_project_context(
    cg: &TraceDecay,
    args: Value,
    registry: Option<&dyn ProjectRegistryReadPort>,
) -> Result<ToolResult> {
    let outcome = read_registered_project_context(
        registry,
        ProjectRegistryContextCommand {
            active_project_root: cg.project_root().to_path_buf(),
            selector: project_context_selector(cg, &args),
        },
    )
    .await?;
    let payload = match outcome {
        ProjectRegistryContextOutcome::RegistryUnavailable => registry_missing_payload(),
        ProjectRegistryContextOutcome::NotFound { registry_path } => json!({
            "status": "not_found",
            "registry_path": display_path(&registry_path),
            "project": null,
            "aliases": [],
            "stores": [],
        }),
        ProjectRegistryContextOutcome::Context(context) => json!({
            "status": "ok",
            "is_active": context.is_active,
            "registry_path": display_path(&context.registry_path),
            "project": context.project,
            "aliases": context.aliases,
            "stores": context.stores,
        }),
    };
    Ok(project_registry_result(cg, &args, &payload))
}
