use tracedecay_application::feedback::FeedbackCompletedPublicationV1;
use tracedecay_application::{
    CancellationObservation, CancellationStage, CoverageCompleteness, CoverageDomainState,
    EvidenceAuthority, EvidenceCoverage, EvidenceDomain, EvidenceIdentity, FreshnessState,
    Omission, OmissionReason, OpaqueCursor, OperationBudgetUsage, PageCursor, PageState,
    RequestAdmission, RequestContext, RetrievalEvidence, RetrievalPortOutcome, TemporalState,
};
use tracedecay_domain::{ComponentVersion, UtcMicros};
use tracedecay_tool_catalog::SortContractId;

const FEEDBACK_SORT_CONTRACT_ID: &str = "sort.application.feedback.finding-id.v1";

pub(super) fn interruption<T>(
    context: &RequestContext,
    finished_at: UtcMicros,
    domains: Vec<EvidenceDomain>,
) -> Option<RetrievalPortOutcome<T>> {
    match context.admission_at(finished_at) {
        RequestAdmission::Admitted => None,
        RequestAdmission::Cancelled => Some(terminal_interruption(
            finished_at,
            domains,
            OmissionReason::Cancelled,
            false,
        )),
        RequestAdmission::TimedOut => Some(terminal_interruption(
            finished_at,
            domains,
            OmissionReason::TimedOut,
            true,
        )),
    }
}

pub(super) fn complete<T>(
    payload: T,
    publications: Vec<&FeedbackCompletedPublicationV1>,
    domains: Vec<EvidenceDomain>,
    page: Option<(u64, Option<OpaqueCursor>)>,
    expires_at: Option<UtcMicros>,
    finished_at: UtcMicros,
) -> RetrievalPortOutcome<T> {
    let coverage_returned = if page.is_some() {
        publications.len() as u64
    } else {
        1
    };
    let Ok(coverage) = EvidenceCoverage::complete(
        domains.clone(),
        coverage_returned,
        coverage_returned,
        coverage_returned,
    ) else {
        return unavailable(finished_at, domains);
    };
    let page = match page {
        Some((total, cursor)) => PageState {
            sort_contract_id: feedback_sort_contract(),
            sort_revision: 1,
            total: Some(total),
            returned: coverage_returned,
            cursor: cursor.map(|cursor| PageCursor::Opaque { cursor }),
            expires_at,
        },
        None => match PageState::first_page(feedback_sort_contract(), 1, Some(1), 1) {
            Ok(page) => page,
            Err(_) => return unavailable(finished_at, domains),
        },
    };
    RetrievalPortOutcome::Completed(RetrievalEvidence {
        payload: Some(payload),
        temporal: TemporalState::current(finished_at),
        evidence_authorities: publications.into_iter().map(evidence_authority).collect(),
        coverage,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page,
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    })
}

pub(super) fn unavailable<T>(
    finished_at: UtcMicros,
    mut domains: Vec<EvidenceDomain>,
) -> RetrievalPortOutcome<T> {
    domains.sort_unstable();
    domains.dedup();
    let coverage = EvidenceCoverage {
        requested_domains: domains.clone(),
        visited: None,
        eligible: None,
        returned: 0,
        completeness: CoverageCompleteness::Unknown,
        domains: domains
            .iter()
            .copied()
            .map(|domain| CoverageDomainState {
                domain,
                completeness: CoverageCompleteness::Unknown,
            })
            .collect(),
    };
    RetrievalPortOutcome::Unavailable(RetrievalEvidence {
        payload: None,
        temporal: TemporalState {
            freshness: FreshnessState::Unknown,
            ..TemporalState::current(finished_at)
        },
        evidence_authorities: Vec::new(),
        coverage,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState {
            sort_contract_id: feedback_sort_contract(),
            sort_revision: 1,
            total: None,
            returned: 0,
            cursor: None,
            expires_at: None,
        },
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    })
}

fn terminal_interruption<T>(
    finished_at: UtcMicros,
    mut domains: Vec<EvidenceDomain>,
    reason: OmissionReason,
    timed_out: bool,
) -> RetrievalPortOutcome<T> {
    domains.sort_unstable();
    domains.dedup();
    let evidence = RetrievalEvidence {
        payload: None,
        temporal: TemporalState {
            freshness: FreshnessState::Unknown,
            ..TemporalState::current(finished_at)
        },
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage {
            requested_domains: domains.clone(),
            visited: None,
            eligible: None,
            returned: 0,
            completeness: CoverageCompleteness::Unknown,
            domains: domains
                .iter()
                .copied()
                .map(|domain| CoverageDomainState {
                    domain,
                    completeness: CoverageCompleteness::Unknown,
                })
                .collect(),
        },
        omissions: domains
            .into_iter()
            .map(|domain| Omission {
                domain,
                count: 0,
                reason,
            })
            .collect(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState {
            sort_contract_id: feedback_sort_contract(),
            sort_revision: 1,
            total: None,
            returned: 0,
            cursor: None,
            expires_at: None,
        },
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: finished_at,
        }),
    };
    if timed_out {
        RetrievalPortOutcome::TimedOut(evidence)
    } else {
        RetrievalPortOutcome::Cancelled(evidence)
    }
}

fn evidence_authority(publication: &FeedbackCompletedPublicationV1) -> EvidenceAuthority {
    EvidenceAuthority {
        evidence_id: EvidenceIdentity::new(format!(
            "feedback-publication.{}",
            publication.result.result_id.as_str()
        ))
        .unwrap_or_else(|_| {
            panic!("validated feedback result id yields a valid evidence identity")
        }),
        source_kind: "canonical_feedback_publication".to_owned(),
        producer: "feedback_cycle".to_owned(),
        scope: publication.authorized_scope.clone(),
        revision: ComponentVersion::new("feedback.read.v1")
            .unwrap_or_else(|_| panic!("static feedback reader revision is valid")),
        horizon: Some(publication.authority.revalidated_at),
    }
}

fn feedback_sort_contract() -> SortContractId {
    SortContractId::new(FEEDBACK_SORT_CONTRACT_ID)
        .unwrap_or_else(|_| panic!("static feedback sort contract is valid"))
}
