use std::collections::BTreeMap;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use tracedecay_domain::{
    HydrationStateV1, MessageOccurrenceIdV1, RetrievalAnchorId, RetrievalGrainV1,
    SessionAuthorityClassV1, SessionCursorKeyIdV1, SessionCursorVersionV1, SessionId,
    SessionSummaryIdV1, SessionSummaryRecordV1, SignedCursorKeyRefV1, SummarySourceHorizonV1,
    TemporalAssertionKindV1, TemporalModeV1, TemporalValidityV1, UtcMicros,
};

use super::candidates::{CandidateChannel, CandidatePlan};
use super::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use super::cursor::{CursorError, verify_cursor};
use super::hydration::{
    HydrationAuthorization, HydrationDenial, HydrationFuture, HydrationGrant, HydrationSink,
    TemporalHydrationPort,
};
use super::ports::{
    BindingDigest, CandidatePageSink, ExecutionLimits, InMemoryCursorAuthenticator, KernelVersions,
    PageKey, PageRequest, PageStatus, PortFuture, SummarySourceRecord, TemporalExecutionSnapshot,
    TemporalPortError, TemporalReadPort, TemporalRecord, TemporalRecordPageSink,
    TemporalSnapshotRequest, TemporalWatermarks,
};
use super::ranking::{DiversityLimits, RankingCandidate, RankingError};
use super::resolution::summary::SummarySourceState;
use super::resolution::types::{
    ResolutionAssertion, ResolutionEvidence, ResolutionOccurrence, ValidatedAuthorization,
};
use super::{TemporalKernelError, TemporalKernelRequest, execute_temporal_kernel};
use crate::test_support::block_on;

struct FakeReadPort {
    candidates: Vec<RankingCandidate>,
    records: Vec<TemporalRecord>,
    candidate_pages: AtomicUsize,
    record_pages: AtomicUsize,
    max_candidate_page_items: AtomicUsize,
    observed_candidate_field_cap: AtomicUsize,
    observed_candidate_page_bytes: AtomicUsize,
    cancel_candidate_page: Option<usize>,
    empty_more: bool,
    oversized_candidate: bool,
}

impl FakeReadPort {
    fn new(candidates: Vec<RankingCandidate>, records: Vec<TemporalRecord>) -> Self {
        Self {
            candidates,
            records,
            candidate_pages: AtomicUsize::new(0),
            record_pages: AtomicUsize::new(0),
            max_candidate_page_items: AtomicUsize::new(0),
            observed_candidate_field_cap: AtomicUsize::new(0),
            observed_candidate_page_bytes: AtomicUsize::new(0),
            cancel_candidate_page: None,
            empty_more: false,
            oversized_candidate: false,
        }
    }
}

impl TemporalReadPort for FakeReadPort {
    fn produce_candidate_page<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.candidate_pages.fetch_add(1, Ordering::SeqCst);
            self.max_candidate_page_items
                .fetch_max(request.page_item_limit(), Ordering::SeqCst);
            self.observed_candidate_page_bytes
                .fetch_max(request.page_total_byte_limit(), Ordering::SeqCst);
            if let Some(caps) = request.candidate_field_caps() {
                self.observed_candidate_field_cap
                    .store(caps.stable_id_bytes(), Ordering::SeqCst);
            }
            if self.cancel_candidate_page == Some(request.page_index()) {
                snapshot.request().execution_control().cancel();
            }
            if self.empty_more && request.page_index() == 0 {
                return Ok(PageStatus::More);
            }
            if self.oversized_candidate && request.page_index() == 0 {
                let mut oversized = candidate("oversized", "oversized", 1);
                oversized.stable_id = "x".repeat(request.max_item_bytes().saturating_add(1));
                sink.push(oversized)?;
                return Ok(PageStatus::Complete);
            }
            let start = page_start(&request).min(self.candidates.len());
            let end = start
                .saturating_add(request.page_item_limit())
                .min(self.candidates.len());
            for candidate in &self.candidates[start..end] {
                sink.push(candidate.clone())?;
            }
            Ok(if end < self.candidates.len() {
                sink.set_continuation_key(PageKey::new(end.to_string()))?;
                PageStatus::More
            } else {
                PageStatus::Complete
            })
        })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.record_pages.fetch_add(1, Ordering::SeqCst);
            let start = page_start(&request).min(self.records.len());
            let end = start
                .saturating_add(request.page_item_limit())
                .min(self.records.len());
            for record in &self.records[start..end] {
                sink.push(clone_temporal_record(record))?;
            }
            Ok(if end < self.records.len() {
                sink.set_continuation_key(PageKey::new(end.to_string()))?;
                PageStatus::More
            } else {
                PageStatus::Complete
            })
        })
    }
}

fn page_start(request: &PageRequest) -> usize {
    request.keyset().map_or(0, |key| {
        key.as_str().parse::<usize>().expect("numeric page key")
    })
}

