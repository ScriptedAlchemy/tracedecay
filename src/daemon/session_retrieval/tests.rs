use super::*;

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
        project_paths: HashSet::new(),
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
    );
    let expand = expand_retrieval_outcome(
        SessionRetrievalOutcome::Stale { freshness },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
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
    );
    let expand = expand_retrieval_outcome(
        SessionRetrievalOutcome::Partial {
            items: Vec::new(),
            freshness,
            omitted: 5,
        },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
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
