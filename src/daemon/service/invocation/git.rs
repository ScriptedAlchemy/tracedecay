//! Git index transaction daemon invocation handlers (`execute_git_read`/`execute_git_preview`/`execute_git_apply`).

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn git_read_evidence_packet(
    request_id: &str,
    request: &crate::application::git_reads::GitReadRequestV1,
    current: &DaemonGitAuthorityStateV1,
    result: crate::application::git_reads::GitReadResultV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<EvidencePacket<serde_json::Value>, ApplicationProblem> {
    let capability_id =
        CapabilityId::new(request.capability_id()).map_err(|_| invalid_git_request())?;
    let use_case_id = UseCaseId::new(request.use_case_id()).map_err(|_| invalid_git_request())?;
    if !current.effective_capabilities.contains(&capability_id) {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let grant_digest = stable_digest(&(
        &current.scope,
        &current.requester,
        &current.policy_digest,
        &current.configuration_digest,
        &current.catalog_digest,
        &current.privacy_digest,
        &capability_id,
        &use_case_id,
    ))?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.daemon.git-read.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(|_| invalid_git_request())?,
        current.policy_revision,
        grant_digest.clone(),
        current.requester.clone(),
        current.evaluated_at,
        current.grant_expires_at,
        current.scope.clone(),
        std::collections::BTreeSet::from([capability_id]),
        std::collections::BTreeSet::from([use_case_id]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid_git_request())?;
    let context = RequestContext::new(
        current.requester.clone(),
        current.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid_git_request())?,
        deadline.clone(),
        cancellation,
    )
    .map_err(|_| invalid_git_request())?;
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.git-read.v1",
            current.policy_revision,
            current.policy_digest.clone(),
            ComponentVersion::new("tracedecay.daemon.git-policy.v2")
                .map_err(|_| invalid_git_request())?,
        )
        .map_err(|_| invalid_git_request())?,
        current.evaluated_at,
    )
    .map_err(|_| invalid_git_request())?;
    let native_coverage = match &result {
        crate::application::git_reads::GitReadResultV1::Status(envelope) => &envelope.coverage,
        crate::application::git_reads::GitReadResultV1::Diff(envelope) => &envelope.coverage,
        crate::application::git_reads::GitReadResultV1::History(envelope) => &envelope.coverage,
        crate::application::git_reads::GitReadResultV1::Blame(envelope) => &envelope.coverage,
        crate::application::git_reads::GitReadResultV1::Hunks(envelope) => &envelope.coverage,
    };
    let coverage = if native_coverage.is_complete() {
        EvidenceCoverage::complete(vec![EvidenceDomain::Source], 1, 1, 1)
            .map_err(|_| invalid_git_request())?
    } else {
        let coverage = EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Source],
            visited: Some(1),
            eligible: Some(1),
            returned: 1,
            completeness: CoverageCompleteness::Partial,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Source,
                completeness: CoverageCompleteness::Partial,
            }],
        };
        coverage.validate().map_err(|_| invalid_git_request())?;
        coverage
    };
    let mut omission_counts = BTreeMap::<OmissionReason, u64>::new();
    for degradation in &native_coverage.degradations {
        use tracedecay_domain::git::GitDegradationV1;
        let reason = match degradation {
            GitDegradationV1::TruncatedOutput => OmissionReason::Budget,
            GitDegradationV1::ConflictedState | GitDegradationV1::InProgressOperation => {
                OmissionReason::Conflict
            }
            GitDegradationV1::UnreadableState => OmissionReason::Failed,
            GitDegradationV1::IgnoredCollision
            | GitDegradationV1::DetachedHead
            | GitDegradationV1::UnbornBranch
            | GitDegradationV1::SparseCheckout
            | GitDegradationV1::SplitIndex
            | GitDegradationV1::SubmoduleState
            | GitDegradationV1::UnsupportedObjectFormat
            | GitDegradationV1::ShallowBoundary => OmissionReason::Unsupported,
        };
        omission_counts
            .entry(reason)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
    }
    let omissions = omission_counts
        .into_iter()
        .map(|(reason, count)| Omission {
            domain: EvidenceDomain::Source,
            count,
            reason,
        })
        .collect();
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(|_| invalid_git_request())?;
    let payload = serde_json::to_value(result).map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "git_read.result_encoding_failed".to_owned(),
            message: "The Git read result could not be encoded".to_owned(),
        })
    })?;
    let evidence_digest = stable_digest(&(
        "tracedecay.native-git-read-evidence.v1",
        request,
        &current.scope,
        &current.configuration_digest,
        &current.catalog_digest,
        &payload,
    ))?;
    Ok(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: vec![EvidenceAuthority {
            evidence_id: EvidenceIdentity::new(format!(
                "evidence.git-read.{}",
                evidence_digest.as_str().trim_start_matches("sha256:")
            ))
            .map_err(|_| invalid_git_request())?,
            source_kind: "native_git".to_owned(),
            producer: "git_query".to_owned(),
            scope: current.scope.clone(),
            revision: ComponentVersion::new("tracedecay.git-read.v1")
                .map_err(|_| invalid_git_request())?,
            horizon: Some(current.evaluated_at),
        }],
        coverage,
        omissions,
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.git-read.stable.v1").map_err(|_| invalid_git_request())?,
            1,
            Some(1),
            1,
        )
        .map_err(|_| invalid_git_request())?,
        execution,
        payload: Some(payload),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_git_read(
    wire_request_id: String,
    project_root: Option<&Path>,
    owner: Option<DaemonGitInvocationOwner>,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: GitReadSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return concealed_application_problem(wire_request_id);
    };
    let Some(project_root) = project_root.map(Path::to_path_buf) else {
        return concealed_application_problem(wire_request_id);
    };
    let expected_operation = match &request.request {
        crate::application::git_reads::GitReadRequestV1::Status => {
            crate::application_surface::ApplicationSurfaceOperation::GitStatus
        }
        crate::application::git_reads::GitReadRequestV1::Diff { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitDiff
        }
        crate::application::git_reads::GitReadRequestV1::History { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitHistory
        }
        crate::application::git_reads::GitReadRequestV1::Blame { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitBlame
        }
        crate::application::git_reads::GitReadRequestV1::Hunks { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitHunks
        }
    };
    if surface_operation != expected_operation {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    }
    if cancellation.is_cancelled() {
        return application_problem(
            wire_request_id,
            ApplicationProblem::cancelled_before_admission(),
        );
    }
    if deadline.is_elapsed_at(observed_at) || deadline.is_elapsed_at(current_micros()) {
        return application_problem(
            wire_request_id,
            ApplicationProblem::timed_out_before_admission(),
        );
    }
    let initial = match owner.current_read_authority(&request.request) {
        Ok(authority) => authority,
        Err(_) => return concealed_application_problem(wire_request_id),
    };
    let remaining_micros = deadline
        .expires_at
        .0
        .saturating_sub(current_micros().0)
        .max(0) as u64;
    let bounds = crate::git_query::GitQueryBounds {
        max_entries: request.max_entries,
        max_bytes: request.max_bytes,
        deadline: Some(std::time::Instant::now() + Duration::from_micros(remaining_micros)),
        cancel: Some(Arc::new(AtomicBool::new(false))),
    };
    let selected_scope = initial.scope.clone();
    let read_request = request.request.clone();
    let authority = crate::application::git_reads::GitReadAuthorityV1::new(
        project_root,
        selected_scope.clone(),
    );
    let outcome = tokio::task::spawn_blocking(move || {
        crate::application::git_reads::execute_git_read(
            Some(&authority),
            &selected_scope,
            &read_request,
            &bounds,
        )
    })
    .await
    .unwrap_or(
        crate::application::git_reads::GitReadOutcomeV1::Unavailable {
            reason: crate::application::git_reads::GitReadUnavailableReasonV1::ReadFailed,
        },
    );
    let terminal = match owner.current_read_authority(&request.request) {
        Ok(authority) => authority,
        Err(_) => return concealed_application_problem(wire_request_id),
    };
    if initial.scope != terminal.scope
        || initial.requester != terminal.requester
        || initial.effective_capabilities != terminal.effective_capabilities
        || initial.grant_expires_at != terminal.grant_expires_at
        || initial.policy_revision != terminal.policy_revision
        || initial.policy_digest != terminal.policy_digest
        || initial.configuration_digest != terminal.configuration_digest
        || initial.catalog_digest != terminal.catalog_digest
        || initial.privacy_digest != terminal.privacy_digest
        || current_micros() >= terminal.grant_expires_at
    {
        return concealed_application_problem(wire_request_id);
    }
    match outcome {
        crate::application::git_reads::GitReadOutcomeV1::Complete { scope, result }
            if scope == terminal.scope =>
        {
            let packet = match git_read_evidence_packet(
                &wire_request_id,
                &request.request,
                &terminal,
                result,
                observed_at,
                deadline,
                cancellation,
            ) {
                Ok(packet) => packet,
                Err(problem) => return application_problem(wire_request_id, problem),
            };
            DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::GitRead {
                    scope,
                    result: DaemonFeedbackResult::from_application(packet),
                },
            )
        }
        crate::application::git_reads::GitReadOutcomeV1::Unavailable {
            reason: crate::application::git_reads::GitReadUnavailableReasonV1::Cancelled,
        } => application_problem(
            wire_request_id,
            ApplicationProblem::cancelled_before_admission(),
        ),
        crate::application::git_reads::GitReadOutcomeV1::Unavailable {
            reason: crate::application::git_reads::GitReadUnavailableReasonV1::TimedOut,
        } => application_problem(
            wire_request_id,
            ApplicationProblem::timed_out_before_admission(),
        ),
        crate::application::git_reads::GitReadOutcomeV1::Complete { .. }
        | crate::application::git_reads::GitReadOutcomeV1::Unavailable { .. } => {
            DaemonInvocationResponse::problem(wire_request_id, DaemonInvocationProblem::Unavailable)
        }
    }
}

