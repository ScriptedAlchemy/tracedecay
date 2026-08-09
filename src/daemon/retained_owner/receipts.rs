//! Typed retained-memory receipt assembly.

use serde::Serialize;
use tracedecay_application::retained_surfaces::{
    FactCommitDispositionV1, FactCommitOwnerV1, FactCommitReceiptV1, FactMutationReceiptV1,
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
use tracedecay_domain::{
    ComponentVersion, FactOwnerV1, ManifestDigest, TemporalModeV1, canonical_sha256,
};
use tracedecay_store::{FactCommitOutcome, ProjectMemoryFactMutationReceiptV1};
use tracedecay_tool_catalog::SortContractId;

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

pub(super) fn effect_outcome(
    configuration_digest: &ManifestDigest,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    result: RetainedSurfaceResultV1,
    mutation: &ProjectMemoryFactMutationReceiptV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let finished_at = now_micros();
    let authority = authority_receipt(context, finished_at)?;
    let input_digest = ManifestDigest::new(mutation.input_digest().as_str().to_owned())
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let expected_state = canonical_sha256(&(
        "tracedecay.application.retained.fact.expected-state.v1",
        mutation.receipt().owner(),
        mutation.receipt().fact_id(),
        mutation
            .expected_last_event_id()
            .map(|event| event.as_str()),
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let mutation_projection = fact_mutation(mutation)?;
    let committed_state = canonical_sha256(&(
        "tracedecay.application.retained.fact.committed-state.v1",
        &mutation_projection,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let idempotency_key = IdempotencyKey::new(mutation.operation_id().as_str().to_owned())
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let catalog = tracedecay_application::retained_surface_catalog_contribution()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let effect_class = catalog
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == context.operation.capability_id())
        .map(|manifest| manifest.effect())
        .filter(|effect| effect.is_effect())
        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let catalog_digest =
        canonical_sha256(&catalog).map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let privacy_digest = canonical_sha256(&(
        "tracedecay.application.retained-surface.privacy.v1",
        &context.request_context.grant().grant_id,
        &context.request_context.grant().digest,
        context.request_context.grant().disclosure,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let receipt = EffectReceipt {
        operation: context.operation.use_case_id().clone(),
        request_id: context.request_context.request_id().clone(),
        actor: context.request_context.actor().clone(),
        scope: context.request_context.scope().clone(),
        effect_class,
        idempotency_key: idempotency_key.clone(),
        input_digest: input_digest.clone(),
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: configuration_digest.clone(),
        catalog_digest,
        privacy_digest,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    let execution = OperationReceipt::completed(
        context.observed_at,
        finished_at,
        context.request_context.deadline().clone(),
        measured_budget(context.observed_at, finished_at, &result)?,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    EffectResult::new(
        EffectId::new(mutation.operation_id().as_str().to_owned())
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
        effect_class,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )
    .map(ApplicationOutcome::Effect)
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

/// Records an admitted fact effect that deliberately made no authority
/// mutation. The expected state binds the exact typed request and result and
/// becomes the unchanged committed state, so callers do not misrepresent a
/// rejected, duplicate, or already-absent fact as a successful write.
pub(super) fn no_op_effect_outcome<T: Serialize>(
    configuration_digest: &ManifestDigest,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &T,
    result: RetainedSurfaceResultV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let finished_at = now_micros();
    let authority = authority_receipt(context, finished_at)?;
    let input_digest = canonical_sha256(&(context.operation.capability_id().as_str(), request))
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let expected_state = canonical_sha256(&(
        context.operation.capability_id().as_str(),
        context.request_context.scope(),
        &result,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let idempotency_key =
        IdempotencyKey::new(context.request_context.request_id().as_str().to_owned())
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let catalog = tracedecay_application::retained_surface_catalog_contribution()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let effect_class = catalog
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == context.operation.capability_id())
        .map(|manifest| manifest.effect())
        .filter(|effect| effect.is_effect())
        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let catalog_digest =
        canonical_sha256(&catalog).map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let privacy_digest = canonical_sha256(&(
        "tracedecay.application.retained-surface.privacy.v1",
        &context.request_context.grant().grant_id,
        &context.request_context.grant().digest,
        context.request_context.grant().disclosure,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let receipt = EffectReceipt {
        operation: context.operation.use_case_id().clone(),
        request_id: context.request_context.request_id().clone(),
        actor: context.request_context.actor().clone(),
        scope: context.request_context.scope().clone(),
        effect_class,
        idempotency_key: idempotency_key.clone(),
        input_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: configuration_digest.clone(),
        catalog_digest,
        privacy_digest,
        outcome: EffectTermination::Completed,
        committed_state: Some(expected_state.clone()),
        external_proof: None,
    };
    let execution = OperationReceipt::completed(
        context.observed_at,
        finished_at,
        context.request_context.deadline().clone(),
        measured_budget(context.observed_at, finished_at, &result)?,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    EffectResult::new(
        EffectId::new(context.request_context.request_id().as_str().to_owned())
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
        effect_class,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )
    .map(ApplicationOutcome::Effect)
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

pub(super) fn fact_mutation(
    mutation: &ProjectMemoryFactMutationReceiptV1,
) -> Result<FactMutationReceiptV1, RetainedSurfaceExecutionErrorV1> {
    let disposition = match mutation.commit() {
        FactCommitOutcome::Committed(_) => FactCommitDispositionV1::Committed,
        FactCommitOutcome::IdempotentReplay(_) => FactCommitDispositionV1::IdempotentReplay,
        FactCommitOutcome::Conflict(_) => {
            return Err(RetainedSurfaceExecutionErrorV1::Conflict);
        }
    };
    let receipt = mutation.receipt();
    Ok(FactMutationReceiptV1 {
        operation_id: mutation.operation_id().as_str().to_owned(),
        input_digest: mutation.input_digest().as_str().to_owned(),
        commit: FactCommitReceiptV1 {
            disposition,
            fact_id: receipt.fact_id().clone(),
            owner: match receipt.owner() {
                FactOwnerV1::Profile => FactCommitOwnerV1::Profile,
                FactOwnerV1::Project { project_id } => FactCommitOwnerV1::Project {
                    project_id: project_id.as_str().to_owned(),
                },
            },
            expected_last_event_id: receipt
                .expected_last_event_id()
                .map(|event| event.as_str().to_owned()),
            committed_event_ids: receipt
                .committed_event_ids()
                .iter()
                .map(|event| event.as_str().to_owned())
                .collect(),
            last_event_id: receipt.last_event_id().as_str().to_owned(),
            active_assertion_id: receipt
                .active_assertion_id()
                .map(|assertion| assertion.as_str().to_owned()),
        },
        expected_last_event_id: mutation
            .expected_last_event_id()
            .map(|event| event.as_str().to_owned()),
        committed_generation: mutation.committed_generation().as_str().to_owned(),
        replayed: mutation.replayed(),
    })
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
