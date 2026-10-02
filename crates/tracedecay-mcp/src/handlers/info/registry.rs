//! `tracedecay_project_list`, `tracedecay_project_search`, and
//! `tracedecay_project_context` over the profile project registry.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    ProjectContextResultV1, ProjectContextSurfaceRequestV1, ProjectListSurfaceRequestV1,
    ProjectRegistryListingResultV1, ProjectSearchSurfaceRequestV1,
};
use tracedecay_contracts::{
    ProjectRegistryContextCommand, ProjectRegistryContextOutcome, ProjectRegistryListingCommand,
    ProjectRegistryListingScope, ProjectRegistryListingView, ProjectRegistryReadPort,
    ProjectRegistrySelector, ProjectRegistryView, render_project_registry_view,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use crate::handlers::graph::graph_tool_completion;
use crate::handlers::support::{decode_primitive_request, decode_selector_request};

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn bounded_limit(limit: Option<usize>, default: usize, max: usize) -> usize {
    limit.map_or(default, |value| value.clamp(1, max))
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

/// Computes one profile registry read. `active_project_root` is the project
/// the calling connection serves, when it has one; it only marks that
/// project active and is the context read's default selector.
#[tracing::instrument(name = "mcp.info.project_registry.total", level = "trace", skip_all)]
pub async fn compute_registry_read(
    registry: &dyn ProjectRegistryReadPort,
    active_project_root: Option<&Path>,
    operation: ApplicationSurfaceOperation,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let tool_name = operation.mcp_tool_name();
    let active_project_root = active_project_root.map(Path::to_path_buf);
    let result = match operation {
        ApplicationSurfaceOperation::ProjectList => {
            let request: ProjectListSurfaceRequestV1 = decode_primitive_request(&args, tool_name)?;
            let limit = bounded_limit(request.limit, 25, 100);
            let listing = registry
                .list(ProjectRegistryListingCommand {
                    active_project_root,
                    scope: ProjectRegistryListingScope::All,
                    limit,
                })
                .await?;
            GraphToolResultV1::ProjectList(listing_result(
                "registered projects".to_owned(),
                None,
                limit,
                listing,
            ))
        }
        ApplicationSurfaceOperation::ProjectSearch => {
            let request: ProjectSearchSurfaceRequestV1 =
                decode_primitive_request(&args, tool_name)?;
            let limit = bounded_limit(request.limit, 10, 50);
            let listing = registry
                .list(ProjectRegistryListingCommand {
                    active_project_root,
                    scope: ProjectRegistryListingScope::Matching {
                        query: request.query.clone(),
                    },
                    limit,
                })
                .await?;
            GraphToolResultV1::ProjectSearch(listing_result(
                format!("projects matching \"{}\"", request.query),
                Some(request.query),
                limit,
                listing,
            ))
        }
        ApplicationSurfaceOperation::ProjectContext => {
            let request: ProjectContextSurfaceRequestV1 =
                decode_selector_request(&args, tool_name)?;
            let selector = project_context_selector(active_project_root.as_deref(), request)?;
            let outcome = registry
                .context(ProjectRegistryContextCommand {
                    active_project_root,
                    selector,
                })
                .await?;
            GraphToolResultV1::ProjectContext(match outcome {
                ProjectRegistryContextOutcome::NotFound { registry_path } => {
                    ProjectContextResultV1::NotFound {
                        registry_path: display_path(&registry_path),
                        project: (),
                        aliases: Vec::new(),
                        stores: Vec::new(),
                    }
                }
                ProjectRegistryContextOutcome::Context(context) => ProjectContextResultV1::Ok {
                    is_active: context.is_active,
                    registry_path: display_path(&context.registry_path),
                    project: Box::new(context.project),
                    aliases: context.aliases,
                    stores: context.stores,
                },
            })
        }
        operation => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "unknown tool: {} is not a profile registry read",
                    operation.mcp_tool_name()
                ),
            });
        }
    };
    Ok(graph_tool_completion(result, Vec::new()))
}

fn listing_result(
    title: String,
    query: Option<String>,
    limit: usize,
    listing: ProjectRegistryListingView,
) -> ProjectRegistryListingResultV1 {
    ProjectRegistryListingResultV1::Ok {
        title,
        registry_path: display_path(&listing.registry_path),
        query,
        limit,
        truncated: listing.truncated,
        summary: listing.view.summary,
        project_tree: listing.view.project_tree,
        projects: listing.projects,
    }
}

/// The listing's tree view as the registry tools render it.
pub(crate) fn render_registry_listing_md(listing: &ProjectRegistryListingResultV1) -> String {
    let ProjectRegistryListingResultV1::Ok {
        title,
        summary,
        project_tree,
        ..
    } = listing;
    render_project_registry_view(
        title,
        &ProjectRegistryView {
            summary: summary.clone(),
            project_tree: project_tree.clone(),
        },
    )
}

/// The selector defaults to the served project root; a projectless call has
/// no such default and must name the project explicitly.
fn project_context_selector(
    project_root: Option<&Path>,
    request: ProjectContextSurfaceRequestV1,
) -> Result<ProjectRegistrySelector> {
    if let Some(selector) = request.project_selector {
        return Ok(ProjectRegistrySelector::ProjectId(selector.project_id));
    }
    let Some(path) = request.path else {
        let Some(project_root) = project_root else {
            return Err(TraceDecayError::missing_required_parameter(
                "missing required parameter: path or project_selector (no active project is \
                 connected)",
            ));
        };
        return Ok(ProjectRegistrySelector::Path {
            path: project_root.to_path_buf(),
            allow_git_identity: true,
        });
    };
    let path = Path::new(&path);
    let allow_git_identity =
        path.is_absolute() && is_explicit_project_path_selector(path.to_string_lossy().as_ref());
    Ok(ProjectRegistrySelector::Path {
        path: path.to_path_buf(),
        allow_git_identity,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tracedecay_contracts::ProjectRegistrySelector;
    use tracedecay_contracts::retrieval::{
        ProjectContextSurfaceRequestV1, RegisteredProjectIdSelectorV1,
    };

    use super::project_context_selector;

    #[test]
    fn project_context_selector_defaults_to_the_served_root_only_when_one_exists() {
        let served = Path::new("/srv/checkout");
        assert_eq!(
            project_context_selector(Some(served), ProjectContextSurfaceRequestV1::default())
                .expect("served root selector"),
            ProjectRegistrySelector::Path {
                path: served.to_path_buf(),
                allow_git_identity: true,
            }
        );

        let error = project_context_selector(None, ProjectContextSurfaceRequestV1::default())
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
            project_context_selector(
                None,
                ProjectContextSurfaceRequestV1 {
                    project_selector: None,
                    path: Some(other.to_owned()),
                }
            )
            .expect("explicit path selector"),
            ProjectRegistrySelector::Path {
                path: Path::new(other).to_path_buf(),
                allow_git_identity: true,
            }
        );
        assert_eq!(
            project_context_selector(
                None,
                ProjectContextSurfaceRequestV1 {
                    project_selector: Some(RegisteredProjectIdSelectorV1 {
                        project_id: "project.other".to_owned(),
                    }),
                    path: None,
                }
            )
            .expect("project id selector"),
            ProjectRegistrySelector::ProjectId("project.other".to_owned())
        );
    }
}
