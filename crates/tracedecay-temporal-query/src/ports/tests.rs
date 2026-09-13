use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use tracedecay_domain::{
    RetrievalAnchorId, RetrievalGrainV1, SessionId, SessionSourceCoverageStateV1,
    SignedCursorKeyRefV1, TemporalModeV1,
};

use super::cursor_authentication::MAX_CURSOR_SECRET_BYTES;
use super::execution::{MAX_READ_ITEMS, MAX_READ_TOTAL_BYTES};
use super::paging::{MAX_BOUNDED_PAGE_PREALLOC, MAX_PAGE_ITEMS_CAP};
use super::*;
use crate::candidates::{CandidateChannel, CandidatePlan};
use crate::ranking::RankingCandidate;
use crate::resolution::summary::SummarySourceState;
use crate::resolution::types::ValidatedAuthorization;
use crate::test_support::block_on;

fn session_id() -> SessionId {
    serde_json::from_str("\"session-1\"").expect("valid session id")
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn participant(session: &str, source: &str, generation: u64) -> TemporalParticipantGeneration {
    TemporalParticipantGeneration::new(
        SessionId::new(session).expect("session"),
        source,
        TemporalWatermarks {
            generation,
            source: 2,
            projection: 3,
            index: 4,
            summary: 5,
        },
        6,
        &BindingDigest::new("configuration", digest('7')).expect("configuration"),
        &BindingDigest::new("authorization", digest('8')).expect("authorization"),
        TemporalParticipantAuthorization::Authorized,
        TemporalSourceAccess::Available,
    )
    .expect("participant")
}

#[test]
fn expired_deadline_fails_at_checkpoint() {
    let control = ExecutionControl::new(Some(Instant::now()));

    assert_eq!(
        control.checkpoint(),
        Err(TemporalPortError::DeadlineExceeded)
    );
}

#[test]
fn snapshot_request_requires_canonical_bindings() {
    let error = TemporalSnapshotRequest::new(
        session_id(),
        "",
        digest('a'),
        digest('b'),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
    )
    .expect_err("empty root digest must fail closed");

    assert_eq!(
        error,
        TemporalPortError::InvalidBinding {
            field: "root_digest"
        }
    );
}

#[test]
fn semantic_filter_rejects_ambiguous_or_unstable_bindings() {
    let unsorted = TemporalCandidateFilterV1 {
        roles: vec!["user".to_string(), "assistant".to_string()],
        ..TemporalCandidateFilterV1::default()
    };
    assert_eq!(
        unsorted.validate(),
        Err(TemporalPortError::InvalidBinding { field: "roles" })
    );
    let orphan_agent = TemporalCandidateFilterV1 {
        workflow_agent: Some("worker".to_string()),
        ..TemporalCandidateFilterV1::default()
    };
    assert_eq!(
        orphan_agent.validate(),
        Err(TemporalPortError::InvalidBinding {
            field: "workflow_agent"
        })
    );
}

#[test]
fn participant_manifest_is_sorted_unique_bounded_and_epoch_bound() {
    let manifest = TemporalParticipantManifest::new(vec![
        participant("session-2", "source-b", 2),
        participant("session-1", "source-a", 1),
    ])
    .expect("manifest");
    assert_eq!(
        manifest
            .entries()
            .iter()
            .map(|entry| (entry.session_id().as_str(), entry.source_id()))
            .collect::<Vec<_>>(),
        [("session-1", "source-a"), ("session-2", "source-b")]
    );

    let changed = TemporalParticipantManifest::new(vec![
        participant("session-1", "source-a", 1),
        participant("session-2", "source-b", 3),
    ])
    .expect("changed manifest");
    assert_ne!(manifest.epoch_digest(), changed.epoch_digest());

    assert_eq!(
        TemporalParticipantManifest::new(vec![
            participant("session-1", "source-a", 1),
            participant("session-1", "source-a", 1),
        ]),
        Err(TemporalPortError::DuplicateParticipant)
    );

    let accepted = (0..MAX_TEMPORAL_PARTICIPANTS)
        .map(|index| participant("session-1", &format!("s{index:03}"), 1))
        .collect();
    assert!(TemporalParticipantManifest::new(accepted).is_ok());
    let rejected = (0..=MAX_TEMPORAL_PARTICIPANTS)
        .map(|index| participant("session-1", &format!("s{index:03}"), 1))
        .collect();
    assert!(matches!(
        TemporalParticipantManifest::new(rejected),
        Err(TemporalPortError::ParticipantLimitExceeded {
            observed,
            maximum: MAX_TEMPORAL_PARTICIPANTS,
        }) if observed == MAX_TEMPORAL_PARTICIPANTS + 1
    ));
}

fn participant_entries_with_canonical_bytes(
    target_bytes: usize,
) -> Vec<TemporalParticipantGeneration> {
    let mut entries = (0..128)
        .map(|index| participant("session-1", &format!("s{index:03}"), 1))
        .collect::<Vec<_>>();
    let base_bytes = serde_json::to_vec(&entries).unwrap().len();
    assert!(base_bytes <= target_bytes);
    let mut remaining = target_bytes - base_bytes;
    for entry in &mut entries {
        let available = 512_usize.saturating_sub(entry.source_id.len());
        let add = available.min(remaining);
        entry.source_id.push_str(&"x".repeat(add));
        remaining -= add;
        if remaining == 0 {
            break;
        }
    }
    assert_eq!(remaining, 0, "test entries could not reach target size");
    assert_eq!(serde_json::to_vec(&entries).unwrap().len(), target_bytes);
    entries
}

#[test]
fn participant_manifest_accepts_exact_canonical_byte_limit() {
    let entries = participant_entries_with_canonical_bytes(MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES);
    assert!(TemporalParticipantManifest::new(entries).is_ok());
}

#[test]
fn participant_manifest_rejects_one_byte_over_canonical_limit() {
    let entries =
        participant_entries_with_canonical_bytes(MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES + 1);
    assert_eq!(
        TemporalParticipantManifest::new(entries),
        Err(TemporalPortError::ParticipantManifestBytesExceeded {
            observed: MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES + 1,
            maximum: MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES,
        })
    );
}

struct ScopeObservingPort {
    observed: Mutex<Vec<TemporalRetrievalScope>>,
}

impl TemporalReadPort for ScopeObservingPort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        _request: PageRequest,
        _sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async {
            Err(TemporalPortError::Read {
                operation: "legacy candidate entry point",
                message: "scope-aware kernel must not call the legacy entry point".to_string(),
            })
        })
    }

    fn produce_candidate_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        _request: PageRequest,
        _sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.observed
                .lock()
                .expect("observed lock")
                .push(scope.clone());
            Ok(PageStatus::Complete)
        })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        _request: PageRequest,
        _sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async {
            Err(TemporalPortError::Read {
                operation: "legacy record entry point",
                message: "scope-aware kernel must not call the legacy entry point".to_string(),
            })
        })
    }

    fn produce_temporal_record_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        _request: PageRequest,
        _sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.observed
                .lock()
                .expect("observed lock")
                .push(scope.clone());
            Ok(PageStatus::Complete)
        })
    }
}

