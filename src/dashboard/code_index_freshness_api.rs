//! `GET /api/code-index/freshness` — per-mounted-worktree code-index generation
//! and freshness state (plan 11 §"Typed presentation contracts").
//!
//! The authoritative source is the daemon-owned
//! `crate::daemon::code_index_scheduler` registry, which holds the map of live
//! per-worktree schedulers, their latest sealed generation identity, last
//! reconcile, staleness-ladder state, and hook-hint queues. That registry lives
//! in the daemon runtime and is **not** threaded into [`DashboardState`], so the
//! dashboard cannot read it today.
//!
//! Rather than fabricate a generation identity or a "fresh" claim, this route is
//! typed `unsupported` (plan §"Known backend gaps") and reports the exact seam
//! required to feed it: a read port over `CodeIndexSchedulerRegistry`
//! (latest-generation id, last-reconcile watermark, staleness-threshold state,
//! and hook-hint counts) added to `DashboardState`. The intended per-worktree
//! payload shape is modelled here so the frontend contract is generated against
//! the real shape even while the source is unwired.

use axum::Json;
use axum::extract::State;
use schemars::JsonSchema;
use serde::Serialize;

use super::DashboardState;
use super::read_model::{
    DashboardEnvelopeV1, DashboardLegalActionKindV1, DashboardLegalActionRefV1, scope_from_state,
};

/// Freshness/generation state for one mounted worktree. Unpopulated until the
/// scheduler-registry read port is wired into the dashboard state.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct CodeIndexWorktreeFreshnessV1 {
    /// Display path of the mounted worktree root.
    pub worktree_root: String,
    /// Latest sealed generation identity, when a complete generation exists.
    pub latest_generation_id: Option<String>,
    /// Last reconcile observation time (microseconds since the Unix epoch).
    pub last_reconcile_micros: Option<i64>,
    /// Staleness-ladder state label (e.g. `fresh`, `stale`, `reresolving`).
    pub staleness_state: Option<String>,
    /// Pending hook-hint count, when cheaply available.
    pub hook_hint_count: Option<u64>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct CodeIndexFreshnessPayloadV1 {
    pub worktrees: Vec<CodeIndexWorktreeFreshnessV1>,
    /// The seam that must be closed to feed this route with live data.
    pub required_source: String,
    pub note: String,
}

const REQUIRED_SOURCE: &str = "a read port over crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistry \
     (latest_generation_id, last-reconcile watermark, staleness-threshold state, \
     hook-hint counts) threaded into DashboardState";

const NOTE: &str = "the code-index scheduler registry is owned by the daemon runtime and is not \
     yet exposed to the dashboard state; per-worktree generation/freshness is \
     typed unsupported until that read port is wired";

/// `GET /api/code-index/freshness`
pub(crate) async fn freshness(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<CodeIndexFreshnessPayloadV1>> {
    let payload = CodeIndexFreshnessPayloadV1 {
        worktrees: Vec::new(),
        required_source: REQUIRED_SOURCE.to_string(),
        note: NOTE.to_string(),
    };
    let envelope = DashboardEnvelopeV1::unsupported(scope_from_state(&state), payload)
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
    use crate::dashboard::read_model::DashboardDomainStateV1;
    use crate::tracedecay::TraceDecay;

    async fn state_for_test() -> (tempfile::TempDir, DashboardState) {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");
        let state = crate::dashboard::build_state(&cg)
            .await
            .expect("dashboard state");
        (project, state)
    }

    #[tokio::test]
    async fn freshness_route_is_typed_unsupported_and_names_the_seam() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;
        let Json(envelope) = freshness(State(state)).await;

        assert_eq!(envelope.schema_revision, 1);
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert!(envelope.payload.worktrees.is_empty());
        assert!(
            envelope
                .payload
                .required_source
                .contains("CodeIndexSchedulerRegistry"),
            "the seam must name the registry read port"
        );
    }
}
