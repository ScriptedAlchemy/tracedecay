use super::*;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_domain::{RetrievalAnchorId, RetrievalGrainV1, SessionId, TemporalModeV1};
use tracedecay_sessions::lcm::contracts::{LcmDataFreshness, LcmRetrievalOutcome};
use tracedecay_sessions::runtime::SessionMessageRecord;
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, TemporalAuthorizedRoot, TemporalSnapshotRequest,
    TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

struct ReconstructionFixture {
    anchor_id: RetrievalAnchorId,
    provider: String,
    session_id: String,
    content: Vec<u8>,
}

struct RecordingNonSummaryReconstructionBatch {
    snapshot_opens: Arc<AtomicUsize>,
    responses: Mutex<Option<Vec<Result<SessionMessageRecord, SessionTemporalExecutionError>>>>,
}

impl NonSummaryReconstructionBatch for RecordingNonSummaryReconstructionBatch {
    async fn reconstruct_non_summary<'a>(
        &self,
        inputs: Vec<NonSummaryReconstructionInput<'a>>,
    ) -> Result<
        Vec<Result<SessionMessageRecord, SessionTemporalExecutionError>>,
        SessionTemporalExecutionError,
    > {
        assert_eq!(inputs.len(), 50, "one page must be admitted as one batch");
        let root = inputs[0].snapshot.request().authorized_root();
        assert!(root.is_some(), "batch inputs must carry a registered root");
        for (rank, input) in inputs.iter().enumerate() {
            assert_eq!(
                input.snapshot.request().authorized_root(),
                root,
                "mixed registered roots must not share a reconstruction snapshot"
            );
            assert_eq!(
                input.provider,
                if rank % 2 == 0 { "codex" } else { "claude" },
                "the batch retains each message provider identity"
            );
            assert_eq!(
                input.session_id,
                if rank % 2 == 0 {
                    "codex-session"
                } else {
                    "claude-session"
                },
                "the batch retains each message session identity"
            );
            assert_eq!(
                input.content,
                format!("canonical content {rank}").as_bytes(),
                "the reconstruction batch keeps canonical hydrated content"
            );
        }
        if inputs[0].snapshot.request().cancellation_requested() {
            return Err(SessionTemporalExecutionError::Cancelled);
        }
        self.snapshot_opens.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .responses
            .lock()
            .expect("recorded batch responses")
            .take()
            .expect("one authorized batch response"))
    }
}

