//! Context projection composition for test runs, advisory findings, impact, and affected tests.

use tracedecay_contracts::{OperationTermination, now_micros};
use tracedecay_domain::feedback::{
    FeedbackCycleResultV1, FeedbackCycleTerminationV1, FeedbackDiagnosticProducerV1,
    FeedbackFindingLifecycleV1, FeedbackFindingV1, FeedbackImpactStateV1,
    ProviderEvaluationStateV1,
};
use tracedecay_lsp::{
    AdmittedRoot, ContextCoverage, ContextFreshness, ContextProducerState,
    ContextProjectionEnvelope, ContextProjectionItem, ContextProjectionKind,
    ContextProjectionOutcome, GatewayDiagnosticCoverage, GatewayDiagnosticLifecycle,
    GatewayDiagnosticProviderState, MAX_CONTEXT_PROJECTION_ITEMS,
    MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES, MAX_CONTEXT_SUMMARY_BYTES, TRACEDECAY_CONTEXT_REVISION,
};

use super::LspFeedbackProjectionScope;
use crate::operation_stream::ManagedTestRunSnapshot;

/// `document_uri` is the client's spelling and is echoed on the envelope;
/// `retained_document_uri` is the identity the managed run recorded the
/// document's digest under (see
/// `RegisteredProjectLspAuthority::retained_document_uri`).
pub(super) fn test_run_projection(
    root: AdmittedRoot,
    document_uri: Option<String>,
    retained_document_uri: Option<&str>,
    scope: LspFeedbackProjectionScope,
    snapshot: ManagedTestRunSnapshot,
) -> ContextProjectionOutcome {
    let termination = match snapshot.termination {
        Some(termination) => termination,
        None if snapshot.deadline.is_elapsed_at(now_micros()) => OperationTermination::TimedOut,
        None => return ContextProjectionOutcome::Pending,
    };
    let Some(head_commit_id) = snapshot.head_commit_id.as_ref() else {
        return ContextProjectionOutcome::Deferred {
            reason: "managed-test-run-head-unbound".to_owned(),
        };
    };
    let Some(code_generation_id) = snapshot.code_generation_id.as_ref() else {
        return ContextProjectionOutcome::Deferred {
            reason: "managed-test-run-code-generation-unbound".to_owned(),
        };
    };
    if head_commit_id != &scope.head_commit_id || code_generation_id != &scope.code_generation_id {
        return ContextProjectionOutcome::Deferred {
            reason: "managed-test-run-source-identity-stale".to_owned(),
        };
    }
    if let Some(retained_document_uri) = retained_document_uri {
        let Some(current_digest) = scope.document_content_digest.as_ref() else {
            return ContextProjectionOutcome::Deferred {
                reason: "managed-test-run-document-content-unbound".to_owned(),
            };
        };
        let Some(retained_digest) = snapshot.document_content_digests.get(retained_document_uri)
        else {
            return ContextProjectionOutcome::Deferred {
                reason: "managed-test-run-document-content-unbound".to_owned(),
            };
        };
        if retained_digest != current_digest {
            return ContextProjectionOutcome::Deferred {
                reason: "managed-test-run-document-content-stale".to_owned(),
            };
        }
    }
    let missing_results = snapshot
        .completed
        .saturating_sub(snapshot.available_results as u64);
    let bounded_omissions = snapshot.available_results.saturating_sub(
        snapshot
            .result_offset
            .saturating_add(snapshot.results.len()),
    ) as u64;
    let mut omitted_count =
        usize::try_from(missing_results.saturating_add(bounded_omissions)).unwrap_or(usize::MAX);
    let completed_with_full_results = snapshot.total == Some(snapshot.completed)
        && snapshot.results.len() as u64 == snapshot.completed
        && omitted_count == 0;
    let (coverage, producer_state, include_results) = match termination {
        OperationTermination::Completed if completed_with_full_results => (
            ContextCoverage::Complete,
            ContextProducerState::Complete,
            true,
        ),
        OperationTermination::Completed | OperationTermination::Partial => (
            ContextCoverage::Partial,
            ContextProducerState::Partial,
            true,
        ),
        OperationTermination::Cancelled => (
            ContextCoverage::Unavailable,
            ContextProducerState::Cancelled,
            false,
        ),
        OperationTermination::TimedOut => (
            ContextCoverage::Unavailable,
            ContextProducerState::TimedOut,
            false,
        ),
        OperationTermination::Failed => {
            (ContextCoverage::Failed, ContextProducerState::Failed, false)
        }
        OperationTermination::Unavailable => (
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable,
            false,
        ),
        OperationTermination::EffectUnknown => (
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable,
            false,
        ),
    };
    let operation_id = snapshot.operation_id.to_string();
    let items = if include_results {
        snapshot
            .results
            .into_iter()
            .enumerate()
            .map(|(index, result)| ContextProjectionItem {
                stable_id: format!(
                    "{operation_id}.{}",
                    snapshot.result_offset.saturating_add(index)
                ),
                summary: bounded_test_run_summary(&result.test, result.passed),
                retrieval_handle: None,
            })
            .collect()
    } else {
        omitted_count = omitted_count.saturating_add(snapshot.results.len());
        Vec::new()
    };
    let omission_reasons = projection_omission_reasons(coverage, omitted_count, producer_state);
    ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
        root_uri: root.uri().to_owned(),
        document_uri,
        kind: ContextProjectionKind::test_run_results(),
        generation: scope.generation,
        identity: scope.projection_identity(),
        freshness: ContextFreshness::Current,
        producer_state,
        coverage,
        revision: TRACEDECAY_CONTEXT_REVISION,
        items,
        omitted_count,
        omission_reasons,
        retrieval_handle: None,
    })
}

