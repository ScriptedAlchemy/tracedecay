//! Typed retained-memory receipt assembly.

use serde::Serialize;
use tracedecay_application::retained_surfaces::{
    RetainedSurfaceEvidenceTerminalV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
    SessionCoverageModeV1,
};
use tracedecay_application::{
    ApplicationOutcome, AuthorityReceipt, EffectId, EffectReceipt, EffectResult, EffectTermination,
    EvidenceAuthority, EvidenceCoverage, EvidenceIdentity, EvidencePacket, IdempotencyKey,
    Omission, OpaqueCursor, OperationBudgetUsage, OperationReceipt, PageState, PolicyDecisionRef,
    ReconciliationState, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    TemporalState, now_micros,
};
use tracedecay_domain::{ComponentVersion, ManifestDigest, TemporalModeV1, canonical_sha256};
use tracedecay_tool_catalog::{EffectClass, SortContractId};

pub(super) fn evidence_outcome(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    operation: RetainedSurfaceOperation,
    result: RetainedSurfaceResultV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let facts = result.evidence_facts().map_err(map_evidence_terminal)?;
    let domain = facts.domain;
    let finished_at = now_micros();
    let authority = authority_receipt(context, finished_at)?;
    let result_digest = canonical_sha256(&(operation.as_str(), &result))
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let evidence_id = EvidenceIdentity::new(format!(
        "evidence.retained.{}",
        result_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let coverage = EvidenceCoverage {
        requested_domains: vec![domain],
        visited: facts.visited,
        eligible: facts.eligible,
        returned: facts.returned,
        completeness: facts.completeness,
        domains: vec![tracedecay_application::CoverageDomainState {
            domain,
            completeness: facts.completeness,
        }],
    };
    coverage
        .validate()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let page = PageState::first_page(
        SortContractId::new(format!("sort.retained.{}.v1", operation.as_str()))
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
        1,
        facts.total,
        facts.returned,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let page = page_with_next_cursor(page, facts.next_cursor.as_deref())?;
    let execution = OperationReceipt::completed(
        context.observed_at,
        finished_at,
        context.request_context.deadline().clone(),
        measured_budget(context.observed_at, finished_at, &result)?,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    Ok(ApplicationOutcome::Evidence(EvidencePacket {
        temporal: evidence_temporal_state(&facts, context.observed_at, finished_at)?,
        authority,
        evidence_authorities: vec![EvidenceAuthority {
            evidence_id,
            source_kind: "mounted_retained_authority".to_owned(),
            producer: operation.as_str().to_owned(),
            scope: context.request_context.scope().clone(),
            revision: ComponentVersion::new("tracedecay.application.retained-surface.v1")
                .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
            horizon: None,
        }],
        coverage,
        omissions: facts
            .omissions
            .into_iter()
            .map(|omission| Omission {
                domain,
                count: omission.count,
                reason: omission.reason,
            })
            .collect(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page,
        execution,
        payload: Some(result),
    }))
}

pub(super) fn session_refresh_effect_outcome<T: Serialize>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    operation: RetainedSurfaceOperation,
    configuration_digest: &ManifestDigest,
    request: &T,
    durable_operation_id: &str,
    result: RetainedSurfaceResultV1,
    reconciliation_required: bool,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    if !matches!(
        operation,
        RetainedSurfaceOperation::SessionRefreshBegin
            | RetainedSurfaceOperation::SessionRefreshCancel
    ) || durable_operation_id.trim().is_empty()
    {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    let finished_at = now_micros();
    let authority = authority_receipt(context, finished_at)?;
    let input_digest = canonical_sha256(&(
        "tracedecay.retained.effect.input.v1",
        operation.as_str(),
        request,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let expected_state = canonical_sha256(&(
        "tracedecay.retained.effect.expected-state.v1",
        context.request_context.scope(),
        operation.as_str(),
        request,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let committed_state = canonical_sha256(&(
        "tracedecay.retained.effect.committed-state.v1",
        operation.as_str(),
        durable_operation_id,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let catalog_digest = canonical_sha256(&(
        "tracedecay.retained.effect.catalog.v1",
        context.operation.capability_id(),
        context.operation.use_case_id(),
        context.operation.result_contract(),
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let privacy_digest = canonical_sha256(&(
        "tracedecay.retained.effect.privacy.v1",
        context.request_context.scope(),
        context.request_context.grant().disclosure,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let suffix = input_digest.as_str().trim_start_matches("sha256:");
    let idempotency_key = IdempotencyKey::new(format!(
        "idempotency.retained.{}.{suffix}",
        operation.as_str()
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let termination = if reconciliation_required {
        EffectTermination::Partial
    } else {
        EffectTermination::Completed
    };
    let receipt = EffectReceipt {
        operation: context.operation.use_case_id().clone(),
        request_id: context.request_context.request_id().clone(),
        actor: context.request_context.actor().clone(),
        scope: context.request_context.scope().clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest,
        expected_state: expected_state.clone(),
        policy_digest: context.request_context.grant().digest.clone(),
        configuration_digest: configuration_digest.clone(),
        catalog_digest,
        privacy_digest,
        outcome: termination,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    receipt
        .validate()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    if reconciliation_required {
        return Err(RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code: "application.retained.session-refresh.delivery-failed".to_owned(),
            committed_receipt: receipt,
            detail: "The session refresh committed, but required scheduler delivery failed."
                .to_owned(),
        });
    }
    let execution = OperationReceipt::completed(
        context.observed_at,
        finished_at,
        context.request_context.deadline().clone(),
        measured_budget(context.observed_at, finished_at, &result)?,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let effect = EffectResult::new(
        EffectId::new(format!(
            "effect.retained.{}.{}",
            operation.as_str(),
            durable_operation_id
        ))
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
        EffectClass::Administrative,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    Ok(ApplicationOutcome::Effect(effect))
}

fn map_evidence_terminal(
    terminal: RetainedSurfaceEvidenceTerminalV1,
) -> RetainedSurfaceExecutionErrorV1 {
    match terminal {
        RetainedSurfaceEvidenceTerminalV1::Effect => {
            RetainedSurfaceExecutionErrorV1::InvalidRequest
        }
        RetainedSurfaceEvidenceTerminalV1::Busy
        | RetainedSurfaceEvidenceTerminalV1::CursorManifestLimitExceeded => {
            RetainedSurfaceExecutionErrorV1::Saturated
        }
        RetainedSurfaceEvidenceTerminalV1::Cancelled => RetainedSurfaceExecutionErrorV1::Cancelled,
        RetainedSurfaceEvidenceTerminalV1::Conflict => RetainedSurfaceExecutionErrorV1::Conflict,
        RetainedSurfaceEvidenceTerminalV1::Denied
        | RetainedSurfaceEvidenceTerminalV1::NotFoundOrNotAuthorized => {
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        }
        RetainedSurfaceEvidenceTerminalV1::TimedOut => RetainedSurfaceExecutionErrorV1::TimedOut,
        RetainedSurfaceEvidenceTerminalV1::Unsupported => {
            RetainedSurfaceExecutionErrorV1::Unsupported
        }
        RetainedSurfaceEvidenceTerminalV1::Failed
        | RetainedSurfaceEvidenceTerminalV1::InvalidOutput
        | RetainedSurfaceEvidenceTerminalV1::Unavailable => {
            RetainedSurfaceExecutionErrorV1::Unavailable
        }
    }
}

fn page_with_next_cursor(
    mut page: PageState,
    next_cursor: Option<&str>,
) -> Result<PageState, RetainedSurfaceExecutionErrorV1> {
    page.cursor = next_cursor
        .map(|cursor| OpaqueCursor::new(cursor.to_owned()))
        .transpose()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    Ok(page)
}

fn evidence_temporal_state(
    facts: &tracedecay_application::retained_surfaces::RetainedSurfaceEvidenceFactsV1,
    requested_at: tracedecay_domain::UtcMicros,
    resolved_at: tracedecay_domain::UtcMicros,
) -> Result<TemporalState, RetainedSurfaceExecutionErrorV1> {
    let Some(temporal) = &facts.temporal else {
        return Ok(TemporalState {
            requested_mode: TemporalModeV1::Current,
            requested_at,
            resolved_at,
            source_generation: None,
            watermark_digest: None,
            freshness: facts.freshness,
        });
    };
    let requested_mode = temporal_request_mode(&temporal.requests)?;
    let watermark_digest = canonical_sha256(&temporal.watermarks)
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    Ok(TemporalState {
        requested_mode,
        requested_at,
        resolved_at,
        source_generation: None,
        watermark_digest: Some(watermark_digest),
        freshness: facts.freshness,
    })
}

fn temporal_request_mode(
    requests: &[tracedecay_application::retained_surfaces::RetainedSurfaceTemporalRequestV1],
) -> Result<TemporalModeV1, RetainedSurfaceExecutionErrorV1> {
    let Some(first) = requests.first() else {
        return Ok(TemporalModeV1::Current);
    };
    if requests.iter().any(|request| request.mode != first.mode) {
        return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
    }
    Ok(match first.mode {
        SessionCoverageModeV1::Current => TemporalModeV1::Current,
        SessionCoverageModeV1::AsOf { cutoff } => TemporalModeV1::AsOf {
            cutoff: tracedecay_domain::UtcMicros(cutoff),
        },
        SessionCoverageModeV1::Evolution => TemporalModeV1::Evolution,
        SessionCoverageModeV1::Forensic => TemporalModeV1::Forensic,
    })
}

pub(super) fn measured_budget<T: Serialize>(
    started_at: tracedecay_domain::UtcMicros,
    finished_at: tracedecay_domain::UtcMicros,
    result: &T,
) -> Result<OperationBudgetUsage, RetainedSurfaceExecutionErrorV1> {
    let elapsed_micros = finished_at
        .0
        .checked_sub(started_at.0)
        .and_then(|elapsed| u64::try_from(elapsed).ok())
        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let bytes_consumed = serde_json::to_vec(result)
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
        .and_then(|payload| {
            u64::try_from(payload.len()).map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
        })?;
    Ok(OperationBudgetUsage {
        units_consumed: 1,
        bytes_consumed,
        elapsed_micros,
    })
}

pub(super) fn authority_receipt(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    observed_at: tracedecay_domain::UtcMicros,
) -> Result<AuthorityReceipt, RetainedSurfaceExecutionErrorV1> {
    let policy = PolicyDecisionRef::new(
        "policy.admitted-capability-grant.v1",
        context.request_context.grant().revision,
        context.request_context.grant().digest.clone(),
        ComponentVersion::new("tracedecay.application.retained-surface.v1")
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    AuthorityReceipt::from_context(context.request_context, policy, observed_at)
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_application::retained_surfaces::{
        RetainedOutcomeStatusV1, SessionRefreshBeginResultV1,
    };
    use tracedecay_application::{
        ApplicationOutcome, CancellationContext, CancellationSignal, CapabilityGrantId,
        CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext, RequestId,
        RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
        retained_surface_application_operation,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
    };

    use super::{
        EffectTermination, RetainedSurfaceOperation, RetainedSurfaceResultV1,
        session_refresh_effect_outcome,
    };

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("valid digest")
    }

    fn refresh_effect_settlement(
        reconciliation_required: bool,
    ) -> (
        Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1>,
        tracedecay_tool_catalog::UseCaseId,
        RequestId,
    ) {
        let operation =
            retained_surface_application_operation(RetainedSurfaceOperation::SessionRefreshBegin)
                .expect("begin operation");
        let scope = tracedecay_application::ResolvedScope::new(
            ProjectId::new("project.retained.refresh").expect("project"),
            RepositoryId::new("repository.retained.refresh").expect("repository"),
            WorktreeId::new("worktree.retained.refresh").expect("worktree"),
            None,
        )
        .expect("scope");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.retained.refresh").expect("grant id"),
            1,
            digest('a'),
            ActorId::new("actor.retained.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            scope.clone(),
            BTreeSet::from([operation.capability_id().clone()]),
            BTreeSet::from([operation.use_case_id().clone()]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = RequestContext::new(
            ActorId::new("actor.retained.caller").expect("caller"),
            scope,
            grant,
            RequestId::new("request.retained.refresh").expect("request"),
            Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            CancellationContext::active("cancel.retained.refresh").expect("cancellation"),
        )
        .expect("context");
        let cancellation = CancellationSignal::active("cancel.retained.refresh").expect("signal");
        let execution = RetainedSurfaceExecutionContextV1 {
            request_context: &context,
            cancellation_signal: &cancellation,
            operation: &operation,
            observed_at: UtcMicros(1),
        };
        let result = RetainedSurfaceResultV1::SessionRefreshBegin(SessionRefreshBeginResultV1 {
            outcome: RetainedOutcomeStatusV1::Started,
            scope: "project".to_owned(),
            tool: "tracedecay_session_refresh".to_owned(),
            accepted_at: Some(2),
            handle: Some("srh_fixture".to_owned()),
            operation_id: Some("refresh.operation.fixture".to_owned()),
            progress: None,
            receipt: None,
            error: None,
        });

        let expected_operation = operation.use_case_id().clone();
        let expected_request = context.request_id().clone();
        let outcome = session_refresh_effect_outcome(
            &execution,
            RetainedSurfaceOperation::SessionRefreshBegin,
            &digest('b'),
            &"request fixture",
            "refresh.operation.fixture",
            result,
            reconciliation_required,
        );
        (outcome, expected_operation, expected_request)
    }

    #[test]
    fn refresh_effect_receipt_binds_the_durable_operation() {
        let (outcome, expected_operation, expected_request) = refresh_effect_settlement(false);
        let outcome = outcome.expect("effect outcome");
        let ApplicationOutcome::Effect(effect) = outcome else {
            panic!("refresh begin must be an effect");
        };
        assert!(
            effect
                .effect_id
                .as_str()
                .ends_with("refresh.operation.fixture")
        );
        assert_eq!(effect.receipt.operation, expected_operation);
        assert_eq!(effect.receipt.request_id, expected_request);
        assert!(effect.receipt.committed_state.is_some());
        assert!(effect.payload.is_some());
    }

    #[test]
    fn delivery_failure_preserves_the_committed_state_for_reconciliation() {
        let (completed, _, _) = refresh_effect_settlement(false);
        let ApplicationOutcome::Effect(completed) = completed.expect("completed settlement") else {
            panic!("refresh begin must be an effect");
        };
        let (partial, _, _) = refresh_effect_settlement(true);
        let RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code,
            committed_receipt,
            ..
        } = partial.expect_err("failed delivery must be a partial effect")
        else {
            panic!("failed delivery must retain its committed receipt");
        };

        assert_eq!(
            reason_code,
            "application.retained.session-refresh.delivery-failed"
        );
        assert_eq!(committed_receipt.outcome, EffectTermination::Partial);
        let durable_commit = canonical_sha256(&(
            "tracedecay.retained.effect.committed-state.v1",
            RetainedSurfaceOperation::SessionRefreshBegin.as_str(),
            "refresh.operation.fixture",
        ))
        .expect("canonical durable operation digest");
        assert_eq!(committed_receipt.committed_state, Some(durable_commit));
        assert_eq!(
            committed_receipt.committed_state,
            completed.receipt.committed_state
        );
    }
}