fn reconstruction_snapshot(cancelled: bool) -> TemporalExecutionSnapshot {
    TemporalExecutionSnapshot::new_authorized(
        TemporalSnapshotRequest::new(
            SessionId::new("codex-session").expect("session"),
            "root.batch",
            "request.batch",
            "access.batch",
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("snapshot request")
        .with_authorized_root(
            TemporalAuthorizedRoot::profile("profile.batch", "store.batch", "root.batch")
                .expect("registered root"),
        )
        .expect("root binding")
        .with_cancellation_requested(cancelled),
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
            configuration_digest: BindingDigest::new("configuration", "batch-test")
                .expect("configuration"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn reconstruction_inputs<'a>(
    snapshot: &'a TemporalExecutionSnapshot,
    fixtures: &'a [ReconstructionFixture],
) -> Vec<NonSummaryReconstructionInput<'a>> {
    fixtures
        .iter()
        .map(|fixture| NonSummaryReconstructionInput {
            snapshot,
            anchor_id: &fixture.anchor_id,
            provider: fixture.provider.as_str(),
            session_id: fixture.session_id.as_str(),
            content: fixture.content.as_slice(),
        })
        .collect()
}

#[tokio::test]
async fn fifty_non_summary_results_share_one_frozen_reconstruction_batch() {
    const RESULTS: usize = 50;
    let fixtures = (0..RESULTS)
        .map(|rank| ReconstructionFixture {
            anchor_id: RetrievalAnchorId::new(format!("anchor.{rank:02}")).expect("anchor"),
            provider: if rank % 2 == 0 {
                "codex".to_string()
            } else {
                "claude".to_string()
            },
            session_id: if rank % 2 == 0 {
                "codex-session".to_string()
            } else {
                "claude-session".to_string()
            },
            content: format!("canonical content {rank}").into_bytes(),
        })
        .collect::<Vec<_>>();
    let responses = fixtures
        .iter()
        .enumerate()
        .map(|(rank, candidate)| match rank {
            11 => Err(SessionTemporalExecutionError::Denied),
            23 => Err(SessionTemporalExecutionError::Redacted),
            31 => Err(SessionTemporalExecutionError::Unavailable),
            _ => Ok(SessionMessageRecord {
                provider: candidate.provider.clone(),
                message_id: format!("message.{rank:02}"),
                session_id: candidate.session_id.clone(),
                role: "assistant".to_string(),
                timestamp: Some(i64::try_from(rank).expect("timestamp")),
                ordinal: i64::try_from(rank).expect("ordinal"),
                text: String::from_utf8(candidate.content.clone()).expect("content"),
                kind: None,
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            }),
        })
        .collect::<Vec<_>>();
    let snapshot_opens = Arc::new(AtomicUsize::new(0));
    let batch = RecordingNonSummaryReconstructionBatch {
        snapshot_opens: Arc::clone(&snapshot_opens),
        responses: Mutex::new(Some(responses)),
    };
    let snapshot = reconstruction_snapshot(false);
    let reconstructed =
        reconstruct_non_summary_results(&batch, reconstruction_inputs(&snapshot, &fixtures))
            .await
            .expect("authorized batch");
    assert_eq!(
        snapshot_opens.load(Ordering::SeqCst),
        1,
        "one page of non-summary results must open exactly one frozen reconstruction snapshot"
    );
    assert_eq!(
        reconstructed
            .iter()
            .enumerate()
            .filter_map(|(rank, result)| result.as_ref().ok().map(|_| rank))
            .collect::<Vec<_>>(),
        (0..RESULTS)
            .filter(|rank| !matches!(rank, 11 | 23 | 31))
            .collect::<Vec<_>>(),
        "available messages must retain rank order without promoting a lower result"
    );
    for (rank, expected_provider, expected_session) in [
        (7, "claude", "claude-session"),
        (8, "codex", "codex-session"),
    ] {
        let message = reconstructed[rank].as_ref().expect("available message");
        assert_eq!(message.provider, expected_provider);
        assert_eq!(message.session_id, expected_session);
        assert_eq!(message.text, format!("canonical content {rank}"));
    }
    assert!(
        reconstructed[11]
            .as_ref()
            .is_err_and(|error| matches!(error, SessionTemporalExecutionError::Denied))
    );
    assert!(
        reconstructed[23]
            .as_ref()
            .is_err_and(|error| matches!(error, SessionTemporalExecutionError::Redacted))
    );
    assert!(
        reconstructed[31]
            .as_ref()
            .is_err_and(|error| matches!(error, SessionTemporalExecutionError::Unavailable))
    );

    let cancelled_snapshot = reconstruction_snapshot(true);
    assert!(matches!(
        reconstruct_non_summary_results(
            &batch,
            reconstruction_inputs(&cancelled_snapshot, &fixtures),
        )
        .await,
        Err(SessionTemporalExecutionError::Cancelled)
    ));
    assert_eq!(
        snapshot_opens.load(Ordering::SeqCst),
        1,
        "cancellation before batch admission must not open another reconstruction snapshot"
    );
}

#[test]
fn stored_retrieval_does_not_require_refresh_worker() {
    assert!(!requires_refresh_worker(
        SessionFreshnessPolicy::AllowStored
    ));
    assert!(requires_refresh_worker(
        SessionFreshnessPolicy::RequireFresh
    ));
}

fn typed<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("typed test identity")
}

#[test]
fn registered_profile_binding_replaces_the_legacy_request_profile_identity() {
    let brain_id = typed::<tracedecay_domain::BrainId>("brain.session-retrieval");
    let profile_id = typed::<tracedecay_domain::UserProfileId>("profile.durable-session-retrieval");
    let root = DaemonSessionRetrievalRoot::profile().expect("profile root");
    assert_eq!(
        root.identity.profile_id().as_str(),
        MESSAGE_SEARCH_PROFILE_ID
    );

    let root = root
        .with_profile_runtime_identity(brain_id.clone(), profile_id.clone())
        .expect("durable profile binding");

    assert_eq!(root.identity.profile_id().as_str(), profile_id.as_str());
    assert_eq!(
        root.expected_runtime_shard,
        Some(StoreShardIdV1::profile_sessions(brain_id, profile_id))
    );
}

#[test]
fn registered_project_binding_uses_one_durable_profile_and_typed_project() {
    let brain_id = typed::<tracedecay_domain::BrainId>("brain.session-retrieval");
    let profile_id = typed::<tracedecay_domain::UserProfileId>("profile.durable-session-retrieval");
    let project_id = ProjectId::new("project.session-retrieval").expect("project identity");
    let identity = ResolvedSessionIdentity::for_project(
        ProfileId::new(MESSAGE_SEARCH_PROFILE_ID).expect("legacy profile"),
        project_id.clone(),
        SessionStoreId::new("store.project.test").expect("store identity"),
        SessionRootId::new("root.project.test").expect("root identity"),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.project.test").expect("repository identity"),
            WorktreeId::new("/project/test").expect("worktree identity"),
            BranchId::new("branch.project.test").expect("branch identity"),
        ),
    );
    let root = DaemonSessionRetrievalRoot {
        store_scope: SessionRetrievalStoreScope::Project,
        identity,
        project_id: Some(project_id.as_str().to_owned()),
        authorized_root: None,
        expected_runtime_shard: None,
    }
    .with_project_runtime_identity(brain_id.clone(), profile_id.clone())
    .expect("durable project binding");

    assert_eq!(root.identity.profile_id().as_str(), profile_id.as_str());
    assert_eq!(root.identity.project_id(), Some(&project_id));
    assert_eq!(
        root.expected_runtime_shard,
        Some(StoreShardIdV1::project_sessions(
            brain_id, profile_id, project_id,
        ))
    );
}