fn clone_temporal_record(record: &TemporalRecord) -> TemporalRecord {
    match record {
        TemporalRecord::Occurrence(value) => TemporalRecord::Occurrence(value.clone()),
        TemporalRecord::Copy(value) => TemporalRecord::Copy(value.clone()),
        TemporalRecord::Assertion(value) => TemporalRecord::Assertion(value.clone()),
        TemporalRecord::Summary(value) => TemporalRecord::Summary(value.clone()),
        TemporalRecord::SummarySource(value) => TemporalRecord::SummarySource(value.clone()),
    }
}

#[derive(Default)]
struct FakeHydrator {
    payloads: BTreeMap<RetrievalAnchorId, Vec<u8>>,
    denials: BTreeMap<RetrievalAnchorId, HydrationStateV1>,
    calls: Mutex<Vec<String>>,
}

impl TemporalHydrationPort for FakeHydrator {
    fn authorize_hydration<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        anchor_id: &'a RetrievalAnchorId,
    ) -> HydrationFuture<'a, HydrationAuthorization> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("authorize:{anchor_id}"));
            match self.denials.get(anchor_id).copied() {
                Some(state) => Ok(HydrationAuthorization::Denied(HydrationDenial::new(state)?)),
                None => Ok(HydrationAuthorization::Authorized),
            }
        })
    }

    fn read_authorized<'a>(
        &'a self,
        grant: &'a HydrationGrant<'_>,
        sink: &'a mut HydrationSink<'_>,
    ) -> HydrationFuture<'a, ()> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("read:{}", grant.anchor_id()));
            let payload = self
                .payloads
                .get(grant.anchor_id())
                .cloned()
                .unwrap_or_else(|| format!("payload:{}", grant.anchor_id()).into_bytes());
            sink.write_chunk(&payload)
        })
    }
}

struct Words;

impl VersionedTokenEstimator for Words {
    fn version(&self) -> &'static str {
        "words-v1"
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn anchor(value: &str) -> RetrievalAnchorId {
    RetrievalAnchorId::new(value).expect("valid anchor")
}

fn cursor_key(id: &str, version: u16) -> SignedCursorKeyRefV1 {
    SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new(id).expect("valid key id"),
        version: SessionCursorVersionV1::new(version).expect("valid key version"),
    }
}

fn authenticator(id: &str, version: u16, secret: u8) -> InMemoryCursorAuthenticator {
    InMemoryCursorAuthenticator::new(cursor_key(id, version), vec![secret; 32])
        .expect("valid authenticator")
}

fn candidate(stable_id: &str, anchor_id: &str, raw_score: i64) -> RankingCandidate {
    RankingCandidate {
        stable_id: stable_id.to_string(),
        anchor_id: anchor(anchor_id),
        retriever_record_id: stable_id.to_string(),
        channel: CandidateChannel::Phrase,
        raw_score,
        knowledge_at_micros: raw_score,
        logical_message: Some(format!("logical-{anchor_id}")),
        turn: Some("turn-1".to_string()),
        session: Some("session-1".to_string()),
        source: Some("source-1".to_string()),
        evidence_role: Some("message".to_string()),
        exact_ranges: Vec::new(),
    }
}

fn occurrence(byte: char, anchor_id: &str, knowledge_at: i64) -> ResolutionOccurrence {
    ResolutionOccurrence {
        occurrence_id: MessageOccurrenceIdV1::new(format!(
            "sha256:{}",
            byte.to_string().repeat(64)
        ))
        .expect("valid occurrence"),
        anchor_id: anchor(anchor_id),
        knowledge_at: UtcMicros(knowledge_at),
        valid_time: TemporalValidityV1::Known {
            valid_at: UtcMicros(knowledge_at),
        },
        evidence: ResolutionEvidence::new(
            SessionAuthorityClassV1::CanonicalObservation,
            ValidatedAuthorization::Authorized,
        ),
    }
}

fn assertion(
    kind: TemporalAssertionKindV1,
    subject: &str,
    object: &str,
    knowledge_at: i64,
) -> ResolutionAssertion {
    ResolutionAssertion {
        kind,
        subject_anchor_id: anchor(subject),
        object_anchor_id: anchor(object),
        knowledge_at: UtcMicros(knowledge_at),
        valid_time: TemporalValidityV1::Known {
            valid_at: UtcMicros(knowledge_at),
        },
        evidence: ResolutionEvidence::new(
            SessionAuthorityClassV1::CanonicalObservation,
            ValidatedAuthorization::Authorized,
        )
        .with_supporting_anchor(anchor("assertion-evidence")),
    }
}

fn summary(id: &str, source_anchor: &str, knowledge_through: i64) -> SessionSummaryRecordV1 {
    summary_with_sources(id, &[source_anchor], knowledge_through)
}