#[test]
fn candidate_record_and_summary_provider_path_observes_frozen_root_scope() {
    block_on(async {
        let port = ScopeObservingPort {
            observed: Mutex::new(Vec::new()),
        };
        let request = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid request")
        .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot);
        let snapshot = TemporalExecutionSnapshot::new(
            request,
            TemporalWatermarks {
                generation: 1,
                source: 2,
                projection: 3,
                index: 4,
                summary: 5,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            None,
        )
        .expect("valid snapshot");
        let mut candidate_state =
            CandidateReadState::new(PageLimits::new(1, 1024, 1024, 1).expect("candidate limits"));
        let mut record_state =
            TemporalRecordReadState::new(PageLimits::new(1, 1024, 1024, 1).expect("record limits"));

        pull_candidate_page(
            &port,
            &snapshot,
            &CandidatePlan::default(),
            &mut candidate_state,
        )
        .await
        .expect("candidate scope");
        pull_temporal_record_page(&port, &snapshot, &[], &mut record_state)
            .await
            .expect("record and summary scope");

        assert_eq!(
            *port.observed.lock().expect("observed lock"),
            [
                TemporalRetrievalScope::AllSessionsInAuthorizedRoot,
                TemporalRetrievalScope::AllSessionsInAuthorizedRoot,
            ]
        );
    });
}

#[test]
fn snapshot_request_rejects_noncanonical_provider_scope() {
    let request = TemporalSnapshotRequest::new(
        session_id(),
        digest('0'),
        digest('1'),
        digest('2'),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
    )
    .expect("valid request");

    assert_eq!(
        request.with_provider_scope(Some(" claude".to_string())),
        Err(TemporalPortError::InvalidBinding {
            field: "provider_scope"
        })
    );
}

#[test]
fn execution_snapshot_requires_explicit_validated_authorization() {
    let request = TemporalSnapshotRequest::new(
        session_id(),
        digest('0'),
        digest('1'),
        digest('2'),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
    )
    .expect("valid request");

    assert_eq!(
        TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation: 1,
                source: 2,
                projection: 3,
                index: 4,
                summary: 5,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'),)
                    .expect("valid digest"),
            },
            None,
            ValidatedAuthorization::Unauthorized,
        ),
        Err(TemporalPortError::UnauthorizedSnapshot)
    );
}

#[test]
fn cursor_key_provider_requires_at_least_256_bits_and_redacts_debug() {
    let key_ref = SignedCursorKeyRefV1 {
        key_id: tracedecay_domain::SessionCursorKeyIdV1::new("key-1").expect("valid key id"),
        version: tracedecay_domain::SessionCursorVersionV1::new(1).expect("valid key version"),
    };
    assert!(matches!(
        InMemoryCursorAuthenticator::new(key_ref.clone(), vec![7; 31]),
        Err(CursorKeyError::InvalidMaterial)
    ));
    assert!(matches!(
        InMemoryCursorAuthenticator::new(key_ref.clone(), vec![7; MAX_CURSOR_SECRET_BYTES + 1]),
        Err(CursorKeyError::InvalidMaterial)
    ));
    let provider =
        InMemoryCursorAuthenticator::new(key_ref, vec![7; 32]).expect("256-bit key is valid");
    let debug = format!("{provider:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("[7, 7"));
}

fn anchor(value: &str) -> RetrievalAnchorId {
    serde_json::from_str(&format!("\"{value}\"")).expect("valid anchor")
}

fn candidate(stable_id: impl Into<String>) -> RankingCandidate {
    RankingCandidate {
        stable_id: stable_id.into(),
        anchor_id: anchor("anchor-1"),
        retriever_record_id: "record-1".to_string(),
        channel: CandidateChannel::Phrase,
        raw_score: 10,
        knowledge_at_micros: 7,
        logical_message: Some("logical-1".to_string()),
        turn: Some("turn-1".to_string()),
        session: Some("session-1".to_string()),
        source: Some("source-1".to_string()),
        evidence_role: Some("message".to_string()),
        exact_ranges: Vec::new(),
        participant_generation: 1,
    }
}

#[test]
fn prepared_candidate_cohort_digest_binds_order_and_generation() {
    let first = candidate("first");
    let mut second = candidate("second");
    second.anchor_id = anchor("anchor-2");
    second.retriever_record_id = "record-2".to_string();

    let original =
        TemporalPreparedCandidateCohort::new(vec![first.clone(), second.clone()]).expect("cohort");
    let reordered = TemporalPreparedCandidateCohort::new(vec![second.clone(), first.clone()])
        .expect("reordered cohort");
    let mut changed_generation = second;
    changed_generation.participant_generation += 1;
    let changed = TemporalPreparedCandidateCohort::new(vec![first, changed_generation])
        .expect("changed generation cohort");

    assert_ne!(original.ordered_digest(), reordered.ordered_digest());
    assert_ne!(original.ordered_digest(), changed.ordered_digest());
}

fn snapshot_with_control(control: ExecutionControl) -> TemporalExecutionSnapshot {
    let request = TemporalSnapshotRequest::new(
        session_id(),
        digest('0'),
        digest('1'),
        digest('2'),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
    )
    .expect("valid request")
    .with_execution_control(control);
    TemporalExecutionSnapshot::new_authorized(
        request,
        TemporalWatermarks {
            generation: 1,
            source: 2,
            projection: 3,
            index: 4,
            summary: 5,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                .expect("valid digest"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("valid snapshot")
}

struct PagingPort {
    calls: AtomicUsize,
}

impl TemporalReadPort for PagingPort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let all = ["candidate-0", "candidate-1", "candidate-2"];
            let start = request
                .keyset()
                .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
            for stable_id in all.iter().skip(start).take(request.page_item_limit()) {
                sink.push(candidate(*stable_id))?;
            }
            Ok(if start + sink.len() < all.len() {
                sink.set_continuation_key(PageKey::new((start + sink.len()).to_string()))?;
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
        _request: PageRequest,
        _sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async { Ok(PageStatus::Complete) })
    }
}

#[test]
fn bounded_async_pull_streams_multiple_pages_without_preloaded_vecs() {
    block_on(async {
        let port = PagingPort {
            calls: AtomicUsize::new(0),
        };
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let limits = PageLimits::new(3, 16 * 1024, 4 * 1024, 1).expect("valid limits");
        let mut state = CandidateReadState::new(limits);
        let plan = CandidatePlan::default();
        let mut stable_ids = Vec::new();

        loop {
            let page = pull_candidate_page(&port, &snapshot, &plan, &mut state)
                .await
                .expect("bounded page");
            let status = page.status();
            stable_ids.extend(page.into_items().into_iter().map(|value| value.stable_id));
            if status == PageStatus::Complete {
                break;
            }
        }

        assert_eq!(stable_ids, ["candidate-0", "candidate-1", "candidate-2"]);
        assert_eq!(port.calls.load(Ordering::SeqCst), 3);
    });
}

struct OversizedPort;

struct PreparationFromReadPort<'a> {
    port: &'a dyn TemporalReadPort,
    snapshot: &'a TemporalExecutionSnapshot,
    plan: &'a CandidatePlan,
}

impl TemporalCandidatePreparationPort for PreparationFromReadPort<'_> {
    fn produce_prepared_candidate_page<'a>(
        &'a self,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        self.port
            .produce_candidate_page(self.snapshot, self.plan, request, sink)
    }
}

impl TemporalReadPort for OversizedPort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        _request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            sink.push(candidate("x".repeat(1024)))?;
            Ok(PageStatus::Complete)
        })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        _request: PageRequest,
        _sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async { Ok(PageStatus::Complete) })
    }
}

