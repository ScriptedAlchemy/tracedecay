//! LSP session lifecycle: registered-owner lookup, session admission, frame relay, and expiry.

use super::*;
use tracedecay_lsp::LspRuntimeFailure;
use tracedecay_runtime_core::cancellation::CancellationToken;

mod project_lifecycle;
mod workspace_admission;
mod workspace_diagnostics;

use workspace_admission::CurrentLspWorkspaceAuthorityV1;
pub(super) use workspace_diagnostics::PublishedCodeIndexWorkspaceDocuments;

pub(super) fn admit_lsp_control(
    request_id: String,
    deadline: &Deadline,
    cancellation: &CancellationContext,
) -> Result<(), Box<DaemonInvocationResponse>> {
    if cancellation.is_cancelled() {
        return Err(Box::new(DaemonInvocationResponse::application_problem(
            request_id,
            ApplicationProblem::cancelled_before_admission(),
        )));
    }
    if deadline.is_elapsed_at(current_micros()) {
        return Err(Box::new(DaemonInvocationResponse::application_problem(
            request_id,
            ApplicationProblem::timed_out_before_admission(),
        )));
    }
    Ok(())
}

pub(super) fn canonicalize_lsp_roots(
    roots: &mut [(
        PathBuf,
        String,
        ResolvedScope,
        tracedecay_application::RegisteredRootLocatorV1,
    )],
) -> bool {
    roots.sort_by(|left, right| left.2.scope_digest.cmp(&right.2.scope_digest));
    !roots
        .windows(2)
        .any(|pair| pair[0].2.scope_digest == pair[1].2.scope_digest)
}