pub(super) fn projection_omission_reasons(
    coverage: ContextCoverage,
    omitted_count: usize,
    producer_state: ContextProducerState,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if omitted_count > 0 {
        reasons.push("bounded-projection-items".to_owned());
    }
    if coverage != ContextCoverage::Complete {
        reasons.push(
            match producer_state {
                ContextProducerState::Complete => "projection-incomplete",
                ContextProducerState::Partial => "producer-partial",
                ContextProducerState::Indexing => "producer-indexing",
                ContextProducerState::Unavailable => "producer-unavailable",
                ContextProducerState::Failed => "producer-failed",
                ContextProducerState::Cancelled => "producer-cancelled",
                ContextProducerState::TimedOut => "producer-timed-out",
            }
            .to_owned(),
        );
    }
    reasons
}

pub(super) fn bounded_test_run_summary(test: &str, passed: bool) -> String {
    let prefix = if passed { "passed: " } else { "failed: " };
    let truncated = tracedecay_runtime_core::text::utf8_prefix_at_or_before(
        test,
        MAX_CONTEXT_SUMMARY_BYTES.saturating_sub(prefix.len()),
    );
    format!("{prefix}{truncated}")
}

pub(super) fn finding_item(finding: &FeedbackFindingV1) -> Option<ContextProjectionItem> {
    if finding.lifecycle != FeedbackFindingLifecycleV1::Active {
        return None;
    }
    projection_item(
        finding.finding_id.as_str(),
        finding
            .safe_bounded_preview
            .clone()
            .unwrap_or_else(|| "feedback finding".to_owned()),
    )
}

/// Maps a negotiated advisory projection to the canonical producer that owns
/// its findings. No gateway-local source or inferred contributor is involved.
pub(super) fn advisory_projection_producer(
    kind: &ContextProjectionKind,
) -> Option<FeedbackDiagnosticProducerV1> {
    match kind.as_str() {
        ContextProjectionKind::GITHUB_REVIEW => Some(FeedbackDiagnosticProducerV1::GitHubReview),
        ContextProjectionKind::CI_FAILURE_LOCALIZATION => {
            Some(FeedbackDiagnosticProducerV1::CiLocalization)
        }
        ContextProjectionKind::AGENT_PROXIMITY => Some(FeedbackDiagnosticProducerV1::Proximity),
        _ => None,
    }
}

/// A finding is visible in exactly the advisory projection backed by its
/// canonical diagnostic producer. Lifecycle filtering remains centralized in
/// `finding_item`, so a saved cycle that clears the finding clears the LSP
/// projection as well.
pub(super) fn advisory_finding_matches(
    finding: &FeedbackFindingV1,
    producer: FeedbackDiagnosticProducerV1,
) -> bool {
    finding
        .diagnostic_projection
        .as_ref()
        .is_some_and(|projection| projection.producer == producer)
}

/// Checks document membership before an LSP context handle can be bound. An
/// advisory finding names its own file; ordinary canonical findings are
/// document-scoped through the cycle impact target. If neither can establish
/// that relationship, this document cannot safely expose the finding.
pub(super) fn finding_matches_document(
    finding: &FeedbackFindingV1,
    scope: &LspFeedbackProjectionScope,
    impact_target_file: Option<&tracedecay_domain::FileOccurrenceId>,
) -> bool {
    let finding_file = finding
        .diagnostic_projection
        .as_ref()
        .map(|projection| &projection.file)
        .or(impact_target_file);
    if let Some(document_file) = scope.document_file_occurrence_id.as_ref() {
        return finding_file == Some(document_file);
    }
    let Some(document_relative_path) = scope.document_relative_path.as_deref() else {
        return true;
    };
    finding_file.is_some_and(|file| file.as_str() == document_relative_path)
}