pub(super) async fn execute_git_preview(
    operation_events: &OperationEventAuthority,
    wire_request_id: String,
    owner: Option<DaemonGitInvocationOwner>,
    request: GitPreviewSurfaceRequest,
    _observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return concealed_application_problem(wire_request_id);
    };
    if request.repository_snapshot.project_id != owner.project_id {
        return concealed_application_problem(wire_request_id);
    }
    let service = Arc::clone(&owner.service);
    let operation = request.operation;
    let authority =
        match tokio::task::spawn_blocking(move || owner.current_authority(operation)).await {
            Ok(Ok(authority)) => authority,
            Ok(Err(error)) => {
                return application_problem(wire_request_id, map_git_port_problem(error));
            }
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    wire_request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        };
    let request = match build_git_preview_request(
        &wire_request_id,
        request,
        &authority,
        deadline,
        cancellation,
    ) {
        Ok(request) => request,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let scope = request.context.scope().clone();
    let Ok(emitter) = operation_events
        .begin(
            &request.context,
            OperationKind::GitPreview,
            request.observed_at,
        )
        .await
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    let _ = emitter.progress(0, Some(1)).await;
    let started_at = request.observed_at;
    let effective_deadline = request.context.deadline().clone();
    let result = tokio::task::spawn_blocking(move || {
        GitIndexTransactionService::new(SharedGitTransactionPort {
            service,
            cancellation: None,
        })
        .preview(request)
    })
    .await;
    let response = match result {
        Ok(Ok(preview)) => match DaemonGitPreviewResult::from_application(preview) {
            Ok(preview) => DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::GitPreview { scope, preview },
            ),
            Err(_) => DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            ),
        },
        Ok(Err(error)) => application_problem(wire_request_id, map_git_error(error)),
        Err(_) => {
            DaemonInvocationResponse::problem(wire_request_id, DaemonInvocationProblem::Unavailable)
        }
    };
    publish_invocation_terminal(&emitter, &response, started_at, effective_deadline).await;
    response
}