#[test]
fn prepared_cohort_preserves_typed_candidate_byte_budget_failure() {
    block_on(async {
        let limits = ExecutionLimits {
            candidate_limit: 1,
            candidate_total_bytes: 128,
            candidate_item_bytes: 128,
            ..ExecutionLimits::default()
        };
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let request = snapshot.request().clone().with_limits(limits);
        let plan = CandidatePlan::default();
        let port = PreparationFromReadPort {
            port: &OversizedPort,
            snapshot: &snapshot,
            plan: &plan,
        };

        assert_eq!(
            prepare_temporal_candidate_cohort(&request, &port).await,
            Err(TemporalPortError::BudgetExceeded {
                resource: "candidate item bytes",
                accounting: Some(ReadBudgetAccounting::requested(128, 1_313)),
            })
        );
    });
}

struct OverproducingPort;

impl TemporalReadPort for OverproducingPort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        _request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            sink.push(candidate("first"))?;
            sink.push(candidate("second"))?;
            Ok(PageStatus::Complete)
        })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        _request: PageRequest,
        _sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async { Ok(PageStatus::Complete) })
    }
}

#[test]
fn sink_rejects_producer_that_ignores_item_and_page_limits() {
    block_on(async {
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let mut state =
            CandidateReadState::new(PageLimits::new(1, 4096, 4096, 1).expect("valid limits"));

        assert_eq!(
            pull_candidate_page(
                &OverproducingPort,
                &snapshot,
                &CandidatePlan::default(),
                &mut state,
            )
            .await,
            Err(TemporalPortError::BudgetExceeded {
                resource: "candidate item count",
                accounting: Some(ReadBudgetAccounting::consumed_with_more(1, 1)),
            })
        );
    });
}

struct CancellingPort {
    control: ExecutionControl,
    entered: Arc<AtomicBool>,
}

impl TemporalReadPort for CancellingPort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        _request: PageRequest,
        _sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        let control = self.control.clone();
        let entered = Arc::clone(&self.entered);
        Box::pin(async move {
            entered.store(true, Ordering::Release);
            control.cancel();
            Ok(PageStatus::Complete)
        })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        _request: PageRequest,
        _sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async { Ok(PageStatus::Complete) })
    }
}

#[test]
fn async_pull_observes_live_cancellation_midstream() {
    block_on(async {
        let control = ExecutionControl::default();
        let snapshot = snapshot_with_control(control.clone());
        let mut state =
            CandidateReadState::new(PageLimits::new(1, 1024, 1024, 1).expect("valid limits"));
        let entered = Arc::new(AtomicBool::new(false));
        let port = CancellingPort {
            control,
            entered: Arc::clone(&entered),
        };

        let result =
            pull_candidate_page(&port, &snapshot, &CandidatePlan::default(), &mut state).await;

        assert!(entered.load(Ordering::Acquire));
        assert_eq!(result, Err(TemporalPortError::Cancelled));
    });
}

#[test]
fn prepared_cohort_preserves_live_cancellation() {
    block_on(async {
        let control = ExecutionControl::default();
        let snapshot = snapshot_with_control(control.clone());
        let entered = Arc::new(AtomicBool::new(false));
        let producer = CancellingPort {
            control,
            entered: Arc::clone(&entered),
        };
        let plan = CandidatePlan::default();
        let port = PreparationFromReadPort {
            port: &producer,
            snapshot: &snapshot,
            plan: &plan,
        };

        let result = prepare_temporal_candidate_cohort(snapshot.request(), &port).await;

        assert!(entered.load(Ordering::Acquire));
        assert_eq!(result, Err(TemporalPortError::Cancelled));
    });
}

struct DeadlineCrossingPort {
    deadline: Instant,
    entered: Arc<AtomicBool>,
}

impl TemporalReadPort for DeadlineCrossingPort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        _request: PageRequest,
        _sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        let deadline = self.deadline;
        let entered = Arc::clone(&self.entered);
        Box::pin(async move {
            entered.store(true, Ordering::Release);
            while Instant::now() < deadline {
                std::hint::spin_loop();
            }
            Ok(PageStatus::Complete)
        })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        _request: PageRequest,
        _sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async { Ok(PageStatus::Complete) })
    }
}

#[test]
fn prepared_cohort_preserves_deadline_after_producer_work() {
    block_on(async {
        let deadline = Instant::now() + Duration::from_millis(100);
        let snapshot = snapshot_with_control(ExecutionControl::new(Some(deadline)));
        let entered = Arc::new(AtomicBool::new(false));
        let producer = DeadlineCrossingPort {
            deadline,
            entered: Arc::clone(&entered),
        };
        let plan = CandidatePlan::default();
        let port = PreparationFromReadPort {
            port: &producer,
            snapshot: &snapshot,
            plan: &plan,
        };

        let result = prepare_temporal_candidate_cohort(snapshot.request(), &port).await;

        assert!(entered.load(Ordering::Acquire));
        assert_eq!(result, Err(TemporalPortError::DeadlineExceeded));
    });
}

