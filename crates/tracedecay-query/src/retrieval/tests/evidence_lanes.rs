use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tracedecay_application::{CancellationSignal, VerifiedWorkGraphVersionV1};
use tracedecay_domain::{
    AttemptId, CodeGenerationId, ComponentVersion, ContentDigest, CursorPayloadDigest,
    DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
    DiagnosticRecordStateV1, DiagnosticSeverityV1, FreshnessCompatibilityV1,
    GenerationDiagnosticV1, ManifestDigest, ObservationSourceIdentityV1, ProviderId,
    RetrievalFailure, RetrievalGrainV1, RetrieverBatch, RetrieverContinuation, RetrieverCoverage,
    RetrieverKind, RetrieverOutcome, RunId, SessionId, SourceSpan, SourceStoreId, TaskId,
    TemporalModeV1, UtcMicros, WorkAttemptIdentityV1, WorkGraphVersionV1,
    WorkProductEventSequenceV1, WorkProductSourceWatermarkV1,
};
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, TemporalExecutionSnapshot, TemporalParticipantAuthorization,
    TemporalParticipantGeneration, TemporalParticipantManifest, TemporalSnapshotRequest,
    TemporalSourceAccess, TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

use crate::retrieval::evidence_lanes::score_diagnostic;
use crate::retrieval::evidence_lanes::{
    DiagnosticCandidateReadPortV1, DiagnosticLaneEvidenceV1, DiagnosticLaneRequestV1,
    DiagnosticLaneRetrieverV1, DiagnosticMatchReasonV1, EvidenceLaneExecutionControlV1,
    TaskSessionBindingErrorV1, TaskSessionBindingV1, TaskSessionPlan23BindingV1,
    TemporalCandidateChannelV1, TemporalCandidateContributionV1, TemporalCandidateExportPortV1,
    TemporalLaneEvidenceV1, TemporalLaneRequestV1, TemporalLaneRetrieverV1,
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

fn generation_diagnostic() -> GenerationDiagnosticV1 {
    let mut diagnostic = GenerationDiagnosticV1 {
        diagnostic_anchor: id("anchor.diagnostic.fixture"),
        generation_id: id("generation.fixture"),
        repository: id("repository.fixture"),
        worktree: None,
        reference: None,
        source_revision: None,
        file_occurrence_id: id("file.fixture"),
        content_digest: id::<ContentDigest>(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        span: SourceSpan {
            start_byte: 4,
            end_byte: 12,
        },
        symbol_occurrence_id: None,
        code: "E0308".to_owned(),
        severity: DiagnosticSeverityV1::Error,
        message: "mismatched types".to_owned(),
        message_digest: id(
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
        provenance: DiagnosticProvenanceV1 {
            producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
            producer: id::<ProviderId>("provider.rustc"),
            analyzer_revision: id::<ComponentVersion>("analyzer.rustc.v1"),
            configuration_revision: id::<ComponentVersion>("configuration.rustc.v1"),
            sanitization_receipt: None,
        },
        evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
        collected_at: UtcMicros(7),
        state: DiagnosticRecordStateV1::Current,
    };
    diagnostic.message_digest = diagnostic.compute_message_digest().expect("message digest");
    diagnostic
}

#[test]
fn canonical_diagnostic_scoring_retains_the_match_reason_and_term_coverage() {
    let diagnostic = generation_diagnostic();
    let exact_terms = BTreeSet::from(["e0308"]);
    assert_eq!(
        score_diagnostic(&diagnostic, "e0308", &exact_terms).expect("exact score"),
        Some((DiagnosticMatchReasonV1::CodeExact, 1, 1_000_000)),
    );

    let overlap_terms = BTreeSet::from(["mismatched", "result"]);
    assert_eq!(
        score_diagnostic(&diagnostic, "mismatched result", &overlap_terms).expect("overlap score"),
        Some((DiagnosticMatchReasonV1::TokenOverlap, 1, 650_000)),
    );
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
fn deadline_and_cancellation_stop_mounted_evidence_lanes_before_source_reads() {
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
fn task_session_binding_seals_exact_task_graph_attempt_and_provider_source() {
    let task = id::<TaskId>("task.fixture");
    let accepted = WorkAttemptIdentityV1::new(
        task.clone(),
        id::<RunId>("run.fixture"),
        id::<AttemptId>("attempt.fixture"),
    )
    .expect("attempt identity");
    let verified = VerifiedWorkGraphVersionV1::new(
        WorkGraphVersionV1::new(7).expect("graph version"),
        WorkProductEventSequenceV1::new(11).expect("event sequence"),
        WorkProductSourceWatermarkV1::new(BTreeMap::<SourceStoreId, u64>::new())
            .expect("source watermark"),
        id("sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
    )
    .expect("verified graph");
    let source = ObservationSourceIdentityV1::for_provider(id("claude"), id("session.fixture"))
        .expect("sealed source");

    let binding = TaskSessionBindingV1::new(
        task.clone(),
        verified.clone(),
        &BTreeSet::from([accepted.clone()]),
        accepted.clone(),
        source.clone(),
    )
    .expect("accepted binding");
    assert_eq!(binding.task_id(), &task);
    assert_eq!(binding.verified_version(), &verified);
    assert_eq!(binding.accepted_attempt(), &accepted);
    assert_eq!(binding.source(), &source);

    let foreign_task_attempt =
        WorkAttemptIdentityV1::new(id("task.foreign"), id("run.fixture"), id("attempt.fixture"))
            .expect("foreign attempt");
    assert_eq!(
        TaskSessionBindingV1::new(
            task.clone(),
            verified.clone(),
            &BTreeSet::from([foreign_task_attempt.clone()]),
            foreign_task_attempt,
            source.clone(),
        ),
        Err(TaskSessionBindingErrorV1::ForeignTask),
    );
    assert_eq!(
        TaskSessionBindingV1::new(task, verified, &BTreeSet::new(), accepted, source,),
        Err(TaskSessionBindingErrorV1::AttemptNotAccepted),
    );
}

fn task_session_plan23(provider: &str) -> TaskSessionPlan23BindingV1 {
    let snapshot_request = TemporalSnapshotRequest::new(
        id::<SessionId>("session.fixture"),
        format!("sha256:{}", "1".repeat(64)),
        format!("sha256:{}", "2".repeat(64)),
        format!("sha256:{}", "3".repeat(64)),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
    )
    .and_then(|request| request.with_provider_scope(Some(provider.to_owned())))
    .expect("snapshot request");
    let configuration =
        BindingDigest::new("configuration_digest", format!("sha256:{}", "4".repeat(64)))
            .expect("configuration digest");
    let participant = TemporalParticipantGeneration::new(
        id("session.fixture"),
        provider,
        TemporalWatermarks {
            generation: 1,
            source: 2,
            projection: 2,
            index: 2,
            summary: 2,
        },
        2,
        &configuration,
        snapshot_request.access_digest(),
        TemporalParticipantAuthorization::Authorized,
        TemporalSourceAccess::Available,
    )
    .expect("participant");
    let snapshot = TemporalExecutionSnapshot::new_authorized(
        snapshot_request,
        TemporalWatermarks {
            generation: 1,
            source: 2,
            projection: 2,
            index: 2,
            summary: 2,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: configuration,
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .and_then(|snapshot| {
        snapshot.with_participant_manifest(
            TemporalParticipantManifest::new(vec![participant]).expect("manifest"),
        )
    })
    .expect("authoritative snapshot");
    TaskSessionPlan23BindingV1::new(snapshot, None).expect("Plan-23 binding")
}

#[test]
fn task_session_plan23_binding_denies_provider_collision() {
    let task = id::<TaskId>("task.fixture");
    let attempt =
        WorkAttemptIdentityV1::new(task.clone(), id("run.fixture"), id("attempt.fixture"))
            .expect("attempt");
    let binding = TaskSessionBindingV1::new(
        task,
        VerifiedWorkGraphVersionV1::new(
            WorkGraphVersionV1::new(7).expect("version"),
            WorkProductEventSequenceV1::new(11).expect("sequence"),
            WorkProductSourceWatermarkV1::new(BTreeMap::<SourceStoreId, u64>::new())
                .expect("watermark"),
            id("sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
        )
        .expect("verified graph"),
        &BTreeSet::from([attempt.clone()]),
        attempt,
        ObservationSourceIdentityV1::for_provider(id("claude"), id("session.fixture"))
            .expect("source"),
    )
    .expect("binding");

    assert!(task_session_plan23("claude").matches(&binding));
    assert!(
        !task_session_plan23("codex").matches(&binding),
        "same session id under a foreign provider must be denied",
    );
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
        contributions: vec![
            TemporalCandidateContributionV1 {
                channel: TemporalCandidateChannelV1::ExactMessage,
                source_occurrence: compact.source_occurrence_id.clone(),
                source_id: Some("claude".to_owned()),
                retriever_ordinal: 0,
                raw_score: 12,
                calibrated_score_micros: 900_000,
                exact_ranges: Vec::new(),
            },
            TemporalCandidateContributionV1 {
                channel: TemporalCandidateChannelV1::Summary,
                source_occurrence: compact.source_occurrence_id.clone(),
                source_id: Some("claude".to_owned()),
                retriever_ordinal: 1,
                raw_score: 10,
                calibrated_score_micros: 800_000,
                exact_ranges: Vec::new(),
            },
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
            .contributions
            .iter()
            .map(|contribution| contribution.channel)
            .collect::<Vec<_>>(),
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
        match_reason: DiagnosticMatchReasonV1::CodeExact,
        matched_query_terms: 1,
        query_terms: 1,
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
        contributions: vec![TemporalCandidateContributionV1 {
            channel: TemporalCandidateChannelV1::ExactMessage,
            source_occurrence: compact.source_occurrence_id.clone(),
            source_id: Some("claude".to_owned()),
            retriever_ordinal: 0,
            raw_score: 12,
            calibrated_score_micros: 900_000,
            exact_ranges: Vec::new(),
        }],
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