pub(super) async fn execute_git_apply(
    operation_events: &OperationEventAuthority,
    wire_request_id: String,
    owner: Option<DaemonGitInvocationOwner>,
    request: GitApplySurfaceRequest,
    _observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return concealed_application_problem(wire_request_id);
    };
    if request.preview.repository_snapshot.project_id != owner.project_id {
        return concealed_application_problem(wire_request_id);
    }
    let service = Arc::clone(&owner.service);
    let operation = request.preview.operation;
    let authority =
        match tokio::task::spawn_blocking(move || owner.current_authority(operation)).await {
            Ok(Ok(authority)) => authority,
            Ok(Err(error)) => {
                return application_problem(wire_request_id, map_git_port_problem(error));
            }
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    wire_request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        };
    let request = match build_git_apply_request(
        &wire_request_id,
        request,
        &authority,
        deadline,
        cancellation,
    ) {
        Ok(request) => request,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let scope = request.context.scope().clone();
    let Ok(emitter) = operation_events
        .begin(
            &request.context,
            OperationKind::GitApply,
            request.observed_at,
        )
        .await
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    let _ = emitter.progress(0, Some(1)).await;
    let started_at = request.observed_at;
    let effective_deadline = request.context.deadline().clone();
    let cancellation = emitter.clone();
    let result = tokio::task::spawn_blocking(move || {
        GitIndexTransactionService::new(SharedGitTransactionPort {
            service,
            cancellation: Some(cancellation),
        })
        .apply(request)
    })
    .await;
    let response = match result {
        Ok(Ok(effect)) => match DaemonGitEffectResult::from_application(effect) {
            Ok(effect) => DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::GitApply { scope, effect },
            ),
            Err(_) => DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            ),
        },
        Ok(Err(error)) => application_problem(wire_request_id, map_git_error(error)),
        Err(_) => {
            DaemonInvocationResponse::problem(wire_request_id, DaemonInvocationProblem::Unavailable)
        }
    };
    publish_invocation_terminal(&emitter, &response, started_at, effective_deadline).await;
    response
}

