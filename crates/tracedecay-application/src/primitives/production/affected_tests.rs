//! Affected-tests retrieval port and its attribution evidence.

use std::sync::Arc;

use tracedecay_code_index::provider::{
    GenerationProviderCoverageV1, GenerationProviderReadV1, GenerationTestAttributionJoinReadPort,
};
use tracedecay_code_index::test_attribution::{
    GenerationTestJoinCoverageV1, GenerationTestJoinDispositionV1, GenerationTestJoinV1,
};
use tracedecay_contracts::retrieval::{
    AffectedTestAttributionV1, AffectedTestsRequest, AffectedTestsResult, RetrievalPortContext,
    RetrievalPortOutcome,
};
use tracedecay_contracts::{
    CoverageCompleteness, CoverageDomainState, EvidenceAuthority, EvidenceCoverage, EvidenceDomain,
    EvidenceIdentity, FreshnessState, Omission, OmissionReason, OperationBudgetUsage,
    ResolvedScope, RetrievalEvidence, TemporalState,
};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, ProviderEvaluationStateV1, UtcMicros,
};

use super::{empty_primitive_page, now_observed, primitive_page};

pub struct TraceDecayAffectedTestsPortV1 {
    project_id: Option<ProjectId>,
    attribution: Option<Arc<dyn GenerationTestAttributionJoinReadPort + Send + Sync>>,
}

impl TraceDecayAffectedTestsPortV1 {
    pub fn new(project_id: ProjectId, generation: CodeGenerationId) -> Self {
        Self::from_binding(Some(project_id), generation, None)
    }

    pub fn with_generation_attribution(
        project_id: ProjectId,
        generation: CodeGenerationId,
        attribution: Arc<dyn GenerationTestAttributionJoinReadPort + Send + Sync>,
    ) -> Self {
        Self::from_binding(Some(project_id), generation, Some(attribution))
    }

    pub(super) fn from_binding(
        project_id: Option<ProjectId>,
        _generation: CodeGenerationId,
        attribution: Option<Arc<dyn GenerationTestAttributionJoinReadPort + Send + Sync>>,
    ) -> Self {
        Self {
            project_id,
            attribution,
        }
    }
}

impl tracedecay_contracts::AffectedTestsRetrievalPort for TraceDecayAffectedTestsPortV1 {
    #[hotpath::measure(label = "usecases.primitives.affected_tests")]
    fn affected_tests(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &AffectedTestsRequest,
    ) -> RetrievalPortOutcome<AffectedTestsResult> {
        let finished_at = now_observed();
        if self.project_id.as_ref() != Some(&context.request.scope().project_id) {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Unavailable,
                FreshnessState::Unknown,
            );
        }
        let Some(attribution) = &self.attribution else {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Unavailable,
                FreshnessState::Unknown,
            );
        };
        attributed_tests_outcome(
            request,
            context.request.scope().clone(),
            attribution.read_test_attribution(&request.generation),
            finished_at,
        )
    }
}

