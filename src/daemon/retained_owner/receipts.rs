//! Typed retained-memory receipt assembly.

use serde::Serialize;
use tracedecay_application::retained_surfaces::{
    RetainedSurfaceEvidenceTerminalV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
    SessionCoverageModeV1,
};
use tracedecay_application::{
    ApplicationOutcome, AuthorityReceipt, CancellationStage, Deadline, EffectId, EffectReceipt,
    EffectResult, EffectTermination, EvidenceAuthority, EvidenceCoverage, EvidenceIdentity,
    EvidencePacket, IdempotencyKey, Omission, OperationBudgetUsage, OperationReceipt, PageState,
    PolicyDecisionRef, ReconciliationState, RetainedSurfaceExecutionContextV1,
    RetainedSurfaceExecutionErrorV1, TemporalState, now_micros,
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
    let effective_deadline = effective_memory_deadline(context);
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
    let mut page = PageState::first_page(
        SortContractId::new(format!("sort.retained.{}.v1", operation.as_str()))
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
        1,
        facts.total,
        facts.returned,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    page.cursor = facts.next_cursor.clone();
    let execution = OperationReceipt::completed(
        context.observed_at,
        finished_at,
        effective_deadline.clone(),
        measured_budget(context.observed_at, finished_at, &result)?,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let outcome = ApplicationOutcome::Evidence(EvidencePacket {
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
    });
    if effective_deadline.is_elapsed_at(now_micros()) {
        return Err(RetainedSurfaceExecutionErrorV1::TimedOut(
            CancellationStage::DuringRead,
        ));
    }
    Ok(outcome)
}

pub(super) fn effective_memory_deadline(
    context: &RetainedSurfaceExecutionContextV1<'_>,
) -> Deadline {
    Deadline {
        expires_at: context
            .request_context
            .deadline()
            .expires_at
            .min(context.request_context.grant().expires_at),
    }
}

pub(crate) struct PreparedRetainedEffect {
    operation: RetainedSurfaceOperation,
    durable_operation_id: String,
    effect_id: EffectId,
    idempotency_key: IdempotencyKey,
    authority: AuthorityReceipt,
    expected_state: ManifestDigest,
    receipt_template: EffectReceipt,
}

pub(crate) fn prepare_retained_effect<T: Serialize>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    operation: RetainedSurfaceOperation,
    configuration_digest: &ManifestDigest,
    request: &T,
    durable_operation_id: &str,
) -> Result<PreparedRetainedEffect, RetainedSurfaceExecutionErrorV1> {
    let admitted_operation = matches!(
        operation,
        RetainedSurfaceOperation::FactStoreCurate
            | RetainedSurfaceOperation::SessionRefreshBegin
            | RetainedSurfaceOperation::SessionRefreshCancel
            | RetainedSurfaceOperation::FactStoreAdd
            | RetainedSurfaceOperation::FactStoreUpdate
            | RetainedSurfaceOperation::FactStoreRemove
            | RetainedSurfaceOperation::FactFeedback
            | RetainedSurfaceOperation::FactStoreSearch
    );
    if !admitted_operation || durable_operation_id.trim().is_empty() {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    let prepared_at = now_micros();
    let authority = authority_receipt(context, prepared_at)?;
    let delivery_id = (operation == RetainedSurfaceOperation::FactStoreSearch)
        .then_some(context.request_context.request_id());
    let input_digest = canonical_sha256(&(
        "tracedecay.retained.effect.input.v1",
        operation.as_str(),
        context.request_context.actor(),
        delivery_id,
        request,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let expected_state = canonical_sha256(&(
        "tracedecay.retained.effect.expected-state.v1",
        context.request_context.scope(),
        operation.as_str(),
        context.request_context.actor(),
        delivery_id,
        request,
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
    let effect_id = EffectId::new(format!(
        "effect.retained.{}.{}",
        operation.as_str(),
        durable_operation_id
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let mut receipt_template = EffectReceipt {
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
        outcome: EffectTermination::Partial,
        committed_state: Some(
            canonical_sha256(&"tracedecay.retained.effect.uncommitted-placeholder.v1")
                .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
        ),
        external_proof: None,
    };
    receipt_template
        .validate()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    receipt_template.committed_state = None;
    Ok(PreparedRetainedEffect {
        operation,
        durable_operation_id: durable_operation_id.to_owned(),
        effect_id,
        idempotency_key,
        authority,
        expected_state,
        receipt_template,
    })
}

impl PreparedRetainedEffect {
    pub(crate) fn material_committed_state_digest<C: Serialize + ?Sized>(
        &self,
        committed_state_material: &C,
    ) -> Result<ManifestDigest, RetainedSurfaceExecutionErrorV1> {
        canonical_sha256(&(
            "tracedecay.retained.effect.committed-state.v1",
            self.operation.as_str(),
            &self.durable_operation_id,
            committed_state_material,
        ))
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
    }

    fn partial_receipt(&self, committed_state: &ManifestDigest) -> EffectReceipt {
        let mut receipt = self.receipt_template.clone();
        receipt.committed_state = Some(committed_state.clone());
        receipt
    }

    pub(super) fn partial_with_digest(
        &self,
        committed_state: &ManifestDigest,
        reason_code: &str,
        detail: &str,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        Err(self.partial_error_with_digest(committed_state, reason_code, detail))
    }

    pub(crate) fn partial_error_with_digest(
        &self,
        committed_state: &ManifestDigest,
        reason_code: &str,
        detail: &str,
    ) -> RetainedSurfaceExecutionErrorV1 {
        RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code: reason_code.to_owned(),
            committed_receipt: self.partial_receipt(committed_state),
            detail: detail.to_owned(),
        }
    }

    pub(super) fn memory_projection_failed(
        &self,
        committed_state: &ManifestDigest,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        self.partial_with_digest(
            committed_state,
            "application.retained.memory-result-projection-failed",
            "The canonical fact committed, but its public projection could not be assembled.",
        )
    }

    pub(super) fn memory_expiry_failed(
        &self,
        committed_state: &ManifestDigest,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        let (reason_code, detail) = memory_expiry_detail();
        self.partial_with_digest(committed_state, reason_code, detail)
    }

    pub(super) fn complete<C: Serialize + ?Sized>(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        committed_state_material: &C,
        reconciliation: ReconciliationState,
        result: RetainedSurfaceResultV1,
        partial: Option<(&str, &str)>,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        let committed_state = self.material_committed_state_digest(committed_state_material)?;
        self.complete_with_digest(context, &committed_state, reconciliation, result, partial)
    }

    pub(crate) fn complete_with_digest(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        committed_state: &ManifestDigest,
        reconciliation: ReconciliationState,
        result: RetainedSurfaceResultV1,
        partial: Option<(&str, &str)>,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        let finished_at = now_micros();
        self.complete_at(
            context.observed_at,
            finished_at,
            effective_memory_deadline(context),
            committed_state,
            reconciliation,
            result,
            partial,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn complete_at(
        &self,
        observed_at: tracedecay_domain::UtcMicros,
        finished_at: tracedecay_domain::UtcMicros,
        effective_deadline: Deadline,
        committed_state: &ManifestDigest,
        reconciliation: ReconciliationState,
        result: RetainedSurfaceResultV1,
        partial: Option<(&str, &str)>,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        let partial_receipt = self.partial_receipt(committed_state);
        if let Some((reason_code, detail)) = partial {
            return Err(RetainedSurfaceExecutionErrorV1::PartialEffect {
                reason_code: reason_code.to_owned(),
                committed_receipt: partial_receipt,
                detail: detail.to_owned(),
            });
        }
        let post_commit_failure =
            |detail: &'static str| RetainedSurfaceExecutionErrorV1::PartialEffect {
                reason_code: "application.retained.effect-delivery-failed".to_owned(),
                committed_receipt: partial_receipt.clone(),
                detail: detail.to_owned(),
            };
        let execution = OperationReceipt::completed(
            observed_at,
            finished_at,
            effective_deadline.clone(),
            measured_budget(observed_at, finished_at, &result).map_err(|_| {
                post_commit_failure(
                    "The effect committed, but its delivery budget could not be measured.",
                )
            })?,
        )
        .map_err(|_| {
            post_commit_failure(
                "The effect committed, but its execution receipt could not be assembled.",
            )
        })?;
        let mut completed_receipt = partial_receipt.clone();
        completed_receipt.outcome = EffectTermination::Completed;
        completed_receipt.validate().map_err(|_| {
            post_commit_failure(
                "The effect committed, but its completed receipt could not be validated.",
            )
        })?;
        // Reconciliation covers the authoritative effect named by this receipt.
        // Derived memory-graph lag is reported independently by typed read coverage.
        let effect = EffectResult::new(
            self.effect_id.clone(),
            EffectClass::Administrative,
            self.idempotency_key.clone(),
            self.authority.clone(),
            self.expected_state.clone(),
            execution,
            reconciliation,
            completed_receipt,
            Some(result),
        )
        .map_err(|_| {
            post_commit_failure(
                "The effect committed, but its public result could not be assembled.",
            )
        })?;
        if effective_deadline.is_elapsed_at(finished_at) {
            let (reason_code, detail) = self.expiry_partial();
            return Err(RetainedSurfaceExecutionErrorV1::PartialEffect {
                reason_code: reason_code.to_owned(),
                committed_receipt: partial_receipt,
                detail: detail.to_owned(),
            });
        }
        Ok(ApplicationOutcome::Effect(effect))
    }

    fn expiry_partial(&self) -> (&'static str, &'static str) {
        match self.operation {
            RetainedSurfaceOperation::FactStoreAdd
            | RetainedSurfaceOperation::FactStoreUpdate
            | RetainedSurfaceOperation::FactStoreRemove
            | RetainedSurfaceOperation::FactFeedback
            | RetainedSurfaceOperation::FactStoreSearch => memory_expiry_detail(),
            RetainedSurfaceOperation::SessionRefreshBegin
            | RetainedSurfaceOperation::SessionRefreshCancel => (
                "application.retained.effect-admission-expiry-after-commit",
                "The retained effect committed after the request or capability grant expired.",
            ),
            _ => (
                "application.retained.effect-delivery-failed",
                "The retained effect committed, but its delivery did not settle in time.",
            ),
        }
    }
}

pub(super) fn memory_expiry_partial(
    settled_after_expiry: bool,
) -> Option<(&'static str, &'static str)> {
    settled_after_expiry.then_some(memory_expiry_detail())
}

fn memory_expiry_detail() -> (&'static str, &'static str) {
    (
        "application.retained.memory-admission-expiry-after-commit",
        "The canonical fact mutation committed after the request or capability grant expired.",
    )
}

pub(super) fn retained_effect_outcome<T: Serialize, C: Serialize + ?Sized>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    operation: RetainedSurfaceOperation,
    configuration_digest: &ManifestDigest,
    request: &T,
    durable_operation_id: &str,
    committed_state_material: &C,
    reconciliation: ReconciliationState,
    result: Option<RetainedSurfaceResultV1>,
    partial: Option<(&str, &str)>,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let prepared = prepare_retained_effect(
        context,
        operation,
        configuration_digest,
        request,
        durable_operation_id,
    )?;
    let result = result.ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    prepared.complete(
        context,
        committed_state_material,
        reconciliation,
        result,
        partial,
    )
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
    let partial = reconciliation_required.then_some((
        "application.retained.session-refresh.delivery-failed",
        "The session refresh committed, but required scheduler delivery failed.",
    ));
    retained_effect_outcome(
        context,
        operation,
        configuration_digest,
        request,
        durable_operation_id,
        durable_operation_id,
        ReconciliationState::Reconciled,
        Some(result),
        partial,
    )
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
        RetainedSurfaceEvidenceTerminalV1::Cancelled => {
            RetainedSurfaceExecutionErrorV1::Cancelled(CancellationStage::DuringRead)
        }
        RetainedSurfaceEvidenceTerminalV1::Conflict => RetainedSurfaceExecutionErrorV1::Conflict,
        RetainedSurfaceEvidenceTerminalV1::Denied
        | RetainedSurfaceEvidenceTerminalV1::NotFoundOrNotAuthorized => {
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        }
        RetainedSurfaceEvidenceTerminalV1::TimedOut => {
            RetainedSurfaceExecutionErrorV1::TimedOut(CancellationStage::DuringRead)
        }
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
            UtcMicros(i64::MAX - 1),
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
        assert_eq!(
            effect.execution.effective_deadline.expires_at,
            UtcMicros(i64::MAX - 1),
            "the earlier capability-grant expiry must bound the receipt"
        );
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