async fn publish_invocation_terminal(
    emitter: &OperationEmitter,
    response: &DaemonInvocationResponse,
    started_at: UtcMicros,
    effective_deadline: Deadline,
) {
    let ended_at = current_micros();
    let ended_at = if ended_at < started_at {
        started_at
    } else {
        ended_at
    };
    let receipt = invocation_operation_receipt(response).unwrap_or_else(|| OperationReceipt {
        started_at,
        ended_at,
        effective_deadline,
        cancellation: None,
        budget: OperationBudgetUsage::default(),
        termination: OperationTermination::Failed,
    });
    if receipt.termination == OperationTermination::Completed {
        let _ = emitter.progress(1, Some(1)).await;
    }
    let _ = emitter.terminal(receipt).await;
}

fn invocation_operation_receipt(response: &DaemonInvocationResponse) -> Option<OperationReceipt> {
    match &response.outcome {
        DaemonInvocationOutcome::GitRead { result, .. } => Some(result.execution().clone()),
        DaemonInvocationOutcome::GitPreview { preview, .. } => Some(preview.execution().clone()),
        DaemonInvocationOutcome::GitApply { effect, .. } => Some(effect.execution().clone()),
        DaemonInvocationOutcome::Feedback { result, .. } => Some(result.execution().clone()),
        _ => None,
    }
}

fn build_git_preview_request(
    request_id: &str,
    request: GitPreviewSurfaceRequest,
    current: &DaemonGitAuthorityStateV1,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<GitIndexPreviewRequestV1, ApplicationProblem> {
    let observed_at = current.evaluated_at;
    let preview_id = mint_git_preview_id()?;
    let mut selected_hunks = request.selected_hunks;
    for hunk in &mut selected_hunks {
        preview_id.as_str().clone_into(&mut hunk.preview_id);
    }
    let (context, authority, binding) = git_request_authority(
        request_id,
        &request.repository_snapshot,
        request.operation,
        current,
        deadline,
        cancellation,
        observed_at,
    )?;
    Ok(GitIndexPreviewRequestV1 {
        context,
        authority,
        binding,
        preview_id,
        repository_snapshot: request.repository_snapshot,
        selected_hunks,
        commit_intent: request.commit_intent,
        observed_at,
    })
}

fn mint_git_preview_id() -> Result<GitIndexPreviewId, ApplicationProblem> {
    let identity =
        mint_global_opaque_id(GlobalOpaqueIdentityKind::GitIndexPreview).map_err(|_| {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "git_index.preview_identity_unavailable".to_owned(),
                message: "The daemon could not mint a Git preview identity".to_owned(),
            })
        })?;
    GitIndexPreviewId::new(identity).map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "git_index.preview_identity_unavailable".to_owned(),
            message: "The daemon could not mint a Git preview identity".to_owned(),
        })
    })
}

