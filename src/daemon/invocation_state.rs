//! `DaemonInvocationState`: daemon-generation-local state for the closed
//! invocation protocol, shared by the Unix and portable brokers.

use std::sync::Arc;

use serde_json::Value;
use tracedecay_lsp::LspSessionRegistry;

use crate::errors::{Result, TraceDecayError};

use super::*;

mod multi_root;
mod query_authority;

/// Daemon-generation-local state for the closed invocation protocol.
///
/// The Unix and portable brokers share this state so an authenticated LSP
/// session remains daemon-owned across client connections until it is detached
/// or expires.
#[derive(Clone)]
pub(super) struct DaemonInvocationState {
    pub(super) lsp_session_registry: Arc<tokio::sync::Mutex<LspSessionRegistry>>,
    pub(super) service: DaemonInvocationService,
    pub(super) github_credential_lifecycle:
        github_credential_lifecycle::DaemonGitHubReadOnlyCredentialLifecycleV1,
    pub(super) code_index_schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    query_authority_provider: query_authority_provider::DaemonQueryAuthorityProviderV1,
    semantic_projection_scheduler:
        crate::application::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1,
}

impl Default for DaemonInvocationState {
    fn default() -> Self {
        let code_index_schedulers =
            code_index_scheduler::CodeIndexSchedulerRegistryV1::new(MAX_CACHED_PROJECT_SERVERS);
        let service =
            DaemonInvocationService::with_code_index_schedulers(code_index_schedulers.clone());
        Self {
            lsp_session_registry: Arc::new(tokio::sync::Mutex::new(
                LspSessionRegistry::default(),
            )),
            service,
            github_credential_lifecycle:
                github_credential_lifecycle::DaemonGitHubReadOnlyCredentialLifecycleV1::default(),
            code_index_schedulers,
            query_authority_provider:
                query_authority_provider::DaemonQueryAuthorityProviderV1::default(),
            semantic_projection_scheduler:
                crate::application::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1::default(),
        }
    }
}

impl DaemonInvocationState {
    pub(super) fn configure_github_read_only_credentials(
        &self,
        identity: &profile_identity::LocalProfileIdentityAuthorityV1,
    ) {
        self.github_credential_lifecycle.configure_profile(identity);
    }

    pub(super) fn mount_github_read_only_credential_authority_for_project(
        &self,
        profile_id: &tracedecay_domain::UserProfileId,
        repository_owner: &str,
        repository_name: &str,
    ) -> crate::application::advisory::github_runtime::ProfileGitHubReadOnlyCredentialMountOutcomeV1
    {
        self.github_credential_lifecycle
            .mount(profile_id, repository_owner, repository_name)
    }