fn summary_with_sources(
    id: &str,
    source_anchors: &[&str],
    knowledge_through: i64,
) -> SessionSummaryRecordV1 {
    SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new(id).expect("valid summary id"),
        SessionId::new("session-1").expect("valid session"),
        anchor(&format!("summary-{id}")),
        source_anchors
            .iter()
            .map(|source_anchor| anchor(source_anchor))
            .collect(),
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(knowledge_through),
            valid_through: Some(UtcMicros(knowledge_through)),
        },
        UtcMicros(knowledge_through),
    )
    .expect("valid summary")
}

fn summary_source(anchor_id: &str, state: SummarySourceState) -> TemporalRecord {
    TemporalRecord::SummarySource(SummarySourceRecord {
        anchor_id: anchor(anchor_id),
        state,
    })
}

fn covered_summary_source(anchor_id: &str, at: i64) -> TemporalRecord {
    summary_source(
        anchor_id,
        SummarySourceState::Covered {
            knowledge_at: UtcMicros(at),
            valid_time: TemporalValidityV1::Known {
                valid_at: UtcMicros(at),
            },
        },
    )
}

fn request(mode: TemporalModeV1, limit: usize) -> TemporalKernelRequest {
    request_with_key(mode, limit, cursor_key("key-1", 1))
}

fn request_with_key(
    mode: TemporalModeV1,
    limit: usize,
    key: SignedCursorKeyRefV1,
) -> TemporalKernelRequest {
    let snapshot_request = TemporalSnapshotRequest::new(
        SessionId::new("session-1").expect("valid session"),
        digest('0'),
        digest('1'),
        digest('2'),
        mode,
        RetrievalGrainV1::LogicalMessage,
    )
    .expect("valid request");
    TemporalKernelRequest {
        snapshot: TemporalExecutionSnapshot::new_authorized(
            snapshot_request,
            TemporalWatermarks {
                generation: 7,
                source: 11,
                projection: 13,
                index: 17,
                summary: 19,
            },
            KernelVersions {
                schema: 3,
                ranking: 5,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("configuration digest"),
            },
            Some(key),
            ValidatedAuthorization::Authorized,
        )
        .expect("application-frozen snapshot"),
        query: "\"exact phrase\"".to_string(),
        direct_anchor: None,
        cursor: None,
        limit,
        diversity: DiversityLimits::unbounded(),
        context_budget: ContextBudget {
            max_bytes: 100_000,
            max_tokens: 100_000,
            estimator_version: "words-v1".to_string(),
        },
    }
}

fn basic_port() -> FakeReadPort {
    FakeReadPort::new(
        vec![candidate("a", "a", 20), candidate("b", "b", 10)],
        vec![
            TemporalRecord::Occurrence(occurrence('a', "a", 20)),
            TemporalRecord::Occurrence(occurrence('b', "b", 10)),
        ],
    )
}