pub(super) fn impact_projection(
    cycle: &FeedbackCycleResultV1,
) -> (ContextCoverage, Vec<ContextProjectionItem>, usize) {
    let Some(impact) = cycle.impact.as_ref() else {
        return (impact_coverage(cycle.impact_state), Vec::new(), 0);
    };
    let total_items = impact.affected_files.len() + impact.affected_callers.len();
    let mut items = impact
        .affected_files
        .iter()
        .filter_map(|file| projection_item(file.as_str(), "affected file"))
        .chain(
            impact
                .affected_callers
                .iter()
                .filter_map(|caller| projection_item(caller.as_str(), "affected caller")),
        )
        .collect::<Vec<_>>();
    let omitted_count = total_items.saturating_sub(items.len().min(MAX_CONTEXT_PROJECTION_ITEMS));
    items.truncate(MAX_CONTEXT_PROJECTION_ITEMS);
    (impact_coverage(Some(impact.state)), items, omitted_count)
}

pub(super) fn affected_test_projection(
    cycle: &FeedbackCycleResultV1,
) -> (ContextCoverage, Vec<ContextProjectionItem>, usize) {
    let Some(impact) = cycle.impact.as_ref() else {
        return (impact_coverage(cycle.affected_tests_state), Vec::new(), 0);
    };
    let total_items = impact.affected_tests.len();
    let mut items = impact
        .affected_tests
        .iter()
        .filter_map(|test| projection_item(test.as_str(), "affected test"))
        .collect::<Vec<_>>();
    let omitted_count = total_items.saturating_sub(items.len().min(MAX_CONTEXT_PROJECTION_ITEMS));
    items.truncate(MAX_CONTEXT_PROJECTION_ITEMS);
    (
        impact_coverage(Some(impact.affected_tests_state)),
        items,
        omitted_count,
    )
}

pub(super) fn projection_item(
    stable_id: &str,
    summary: impl Into<String>,
) -> Option<ContextProjectionItem> {
    (stable_id.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES).then(|| ContextProjectionItem {
        stable_id: stable_id.to_owned(),
        summary: summary.into(),
        retrieval_handle: None,
    })
}

pub(super) fn cycle_coverage(cycle: &FeedbackCycleResultV1) -> ContextCoverage {
    if cycle.omitted_findings == 0
        && !cycle.provider_states.is_empty()
        && cycle
            .provider_states
            .iter()
            .all(|state| *state == ProviderEvaluationStateV1::SupportedCompletedComplete)
    {
        ContextCoverage::Complete
    } else if cycle.provider_states.iter().all(|state| {
        matches!(
            state,
            ProviderEvaluationStateV1::Unsupported
                | ProviderEvaluationStateV1::Absent
                | ProviderEvaluationStateV1::Unavailable
        )
    }) {
        ContextCoverage::Unavailable
    } else {
        ContextCoverage::Partial
    }
}

pub(super) fn advisory_provider_state(
    providers: &[tracedecay_domain::feedback::FeedbackAdvisoryProviderStateV1],
    producer: FeedbackDiagnosticProducerV1,
) -> Option<ProviderEvaluationStateV1> {
    providers
        .iter()
        .find(|provider| provider.producer == producer)
        .map(|provider| provider.state)
}

/// Translate one canonical provider evaluation, without allowing an unrelated
/// provider's incomplete coverage to downgrade this projection.
pub(super) fn advisory_projection_status(
    cycle: &FeedbackCycleResultV1,
    producer: FeedbackDiagnosticProducerV1,
) -> (ContextCoverage, ContextProducerState) {
    let (coverage, state) = advisory_provider_status(advisory_provider_state(
        &cycle.advisory_provider_states,
        producer,
    ));
    advisory_coverage(coverage, state, cycle.omitted_findings)
}

pub(super) fn advisory_coverage(
    coverage: ContextCoverage,
    producer_state: ContextProducerState,
    omitted_findings: u64,
) -> (ContextCoverage, ContextProducerState) {
    if omitted_findings > 0
        && coverage == ContextCoverage::Complete
        && producer_state == ContextProducerState::Complete
    {
        // The aggregate result does not identify which advisory producer lost
        // the finding. It is therefore partiality of the cycle, not a
        // producer-specific omitted count.
        (ContextCoverage::Partial, ContextProducerState::Partial)
    } else {
        (coverage, producer_state)
    }
}

pub(super) fn bounded_advisory_item_omissions(
    projected_item_count: usize,
    returned_items: usize,
) -> usize {
    projected_item_count.saturating_sub(returned_items)
}

