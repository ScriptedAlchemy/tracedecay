//! `GET /api/code-index/freshness` — per-mounted-worktree code-index generation
//! and freshness state (plan 11 §"Typed presentation contracts").
//!
//! The authoritative source is the daemon-owned
//! `crate::daemon::code_index_scheduler` registry, which holds the map of live
//! per-worktree schedulers, their latest sealed generation identity, last
//! reconcile, staleness-ladder state, and hook-hint queues. Daemon-owned
//! dashboard construction threads an exact read port into [`DashboardState`].
//! Direct dashboard construction remains typed `unsupported` rather than
//! fabricating a generation identity or a "fresh" claim.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use schemars::JsonSchema;
use serde::Serialize;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessStateV1,
    DashboardFreshnessV1, DashboardLegalActionKindV1, DashboardLegalActionRefV1, scope_from_state,
};

/// Freshness/generation state for one mounted worktree.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct CodeIndexWorktreeFreshnessV1 {
    /// Display path of the mounted worktree root.
    pub worktree_root: String,
    /// Stable repository identity resolved by the scheduler.
    pub repository_id: Option<String>,
    /// Stable worktree identity resolved by the scheduler.
    pub worktree_id: Option<String>,
    /// Exact source reference captured by the sealed generation.
    pub source_reference: Option<String>,
    /// Latest sealed generation identity, when a complete generation exists.
    pub latest_generation_id: Option<String>,
    /// Content identity of the complete source snapshot.
    pub snapshot_content_identity: Option<String>,
    /// Time the complete generation was durably sealed.
    pub sealed_at_micros: Option<i64>,
    /// Last reconcile observation time (microseconds since the Unix epoch).
    pub last_reconcile_micros: Option<i64>,
    /// Staleness-ladder state label (e.g. `fresh`, `stale`, `reresolving`).
    pub staleness_state: Option<String>,
    /// Pending hook-hint count, when cheaply available.
    pub hook_hint_count: Option<u64>,
    /// Whether this read covers the complete mounted scheduler state.
    pub coverage: String,
}

pub type CodeIndexFreshnessReadFuture =
    Pin<Box<dyn Future<Output = Option<CodeIndexWorktreeFreshnessV1>> + Send + 'static>>;
pub type CodeIndexFreshnessReader =
    Arc<dyn Fn(PathBuf) -> CodeIndexFreshnessReadFuture + Send + Sync + 'static>;

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct CodeIndexFreshnessPayloadV1 {
    pub worktrees: Vec<CodeIndexWorktreeFreshnessV1>,
    pub note: String,
}

const LIVE_NOTE: &str =
    "live daemon scheduler state; generation and scope come from the durable sealed generation";
const UNAVAILABLE_NOTE: &str =
    "the dashboard is not attached to a daemon-owned code-index scheduler registry";

/// `GET /api/code-index/freshness`
pub async fn freshness(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<CodeIndexFreshnessPayloadV1>> {
    let authority_attached = state.code_index_freshness_reader.is_some();
    let read = match &state.code_index_freshness_reader {
        Some(reader) => reader(state.project_root.clone()).await,
        None => None,
    };
    let live = read.as_ref();
    let payload = CodeIndexFreshnessPayloadV1 {
        worktrees: read.clone().into_iter().collect(),
        note: if live.is_some() {
            LIVE_NOTE
        } else if authority_attached {
            "the daemon scheduler registry has no mounted scheduler for this project"
        } else {
            UNAVAILABLE_NOTE
        }
        .to_string(),
    };
    let envelope = match live {
        Some(worktree)
            if worktree.latest_generation_id.is_some()
                && worktree.coverage == "complete"
                && worktree.staleness_state.as_deref() == Some("fresh") =>
        {
            DashboardEnvelopeV1::ready(
                scope_from_state(&state),
                DashboardCoverageV1::complete(1, "mounted_worktree"),
                payload,
            )
        }
        Some(worktree) if worktree.latest_generation_id.is_none() => DashboardEnvelopeV1::new(
            scope_from_state(&state),
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
            scope_from_state(&state),
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
            scope_from_state(&state),
            DashboardDomainStateV1::Unknown,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1 {
                state: DashboardFreshnessStateV1::Absent,
                observed_at_micros: None,
                watermark: None,
            },
            payload,
        ),
        None => DashboardEnvelopeV1::unsupported(scope_from_state(&state), payload),
    }
    .with_legal_actions(vec![DashboardLegalActionRefV1::new(
        DashboardLegalActionKindV1::Refresh,
        "use-case.dashboard.code-index.freshness.refresh",
    )]);
    Json(envelope)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::read_model::DashboardDomainStateV1;

    async fn state_for_test() -> (tempfile::TempDir, DashboardState) {
        crate::events_api::dashboard_state_fixture("project.dashboard-code-index").await
    }

    #[tokio::test]
    async fn freshness_route_is_typed_unsupported_without_daemon_authority() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;
        let Json(envelope) = freshness(State(state)).await;

        assert_eq!(envelope.schema_revision, 1);
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert!(envelope.payload.worktrees.is_empty());
        assert!(envelope.payload.note.contains("not attached"));
    }

    #[tokio::test]
    async fn freshness_route_projects_exact_live_scheduler_identity() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.code_index_freshness_reader = Some(Arc::new(|root| {
            Box::pin(async move {
                Some(CodeIndexWorktreeFreshnessV1 {
                    worktree_root: root.display().to_string(),
                    repository_id: Some("repository.fixture".to_owned()),
                    worktree_id: Some("worktree.fixture".to_owned()),
                    source_reference: Some("refs/heads/main".to_owned()),
                    latest_generation_id: Some("generation.fixture".to_owned()),
                    snapshot_content_identity: Some("sha256:fixture".to_owned()),
                    sealed_at_micros: Some(41),
                    last_reconcile_micros: Some(42),
                    staleness_state: Some("fresh".to_owned()),
                    hook_hint_count: Some(0),
                    coverage: "complete".to_owned(),
                })
            })
        }));
        let Json(envelope) = freshness(State(state)).await;
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Ready);
        assert!(envelope.coverage.is_complete());
        let worktree = envelope.payload.worktrees.first().expect("worktree");
        assert_eq!(
            worktree.latest_generation_id.as_deref(),
            Some("generation.fixture")
        );
        assert_eq!(
            worktree.repository_id.as_deref(),
            Some("repository.fixture")
        );
        assert_eq!(worktree.staleness_state.as_deref(), Some("fresh"));
    }

    #[tokio::test]
    async fn mounted_scheduler_without_a_generation_is_loading_not_ready() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.code_index_freshness_reader = Some(Arc::new(|root| {
            Box::pin(async move {
                Some(CodeIndexWorktreeFreshnessV1 {
                    worktree_root: root.display().to_string(),
                    repository_id: None,
                    worktree_id: None,
                    source_reference: None,
                    latest_generation_id: None,
                    snapshot_content_identity: None,
                    sealed_at_micros: None,
                    last_reconcile_micros: Some(42),
                    staleness_state: Some("indexing".to_owned()),
                    hook_hint_count: Some(0),
                    coverage: "complete".to_owned(),
                })
            })
        }));

        let Json(envelope) = freshness(State(state)).await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Loading);
        assert!(!envelope.coverage.is_complete());
    }

    #[tokio::test]
    async fn attached_registry_without_a_mount_is_unknown_not_unsupported() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.code_index_freshness_reader = Some(Arc::new(|_| Box::pin(async { None })));

        let Json(envelope) = freshness(State(state)).await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unknown);
        assert_eq!(envelope.freshness.state, DashboardFreshnessStateV1::Absent);
    }
}