#[test]
fn malicious_producer_cannot_underreport_or_cross_prework_allocation_contract() {
    block_on(async {
        let mut port = FakeReadPort::new(Vec::new(), Vec::new());
        port.oversized_candidate = true;
        let mut request = request(TemporalModeV1::Current, 1);
        request.snapshot = request
            .snapshot
            .with_limits(ExecutionLimits {
                candidate_limit: 1,
                candidate_total_bytes: 64,
                candidate_item_bytes: 4096,
                candidate_stable_id_bytes: 8,
                hydration_limit: 1,
                ..ExecutionLimits::default()
            })
            .expect("test limits only tighten the authorized snapshot");

        let result = execute_temporal_kernel(
            &request,
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await;

        assert_eq!(result, Err(TemporalKernelError::BudgetExceeded));
        assert_eq!(port.max_candidate_page_items.load(Ordering::SeqCst), 1);
        assert_eq!(port.observed_candidate_field_cap.load(Ordering::SeqCst), 8);
        assert_eq!(
            port.observed_candidate_page_bytes.load(Ordering::SeqCst),
            64
        );
        assert_eq!(port.record_pages.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn empty_continuation_page_is_a_typed_port_error_not_empty_success() {
    block_on(async {
        let mut port = FakeReadPort::new(Vec::new(), Vec::new());
        port.empty_more = true;

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 1),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await;

        assert!(matches!(
            result,
            Err(TemporalKernelError::Port(TemporalPortError::Read { .. }))
        ));
    });
}

#[test]
fn live_cancellation_interrupts_a_multipage_candidate_pull() {
    block_on(async {
        let candidates = (0..65)
            .map(|index| candidate(&format!("id-{index:02}"), &format!("a-{index:02}"), index))
            .collect();
        let mut port = FakeReadPort::new(candidates, Vec::new());
        port.cancel_candidate_page = Some(1);
        let mut request = request(TemporalModeV1::Current, 1);
        request.snapshot = request
            .snapshot
            .with_limits(ExecutionLimits {
                candidate_limit: 65,
                hydration_limit: 1,
                ..ExecutionLimits::default()
            })
            .expect("test limits only tighten the authorized snapshot");

        let result = execute_temporal_kernel(
            &request,
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await;

        assert_eq!(result, Err(TemporalKernelError::Cancelled));
        assert_eq!(port.candidate_pages.load(Ordering::SeqCst), 2);
        assert_eq!(port.record_pages.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn key_rotation_reports_precise_cursor_route_mismatch() {
    block_on(async {
        let first_port = basic_port();
        let first = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 1),
            &first_port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("first page");
        let mut second_request =
            request_with_key(TemporalModeV1::Current, 1, cursor_key("key-2", 1));
        second_request.cursor = first.next_cursor;
        let rotated_port = basic_port();

        let result = execute_temporal_kernel(
            &second_request,
            &rotated_port,
            &FakeHydrator::default(),
            // The verifier retains the old key long enough to authenticate the
            // route before reporting that the request now expects a new key.
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await;

        assert_eq!(
            result,
            Err(TemporalKernelError::Cursor(CursorError::KeyIdMismatch))
        );
        assert_eq!(rotated_port.candidate_pages.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn ranking_conflicts_propagate_without_fail_open_selection() {
    block_on(async {
        let mut left = candidate("same", "a", 20);
        left.source = Some("left".to_string());
        let mut right = candidate("same", "b", 10);
        right.source = Some("right".to_string());
        let port = FakeReadPort::new(
            vec![left, right],
            vec![
                TemporalRecord::Occurrence(occurrence('a', "a", 20)),
                TemporalRecord::Occurrence(occurrence('b', "b", 10)),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 2),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await;

        assert_eq!(
            result,
            Err(TemporalKernelError::Ranking(
                RankingError::ConflictingDuplicateMetadata {
                    stable_id: "same".to_string(),
                }
            ))
        );
    });
}

#[test]
fn evolution_chain_is_exposed_in_canonical_context() {
    block_on(async {
        let port = FakeReadPort::new(
            vec![
                candidate("original", "original", 30),
                candidate("correction", "correction", 20),
                candidate("successor", "successor", 10),
            ],
            vec![
                TemporalRecord::Occurrence(occurrence('a', "original", 30)),
                TemporalRecord::Occurrence(occurrence('b', "correction", 20)),
                TemporalRecord::Occurrence(occurrence('c', "successor", 10)),
                TemporalRecord::Assertion(assertion(
                    TemporalAssertionKindV1::Corrects,
                    "correction",
                    "original",
                    31,
                )),
                TemporalRecord::Assertion(assertion(
                    TemporalAssertionKindV1::Supersedes,
                    "successor",
                    "correction",
                    32,
                )),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Evolution, 3),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("evolution");

        assert_eq!(result.lineage.len(), 2);
        assert_eq!(result.context.bundle.lineage, result.lineage);
        assert_eq!(
            result
                .lineage
                .iter()
                .map(|edge| edge.kind)
                .collect::<Vec<_>>(),
            vec![
                TemporalAssertionKindV1::Supersedes,
                TemporalAssertionKindV1::Corrects,
            ]
        );
        let rendered: serde_json::Value =
            serde_json::from_str(&result.context.rendered).expect("canonical context");
        assert_eq!(
            rendered["bundle"]["lineage"][1]["object_anchor_id"],
            "original"
        );
    });
}

#[test]
fn unresolved_conflict_is_exposed_in_result_and_canonical_context() {
    block_on(async {
        let port = FakeReadPort::new(
            vec![
                candidate("left", "left", 20),
                candidate("right", "right", 10),
            ],
            vec![
                TemporalRecord::Occurrence(occurrence('a', "left", 1)),
                TemporalRecord::Occurrence(occurrence('b', "right", 2)),
                TemporalRecord::Assertion(assertion(
                    TemporalAssertionKindV1::Contradicts,
                    "right",
                    "left",
                    3,
                )),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 2),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("conflict");

        assert_eq!(result.conflicts.len(), 2);
        assert_eq!(result.context.bundle.conflicts, result.conflicts);
        assert_eq!(result.lineage[0].kind, TemporalAssertionKindV1::Contradicts);
        let rendered: serde_json::Value =
            serde_json::from_str(&result.context.rendered).expect("canonical context");
        assert_eq!(rendered["bundle"]["conflicts"].as_array().unwrap().len(), 2);
    });
}

#[test]
fn reciprocal_corrections_remain_visible_as_a_conflicted_context_cycle() {
    block_on(async {
        let port = FakeReadPort::new(
            vec![
                candidate("left", "left", 20),
                candidate("right", "right", 10),
            ],
            vec![
                TemporalRecord::Occurrence(occurrence('a', "left", 1)),
                TemporalRecord::Occurrence(occurrence('b', "right", 2)),
                TemporalRecord::Assertion(assertion(
                    TemporalAssertionKindV1::Corrects,
                    "right",
                    "left",
                    3,
                )),
                TemporalRecord::Assertion(assertion(
                    TemporalAssertionKindV1::Corrects,
                    "left",
                    "right",
                    4,
                )),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 2),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("conflicted correction cycle");

        assert_eq!(result.ranked.len(), 2);
        assert_eq!(result.conflicts.len(), 2);
        assert_eq!(result.lineage.len(), 2);
        assert_eq!(result.context.bundle.conflicts, result.conflicts);
        assert_eq!(result.context.bundle.lineage, result.lineage);
    });
}

#[test]
fn context_excludes_unrelated_off_page_conflicts_and_lineage() {
    block_on(async {
        let port = FakeReadPort::new(
            vec![
                candidate("selected", "selected", 30),
                candidate("left", "left", 20),
                candidate("right", "right", 10),
            ],
            vec![
                TemporalRecord::Occurrence(occurrence('a', "selected", 1)),
                TemporalRecord::Occurrence(occurrence('b', "left", 2)),
                TemporalRecord::Occurrence(occurrence('c', "right", 3)),
                TemporalRecord::Assertion(assertion(
                    TemporalAssertionKindV1::Contradicts,
                    "right",
                    "left",
                    4,
                )),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 1),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("first page");

        assert_eq!(result.ranked[0].anchor_id, anchor("selected"));
        assert!(result.conflicts.is_empty());
        assert!(result.lineage.is_empty());
    });
}

#[test]
fn unauthorized_assertion_metadata_never_enters_resolution_or_context() {
    block_on(async {
        let mut denied = assertion(TemporalAssertionKindV1::Contradicts, "right", "left", 3);
        denied.evidence = ResolutionEvidence::new(
            SessionAuthorityClassV1::CanonicalObservation,
            ValidatedAuthorization::Unauthorized,
        )
        .with_supporting_anchor(anchor("private-lineage"));
        let port = FakeReadPort::new(
            vec![
                candidate("left", "left", 20),
                candidate("right", "right", 10),
            ],
            vec![
                TemporalRecord::Occurrence(occurrence('a', "left", 1)),
                TemporalRecord::Occurrence(occurrence('b', "right", 2)),
                TemporalRecord::Assertion(denied),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 2),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("unauthorized assertion is ignored");

        assert!(result.lineage.is_empty());
        assert!(result.conflicts.is_empty());
        assert!(!result.context.rendered.contains("private-lineage"));
    });
}

#[test]
fn invalid_summary_successor_does_not_hide_eligible_predecessor() {
    block_on(async {
        let predecessor = summary("predecessor", "source", 7);
        let invalid_successor = summary("successor", "missing", 8)
            .with_predecessor(predecessor.summary_id().clone())
            .expect("valid predecessor reference");
        let port = FakeReadPort::new(
            vec![
                candidate("source", "source", 30),
                candidate("predecessor", "summary-predecessor", 20),
                candidate("successor", "summary-successor", 10),
            ],
            vec![
                TemporalRecord::Occurrence(occurrence('a', "source", 7)),
                TemporalRecord::Summary(predecessor),
                TemporalRecord::Summary(invalid_successor),
                covered_summary_source("source", 7),
                summary_source("missing", SummarySourceState::Missing),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 3),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("current");
        let anchors = result
            .ranked
            .iter()
            .map(|candidate| candidate.anchor_id.clone())
            .collect::<Vec<_>>();

        assert!(anchors.contains(&anchor("summary-predecessor")));
        assert!(!anchors.contains(&anchor("summary-successor")));
        assert_eq!(result.coverage.unknown, 1);
        assert_eq!(result.summary_omissions.len(), 1);
        assert_eq!(result.context.bundle.omissions.len(), 2);
        let rendered: serde_json::Value =
            serde_json::from_str(&result.context.rendered).expect("canonical context");
        assert_eq!(
            rendered["summary_omissions"][0]["rejection"]["MissingSource"]["anchor_id"],
            "missing"
        );
    });
}

#[test]
fn evolution_accepts_predecessor_and_successor_with_an_identical_shared_source_state() {
    block_on(async {
        let predecessor = summary("predecessor", "shared-source", 7);
        let successor = summary("successor", "shared-source", 8)
            .with_predecessor(predecessor.summary_id().clone())
            .expect("valid predecessor reference");
        let port = FakeReadPort::new(
            vec![
                candidate("predecessor", "summary-predecessor", 20),
                candidate("successor", "summary-successor", 10),
            ],
            vec![
                TemporalRecord::Summary(predecessor),
                TemporalRecord::Summary(successor),
                covered_summary_source("shared-source", 7),
                covered_summary_source("shared-source", 7),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Evolution, 2),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("shared summary source state");

        assert_eq!(result.ranked.len(), 2);
        assert!(result.summary_omissions.is_empty());
    });
}

#[test]
fn contradictory_duplicate_summary_source_states_are_rejected() {
    block_on(async {
        let port = FakeReadPort::new(
            vec![candidate("summary", "summary-one", 10)],
            vec![
                TemporalRecord::Summary(summary("one", "shared-source", 7)),
                covered_summary_source("shared-source", 7),
                summary_source("shared-source", SummarySourceState::Missing),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 1),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await;

        assert!(matches!(
            result,
            Err(TemporalKernelError::Port(TemporalPortError::Read {
                operation: "collect summary source states",
                message,
            })) if message == "adapter returned contradictory summary source states"
        ));
    });
}

#[test]
fn summary_lineage_is_limited_to_the_selected_ranked_page() {
    block_on(async {
        let port = FakeReadPort::new(
            vec![
                candidate("summary-one", "summary-one", 20),
                candidate("summary-two", "summary-two", 10),
            ],
            vec![
                TemporalRecord::Summary(summary("one", "source-one", 7)),
                TemporalRecord::Summary(summary("two", "source-two", 7)),
                covered_summary_source("source-one", 7),
                covered_summary_source("source-two", 7),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 1),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("first summary page");

        assert_eq!(result.ranked.len(), 1);
        assert_eq!(result.ranked[0].anchor_id, anchor("summary-one"));
        assert_eq!(result.lineage.len(), 1);
        assert_eq!(result.lineage[0].subject_anchor_id, anchor("summary-one"));
        assert_eq!(result.lineage[0].object_anchor_id, anchor("source-one"));
        assert!(
            result
                .lineage
                .iter()
                .all(|edge| edge.subject_anchor_id != anchor("summary-two"))
        );
    });
}

/// A derived-evidence group anchor is a span/burst container, not a retrievable
/// payload: no hydration authority resolves one. Ranking it would spend a result
/// slot and then report an unresolvable omission, so the group stays out of the
/// ranked page while its member occurrences keep their ordinary coverage.
#[test]
fn derived_group_candidate_never_ranks_as_a_standalone_row() {
    block_on(async {
        let derived_anchor = anchor("derived-span");
        let mut derived = candidate("derived-span", "derived-span", 20);
        derived.channel = CandidateChannel::Span;
        derived.retriever_record_id = "span-evidence-id".to_string();
        let mut member = occurrence('a', "source-occurrence", 20);
        member.evidence = member
            .evidence
            .with_supporting_anchor(derived_anchor.clone());
        let port = FakeReadPort::new(
            vec![derived],
            vec![TemporalRecord::Occurrence(member.clone())],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 1),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("derived span member");

        assert!(
            result
                .ranked
                .iter()
                .all(|candidate| candidate.anchor_id != derived_anchor),
            "a derived-evidence group container must never be ranked"
        );
        assert!(
            result
                .hydrated
                .iter()
                .all(|hydrated| hydrated.anchor_id() != &derived_anchor),
            "a group container must never reach hydration, so it can never be omitted"
        );
        // The member the group pulled into the record read still counts as
        // covered, exactly as before: only the container itself is withheld.
        assert_eq!(result.coverage.visible, 1);
        assert_eq!(result.coverage.total(), Some(1));
    });
}

#[test]
fn summary_availability_maps_to_explicit_coverage_and_canonical_omissions() {
    block_on(async {
        let port = FakeReadPort::new(
            vec![
                candidate("covered", "summary-covered", 40),
                candidate("unauthorized", "summary-unauthorized", 30),
                candidate("locked", "summary-locked", 20),
                candidate("deleted", "summary-deleted", 10),
            ],
            vec![
                TemporalRecord::Summary(summary("covered", "covered-source", 7)),
                TemporalRecord::Summary(summary("unauthorized", "unauthorized-source", 7)),
                TemporalRecord::Summary(summary("locked", "locked-source", 7)),
                TemporalRecord::Summary(summary("deleted", "deleted-source", 7)),
                covered_summary_source("covered-source", 7),
                summary_source("unauthorized-source", SummarySourceState::Unauthorized),
                summary_source("locked-source", SummarySourceState::Locked),
                summary_source("deleted-source", SummarySourceState::Deleted),
            ],
        );

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 4),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("summary availability");

        assert_eq!(result.coverage.visible, 1);
        assert_eq!(result.coverage.hidden, 1);
        assert_eq!(result.coverage.unknown, 1);
        assert_eq!(result.coverage.redacted, 1);
        assert_eq!(result.summary_omissions.len(), 2);
        assert_eq!(result.context.bundle.omissions.len(), 4);
        let rendered: serde_json::Value =
            serde_json::from_str(&result.context.rendered).expect("canonical context");
        assert_eq!(
            rendered["summary_omissions"]
                .as_array()
                .expect("summary omissions")
                .len(),
            2
        );
    });
}

#[test]
fn unauthorized_mixed_source_permutations_are_publicly_indistinguishable() {
    block_on(async {
        let mut rendered_contexts = Vec::new();
        for source_anchors in [
            ["redacted-source", "unauthorized-source"],
            ["unauthorized-source", "redacted-source"],
        ] {
            let port = FakeReadPort::new(
                vec![candidate("private-summary", "summary-private", 10)],
                vec![
                    TemporalRecord::Summary(summary_with_sources("private", &source_anchors, 7)),
                    summary_source("redacted-source", SummarySourceState::Redacted),
                    summary_source("unauthorized-source", SummarySourceState::Unauthorized),
                ],
            );

            let outcome = execute_temporal_kernel(
                &request(TemporalModeV1::Current, 1),
                &port,
                &FakeHydrator::default(),
                &authenticator("key-1", 1, 7),
                &Words,
            )
            .await;
            let debug = format!("{outcome:?}");
            for private_value in [
                "private-summary",
                "summary-private",
                "redacted-source",
                "unauthorized-source",
            ] {
                assert!(!debug.contains(private_value));
            }

            let result = outcome.expect("mixed private summary");
            assert_eq!(result.coverage.hidden, 1);
            assert_eq!(
                result.coverage.visible
                    + result.coverage.hidden
                    + result.coverage.unknown
                    + result.coverage.redacted,
                1
            );
            assert!(result.ranked.is_empty());
            assert!(result.summary_omissions.is_empty());
            assert!(result.context.bundle.omissions.is_empty());
            assert!(result.conflicts.is_empty());
            assert!(result.lineage.is_empty());
            assert!(result.next_cursor.is_none());
            for private_value in [
                "private-summary",
                "summary-private",
                "redacted-source",
                "unauthorized-source",
            ] {
                assert!(!result.context.rendered.contains(private_value));
            }
            rendered_contexts.push(result.context.rendered);
        }

        assert_eq!(rendered_contexts[0], rendered_contexts[1]);
    });
}

#[test]
fn denied_hydration_is_authorized_first_and_payload_read_is_impossible() {
    block_on(async {
        let port = FakeReadPort::new(
            vec![candidate("denied", "denied", 10)],
            vec![TemporalRecord::Occurrence(occurrence('a', "denied", 10))],
        );
        let hydrator = FakeHydrator {
            denials: [(anchor("denied"), HydrationStateV1::Redacted)]
                .into_iter()
                .collect(),
            ..FakeHydrator::default()
        };

        let result = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 1),
            &port,
            &hydrator,
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("denied metadata");

        assert_eq!(
            *hydrator.calls.lock().expect("calls lock"),
            vec!["authorize:denied"]
        );
        assert!(result.context.bundle.records.is_empty());
        assert_eq!(result.context.bundle.omissions.len(), 1);
        assert_eq!(result.coverage.redacted, 1);
        assert_eq!(result.coverage.total(), Some(1));
    });
}

#[test]
fn interleaved_hydration_preserves_ranked_results_omissions_and_cursor() {
    block_on(async {
        let candidates = vec![
            candidate("available-high", "available-high", 50),
            candidate("denied-z", "z-denied", 40),
            candidate("available-mid", "available-mid", 30),
            candidate("denied-a", "a-denied", 20),
            candidate("page-tail", "page-tail", 10),
        ];
        let records = vec![
            TemporalRecord::Occurrence(occurrence('a', "available-high", 50)),
            TemporalRecord::Occurrence(occurrence('b', "z-denied", 40)),
            TemporalRecord::Occurrence(occurrence('c', "available-mid", 30)),
            TemporalRecord::Occurrence(occurrence('d', "a-denied", 20)),
            TemporalRecord::Occurrence(occurrence('e', "page-tail", 10)),
        ];
        let denied_request = request(TemporalModeV1::Current, 4);
        let hydrator = FakeHydrator {
            denials: [
                (anchor("z-denied"), HydrationStateV1::Redacted),
                (anchor("a-denied"), HydrationStateV1::Locked),
            ]
            .into_iter()
            .collect(),
            ..FakeHydrator::default()
        };

        let denied = execute_temporal_kernel(
            &denied_request,
            &FakeReadPort::new(
                candidates.clone(),
                records.iter().map(clone_temporal_record).collect(),
            ),
            &hydrator,
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("interleaved hydration");
        let authorized = execute_temporal_kernel(
            &denied_request,
            &FakeReadPort::new(candidates, records),
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("fully authorized hydration");

        assert_eq!(
            denied
                .hydrated
                .iter()
                .map(|result| {
                    (
                        result.rank(),
                        result.stable_id().to_string(),
                        result.anchor_id().clone(),
                        result.state(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    0,
                    "available-high".to_string(),
                    anchor("available-high"),
                    HydrationStateV1::Available,
                ),
                (
                    1,
                    "denied-z".to_string(),
                    anchor("z-denied"),
                    HydrationStateV1::Redacted,
                ),
                (
                    2,
                    "available-mid".to_string(),
                    anchor("available-mid"),
                    HydrationStateV1::Available,
                ),
                (
                    3,
                    "denied-a".to_string(),
                    anchor("a-denied"),
                    HydrationStateV1::Locked,
                ),
            ]
        );
        assert_eq!(
            denied
                .context
                .bundle
                .omissions
                .iter()
                .map(|omission| omission.anchor_id.clone())
                .collect::<Vec<_>>(),
            vec![Some(anchor("z-denied")), Some(anchor("a-denied"))]
        );
        assert_eq!(
            denied
                .hydrated
                .iter()
                .map(|result| result.content().is_some())
                .collect::<Vec<_>>(),
            vec![true, false, true, false]
        );
        let rendered: serde_json::Value =
            serde_json::from_str(&denied.context.rendered).expect("rendered context");
        assert_eq!(
            rendered["bundle"]["omissions"]
                .as_array()
                .expect("omission array")
                .iter()
                .map(|omission| omission["anchor_id"].as_str())
                .collect::<Vec<_>>(),
            vec![Some("z-denied"), Some("a-denied")]
        );
        assert_eq!(denied.ranked, authorized.ranked);
        let cursor_authenticator = authenticator("key-1", 1, 7);
        let denied_cursor = denied.next_cursor.as_deref().expect("denied cursor");
        let authorized_cursor = authorized
            .next_cursor
            .as_deref()
            .expect("authorized cursor");
        assert_eq!(
            verify_cursor(denied_cursor, &denied.snapshot, &cursor_authenticator),
            verify_cursor(
                authorized_cursor,
                &authorized.snapshot,
                &cursor_authenticator
            )
        );
    });
}

#[test]
fn exact_context_budget_preserves_canonical_accounting() {
    block_on(async {
        let port = FakeReadPort::new(
            vec![candidate("exact", "exact", 10)],
            vec![TemporalRecord::Occurrence(occurrence('a', "exact", 10))],
        );
        let first = execute_temporal_kernel(
            &request(TemporalModeV1::Current, 1),
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("baseline");
        let mut exact_request = request(TemporalModeV1::Current, 1);
        exact_request.context_budget.max_bytes = first.context.accounted_bytes;

        let exact = execute_temporal_kernel(
            &exact_request,
            &port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("exact budget");

        assert_eq!(exact.context.rendered, first.context.rendered);
        assert_eq!(
            exact.context.accounted_bytes,
            exact.context.rendered.len() as u64
        );
        assert_eq!(
            exact.context.accounted_bytes,
            exact_request.context_budget.max_bytes
        );
    });
}

#[test]
fn full_pipeline_is_deterministic_across_restart_and_cursor_resume() {
    block_on(async {
        let first_port = basic_port();
        let first_request = request(TemporalModeV1::Current, 1);
        let first = execute_temporal_kernel(
            &first_request,
            &first_port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("first run");
        let restarted_port = basic_port();
        let restarted = execute_temporal_kernel(
            &first_request,
            &restarted_port,
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("restart");

        let first_cursor = first.next_cursor.as_deref().expect("first cursor");
        let restarted_cursor = restarted.next_cursor.as_deref().expect("restarted cursor");
        let cursor_authenticator = authenticator("key-1", 1, 7);
        assert_eq!(
            verify_cursor(first_cursor, &first.snapshot, &cursor_authenticator),
            verify_cursor(restarted_cursor, &restarted.snapshot, &cursor_authenticator)
        );
        let mut first_without_cursor = first.clone();
        first_without_cursor.next_cursor = None;
        let mut restarted_without_cursor = restarted.clone();
        restarted_without_cursor.next_cursor = None;
        assert_eq!(first_without_cursor, restarted_without_cursor);
        assert_eq!(first.snapshot.watermarks().generation, 7);
        assert_eq!(restarted.snapshot, first.snapshot);
        assert_eq!(first.coverage.total(), Some(2));
        assert!(first.next_cursor.is_some());

        let mut resume_request = first_request;
        resume_request.cursor = first.next_cursor;
        let resumed = execute_temporal_kernel(
            &resume_request,
            &basic_port(),
            &FakeHydrator::default(),
            &authenticator("key-1", 1, 7),
            &Words,
        )
        .await
        .expect("resume");

        assert_eq!(resumed.ranked.len(), 1);
        assert_ne!(resumed.ranked[0].stable_id, restarted.ranked[0].stable_id);
        assert!(resumed.next_cursor.is_none());
    });
}
