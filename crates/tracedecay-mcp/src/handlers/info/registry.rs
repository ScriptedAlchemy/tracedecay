//! `tracedecay_project_list`, `tracedecay_project_search`, and
//! `tracedecay_project_context` over the profile project registry.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_contracts::{
    ProjectRegistryContextCommand, ProjectRegistryContextOutcome, ProjectRegistryListingCommand,
    ProjectRegistryListingOutcome, ProjectRegistryListingScope, ProjectRegistryReadPort,
    ProjectRegistrySelector, ProjectRegistryView, list_registered_projects,
    read_registered_project_context, render_project_registry_view,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::ToolResult;
use crate::rendered_tool_result;
use crate::tools::render;

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn bounded_limit(args: &Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .map_or(default, |value| value.clamp(1, max))
}

fn registry_result(args: &Value, payload: &Value) -> ToolResult {
    render_registry_result(None, args, payload)
}

/// The registry tree view renders only an `ok` listing. An `unavailable`
/// payload carries the same zeroed tree keys for shape stability and must
/// not be read back as "no registered projects".
fn render_registry_result(root: Option<&Path>, args: &Value, payload: &Value) -> ToolResult {
    rendered_tool_result(root, args, payload, vec![], || {
        if payload.get("status").and_then(Value::as_str) == Some("ok")
            && payload.get("project_tree").is_some()
        {
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
        "status": "unavailable",
        "message": "project registry is not present for this profile",
        "projects": [],
    })
}

/// Zeroed summary/tree keys for the missing-registry branch, mirroring the
/// ok-shape's `summary`/`project_tree` so callers get a stable payload shape
/// regardless of whether the registry is present.
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

/// Whether a selector names a path rather than a bare project name. Pure
/// syntax: it decides whether a selector may fall back to Git identity.
/// Must stay aligned with
/// `RegisteredGlobalDb::is_explicit_project_path_selector`.
fn is_explicit_project_path_selector(selector: &str) -> bool {
    let selector = selector.trim();
    !selector.is_empty()
        && (Path::new(selector).is_absolute()
            || selector == "."
            || selector == ".."
            || selector.contains('/')
            || selector.contains('\\'))
}

/// `project_root` is the served project when the call arrives over a
/// project connection; a projectless connection reads the same profile
/// registry with `None` and marks no project active.
#[hotpath::measure(label = "mcp.info.project_list.total")]
pub async fn handle_project_list(
    project_root: Option<&Path>,
    args: Value,
    registry: Option<&dyn ProjectRegistryReadPort>,
) -> Result<ToolResult> {
    let limit = bounded_limit(&args, 25, 100);
    let outcome = hotpath::future!(
        list_registered_projects(
            registry,
            ProjectRegistryListingCommand {
                active_project_root: project_root.map(Path::to_path_buf),
                scope: ProjectRegistryListingScope::All,
                limit,
            },
        ),
        label = "mcp.info.project_list.list"
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

#[hotpath::measure(label = "mcp.info.project_search.total")]
pub async fn handle_project_search(
    project_root: Option<&Path>,
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
    let outcome = hotpath::future!(
        list_registered_projects(
            registry,
            ProjectRegistryListingCommand {
                active_project_root: project_root.map(Path::to_path_buf),
                scope: ProjectRegistryListingScope::Matching {
                    query: query.clone(),
                },
                limit,
            },
        ),
        label = "mcp.info.project_search.search"
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
/// `unavailable` payload with the same summary/tree keys as the `ok` shape.
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

/// The selector defaults to the served project root; a projectless call has
/// no such default and must name the project explicitly.
fn project_context_selector(
    project_root: Option<&Path>,
    args: &Value,
) -> Result<ProjectRegistrySelector> {
    if let Some(project_id) = args
        .get("project_selector")
        .and_then(Value::as_object)
        .and_then(|selector| selector.get("project_id"))
        .and_then(Value::as_str)
    {
        return Ok(ProjectRegistrySelector::ProjectId(project_id.to_owned()));
    }
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        let Some(project_root) = project_root else {
            return Err(TraceDecayError::Config {
                message: "missing required parameter: path or project_selector (no active \
                          project is connected)"
                    .to_string(),
            });
        };
        return Ok(ProjectRegistrySelector::Path {
            path: project_root.to_path_buf(),
            allow_git_identity: true,
        });
    };
    let path = Path::new(path);
    let allow_git_identity =
        path.is_absolute() && is_explicit_project_path_selector(path.to_string_lossy().as_ref());
    Ok(ProjectRegistrySelector::Path {
        path: path.to_path_buf(),
        allow_git_identity,
    })
}

#[hotpath::measure(label = "mcp.info.project_context.total")]
pub async fn handle_project_context(
    project_root: Option<&Path>,
    args: Value,
    registry: Option<&dyn ProjectRegistryReadPort>,
) -> Result<ToolResult> {
    let selector = project_context_selector(project_root, &args)?;
    let outcome = hotpath::future!(
        read_registered_project_context(
            registry,
            ProjectRegistryContextCommand {
                active_project_root: project_root.map(Path::to_path_buf),
                selector,
            },
        ),
        label = "mcp.info.project_context.read"
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
    Ok(render_registry_result(project_root, &args, &payload))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;
    use tracedecay_contracts::{ProjectRegistryListingOutcome, ProjectRegistrySelector};

    use super::{project_context_selector, registry_listing_result, registry_missing_payload};

    #[test]
    fn missing_registry_payload_preserves_unavailable_state() {
        let payload = registry_missing_payload();

        assert_eq!(payload["status"], "unavailable");
        assert_eq!(
            payload["message"],
            "project registry is not present for this profile"
        );
        assert_eq!(payload["projects"].as_array().map(Vec::len), Some(0));
        assert!(payload.get("registry_path").is_none());
    }

    /// The rendered text of an unavailable registry must say so; the zeroed
    /// tree keys exist for payload-shape stability, not to read as an empty
    /// listing.
    #[test]
    fn unavailable_listing_does_not_render_as_an_empty_registry() {
        let result = registry_listing_result(
            &json!({}),
            "registered projects",
            None,
            25,
            ProjectRegistryListingOutcome::RegistryUnavailable,
        );
        let text = result.value["content"][0]["text"]
            .as_str()
            .expect("rendered text");
        assert!(
            text.contains("unavailable") && text.contains("not present"),
            "unavailable state must survive rendering, got:\n{text}"
        );
        assert!(
            !text.contains("No registered projects found"),
            "an absent registry must not read as an empty one, got:\n{text}"
        );
    }

    #[test]
    fn project_context_selector_defaults_to_the_served_root_only_when_one_exists() {
        let served = Path::new("/srv/checkout");
        assert_eq!(
            project_context_selector(Some(served), &json!({})).expect("served root selector"),
            ProjectRegistrySelector::Path {
                path: served.to_path_buf(),
                allow_git_identity: true,
            }
        );

        let error = project_context_selector(None, &json!({}))
            .expect_err("a projectless read with no selector must be refused");
        assert!(
            error
                .to_string()
                .contains("missing required parameter: path or project_selector"),
            "unexpected refusal: {error}"
        );

        // Git identity is reserved for a host-absolute path; `/srv/other` is
        // drive-relative on Windows.
        let other = if cfg!(windows) {
            r"C:\srv\other"
        } else {
            "/srv/other"
        };
        assert_eq!(
            project_context_selector(None, &json!({"path": other}))
                .expect("explicit path selector"),
            ProjectRegistrySelector::Path {
                path: Path::new(other).to_path_buf(),
                allow_git_identity: true,
            }
        );
        assert_eq!(
            project_context_selector(
                None,
                &json!({"project_selector": {"project_id": "project.other"}})
            )
            .expect("project id selector"),
            ProjectRegistrySelector::ProjectId("project.other".to_owned())
        );
    }
}
