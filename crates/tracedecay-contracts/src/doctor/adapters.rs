//! Doctor kernel read mappers and the resolved-input bundle.
//!
//! Pure functions map live daemon signals into typed kernel reads. The
//! composition root wires those reads into [`DoctorReportComposerV1`] through
//! the genuine source ports defined in [`super::sources`]. This module owns no
//! store, scheduler, or transport.
//!
//! [`DaemonRuntimeHealthSignalV1`] is the boundary type daemon writers fill.
//! Mapping it through [`runtime_health_read`] never imports daemon types.

use tracedecay_domain::CodeGenerationId;

use crate::feedback::FeedbackPublicationV1;

use super::sources::{
    AdvisoryFeedbackFindingReadV1, AdvisoryFeedbackReadV1, AdvisoryFeedbackSummaryReadV1,
    CodeIndexMountReadV1, ConfigurationAuthorityReadV1, DoctorStorageFamilyReadV1,
    DoctorStorageIncompleteReasonV1, HostIntegrationReadV1, IngestRefusalCensusReadV1,
    LanguageServerReadV1, ObservabilityReadV1, OperationalAuditReadV1, RuntimeHealthReadV1,
    RuntimeLivenessV1, SemanticOwnerReadV1,
};
use super::types::{DoctorCoverageCompletenessV1, DoctorStorageFindingV1};

/// The real daemon/runtime health signals a serving runtime writes.
///
/// This is the *writer-side* snapshot: the daemon fills each field from its
/// own startup and storage-authority probes. Each optional signal is `None`
/// when that probe has not run, so an undetermined signal weakens coverage
/// rather than being assumed healthy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DaemonRuntimeHealthSignalV1 {
    /// The daemon runtime is serving requests (its actors are alive).
    pub serving: bool,
    /// Schema migration and compatibility projections have converged.
    pub startup_converged: bool,
    /// The storage quick-check passed, when the daemon has run it.
    pub quick_check_ok: Option<bool>,
    /// The storage authority audit passed, when the daemon has run it.
    pub authority_audit_ok: Option<bool>,
    /// The session temporal projections are healthy, when determined.
    pub temporal_ok: Option<bool>,
}

/// Map a daemon runtime-health signal into its kernel read.
///
/// A runtime that is not serving is genuinely undetermined health, not a proven
/// degraded condition: it reports `Unreachable`. A serving runtime whose storage
/// authority signals prove a failure is `Stuck`; one that is serving but has not
/// converged is `Degraded`; one that is serving, converged, and clean is
/// `Healthy` — but only with complete coverage when every optional signal was
/// actually observed. A missing signal drops coverage to partial (an honest
/// "healthy so far as observed", never a healthy-complete claim).
#[must_use]
pub fn runtime_health_read(signal: &DaemonRuntimeHealthSignalV1) -> RuntimeHealthReadV1 {
    if !signal.serving {
        return RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Unreachable,
            coverage: DoctorCoverageCompletenessV1::Unknown,
        };
    }
    let proven_failure = signal.quick_check_ok == Some(false)
        || signal.authority_audit_ok == Some(false)
        || signal.temporal_ok == Some(false);
    if proven_failure {
        return RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Stuck,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
    }
    if !signal.startup_converged {
        return RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Degraded,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
    }
    let fully_observed = signal.quick_check_ok == Some(true)
        && signal.authority_audit_ok == Some(true)
        && signal.temporal_ok == Some(true);
    let coverage = if fully_observed {
        DoctorCoverageCompletenessV1::Complete
    } else {
        DoctorCoverageCompletenessV1::Partial
    };
    RuntimeHealthReadV1::Observed {
        liveness: RuntimeLivenessV1::Healthy,
        coverage,
    }
}