pub(super) fn attributed_tests_outcome(
    request: &AffectedTestsRequest,
    scope: ResolvedScope,
    read: GenerationProviderReadV1<GenerationTestJoinV1>,
    finished_at: UtcMicros,
) -> RetrievalPortOutcome<AffectedTestsResult> {
    if read.validate().is_err() {
        return affected_tests_unavailable(
            request,
            finished_at,
            OmissionReason::Failed,
            FreshnessState::Unknown,
        );
    }
    match read.provider_state {
        ProviderEvaluationStateV1::Cancelled => {
            return RetrievalPortOutcome::Cancelled(affected_tests_evidence(
                request,
                None,
                finished_at,
                CoverageCompleteness::Unknown,
                FreshnessState::Unknown,
                None,
                None,
                None,
                None,
                Some(OmissionReason::Cancelled),
            ));
        }
        ProviderEvaluationStateV1::TimedOut => {
            return RetrievalPortOutcome::TimedOut(affected_tests_evidence(
                request,
                None,
                finished_at,
                CoverageCompleteness::Unknown,
                FreshnessState::Unknown,
                None,
                None,
                None,
                None,
                Some(OmissionReason::TimedOut),
            ));
        }
        ProviderEvaluationStateV1::SupportedCompletedComplete
        | ProviderEvaluationStateV1::Partial => {}
        ProviderEvaluationStateV1::Stale => {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Stale,
                FreshnessState::Stale,
            );
        }
        ProviderEvaluationStateV1::Unsupported
        | ProviderEvaluationStateV1::Absent
        | ProviderEvaluationStateV1::Indexing
        | ProviderEvaluationStateV1::Failed
        | ProviderEvaluationStateV1::Unavailable => {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Unavailable,
                FreshnessState::Unknown,
            );
        }
    }

    let Some(join) = read.evidence else {
        return affected_tests_unavailable(
            request,
            finished_at,
            OmissionReason::Unavailable,
            FreshnessState::Unknown,
        );
    };
    if join.generation_id != request.generation
        || join.test_watermark.generation_id != request.generation
    {
        return affected_tests_unavailable(
            request,
            finished_at,
            OmissionReason::Stale,
            FreshnessState::Stale,
        );
    }

    let mut tests = Vec::new();
    let mut attributions = Vec::new();
    let mut matching_incomplete = false;
    for record in &join.records {
        if record.attribution.generation_id != request.generation {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Stale,
                FreshnessState::Stale,
            );
        }
        let covers_requested_symbol = record
            .attribution
            .covered_occurrences
            .contains(&request.symbol);
        if !covers_requested_symbol {
            continue;
        }
        if let GenerationTestJoinDispositionV1::Current { evidence_class } = &record.disposition {
            let Some(test_occurrence) = &record.test_occurrence else {
                return affected_tests_unavailable(
                    request,
                    finished_at,
                    OmissionReason::Failed,
                    FreshnessState::Unknown,
                );
            };
            if test_occurrence.occurrence_id != record.attribution.test_occurrence {
                return affected_tests_unavailable(
                    request,
                    finished_at,
                    OmissionReason::Failed,
                    FreshnessState::Unknown,
                );
            }
            tests.push(test_occurrence.occurrence_id.clone());
            attributions.push(AffectedTestAttributionV1 {
                test: test_occurrence.occurrence_id.clone(),
                evidence_class: *evidence_class,
            });
        } else {
            matching_incomplete = true;
            if matches!(
                record.disposition,
                GenerationTestJoinDispositionV1::StaleEvidence
                    | GenerationTestJoinDispositionV1::UnknownUnsupported
            ) && record.test_occurrence.as_ref().is_some_and(|occurrence| {
                occurrence.occurrence_id == record.attribution.test_occurrence
            }) {
                attributions.push(AffectedTestAttributionV1 {
                    test: record.attribution.test_occurrence.clone(),
                    evidence_class: record.attribution.evidence_class,
                });
            }
        }
    }
    tests.sort();
    tests.dedup();
    attributions.sort_by(|left, right| {
        (&left.test, left.evidence_class).cmp(&(&right.test, right.evidence_class))
    });
    attributions.dedup();

    let complete = read.provider_state == ProviderEvaluationStateV1::SupportedCompletedComplete
        && read.coverage.is_complete()
        && matches!(join.coverage, GenerationTestJoinCoverageV1::Complete)
        && !matching_incomplete;
    let (visited, eligible) = affected_tests_provider_counts(&read.coverage);
    if eligible.is_some_and(|eligible| tests.len() as u64 > eligible) {
        return affected_tests_unavailable(
            request,
            finished_at,
            OmissionReason::Failed,
            FreshnessState::Unknown,
        );
    }
    let evidence = affected_tests_evidence(
        request,
        Some(AffectedTestsResult {
            tests,
            attributions,
        }),
        finished_at,
        if complete {
            CoverageCompleteness::Complete
        } else {
            CoverageCompleteness::Partial
        },
        if complete {
            FreshnessState::Current
        } else {
            FreshnessState::Unknown
        },
        visited,
        eligible,
        Some(EvidenceAuthority {
            evidence_id: match EvidenceIdentity::new(format!(
                "evidence.test-attribution.{}",
                join.test_watermark
                    .evidence_digest
                    .as_str()
                    .trim_start_matches("sha256:")
            )) {
                Ok(identity) => identity,
                Err(_) => {
                    return affected_tests_unavailable(
                        request,
                        finished_at,
                        OmissionReason::Failed,
                        FreshnessState::Unknown,
                    );
                }
            },
            source_kind: "test_attribution".to_owned(),
            producer: "code_index".to_owned(),
            scope,
            revision: join.test_watermark.attribution_revision.clone(),
            horizon: None,
        }),
        Some(join.test_watermark.evidence_digest.clone()),
        (!complete).then_some(OmissionReason::Unavailable),
    );
    if complete {
        RetrievalPortOutcome::Completed(evidence)
    } else {
        RetrievalPortOutcome::Partial(evidence)
    }
}

pub(super) fn affected_tests_provider_counts(
    coverage: &GenerationProviderCoverageV1,
) -> (Option<u64>, Option<u64>) {
    match coverage {
        GenerationProviderCoverageV1::Complete {
            examined, eligible, ..
        }
        | GenerationProviderCoverageV1::Partial {
            examined, eligible, ..
        } => (Some(*examined), Some(*eligible)),
        GenerationProviderCoverageV1::Unavailable => (None, None),
    }
}

pub(super) fn affected_tests_unavailable(
    request: &AffectedTestsRequest,
    finished_at: UtcMicros,
    reason: OmissionReason,
    freshness: FreshnessState,
) -> RetrievalPortOutcome<AffectedTestsResult> {
    RetrievalPortOutcome::Unavailable(affected_tests_evidence(
        request,
        None,
        finished_at,
        CoverageCompleteness::Unknown,
        freshness,
        None,
        None,
        None,
        None,
        Some(reason),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn affected_tests_evidence(
    request: &AffectedTestsRequest,
    payload: Option<AffectedTestsResult>,
    finished_at: UtcMicros,
    completeness: CoverageCompleteness,
    freshness: FreshnessState,
    visited: Option<u64>,
    eligible: Option<u64>,
    evidence_authority: Option<EvidenceAuthority>,
    watermark_digest: Option<ManifestDigest>,
    omission: Option<OmissionReason>,
) -> RetrievalEvidence<AffectedTestsResult> {
    let returned = payload
        .as_ref()
        .map_or(0, |result| result.tests.len() as u64);
    RetrievalEvidence {
        payload,
        temporal: TemporalState {
            requested_mode: request.meta.temporal,
            requested_at: finished_at,
            resolved_at: finished_at,
            source_generation: Some(request.generation.clone()),
            code_graph_freshness: None,
            watermark_digest,
            freshness,
        },
        evidence_authorities: evidence_authority.into_iter().collect(),
        coverage: EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Test],
            visited,
            eligible,
            returned,
            completeness,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Test,
                completeness,
            }],
        },
        omissions: omission
            .map(|reason| Omission {
                domain: EvidenceDomain::Test,
                count: 0,
                reason,
            })
            .into_iter()
            .collect(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: primitive_page(eligible, returned).unwrap_or_else(|_| empty_primitive_page()),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    }
}
