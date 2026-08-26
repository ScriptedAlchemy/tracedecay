//! Retained-handle project server resolution.
//!
//! Resolves an already-mounted `McpServer` for a requested worktree root,
//! rejecting ambiguous matches instead of mixing one graph with another
//! server's query, session, application, or lifecycle authorities.
//!
//! The resolver returns the selected retained server rather than reconstructing
//! a graph-only authority from registry paths.

use super::*;

fn sole_mounted_server_matching(
    servers: &[(
        Arc<crate::mcp::McpServer>,
        Arc<crate::tracedecay::TraceDecay>,
    )],
    predicate: impl Fn(&crate::tracedecay::TraceDecay) -> bool,
) -> std::result::Result<Option<Arc<crate::mcp::McpServer>>, ()> {
    let mut matches = servers
        .iter()
        .filter(|(_, graph)| predicate(graph.as_ref()));
    let Some((server, _)) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(());
    }
    Ok(Some(Arc::clone(server)))
}

pub(super) fn retained_project_server_resolver(
    administration: StoreAdministration,
) -> crate::mcp::server::RetainedProjectServerResolver {
    Arc::new(move |request| {
        let administration = administration.clone();
        Box::pin(async move {
            let expected_profile_id = administration.profile_identity()?.profile_id().clone();
            let mounted_servers = {
                let servers = administration.project_servers().lock().await;
                servers
                    .values()
                    .filter(|server| {
                        server
                            .profile_identity()
                            .is_some_and(|identity| identity.profile_id() == &expected_profile_id)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let mut servers = Vec::with_capacity(mounted_servers.len());
            for server in mounted_servers {
                servers.push((Arc::clone(&server), server.cg().await));
            }
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
                return sole_mounted_server_matching(&servers, |graph| {
                    authority::canonical_identity_path(graph.project_root()).ok()
                        == Some(requested_root.clone())
                })
                .map_err(|()| {
                    TraceDecayError::project_route(
                        "project_route_ambiguous",
                        false,
                        format!(
                            "multiple mounted project servers claim workspace {}",
                            request.requested_worktree_root.display()
                        ),
                    )
                });
            };
            let project_id = owner.project.project_id.as_str();
            let candidates = servers
                .into_iter()
                .filter(|(_, graph)| {
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
                sole_mounted_server_matching(&candidates, |graph| {
                    root_matches(graph, &requested_root) && branch_matches(graph)
                }),
                sole_mounted_server_matching(&candidates, branch_matches),
                sole_mounted_server_matching(&candidates, |graph| {
                    root_matches(graph, &requested_root)
                }),
                sole_mounted_server_matching(&candidates, |graph| {
                    root_matches(graph, &registered_root)
                }),
                sole_mounted_server_matching(&candidates, |_| true),
            ] {
                match selected {
                    Ok(Some(server)) => return Ok(Some(server)),
                    Ok(None) => {}
                    Err(()) => {
                        return Err(TraceDecayError::project_route(
                            "project_route_ambiguous",
                            false,
                            format!(
                                "multiple mounted project servers claim registered project '{}'",
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