/// Project the latest exact-scope durable feedback publication into Doctor's
/// distinct advisory port. Host conformance remains a separate source.
#[must_use]
pub fn advisory_feedback_read_from_publication(
    publication: Option<&FeedbackPublicationV1>,
    current_generation: Option<&CodeGenerationId>,
) -> AdvisoryFeedbackReadV1 {
    let Some(publication) = publication else {
        return AdvisoryFeedbackReadV1::Absent;
    };
    if publication.validate().is_err() {
        return AdvisoryFeedbackReadV1::Unknown;
    }
    let Some(generation_id) = publication.input.target.generation_id.clone() else {
        return AdvisoryFeedbackReadV1::Unknown;
    };
    let generation_current = current_generation == Some(&generation_id);
    let summary = AdvisoryFeedbackSummaryReadV1 {
        result_id: publication.result.result_id.clone(),
        cycle_id: publication.result.cycle_id.clone(),
        scope: publication.result.scope.clone(),
        generation_id: generation_id.clone(),
        generation_current,
        termination: publication.result.termination,
        provider_states: publication.result.provider_states.clone(),
        total_findings: publication.result.total_findings,
        returned_findings: publication.result.returned_findings,
        omitted_findings: publication.result.omitted_findings,
    };
    let impact_anchors = publication
        .result
        .impact
        .as_ref()
        .map(|impact| impact.evidence_anchors.as_slice())
        .unwrap_or_default();
    let findings = publication
        .result
        .findings
        .iter()
        .map(|finding| {
            let mut evidence_anchors = finding
                .retrieval_anchor_id
                .iter()
                .cloned()
                .chain(impact_anchors.iter().cloned())
                .collect::<Vec<_>>();
            evidence_anchors.sort();
            evidence_anchors.dedup();
            AdvisoryFeedbackFindingReadV1 {
                result_id: publication.result.result_id.clone(),
                cycle_id: publication.result.cycle_id.clone(),
                finding_id: finding.finding_id.clone(),
                scope: publication.result.scope.clone(),
                generation_id: generation_id.clone(),
                generation_current,
                lifecycle: finding.lifecycle,
                provider_state: finding.provider_state,
                evidence_anchors,
                total_findings: publication.result.total_findings,
                returned_findings: publication.result.returned_findings,
                omitted_findings: publication.result.omitted_findings,
            }
        })
        .collect();
    AdvisoryFeedbackReadV1::Observed {
        summary: Box::new(summary),
        findings,
    }
}

/// Wrap a set of typed storage findings the retention producers emitted into a
/// kernel read.
///
/// An empty finding set is a typed [`DoctorStorageFamilyReadV1::Absent`] — the
/// runtime was consulted but produced nothing — never a fabricated healthy
/// claim; the composer classifies an empty observed read as absent regardless,
/// and this keeps the intent explicit at the source.
#[must_use]
pub fn storage_family_read(findings: Vec<DoctorStorageFindingV1>) -> DoctorStorageFamilyReadV1 {
    if findings.is_empty() {
        DoctorStorageFamilyReadV1::Absent
    } else {
        DoctorStorageFamilyReadV1::Observed { findings }
    }
}

/// Combine two independently consulted storage-family reads.
///
/// Findings accumulate. When either side is unresolved, coverage weakens to
/// the more severe incomplete reason rather than dropping observed findings.
#[must_use]
pub fn merge_storage_reads(
    first: DoctorStorageFamilyReadV1,
    second: DoctorStorageFamilyReadV1,
) -> DoctorStorageFamilyReadV1 {
    let (mut findings, first_incomplete) = storage_read_parts(first);
    let (second_findings, second_incomplete) = storage_read_parts(second);
    findings.extend(second_findings);
    let incomplete = first_incomplete.max(second_incomplete);

    match (findings.is_empty(), incomplete) {
        (false, Some(reason)) => DoctorStorageFamilyReadV1::ObservedIncomplete { findings, reason },
        (false, None) => storage_family_read(findings),
        (true, Some(DoctorStorageIncompleteReasonV1::Unsupported)) => {
            DoctorStorageFamilyReadV1::Unsupported
        }
        (true, Some(DoctorStorageIncompleteReasonV1::Denied)) => DoctorStorageFamilyReadV1::Denied,
        (true, Some(DoctorStorageIncompleteReasonV1::Unknown)) => {
            DoctorStorageFamilyReadV1::Unknown
        }
        (true, Some(DoctorStorageIncompleteReasonV1::Unavailable { detail })) => {
            DoctorStorageFamilyReadV1::Unavailable { detail }
        }
        (true, Some(DoctorStorageIncompleteReasonV1::ResetRequired { detail })) => {
            DoctorStorageFamilyReadV1::ResetRequired { detail }
        }
        (true, Some(DoctorStorageIncompleteReasonV1::Corrupt { detail })) => {
            DoctorStorageFamilyReadV1::Corrupt { detail }
        }
        (true, None) => DoctorStorageFamilyReadV1::Absent,
    }
}