fn summary_record(anchor_id: &str) -> TemporalRecord {
    TemporalRecord::SummarySource(SummarySourceRecord {
        anchor_id: anchor(anchor_id),
        state: SummarySourceState::Missing,
    })
}

/// Producer that always reports More after filling at most one item, with a
/// stable continuation — used to prove caps cannot downgrade More → Complete.
struct AlwaysMorePort {
    candidate_ids: Vec<&'static str>,
    record_anchors: Vec<&'static str>,
}

impl AlwaysMorePort {
    fn new(candidate_ids: Vec<&'static str>, record_anchors: Vec<&'static str>) -> Self {
        Self {
            candidate_ids,
            record_anchors,
        }
    }
}

impl TemporalReadPort for AlwaysMorePort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            let start = request
                .keyset()
                .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
            if let Some(stable_id) = self.candidate_ids.get(start) {
                sink.push(candidate(*stable_id))?;
            }
            sink.set_continuation_key(PageKey::new((start + 1).to_string()))?;
            Ok(PageStatus::More)
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
            let start = request
                .keyset()
                .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
            if let Some(anchor_id) = self.record_anchors.get(start) {
                sink.push(summary_record(anchor_id))?;
            }
            sink.set_continuation_key(PageKey::new((start + 1).to_string()))?;
            Ok(PageStatus::More)
        })
    }
}

struct ExactCompletePort {
    candidates: Vec<&'static str>,
    records: Vec<&'static str>,
}

impl TemporalReadPort for ExactCompletePort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            let start = request
                .keyset()
                .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
            let end = (start + request.page_item_limit()).min(self.candidates.len());
            for stable_id in &self.candidates[start..end] {
                sink.push(candidate(*stable_id))?;
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
            let start = request
                .keyset()
                .map_or(0, |key| key.as_str().parse::<usize>().expect("numeric key"));
            let end = (start + request.page_item_limit()).min(self.records.len());
            for anchor_id in &self.records[start..end] {
                sink.push(summary_record(anchor_id))?;
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

struct OversizedRecordPort;

impl TemporalReadPort for OversizedRecordPort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        _request: PageRequest,
        _sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async { Ok(PageStatus::Complete) })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        _request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            // Inflate measured JSON size via a long anchor id.
            sink.push(summary_record(&"r".repeat(512)))?;
            Ok(PageStatus::Complete)
        })
    }
}

/// A candidate cohort is a bounded window over storage order, so filling the
/// item cap while the producer still holds rows yields the window plus the key
/// the next window resumes from — never a refusal that hides the remainder.
#[test]
fn candidate_item_cap_with_producer_more_is_a_resumable_window() {
    block_on(async {
        let port = AlwaysMorePort::new(vec!["c0", "c1"], Vec::new());
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let mut state =
            CandidateReadState::new(PageLimits::new(1, 16 * 1024, 4 * 1024, 1).expect("limits"));

        let page = pull_candidate_page(&port, &snapshot, &CandidatePlan::default(), &mut state)
            .await
            .expect("a full item cap is a bounded window, not incomplete coverage");
        assert_eq!(page.status(), PageStatus::More);
        assert_eq!(page.items().len(), 1);
        assert_eq!(state.consumed_items(), 1);
        assert!(state.is_exhausted());
        assert_eq!(
            state.keyset().map(|key| key.as_str().to_owned()),
            Some("1".to_owned())
        );
    });
}

#[test]
fn candidate_total_bytes_cap_with_producer_more_is_a_resumable_window() {
    block_on(async {
        let first = candidate("c0");
        let encoded = first.measured_encoded_bytes().expect("measured");
        let port = AlwaysMorePort::new(vec!["c0", "c1"], Vec::new());
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let mut state =
            CandidateReadState::new(PageLimits::new(8, encoded, encoded, 1).expect("limits"));

        let page = pull_candidate_page(&port, &snapshot, &CandidatePlan::default(), &mut state)
            .await
            .expect("a full byte cap is a bounded window, not incomplete coverage");
        assert_eq!(page.status(), PageStatus::More);
        assert_eq!(state.consumed_bytes(), encoded);
        assert!(state.consumed_items() < 8);
        assert!(state.is_exhausted());
        assert_eq!(
            state.keyset().map(|key| key.as_str().to_owned()),
            Some("1".to_owned())
        );
    });
}

#[test]
fn record_total_bytes_cap_with_producer_more_is_incomplete_coverage() {
    block_on(async {
        let first = summary_record("r0");
        let encoded = first.measured_encoded_bytes().expect("measured");
        let port = AlwaysMorePort::new(Vec::new(), vec!["r0", "r1"]);
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let mut state =
            TemporalRecordReadState::new(PageLimits::new(8, encoded, encoded, 1).expect("limits"));

        match pull_temporal_record_page(&port, &snapshot, &[], &mut state).await {
            Err(error) => assert_eq!(
                error,
                TemporalPortError::BudgetExceeded {
                    resource: "record total bytes",
                    accounting: Some(ReadBudgetAccounting::consumed_with_more(33, 33)),
                }
            ),
            Ok(_) => panic!("More + record total-byte cap must be incomplete coverage"),
        }
        assert_eq!(state.consumed_bytes(), encoded);
        assert!(state.consumed_items() < 8);
    });
}

#[test]
fn record_item_bytes_cap_fails_closed_without_complete() {
    block_on(async {
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let mut state =
            TemporalRecordReadState::new(PageLimits::new(2, 16 * 1024, 64, 2).expect("limits"));

        match pull_temporal_record_page(&OversizedRecordPort, &snapshot, &[], &mut state).await {
            Err(error) => assert_eq!(
                error,
                TemporalPortError::BudgetExceeded {
                    resource: "record item bytes",
                    accounting: Some(ReadBudgetAccounting::requested(64, 543)),
                }
            ),
            Ok(_) => panic!("oversized record must fail closed"),
        }
        assert_eq!(state.consumed_items(), 0);
    });
}

