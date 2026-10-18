//! Hint-outcome settlement journeys over registered project stores.
//!
//! The settlement pass under test lives in
//! `tracedecay_agent_hosts::hooks::hint_outcomes::settlement`; the fixture is
//! the root crate's registered host-admission test runtime, which is why the
//! journey lives here rather than beside the moved code.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use tempfile::TempDir;
use tracedecay_agent_hosts::agents::context_scout::intent_pass::intent_hint_emitted_event;
use tracedecay_agent_hosts::agents::context_scout::{
    ContextScoutSessionClaimInputV1, ContextScoutSessionEvidenceExt,
};
use tracedecay_agent_hosts::hooks::hint_outcomes::HintOutcomeStats;
use tracedecay_agent_hosts::hooks::hint_outcomes::settlement::{
    HintOutcomeSettlement, settle_project_hint_outcomes,
};
use tracedecay_application::observability::RegisteredObservabilityPortV1;
use tracedecay_contracts::context_scout::{
    ContextScoutAddressV1, ContextScoutCandidateV1, ContextScoutCategoryV1,
    ContextScoutDeliveryOutcomeV1, ContextScoutDeliveryReceiptV1, ContextScoutDeliveryWindowV1,
    ContextScoutDurableQueueEntryV1, ContextScoutEvidenceAvailabilityV1,
    ContextScoutIntentAnswerV1, ContextScoutIntentKindV1, ContextScoutIntentReceiptV1,
    ContextScoutModelOutcomeV1, ContextScoutRedactionReceiptV1, ContextScoutRouteV1,
    ContextScoutSessionEvidenceV1, ContextScoutSessionWindowV1, ContextScoutSuggestionEnvelopeV1,
    ContextScoutWorkV1, DecisionEngineIdentityV1, IntentDecisionSourceV1,
};
use tracedecay_contracts::{
    AuthorityReceipt, CapabilityGrantId, DisclosureClass, ObservabilityHorizonV1,
    ObservabilityQueryPort, ObservabilityQueryV1, PolicyDecisionRef, ResolvedScope,
};
use tracedecay_domain::{
    AdoptionOutcomeLinkedV1, CodeGenerationId, ComponentVersion, CoverageStateV1, ManifestDigest,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ProjectId, ProviderId, RefId, RepositoryId,
    SessionId, UtcMicros, WorktreeId,
};
use tracedecay_global_db::{AnalyticsEventInsert, AnalyticsEventQuery, RegisteredGlobalDb};
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

const HINT_TS: i64 = 1_000_000;
/// Past `hooks::hint_outcomes::HORIZON_SECS` (30 minutes) after `HINT_TS`,
/// so a non-matching window settles as ignored instead of staying open.
const AFTER_HORIZON: i64 = HINT_TS + 2_000;

struct ProjectSettlementFixture {
    _dir: TempDir,
    runtime: HostAdmissionTestRuntimeV1,
    project_root: std::path::PathBuf,
    /// Scope the observability envelopes are attributed to: the registered
    /// project session binding, not the analytics project key.
    binding_project_id: ProjectId,
}

impl ProjectSettlementFixture {
    async fn open(scope: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let project_root = dir.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let binding_project_id = ProjectId::new(scope).expect("project id");
        let runtime = HostAdmissionTestRuntimeV1::project(
            dir.path().join("profile"),
            &project_root,
            binding_project_id.clone(),
        )
        .await
        .expect("open registered project runtime");
        Self {
            _dir: dir,
            runtime,
            project_root,
            binding_project_id,
        }
    }

    fn analytics(&self) -> &RegisteredGlobalDb {
        self.runtime.profile_database_for_test()
    }

    fn sessions(&self) -> &RegisteredGlobalDb {
        self.runtime
            .registered_database(HostAdmissionScope::Project)
            .expect("registered project session database")
    }

    fn analytics_project_key(&self) -> String {
        RegisteredGlobalDb::canonical_project_key(&self.project_root)
    }

    async fn seed_hint(&self, session_id: &str, hint_id: &str) {
        self.runtime
            .append_profile_analytics_event_for_test(&AnalyticsEventInsert {
                provider: "hook_claude".to_owned(),
                project_id: self.analytics_project_key(),
                session_id: Some(session_id.to_owned()),
                timestamp: HINT_TS,
                event_kind: "hint_emitted".to_owned(),
                hook_name: None,
                tool_name: None,
                tool_category: None,
                skill_name: None,
                hint_category: Some("search".to_owned()),
                hint_id: Some(hint_id.to_owned()),
                outcome: Some("observed".to_owned()),
                metadata_json: None,
            })
            .await
            .expect("seed hint_emitted");
    }

