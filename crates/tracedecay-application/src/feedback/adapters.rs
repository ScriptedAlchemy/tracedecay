//! Concrete composition adapters for the feedback ports.
//!
//! These adapters deliberately translate between existing diagnostic and
//! retrieval contracts. They do not own a diagnostic history, graph, test map,
//! provider lifecycle, or persistence path.

use tracedecay_domain::feedback::{
    FeedbackBaselineStateV1, FeedbackContentIdentityV1, FeedbackDiagnosticBaselineIdentityV1,
    FeedbackDiagnosticBaselineV1, FeedbackDiagnosticV1, FeedbackDurabilityV1,
    FeedbackEvaluationInputV1,
};
use tracedecay_domain::{GenerationDiagnosticV1, RetrievalAnchorId};

use super::ports::{FeedbackDiagnosticsPort, FeedbackDiagnosticsRequest, FeedbackRuntimeStateV1};
use crate::context::{RequestAdmission, RequestContext};
use crate::diagnostics::{
    AnalyzerAdmittedDiagnosticProviderV1, CurrentDiagnosticsRequest, DiagnosticProviderIdentity,
    DiagnosticProviderPort, DiagnosticProviderResult, DiagnosticProviderState,
    GenerationDiagnosticHistoryPort, GenerationDiagnosticHistoryRequest,
};
use crate::error::ApplicationContractError;

/// Builds the one canonical baseline identity shared by feedback orchestration
/// and the generation-bound diagnostics adapter. Keeping this calculation in
/// one place prevents a store adapter from silently changing comparison scope.
pub(crate) fn feedback_baseline_identity(
    input: &FeedbackEvaluationInputV1,
    runtime: &FeedbackRuntimeStateV1,
    provider: &DiagnosticProviderIdentity,
) -> Result<FeedbackDiagnosticBaselineIdentityV1, ApplicationContractError> {
    let FeedbackContentIdentityV1::SavedContent {
        generation_digest,
        file_digest,
    } = &input.request.content
    else {
        return Err(ApplicationContractError::Inconsistent {
            field: "overlay feedback baseline request",
        });
    };
    Ok(FeedbackDiagnosticBaselineIdentityV1 {
        current_generation_id: input.target.generation_id.clone().ok_or(
            ApplicationContractError::Inconsistent {
                field: "feedback baseline generation",
            },
        )?,
        current_generation_digest: generation_digest.clone(),
        current_head_commit_id: input.request.scope.head_commit_id.clone(),
        current_content_digest: file_digest.clone(),
        provider_identity_digest: provider.compute_digest()?,
        horizon: runtime.authoritative.baseline_horizon.clone().ok_or(
            ApplicationContractError::Inconsistent {
                field: "feedback baseline horizon",
            },
        )?,
    })
}

/// Generation-bound diagnostic/history composition over an already-owned
/// provider/store port. Analyzer admission is supplied per canonical provider
/// identity and gates reads before the source port is called.
pub struct GenerationBoundFeedbackDiagnosticsAdapter<P> {
    source: P,
    providers: Vec<AnalyzerAdmittedDiagnosticProviderV1>,
}

impl<P> GenerationBoundFeedbackDiagnosticsAdapter<P> {
    pub fn new(
        source: P,
        providers: Vec<AnalyzerAdmittedDiagnosticProviderV1>,
    ) -> Result<Self, ApplicationContractError> {
        for provider in &providers {
            provider.validate()?;
        }
        if providers.iter().enumerate().any(|(index, provider)| {
            providers[index.saturating_add(1)..]
                .iter()
                .any(|other| other.identity() == provider.identity())
        }) {
            return Err(ApplicationContractError::Duplicate {
                field: "analyzer-admitted diagnostic provider",
            });
        }
        Ok(Self { source, providers })
    }

    fn admission_for(
        &self,
        identity: &DiagnosticProviderIdentity,
    ) -> Option<&AnalyzerAdmittedDiagnosticProviderV1> {
        self.providers
            .iter()
            .find(|provider| provider.admits_identity(identity))
    }
}