#[test]
fn producer_complete_at_exact_item_cap_remains_complete_for_candidates_and_records() {
    block_on(async {
        let port = ExactCompletePort {
            candidates: vec!["c0", "c1"],
            records: vec!["r0", "r1"],
        };
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let mut candidate_state =
            CandidateReadState::new(PageLimits::new(2, 16 * 1024, 4 * 1024, 2).expect("limits"));
        let mut record_state = TemporalRecordReadState::new(
            PageLimits::new(2, 16 * 1024, 4 * 1024, 2).expect("limits"),
        );

        let candidates = pull_candidate_page(
            &port,
            &snapshot,
            &CandidatePlan::default(),
            &mut candidate_state,
        )
        .await
        .expect("exact candidate page");
        assert_eq!(candidates.status(), PageStatus::Complete);
        assert_eq!(candidates.continuation(), None);
        assert_eq!(candidates.items().len(), 2);

        let records = pull_temporal_record_page(&port, &snapshot, &[], &mut record_state)
            .await
            .expect("exact record page");
        assert_eq!(records.status(), PageStatus::Complete);
        assert_eq!(records.continuation(), None);
        assert_eq!(records.items().len(), 2);
    });
}

#[test]
fn more_under_non_exhausted_limits_preserves_continuation() {
    block_on(async {
        let port = ExactCompletePort {
            candidates: vec!["c0", "c1", "c2"],
            records: vec!["r0", "r1", "r2"],
        };
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let mut candidate_state =
            CandidateReadState::new(PageLimits::new(8, 16 * 1024, 4 * 1024, 1).expect("limits"));
        let mut record_state = TemporalRecordReadState::new(
            PageLimits::new(8, 16 * 1024, 4 * 1024, 1).expect("limits"),
        );

        let first_candidates = pull_candidate_page(
            &port,
            &snapshot,
            &CandidatePlan::default(),
            &mut candidate_state,
        )
        .await
        .expect("first candidate page");
        assert_eq!(first_candidates.status(), PageStatus::More);
        assert_eq!(
            first_candidates.continuation().map(PageKey::as_str),
            Some("1")
        );

        let second_candidates = pull_candidate_page(
            &port,
            &snapshot,
            &CandidatePlan::default(),
            &mut candidate_state,
        )
        .await
        .expect("second candidate page");
        assert_eq!(second_candidates.status(), PageStatus::More);
        assert_eq!(
            second_candidates.items()[0].stable_id.as_str(),
            "c1",
            "continuation must not skip or drop candidates"
        );

        let first_records = pull_temporal_record_page(&port, &snapshot, &[], &mut record_state)
            .await
            .expect("first record page");
        assert_eq!(first_records.status(), PageStatus::More);
        assert_eq!(first_records.continuation().map(PageKey::as_str), Some("1"));

        let second_records = pull_temporal_record_page(&port, &snapshot, &[], &mut record_state)
            .await
            .expect("second record page");
        assert_eq!(second_records.status(), PageStatus::More);
        match &second_records.items()[0] {
            TemporalRecord::SummarySource(record) => {
                assert_eq!(
                    record.anchor_id.to_string(),
                    "r1",
                    "continuation must not skip or drop records"
                );
            }
            _ => panic!("expected summary source record"),
        }
    });
}

#[test]
fn exhausted_caps_never_synthesize_complete_or_silently_drop_unread_work() {
    block_on(async {
        let port = AlwaysMorePort::new(vec!["c0", "c1", "c2"], vec!["r0", "r1", "r2"]);
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let mut candidate_state =
            CandidateReadState::new(PageLimits::new(1, 16 * 1024, 4 * 1024, 1).expect("limits"));
        let mut record_state = TemporalRecordReadState::new(
            PageLimits::new(1, 16 * 1024, 4 * 1024, 1).expect("limits"),
        );

        let candidate_window = pull_candidate_page(
            &port,
            &snapshot,
            &CandidatePlan::default(),
            &mut candidate_state,
        )
        .await
        .expect("More + candidate cap is a bounded window");
        assert_eq!(candidate_window.status(), PageStatus::More);
        assert!(candidate_state.is_exhausted());
        // The window is closed: a follow-up pull on the same exhausted state
        // must keep failing closed — never an empty Complete that would drop
        // the rows the continuation key still owes.
        let candidate_follow_up = pull_candidate_page(
            &port,
            &snapshot,
            &CandidatePlan::default(),
            &mut candidate_state,
        )
        .await
        .expect_err("exhausted candidate state must not synthesize Complete");
        assert_eq!(
            candidate_follow_up,
            TemporalPortError::BudgetExceeded {
                resource: "candidate item count",
                accounting: Some(ReadBudgetAccounting::consumed_with_more(1, 1)),
            }
        );
        assert_ne!(
            candidate_follow_up,
            TemporalPortError::Read {
                operation: "produce bounded page",
                message: "producer returned an empty continuation page".to_string(),
            }
        );

        let Err(record_err) =
            pull_temporal_record_page(&port, &snapshot, &[], &mut record_state).await
        else {
            panic!("More + record cap must not complete");
        };
        assert_eq!(
            record_err,
            TemporalPortError::BudgetExceeded {
                resource: "record item count",
                accounting: Some(ReadBudgetAccounting::consumed_with_more(1, 1)),
            }
        );
        let Err(record_follow_up) =
            pull_temporal_record_page(&port, &snapshot, &[], &mut record_state).await
        else {
            panic!("exhausted record state must not synthesize Complete");
        };
        assert_eq!(
            record_follow_up,
            TemporalPortError::BudgetExceeded {
                resource: "record item count",
                accounting: Some(ReadBudgetAccounting::consumed_with_more(1, 1)),
            }
        );
    });
}

#[test]
fn page_limits_reject_zero_inverted_and_absolute_ceilings() {
    assert_eq!(
        PageLimits::new(0, 1024, 1024, 1),
        Err(TemporalPortError::BudgetExceeded {
            resource: "item count", accounting: None
        })
    );
    assert_eq!(
        PageLimits::new(1, 0, 1024, 1),
        Err(TemporalPortError::BudgetExceeded {
            resource: "total bytes", accounting: None
        })
    );
    assert_eq!(
        PageLimits::new(1, 1024, 0, 1),
        Err(TemporalPortError::BudgetExceeded {
            resource: "item bytes", accounting: None
        })
    );
    assert_eq!(
        PageLimits::new(1, 1024, 1024, 2),
        Err(TemporalPortError::BudgetExceeded {
            resource: "page item count",
            accounting: Some(ReadBudgetAccounting::requested(1, 2)),
        })
    );
    assert_eq!(
        PageLimits::new(usize::MAX, 1024, 1024, 1),
        Err(TemporalPortError::BudgetExceeded {
            resource: "item count",
            accounting: Some(ReadBudgetAccounting::requested(MAX_READ_ITEMS as u64, usize::MAX as u64)),
        })
    );
    assert_eq!(
        PageLimits::new(MAX_READ_ITEMS, MAX_READ_TOTAL_BYTES + 1, 1024, 1),
        Err(TemporalPortError::BudgetExceeded {
            resource: "total bytes",
            accounting: Some(ReadBudgetAccounting::requested(
                MAX_READ_TOTAL_BYTES as u64,
                MAX_READ_TOTAL_BYTES as u64 + 1,
            )),
        })
    );
    assert!(
        PageLimits::new(1, 1024, 1024, 1).is_ok(),
        "canonical small limits must remain accepted"
    );
}