fn storage_read_parts(
    read: DoctorStorageFamilyReadV1,
) -> (
    Vec<DoctorStorageFindingV1>,
    Option<DoctorStorageIncompleteReasonV1>,
) {
    match read {
        DoctorStorageFamilyReadV1::Observed { findings } => (findings, None),
        DoctorStorageFamilyReadV1::ObservedIncomplete { findings, reason } => {
            (findings, Some(reason))
        }
        DoctorStorageFamilyReadV1::Unsupported => (
            Vec::new(),
            Some(DoctorStorageIncompleteReasonV1::Unsupported),
        ),
        DoctorStorageFamilyReadV1::Absent => (Vec::new(), None),
        DoctorStorageFamilyReadV1::Denied => {
            (Vec::new(), Some(DoctorStorageIncompleteReasonV1::Denied))
        }
        DoctorStorageFamilyReadV1::Unknown => {
            (Vec::new(), Some(DoctorStorageIncompleteReasonV1::Unknown))
        }
        DoctorStorageFamilyReadV1::Unavailable { detail } => (
            Vec::new(),
            Some(DoctorStorageIncompleteReasonV1::Unavailable { detail }),
        ),
        DoctorStorageFamilyReadV1::ResetRequired { detail } => (
            Vec::new(),
            Some(DoctorStorageIncompleteReasonV1::ResetRequired { detail }),
        ),
        DoctorStorageFamilyReadV1::Corrupt { detail } => (
            Vec::new(),
            Some(DoctorStorageIncompleteReasonV1::Corrupt { detail }),
        ),
    }
}

/// The resolved kernel reads a Doctor report composes from.
///
/// The composition root builds this bundle from the real signals it can reach
/// and wires each read into [`DoctorReportComposerV1`]. A signal the surface
/// cannot obtain carries its honest typed absence rather than a fabricated
/// healthy read.
#[derive(Clone, Debug)]
pub struct DoctorKernelInputsV1 {
    /// Configuration-authority read (Configuration family).
    pub configuration: ConfigurationAuthorityReadV1,
    /// Daemon/runtime health read (`StorageRuntime` family).
    pub runtime: RuntimeHealthReadV1,
    /// Remote HTTPS and exact registered-profile operational authority.
    pub operational_audit: OperationalAuditReadV1,
    /// Host/agent integration conformance read (Advisory family).
    pub host: HostIntegrationReadV1,
    /// Mounted canonical feedback-owner read (Advisory family).
    pub advisory_feedback: AdvisoryFeedbackReadV1,
    /// Live language-server/analyzer read (`LanguageServer` family).
    pub language_server: LanguageServerReadV1,
    /// Code-index mount read (`SemanticIndex` family).
    pub code_index: CodeIndexMountReadV1,
    /// Independent semantic activation-owner read (`SemanticIndex` family).
    pub semantic_owner: SemanticOwnerReadV1,
    /// Canonical durable Plan-26 feedback read (`Observability` family).
    pub observability: ObservabilityReadV1,
    /// Durable ingest-coverage refusal census (`Observability` family).
    pub ingest_refusals: IngestRefusalCensusReadV1,
    /// Storage retention/size read (Storage family).
    pub storage: DoctorStorageFamilyReadV1,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::doctor::{
        DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
        DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
        DoctorStorageFamilyReadV1, DoctorStorageFindingKindV1, DoctorStorageFindingV1,
        DoctorStorageIncompleteReasonV1, RuntimeHealthReadV1, RuntimeLivenessV1,
    };

    use super::*;

