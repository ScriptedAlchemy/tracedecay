use std::collections::BTreeSet;

use tracedecay_agent_hosts::agents::context_scout_v2::{
    ContextScoutAddressV1, ContextScoutCandidateV1, ContextScoutCategoryV1, ContextScoutDecisionV1,
    ContextScoutDeliveryWindowV1, ContextScoutEvidenceAvailabilityV1,
    ContextScoutEvidenceEnvelopeV1, ContextScoutEvidenceSourceKindV1,
    ContextScoutEvidenceSourceReceiptV1, ContextScoutLimitsV1, ContextScoutRedactionReceiptV1,
    ContextScoutSelectionInputV1, ContextScoutSuppressionV1, select_deterministic_context_scout,
};
use tracedecay_application::{
    AuthorityReceipt, CoverageCompleteness, CoverageDomainState, DisclosureClass, EvidenceCoverage,
    EvidenceDomain, FreshnessState, PolicyDecisionRef, ResolvedScope, RetrieverContributionState,
    TemporalState,
};
use tracedecay_domain::feedback::{FeedbackContentIdentityV1, FeedbackScopeV1};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ComponentVersion, ManifestDigest, ProjectId, RefId, RepositoryId,
    RetrievalAnchorId, TemporalModeV1, UtcMicros, WorktreeId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(character: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", character.to_string().repeat(64))).unwrap()
}

fn resolved_scope() -> ResolvedScope {
    ResolvedScope::new(
        id::<ProjectId>("project.scout"),
        id::<RepositoryId>("repository.scout"),
        id::<WorktreeId>("worktree.scout"),
        Some(id::<RefId>("refs/heads/main")),
    )
    .unwrap()
}

fn feedback_scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: id("project.scout"),
        repository_id: id("repository.scout"),
        worktree_id: id("worktree.scout"),
        branch_ref: "refs/heads/main".to_owned(),
        head_commit_id: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
    }
}

fn authority(scope: &ResolvedScope) -> AuthorityReceipt {
    AuthorityReceipt {
        grant_id: id("grant.scout"),
        grant_revision: 7,
        grant_digest: digest('a'),
        authorized_scope_digest: scope.scope_digest.clone(),
        disclosure: DisclosureClass::Evidence,
        policy: PolicyDecisionRef::new(
            "policy.scout",
            3,
            digest('b'),
            ComponentVersion::new("policy.scout.v3").unwrap(),
        )
        .unwrap(),
        revalidated_at: UtcMicros(100),
    }
}

fn coverage(completeness: CoverageCompleteness) -> EvidenceCoverage {
    EvidenceCoverage {
        requested_domains: vec![EvidenceDomain::Diagnostic],
        visited: (completeness == CoverageCompleteness::Complete).then_some(1),
        eligible: (completeness == CoverageCompleteness::Complete).then_some(1),
        returned: 1,
        completeness,
        domains: vec![CoverageDomainState {
            domain: EvidenceDomain::Diagnostic,
            completeness,
        }],
    }
}

fn evidence(
    contribution_state: RetrieverContributionState,
    completeness: CoverageCompleteness,
) -> ContextScoutEvidenceEnvelopeV1 {
    let scope = resolved_scope();
    ContextScoutEvidenceEnvelopeV1::claim(
        feedback_scope(),
        scope.clone(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('c'),
            file_digest: digest('d'),
        },
        id::<CodeGenerationId>("generation.scout.1"),
        authority(&scope),
        ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        },
        vec![source_receipt(
            ContextScoutEvidenceSourceKindV1::Git,
            "anchor.github.review.1",
            contribution_state,
            completeness,
        )],
        UtcMicros(110),
    )
    .unwrap()
}