#[test]
fn execution_limits_reject_zero_and_absolute_ceilings() {
    let oversize = ExecutionLimits {
        candidate_limit: MAX_READ_ITEMS + 1,
        ..ExecutionLimits::default()
    };
    assert_eq!(
        oversize.validate(),
        Err(TemporalPortError::BudgetExceeded {
            resource: "candidate item count",
            accounting: Some(ReadBudgetAccounting::requested(
                MAX_READ_ITEMS as u64,
                MAX_READ_ITEMS as u64 + 1,
            )),
        })
    );
    let zero = ExecutionLimits {
        record_item_bytes: 0,
        ..ExecutionLimits::default()
    };
    assert_eq!(
        zero.validate(),
        Err(TemporalPortError::BudgetExceeded {
            resource: "record item bytes", accounting: None
        })
    );
    assert!(ExecutionLimits::default().validate().is_ok());
}

#[test]
fn execution_snapshot_rejects_oversize_execution_limits() {
    let limits = ExecutionLimits {
        candidate_limit: MAX_READ_ITEMS + 1,
        ..ExecutionLimits::default()
    };
    let request = TemporalSnapshotRequest::new(
        session_id(),
        digest('0'),
        digest('1'),
        digest('2'),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
    )
    .expect("valid request")
    .with_limits(limits);
    assert_eq!(
        TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation: 1,
                source: 0,
                projection: 0,
                index: 0,
                summary: 0,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            None,
            ValidatedAuthorization::Authorized,
        ),
        Err(TemporalPortError::BudgetExceeded {
            resource: "candidate item count",
            accounting: Some(ReadBudgetAccounting::requested(
                MAX_READ_ITEMS as u64,
                MAX_READ_ITEMS as u64 + 1,
            )),
        })
    );
}

type LimitGetter = fn(&ExecutionLimits) -> usize;
type LimitSetter = fn(&mut ExecutionLimits, usize);

fn execution_limit_fields() -> [(&'static str, LimitGetter, LimitSetter); 15] {
    [
        (
            "candidate_limit",
            |limits| limits.candidate_limit,
            |limits, value| limits.candidate_limit = value,
        ),
        (
            "candidate_total_bytes",
            |limits| limits.candidate_total_bytes,
            |limits, value| limits.candidate_total_bytes = value,
        ),
        (
            "candidate_item_bytes",
            |limits| limits.candidate_item_bytes,
            |limits, value| limits.candidate_item_bytes = value,
        ),
        (
            "candidate_key_bytes",
            |limits| limits.candidate_key_bytes,
            |limits, value| limits.candidate_key_bytes = value,
        ),
        (
            "candidate_stable_id_bytes",
            |limits| limits.candidate_stable_id_bytes,
            |limits, value| limits.candidate_stable_id_bytes = value,
        ),
        (
            "candidate_anchor_id_bytes",
            |limits| limits.candidate_anchor_id_bytes,
            |limits, value| limits.candidate_anchor_id_bytes = value,
        ),
        (
            "candidate_metadata_field_bytes",
            |limits| limits.candidate_metadata_field_bytes,
            |limits, value| limits.candidate_metadata_field_bytes = value,
        ),
        (
            "record_limit",
            |limits| limits.record_limit,
            |limits, value| limits.record_limit = value,
        ),
        (
            "record_total_bytes",
            |limits| limits.record_total_bytes,
            |limits, value| limits.record_total_bytes = value,
        ),
        (
            "record_item_bytes",
            |limits| limits.record_item_bytes,
            |limits, value| limits.record_item_bytes = value,
        ),
        (
            "record_key_bytes",
            |limits| limits.record_key_bytes,
            |limits, value| limits.record_key_bytes = value,
        ),
        (
            "hydration_limit",
            |limits| limits.hydration_limit,
            |limits, value| limits.hydration_limit = value,
        ),
        (
            "hydration_total_bytes",
            |limits| limits.hydration_total_bytes,
            |limits, value| limits.hydration_total_bytes = value,
        ),
        (
            "hydration_payload_bytes",
            |limits| limits.hydration_payload_bytes,
            |limits, value| limits.hydration_payload_bytes = value,
        ),
        (
            "hydration_chunk_bytes",
            |limits| limits.hydration_chunk_bytes,
            |limits, value| limits.hydration_chunk_bytes = value,
        ),
    ]
}

fn snapshot_with_limits(limits: ExecutionLimits) -> TemporalExecutionSnapshot {
    let request = TemporalSnapshotRequest::new(
        session_id(),
        digest('0'),
        digest('1'),
        digest('2'),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
    )
    .expect("valid request")
    .with_limits(limits);
    TemporalExecutionSnapshot::new_authorized(
        request,
        TemporalWatermarks {
            generation: 1,
            source: 2,
            projection: 3,
            index: 4,
            summary: 5,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                .expect("valid digest"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("valid authorized snapshot")
}

#[test]
fn snapshot_limit_tightening_is_monotonic_for_every_field() {
    let authorized = ExecutionLimits::default();

    for (field, get, set) in execution_limit_fields() {
        let authorized_value = get(&authorized);

        let mut tighter = authorized;
        set(&mut tighter, authorized_value - 1);
        let tightened = snapshot_with_limits(authorized)
            .with_limits(tighter)
            .expect("a valid component-wise decrease must succeed");
        assert_eq!(
            tightened.request().limits(),
            tighter,
            "tightening `{field}` must preserve the requested lower value"
        );
        assert_eq!(
            tightened.authorization(),
            ValidatedAuthorization::Authorized,
            "tightening `{field}` must preserve authorization"
        );

        let mut looser = authorized;
        set(&mut looser, authorized_value + 1);
        assert_eq!(
            snapshot_with_limits(authorized)
                .with_limits(looser)
                .expect_err("a component-wise increase must fail"),
            ExecutionLimitTighteningError::WouldLoosen {
                field,
                authorized: authorized_value,
                requested: authorized_value + 1,
            }
        );
    }
}

#[test]
fn snapshot_limit_tightening_accepts_equal_limits() {
    let limits = ExecutionLimits::default();
    let snapshot = snapshot_with_limits(limits)
        .with_limits(limits)
        .expect("equal limits are monotonic");

    assert_eq!(snapshot.request().limits(), limits);
    assert_eq!(snapshot.authorization(), ValidatedAuthorization::Authorized);
}

struct StableIdPort {
    stable_id: &'static str,
}

impl TemporalReadPort for StableIdPort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        _request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            sink.push(candidate(self.stable_id))?;
            Ok(PageStatus::Complete)
        })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        _request: PageRequest,
        _sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async { Ok(PageStatus::Complete) })
    }
}