    fn orphan_storage_finding() -> DoctorStorageFindingV1 {
        let evidence = DoctorEvidenceRefV1::new(
            DoctorFindingFamilyV1::Storage,
            DoctorEvidenceReferenceV1::new("storage.orphan_store.fixture.age-42d").unwrap(),
        );
        let coverage = DoctorCoverageStatementV1::new(
            DoctorCoverageCompletenessV1::Complete,
            "orphan store identity no longer resolves",
        )
        .unwrap();
        let finding = DoctorFindingV1::new(
            DoctorFindingFamilyV1::Storage,
            DoctorEvidenceStateV1::Degraded,
            vec![evidence],
            coverage,
        )
        .unwrap();
        DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OrphanStore, finding).unwrap()
    }

    #[test]
    fn runtime_healthy_requires_all_signals_observed_for_complete_coverage() {
        let healthy = DaemonRuntimeHealthSignalV1 {
            serving: true,
            startup_converged: true,
            quick_check_ok: Some(true),
            authority_audit_ok: Some(true),
            temporal_ok: Some(true),
        };
        assert_eq!(
            runtime_health_read(&healthy),
            RuntimeHealthReadV1::Observed {
                liveness: RuntimeLivenessV1::Healthy,
                coverage: DoctorCoverageCompletenessV1::Complete,
            }
        );
        for partial in [
            DaemonRuntimeHealthSignalV1 {
                temporal_ok: None,
                ..healthy
            },
            DaemonRuntimeHealthSignalV1 {
                authority_audit_ok: None,
                ..healthy
            },
        ] {
            assert_eq!(
                runtime_health_read(&partial),
                RuntimeHealthReadV1::Observed {
                    liveness: RuntimeLivenessV1::Healthy,
                    coverage: DoctorCoverageCompletenessV1::Partial,
                }
            );
        }
    }

    #[test]
    fn runtime_degraded_stuck_and_unreachable_are_honest() {
        let degraded = DaemonRuntimeHealthSignalV1 {
            serving: true,
            startup_converged: false,
            ..DaemonRuntimeHealthSignalV1::default()
        };
        assert_eq!(
            runtime_health_read(&degraded),
            RuntimeHealthReadV1::Observed {
                liveness: RuntimeLivenessV1::Degraded,
                coverage: DoctorCoverageCompletenessV1::Complete,
            }
        );
        let stuck = DaemonRuntimeHealthSignalV1 {
            serving: true,
            startup_converged: true,
            quick_check_ok: Some(false),
            ..DaemonRuntimeHealthSignalV1::default()
        };
        assert_eq!(
            runtime_health_read(&stuck),
            RuntimeHealthReadV1::Observed {
                liveness: RuntimeLivenessV1::Stuck,
                coverage: DoctorCoverageCompletenessV1::Complete,
            }
        );
        let unreachable = DaemonRuntimeHealthSignalV1::default();
        assert_eq!(
            runtime_health_read(&unreachable),
            RuntimeHealthReadV1::Observed {
                liveness: RuntimeLivenessV1::Unreachable,
                coverage: DoctorCoverageCompletenessV1::Unknown,
            }
        );
    }

    #[test]
    fn storage_family_read_absent_when_empty() {
        assert_eq!(
            storage_family_read(Vec::new()),
            DoctorStorageFamilyReadV1::Absent
        );
    }

    #[test]
    fn unresolved_storage_producers_preserve_findings_and_weaken_coverage() {
        for (unresolved, expected_reason) in [
            (
                DoctorStorageFamilyReadV1::Unsupported,
                DoctorStorageIncompleteReasonV1::Unsupported,
            ),
            (
                DoctorStorageFamilyReadV1::Denied,
                DoctorStorageIncompleteReasonV1::Denied,
            ),
            (
                DoctorStorageFamilyReadV1::Unknown,
                DoctorStorageIncompleteReasonV1::Unknown,
            ),
        ] {
            let observed = storage_family_read(vec![orphan_storage_finding()]);
            for merged in [
                merge_storage_reads(observed.clone(), unresolved.clone()),
                merge_storage_reads(unresolved, observed),
            ] {
                match merged {
                    DoctorStorageFamilyReadV1::ObservedIncomplete { findings, reason } => {
                        assert_eq!(reason, expected_reason);
                        assert_eq!(findings.len(), 1);
                        assert_eq!(findings[0].kind(), DoctorStorageFindingKindV1::OrphanStore);
                    }
                    other => panic!("expected incomplete observations, got {other:?}"),
                }
            }
        }
    }
}