pub(super) async fn runtime_lsp_actor(
    workspace: AuthorizedLspWorkspace,
    factories: Vec<(AdmittedRoot, Arc<DaemonLspSessionFactory>)>,
) -> std::result::Result<Option<RuntimeLspActor>, LspRuntimeFailure> {
    DaemonLspSessionFactory::open_federated_workspace_session(workspace, factories).await
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
        let owner = self.lsp_owner(Some(project_root)).await?;
        let grant = owner.scope_grant?;
        let scope_set = owner.scope_set_storage?.read(scope_set_id).ok().flatten()?;
        // Persisted scope routes are actor-sealed: an admission on the active
        // root authorizes reading only the scope sets its own actor persisted,
        // never another actor's mounted routes.
        (scope_set.actor_id() == &grant.issuer).then_some(scope_set)
    }

    pub(crate) async fn compare_and_swap_scope_set(
        &self,
        active_project_root: &Path,
        request: MultiRootScopeSetCasRequestV1,
        mut roots: Vec<(
            PathBuf,
            ResolvedScope,
            tracedecay_application::RegisteredRootLocatorV1,
        )>,
        observed_at: UtcMicros,
        deadline: &Deadline,
        request_cancellation: Option<&CancellationToken>,
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
        let mut admissions = Vec::with_capacity(roots.len());
        let mut storages = vec![active_storage.clone()];
        for (ordinal, (project_root, scope, locator)) in roots.iter().enumerate() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let grant = owner.scope_grant?;
            if grant.scope != *scope {
                return None;
            }
            if let Some(storage) = owner.scope_set_storage {
                storages.push(storage);
            }
            let context = RequestContext::new(
                grant.issuer.clone(),
                scope.clone(),
                grant,
                RequestId::new(format!("request.multi-root.cas.{ordinal}")).ok()?,
                Deadline::new(UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000))).ok()?,
                CancellationContext::active(format!("cancel.multi-root.cas.{ordinal}")).ok()?,
            )
            .ok()?;
            admissions.push(
                tracedecay_application::AuthorizedRootAdmission::new(context, locator.clone())
                    .ok()?,
            );
        }
        let next = AuthorizedScopeSetAuthority::authorize_registered(
            request.scope_set_id,
            next_revision,
            admissions,
            &capability,
            &use_case,
            observed_at,
        )
        .ok()?;
        if request_cancellation.is_some_and(CancellationToken::is_cancelled)
            || deadline.is_elapsed_at(current_micros())
        {
            return None;
        }
        // Cancellation or deadline expiry may refuse the effect before its
        // first durable write. Once the first compare-and-swap starts, finish
        // every authorized replica so the caller receives one authoritative
        // settlement instead of a fabricated interruption over a partial
        // effect.
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
        self.begin_shutdown().await;
        let lease_shutdown = self.lsp_lease_tasks.shutdown().await;
        self.lsp_sessions.lock().await.clear();
        self.authorized_lsp_workspaces.lock().await.clear();
        self.context_scout_registries.lock().await.clear();
        self.project_runtimes.shut_down_all().await;
        self.operation_events.expire_all().await;
        if let Err(problem) = lease_shutdown {
            tracing::error!(
                ?problem,
                "daemon LSP lease task failed while shutdown joined it"
            );
        }
    }

    #[cfg(test)]
    pub(crate) async fn active_lsp_runtime_count(&self) -> usize {
        self.lsp_sessions.lock().await.len()
    }

    async fn admit_lsp_workspace_holder(
        &self,
        workspace: &AuthorizedLspWorkspace,
    ) -> Option<Vec<tokio::sync::OwnedRwLockReadGuard<()>>> {
        let mut roots = Vec::with_capacity(workspace.roots().len());
        for root in workspace.roots() {
            let path = url::Url::parse(root.uri()).ok()?.to_file_path().ok()?;
            roots.push(path);
        }
        self.worktree_holder_admission.admit_holders(roots).await
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
        let Some(workspace_authority) = self
            .current_lsp_workspace_authority(&workspace, Some(&lsp_owner))
            .await
        else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let Some(_holder_admission) = self.admit_lsp_workspace_holder(&workspace).await else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let (actor, scope_set_id, scope_set_digest) = match workspace_authority {
            CurrentLspWorkspaceAuthorityV1::Federated(authorized) => {
                let Ok(Some(actor)) =
                    runtime_lsp_actor(workspace.clone(), authorized.factories).await
                else {
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
            CurrentLspWorkspaceAuthorityV1::Single => {
                let Ok(actor) = lsp_owner
                    .factory
                    .open_workspace_session(workspace.clone())
                    .await
                else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                (actor, None, None)
            }
        };
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
        let project_identity = lsp_owner.project_identity.clone();
        self.lsp_sessions.lock().await.insert(
            session_id,
            RuntimeLspSession {
                expires_at_ms,
                project_identity,
                actor,
                delivery_settlements: lsp_owner.delivery_settlements,
                in_flight_delivery_attempt: None,
                next_delivery_sequence: 1,
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
        let admission = session
            .actor
            .try_handle_client_payload(frame.as_bytes(), now_ms);
        let (backpressured, closed) = match admission {
            ClientFrameAdmission::Consumed(dispatch) => (false, dispatch.closed),
            ClientFrameAdmission::Backpressured => (true, false),
            ClientFrameAdmission::Closed => (false, true),
        };
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspFrameAccepted {
                backpressured,
                closed,
            },
        )
    }

    /// The one fenced workspace-folder intent a session actor is holding for
    /// its daemon owner, if any.
    pub(crate) async fn pending_lsp_workspace_folder_mutation(
        &self,
        session: &DaemonLspSessionAccess,
    ) -> Option<tracedecay_lsp::WorkspaceFolderMutation> {
        let access = session.clone().into_access().ok()?;
        let sessions = self.lsp_sessions.lock().await;
        sessions
            .get(access.session_id())?
            .actor
            .pending_workspace_folder_mutation()
    }

    /// Answers a fenced workspace-folder intent: an authorized workspace
    /// applies it, `None` rejects it. A stale fence (the actor re-parsed a
    /// newer change or the scope set moved) settles as a no-op.
    pub(crate) async fn settle_lsp_workspace_folder_mutation(
        &self,
        session: &DaemonLspSessionAccess,
        mutation: &tracedecay_lsp::WorkspaceFolderMutation,
        mut workspace: Option<AuthorizedLspWorkspace>,
    ) {
        let admission_guard = self.lsp_admission_open.lock().await;
        let Ok(access) = session.clone().into_access() else {
            return;
        };
        if !*admission_guard {
            workspace = None;
        }
        let _holder_admission = match workspace.take() {
            Some(candidate) => match self.current_lsp_workspace_authority(&candidate, None).await {
                Some(_) => match self.admit_lsp_workspace_holder(&candidate).await {
                    Some(admission) => {
                        workspace = Some(candidate);
                        Some(admission)
                    }
                    None => None,
                },
                None => None,
            },
            None => None,
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(state) = sessions.get_mut(access.session_id()) else {
            return;
        };
        let _ = match workspace {
            Some(workspace) => state
                .actor
                .apply_workspace_folder_mutation(mutation, workspace),
            None => state.actor.reject_workspace_folder_mutation(mutation),
        };
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
        let outbound = session.actor.poll_outbound().map(ToOwned::to_owned);
        if let Some(frame) = outbound.as_deref() {
            let _ = retain_lsp_delivery_attempt(
                &mut session.in_flight_delivery_attempt,
                &mut session.next_delivery_sequence,
                frame,
                access.session_id(),
                current_micros(),
            );
        }
        let frame = outbound.and_then(|frame| String::from_utf8(frame).ok());
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
        let acknowledged = session.actor.acknowledge_outbound();
        if acknowledged {
            session.settle_in_flight_delivery(
                tracedecay_domain::DeliverySettlementOutcomeV1::Delivered,
                None,
            );
        }
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspAcknowledged { acknowledged },
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
        // The stdio bridge reaches this path after a write or flush failure.
        // Removing the actor discards its in-flight frame, so terminalize that
        // exact captured attempt before any later detach bookkeeping can fail.
        session.settle_in_flight_delivery(
            tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
            Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
        );
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
        let expires_at_ms = now_ms.saturating_add(LSP_SESSION_TTL_MS);
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
        session.expires_at_ms = expires_at_ms;
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
        let _lsp_admission = self.lsp_admission_open.lock().await;
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
                if let Err(problem) = self.lsp_lease_tasks.cancel(access.session_id()).await {
                    tracing::error!(
                        ?problem,
                        session_id = %access.session_id().as_str(),
                        "failed to join LSP lease after project session retirement"
                    );
                }
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
