//! Git index transaction daemon invocation handlers (`execute_git_read`/`execute_git_preview`/`execute_git_apply`).

use super::*;
use tracedecay_domain::GitIndexPreviewV1;

#[allow(clippy::too_many_arguments)]
pub(super) fn git_read_evidence_packet(
    request_id: &str,
    request: &tracedecay_usecases::git_reads::GitReadRequestV1,
    current: &DaemonGitAuthorityStateV1,
    result: tracedecay_usecases::git_reads::GitReadResultV1,
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
        tracedecay_usecases::git_reads::GitReadResultV1::Status(envelope) => &envelope.coverage,
        tracedecay_usecases::git_reads::GitReadResultV1::Diff(envelope) => &envelope.coverage,
        tracedecay_usecases::git_reads::GitReadResultV1::History(envelope) => &envelope.coverage,
        tracedecay_usecases::git_reads::GitReadResultV1::Blame(envelope) => &envelope.coverage,
        tracedecay_usecases::git_reads::GitReadResultV1::Hunks(envelope) => &envelope.coverage,
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
    request_cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        // The route reaching here already passed project resolution and an
        // admitted project open; the git transaction owner registers behind
        // the core publication, so a miss here is a retryable mounting state.
        return runtime_mounting_problem(wire_request_id);
    };
    let Some(project_root) = project_root.map(Path::to_path_buf) else {
        return concealed_application_problem(wire_request_id);
    };
    let expected_operation = match &request.request {
        tracedecay_usecases::git_reads::GitReadRequestV1::Status => {
            crate::application_surface::ApplicationSurfaceOperation::GitStatus
        }
        tracedecay_usecases::git_reads::GitReadRequestV1::Diff { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitDiff
        }
        tracedecay_usecases::git_reads::GitReadRequestV1::History { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitHistory
        }
        tracedecay_usecases::git_reads::GitReadRequestV1::Blame { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitBlame
        }
        tracedecay_usecases::git_reads::GitReadRequestV1::Hunks { .. } => {
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
        max_entries: if matches!(
            &request.request,
            tracedecay_usecases::git_reads::GitReadRequestV1::Hunks { .. }
        ) {
            request
                .max_entries
                .min(tracedecay_domain::MAX_GIT_INDEX_PREVIEW_INPUT_HUNKS as u32)
        } else {
            request.max_entries
        },
        max_bytes: request.max_bytes,
        deadline: Some(std::time::Instant::now() + Duration::from_micros(remaining_micros)),
        cancel: Some(request_cancellation),
    };
    let selected_scope = initial.scope.clone();
    let mut read_request = request.request.clone();
    let hunk_capture = if let tracedecay_usecases::git_reads::GitReadRequestV1::Hunks {
        scope,
        daemon_binding,
    } = &mut read_request
    {
        let operation = match scope {
            tracedecay_domain::GitDiffScopeV1::WorkingTree => {
                GitIndexTransactionOperationV1::StageHunks
            }
            tracedecay_domain::GitDiffScopeV1::Staged => {
                GitIndexTransactionOperationV1::UnstageHunks
            }
            tracedecay_domain::GitDiffScopeV1::CommitRange { .. } => {
                return application_problem(wire_request_id, invalid_git_request());
            }
        };
        let preview_id = match mint_git_preview_id() {
            Ok(preview_id) => preview_id,
            Err(problem) => return application_problem(wire_request_id, problem),
        };
        let input_created_at = initial.evaluated_at;
        let expires_at = UtcMicros(input_created_at.0.saturating_add(30_000_000));
        let root = project_root.clone();
        let project_id = selected_scope.project_id.clone();
        let repository_id = selected_scope.repository_id.clone();
        let worktree_id = selected_scope.worktree_id.clone();
        let snapshot = match tokio::task::spawn_blocking(move || {
            capture_exact_snapshot(
                &root,
                project_id,
                repository_id,
                worktree_id,
                input_created_at,
            )
        })
        .await
        {
            Ok(Ok(snapshot)) => snapshot,
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
        if cancellation.is_cancelled() {
            return application_problem(
                wire_request_id,
                ApplicationProblem::cancelled_before_admission(),
            );
        }
        if deadline.is_elapsed_at(current_micros()) {
            return application_problem(
                wire_request_id,
                ApplicationProblem::timed_out_before_admission(),
            );
        }
        let snapshot_digest = match GitIndexPreviewV1::repository_snapshot_digest(&snapshot) {
            Ok(digest) => digest,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    wire_request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        };
        *daemon_binding = Some(
            tracedecay_usecases::git_reads::DaemonGitHunkPreviewBindingV1 {
                preview_id,
                snapshot_digest,
                expires_at,
            },
        );
        Some((operation, snapshot))
    } else {
        None
    };
    let authority = tracedecay_usecases::git_reads::GitReadAuthorityV1::new(
        project_root,
        selected_scope.clone(),
    );
    let outcome = tokio::task::spawn_blocking(move || {
        tracedecay_usecases::git_reads::execute_git_read(
            Some(&authority),
            &selected_scope,
            &read_request,
            &bounds,
        )
    })
    .await
    .unwrap_or(
        tracedecay_usecases::git_reads::GitReadOutcomeV1::Unavailable {
            reason: tracedecay_usecases::git_reads::GitReadUnavailableReasonV1::ReadFailed,
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
        tracedecay_usecases::git_reads::GitReadOutcomeV1::Complete { scope, mut result }
            if scope == terminal.scope =>
        {
            if let tracedecay_usecases::git_reads::GitReadResultV1::Hunks(envelope) = &mut result {
                for entry in &envelope.value.hunks {
                    if entry.hunk.verify_digest(&entry.digest).is_err() {
                        return DaemonInvocationResponse::problem(
                            wire_request_id,
                            DaemonInvocationProblem::Unavailable,
                        );
                    }
                }
                envelope
                    .value
                    .hunks
                    .sort_by(|left, right| left.digest.cmp(&right.digest));
            }
            if let (
                Some((operation, snapshot)),
                tracedecay_usecases::git_reads::GitReadResultV1::Hunks(envelope),
            ) = (&hunk_capture, &result)
            {
                let input = match GitIndexPreviewInputV1::new_hunk_selection(
                    envelope.value.preview_input_id.clone(),
                    *operation,
                    snapshot.clone(),
                    envelope
                        .value
                        .hunks
                        .iter()
                        .map(|entry| entry.hunk.clone())
                        .collect(),
                    initial.evaluated_at,
                    envelope.value.expires_at,
                ) {
                    Ok(input) => input,
                    Err(_) => {
                        return DaemonInvocationResponse::problem(
                            wire_request_id,
                            DaemonInvocationProblem::Unavailable,
                        );
                    }
                };
                let service = Arc::clone(&owner.service);
                match tokio::task::spawn_blocking(move || service.save_preview_input(input)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        return application_problem(wire_request_id, map_git_port_problem(error));
                    }
                    Err(_) => {
                        return DaemonInvocationResponse::problem(
                            wire_request_id,
                            DaemonInvocationProblem::Unavailable,
                        );
                    }
                }
            }
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
        tracedecay_usecases::git_reads::GitReadOutcomeV1::Unavailable {
            reason: tracedecay_usecases::git_reads::GitReadUnavailableReasonV1::Cancelled,
        } => application_problem(
            wire_request_id,
            ApplicationProblem::cancelled_before_admission(),
        ),
        tracedecay_usecases::git_reads::GitReadOutcomeV1::Unavailable {
            reason: tracedecay_usecases::git_reads::GitReadUnavailableReasonV1::TimedOut,
        } => application_problem(
            wire_request_id,
            ApplicationProblem::timed_out_before_admission(),
        ),
        tracedecay_usecases::git_reads::GitReadOutcomeV1::Unavailable {
            reason: tracedecay_usecases::git_reads::GitReadUnavailableReasonV1::OutputLimitExceeded,
        } => application_problem(wire_request_id, git_read_output_limit_problem()),
        tracedecay_usecases::git_reads::GitReadOutcomeV1::Complete { .. }
        | tracedecay_usecases::git_reads::GitReadOutcomeV1::Unavailable { .. } => {
            DaemonInvocationResponse::problem(wire_request_id, DaemonInvocationProblem::Unavailable)
        }
    }
}

pub(super) fn git_read_output_limit_problem() -> ApplicationProblem {
    ApplicationProblem::Saturated {
        diagnostic: SafeDiagnostic {
            code: "git_read.output_limit_exceeded".to_owned(),
            message: "The Git read exceeded its output byte limit".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
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
        return runtime_mounting_problem(wire_request_id);
    };
    // Snapshot, preview input, and the built request are too large to live in
    // this async state machine: constructing that future on the socket poll
    // stack overflows. Keep every payload on a blocking thread or the heap.
    let join_id = wire_request_id.clone();
    let prepared = match tokio::task::spawn_blocking(move || {
        prepare_git_preview(wire_request_id, owner, request, deadline, cancellation)
    })
    .await
    {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(response)) => return response,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                join_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    Box::pin(settle_prepared_git_preview(operation_events, prepared)).await
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
        return runtime_mounting_problem(wire_request_id);
    };
    let join_id = wire_request_id.clone();
    let prepared = match tokio::task::spawn_blocking(move || {
        prepare_git_apply(wire_request_id, owner, request, deadline, cancellation)
    })
    .await
    {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(response)) => return response,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                join_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    Box::pin(settle_prepared_git_apply(operation_events, prepared)).await
}

struct PreparedGitPreview {
    wire_request_id: String,
    service: Arc<DaemonProjectGitIndexTransactionService>,
    request: GitIndexPreviewRequestV1,
}

struct PreparedGitApply {
    wire_request_id: String,
    service: Arc<DaemonProjectGitIndexTransactionService>,
    request: GitIndexApplyRequestV1,
}

fn prepare_git_preview(
    wire_request_id: String,
    owner: DaemonGitInvocationOwner,
    request: GitPreviewSurfaceRequest,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<Box<PreparedGitPreview>, DaemonInvocationResponse> {
    let service = Arc::clone(&owner.service);
    let operation = request.operation;
    let authority = owner.current_authority(operation).map_err(|error| {
        application_problem(wire_request_id.clone(), map_git_port_problem(error))
    })?;
    let preview_input = match operation {
        GitIndexTransactionOperationV1::CommitIndex => {
            let Some(commit_intent) = request.commit_intent.clone() else {
                return Err(application_problem(wire_request_id, invalid_git_request()));
            };
            if request.preview_input_id.is_some() || !request.selected_hunk_digests.is_empty() {
                return Err(application_problem(wire_request_id, invalid_git_request()));
            }
            let preview_id = mint_git_preview_id()
                .map_err(|problem| application_problem(wire_request_id.clone(), problem))?;
            let snapshot = capture_exact_snapshot(
                &owner.repository_root,
                authority.scope.project_id.clone(),
                authority.scope.repository_id.clone(),
                authority.scope.worktree_id.clone(),
                authority.evaluated_at,
            )
            .map_err(|error| {
                application_problem(wire_request_id.clone(), map_git_port_problem(error))
            })?;
            let input = GitIndexPreviewInputV1::new_commit(
                preview_id,
                snapshot,
                commit_intent,
                authority.evaluated_at,
                UtcMicros(authority.evaluated_at.0.saturating_add(30_000_000)),
            )
            .map_err(|_| application_problem(wire_request_id.clone(), invalid_git_request()))?;
            service.save_preview_input(input.clone()).map_err(|error| {
                application_problem(wire_request_id.clone(), map_git_port_problem(error))
            })?;
            input
        }
        GitIndexTransactionOperationV1::StageHunks
        | GitIndexTransactionOperationV1::UnstageHunks => {
            if request.commit_intent.is_some() || request.selected_hunk_digests.is_empty() {
                return Err(application_problem(wire_request_id, invalid_git_request()));
            }
            let Some(preview_input_id) = request.preview_input_id.clone() else {
                return Err(application_problem(wire_request_id, invalid_git_request()));
            };
            match service.read_preview_input(&preview_input_id, authority.evaluated_at) {
                Ok(input) if input.operation == operation => input,
                Ok(_) => return Err(concealed_application_problem(wire_request_id)),
                Err(error) => {
                    return Err(application_problem(
                        wire_request_id,
                        map_git_port_problem(error),
                    ));
                }
            }
        }
    };
    if preview_input.repository_snapshot.project_id != owner.project_id {
        return Err(concealed_application_problem(wire_request_id));
    }
    let request = build_git_preview_request(
        &wire_request_id,
        request,
        preview_input,
        &authority,
        deadline,
        cancellation,
    )
    .map_err(|problem| application_problem(wire_request_id.clone(), problem))?;
    Ok(Box::new(PreparedGitPreview {
        wire_request_id,
        service,
        request,
    }))
}

fn prepare_git_apply(
    wire_request_id: String,
    owner: DaemonGitInvocationOwner,
    request: GitApplySurfaceRequest,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<Box<PreparedGitApply>, DaemonInvocationResponse> {
    let service = Arc::clone(&owner.service);
    let preview = service.read_preview(&request.preview_id).map_err(|error| {
        application_problem(wire_request_id.clone(), map_git_port_problem(error))
    })?;
    if preview.preview_digest != request.preview_digest
        || preview.repository_snapshot.project_id != owner.project_id
    {
        return Err(concealed_application_problem(wire_request_id));
    }
    let authority = owner
        .current_authority(preview.operation)
        .map_err(|error| {
            application_problem(wire_request_id.clone(), map_git_port_problem(error))
        })?;
    let request = build_git_apply_request(
        &wire_request_id,
        request,
        &preview,
        &authority,
        deadline,
        cancellation,
    )
    .map_err(|problem| application_problem(wire_request_id.clone(), problem))?;
    Ok(Box::new(PreparedGitApply {
        wire_request_id,
        service,
        request,
    }))
}

async fn settle_prepared_git_preview(
    operation_events: &OperationEventAuthority,
    prepared: Box<PreparedGitPreview>,
) -> DaemonInvocationResponse {
    let scope = prepared.request.context.scope().clone();
    let Ok(emitter) = operation_events
        .begin(
            &prepared.request.context,
            OperationKind::GitPreview,
            prepared.request.observed_at,
        )
        .await
    else {
        return DaemonInvocationResponse::problem(
            prepared.wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    let _ = emitter.progress(0, Some(1)).await;
    let started_at = prepared.request.observed_at;
    let effective_deadline = prepared.request.context.deadline().clone();
    let wire_request_id = prepared.wire_request_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        GitIndexTransactionService::new(SharedGitTransactionPort {
            service: prepared.service,
            cancellation: None,
        })
        .preview(prepared.request)
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

async fn settle_prepared_git_apply(
    operation_events: &OperationEventAuthority,
    prepared: Box<PreparedGitApply>,
) -> DaemonInvocationResponse {
    let scope = prepared.request.context.scope().clone();
    let Ok(emitter) = operation_events
        .begin(
            &prepared.request.context,
            OperationKind::GitApply,
            prepared.request.observed_at,
        )
        .await
    else {
        return DaemonInvocationResponse::problem(
            prepared.wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    let _ = emitter.progress(0, Some(1)).await;
    let started_at = prepared.request.observed_at;
    let effective_deadline = prepared.request.context.deadline().clone();
    let wire_request_id = prepared.wire_request_id.clone();
    let cancellation = emitter.clone();
    let result = tokio::task::spawn_blocking(move || {
        GitIndexTransactionService::new(SharedGitTransactionPort {
            service: prepared.service,
            cancellation: Some(cancellation),
        })
        .apply(prepared.request)
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
    input: GitIndexPreviewInputV1,
    current: &DaemonGitAuthorityStateV1,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<GitIndexPreviewRequestV1, ApplicationProblem> {
    let observed_at = current.evaluated_at;
    if input.operation != request.operation
        || request.selected_hunk_digests.len()
            > tracedecay_domain::MAX_GIT_INDEX_PREVIEW_INPUT_HUNKS
        || request
            .selected_hunk_digests
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(invalid_git_request());
    }
    let selected_hunks = request
        .selected_hunk_digests
        .iter()
        .map(|digest| {
            input
                .hunks
                .iter()
                .find(|hunk| {
                    hunk.compute_digest()
                        .is_ok_and(|candidate| candidate == *digest)
                })
                .cloned()
                .ok_or_else(invalid_git_request)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (context, authority, binding) = git_request_authority(
        request_id,
        &input.repository_snapshot,
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
        preview_id: input.preview_id,
        repository_snapshot: input.repository_snapshot,
        selected_hunks,
        commit_intent: input.commit_intent,
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
    preview: &GitIndexPreviewV1,
    current: &DaemonGitAuthorityStateV1,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<GitIndexApplyRequestV1, ApplicationProblem> {
    let observed_at = current.evaluated_at;
    let (context, authority, binding) = git_request_authority(
        request_id,
        &preview.repository_snapshot,
        preview.operation,
        current,
        deadline,
        cancellation,
        observed_at,
    )?;
    Ok(GitIndexApplyRequestV1 {
        context,
        authority: authority.clone(),
        binding,
        preview_id: request.preview_id,
        preview_digest: request.preview_digest,
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
        GitIndexTransactionPortError::ExpiredPreview => ApplicationProblem::stale(SafeDiagnostic {
            code: "git_index.expired_preview".to_owned(),
            message: "The Git index preview input expired".to_owned(),
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