impl<P> GenerationBoundFeedbackDiagnosticsAdapter<P>
where
    P: DiagnosticProviderPort + GenerationDiagnosticHistoryPort + Sync,
{
    async fn current_result(
        &self,
        context: &RequestContext,
        input: &FeedbackEvaluationInputV1,
        expected: &DiagnosticProviderIdentity,
    ) -> DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>> {
        let Some(admission) = self.admission_for(expected) else {
            return provider_result(expected.clone(), DiagnosticProviderState::Absent, None);
        };
        let admitted_state = admission.state();
        if admitted_state != DiagnosticProviderState::SupportedComplete {
            return provider_result(expected.clone(), admitted_state, None);
        }
        if !current_identity_matches_input(expected, input) {
            return provider_result(expected.clone(), DiagnosticProviderState::Unavailable, None);
        }

        let source = self
            .source
            .current_diagnostics(
                context,
                &CurrentDiagnosticsRequest {
                    identity: expected.clone(),
                },
            )
            .await;
        if source.validate().is_err() || source.identity != *expected {
            return provider_result(expected.clone(), DiagnosticProviderState::Failed, None);
        }
        let payload = match source.payload {
            Some(records) if current_records_match_input(&records, input) => Some(
                records
                    .into_iter()
                    .map(|record| FeedbackDiagnosticV1::Saved(Box::new(record)))
                    .collect(),
            ),
            Some(_) => {
                return provider_result(expected.clone(), DiagnosticProviderState::Failed, None);
            }
            None => None,
        };
        provider_result(expected.clone(), source.state, payload)
    }

    async fn history_result(
        &self,
        context: &RequestContext,
        input: &FeedbackEvaluationInputV1,
        runtime: &FeedbackRuntimeStateV1,
        expected: &DiagnosticProviderIdentity,
    ) -> Option<FeedbackDiagnosticBaselineV1> {
        let horizon = runtime.authoritative.baseline_horizon.as_ref()?;
        let identity = feedback_baseline_identity(input, runtime, expected).ok()?;
        let Some(admission) = self.admission_for(expected) else {
            return Some(baseline(
                identity,
                Vec::new(),
                FeedbackBaselineStateV1::Unavailable,
            ));
        };
        let admitted_state = admission.state();
        if admitted_state != DiagnosticProviderState::SupportedComplete {
            return Some(baseline(
                identity,
                Vec::new(),
                baseline_state_for_provider(admitted_state),
            ));
        }
        if !current_identity_matches_input(expected, input) {
            return Some(baseline(
                identity,
                Vec::new(),
                FeedbackBaselineStateV1::Unavailable,
            ));
        }

        let source = self
            .source
            .diagnostics_for_generation(
                context,
                &GenerationDiagnosticHistoryRequest {
                    identity: expected.clone(),
                    generation: horizon.comparison_generation_id.clone(),
                    file: input.target.file.clone(),
                },
            )
            .await;
        if source.validate().is_err() || source.identity != *expected {
            return Some(baseline(
                identity,
                Vec::new(),
                FeedbackBaselineStateV1::Partial,
            ));
        }
        let state = baseline_state_for_provider(source.state);
        let anchors = match source.payload {
            Some(records)
                if historical_records_match_input(
                    &records,
                    input,
                    &horizon.comparison_generation_id,
                ) =>
            {
                let mut anchors = records
                    .into_iter()
                    .map(|record| record.diagnostic_anchor)
                    .collect::<Vec<_>>();
                anchors.sort();
                if anchors.windows(2).any(|pair| pair[0] == pair[1]) {
                    return Some(baseline(
                        identity,
                        Vec::new(),
                        FeedbackBaselineStateV1::Partial,
                    ));
                }
                anchors
            }
            Some(_) => {
                return Some(baseline(
                    identity,
                    Vec::new(),
                    FeedbackBaselineStateV1::Partial,
                ));
            }
            None => Vec::new(),
        };
        Some(baseline(identity, anchors, state))
    }
}

