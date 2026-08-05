//! Connection-scoped LSP session tracking.
//!
//! Records which LSP sessions one connection opened so they are all released
//! when it goes away, and authorizes the workspace a request may reach.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

pub(super) fn invocation_lsp_session_transition(
    request: &DaemonInvocationRequest,
) -> Option<service::invocation::DaemonLspSessionAccess> {
    match &request.payload {
        service::invocation::DaemonInvocationPayload::LspReconnect { session, .. }
        | service::invocation::DaemonInvocationPayload::LspDetach { session, .. } => {
            Some(session.clone())
        }
        _ => None,
    }
}

pub(super) fn update_connection_lsp_sessions(
    sessions: &mut HashMap<String, service::invocation::DaemonLspSessionAccess>,
    transitioned: Option<&service::invocation::DaemonLspSessionAccess>,
    response: &DaemonInvocationResponse,
) {
    match &response.outcome {
        service::invocation::DaemonInvocationOutcome::LspOpened { session, .. } => {
            sessions.insert(session.session_id.clone(), session.clone());
        }
        service::invocation::DaemonInvocationOutcome::LspReconnected { session } => {
            sessions.insert(session.session_id.clone(), session.clone());
        }
        service::invocation::DaemonInvocationOutcome::LspDetached => {
            if let Some(detached) = transitioned {
                sessions.remove(&detached.session_id);
            }
        }
        _ => {}
    }
}

pub(super) async fn cleanup_connection_lsp_sessions(
    invocation: &DaemonInvocationState,
    sessions: HashMap<String, service::invocation::DaemonLspSessionAccess>,
) {
    for session in sessions.into_values() {
        invocation
            .service
            .disconnect_lsp_session(&invocation.lsp_session_registry, session)
            .await;
    }
}

pub(super) fn admitted_lsp_root_for_project_path(project_path: &Path) -> Option<AdmittedRoot> {
    url::Url::from_file_path(project_path)
        .ok()
        .map(|uri| AdmittedRoot::new(uri.to_string()))
}

pub(super) async fn registered_lsp_root_selectors_for_uris(
    store_administration: &StoreAdministration,
    requested_uris: &[String],
) -> Option<(
    Vec<tracedecay_application::RegisteredRootSelectorV1>,
    BTreeMap<PathBuf, String>,
)> {
    if requested_uris.is_empty() || requested_uris.len() > tracedecay_lsp::MAX_LSP_WORKSPACE_ROOTS {
        return None;
    }
    let graphs = store_administration.mounted_project_graphs().await;
    let mut selectors = Vec::with_capacity(requested_uris.len());
    let mut canonical_uris = BTreeMap::new();
    for requested_uri in requested_uris {
        let uri = url::Url::parse(requested_uri).ok()?;
        if uri.scheme() != "file" || uri.query().is_some() || uri.fragment().is_some() {
            return None;
        }
        let requested_path = uri.to_file_path().ok()?.canonicalize().ok()?;
        let mut candidates = graphs
            .iter()
            .filter(|graph| graph.project_root() == requested_path)
            .filter_map(|graph| graph.store_layout().identity.project_id.as_deref())
            .filter_map(|project_id| tracedecay_domain::ProjectId::new(project_id.to_owned()).ok())
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        let [project_id] = candidates.as_slice() else {
            return None;
        };
        selectors.push(
            tracedecay_application::RegisteredRootSelectorV1::new(
                project_id.clone(),
                requested_path.clone(),
            )
            .ok()?,
        );
        canonical_uris.insert(
            requested_path.clone(),
            url::Url::from_file_path(requested_path).ok()?.to_string(),
        );
    }
    Some((selectors, canonical_uris))
}

pub(super) async fn admitted_lsp_workspace_for_request(
    store_administration: &StoreAdministration,
    service: &service::invocation::DaemonInvocationService,
    project_path: &Path,
    request: &DaemonInvocationRequest,
) -> Option<AuthorizedLspWorkspace> {
    let requested_uris = match request.lsp_workspace_folders()? {
        [] => vec![url::Url::from_file_path(project_path).ok()?.to_string()],
        folders => folders.to_vec(),
    };
    // A single folder is only ever the active project: a lone sibling hint
    // must not silently reroute the session. A multi-folder workspace may span
    // registered roots, but the active project must be one of them so the
    // session stays anchored to the admitted route.
    let active_project_path = project_path.canonicalize().ok()?;
    let (selectors, canonical_uris) =
        registered_lsp_root_selectors_for_uris(store_administration, &requested_uris).await?;
    if !canonical_uris.contains_key(&active_project_path) {
        return None;
    }
    let anchor_root_uri = canonical_uris.get(&active_project_path)?.clone();
    let resolved = super::invocation_dispatch::resolve_multi_root_projects(
        store_administration,
        service,
        &selectors,
    )
    .await
    .ok()?;
    let resolved_roots = resolved
        .into_iter()
        .map(|(root, scope, locator)| {
            let uri = canonical_uris.get(&root)?.clone();
            Some((root, uri, scope, locator))
        })
        .collect::<Option<Vec<_>>>()?;
    service
        .authorize_lsp_workspace(
            resolved_roots,
            anchor_root_uri,
            tracedecay_application::clock::now_micros(),
        )
        .await
}