#[test]
fn denied_shared_anchor_stays_at_its_rank_without_promoting_lower_candidate() {
    fn ranked(stable_id: &str, anchor: &RetrievalAnchorId) -> RankedCandidate {
        RankedCandidate {
            stable_id: stable_id.to_string(),
            anchor_id: anchor.clone(),
            normalized_score_micros: 1,
            knowledge_at_micros: 1,
            logical_message: None,
            turn: None,
            session: Some(format!("session.{stable_id}")),
            source: Some("cursor".to_string()),
            evidence_role: Some("assistant".to_string()),
            contributions: Vec::new(),
        }
    }

    let anchor = RetrievalAnchorId::new("anchor.shared").unwrap();
    let selected = [ranked("denied", &anchor), ranked("lower", &anchor)];
    let hydrated = [
        TemporalHydratedResult::unavailable_for_test(
            0,
            "denied",
            anchor.clone(),
            HydrationStateV1::Unauthorized,
        ),
        TemporalHydratedResult::available_for_test(
            1,
            "lower",
            anchor.clone(),
            b"lower candidate".to_vec(),
        ),
    ];

    let omission = page_hydration_slot(0, &selected[0], &hydrated).unwrap_err();
    assert_eq!(omission.rank, 0);
    assert_eq!(omission.anchor, anchor);
    assert_eq!(omission.reason, HydrationStateV1::Unauthorized);

    let lower = page_hydration_slot(1, &selected[1], &hydrated).unwrap();
    assert_eq!(lower.rank(), 1);
    assert_eq!(lower.stable_id(), "lower");
}

