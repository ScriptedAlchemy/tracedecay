use std::sync::Mutex;

use tracedecay_application::{
    HintEmission, HintOutcomeCorrelationPort, HintOutcomeObservation, HintOutcomePortError,
    HintOutcomePortFuture, HintOutcomePortOperation, HintToolActivity,
};

use super::{
    HORIZON_TOOL_STEPS, HintOutcomeStats, Resolution, ToolStep, correlate_hint_outcomes, resolve,
    tool_matches_expected,
};

const PROJECT: &str = "proj_hint_outcomes";
const HINT_TS: i64 = 1_000_000;

struct TestPort {
    resolved: Vec<String>,
    activity: Vec<HintToolActivity>,
    fail_at: Option<HintOutcomePortOperation>,
    appended: Mutex<Vec<HintOutcomeObservation>>,
}

impl TestPort {
    fn active(activity: Vec<HintToolActivity>) -> Self {
        Self {
            resolved: Vec::new(),
            activity,
            fail_at: None,
            appended: Mutex::new(Vec::new()),
        }
    }

    fn result<T>(
        &self,
        operation: HintOutcomePortOperation,
        value: T,
    ) -> Result<T, HintOutcomePortError> {
        if self.fail_at == Some(operation) {
            Err(HintOutcomePortError::new(operation, "injected failure"))
        } else {
            Ok(value)
        }
    }
}

impl HintOutcomeCorrelationPort for TestPort {
    fn resolved_hint_ids<'a>(
        &'a self,
        _project_id: &'a str,
        _limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<String>> {
        Box::pin(async move {
            self.result(
                HintOutcomePortOperation::QueryResolvedHints,
                self.resolved.clone(),
            )
        })
    }

    fn emitted_hints<'a>(
        &'a self,
        project_id: &'a str,
        _limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintEmission>> {
        Box::pin(async move {
            self.result(
                HintOutcomePortOperation::QueryEmittedHints,
                vec![HintEmission {
                    provider: "hook_claude".to_owned(),
                    project_id: project_id.to_owned(),
                    session_id: "session-1".to_owned(),
                    timestamp: HINT_TS,
                    category: "search".to_owned(),
                    hint_id: "hint-1".to_owned(),
                }],
            )
        })
    }

    fn session_tool_activity<'a>(
        &'a self,
        _provider: &'a str,
        _session_id: &'a str,
        _after_timestamp: i64,
        _limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintToolActivity>> {
        Box::pin(async move {
            self.result(
                HintOutcomePortOperation::QuerySessionActivity,
                self.activity.clone(),
            )
        })
    }

    fn append_outcomes<'a>(
        &'a self,
        outcomes: &'a [HintOutcomeObservation],
    ) -> HintOutcomePortFuture<'a, ()> {
        Box::pin(async move {
            self.result(HintOutcomePortOperation::AppendOutcomes, ())?;
            self.appended
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(outcomes);
            Ok(())
        })
    }
}

#[tokio::test]
async fn matching_tool_resolves_once_through_the_canonical_port() {
    let port = TestPort::active(vec![HintToolActivity {
        timestamp: HINT_TS + 1,
        tool_names: vec!["mcp__tracedecay__tracedecay_context".to_owned()],
    }]);

    let stats = correlate_hint_outcomes(&port, PROJECT, HINT_TS + 120)
        .await
        .expect("correlation succeeds");

    assert_eq!(
        stats,
        HintOutcomeStats {
            scanned: 1,
            acted: 1,
            ignored: 0,
            unresolved: 0,
        }
    );
    assert_eq!(
        port.appended
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1
    );
}

#[tokio::test]
async fn resolved_hint_is_idempotently_skipped() {
    let port = TestPort {
        resolved: vec!["hint-1".to_owned()],
        activity: Vec::new(),
        fail_at: None,
        appended: Mutex::new(Vec::new()),
    };

    assert_eq!(
        correlate_hint_outcomes(&port, PROJECT, HINT_TS + 120)
            .await
            .expect("correlation succeeds"),
        HintOutcomeStats::default()
    );
    assert!(
        port.appended
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

#[tokio::test]
async fn append_failure_remains_typed() {
    let port = TestPort {
        fail_at: Some(HintOutcomePortOperation::AppendOutcomes),
        ..TestPort::active(vec![HintToolActivity {
            timestamp: HINT_TS + 1,
            tool_names: vec!["tracedecay_context".to_owned()],
        }])
    };

    let error = correlate_hint_outcomes(&port, PROJECT, HINT_TS + 120)
        .await
        .expect_err("append failure must not become success");
    assert_eq!(error.operation(), "append_outcomes");
    assert_eq!(error.detail(), "injected failure");
}

#[test]
fn tool_matching_tolerates_prefixes_without_suffix_collisions() {
    let expected = ["tracedecay_context", "tracedecay_search"];
    assert!(tool_matches_expected(
        "mcp__tracedecay__tracedecay_context",
        &expected
    ));
    assert!(tool_matches_expected("TraceDecay-Context", &expected));
    assert!(!tool_matches_expected(
        "tracedecay_signature_search",
        &expected
    ));
}

#[test]
fn step_horizon_closes_without_fabricating_wall_clock_activity() {
    let steps = (0..HORIZON_TOOL_STEPS as i64)
        .map(|offset| ToolStep {
            ts: HINT_TS + offset + 1,
            tools: vec!["Read".to_owned()],
        })
        .collect::<Vec<_>>();

    assert!(matches!(
        resolve(HINT_TS, &steps, &["tracedecay_context"], HINT_TS + 5),
        Some(Resolution::Ignored)
    ));
}
