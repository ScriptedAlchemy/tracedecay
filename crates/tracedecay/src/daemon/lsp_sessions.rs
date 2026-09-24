//! Connection-scoped LSP session tracking.
//!
//! Records which LSP sessions one connection opened so they are all released
//! when it goes away, and authorizes the workspace a request may reach.

use tracedecay_daemon_service::{
    DaemonInvocationOutcome, DaemonInvocationPayload, DaemonInvocationProblem,
    DaemonInvocationService, DaemonLspSessionAccess,
};
use tracedecay_runtime_core::logging::log_daemon_event;

use super::*;

pub(super) fn invocation_lsp_session_transition(
    request: &DaemonInvocationRequest,
) -> Option<DaemonLspSessionAccess> {
    match &request.payload {
        DaemonInvocationPayload::LspReconnect { session, .. }
        | DaemonInvocationPayload::LspDetach { session, .. } => Some(session.clone()),
        _ => None,
    }
}

pub(super) fn update_connection_lsp_sessions(
    sessions: &mut HashMap<String, DaemonLspSessionAccess>,
    transitioned: Option<&DaemonLspSessionAccess>,
    response: &DaemonInvocationResponse,
) {
    match &response.outcome {
        DaemonInvocationOutcome::LspOpened { session, .. } => {
            sessions.insert(session.session_id.clone(), session.clone());
        }
        DaemonInvocationOutcome::LspReconnected { session } => {
            sessions.insert(session.session_id.clone(), session.clone());
        }
        DaemonInvocationOutcome::LspDetached => {
            if let Some(detached) = transitioned {
                sessions.remove(&detached.session_id);
            }
        }
        _ => {}
    }
}

#[hotpath::measure(label = "daemon.lsp_sessions.cleanup", future = true)]
pub(super) async fn cleanup_connection_lsp_sessions(
    invocation: &DaemonInvocationState,
    sessions: HashMap<String, DaemonLspSessionAccess>,
) {
    for session in sessions.into_values() {
        invocation
            .service
            .disconnect_lsp_session(&invocation.lsp_session_registry, session)
            .await;
    }
}

pub(super) fn admitted_lsp_root_for_project_path(project_path: &Path) -> Option<AdmittedRoot> {
    // The root published to clients and the root document containment strips
    // against have to be the same string, so both come from one authority.
    tracedecay_application::primitives::admitted_root_uri_for_project(project_path)
        .ok()
        .map(AdmittedRoot::new)
}

pub(super) async fn admitted_lsp_workspace_for_request(
    store_administration: &StoreAdministration,
    service: &DaemonInvocationService,
    project_path: &Path,
    request: &DaemonInvocationRequest,
) -> Option<AuthorizedLspWorkspace> {
    let requested_uris = match request.lsp_workspace_folders()? {
        [] => vec![url::Url::from_file_path(project_path).ok()?.to_string()],
        folders => folders.to_vec(),
    };
    authorize_lsp_workspace_for_uris(store_administration, service, project_path, requested_uris)
        .await
}

/// Settles the one fenced workspace-folder mutation a session actor may hold
/// after a client frame. Only the daemon resolves and authorizes folder URIs:
/// an authorized next root set is applied with the client's active root
/// preserved as the anchor; anything else rejects the intent so the actor's
/// fence never dangles.
#[hotpath::measure(label = "daemon.lsp_sessions.settle", future = true)]
pub(super) async fn settle_pending_lsp_workspace_mutation(
    store_administration: &StoreAdministration,
    service: &DaemonInvocationService,
    project_path: &Path,
    session: &DaemonLspSessionAccess,
) {
    let Some(mutation) = service.pending_lsp_workspace_folder_mutation(session).await else {
        return;
    };
    let workspace = authorize_lsp_workspace_for_uris(
        store_administration,
        service,
        project_path,
        mutation.next_root_uris.clone(),
    )
    .await
    .and_then(|workspace| {
        AuthorizedLspWorkspace::anchored(
            workspace.scope_set_digest().cloned(),
            workspace.roots().to_vec(),
            mutation.active_root_uri.clone(),
        )
        .ok()
    });
    service
        .settle_lsp_workspace_folder_mutation(session, &mutation, workspace)
        .await;
}

