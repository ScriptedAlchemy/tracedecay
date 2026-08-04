//! Connection-scoped LSP session tracking.
//!
//! Records which LSP sessions one connection opened so they are all released
//! when it goes away, and authorizes the workspace a request may reach.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

#[derive(Clone)]
pub(super) enum ConnectionLspSessionTransition {
    Reconnect,
    Detach(service::invocation::DaemonLspSessionAccess),
}

pub(super) fn invocation_lsp_session_transition(
    request: &DaemonInvocationRequest,
) -> Option<ConnectionLspSessionTransition> {
    match &request.payload {
        service::invocation::DaemonInvocationPayload::LspReconnect { .. } => {
            Some(ConnectionLspSessionTransition::Reconnect)
        }
        service::invocation::DaemonInvocationPayload::LspDetach { session, .. } => {
            Some(ConnectionLspSessionTransition::Detach(session.clone()))
        }
        _ => None,
    }
}

pub(super) fn update_connection_lsp_sessions(
    sessions: &mut HashMap<String, service::invocation::DaemonLspSessionAccess>,
    transitioned: Option<&ConnectionLspSessionTransition>,
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
            if let Some(ConnectionLspSessionTransition::Detach(detached)) = transitioned {
                sessions.remove(&detached.session_id);
            }
        }
        service::invocation::DaemonInvocationOutcome::Problem {
            problem: service::invocation::DaemonInvocationProblem::Unavailable,
        } => {
            if let Some(ConnectionLspSessionTransition::Detach(detached)) = transitioned {
                sessions.remove(&detached.session_id);
            }
        }
        _ => {}
    }
}

pub(super) async fn cleanup_connection_lsp_sessions(
    invocation: &DaemonInvocationState,
    sessions: HashMap<String, service::invocation::DaemonLspSessionAccess>,
) -> std::result::Result<(), service::invocation::DaemonInvocationProblem> {
    let mut outcome = Ok(());
    for session in sessions.into_values() {
        if let Err(problem) = invocation
            .service
            .disconnect_lsp_session(&invocation.lsp_session_registry, session)
            .await
        {
            outcome = Err(problem);
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_lsp::{LspSessionAccess, LspSessionCredential, LspSessionId};

    fn session_access() -> service::invocation::DaemonLspSessionAccess {
        let access = LspSessionAccess::new(
            LspSessionId::new("connection-owned-session").expect("session id"),
            LspSessionCredential::new(vec![7; 16]).expect("credential"),
        );
        service::invocation::DaemonLspSessionAccess::from_access(&access)
    }

    #[test]
    fn terminal_detach_failure_releases_connection_ownership() {
        let access = session_access();
        let mut sessions = HashMap::from([(access.session_id.clone(), access.clone())]);
        let transition = ConnectionLspSessionTransition::Detach(access);
        let response = DaemonInvocationResponse::problem(
            "request.detach",
            service::invocation::DaemonInvocationProblem::Unavailable,
        );

        update_connection_lsp_sessions(&mut sessions, Some(&transition), &response);

        assert!(
            sessions.is_empty(),
            "a detach failure after terminal cleanup must not trigger a second disconnect"
        );
    }

    #[test]
    fn reconnect_failure_retains_connection_ownership() {
        let access = session_access();
        let mut sessions = HashMap::from([(access.session_id.clone(), access.clone())]);
        let transition = ConnectionLspSessionTransition::Reconnect;
        let response = DaemonInvocationResponse::problem(
            "request.reconnect",
            service::invocation::DaemonInvocationProblem::Unavailable,
        );

        update_connection_lsp_sessions(&mut sessions, Some(&transition), &response);

        assert_eq!(sessions.len(), 1);
    }
}

pub(super) fn admitted_lsp_root_for_project_path(project_path: &Path) -> Option<AdmittedRoot> {
    url::Url::from_file_path(project_path)
        .ok()
        .map(|uri| AdmittedRoot::new(uri.to_string()))
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
    if requested_uris.len() > tracedecay_lsp::MAX_LSP_WORKSPACE_ROOTS {
        return None;
    }
    // A single folder is only ever the active project: a lone sibling hint
    // must not silently reroute the session. A multi-folder workspace may span
    // registered roots, but the active project must be one of them so the
    // session stays anchored to the admitted route.
    let single_root = requested_uris.len() == 1;
    let active_project_path = project_path.canonicalize().ok()?;
    let graphs = store_administration.mounted_project_graphs().await;
    let mut resolved_roots = Vec::with_capacity(requested_uris.len());
    let mut admits_active_project = false;
    for requested_uri in requested_uris {
        let uri = url::Url::parse(&requested_uri).ok()?;
        if uri.scheme() != "file" || uri.query().is_some() || uri.fragment().is_some() {
            return None;
        }
        let requested_path = uri.to_file_path().ok()?.canonicalize().ok()?;
        if single_root && requested_path != active_project_path {
            return None;
        }
        if requested_path == active_project_path {
            admits_active_project = true;
        }
        let canonical_uri = url::Url::from_file_path(&requested_path).ok()?.to_string();
        let mut candidates = Vec::new();
        for graph in &graphs {
            let Some(raw_project_id) = graph.store_layout().identity.project_id.as_deref() else {
                continue;
            };
            let Ok(project_id) = tracedecay_domain::ProjectId::new(raw_project_id.to_owned())
            else {
                continue;
            };
            #[allow(deprecated)]
            let Ok(scope) = crate::application::context::resolve_registered_root_scope(
                graph.project_root(),
                &requested_path,
                &project_id,
            ) else {
                continue;
            };
            if !service
                .lsp_owner_matches_scope(graph.project_root(), &scope)
                .await
            {
                continue;
            }
            candidates.push((graph.project_root().to_path_buf(), scope));
        }
        candidates.sort_by(|left, right| left.1.scope_digest.cmp(&right.1.scope_digest));
        candidates.dedup_by(|left, right| left.1.scope_digest == right.1.scope_digest);
        let [(registered_root, scope)] = candidates.as_slice() else {
            return None;
        };
        resolved_roots.push((registered_root.clone(), canonical_uri, scope.clone()));
    }
    if !admits_active_project {
        return None;
    }
    service
        .authorize_lsp_workspace(resolved_roots, tracedecay_application::clock::now_micros())
        .await
}