    async fn seed_session_activity(&self, session_id: &str, tools: Option<&str>) {
        let inserted = self
            .runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &SessionRecord {
                    provider: "claude".to_owned(),
                    session_id: session_id.to_owned(),
                    project_key: self.analytics_project_key(),
                    project_path: self.project_root.display().to_string(),
                    title: None,
                    started_at: Some(HINT_TS),
                    ended_at: None,
                    transcript_path: None,
                    metadata_json: None,
                    parent_session_id: None,
                    is_subagent: false,
                    agent_id: None,
                    parent_tool_use_id: None,
                },
            )
            .await
            .expect("upsert project session");
        assert!(inserted, "session should upsert");
        let Some(tools) = tools else {
            return;
        };
        self.runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &SessionMessageRecord {
                    provider: "claude".to_owned(),
                    message_id: format!("{session_id}:1"),
                    session_id: session_id.to_owned(),
                    role: "assistant".to_owned(),
                    timestamp: Some(HINT_TS + 60),
                    ordinal: 1,
                    text: "activity".to_owned(),
                    kind: None,
                    model: None,
                    tool_names: Some(tools.to_owned()),
                    source_path: None,
                    source_offset: Some(1),
                    metadata_json: None,
                },
            )
            .await
            .expect("upsert project session message");
    }

    async fn settle(&self, now_secs: i64) -> HintOutcomeStats {
        let settlement = settle_project_hint_outcomes(
            Some(self.analytics()),
            Some(self.sessions()),
            Vec::new(),
            &self.project_root,
            now_secs,
        )
        .await;
        let HintOutcomeSettlement::Settled { stats, .. } = settlement else {
            panic!("expected a settled pass, got {settlement:?}");
        };
        stats
    }

    async fn adoption_outcome_events(&self) -> Vec<ObservabilityEnvelopeV1> {
        RegisteredObservabilityPortV1::new(self.sessions())
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: self.binding_project_id.as_str().to_owned(),
                event_kinds: vec!["adoption.outcome.linked.v1".to_owned()],
                horizon: ObservabilityHorizonV1 {
                    since_micros: 0,
                    until_micros: i64::MAX,
                },
                after_watermark: None,
                limit: 8,
            })
            .await
            .expect("read persisted adoption outcomes")
            .events
    }
}

fn outcome_payload(event: &ObservabilityEnvelopeV1) -> &AdoptionOutcomeLinkedV1 {
    let ObservabilityPayloadV1::AdoptionOutcome(outcome) = &event.payload else {
        panic!("unexpected payload for {}", event.event_kind);
    };
    outcome
}

#[tokio::test]
async fn settlement_records_settled_hints_as_a_linked_adoption_outcome() {
    let fixture = ProjectSettlementFixture::open("project.hint.adoption.mixed").await;
    // One hint acted on (matching tracedecay tool observed after the
    // hint), one ignored (only non-matching activity, horizon elapsed),
    // and one still open (no post-hint activity ingested yet).
    fixture.seed_hint("s-acted", "h-acted").await;
    fixture
        .seed_session_activity("s-acted", Some("tracedecay_context"))
        .await;
    fixture.seed_hint("s-ignored", "h-ignored").await;
    fixture
        .seed_session_activity("s-ignored", Some("Read"))
        .await;
    fixture.seed_hint("s-open", "h-open").await;
    fixture.seed_session_activity("s-open", None).await;

    let stats = fixture.settle(AFTER_HORIZON).await;
    assert_eq!(
        stats,
        HintOutcomeStats {
            scanned: 3,
            acted: 1,
            ignored: 1,
            unresolved: 1,
        }
    );

    let events = fixture.adoption_outcome_events().await;
    assert_eq!(events.len(), 1, "one funnel record per settlement pass");
    assert_eq!(
        outcome_payload(&events[0]),
        &AdoptionOutcomeLinkedV1 {
            invoked: 2,
            terminal: 2,
            independently_useful: 1,
            repeat_useful: 0,
            censored: 0,
            unknown: 0,
        },
        "only exactly-once settled hints may carry funnel mass"
    );
    assert_eq!(
        events[0].coverage,
        CoverageStateV1::Partial,
        "an unresolved remainder must weaken the census, never render Known"
    );

    // A later pass re-scans only the still-open hint, settles nothing,
    // and must not re-count it as new funnel mass.
    let stats = fixture.settle(AFTER_HORIZON + 240).await;
    assert_eq!(
        stats,
        HintOutcomeStats {
            scanned: 1,
            acted: 0,
            ignored: 0,
            unresolved: 1,
        }
    );
    assert_eq!(fixture.adoption_outcome_events().await.len(), 1);
}

