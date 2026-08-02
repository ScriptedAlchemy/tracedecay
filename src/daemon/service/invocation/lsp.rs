//! LSP session lifecycle: registered-owner lookup, session admission, frame relay, and expiry.

use super::*;
use tracedecay_lsp::MAX_LSP_WORKSPACE_ROOTS;

pub(super) fn canonicalize_lsp_roots(roots: &mut [(PathBuf, String, ResolvedScope)]) -> bool {
    roots.sort_by(|left, right| left.2.scope_digest.cmp(&right.2.scope_digest));
    !roots
        .windows(2)
        .any(|pair| pair[0].2.scope_digest == pair[1].2.scope_digest)
}

pub(super) fn runtime_lsp_actor(
    workspace: AuthorizedLspWorkspace,
    factories: Vec<(AdmittedRoot, Arc<DaemonLspSessionFactory>)>,
) -> Option<RuntimeLspActor> {
    DaemonLspSessionFactory::open_federated_workspace_session(workspace, factories)
}

impl DaemonInvocationService {
    pub(super) async fn install_lsp_owner(
        &self,
        project_root: PathBuf,
        owner: DaemonLspInvocationOwner,
    ) -> Result<(), ProjectRuntimeRegistryError> {
        // Reinstalled on every project open by the same admission authority.
        self.project_runtimes.publish(project_root, owner).await
    }

    pub(crate) async fn lsp_owner(
        &self,
        project_root: Option<&Path>,
    ) -> Option<DaemonLspInvocationOwner> {
        let project_root = project_root?;
        if let Some(owner) = self
            .project_runtimes
            .get::<DaemonLspInvocationOwner>(project_root)
            .await
        {
            return Some(owner);
        }
        let canonical_root = project_root.canonicalize().ok()?;
        self.project_runtimes.get(&canonical_root).await
    }

    pub(crate) async fn lsp_owner_matches_scope(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
    ) -> bool {
        self.lsp_owner(Some(project_root))
            .await
            .and_then(|owner| owner.scope_grant)
            .is_some_and(|grant| grant.scope == *scope)
    }