impl<P> FeedbackDiagnosticsPort for GenerationBoundFeedbackDiagnosticsAdapter<P>
where
    P: DiagnosticProviderPort + GenerationDiagnosticHistoryPort + Sync,
{
    fn diagnostics<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a FeedbackDiagnosticsRequest,
    ) -> super::FeedbackPortFuture<'a, Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>>
    {
        Box::pin(async move {
            if request.validate().is_err() {
                return Vec::new();
            }
            let interrupted_state = match context.admission_at(request.input.observed_at) {
                RequestAdmission::Admitted => None,
                RequestAdmission::Cancelled => Some(DiagnosticProviderState::Cancelled),
                RequestAdmission::TimedOut => Some(DiagnosticProviderState::TimedOut),
            };
            if let Some(state) = interrupted_state {
                return request
                    .providers
                    .iter()
                    .cloned()
                    .map(|provider| provider_result(provider, state, None))
                    .collect();
            }
            let mut results = Vec::with_capacity(request.providers.len());
            for provider in &request.providers {
                results.push(self.current_result(context, &request.input, provider).await);
            }
            results
        })
    }

    fn diagnostic_history<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a FeedbackDiagnosticsRequest,
        runtime: &'a FeedbackRuntimeStateV1,
    ) -> super::FeedbackPortFuture<'a, Vec<FeedbackDiagnosticBaselineV1>> {
        Box::pin(async move {
            if request.validate().is_err()
                || request.input.request.durability() != FeedbackDurabilityV1::Durable
                || runtime.authoritative.baseline_horizon.is_none()
                || context.admission_at(request.input.observed_at) != RequestAdmission::Admitted
            {
                return Vec::new();
            }
            let mut baselines = Vec::with_capacity(request.providers.len());
            for provider in &request.providers {
                if let Some(result) = self
                    .history_result(context, &request.input, runtime, provider)
                    .await
                {
                    baselines.push(result);
                }
            }
            baselines
        })
    }
}

fn provider_result<T>(
    identity: DiagnosticProviderIdentity,
    state: DiagnosticProviderState,
    payload: Option<T>,
) -> DiagnosticProviderResult<T> {
    DiagnosticProviderResult::new(identity.clone(), state, payload).unwrap_or_else(|_| {
        DiagnosticProviderResult::new(identity, DiagnosticProviderState::Unavailable, None)
            .expect("unavailable diagnostic provider result is always valid")
    })
}

fn current_identity_matches_input(
    identity: &DiagnosticProviderIdentity,
    input: &FeedbackEvaluationInputV1,
) -> bool {
    input.request.durability() == FeedbackDurabilityV1::Durable
        && identity.source.clean_generation() == input.target.generation_id.as_ref()
        && identity.document.file == input.target.file
}

fn current_records_match_input(
    records: &[GenerationDiagnosticV1],
    input: &FeedbackEvaluationInputV1,
) -> bool {
    input
        .target
        .generation_id
        .as_ref()
        .is_some_and(|generation| {
            records.iter().all(|record| {
                record.validate().is_ok()
                    && record.is_current()
                    && record.generation_id == *generation
                    && record.file_occurrence_id == input.target.file
            })
        })
}

fn historical_records_match_input(
    records: &[GenerationDiagnosticV1],
    input: &FeedbackEvaluationInputV1,
    generation: &tracedecay_domain::CodeGenerationId,
) -> bool {
    records.iter().all(|record| {
        record.validate().is_ok()
            && record.generation_id == *generation
            && record.file_occurrence_id == input.target.file
    })
}

fn baseline_state_for_provider(state: DiagnosticProviderState) -> FeedbackBaselineStateV1 {
    match state {
        DiagnosticProviderState::SupportedComplete => FeedbackBaselineStateV1::Complete,
        DiagnosticProviderState::Stale => FeedbackBaselineStateV1::Stale,
        DiagnosticProviderState::Partial
        | DiagnosticProviderState::Cancelled
        | DiagnosticProviderState::TimedOut
        | DiagnosticProviderState::Failed
        | DiagnosticProviderState::Indexing => FeedbackBaselineStateV1::Partial,
        DiagnosticProviderState::Unsupported
        | DiagnosticProviderState::Absent
        | DiagnosticProviderState::Unavailable => FeedbackBaselineStateV1::Unavailable,
    }
}

fn baseline(
    identity: FeedbackDiagnosticBaselineIdentityV1,
    diagnostic_anchors: Vec<RetrievalAnchorId>,
    state: FeedbackBaselineStateV1,
) -> FeedbackDiagnosticBaselineV1 {
    FeedbackDiagnosticBaselineV1 {
        identity,
        diagnostic_anchors,
        state,
    }
}