#[tokio::test]
async fn settlement_with_only_open_hints_emits_no_adoption_outcome() {
    let fixture = ProjectSettlementFixture::open("project.hint.adoption.open").await;
    fixture.seed_hint("s-open", "h-open").await;
    fixture.seed_session_activity("s-open", None).await;

    let stats = fixture.settle(AFTER_HORIZON).await;
    assert_eq!(
        stats,
        HintOutcomeStats {
            scanned: 1,
            acted: 0,
            ignored: 0,
            unresolved: 1,
        }
    );
    assert!(
        fixture.adoption_outcome_events().await.is_empty(),
        "an all-open pass settled nothing and must not fabricate funnel mass"
    );
}

#[tokio::test]
async fn fully_settled_pass_records_a_known_coverage_census() {
    let fixture = ProjectSettlementFixture::open("project.hint.adoption.known").await;
    fixture.seed_hint("s-acted", "h-acted").await;
    fixture
        .seed_session_activity("s-acted", Some("tracedecay_context"))
        .await;

    let stats = fixture.settle(AFTER_HORIZON).await;
    assert_eq!(
        stats,
        HintOutcomeStats {
            scanned: 1,
            acted: 1,
            ignored: 0,
            unresolved: 0,
        }
    );
    let events = fixture.adoption_outcome_events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        outcome_payload(&events[0]),
        &AdoptionOutcomeLinkedV1 {
            invoked: 1,
            terminal: 1,
            independently_useful: 1,
            repeat_useful: 0,
            censored: 0,
            unknown: 0,
        }
    );
    assert_eq!(
        events[0].coverage,
        CoverageStateV1::Known,
        "a pass that settled every scanned hint is a complete census"
    );
}

/// The daemon's delivery row for one intent-pass suggestion, built by the
/// production builder from a delivered session-evidence entry.
fn delivered_intent_hint(
    project_key: &str,
    session_id: &str,
    intent: ContextScoutIntentKindV1,
    envelope_id: [u8; 16],
) -> AnalyticsEventInsert {
    let scope = ResolvedScope::new(
        ProjectId::new("project.hint.intent").unwrap(),
        RepositoryId::new("repository.hint.intent").unwrap(),
        WorktreeId::new("worktree.hint.intent").unwrap(),
        Some(RefId::new("refs/heads/main").unwrap()),
    )
    .unwrap();
    let digest = |fill: char| {
        ManifestDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
    };
    let authority = AuthorityReceipt {
        grant_id: CapabilityGrantId::new("grant.hint.intent".to_owned()).unwrap(),
        grant_revision: 1,
        grant_digest: digest('a'),
        authorized_scope_digest: scope.scope_digest.clone(),
        disclosure: DisclosureClass::Evidence,
        policy: PolicyDecisionRef::new(
            "policy.context-scout.intent-pass.v1",
            1,
            digest('b'),
            ComponentVersion::new("tracedecay.context-scout.intent-pass.v1").unwrap(),
        )
        .unwrap(),
        revalidated_at: UtcMicros(1),
    };
    let evidence = ContextScoutSessionEvidenceV1::claim(ContextScoutSessionClaimInputV1 {
        authorized_scope: scope,
        provider_id: ProviderId::new("claude").unwrap(),
        session_id: SessionId::new(session_id.to_owned()).unwrap(),
        protected_session_id: [3; 32],
        window: ContextScoutSessionWindowV1 {
            first_store_id: 1,
            last_store_id: 4,
            message_count: 4,
            window_digest: digest('c'),
            newest_timestamp: Some(HINT_TS * 1_000_000),
        },
        prompt_excerpt_digest: None,
        code_generation_id: Some(CodeGenerationId::new("generation.hint.intent").unwrap()),
        authority,
        redaction: ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        },
        availability: ContextScoutEvidenceAvailabilityV1::Complete,
        decision: ContextScoutIntentReceiptV1 {
            engine: Some(DecisionEngineIdentityV1 {
                base_url_digest: digest('d'),
                model: Some("kev-4b".to_owned()),
            }),
            outcome: ContextScoutModelOutcomeV1::Succeeded,
            answers: vec![ContextScoutIntentAnswerV1 {
                intent,
                probability_permille: 910,
                threshold_permille: 600,
            }],
            facts: Vec::new(),
            selected: intent,
            source: IntentDecisionSourceV1::Model,
        },
        claimed_at: UtcMicros(2),
    })
    .unwrap();
    let address = ContextScoutAddressV1 {
        profile_id: [1; 16],
        provider_id: [2; 16],
        protected_session_id: [3; 32],
        thread_id: [4; 16],
        turn_id: [5; 16],
        agent_id: [6; 16],
        logical_message_id: [7; 16],
        project_id: [8; 16],
    };
    let entry = ContextScoutDurableQueueEntryV1 {
        work: ContextScoutWorkV1 {
            address,
            generation: 1,
            input_watermark: [9; 32],
        },
        route: ContextScoutRouteV1::DecisionEngine,
        model_outcome: ContextScoutModelOutcomeV1::Succeeded,
        model_receipt: None,
        envelope: ContextScoutSuggestionEnvelopeV1 {
            envelope_id,
            address,
            input_watermark: [9; 32],
            configuration_revision: [10; 32],
            delivery_window: ContextScoutDeliveryWindowV1::Immediate,
            candidate: ContextScoutCandidateV1 {
                dedupe_key: [11; 32],
                category: ContextScoutCategoryV1::Retrieval,
                relevance_score: 59_637,
                suggestion_text: tracedecay_policy::intent_catalog::suggestion_text(intent),
                evidence: evidence.into(),
                expires_at: UtcMicros((HINT_TS + 600) * 1_000_000),
            },
        },
    };
    intent_hint_emitted_event(
        project_key,
        &entry,
        &ContextScoutDeliveryReceiptV1 {
            receipt_id: [12; 16],
            envelope_id,
            delivered_at: UtcMicros(HINT_TS * 1_000_000),
            outcome: ContextScoutDeliveryOutcomeV1::Displayed,
        },
    )
    .expect("session evidence emits a hint")
}