#[test]
fn candidate_pull_observes_post_authorization_tightening() {
    block_on(async {
        let authorized = ExecutionLimits::default();
        let mut tighter = authorized;
        tighter.candidate_stable_id_bytes = 4;
        let snapshot = snapshot_with_limits(authorized)
            .with_limits(tighter)
            .expect("valid tightening");
        let mut state =
            CandidateReadState::new(PageLimits::new(1, 16 * 1024, 4 * 1024, 1).expect("limits"));

        assert_eq!(
            pull_candidate_page(
                &StableIdPort { stable_id: "12345" },
                &snapshot,
                &CandidatePlan::default(),
                &mut state,
            )
            .await,
            Err(TemporalPortError::BudgetExceeded {
                resource: "candidate stable id bytes", accounting: None
            })
        );
    });
}

struct UnreachableReadPort;

impl TemporalReadPort for UnreachableReadPort {
    fn produce_candidate_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _plan: &'a CandidatePlan,
        _request: PageRequest,
        _sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async { panic!("looser candidate read state reached the producer") })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _candidates: &'a [RankingCandidate],
        _request: PageRequest,
        _sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async { panic!("looser record read state reached the producer") })
    }
}

#[test]
fn pull_rejects_read_state_looser_than_tightened_snapshot() {
    block_on(async {
        // The admitted snapshot each looser read state is measured against; the
        // refusal must name these ceilings, not the read state's own.
        const ADMITTED_ITEMS: u64 = 1;
        const ADMITTED_BYTES: u64 = 128;
        const ADMITTED_ITEM_BYTES: u64 = 64;
        let authorized = ExecutionLimits::default();
        let mut tighter = authorized;
        tighter.candidate_limit = ADMITTED_ITEMS as usize;
        tighter.candidate_total_bytes = ADMITTED_BYTES as usize;
        tighter.candidate_item_bytes = ADMITTED_ITEM_BYTES as usize;
        tighter.record_limit = ADMITTED_ITEMS as usize;
        tighter.record_total_bytes = ADMITTED_BYTES as usize;
        tighter.record_item_bytes = ADMITTED_ITEM_BYTES as usize;
        let snapshot = snapshot_with_limits(authorized)
            .with_limits(tighter)
            .expect("valid tightening");

        // The refusal names the admitted ceiling and what the state asked for,
        // so a caller can correct the request instead of guessing at a name.
        let candidate_cases = [
            ("candidate item count", PageLimits::new(2, 128, 64, 1), ADMITTED_ITEMS, 2),
            ("candidate total bytes", PageLimits::new(1, 129, 64, 1), ADMITTED_BYTES, 129),
            ("candidate item bytes", PageLimits::new(1, 128, 65, 1), ADMITTED_ITEM_BYTES, 65),
        ];
        for (resource, limits, admitted, asked) in candidate_cases {
            let limits = limits.expect("looser page limits");
            let mut state = CandidateReadState::new(limits);
            assert_eq!(
                pull_candidate_page(
                    &UnreachableReadPort,
                    &snapshot,
                    &CandidatePlan::default(),
                    &mut state,
                )
                .await,
                Err(TemporalPortError::BudgetExceeded {
                    resource,
                    accounting: Some(ReadBudgetAccounting::requested(admitted, asked)),
                })
            );
        }

        let record_cases = [
            ("record item count", PageLimits::new(2, 128, 64, 1), ADMITTED_ITEMS, 2),
            ("record total bytes", PageLimits::new(1, 129, 64, 1), ADMITTED_BYTES, 129),
            ("record item bytes", PageLimits::new(1, 128, 65, 1), ADMITTED_ITEM_BYTES, 65),
        ];
        for (resource, limits, admitted, asked) in record_cases {
            let limits = limits.expect("looser page limits");
            let mut state = TemporalRecordReadState::new(limits);
            let Err(error) =
                pull_temporal_record_page(&UnreachableReadPort, &snapshot, &[], &mut state).await
            else {
                panic!("looser record state must fail before producer entry");
            };
            assert_eq!(
                error,
                TemporalPortError::BudgetExceeded {
                    resource,
                    accounting: Some(ReadBudgetAccounting::requested(admitted, asked)),
                }
            );
        }
    });
}

#[test]
fn bounded_page_sink_caps_initial_capacity_for_attacker_limits() {
    let limits =
        PageLimits::new(MAX_PAGE_ITEMS_CAP, 1024, 1024, MAX_PAGE_ITEMS_CAP).expect("limits");
    let mut state = CandidateReadState::new(limits);
    let control = ExecutionControl::default();
    let sink = state.begin_page(&control, 256, None, CANDIDATE_READ_BUDGET);
    assert!(sink.preallocated_capacity() <= MAX_BOUNDED_PAGE_PREALLOC);
    assert!(sink.preallocated_capacity() <= MAX_PAGE_ITEMS_CAP);
}

