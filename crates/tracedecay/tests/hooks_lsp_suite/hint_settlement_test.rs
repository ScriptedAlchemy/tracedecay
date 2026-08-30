//! Hint-outcome settlement journeys over registered project stores.
//!
//! The settlement pass under test lives in
//! `tracedecay_agent_hosts::hooks::hint_outcomes::settlement`; the fixture is
//! the root crate's registered host-admission test runtime, which is why the
//! journey lives here rather than beside the moved code.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_agent_hosts::hooks::hint_outcomes::HintOutcomeStats;
use tracedecay_agent_hosts::hooks::hint_outcomes::settlement::{
    HintOutcomeSettlement, settle_project_hint_outcomes,
};
use tracedecay_application::{
    ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
};
use tracedecay_domain::{
    AdoptionOutcomeLinkedV1, CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ProjectId,
};
use tracedecay_global_db::{AnalyticsEventInsert, RegisteredGlobalDb};
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
use tracedecay_usecases::observability::RegisteredObservabilityPortV1;

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
        let inserted = self
            .runtime
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
        assert!(inserted, "session message should upsert");
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