    pub(super) fn advisory_runtime_registrar(&self) -> DaemonAdvisoryRuntimeRegistrar {
        DaemonAdvisoryRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn feedback_runtime_registrar(&self) -> DaemonFeedbackRuntimeRegistrar {
        DaemonFeedbackRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn context_scout_runtime_registrar(&self) -> DaemonContextScoutRuntimeRegistrar {
        DaemonContextScoutRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn primitive_runtime_registrar(&self) -> DaemonPrimitiveRuntimeRegistrar {
        DaemonPrimitiveRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn configuration_runtime_registrar(&self) -> DaemonConfigurationRuntimeRegistrar {
        DaemonConfigurationRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn work_runtime_registrar(&self) -> DaemonWorkRuntimeRegistrar {
        DaemonWorkRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn semantic_runtime_registrar(&self) -> DaemonSemanticRuntimeRegistrar {
        DaemonSemanticRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn lsp_owner_registrar(&self) -> DaemonLspOwnerRegistrar {
        DaemonLspOwnerRegistrar::new(&self.service)
    }

    pub(super) async fn mount_code_index(
        &self,
        project_id: tracedecay_domain::ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_runtime: Option<&crate::semantic_code::DaemonSemanticRuntimeHandleV1>,
        semantic_database: Option<Arc<crate::db::Database>>,
        semantic_lifecycle: Option<Arc<crate::semantic_code::SemanticModelLifecycleOwnerV1>>,
        semantic_resources: Option<crate::config::SemanticResourceCeilings>,
    ) -> Result<()> {
        // Code-index identity is anchored on the project root's own git
        // repository (`IndexingIdentityV1::resolve` uses `gix::open` on the
        // root, no upward discovery). A non-git project has no code-index
        // identity by design: skip mounting instead of failing project open —
        // every non-code-index surface stays available.
        let git_control = project_root.join(".git");
        if !git_control.is_dir() && !git_control.is_file() {
            tracing::warn!(
                event = "code_index_mount",
                outcome = "skipped",
                project = %project_root.display(),
                reason = "missing project-root .git control path",
                "project root is not a git repository; code index disabled"
            );
            return Ok(());
        }
        let canonical_project_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let scoped_code_index_store_root = code_index_scheduler::scoped_code_index_store_root(
            &store_root,
            &canonical_project_root,
        );
        let semantic_schedule = semantic_runtime
            .zip(semantic_database)
            .zip(semantic_lifecycle)
            .zip(semantic_resources)
            .zip(code_index_scheduler::identity::worktree_id_for(project_root).ok())
            .map(
                |((((handle, database), lifecycle), resources), worktree_id)| {
                    crate::application::semantic_runtime::production_saved_generation_schedule_hook(
                        crate::application::semantic_runtime::SavedGenerationScheduleHookParametersV1 {
                            project_root: project_root.to_path_buf(),
                            code_index_store_root: scoped_code_index_store_root.clone(),
                            worktree_id,
                            handle: handle.clone(),
                            database,
                            lifecycle,
                            resources,
                            fair_scheduler: self.semantic_projection_scheduler.clone(),
                        },
                    )
                },
            );
        self.code_index_schedulers
            .mount_worktree(project_id, project_root, store_root, semantic_schedule)
            .await
            .map(|_| ())
            .map_err(|error| {
                // A retryable admission timeout is a busy daemon, not a broken
                // store: say so, so the caller retries instead of reopening.
                if error.is_retryable() {
                    TraceDecayError::Config {
                        message: format!(
                            "code-index scheduler is warming and could not be mounted yet: \
                             {error}"
                        ),
                    }
                } else {
                    TraceDecayError::Config {
                        message: format!("code-index scheduler could not be mounted: {error}"),
                    }
                }
            })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_multi_root_for_project(
        &self,
        store_administration: &StoreAdministration,
        active_project_root: &Path,
        request_id: String,
        request: tracedecay_application::MultiRootExecuteRequestV1,
        observed_at: tracedecay_domain::UtcMicros,
        deadline: tracedecay_application::Deadline,
        cancellation: tracedecay_application::CancellationContext,
    ) -> DaemonInvocationResponse {
        if cancellation.is_cancelled() {
            return DaemonInvocationResponse::with_outcome(
                request_id,
                service::invocation::DaemonInvocationOutcome::ApplicationProblem {
                    problem: tracedecay_application::ApplicationProblem::cancelled_before_admission(
                    ),
                },
            );
        }
        if deadline.is_elapsed_at(observed_at) {
            return DaemonInvocationResponse::with_outcome(
                request_id,
                service::invocation::DaemonInvocationOutcome::ApplicationProblem {
                    problem: tracedecay_application::ApplicationProblem::timed_out_before_admission(
                    ),
                },
            );
        }
        let Some(storage) = multi_root::canonical_scope_set_storage(store_administration).await
        else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        };
        let Some(scope_set) = self
            .service
            .persisted_scope_set(
                active_project_root,
                Some(&storage),
                &request.scope_set_id,
                tracedecay_application::MultiRootApplicationOperation::Execute,
                observed_at,
                &deadline,
                &cancellation,
            )
            .await
        else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if scope_set.revision() != request.scope_set_revision
            || scope_set.digest() != &request.scope_set_digest
        {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        let operation_value = match serde_json::to_value(&request.operation) {
            Ok(value) => value,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        let (capability_id, use_case_id) = match multi_root_operation_authority(&request.operation)
        {
            Ok(authority) => authority,
            Err(problem) => {
                return DaemonInvocationResponse::problem(request_id, problem);
            }
        };
        let Ok(query_digest) = tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon.multi-root-query.v1",
            &operation_value,
        )) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::InvalidRequest,
            );
        };
        let Ok(order_digest) = tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon.multi-root-order.v1",
            scope_set
                .roots()
                .iter()
                .map(|root| &root.scope().scope_digest)
                .collect::<Vec<_>>(),
        )) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::InvalidRequest,
            );
        };
        let database = match store_administration.registered_profile_database().await {
            Ok(database) => database,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            }
        };
        let cursor_key = match database.ensure_active_session_cursor_key_result().await {
            Ok(key) => key,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            }
        };
        let cursor_authenticator = match database.load_session_cursor_key_provider_result().await {
            Ok(authenticator) => authenticator,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            }
        };
        let continuation = match &request.continuation {
            Some(continuation) => {
                match multi_root_continuation::open(
                    continuation,
                    &cursor_authenticator,
                    observed_at,
                ) {
                    Ok(state)
                        if state.scope_set_id == request.scope_set_id
                            && state.scope_set_revision == request.scope_set_revision
                            && state.scope_set_digest == request.scope_set_digest
                            && state.query_digest == query_digest
                            && state.order_digest == order_digest
                            && state.next_page == request.page
                            && state.root_generations.len() == scope_set.roots().len() =>
                    {
                        Some(state)
                    }
                    Ok(_) | Err(_) => {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                        );
                    }
                }
            }
            None if request.page == 0 => None,
            None => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        let mut contexts = Vec::new();
        let mut root_contexts = Vec::with_capacity(scope_set.roots().len());
        let mut generations = Vec::with_capacity(scope_set.roots().len());
        let mut resolved_roots = Vec::with_capacity(scope_set.roots().len());
        for (ordinal, root) in scope_set.roots().iter().enumerate() {
            let scope = root.scope();
            let root = match multi_root::resolve_authorized_root(
                store_administration,
                database.as_ref(),
                root,
            )
            .await
            {
                Ok(root) => root,
                Err(multi_root::AuthorizedRootResolution::Denied) => {
                    let Ok(generation) = denied_root_generation(scope) else {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            service::invocation::DaemonInvocationProblem::InvalidRequest,
                        );
                    };
                    generations.push(generation);
                    root_contexts.push(None);
                    resolved_roots.push(None);
                    continue;
                }
                Err(multi_root::AuthorizedRootResolution::Unavailable(reason)) => {
                    let Ok(generation) = unavailable_root_generation(scope, reason) else {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            service::invocation::DaemonInvocationProblem::InvalidRequest,
                        );
                    };
                    generations.push(generation);
                    root_contexts.push(None);
                    resolved_roots.push(None);
                    continue;
                }
            };
            let Some(context) = self
                .service
                .multi_root_query_context(
                    &root,
                    scope,
                    ordinal,
                    observed_at,
                    &deadline,
                    &cancellation,
                    &capability_id,
                    &use_case_id,
                )
                .await
            else {
                let Ok(generation) = denied_root_generation(scope) else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::InvalidRequest,
                    );
                };
                generations.push(generation);
                root_contexts.push(None);
                resolved_roots.push(None);
                continue;
            };
            contexts.push(context.clone());
            root_contexts.push(Some(context));
            let sealed = continuation
                .as_ref()
                .and_then(|state| state.root_generations.get(ordinal));
            let completed = match multi_root::completed_root_generation(
                continuation.as_ref(),
                ordinal,
                scope,
            ) {
                Ok(completed) => completed,
                Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
            };
            if let Some(completed) = completed {
                generations.push(completed);
                resolved_roots.push(None);
                continue;
            }
            let (generation, latest) = match multi_root::resolve_root_query_generation(
                &self.code_index_schedulers,
                scope,
                sealed,
            )
            .await
            {
                Ok(resolved) => resolved,
                Err(problem) => {
                    return DaemonInvocationResponse::problem(request_id, problem);
                }
            };
            generations.push(generation);
            resolved_roots.push(latest.map(|latest| (root, latest)));
        }
        let authorization =
            match tracedecay_application::MultiRootAuthorizationBindingV1::from_contexts(&contexts)
            {
                Ok(authorization) => authorization,
                Err(_) => {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                    );
                }
            };
        if let Some(state) = &continuation {
            let binding_matches =
                state.root_generations == generations && state.authorization == authorization;
            if !binding_matches {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            }
        }
        let root_cursors = if let Some(state) = &continuation {
            state.root_cursors.clone()
        } else {
            match generations
                .iter()
                .map(|generation| {
                    tracedecay_application::MultiRootRootCursorV1::new(
                        generation.scope_digest.clone(),
                        None,
                    )
                })
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(cursors) => cursors,
                Err(_) => {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::InvalidRequest,
                    );
                }
            }
        };
        if root_cursors.len() != generations.len() {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::InvalidRequest,
            );
        }
        let mut next_root_cursors = match root_cursors
            .iter()
            .map(|cursor| {
                tracedecay_application::MultiRootRootCursorV1::new(
                    cursor.scope_digest.clone(),
                    None,
                )
            })
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(cursors) => cursors,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        if next_root_cursors.len() != generations.len() {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::InvalidRequest,
            );
        }
        let mut outcomes = BTreeMap::new();
        let mut last_order_key = None;
        let mut fused_rank = request.page.saturating_mul(100);
        for (ordinal, ((authorized_root, generation), root)) in scope_set
            .roots()
            .iter()
            .zip(generations.iter())
            .zip(resolved_roots.iter())
            .enumerate()
        {
            let scope = authorized_root.scope();
            if multi_root::cursor_is_complete(&root_cursors[ordinal]) {
                multi_root::terminalize_cursor(&mut next_root_cursors[ordinal]);
                multi_root::insert_completed_root_outcome(&mut outcomes, scope, generation);
                continue;
            }
            let Some((root, pinned_generation)) = root else {
                multi_root::retain_unexecuted_cursor(
                    generation,
                    &root_cursors[ordinal],
                    &mut next_root_cursors[ordinal],
                );
                continue;
            };
            let Some(admitted_context) = &root_contexts[ordinal] else {
                continue;
            };
            if !matches!(
                generation.outcome,
                tracedecay_domain::ScopeOutcome::Exact(_)
                    | tracedecay_domain::ScopeOutcome::Partial { .. }
            ) {
                continue;
            }
            let Some(revalidated_context) = self
                .service
                .multi_root_query_context(
                    root,
                    scope,
                    ordinal,
                    observed_at,
                    &deadline,
                    &cancellation,
                    &capability_id,
                    &use_case_id,
                )
                .await
            else {
                multi_root::terminalize_cursor(&mut next_root_cursors[ordinal]);
                outcomes.insert(
                    scope.scope_digest.clone(),
                    tracedecay_domain::ScopeOutcome::Denied,
                );
                continue;
            };
            if &revalidated_context != admitted_context {
                multi_root::terminalize_cursor(&mut next_root_cursors[ordinal]);
                outcomes.insert(
                    scope.scope_digest.clone(),
                    tracedecay_domain::ScopeOutcome::Denied,
                );
                continue;
            }
            let value = multi_root::execute_one_multi_root_operation(
                self,
                store_administration,
                root,
                scope,
                ordinal,
                &request.operation,
                root_cursors[ordinal].cursor.as_ref(),
                observed_at,
                deadline.clone(),
                cancellation.clone(),
                pinned_generation,
            )
            .await;
            let outcome = match value {
                Ok((value, next_cursor)) => {
                    next_root_cursors[ordinal].cursor =
                        Some(next_cursor.unwrap_or(
                            tracedecay_application::MultiRootRootContinuationV1::Complete,
                        ));
                    let Ok(evidence_digest) = tracedecay_domain::canonical_sha256(&(
                        "tracedecay.multi-root.total-order-key.v1",
                        fused_rank,
                        ordinal,
                        &value,
                    )) else {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            service::invocation::DaemonInvocationProblem::InvalidRequest,
                        );
                    };
                    let Ok(root_ordinal) = u32::try_from(ordinal) else {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            service::invocation::DaemonInvocationProblem::InvalidRequest,
                        );
                    };
                    last_order_key = Some(tracedecay_application::MultiRootTotalOrderKeyV1 {
                        fused_rank,
                        root_ordinal,
                        evidence_digest,
                    });
                    fused_rank = fused_rank.saturating_add(1);
                    tracedecay_domain::ScopeOutcome::Exact(vec![value])
                }
                Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized) => {
                    multi_root::terminalize_cursor(&mut next_root_cursors[ordinal]);
                    tracedecay_domain::ScopeOutcome::Denied
                }
                Err(_) => {
                    next_root_cursors[ordinal] = root_cursors[ordinal].clone();
                    tracedecay_domain::ScopeOutcome::Unavailable {
                        reason: tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
                    }
                }
            };
            outcomes.insert(scope.scope_digest.clone(), outcome);
        }
        let Some(next_page) = request.page.checked_add(1) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::InvalidRequest,
            );
        };
        const MULTI_ROOT_CONTINUATION_TTL_MICROS_V1: i64 = 60 * 1_000_000;
        let expires_at = tracedecay_domain::UtcMicros(
            observed_at
                .0
                .saturating_add(MULTI_ROOT_CONTINUATION_TTL_MICROS_V1)
                .min(authorization.expires_at.0),
        );
        let next_continuation = if next_root_cursors.iter().any(|cursor| {
            matches!(
                cursor.cursor.as_ref(),
                Some(tracedecay_application::MultiRootRootContinuationV1::Page(_))
                    | Some(tracedecay_application::MultiRootRootContinuationV1::Work(_))
            )
        }) {
            let next_state = match tracedecay_application::MultiRootContinuationStateV1::new(
                scope_set.scope_set_id().clone(),
                scope_set.revision(),
                scope_set.digest().clone(),
                generations.clone(),
                next_root_cursors,
                query_digest.clone(),
                order_digest.clone(),
                authorization,
                observed_at,
                expires_at,
                next_page,
                last_order_key,
            ) {
                Ok(state) => state,
                Err(_) => {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::InvalidRequest,
                    );
                }
            };
            match multi_root_continuation::seal(&next_state, &cursor_key, &cursor_authenticator) {
                Ok(continuation) => Some(continuation),
                Err(_) => {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::Unavailable,
                    );
                }
            }
        } else {
            None
        };
        let query = tracedecay_application::MultiRootQueryRequestV1 {
            scope_set,
            contexts,
            root_generations: generations,
            capability_id,
            use_case_id,
            observed_at,
            query: operation_value,
            query_digest,
            order_digest,
            page: request.page,
            continuation,
            next_continuation,
        };
        let page = match self
            .service
            .execute_multi_root_query(PrecomputedMultiRootQueryPort { outcomes }, query)
        {
            Ok(page) => page,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        let Ok(application_request_id) = tracedecay_application::RequestId::new(request_id.clone())
        else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::InvalidRequest,
            );
        };
        let Some((scope, outcome)) = self
            .service
            .multi_root_evidence(
                active_project_root,
                application_request_id,
                "execute",
                page,
                observed_at,
                deadline,
                cancellation,
            )
            .await
        else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        };
        DaemonInvocationResponse::with_outcome(
            request_id,
            service::invocation::DaemonInvocationOutcome::MultiRootQueryPage { scope, outcome },
        )
    }

    pub(super) async fn shutdown(&self) {
        self.service.begin_shutdown().await;
        self.github_credential_lifecycle.shutdown();
        self.code_index_schedulers.shutdown().await;
        self.lsp_session_registry.lock().await.expire_at(u64::MAX);
        self.service.expire_all().await;
    }

    pub(super) async fn invoke_for_project(
        &self,
        store_administration: &StoreAdministration,
        project_path: Option<&Path>,
        pinned_generation: Option<(
            tracedecay_application::ResolvedScope,
            code_index_scheduler::LatestCompleteCodeIndexV1,
        )>,
        request: DaemonInvocationRequest,
    ) -> DaemonInvocationResponse {
        if !matches!(
            &request.payload,
            service::invocation::DaemonInvocationPayload::CallableCode { .. }
        ) && let Some((scope, pinned)) = &pinned_generation
            && multi_root::ensure_pinned_generation(self, scope, pinned)
                .await
                .is_err()
        {
            return DaemonInvocationResponse::problem(
                request.request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        }
        if let Some(response) = invalid_multi_root_invocation_response(&request) {
            return response;
        }
        let request_project_path = request.requires_project().then_some(project_path).flatten();
        if let service::invocation::DaemonInvocationPayload::MultiRootScopeSetRead {
            request: scope_set_request,
            observed_at,
            deadline,
            cancellation,
        } = &request.payload
        {
            let Some(active_project_root) = request_project_path else {
                return DaemonInvocationResponse::problem(
                    request.request_id.clone(),
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            let storage = multi_root::canonical_scope_set_storage(store_administration).await;
            let scope_set = self
                .service
                .persisted_scope_set(
                    active_project_root,
                    storage.as_ref(),
                    &scope_set_request.scope_set_id,
                    tracedecay_application::MultiRootApplicationOperation::ScopeSetRead,
                    *observed_at,
                    deadline,
                    cancellation,
                )
                .await;
            let Ok(application_request_id) =
                tracedecay_application::RequestId::new(request.request_id.clone())
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            };
            let Some((scope, outcome)) = self
                .service
                .multi_root_evidence(
                    active_project_root,
                    application_request_id,
                    "scope_set_read",
                    scope_set,
                    *observed_at,
                    deadline.clone(),
                    cancellation.clone(),
                )
                .await
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            };
            return DaemonInvocationResponse::with_outcome(
                request.request_id,
                service::invocation::DaemonInvocationOutcome::MultiRootScopeSetRead {
                    scope,
                    outcome,
                },
            );
        }
        if let service::invocation::DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap {
            request: scope_set_request,
            observed_at,
            deadline,
            cancellation,
        } = &request.payload
        {
            let Some(active_project_root) = request_project_path else {
                return DaemonInvocationResponse::problem(
                    request.request_id.clone(),
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            if cancellation.is_cancelled() {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::cancelled_before_admission(),
                );
            }
            if deadline.is_elapsed_at(*observed_at) {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::timed_out_before_admission(),
                );
            }
            let roots = match resolve_multi_root_projects(
                store_administration,
                &self.service,
                &scope_set_request.roots,
            )
            .await
            {
                Ok(roots) => roots,
                Err(problem) => {
                    return DaemonInvocationResponse::problem(request.request_id.clone(), problem);
                }
            };
            let Ok(application_request_id) =
                tracedecay_application::RequestId::new(request.request_id.clone())
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            };
            let Some(storage) = multi_root::canonical_scope_set_storage(store_administration).await
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            };
            return match self
                .service
                .compare_and_swap_scope_set(
                    active_project_root,
                    &request.request_id,
                    scope_set_request.clone(),
                    &storage,
                    roots,
                    *observed_at,
                    deadline,
                    cancellation,
                )
                .await
            {
                Some((_scope, result)) => {
                    let Some((scope, outcome)) = self
                        .service
                        .multi_root_evidence(
                            active_project_root,
                            application_request_id,
                            "scope_set_compare_and_swap",
                            result,
                            *observed_at,
                            deadline.clone(),
                            cancellation.clone(),
                        )
                        .await
                    else {
                        return DaemonInvocationResponse::problem(
                            request.request_id,
                            service::invocation::DaemonInvocationProblem::Unavailable,
                        );
                    };
                    DaemonInvocationResponse::with_outcome(
                        request.request_id,
                        service::invocation::DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap {
                            scope,
                            outcome,
                        },
                    )
                }
                None => DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                ),
            };
        }
        if let service::invocation::DaemonInvocationPayload::MultiRootExecute {
            request: execute_request,
            observed_at,
            deadline,
            cancellation,
        } = &request.payload
        {
            let Some(active_project_root) = request_project_path else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            return self
                .execute_multi_root_for_project(
                    store_administration,
                    active_project_root,
                    request.request_id,
                    execute_request.clone(),
                    *observed_at,
                    deadline.clone(),
                    cancellation.clone(),
                )
                .await;
        }
        let lsp_workspace =
            if request.operation() == service::invocation::DaemonInvocationOperation::LspOpen {
                match request_project_path {
                    Some(project_path) => {
                        admitted_lsp_workspace_for_request(
                            store_administration,
                            &self.service,
                            project_path,
                            &request,
                        )
                        .await
                    }
                    None => None,
                }
            } else {
                None
            };
        let git_service = if invocation_is_git_operation(request.operation()) {
            git_service_for_project_path(store_administration, request_project_path).await
        } else {
            None
        };
        self.service
            .invoke_with_query_generation(
                &self.lsp_session_registry,
                request_project_path,
                lsp_workspace,
                git_service,
                pinned_generation,
                request,
            )
            .await
    }
}