    pub(crate) async fn multi_root_query_context(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
        ordinal: usize,
        observed_at: UtcMicros,
    ) -> Option<(RequestContext, ManifestDigest)> {
        let owner = self.lsp_owner(Some(project_root)).await?;
        let grant = owner.scope_grant?;
        if grant.scope != *scope {
            return None;
        }
        let digest = grant.digest.clone();
        let context = RequestContext::new(
            grant.issuer.clone(),
            scope.clone(),
            grant,
            RequestId::new(format!("request.multi-root.query.{ordinal}")).ok()?,
            Deadline::new(UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000))).ok()?,
            CancellationContext::active(format!("cancel.multi-root.query.{ordinal}")).ok()?,
        )
        .ok()?;
        Some((context, digest))
    }

    pub(crate) async fn persisted_scope_set(
        &self,
        project_root: &Path,
        scope_set_id: &ScopeSetId,
    ) -> Option<AuthorizedScopeSet> {
        self.lsp_owner(Some(project_root))
            .await?
            .scope_set_storage?
            .read(scope_set_id)
            .ok()?
    }

    pub(crate) async fn authorize_lsp_workspace(
        &self,
        mut roots: Vec<(PathBuf, String, ResolvedScope)>,
        observed_at: UtcMicros,
    ) -> Option<AuthorizedLspWorkspace> {
        if roots.is_empty() || roots.len() > MAX_LSP_WORKSPACE_ROOTS {
            return None;
        }
        if !canonicalize_lsp_roots(&mut roots) {
            return None;
        }
        if let [(project_root, uri, scope)] = roots.as_slice() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let grant = owner.scope_grant?;
            if grant.scope != *scope {
                return None;
            }
            return Some(AuthorizedLspWorkspace::single(AdmittedRoot::authorized(
                uri.clone(),
                scope.scope_digest.clone(),
            )));
        }
        self.authorize_federated_lsp_workspace(&roots, observed_at)
            .await
    }

    async fn authorize_federated_lsp_workspace(
        &self,
        roots: &[(PathBuf, String, ResolvedScope)],
        observed_at: UtcMicros,
    ) -> Option<AuthorizedLspWorkspace> {
        let selector_digest = canonical_sha256(&(
            "tracedecay.daemon.lsp-workspace-selector.v1",
            roots
                .iter()
                .map(|(_, _, scope)| &scope.scope_digest)
                .collect::<Vec<_>>(),
        ))
        .ok()?;
        let scope_set_id = ScopeSetId::new(format!(
            "scope-set.lsp.{}",
            selector_digest.as_str().trim_start_matches("sha256:")
        ))
        .ok()?;
        let capability =
            CapabilityId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_CAPABILITY_ID_V1)
                .ok()?;
        let use_case =
            UseCaseId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_USE_CASE_ID_V1)
                .ok()?;
        let mut contexts = Vec::with_capacity(roots.len());
        let mut factories = Vec::with_capacity(roots.len());
        let mut admitted = Vec::with_capacity(roots.len());
        let mut storages = Vec::with_capacity(roots.len());
        for (ordinal, (project_root, uri, scope)) in roots.iter().enumerate() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let grant = owner.scope_grant?;
            if grant.scope != *scope {
                return None;
            }
            let storage = owner.scope_set_storage?;
            contexts.push(
                RequestContext::new(
                    grant.issuer.clone(),
                    scope.clone(),
                    grant,
                    RequestId::new(format!("request.lsp-workspace.admit.{ordinal}")).ok()?,
                    Deadline::new(UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000)))
                        .ok()?,
                    CancellationContext::active(format!("cancel.lsp-workspace.admit.{ordinal}"))
                        .ok()?,
                )
                .ok()?,
            );
            let root = AdmittedRoot::authorized(uri.clone(), scope.scope_digest.clone());
            factories.push((root.clone(), owner.factory.clone()));
            admitted.push(root);
            storages.push(storage);
        }
        let expected_revision = storages
            .first()?
            .read(&scope_set_id)
            .ok()?
            .map(|current| current.revision());
        let next_revision = match expected_revision {
            Some(current) => ScopeSetRevision::new(current.get().checked_add(1)?).ok()?,
            None => ScopeSetRevision::new(1).ok()?,
        };
        let scope_set = AuthorizedScopeSetAuthority::authorize(
            scope_set_id,
            next_revision,
            contexts,
            &capability,
            &use_case,
            observed_at,
        )
        .ok()?;
        for storage in &storages {
            match storage
                .compare_and_swap(expected_revision, &scope_set)
                .ok()?
            {
                tracedecay_store::runtime::ScopeSetCasOutcomeV1::Applied(_) => {}
                tracedecay_store::runtime::ScopeSetCasOutcomeV1::Conflict { .. } => {
                    let stored = storage.read(scope_set.scope_set_id()).ok()?;
                    if stored.as_ref() != Some(&scope_set) {
                        return None;
                    }
                }
            }
        }
        let digest = scope_set.digest().clone();
        let workspace = AuthorizedLspWorkspace::new(Some(digest.clone()), admitted).ok()?;
        self.authorized_lsp_workspaces.lock().await.insert(
            digest,
            AuthorizedDaemonLspWorkspace {
                scope_set,
                factories,
            },
        );
        Some(workspace)
    }

    pub(crate) async fn compare_and_swap_scope_set(
        &self,
        active_project_root: &Path,
        request: MultiRootScopeSetCasRequestV1,
        mut roots: Vec<(PathBuf, ResolvedScope)>,
        observed_at: UtcMicros,
    ) -> Option<(ResolvedScope, MultiRootScopeSetCasResultV1)> {
        request.validate().ok()?;
        roots.sort_by(|left, right| left.1.scope_digest.cmp(&right.1.scope_digest));
        if roots.is_empty()
            || roots
                .windows(2)
                .any(|pair| pair[0].1.scope_digest == pair[1].1.scope_digest)
        {
            return None;
        }
        let active_owner = self.lsp_owner(Some(active_project_root)).await?;
        let active_scope = active_owner.scope_grant.as_ref()?.scope.clone();
        let active_storage = active_owner.scope_set_storage?;
        let current = active_storage.read(&request.scope_set_id).ok()?;
        let next_revision = match (request.expected_revision, current.as_ref()) {
            (None, None) => ScopeSetRevision::new(1).ok()?,
            (Some(expected), Some(current)) if current.revision() == expected => {
                ScopeSetRevision::new(expected.get().checked_add(1)?).ok()?
            }
            _ => {
                return Some((
                    active_scope,
                    MultiRootScopeSetCasResultV1 {
                        status: MultiRootScopeSetCasStatusV1::Conflict,
                        scope_set: current,
                    },
                ));
            }
        };
        let capability =
            CapabilityId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_CAPABILITY_ID_V1)
                .ok()?;
        let use_case =
            UseCaseId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_USE_CASE_ID_V1)
                .ok()?;
        let mut contexts = Vec::with_capacity(roots.len());
        let mut storages = vec![active_storage.clone()];
        for (ordinal, (project_root, scope)) in roots.iter().enumerate() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let grant = owner.scope_grant?;
            if grant.scope != *scope {
                return None;
            }
            if let Some(storage) = owner.scope_set_storage {
                storages.push(storage);
            }
            contexts.push(
                RequestContext::new(
                    grant.issuer.clone(),
                    scope.clone(),
                    grant,
                    RequestId::new(format!("request.multi-root.cas.{ordinal}")).ok()?,
                    Deadline::new(UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000)))
                        .ok()?,
                    CancellationContext::active(format!("cancel.multi-root.cas.{ordinal}")).ok()?,
                )
                .ok()?,
            );
        }
        let next = AuthorizedScopeSetAuthority::authorize(
            request.scope_set_id,
            next_revision,
            contexts,
            &capability,
            &use_case,
            observed_at,
        )
        .ok()?;
        for storage in storages {
            match storage
                .compare_and_swap(request.expected_revision, &next)
                .ok()?
            {
                tracedecay_store::runtime::ScopeSetCasOutcomeV1::Applied(_) => {}
                tracedecay_store::runtime::ScopeSetCasOutcomeV1::Conflict { .. } => {
                    let stored = storage.read(next.scope_set_id()).ok()?;
                    if stored.as_ref() != Some(&next) {
                        return None;
                    }
                }
            }
        }
        Some((
            active_scope,
            MultiRootScopeSetCasResultV1 {
                status: MultiRootScopeSetCasStatusV1::Applied,
                scope_set: Some(next),
            },
        ))
    }

    pub(crate) async fn multi_root_evidence<T>(
        &self,
        project_root: &Path,
        request_id: RequestId,
        operation_key: &str,
        payload: T,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Option<(ResolvedScope, ApplicationOutcome<T>)>
    where
        T: Serialize,
    {
        let owner = self.lsp_owner(Some(project_root)).await?;
        let grant = owner.scope_grant?;
        let scope = grant.scope.clone();
        let context = RequestContext::new(
            grant.issuer.clone(),
            scope.clone(),
            grant.clone(),
            request_id,
            deadline.clone(),
            cancellation,
        )
        .ok()?;
        let policy_digest = canonical_sha256(&(
            "tracedecay.daemon.multi-root-policy.v1",
            &grant.digest,
            operation_key,
        ))
        .ok()?;
        let policy = PolicyDecisionRef::new(
            format!("policy.daemon.multi-root.{operation_key}.v1"),
            1,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.multi-root-policy.v1").ok()?,
        )
        .ok()?;
        let authority = AuthorityReceipt::from_context(&context, policy, observed_at).ok()?;
        let execution = OperationReceipt::completed(
            observed_at,
            current_micros(),
            deadline,
            OperationBudgetUsage::default(),
        )
        .ok()?;
        let evidence_digest = canonical_sha256(&(
            "tracedecay.daemon.multi-root-evidence.v1",
            operation_key,
            &scope,
            &payload,
        ))
        .ok()?;
        let packet = EvidencePacket {
            temporal: TemporalState::current(execution.ended_at),
            authority,
            evidence_authorities: vec![EvidenceAuthority {
                evidence_id: EvidenceIdentity::new(format!(
                    "evidence.multi-root.{}",
                    evidence_digest.as_str().trim_start_matches("sha256:")
                ))
                .ok()?,
                source_kind: "registered_multi_root".to_owned(),
                producer: operation_key.to_owned(),
                scope: scope.clone(),
                revision: ComponentVersion::new("tracedecay.multi-root.v1").ok()?,
                horizon: Some(execution.ended_at),
            }],
            coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)
                .ok()?,
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page: PageState::first_page(
                SortContractId::new("sort.multi-root.scope-order.v1").ok()?,
                1,
                Some(1),
                1,
            )
            .ok()?,
            execution,
            payload: Some(payload),
        };
        Some((scope, ApplicationOutcome::Evidence(packet)))
    }

    pub(crate) async fn expire_all(&self) {
        self.lsp_sessions.lock().await.clear();
        self.authorized_lsp_workspaces.lock().await.clear();
        self.context_scout_registries.lock().await.clear();
        self.project_runtimes.shut_down_all().await;
        if let Ok(mut registry) = pr13_hook_orchestration_registry().lock() {
            registry.retain(|_, runtime| runtime.strong_count() > 0);
        }
        self.operation_events.expire_all().await;
    }

    #[cfg(all(test, not(windows)))]
    pub(crate) async fn active_lsp_runtime_count(&self) -> usize {
        self.lsp_sessions.lock().await.len()
    }

    pub(super) async fn open_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        workspace: Option<AuthorizedLspWorkspace>,
        request_id: String,
        client_revision: String,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
        now_ms: u64,
        lsp_owner: Option<DaemonLspInvocationOwner>,
    ) -> DaemonInvocationResponse {
        let Some(workspace) = workspace else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let Some(lsp_owner) = lsp_owner else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let authorized = if let Some(digest) = workspace.scope_set_digest() {
            self.authorized_lsp_workspaces
                .lock()
                .await
                .get(digest)
                .cloned()
        } else {
            None
        };
        if authorized.as_ref().is_some_and(|authorized| {
            !authorized
                .factories
                .iter()
                .any(|(_, factory)| Arc::ptr_eq(factory, &lsp_owner.factory))
        }) {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        let request = LspSessionOpenRequest {
            requested_root_uri,
            workspace_folders,
            client_revision,
        };
        let access = {
            let mut registry = lsp_registry.lock().await;
            let existing = std::mem::take(&mut *registry);
            let mut endpoint = DaemonLspSessionEndpoint::with_registry(
                AdmittedWorkspaceSessionAdmission {
                    workspace: workspace.clone(),
                },
                existing,
            );
            let result = endpoint.open(request, now_ms);
            *registry = endpoint.into_registry();
            result
        };
        let Ok(access) = access else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let expires_at_ms = now_ms.saturating_add(LSP_SESSION_TTL_MS);
        let session_id = access.session_id().clone();
        let (actor, scope_set_id, scope_set_digest) = match authorized {
            Some(authorized) => {
                let Some(actor) = runtime_lsp_actor(workspace, authorized.factories) else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                (
                    actor,
                    Some(authorized.scope_set.scope_set_id().clone()),
                    Some(authorized.scope_set.digest().clone()),
                )
            }
            None => (
                lsp_owner.factory.open_workspace_session(workspace),
                None,
                None,
            ),
        };
        self.lsp_sessions.lock().await.insert(
            session_id,
            RuntimeLspSession {
                expires_at_ms,
                actor,
            },
        );
        DaemonInvocationResponse::lsp_opened(
            request_id,
            DaemonLspSessionAccess::from_access(&access),
            expires_at_ms,
            scope_set_id,
            scope_set_digest,
        )
    }

    pub(super) async fn send_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        frame: String,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let dispatch = session.actor.handle_payload(frame.as_bytes(), now_ms);
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspFrameAccepted {
                backpressured: dispatch.backpressured,
                closed: dispatch.closed,
            },
        )
    }

    pub(super) async fn poll_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let dispatch = session.actor.flush_due(now_ms);
        let frame = session
            .actor
            .poll_outbound()
            .and_then(|frame| std::str::from_utf8(frame).ok())
            .map(str::to_owned);
        let closed = dispatch.closed
            || matches!(
                session.actor.lifecycle(),
                SessionLifecycle::Exited | SessionLifecycle::Expired
            );
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspFrame { frame, closed },
        )
    }

    pub(super) async fn acknowledge_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspAcknowledged {
                acknowledged: session.actor.acknowledge_outbound(),
            },
        )
    }

    pub(super) async fn detach_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let endpoint_detached = {
            let mut registry = lsp_registry.lock().await;
            registry.close(&access, now_ms).is_ok()
        };
        let Some(mut session) = self.lsp_sessions.lock().await.remove(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if !endpoint_detached {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        let _ = session.actor.detach();
        DaemonInvocationResponse::with_outcome(request_id, DaemonInvocationOutcome::LspDetached)
    }

    pub(super) async fn reconnect_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match session.into_access() {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut credential_bytes = [0_u8; 32];
        if getrandom::getrandom(&mut credential_bytes).is_err() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        let Ok(credential) = LspSessionCredential::new(credential_bytes.to_vec()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let reconnected_access = lsp_registry
            .lock()
            .await
            .reconnect_with_credential(&access, credential, now_ms);
        let Ok(reconnected_access) = reconnected_access else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            drop(sessions);
            let _ = lsp_registry.lock().await.close(&reconnected_access, now_ms);
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let actor_reconnected = match session.actor.lifecycle() {
            SessionLifecycle::Detached => session.actor.reconnect().is_ok(),
            SessionLifecycle::AwaitingInitialize
            | SessionLifecycle::AwaitingInitialized
            | SessionLifecycle::Ready
            | SessionLifecycle::Shutdown => true,
            SessionLifecycle::Exited | SessionLifecycle::Expired => false,
        };
        if !actor_reconnected {
            drop(sessions);
            let _ = lsp_registry.lock().await.close(&reconnected_access, now_ms);
            self.lsp_sessions.lock().await.remove(access.session_id());
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspReconnected {
                session: DaemonLspSessionAccess::from_access(&reconnected_access),
            },
        )
    }

    pub(crate) async fn disconnect_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        session: DaemonLspSessionAccess,
    ) {
        let Ok(access) = session.into_access() else {
            return;
        };
        let now_ms = now_millis();
        if lsp_registry.lock().await.detach(&access, now_ms).is_err() {
            return;
        }
        let expires_at_ms = {
            let mut sessions = self.lsp_sessions.lock().await;
            let Some(session) = sessions.get_mut(access.session_id()) else {
                return;
            };
            let _ = session.actor.detach();
            session.expires_at_ms
        };
        let sessions = Arc::clone(&self.lsp_sessions);
        let registry = Arc::clone(lsp_registry);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(
                expires_at_ms.saturating_sub(now_millis()),
            ))
            .await;
            let now_ms = now_millis();
            registry.lock().await.expire_at(now_ms);
            sessions
                .lock()
                .await
                .retain(|_, session| session.expires_at_ms > now_ms);
        });
    }

    pub(super) async fn authenticate(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> Result<LspSessionAccess, DaemonInvocationProblem> {
        let access = session.into_access()?;
        let authentication = {
            let mut registry = lsp_registry.lock().await;
            registry
                .authenticate(&access, now_ms)
                .map(|_| ())
                .map_err(|error| matches!(error, LspEndpointError::SessionExpired))
        };
        match authentication {
            Ok(()) => Ok(access),
            Err(expired) => {
                if expired {
                    self.lsp_sessions.lock().await.remove(access.session_id());
                }
                Err(DaemonInvocationProblem::NotFoundOrNotAuthorized)
            }
        }
    }

    pub(super) async fn expire_sessions(&self, now_ms: u64) {
        self.lsp_sessions
            .lock()
            .await
            .retain(|_, session| session.expires_at_ms > now_ms);
    }
}