#[hotpath::measure(label = "daemon.lsp_sessions.authorize", future = true)]
async fn authorize_lsp_workspace_for_uris(
    store_administration: &StoreAdministration,
    service: &DaemonInvocationService,
    project_path: &Path,
    requested_uris: Vec<String>,
) -> Option<AuthorizedLspWorkspace> {
    if requested_uris.is_empty()
        || requested_uris.len() > tracedecay_daemon_protocol::MAX_LSP_WORKSPACE_ROOTS
    {
        return lsp_workspace_refused("root_count_out_of_bounds", project_path);
    }
    // A single folder is only ever the active project: a lone sibling hint
    // must not silently reroute the session. A multi-folder workspace may span
    // registered roots, but the active project must be one of them so the
    // session stays anchored to the admitted route.
    let single_root = requested_uris.len() == 1;
    let Ok(active_project_path) = project_path.canonicalize() else {
        return lsp_workspace_refused("active_project_unresolvable", project_path);
    };
    let graphs = store_administration.mounted_project_graphs().await;
    let mut selectors = Vec::with_capacity(requested_uris.len());
    let mut canonical_uris = BTreeMap::new();
    let mut admits_active_project = false;
    for requested_uri in requested_uris {
        let Ok(uri) = url::Url::parse(&requested_uri) else {
            return lsp_workspace_refused("root_uri_unparseable", project_path);
        };
        if uri.scheme() != "file" || uri.query().is_some() || uri.fragment().is_some() {
            return lsp_workspace_refused("root_uri_not_a_local_file", project_path);
        }
        let Some(requested_path) = uri
            .to_file_path()
            .ok()
            .and_then(|path| path.canonicalize().ok())
        else {
            return lsp_workspace_refused("root_path_unresolvable", project_path);
        };
        if single_root
            && !tracedecay_runtime_core::path_safety::same_canonical_path(
                &requested_path,
                &active_project_path,
            )
        {
            return lsp_workspace_refused("single_root_is_not_the_active_project", &requested_path);
        }
        if tracedecay_runtime_core::path_safety::same_canonical_path(
            &requested_path,
            &active_project_path,
        ) {
            admits_active_project = true;
        }
        let mut candidates = Vec::new();
        for graph in &graphs {
            if !tracedecay_runtime_core::path_safety::same_canonical_path(
                graph.project_root(),
                &requested_path,
            ) {
                continue;
            }
            let Some(raw_project_id) = graph.store_layout().identity.project_id.as_deref() else {
                continue;
            };
            let Ok(project_id) = tracedecay_domain::ProjectId::new(raw_project_id.to_owned())
            else {
                continue;
            };
            candidates.push(project_id);
        }
        candidates.sort();
        candidates.dedup();
        let [project_id] = candidates.as_slice() else {
            return lsp_workspace_refused(
                if candidates.is_empty() {
                    "root_has_no_mounted_project"
                } else {
                    "root_has_ambiguous_mounted_projects"
                },
                &requested_path,
            );
        };
        let Ok(selector) = tracedecay_contracts::RegisteredRootSelectorV1::new(
            project_id.clone(),
            requested_path.clone(),
        ) else {
            return lsp_workspace_refused("root_selector_invalid", &requested_path);
        };
        selectors.push(selector);
        let Ok(canonical_uri) = url::Url::from_file_path(&requested_path) else {
            return lsp_workspace_refused("root_uri_unrepresentable", &requested_path);
        };
        canonical_uris.insert(requested_path, canonical_uri.to_string());
    }
    if !admits_active_project {
        return lsp_workspace_refused("active_project_not_requested", project_path);
    }
    let resolved = match super::invocation_dispatch::resolve_multi_root_projects(
        store_administration,
        service,
        &selectors,
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(problem) => {
            return lsp_workspace_refused(
                if problem == DaemonInvocationProblem::Unavailable {
                    "registered_root_unavailable"
                } else {
                    "registered_root_not_authorized"
                },
                project_path,
            );
        }
    };
    let mut resolved_roots = Vec::with_capacity(resolved.len());
    for (root, scope, locator) in resolved {
        let Some(uri) = canonical_uris.get(&root).cloned() else {
            return lsp_workspace_refused("resolved_root_spelling_diverged", &root);
        };
        resolved_roots.push((root, uri, scope, locator));
    }
    let authorized = service
        .authorize_lsp_workspace(resolved_roots, tracedecay_contracts::clock::now_micros())
        .await;
    if authorized.is_none() {
        return lsp_workspace_refused("workspace_authorization_refused", project_path);
    }
    authorized
}

/// Every refusal above reaches the client as the same non-diagnostic
/// `Denied`, so the cause is recorded in the operator log instead.
fn lsp_workspace_refused<T>(reason_code: &str, root: &Path) -> Option<T> {
    log_daemon_event(
        "lsp_workspace_refused",
        &[
            ("root", root.display().to_string()),
            ("reason_code", reason_code.to_owned()),
        ],
    );
    None
}