/// A daemon-written intent delivery row settles like a hook hint: the
/// intent key selects the catalog's expected tools, so a later message search
/// acts on a session-recall suggestion and an unrelated read ignores it.
#[tokio::test]
async fn daemon_intent_delivery_rows_settle_against_their_intent_key() {
    let fixture = ProjectSettlementFixture::open("project.hint.intent").await;
    let project_key = fixture.analytics_project_key();
    for (session, envelope_id) in [("s-intent-acted", [21; 16]), ("s-intent-ignored", [22; 16])] {
        let row = delivered_intent_hint(
            &project_key,
            session,
            ContextScoutIntentKindV1::SessionRecall,
            envelope_id,
        );
        assert_eq!(row.provider, "hook_claude");
        assert_eq!(row.hint_category.as_deref(), Some("session_recall"));
        assert_eq!(row.timestamp, HINT_TS);
        fixture
            .runtime
            .append_profile_analytics_event_for_test(&row)
            .await
            .expect("append daemon hint_emitted");
    }
    fixture
        .seed_session_activity(
            "s-intent-acted",
            Some("mcp__tracedecay__tracedecay_message_search"),
        )
        .await;
    fixture
        .seed_session_activity("s-intent-ignored", Some("Read"))
        .await;

    let stats = fixture.settle(AFTER_HORIZON).await;
    assert_eq!(
        stats,
        HintOutcomeStats {
            scanned: 2,
            acted: 1,
            ignored: 1,
            unresolved: 0,
        }
    );
    let outcomes = fixture
        .analytics()
        .query_analytics_events(&AnalyticsEventQuery {
            project_id: Some(project_key),
            event_kind: Some("hint_outcome".to_owned()),
            limit: 8,
            ..Default::default()
        })
        .await
        .expect("read hint outcomes");
    let mut settled = outcomes
        .iter()
        .map(|event| {
            (
                event.hint_id.clone().unwrap_or_default(),
                event.hint_category.clone().unwrap_or_default(),
                event.outcome.clone().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    settled.sort();
    assert_eq!(
        settled,
        vec![
            (
                hex::encode([21; 16]),
                "session_recall".to_owned(),
                "acted".to_owned()
            ),
            (
                hex::encode([22; 16]),
                "session_recall".to_owned(),
                "ignored".to_owned()
            ),
        ]
    );
}
