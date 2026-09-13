//! `GET /api/code-index/freshness` — per-mounted-worktree code-index generation
//! and freshness state.
//!
//! The authoritative source is the daemon-owned
//! `crate::daemon::code_index_scheduler` registry, which holds the map of live
//! per-worktree schedulers, their latest sealed generation identity, last
//! reconcile, staleness-ladder state, and hook-hint queues. Daemon-owned
//! dashboard construction threads an exact read port into [`DashboardState`].
//! Direct dashboard construction remains typed `unsupported` rather than
//! fabricating a generation identity or a "fresh" claim. The wire types are
//! owned by `tracedecay_contracts::code_index_freshness`.

use axum::Json;
use axum::extract::State;
use tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessStateV1,
    DashboardFreshnessV1, DashboardLegalActionKindV1, DashboardLegalActionRefV1, scope_from_state,
};

/// `GET /api/code-index/freshness`
pub async fn freshness(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<CodeIndexFreshnessPayloadV1>> {
    let envelope = hotpath::future!(
        async move { project_code_index_freshness(&state).await },
        label = "dashboard_api.freshness.projection"
    )
    .await;
    crate::observe::record_freshness_state(envelope.freshness.state);
    Json(envelope)
}

async fn project_code_index_freshness(
    state: &DashboardState,
) -> DashboardEnvelopeV1<CodeIndexFreshnessPayloadV1> {
    let authority_attached = state.code_index_freshness_reader.is_some();
    let read = match &state.code_index_freshness_reader {
        Some(reader) => reader(state.project_root.clone()).await,
        None => None,
    };
    let live = read.as_ref();
    let payload = match (authority_attached, read.clone()) {
        (true, Some(worktree)) => {
            CodeIndexFreshnessPayloadV1::from_scheduler_observation([worktree])
        }
        (true, None) => CodeIndexFreshnessPayloadV1::from_unmounted_scheduler(),
        (false, _) => CodeIndexFreshnessPayloadV1::from_unattached_registry(),
    };
    match live {
        Some(worktree)
            if worktree.latest_generation_id.is_some()
                && worktree.coverage == "complete"
                && worktree.staleness_state.as_deref() == Some("fresh") =>
        {
            DashboardEnvelopeV1::ready(
                scope_from_state(state),
                DashboardCoverageV1::complete(1, "mounted_worktree"),
                payload,
            )
        }
        // A parked deterministic contract violation with nothing serving is a
        // typed error surface, not an indefinite loading spinner: the reason
        // and remediation ride in the worktree payload.
        Some(worktree) if worktree.parked.is_some() && worktree.latest_generation_id.is_none() => {
            let reason = worktree
                .parked
                .as_ref()
                .map(|parked| parked.reason.clone())
                .unwrap_or_default();
            DashboardEnvelopeV1::new(
                scope_from_state(state),
                DashboardDomainStateV1::Error,
                DashboardCoverageV1::partial(
                    1,
                    0,
                    "mounted_worktree",
                    vec![format!("background convergence is parked: {reason}")],
                ),
                DashboardFreshnessV1::unknown(),
                payload,
            )
        }
        Some(worktree) if worktree.latest_generation_id.is_none() => DashboardEnvelopeV1::new(
            scope_from_state(state),
            if worktree.staleness_state.as_deref() == Some("indexing") {
                DashboardDomainStateV1::Loading
            } else {
                DashboardDomainStateV1::Unknown
            },
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            payload,
        ),
        Some(_) => DashboardEnvelopeV1::new(
            scope_from_state(state),
            DashboardDomainStateV1::Partial,
            DashboardCoverageV1::partial(
                1,
                0,
                "mounted_worktree",
                vec!["scheduler freshness coverage is incomplete".to_owned()],
            ),
            DashboardFreshnessV1::unknown(),
            payload,
        ),
        None if authority_attached => DashboardEnvelopeV1::new(
            scope_from_state(state),
            DashboardDomainStateV1::Unknown,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1 {
                state: DashboardFreshnessStateV1::Absent,
                observed_at_micros: None,
                watermark: None,
            },
            payload,
        ),
        None => DashboardEnvelopeV1::unsupported(scope_from_state(state), payload),
    }
    .with_legal_actions(vec![DashboardLegalActionRefV1::new(
        DashboardLegalActionKindV1::Refresh,
        "use-case.dashboard.code-index.freshness.refresh",
    )])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::read_model::DashboardDomainStateV1;
    use tracedecay_contracts::code_index_freshness::{
        CodeGraphServingReadinessV1, CodeIndexWorktreeFreshnessV1,
    };

    async fn state_for_test() -> (tempfile::TempDir, DashboardState) {
        crate::events_api::dashboard_state_fixture("project.dashboard-code-index").await
    }

    #[tokio::test]
    async fn freshness_route_is_typed_unsupported_without_daemon_authority() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;
        let Json(envelope) = freshness(State(state)).await;

        assert_eq!(envelope.schema_revision, 1);
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert!(envelope.payload.worktrees.is_empty());
        assert!(envelope.payload.note.contains("not attached"));
    }

    #[tokio::test]
    async fn mounted_scheduler_without_a_generation_is_loading_not_ready() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.code_index_freshness_reader = Some(Arc::new(|root| {
            Box::pin(async move {
                Some(CodeIndexWorktreeFreshnessV1 {
                    worktree_root: root.display().to_string(),
                    repository_id: None,
                    worktree_id: None,
                    source_reference: None,
                    source_revision: None,
                    latest_generation_id: None,
                    code_graph_serving: Some(CodeGraphServingReadinessV1::Unavailable {
                        reason: "generation_unavailable".to_owned(),
                    }),
                    snapshot_content_identity: None,
                    sealed_at_micros: None,
                    last_reconcile_micros: Some(42),
                    staleness_state: Some("indexing".to_owned()),
                    rebuild_in_flight: false,
                    hook_hint_count: Some(0),
                    coverage: "complete".to_owned(),
                    progress: None,
                    parked: None,
                    generation_recovery: None,
                })
            })
        }));

        let Json(envelope) = freshness(State(state)).await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Loading);
        assert!(!envelope.coverage.is_complete());
    }

    #[tokio::test]
    async fn attached_registry_without_a_mount_is_unknown_not_unsupported() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.code_index_freshness_reader = Some(Arc::new(|_| Box::pin(async { None })));

        let Json(envelope) = freshness(State(state)).await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unknown);
        assert_eq!(envelope.freshness.state, DashboardFreshnessStateV1::Absent);
    }
}