#[test]
fn continuation_key_enforces_exact_byte_cap() {
    block_on(async {
        struct ContinuationPort {
            key_len: usize,
        }
        impl TemporalReadPort for ContinuationPort {
            fn produce_candidate_page<'a>(
                &'a self,
                _snapshot: &'a TemporalExecutionSnapshot,
                _plan: &'a CandidatePlan,
                _request: PageRequest,
                sink: &'a mut CandidatePageSink<'_>,
            ) -> PortFuture<'a, PageStatus> {
                Box::pin(async move {
                    sink.push(candidate("c0"))?;
                    sink.set_continuation_key(PageKey::new("k".repeat(self.key_len)))?;
                    Ok(PageStatus::More)
                })
            }
            fn produce_temporal_record_page<'a>(
                &'a self,
                _snapshot: &'a TemporalExecutionSnapshot,
                _candidates: &'a [RankingCandidate],
                _request: PageRequest,
                _sink: &'a mut TemporalRecordPageSink<'_>,
            ) -> PortFuture<'a, PageStatus> {
                Box::pin(async { Ok(PageStatus::Complete) })
            }
        }
        let snapshot = snapshot_with_control(ExecutionControl::default());
        let mut ok_state =
            CandidateReadState::new(PageLimits::new(8, 16 * 1024, 4 * 1024, 1).expect("limits"));
        pull_candidate_page(
            &ContinuationPort { key_len: 256 },
            &snapshot,
            &CandidatePlan::default(),
            &mut ok_state,
        )
        .await
        .expect("key at default cap");

        let mut over_state =
            CandidateReadState::new(PageLimits::new(8, 16 * 1024, 4 * 1024, 1).expect("limits"));
        assert_eq!(
            pull_candidate_page(
                &ContinuationPort { key_len: 257 },
                &snapshot,
                &CandidatePlan::default(),
                &mut over_state,
            )
            .await,
            Err(TemporalPortError::BudgetExceeded {
                resource: "continuation key bytes",
                accounting: Some(ReadBudgetAccounting::requested(256, 257)),
            })
        );
    });
}

#[test]
fn legacy_only_port_fails_closed_for_root_wide_scope() {
    block_on(async {
        struct LegacyOnlyPort;
        impl TemporalReadPort for LegacyOnlyPort {
            fn produce_candidate_page<'a>(
                &'a self,
                _snapshot: &'a TemporalExecutionSnapshot,
                _plan: &'a CandidatePlan,
                _request: PageRequest,
                _sink: &'a mut CandidatePageSink<'_>,
            ) -> PortFuture<'a, PageStatus> {
                Box::pin(async { Ok(PageStatus::Complete) })
            }
            fn produce_temporal_record_page<'a>(
                &'a self,
                _snapshot: &'a TemporalExecutionSnapshot,
                _candidates: &'a [RankingCandidate],
                _request: PageRequest,
                _sink: &'a mut TemporalRecordPageSink<'_>,
            ) -> PortFuture<'a, PageStatus> {
                Box::pin(async { Ok(PageStatus::Complete) })
            }
        }
        let request = TemporalSnapshotRequest::new(
            session_id(),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("valid request")
        .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot);
        let snapshot = TemporalExecutionSnapshot::new(
            request,
            TemporalWatermarks {
                generation: 1,
                source: 0,
                projection: 0,
                index: 0,
                summary: 0,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            None,
        )
        .expect("valid snapshot");
        let mut candidate_state =
            CandidateReadState::new(PageLimits::new(1, 1024, 1024, 1).expect("limits"));
        let err = pull_candidate_page(
            &LegacyOnlyPort,
            &snapshot,
            &CandidatePlan::default(),
            &mut candidate_state,
        )
        .await
        .expect_err("root-wide must not use silent legacy default");
        assert!(matches!(
            err,
            TemporalPortError::Read {
                operation: "produce candidate page for scope",
                ..
            }
        ));
    });
}

#[test]
fn participant_manifest_reports_mixed_source_freshness_from_real_frontiers() {
    let configuration = BindingDigest::new("configuration_digest", digest('3')).expect("digest");
    let authorization = BindingDigest::new("authorization_digest", digest('4')).expect("digest");
    let participant = |source: &str, source_watermark, projection_watermark| {
        TemporalParticipantGeneration::new(
            SessionId::new(format!("session.{source}")).unwrap(),
            source,
            TemporalWatermarks {
                generation: 1,
                source: source_watermark,
                projection: projection_watermark,
                index: projection_watermark,
                summary: 0,
            },
            projection_watermark,
            &configuration,
            &authorization,
            TemporalParticipantAuthorization::Authorized,
            TemporalSourceAccess::Available,
        )
        .unwrap()
    };
    let manifest = TemporalParticipantManifest::new(vec![
        participant("cursor", 10, 10),
        participant("claude", 10, 7),
    ])
    .unwrap();

    let receipt = manifest
        .source_coverage(TemporalModeV1::Current)
        .expect("source coverage");
    assert_eq!(receipt.sources().len(), 2);
    assert_eq!(
        receipt.aggregate_state(),
        tracedecay_domain::SessionSourceCoverageAggregateStateV1::Partial
    );
    assert_eq!(receipt.max_frontier_lag(), 3);
}

#[test]
fn authorized_lifecycle_states_do_not_become_snapshot_denials() {
    let configuration = BindingDigest::new("configuration_digest", digest('3')).expect("digest");
    let authorization = BindingDigest::new("authorization_digest", digest('4')).expect("digest");
    for (access, expected_coverage) in [
        (
            TemporalSourceAccess::Locked,
            SessionSourceCoverageStateV1::Locked,
        ),
        (
            TemporalSourceAccess::RetentionWithheld,
            SessionSourceCoverageStateV1::RetentionWithheld,
        ),
        (
            TemporalSourceAccess::Deleted,
            SessionSourceCoverageStateV1::RetentionWithheld,
        ),
        (
            TemporalSourceAccess::Redacted,
            SessionSourceCoverageStateV1::Redacted,
        ),
        (
            TemporalSourceAccess::Unavailable,
            SessionSourceCoverageStateV1::Unavailable,
        ),
    ] {
        let participant = TemporalParticipantGeneration::new(
            SessionId::new("session.lifecycle").unwrap(),
            "claude",
            TemporalWatermarks {
                generation: 1,
                source: 10,
                projection: 10,
                index: 10,
                summary: 10,
            },
            10,
            &configuration,
            &authorization,
            TemporalParticipantAuthorization::Authorized,
            access,
        )
        .unwrap();
        assert!(participant.is_authorized_for_snapshot());
        let coverage = TemporalParticipantManifest::new(vec![participant])
            .unwrap()
            .source_coverage(TemporalModeV1::Current)
            .unwrap();
        assert_eq!(coverage.sources()[0].state(), expected_coverage);
    }
}

#[test]
fn manifests_without_explicit_authorization_fail_closed() {
    let participant = participant("session.stale", "claude", 1);
    let mut wire = serde_json::to_value(participant).unwrap();
    wire.as_object_mut().unwrap().remove("q");
    let stale: TemporalParticipantGeneration = serde_json::from_value(wire).unwrap();

    assert_eq!(
        stale.authorization(),
        TemporalParticipantAuthorization::Denied
    );
    assert!(!stale.is_authorized_for_snapshot());
}