fn source_receipt(
    source: ContextScoutEvidenceSourceKindV1,
    anchor: &str,
    contribution_state: RetrieverContributionState,
    completeness: CoverageCompleteness,
) -> ContextScoutEvidenceSourceReceiptV1 {
    ContextScoutEvidenceSourceReceiptV1 {
        source,
        contribution_state,
        temporal: TemporalState {
            requested_mode: TemporalModeV1::Current,
            requested_at: UtcMicros(100),
            resolved_at: UtcMicros(110),
            source_generation: Some(id("generation.scout.1")),
            watermark_digest: Some(digest('e')),
            freshness: match contribution_state {
                RetrieverContributionState::Stale => FreshnessState::Stale,
                RetrieverContributionState::Completed | RetrieverContributionState::Partial => {
                    FreshnessState::Current
                }
                _ => FreshnessState::Unknown,
            },
        },
        coverage: coverage(completeness),
        anchors: vec![id::<RetrievalAnchorId>(anchor)],
    }
}

fn address() -> ContextScoutAddressV1 {
    ContextScoutAddressV1 {
        profile_id: [1; 16],
        provider_id: [2; 16],
        protected_session_id: [3; 32],
        thread_id: [4; 16],
        turn_id: [5; 16],
        agent_id: [6; 16],
        logical_message_id: [7; 16],
        project_id: [8; 16],
    }
}

fn candidate(evidence: ContextScoutEvidenceEnvelopeV1) -> ContextScoutCandidateV1 {
    ContextScoutCandidateV1 {
        dedupe_key: [9; 32],
        category: ContextScoutCategoryV1::Verification,
        relevance_score: 900,
        suggestion_text: "Review the unresolved Git evidence before editing this symbol."
            .to_owned(),
        evidence,
        expires_at: UtcMicros(1_000),
    }
}

fn selection(
    evidence: ContextScoutEvidenceEnvelopeV1,
    delivery_window: ContextScoutDeliveryWindowV1,
) -> ContextScoutSelectionInputV1 {
    ContextScoutSelectionInputV1 {
        address: address(),
        input_watermark: [10; 32],
        configuration_revision: [11; 32],
        envelope_id: [12; 16],
        now: UtcMicros(200),
        delivery_window,
        delivered_dedupe_keys: BTreeSet::new(),
        candidates: vec![candidate(evidence)],
    }
}

#[test]
fn evidence_claim_binds_exact_scope_generation_authority_redaction_and_anchor() {
    let evidence = evidence(
        RetrieverContributionState::Completed,
        CoverageCompleteness::Complete,
    );

    assert_eq!(
        evidence.availability,
        ContextScoutEvidenceAvailabilityV1::Complete
    );
    assert_eq!(
        evidence.sources[0].anchors,
        vec![id::<RetrievalAnchorId>("anchor.github.review.1")]
    );
    assert_ne!(evidence.claim_digest, digest('0'));

    let mut wrong_scope = evidence.clone();
    wrong_scope.scope.head_commit_id = id::<CommitId>("1111111111111111111111111111111111111111");
    assert!(wrong_scope.validate().is_err());

    let mut wrong_anchor = evidence.clone();
    wrong_anchor.sources[0].anchors[0] = id("anchor.github.review.other");
    assert!(wrong_anchor.validate().is_err());

    let mut wrong_generation = evidence.clone();
    wrong_generation.sources[0].temporal.source_generation =
        Some(id("generation.scout.superseded"));
    assert!(wrong_generation.validate().is_err());

    let mut false_freshness = evidence.clone();
    false_freshness.sources[0].contribution_state = RetrieverContributionState::Stale;
    assert!(false_freshness.validate().is_err());

    let encoded = serde_json::to_string(&evidence).unwrap();
    assert!(!encoded.contains("raw source"));
    assert!(!encoded.contains("prompt"));
}