fn build_git_apply_request(
    request_id: &str,
    request: GitApplySurfaceRequest,
    current: &DaemonGitAuthorityStateV1,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<GitIndexApplyRequestV1, ApplicationProblem> {
    let observed_at = current.evaluated_at;
    let (context, authority, binding) = git_request_authority(
        request_id,
        &request.preview.repository_snapshot,
        request.preview.operation,
        current,
        deadline,
        cancellation,
        observed_at,
    )?;
    Ok(GitIndexApplyRequestV1 {
        context,
        authority: authority.clone(),
        binding,
        preview_id: request.preview.preview_id,
        preview_digest: request.preview.preview_digest,
        idempotency_key: request.idempotency_key,
        proof: GitIndexEffectProofV1 {
            policy_digest: authority.policy.digest,
            configuration_digest: current.configuration_digest.clone(),
            catalog_digest: current.catalog_digest.clone(),
            privacy_digest: current.privacy_digest.clone(),
            external_proof: None,
        },
        observed_at,
    })
}

fn git_request_authority(
    request_id: &str,
    snapshot: &tracedecay_domain::RepositoryStateSnapshotV1,
    operation: GitIndexTransactionOperationV1,
    current: &DaemonGitAuthorityStateV1,
    deadline: Deadline,
    cancellation: CancellationContext,
    observed_at: UtcMicros,
) -> Result<(RequestContext, AuthorityReceipt, GitIndexOperationBindingV1), ApplicationProblem> {
    if cancellation.is_cancelled() {
        return Err(ApplicationProblem::cancelled_before_admission());
    }
    if deadline.is_elapsed_at(now_micros()) || deadline.is_elapsed_at(observed_at) {
        return Err(ApplicationProblem::timed_out_before_admission());
    }
    snapshot.validate().map_err(|_| invalid_git_request())?;
    if observed_at >= current.grant_expires_at
        || current.evaluated_at >= current.grant_expires_at
        || snapshot.project_id != current.scope.project_id
        || snapshot.repository_id != current.scope.repository_id
        || snapshot.worktree_id.as_ref() != Some(&current.scope.worktree_id)
        || !match (&current.scope.reference, &snapshot.head) {
            (
                Some(reference),
                GitHeadStateV1::Attached { branch, .. } | GitHeadStateV1::Unborn { branch },
            ) => reference.as_str() == branch,
            (None, GitHeadStateV1::Detached { .. }) => true,
            (None, _) | (Some(_), GitHeadStateV1::Detached { .. }) => false,
        }
    {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let binding =
        GitIndexOperationBindingV1::for_operation(operation).map_err(|_| invalid_git_request())?;
    let capability_id = binding.capability_id.clone();
    let use_case_id = binding.use_case_id.clone();
    if !current.effective_capabilities.contains(&capability_id) {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let grant_digest = stable_digest(&(
        &current.scope,
        &current.requester,
        &current.policy_digest,
        &current.configuration_digest,
        &current.catalog_digest,
        &current.privacy_digest,
        &capability_id,
        &use_case_id,
    ))?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.daemon.git.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(|_| invalid_git_request())?,
        current.policy_revision,
        grant_digest,
        current.requester.clone(),
        observed_at,
        current.grant_expires_at,
        current.scope.clone(),
        std::collections::BTreeSet::from([capability_id.clone()]),
        std::collections::BTreeSet::from([use_case_id.clone()]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid_git_request())?;
    let context = RequestContext::new(
        current.requester.clone(),
        current.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid_git_request())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid_git_request())?;
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.git-index.v2",
            current.policy_revision,
            current.policy_digest.clone(),
            ComponentVersion::new("tracedecay.daemon.git-policy.v2")
                .map_err(|_| invalid_git_request())?,
        )
        .map_err(|_| invalid_git_request())?,
        current.evaluated_at,
    )
    .map_err(|_| invalid_git_request())?;
    Ok((context, authority, binding))
}

pub(super) fn stable_digest(
    material: &impl Serialize,
) -> Result<ManifestDigest, ApplicationProblem> {
    canonical_sha256(material).map_err(|_| invalid_git_request())
}

fn invalid_git_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "git_index.invalid_request".to_owned(),
            message: "The Git index request is invalid".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}

fn map_git_error(error: GitIndexTransactionApplicationError) -> ApplicationProblem {
    match error {
        GitIndexTransactionApplicationError::Contract(_) => invalid_git_request(),
        GitIndexTransactionApplicationError::Port(error) => map_git_port_problem(error),
    }
}

fn map_git_port_problem(error: GitIndexTransactionPortError) -> ApplicationProblem {
    match error {
        GitIndexTransactionPortError::StalePreview => ApplicationProblem::stale(SafeDiagnostic {
            code: "git_index.stale_preview".to_owned(),
            message: "The Git index preview is stale or absent".to_owned(),
        }),
        GitIndexTransactionPortError::PolicyDenied => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        GitIndexTransactionPortError::IdempotencyConflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "git_index.idempotency_conflict".to_owned(),
                message: "The idempotency key is already bound to another input".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
        GitIndexTransactionPortError::Unsupported => ApplicationProblem::Unsupported {
            diagnostic: SafeDiagnostic {
                code: "git_index.unsupported".to_owned(),
                message: "The repository state does not support this Git index operation"
                    .to_owned(),
            },
            retry: RetryDirective::AfterRevalidate,
            legal_actions: Vec::new(),
        },
        GitIndexTransactionPortError::DaemonUnavailable
        | GitIndexTransactionPortError::RecoveryRequired
        | GitIndexTransactionPortError::NeedsInspection
        | GitIndexTransactionPortError::NativeFailure => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: match error {
                    GitIndexTransactionPortError::RecoveryRequired => "git_index.recovery_required",
                    GitIndexTransactionPortError::NeedsInspection => "git_index.needs_inspection",
                    GitIndexTransactionPortError::NativeFailure => "git_index.native_failure",
                    _ => "git_index.unavailable",
                }
                .to_owned(),
                message: "The Git index transaction owner is not ready".to_owned(),
            })
        }
    }
}