pub(super) fn advisory_provider_status(
    provider_state: Option<ProviderEvaluationStateV1>,
) -> (ContextCoverage, ContextProducerState) {
    match provider_state {
        Some(ProviderEvaluationStateV1::SupportedCompletedComplete) => {
            (ContextCoverage::Complete, ContextProducerState::Complete)
        }
        Some(
            ProviderEvaluationStateV1::Unsupported
            | ProviderEvaluationStateV1::Absent
            | ProviderEvaluationStateV1::Unavailable,
        )
        | None => (
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable,
        ),
        Some(ProviderEvaluationStateV1::Indexing) => {
            (ContextCoverage::Partial, ContextProducerState::Indexing)
        }
        Some(ProviderEvaluationStateV1::Stale | ProviderEvaluationStateV1::Partial) => {
            (ContextCoverage::Partial, ContextProducerState::Partial)
        }
        Some(ProviderEvaluationStateV1::Cancelled) => (
            ContextCoverage::Unavailable,
            ContextProducerState::Cancelled,
        ),
        Some(ProviderEvaluationStateV1::TimedOut) => {
            (ContextCoverage::Unavailable, ContextProducerState::TimedOut)
        }
        Some(ProviderEvaluationStateV1::Failed) => {
            (ContextCoverage::Failed, ContextProducerState::Failed)
        }
    }
}

pub(super) fn producer_state_for_cycle(cycle: &FeedbackCycleResultV1) -> ContextProducerState {
    match cycle.termination {
        FeedbackCycleTerminationV1::Clean => {
            if cycle_coverage(cycle) == ContextCoverage::Complete {
                ContextProducerState::Complete
            } else {
                ContextProducerState::Partial
            }
        }
        FeedbackCycleTerminationV1::DuplicateNoop => ContextProducerState::Complete,
        FeedbackCycleTerminationV1::IncompleteCoverage
        | FeedbackCycleTerminationV1::StaleReplanRequired => ContextProducerState::Partial,
        FeedbackCycleTerminationV1::BudgetExceeded => ContextProducerState::TimedOut,
        FeedbackCycleTerminationV1::Cancelled | FeedbackCycleTerminationV1::UserStop => {
            ContextProducerState::Cancelled
        }
        FeedbackCycleTerminationV1::Blocked | FeedbackCycleTerminationV1::DaemonUnavailable => {
            ContextProducerState::Unavailable
        }
    }
}

pub(super) fn gateway_diagnostic_coverage(coverage: ContextCoverage) -> GatewayDiagnosticCoverage {
    match coverage {
        ContextCoverage::Complete => GatewayDiagnosticCoverage::Complete,
        ContextCoverage::Partial => GatewayDiagnosticCoverage::Partial,
        ContextCoverage::Unavailable => GatewayDiagnosticCoverage::Unavailable,
        ContextCoverage::Failed => GatewayDiagnosticCoverage::Failed,
    }
}

pub(super) fn gateway_diagnostic_lifecycle(
    lifecycle: FeedbackFindingLifecycleV1,
) -> GatewayDiagnosticLifecycle {
    match lifecycle {
        FeedbackFindingLifecycleV1::Active => GatewayDiagnosticLifecycle::Active,
        FeedbackFindingLifecycleV1::Superseded => GatewayDiagnosticLifecycle::Superseded,
        FeedbackFindingLifecycleV1::Resolved => GatewayDiagnosticLifecycle::Resolved,
        FeedbackFindingLifecycleV1::Cleared => GatewayDiagnosticLifecycle::Cleared,
    }
}

pub(super) fn gateway_diagnostic_provider_state(
    state: ProviderEvaluationStateV1,
) -> GatewayDiagnosticProviderState {
    match state {
        ProviderEvaluationStateV1::SupportedCompletedComplete => {
            GatewayDiagnosticProviderState::SupportedCompletedComplete
        }
        ProviderEvaluationStateV1::Unsupported => GatewayDiagnosticProviderState::Unsupported,
        ProviderEvaluationStateV1::Absent => GatewayDiagnosticProviderState::Absent,
        ProviderEvaluationStateV1::Indexing => GatewayDiagnosticProviderState::Indexing,
        ProviderEvaluationStateV1::Stale => GatewayDiagnosticProviderState::Stale,
        ProviderEvaluationStateV1::Cancelled => GatewayDiagnosticProviderState::Cancelled,
        ProviderEvaluationStateV1::TimedOut => GatewayDiagnosticProviderState::TimedOut,
        ProviderEvaluationStateV1::Failed => GatewayDiagnosticProviderState::Failed,
        ProviderEvaluationStateV1::Partial => GatewayDiagnosticProviderState::Partial,
        ProviderEvaluationStateV1::Unavailable => GatewayDiagnosticProviderState::Unavailable,
    }
}

pub(super) fn impact_coverage(state: Option<FeedbackImpactStateV1>) -> ContextCoverage {
    match state {
        Some(FeedbackImpactStateV1::Complete) => ContextCoverage::Complete,
        Some(FeedbackImpactStateV1::Partial | FeedbackImpactStateV1::Stale) => {
            ContextCoverage::Partial
        }
        Some(FeedbackImpactStateV1::Unavailable) | None => ContextCoverage::Unavailable,
    }
}
