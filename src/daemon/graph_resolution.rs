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
            let requested_common_dir = request
                .requested_git_common_dir
                .as_ref()
                .map(|path| authority::canonical_identity_path(path))
                .transpose()
                .map_err(|error| {
                    TraceDecayError::project_route(
                        "project_route_unavailable",
                        true,
                        format!(
                            "workspace repository identity is unavailable for {}: {error}",
                            request.requested_worktree_root.display()
                        ),
                    )
                })?;
            let selected = sole_mounted_graph_matching(&graphs, |graph| {
                authority::canonical_identity_path(graph.project_root()).ok()
                    == Some(requested_root.clone())
                    && request.owner.as_ref().is_none_or(|owner| {
                        graph.store_layout().identity.project_id.as_deref()
                            == Some(owner.project.project_id.as_str())
                    })
                    && requested_common_dir.as_ref().is_none_or(|requested| {
                        crate::worktree::git_common_dir(graph.project_root())
                            .and_then(|path| authority::canonical_identity_path(&path).ok())
                            .as_ref()
                            == Some(requested)
                    })
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
            })?;
            selected
                .ok_or_else(|| {
                    TraceDecayError::project_route(
                        "project_route_unavailable",
                        true,
                        format!(
                            "registered project graph is not mounted for workspace {}",
                            request.requested_worktree_root.display()
                        ),
                    )
                })
                .map(Some)
        })
    })
}
