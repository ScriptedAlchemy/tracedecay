//! Typed retained-memory receipt assembly.

use serde::Serialize;
use tracedecay_application::retained_surfaces::{
    RetainedSurfaceEvidenceTerminalV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
    SessionCoverageModeV1,
};
use tracedecay_application::{
    ApplicationOutcome, AuthorityReceipt, EvidenceAuthority, EvidenceCoverage, EvidenceIdentity,
    EvidencePacket, Omission, OpaqueCursor, OperationBudgetUsage, OperationReceipt, PageState,
    PolicyDecisionRef, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    TemporalState, now_micros,
};
use tracedecay_domain::{ComponentVersion, TemporalModeV1, canonical_sha256};
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
