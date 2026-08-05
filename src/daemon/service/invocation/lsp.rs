//! LSP session lifecycle: registered-owner lookup, session admission, frame relay, and expiry.

use super::*;
use tracedecay_lsp::MAX_LSP_WORKSPACE_ROOTS;

pub(super) fn admit_lsp_control(
    request_id: String,
    deadline: &Deadline,
    cancellation: &CancellationContext,
) -> Result<(), DaemonInvocationResponse> {
    if cancellation.is_cancelled() {
        return Err(DaemonInvocationResponse::application_problem(
            request_id,
            ApplicationProblem::cancelled_before_admission(),
        ));
    }
    if deadline.is_elapsed_at(current_micros()) {
        return Err(DaemonInvocationResponse::application_problem(
            request_id,
            ApplicationProblem::timed_out_before_admission(),
        ));
    }
    Ok(())
}

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
    pub(crate) async fn begin_shutdown(&self) {
        *self.lsp_admission_open.lock().await = false;
        self.code_index_schedulers.cancel();
        self.project_runtimes.begin_shutdown();
    }

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
        deadline: &Deadline,
        cancellation: &CancellationContext,
        capability: &CapabilityId,
        use_case: &UseCaseId,
    ) -> Option<RequestContext> {
        if cancellation.is_cancelled() || deadline.is_elapsed_at(observed_at) {
            return None;
        }
        let registered = self
            .project_runtimes
            .get::<RegisteredCallableCodeRuntime>(project_root)
            .await?;
        if registered.scope != *scope {
            return None;
        }
        let access = registered.authorization.current(observed_at).await.ok()?;
        if access.scope != *scope
            || observed_at >= access.grant_expires_at
            || !access.effective_capabilities.contains(capability)
        {
            return None;
        }
        let expires_at = UtcMicros(deadline.expires_at.0.min(access.grant_expires_at.0));
        let grant_digest = canonical_sha256(&(
            "tracedecay.daemon.multi-root-operation-grant.v1",
            &access.scope,
            &access.requester,
            &access.configuration_digest,
            capability,
            use_case,
        ))
        .ok()?;
        let grant = CapabilityGrantSnapshot::new(
            tracedecay_application::CapabilityGrantId::new(format!(
                "grant.daemon.multi-root.{}",
                grant_digest.as_str().trim_start_matches("sha256:")
            ))
            .ok()?,
            1,
            grant_digest,
            access.requester.clone(),
            observed_at,
            expires_at,
            scope.clone(),
            BTreeSet::from([capability.clone()]),
            BTreeSet::from([use_case.clone()]),
            tracedecay_application::DisclosureClass::Sensitive,
        )
        .ok()?;
        RequestContext::new(
            access.requester,
            scope.clone(),
            grant,
            RequestId::new(format!("request.multi-root.query.{ordinal}")).ok()?,
            Deadline::new(expires_at).ok()?,
            cancellation.clone(),
        )
        .ok()
    }

    pub(crate) async fn persisted_scope_set(
        &self,
        project_root: &Path,
        storage: Option<&tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage>,
        scope_set_id: &ScopeSetId,
        operation: tracedecay_application::MultiRootApplicationOperation,
        observed_at: UtcMicros,
        deadline: &Deadline,
        cancellation: &CancellationContext,
    ) -> Option<AuthorizedScopeSet> {
        let (capability, use_case) =
            tracedecay_application::multi_root_operation_authority(operation).ok()?;
        let registered = self
            .project_runtimes
            .get::<RegisteredCallableCodeRuntime>(project_root)
            .await?;
        let context = self
            .multi_root_query_context(
                project_root,
                &registered.scope,
                0,
                observed_at,
                deadline,
                cancellation,
                &capability,
                &use_case,
            )
            .await?;
        let storage = match storage {
            Some(storage) => storage.clone(),
            None => {
                self.lsp_owner(Some(project_root))
                    .await?
                    .scope_set_storage?
            }
        };
        let scope_set = storage.read(scope_set_id).ok()??;
        (scope_set.actor_id() == context.actor()).then_some(scope_set)
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
        for (ordinal, (project_root, uri, scope)) in roots.iter().enumerate() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let grant = owner.scope_grant?;
            if grant.scope != *scope {
                return None;
            }
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
        }
        // Workspace-folder admission is an in-memory session boundary, not a
        // saved scope-set mutation. Persisting the same synthetic selector in
        // every participating project would create partial visibility outside
        // the daemon-owned coordinator/recovery path.
        let scope_set = AuthorizedScopeSetAuthority::authorize(
            scope_set_id,
            ScopeSetRevision::new(1).ok()?,
            contexts,
            &capability,
            &use_case,
            observed_at,
        )
        .ok()?;
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
        let operation = match operation_key {
            "scope_set_read" => tracedecay_application::MultiRootApplicationOperation::ScopeSetRead,
            "scope_set_compare_and_swap" => {
                tracedecay_application::MultiRootApplicationOperation::ScopeSetCompareAndSwap
            }
            "execute" => tracedecay_application::MultiRootApplicationOperation::Execute,
            _ => return None,
        };
        let (capability, use_case) =
            tracedecay_application::multi_root_operation_authority(operation).ok()?;
        let registered = self
            .project_runtimes
            .get::<RegisteredCallableCodeRuntime>(project_root)
            .await?;
        let scope = registered.scope.clone();
        let admitted = self
            .multi_root_query_context(
                project_root,
                &scope,
                usize::MAX,
                observed_at,
                &deadline,
                &cancellation,
                &capability,
                &use_case,
            )
            .await?;
        let context = RequestContext::new(
            admitted.actor().clone(),
            scope.clone(),
            admitted.grant().clone(),
            request_id,
            deadline.clone(),
            cancellation,
        )
        .ok()?;
        let policy_digest = canonical_sha256(&(
            "tracedecay.daemon.multi-root-policy.v1",
            &context.grant().digest,
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
        self.begin_shutdown().await;
        let lease_shutdown = self.lsp_lease_tasks.shutdown().await;
        self.lsp_sessions.lock().await.clear();
        self.authorized_lsp_workspaces.lock().await.clear();
        self.context_scout_registries.lock().await.clear();
        self.project_runtimes.shut_down_all().await;
        if let Ok(mut registry) = pr13_hook_orchestration_registry().lock() {
            registry.retain(|_, runtime| runtime.strong_count() > 0);
        }
        self.operation_events.expire_all().await;
        if let Err(problem) = lease_shutdown {
            tracing::error!(
                ?problem,
                "daemon LSP lease task failed while shutdown joined it"
            );
        }
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
        let admission_guard = self.lsp_admission_open.lock().await;
        if !*admission_guard {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        // Retain this bounded admission lease through endpoint and actor
        // publication so state shutdown cannot sweep between the two.
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
        let endpoint_closed = {
            let mut registry = lsp_registry.lock().await;
            match registry.close(&access, now_ms) {
                Ok(()) => true,
                Err(_) => {
                    registry.reclaim(access.session_id());
                    false
                }
            }
        };
        let Some(mut session) = self.lsp_sessions.lock().await.remove(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let lease_cancelled = self
            .lsp_lease_tasks
            .cancel(access.session_id())
            .await
            .is_ok();
        let actor_detached = match session.actor.lifecycle() {
            SessionLifecycle::Exited => true,
            _ => session.actor.detach().is_ok(),
        };
        if !endpoint_closed {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        if !lease_cancelled || !actor_detached {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        DaemonInvocationResponse::with_outcome(request_id, DaemonInvocationOutcome::LspDetached)
    }

    pub(super) async fn reconnect_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let admission_guard = self.lsp_admission_open.lock().await;
        if !*admission_guard {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        // Reconnect owns this bounded admission lease until endpoint, actor,
        // and expiry-task state have converged.
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
        let mut registry = lsp_registry.lock().await;
        if registry.authenticate(&access, now_ms).is_err() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        if self
            .lsp_lease_tasks
            .cancel(access.session_id())
            .await
            .is_err()
        {
            registry.reclaim(access.session_id());
            drop(registry);
            self.lsp_sessions.lock().await.remove(access.session_id());
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        let reconnected_access = registry.reconnect_with_credential(&access, credential, now_ms);
        let Ok(reconnected_access) = reconnected_access else {
            registry.reclaim(access.session_id());
            drop(registry);
            self.lsp_sessions.lock().await.remove(access.session_id());
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        drop(registry);
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            drop(sessions);
            lsp_registry.lock().await.reclaim(access.session_id());
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
            lsp_registry.lock().await.reclaim(access.session_id());
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
        let session_id = access.session_id().clone();
        let sessions = Arc::clone(&self.lsp_sessions);
        let registry = Arc::clone(lsp_registry);
        let (activate_expiry, expiry_activated) = tokio::sync::oneshot::channel::<u64>();
        let expiry = async move {
            let Ok(expires_at_ms) = expiry_activated.await else {
                return;
            };
            tokio::time::sleep(std::time::Duration::from_millis(
                expires_at_ms.saturating_sub(now_millis()),
            ))
            .await;
            registry.lock().await.expire_at(expires_at_ms);
            sessions
                .lock()
                .await
                .retain(|_, session| session.expires_at_ms > expires_at_ms);
        };
        let mut registry = lsp_registry.lock().await;
        let lifecycle = match registry.authenticate(&access, now_ms) {
            Ok(control) => control.lifecycle(),
            Err(_) => return,
        };
        if lifecycle == SessionLifecycle::Detached {
            return;
        }
        if let Err(problem) = self.lsp_lease_tasks.start(session_id, expiry).await {
            registry.reclaim(access.session_id());
            drop(registry);
            self.lsp_sessions.lock().await.remove(access.session_id());
            tracing::error!(
                ?problem,
                session_id = %access.session_id().as_str(),
                "failed to reserve bounded LSP lease reclamation"
            );
            return;
        }
        if registry.detach(&access, now_ms).is_err() {
            drop(registry);
            if let Err(problem) = self.lsp_lease_tasks.cancel(access.session_id()).await {
                tracing::error!(
                    ?problem,
                    session_id = %access.session_id().as_str(),
                    "failed to join unused LSP lease reservation"
                );
            }
            return;
        }
        drop(registry);
        let actor_detached = {
            let mut sessions = self.lsp_sessions.lock().await;
            let Some(session) = sessions.get_mut(access.session_id()) else {
                drop(sessions);
                lsp_registry.lock().await.reclaim(access.session_id());
                return;
            };
            session.actor.detach().map(|()| session.expires_at_ms)
        };
        let expires_at_ms = match actor_detached {
            Ok(expires_at_ms) => expires_at_ms,
            Err(_) => {
                lsp_registry.lock().await.reclaim(access.session_id());
                self.lsp_sessions.lock().await.remove(access.session_id());
                if let Err(problem) = self.lsp_lease_tasks.cancel(access.session_id()).await {
                    tracing::error!(
                        ?problem,
                        session_id = %access.session_id().as_str(),
                        "failed to join LSP lease task while reclaiming a divergent actor"
                    );
                }
                return;
            }
        };
        if activate_expiry.send(expires_at_ms).is_err()
            && let Err(problem) = self.lsp_lease_tasks.cancel(access.session_id()).await
        {
            tracing::error!(
                ?problem,
                session_id = %access.session_id().as_str(),
                "failed to join concurrently cancelled LSP lease reservation"
            );
        }
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
                    if self
                        .lsp_lease_tasks
                        .cancel(access.session_id())
                        .await
                        .is_err()
                    {
                        return Err(DaemonInvocationProblem::Unavailable);
                    }
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
