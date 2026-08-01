//! Retained-handle project graph resolution.
//!
//! Resolves an already-mounted `TraceDecay` graph for a requested worktree
//! root, rejecting ambiguous multi-graph matches instead of guessing.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

fn sole_mounted_graph_matching(
    graphs: &[Arc<crate::tracedecay::TraceDecay>],
    predicate: impl Fn(&crate::tracedecay::TraceDecay) -> bool,
) -> std::result::Result<Option<Arc<crate::tracedecay::TraceDecay>>, ()> {
    let mut matches = graphs.iter().filter(|graph| predicate(graph.as_ref()));
    let Some(graph) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(());
    }
    Ok(Some(Arc::clone(graph)))
}

pub(super) fn retained_project_graph_resolver(
    administration: StoreAdministration,
) -> crate::mcp::server::RetainedProjectGraphResolver {
    Arc::new(move |request| {
        let administration = administration.clone();
        Box::pin(async move {
            let graphs = administration.mounted_project_graphs().await;
            let requested_root = authority::canonical_identity_path(
                &request.requested_worktree_root,
            )
            .map_err(|error| {
                TraceDecayError::project_route(
                    "project_route_unavailable",
                    true,
                    format!(
                        "workspace identity is unavailable for {}: {error}",
                        request.requested_worktree_root.display()
                    ),
                )
            })?;
            let registered_root = authority::canonical_identity_path(&request.registered_root)
                .map_err(|error| {
                    TraceDecayError::project_route(
                        "project_route_unavailable",
                        true,
                        format!(
                            "registered project identity is unavailable for {}: {error}",
                            request.registered_root.display()
                        ),
                    )
                })?;
            let Some(owner) = request.owner.as_ref() else {
                return sole_mounted_graph_matching(&graphs, |graph| {
                    authority::canonical_identity_path(graph.project_root()).ok()
                        == Some(requested_root.clone())
                })
                .map_err(|()| {
                    TraceDecayError::project_route(
                        "project_route_ambiguous",
                        false,
                        format!(
                            "multiple mounted graphs claim workspace {}",
                            request.requested_worktree_root.display()
                        ),
                    )
                });
            };
            let project_id = owner.project.project_id.as_str();
            let candidates = graphs
                .into_iter()
                .filter(|graph| {
                    graph.store_layout().identity.project_id.as_deref() == Some(project_id)
                        && request
                            .requested_git_common_dir
                            .as_ref()
                            .is_none_or(|requested| {
                                let requested = authority::canonical_identity_path(requested).ok();
                                let mounted = crate::worktree::git_common_dir(graph.project_root())
                                    .and_then(|path| {
                                        authority::canonical_identity_path(&path).ok()
                                    });
                                mounted.is_none() || mounted == requested
                            })
                })
                .collect::<Vec<_>>();
            let branch_matches = |graph: &crate::tracedecay::TraceDecay| {
                request.requested_branch.as_deref().is_some_and(|branch| {
                    graph.serving_branch() == Some(branch) || graph.active_branch() == Some(branch)
                })
            };
            let root_matches = |graph: &crate::tracedecay::TraceDecay, root: &Path| {
                authority::canonical_identity_path(graph.project_root()).ok()
                    == Some(root.to_path_buf())
            };
            for selected in [
                sole_mounted_graph_matching(&candidates, |graph| {
                    root_matches(graph, &requested_root) && branch_matches(graph)
                }),
                sole_mounted_graph_matching(&candidates, branch_matches),
                sole_mounted_graph_matching(&candidates, |graph| {
                    root_matches(graph, &requested_root)
                }),
                sole_mounted_graph_matching(&candidates, |graph| {
                    root_matches(graph, &registered_root)
                }),
                sole_mounted_graph_matching(&candidates, |_| true),
            ] {
                match selected {
                    Ok(Some(graph)) => return Ok(Some(graph)),
                    Ok(None) => {}
                    Err(()) => {
                        return Err(TraceDecayError::project_route(
                            "project_route_ambiguous",
                            false,
                            format!(
                                "multiple mounted graphs claim registered project '{}'",
                                owner.project.project_id
                            ),
                        ));
                    }
                }
            }
            Ok(None)
        })
    })
}
