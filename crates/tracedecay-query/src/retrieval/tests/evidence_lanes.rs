use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tracedecay_application::CancellationSignal;
use tracedecay_domain::{
    CodeGenerationId, CursorPayloadDigest, FreshnessCompatibilityV1, ManifestDigest,
    RetrievalFailure, RetrieverBatch, RetrieverContinuation, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome, UtcMicros,
};

use crate::retrieval::evidence_lanes::{
    DiagnosticCandidateReadPortV1, DiagnosticLaneEvidenceV1, DiagnosticLaneRequestV1,
    DiagnosticLaneRetrieverV1, EvidenceLaneExecutionControlV1, TaskSessionCandidateReadPortV1,
    TaskSessionLaneEvidenceV1, TaskSessionLaneRequestV1, TaskSessionLaneRetrieverV1,
    TemporalCandidateChannelV1, TemporalCandidateExportPortV1, TemporalLaneEvidenceV1,
    TemporalLaneRequestV1, TemporalLaneRetrieverV1,
};
use crate::retrieval::ports::RetrievalPortError;
use crate::retrieval::request::RawRetrievalRequestV1;

use super::{candidate, freshness, id, request};

struct CountingEvidencePort {
    calls: Arc<AtomicUsize>,
}

fn empty<E>() -> Result<RetrieverOutcome<RetrieverBatch<E>>, RetrievalPortError> {
    Ok(RetrieverOutcome::Complete(RetrieverBatch {
        candidates: Vec::new(),
        evidence_by_occurrence: Default::default(),
        coverage: RetrieverCoverage::default(),
        continuation: None,
    }))
}

impl TemporalCandidateExportPortV1 for CountingEvidencePort {
    fn export_temporal_candidates(
        &self,
        _request: &TemporalLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TemporalLaneEvidenceV1>>, RetrievalPortError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        empty()
    }
}