#[test]
fn query_lcm_semantic_code_and_git_sources_remain_canonical_references() {
    let scope = resolved_scope();
    let sources = [
        (ContextScoutEvidenceSourceKindV1::Git, "anchor.git.1"),
        (ContextScoutEvidenceSourceKindV1::Code, "anchor.code.1"),
        (
            ContextScoutEvidenceSourceKindV1::Semantic,
            "anchor.semantic.1",
        ),
        (ContextScoutEvidenceSourceKindV1::Lcm, "anchor.lcm.1"),
        (ContextScoutEvidenceSourceKindV1::Query, "anchor.query.1"),
    ]
    .into_iter()
    .map(|(source, anchor)| {
        source_receipt(
            source,
            anchor,
            RetrieverContributionState::Completed,
            CoverageCompleteness::Complete,
        )
    })
    .collect();
    let evidence = ContextScoutEvidenceEnvelopeV1::claim(
        feedback_scope(),
        scope.clone(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('c'),
            file_digest: digest('d'),
        },
        id("generation.scout.1"),
        authority(&scope),
        ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        },
        sources,
        UtcMicros(110),
    )
    .unwrap();

    assert_eq!(
        evidence
            .sources
            .iter()
            .map(|source| source.source)
            .collect::<Vec<_>>(),
        vec![
            ContextScoutEvidenceSourceKindV1::Query,
            ContextScoutEvidenceSourceKindV1::Lcm,
            ContextScoutEvidenceSourceKindV1::Semantic,
            ContextScoutEvidenceSourceKindV1::Code,
            ContextScoutEvidenceSourceKindV1::Git,
        ]
    );
    assert_eq!(evidence.anchor_count(), 5);
    assert_eq!(
        evidence.availability,
        ContextScoutEvidenceAvailabilityV1::Complete
    );
}

#[test]
fn unsolicited_partial_and_stale_evidence_are_typed_suppressions() {
    let partial = select_deterministic_context_scout(
        &selection(
            evidence(
                RetrieverContributionState::Partial,
                CoverageCompleteness::Partial,
            ),
            ContextScoutDeliveryWindowV1::Immediate,
        ),
        ContextScoutLimitsV1::bounded_defaults(),
    )
    .unwrap();
    assert_eq!(
        partial,
        ContextScoutDecisionV1::Suppressed {
            reason: ContextScoutSuppressionV1::EvidencePartial,
        }
    );

    let requested_partial = select_deterministic_context_scout(
        &selection(
            evidence(
                RetrieverContributionState::Partial,
                CoverageCompleteness::Partial,
            ),
            ContextScoutDeliveryWindowV1::OnRequest,
        ),
        ContextScoutLimitsV1::bounded_defaults(),
    )
    .unwrap();
    assert!(matches!(
        requested_partial,
        ContextScoutDecisionV1::Delayed { .. }
    ));

    let stale = select_deterministic_context_scout(
        &selection(
            evidence(
                RetrieverContributionState::Stale,
                CoverageCompleteness::Unknown,
            ),
            ContextScoutDeliveryWindowV1::OnRequest,
        ),
        ContextScoutLimitsV1::bounded_defaults(),
    )
    .unwrap();
    assert_eq!(
        stale,
        ContextScoutDecisionV1::Suppressed {
            reason: ContextScoutSuppressionV1::EvidenceStale,
        }
    );

    let scope = resolved_scope();
    let mixed = ContextScoutEvidenceEnvelopeV1::claim(
        feedback_scope(),
        scope.clone(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('c'),
            file_digest: digest('d'),
        },
        id("generation.scout.1"),
        authority(&scope),
        ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        },
        vec![
            source_receipt(
                ContextScoutEvidenceSourceKindV1::Git,
                "anchor.git.current",
                RetrieverContributionState::Completed,
                CoverageCompleteness::Complete,
            ),
            source_receipt(
                ContextScoutEvidenceSourceKindV1::Code,
                "anchor.code.stale",
                RetrieverContributionState::Stale,
                CoverageCompleteness::Unknown,
            ),
        ],
        UtcMicros(110),
    )
    .unwrap();
    assert_eq!(
        mixed.availability,
        ContextScoutEvidenceAvailabilityV1::Stale
    );
    assert_eq!(
        select_deterministic_context_scout(
            &selection(mixed, ContextScoutDeliveryWindowV1::OnRequest),
            ContextScoutLimitsV1::bounded_defaults(),
        )
        .unwrap(),
        ContextScoutDecisionV1::Suppressed {
            reason: ContextScoutSuppressionV1::EvidenceStale,
        }
    );
}