#[test]
fn complete_page_with_typed_omission_becomes_partial_and_keeps_coverage() {
    let anchor = RetrievalAnchorId::new("anchor.omitted").unwrap();
    let page = SessionRetrievalPageView {
        results: Vec::new(),
        temporal: SessionTemporalMetadataView {
            coverage: TemporalCoverageCountsV1 {
                visible: 0,
                hidden: 0,
                unknown: 1,
                redacted: 0,
            },
            omissions: vec![SessionRetrievalOmissionView {
                rank: 0,
                anchor: anchor.clone(),
                reason: HydrationStateV1::Unauthorized,
            }],
            ..SessionTemporalMetadataView::default()
        },
    };

    let SessionRetrievalServiceOutcome::Partial {
        page,
        freshness,
        omitted,
    } = complete_page_outcome(page, SessionDataFreshness::Fresh, 1)
    else {
        panic!("complete page with an omission must become partial");
    };
    assert_eq!(freshness, SessionDataFreshness::Fresh);
    assert_eq!(omitted, 1);
    assert_eq!(page.temporal.coverage.unknown, 1);
    assert_eq!(page.temporal.omissions[0].rank, 0);
    assert_eq!(page.temporal.omissions[0].anchor, anchor);
    assert_eq!(
        page.temporal.omissions[0].reason,
        HydrationStateV1::Unauthorized
    );
}

#[test]
fn stale_lcm_retrieval_remains_typed_instead_of_generic_unavailable() {
    let freshness = SessionDataFreshness::Stored { generation_lag: 7 };

    let describe = describe_retrieval_outcome(
        SessionRetrievalOutcome::Stale { freshness },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );
    let expand = expand_retrieval_outcome(
        SessionRetrievalOutcome::Stale { freshness },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );

    assert!(matches!(
        describe,
        LcmDescribeServiceOutcome::Stale {
            retrieval: LcmRetrievalOutcome::Stale {
                freshness: LcmDataFreshness::Stored { generation_lag: 7 }
            },
            ..
        }
    ));
    assert!(matches!(
        expand,
        LcmExpandServiceOutcome::Stale {
            retrieval: LcmRetrievalOutcome::Stale {
                freshness: LcmDataFreshness::Stored { generation_lag: 7 }
            },
            ..
        }
    ));
}

#[test]
fn reset_required_lcm_retrieval_preserves_the_owning_store_scope() {
    let describe = describe_retrieval_outcome(
        SessionRetrievalOutcome::ResetRequired,
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );
    let expand = expand_retrieval_outcome(
        SessionRetrievalOutcome::ResetRequired,
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Profile,
    );

    assert!(matches!(
        describe,
        LcmDescribeServiceOutcome::ResetRequired {
            store_scope: SessionRetrievalStoreScope::Project
        }
    ));
    assert!(matches!(
        expand,
        LcmExpandServiceOutcome::ResetRequired {
            store_scope: SessionRetrievalStoreScope::Profile
        }
    ));
}

#[test]
fn zero_item_partial_lcm_retrieval_remains_partial_instead_of_deleted() {
    let freshness = SessionDataFreshness::Partial { generation_lag: 3 };

    let describe = describe_retrieval_outcome(
        SessionRetrievalOutcome::Partial {
            items: Vec::new(),
            freshness,
            omitted: 5,
        },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );
    let expand = expand_retrieval_outcome(
        SessionRetrievalOutcome::Partial {
            items: Vec::new(),
            freshness,
            omitted: 5,
        },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );

    assert!(matches!(
        describe,
        LcmDescribeServiceOutcome::Partial {
            description: None,
            retrieval: LcmRetrievalOutcome::Partial {
                freshness: LcmDataFreshness::Partial { generation_lag: 3 },
                omitted: 5,
            },
            ..
        }
    ));
    assert!(matches!(
        expand,
        LcmExpandServiceOutcome::Partial {
            expansion: None,
            retrieval: LcmRetrievalOutcome::Partial {
                freshness: LcmDataFreshness::Partial { generation_lag: 3 },
                omitted: 5,
            },
            ..
        }
    ));
}