impl TaskSessionCandidateReadPortV1 for CountingEvidencePort {
    fn read_task_session_candidates(
        &self,
        _request: &TaskSessionLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TaskSessionLaneEvidenceV1>>, RetrievalPortError>
    {
        self.calls.fetch_add(1, Ordering::AcqRel);
        empty()
    }
}

impl DiagnosticCandidateReadPortV1 for CountingEvidencePort {
    fn read_diagnostic_candidates(
        &self,
        _request: &DiagnosticLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<DiagnosticLaneEvidenceV1>>, RetrievalPortError>
    {
        self.calls.fetch_add(1, Ordering::AcqRel);
        empty()
    }
}

struct FixedTemporalPort {
    outcome: RetrieverOutcome<RetrieverBatch<TemporalLaneEvidenceV1>>,
}

impl TemporalCandidateExportPortV1 for FixedTemporalPort {
    fn export_temporal_candidates(
        &self,
        _request: &TemporalLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TemporalLaneEvidenceV1>>, RetrievalPortError> {
        Ok(self.outcome.clone())
    }
}

struct FixedDiagnosticPort {
    outcome: RetrieverOutcome<RetrieverBatch<DiagnosticLaneEvidenceV1>>,
}

impl DiagnosticCandidateReadPortV1 for FixedDiagnosticPort {
    fn read_diagnostic_candidates(
        &self,
        _request: &DiagnosticLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<DiagnosticLaneEvidenceV1>>, RetrievalPortError>
    {
        Ok(self.outcome.clone())
    }
}

#[test]
fn deadline_and_cancellation_stop_every_evidence_lane_before_source_reads() {
    let calls = Arc::new(AtomicUsize::new(0));
    let cancelled = CancellationSignal::active("cancel.evidence-lanes").expect("signal");
    cancelled.cancel(UtcMicros(5));
    let cancelled_control = EvidenceLaneExecutionControlV1::new(None, cancelled);
    let deadline_control = EvidenceLaneExecutionControlV1::new(
        Some(Instant::now()),
        CancellationSignal::active("cancel.evidence-deadline").expect("signal"),
    );
    let port = CountingEvidencePort {
        calls: Arc::clone(&calls),
    };
    let raw = RawRetrievalRequestV1::new("needle".to_owned(), request())
        .sanitize(id("sanitizer.fixture.v1"), id("normalization.fixture.v1"))
        .expect("sanitized request");
    let temporal = TemporalLaneRetrieverV1::new(&port);
    let task_session = TaskSessionLaneRetrieverV1::new(&port);
    let diagnostic = DiagnosticLaneRetrieverV1::new(&port);

    assert!(matches!(
        temporal
            .execute(&TemporalLaneRequestV1::new(
                raw.request(),
                raw.query_view(),
                id("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                &cancelled_control,
            ))
            .expect("typed temporal outcome"),
        RetrieverOutcome::Cancelled
    ));
    assert!(matches!(
        task_session
            .execute(&TaskSessionLaneRequestV1::new(
                raw.request(),
                raw.query_view(),
                id("task.fixture"),
                id("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                &cancelled_control,
            ))
            .expect("typed task outcome"),
        RetrieverOutcome::Cancelled
    ));
    assert!(matches!(
        diagnostic
            .execute(&DiagnosticLaneRequestV1::new(
                raw.request(),
                raw.query_view(),
                id("generation.fixture"),
                &deadline_control,
            ))
            .expect("typed diagnostic outcome"),
        RetrieverOutcome::TimedOut(_)
    ));
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[test]
fn canonical_evidence_lanes_are_independent_from_the_query_fallback_set() {
    assert_eq!(RetrieverKind::Temporal.as_str(), "temporal");
    assert_eq!(RetrieverKind::TaskSession.as_str(), "task_session");
    assert_eq!(RetrieverKind::Diagnostic.as_str(), "diagnostic");
    for lane in [
        RetrieverKind::Temporal,
        RetrieverKind::TaskSession,
        RetrieverKind::Diagnostic,
    ] {
        assert!(!lane.is_query_fallback_lane());
    }
}

#[test]
fn temporal_lane_preserves_authenticated_epoch_continuation_and_explanation() {
    let base = request();
    let raw = RawRetrievalRequestV1::new("prior decision".to_owned(), base)
        .sanitize(id("sanitizer.fixture.v1"), id("normalization.fixture.v1"))
        .expect("sanitized request");
    let control = EvidenceLaneExecutionControlV1::new(
        None,
        CancellationSignal::active("cancel.temporal-lane").expect("signal"),
    );
    let participant_epoch: ManifestDigest =
        id("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let mut compact = candidate(RetrieverKind::Temporal, "temporal", 810_000, 0);
    compact.session_or_thread_id = Some(id("session.fixture"));
    compact.freshness = freshness("namespace.session", "source.claude");
    let evidence = TemporalLaneEvidenceV1 {
        candidate_anchor: compact.anchor_id.clone(),
        source_occurrence: compact.source_occurrence_id.clone(),
        authorization_revision: raw.request().snapshot.authorization_revision.clone(),
        participant_epoch: participant_epoch.clone(),
        session_id: id("session.fixture"),
        source_id: "claude".to_owned(),
        hydration_anchor: compact.retriever_evidence_anchor.clone(),
        channels: vec![
            TemporalCandidateChannelV1::ExactMessage,
            TemporalCandidateChannelV1::Summary,
        ],
    };
    let port = FixedTemporalPort {
        outcome: RetrieverOutcome::Complete(RetrieverBatch {
            evidence_by_occurrence: BTreeMap::from([(
                compact.source_occurrence_id.clone(),
                evidence,
            )]),
            candidates: vec![compact],
            coverage: RetrieverCoverage {
                examined: 3,
                eligible: 1,
                excluded: 1,
                capped: 1,
                unknown: 0,
            },
            continuation: Some(RetrieverContinuation {
                lane: RetrieverKind::Temporal,
                checkpoint_digest: id::<CursorPayloadDigest>(
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                ),
                exhausted: false,
            }),
        }),
    };

    let outcome = TemporalLaneRetrieverV1::new(&port)
        .execute(&TemporalLaneRequestV1::new(
            raw.request(),
            raw.query_view(),
            participant_epoch,
            &control,
        ))
        .expect("temporal lane");
    let RetrieverOutcome::Complete(batch) = outcome else {
        panic!("temporal lane must remain complete");
    };
    assert_eq!(batch.candidates[0].retriever, RetrieverKind::Temporal);
    assert_eq!(
        batch.continuation.expect("bounded continuation").lane,
        RetrieverKind::Temporal
    );
    assert_eq!(
        batch
            .evidence_by_occurrence
            .values()
            .next()
            .expect("evidence")
            .channels,
        [
            TemporalCandidateChannelV1::ExactMessage,
            TemporalCandidateChannelV1::Summary,
        ],
    );
}

#[test]
fn evidence_lanes_reject_authorization_epoch_and_lane_substitution() {
    let base = request();
    let raw = RawRetrievalRequestV1::new("E0308".to_owned(), base)
        .sanitize(id("sanitizer.fixture.v1"), id("normalization.fixture.v1"))
        .expect("sanitized request");
    let control = EvidenceLaneExecutionControlV1::new(
        None,
        CancellationSignal::active("cancel.diagnostic-lane").expect("signal"),
    );
    let generation: CodeGenerationId = id("generation.fixture");
    let compact = candidate(RetrieverKind::Lexical, "diagnostic", 990_000, 0);
    let evidence = DiagnosticLaneEvidenceV1 {
        candidate_anchor: compact.anchor_id.clone(),
        source_occurrence: compact.source_occurrence_id.clone(),
        authorization_revision: id("authorization.foreign"),
        generation: generation.clone(),
        provider: id("provider.rustc"),
        file: id("file.fixture"),
        diagnostic_anchor: compact.retriever_evidence_anchor.clone(),
    };
    let port = FixedDiagnosticPort {
        outcome: RetrieverOutcome::Complete(RetrieverBatch {
            evidence_by_occurrence: BTreeMap::from([(
                compact.source_occurrence_id.clone(),
                evidence,
            )]),
            candidates: vec![compact],
            coverage: RetrieverCoverage::default(),
            continuation: None,
        }),
    };

    let error = DiagnosticLaneRetrieverV1::new(&port)
        .execute(&DiagnosticLaneRequestV1::new(
            raw.request(),
            raw.query_view(),
            generation,
            &control,
        ))
        .expect_err("foreign authorization and lexical substitution must fail");
    assert!(matches!(error, RetrievalPortError::Contract(_)));
}

#[test]
fn temporal_lane_rejects_non_authoritative_hydration_anchor() {
    let base = request();
    let raw = RawRetrievalRequestV1::new("exact retained bytes".to_owned(), base)
        .sanitize(id("sanitizer.fixture.v1"), id("normalization.fixture.v1"))
        .expect("sanitized request");
    let control = EvidenceLaneExecutionControlV1::new(
        None,
        CancellationSignal::active("cancel.temporal-hydration").expect("signal"),
    );
    let participant_epoch: ManifestDigest =
        id("sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd");
    let compact = candidate(RetrieverKind::Temporal, "hydration", 900_000, 0);
    let evidence = TemporalLaneEvidenceV1 {
        candidate_anchor: compact.anchor_id.clone(),
        source_occurrence: compact.source_occurrence_id.clone(),
        authorization_revision: raw.request().snapshot.authorization_revision.clone(),
        participant_epoch: participant_epoch.clone(),
        session_id: id("session.fixture"),
        source_id: "claude".to_owned(),
        hydration_anchor: id("anchor.unrelated-payload"),
        channels: vec![TemporalCandidateChannelV1::ExactMessage],
    };
    let port = FixedTemporalPort {
        outcome: RetrieverOutcome::Complete(RetrieverBatch {
            evidence_by_occurrence: BTreeMap::from([(
                compact.source_occurrence_id.clone(),
                evidence,
            )]),
            candidates: vec![compact],
            coverage: RetrieverCoverage::default(),
            continuation: None,
        }),
    };

    let error = TemporalLaneRetrieverV1::new(&port)
        .execute(&TemporalLaneRequestV1::new(
            raw.request(),
            raw.query_view(),
            participant_epoch,
            &control,
        ))
        .expect_err("hydration must remain on the candidate's canonical source anchor");
    assert!(matches!(error, RetrievalPortError::Contract(_)));
}

#[test]
fn diagnostic_partial_and_stale_states_are_not_fabricated_as_empty_success() {
    let base = request();
    let raw = RawRetrievalRequestV1::new("warning".to_owned(), base)
        .sanitize(id("sanitizer.fixture.v1"), id("normalization.fixture.v1"))
        .expect("sanitized request");
    let control = EvidenceLaneExecutionControlV1::new(
        None,
        CancellationSignal::active("cancel.partial-diagnostic").expect("signal"),
    );
    let generation: CodeGenerationId = id("generation.fixture");
    let port = FixedDiagnosticPort {
        outcome: RetrieverOutcome::Partial {
            value: RetrieverBatch {
                candidates: Vec::new(),
                evidence_by_occurrence: BTreeMap::new(),
                coverage: RetrieverCoverage {
                    unknown: 2,
                    ..RetrieverCoverage::default()
                },
                continuation: None,
            },
            reason: RetrievalFailure::StaleSource,
        },
    };

    let outcome = DiagnosticLaneRetrieverV1::new(&port)
        .execute(&DiagnosticLaneRequestV1::new(
            raw.request(),
            raw.query_view(),
            generation,
            &control,
        ))
        .expect("typed partial outcome");
    assert!(matches!(
        outcome,
        RetrieverOutcome::Partial {
            reason: RetrievalFailure::StaleSource,
            ..
        }
    ));
    let mut stale = freshness("namespace.diagnostic", "provider.rustc");
    stale.compatibility = FreshnessCompatibilityV1::Stale;
    assert!(matches!(
        RetrieverOutcome::<RetrieverBatch<DiagnosticLaneEvidenceV1>>::Stale(stale),
        RetrieverOutcome::Stale(_)
    ));
}
