//! `GET /api/code-index/freshness` — per-mounted-worktree code-index generation
//! and freshness state.
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
use serde::{Deserialize, Serialize};

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessStateV1,
    DashboardFreshnessV1, DashboardLegalActionKindV1, DashboardLegalActionRefV1, scope_from_state,
};

/// The durable build phase whose committed boundary the dashboard is reading.
///
/// A phase is not inferred from scheduler state. The mounted registry publishes
/// the exact phase that owns the active generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeIndexBuildPhaseV1 {
    SourceScan,
    RelationalPreparation,
    BulkCommit,
    IndexBuild,
    Verification,
    Ready,
}

/// A typed reason an otherwise active generation cannot make durable progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeIndexBuildBlockedReasonV1 {
    ResidentMemory,
    SourceUnavailable,
    ArtifactStoreUnavailable,
    RetryBackoff,
}

/// The latest committed progress boundary for one active code-index generation.
///
/// Every count is scoped to `generation_id`. The snapshot never includes a
/// staged page: work is reported only after the batch that owns it commits.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CodeIndexBuildProgressV1 {
    /// Exact generation receiving the committed build work.
    pub generation_id: String,
    /// Durable daemon-authority epoch that produced this snapshot.
    ///
    /// This orders snapshots across daemon restarts without relying on wall
    /// clock time.
    pub daemon_incarnation: u64,
    /// Registry-minted scheduler incarnation within one daemon.
    ///
    /// A worktree retirement/remount creates a new value. `progress_epoch` is
    /// comparable only when both incarnation fields match.
    pub producer_incarnation: u64,
    /// Monotonic publication epoch for replacing delayed progress reads.
    pub progress_epoch: u64,
    /// Identity of the sealed source whose authenticated bounds define progress.
    pub sealed_source_digest: String,
    /// Durable pipeline phase that published this snapshot.
    pub phase: CodeIndexBuildPhaseV1,
    /// Source pages committed to the artifact database.
    pub committed_pages: u64,
    /// Search chunks committed to the artifact database.
    pub committed_chunks: u64,
    /// Import evidence rows committed to the artifact database.
    pub committed_imports: u64,
    /// Payload bytes committed to the artifact database.
    pub committed_payload_bytes: u64,
    /// Authenticated sealed-source file boundary completed by committed work.
    pub completed_files: u64,
    /// Authenticated sealed-source file bound for this generation.
    pub total_files: u64,
    /// Authenticated lexical-span boundary completed by committed work, in the
    /// sealed source's own advance unit.
    ///
    /// The unit is the source's, not this surface's: a sealed-layout source
    /// advances over the files-array byte span, while a published or
    /// partitioned source advances over file ordinals because its file records
    /// are not a contiguous byte range at all. Both are monotone and share the
    /// denominator below, so completion and the derived estimate are truthful;
    /// naming them bytes was not, and a consumer that multiplied this by an
    /// average file size was wrong by orders of magnitude (issue #1205).
    pub completed_lexical_units: u64,
    /// Authenticated lexical-span bound for this generation, in the same unit
    /// as [`Self::completed_lexical_units`].
    pub total_lexical_units: u64,
    /// Source pages in the batch currently being processed.
    pub current_batch_pages: u64,
    /// Sealed payload bytes in the batch currently being processed.
    pub current_batch_payload_bytes: u64,
    /// Monotonic elapsed time for this process's active generation build.
    pub elapsed_micros: u64,
    /// Duration of the last committed SQLite batch, when one exists.
    pub last_commit_latency_micros: Option<u64>,
    /// Rolling committed-file throughput, absent until it is established.
    pub files_per_second: Option<f64>,
    /// Rolling committed lexical-span throughput in units per second, absent
    /// until it is established. Not a byte rate; see
    /// [`Self::completed_lexical_units`].
    pub lexical_units_per_second: Option<f64>,
    /// Estimated remaining build duration, absent without a truthful rate.
    pub estimated_remaining_seconds: Option<u64>,
    /// Unix-epoch timestamp of the last durable progress publication.
    pub last_progress_micros: i64,
    /// Reason the active generation cannot currently advance, when known.
    pub blocked_reason: Option<CodeIndexBuildBlockedReasonV1>,
}

/// A deterministic contract violation that parked background convergence.
///
/// Parked is not dead: the worker keeps re-observing the violation on its
/// ordinary wake cadence, so an operator fix (for example restoring an
/// owner-private mode) is picked up on the next wake without a restart. The
/// state exists so `status`, doctor, and the dashboard report the violation
/// typed instead of an indefinite "warming".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeIndexConvergenceParkedV1 {
    /// Exact typed failure that parked convergence.
    pub reason: String,
    /// Operator action that clears the violation.
    pub remediation: String,
    /// When the violation was first observed (microseconds since the Unix epoch).
    pub parked_at_micros: i64,
    /// Background passes that re-observed the violation since parking.
    pub observed_passes: u64,
    /// Whether the worker re-checks this violation on every ordinary wake.
    /// True for filesystem/contract violations an operator fix clears in
    /// place; false for an abnormal task failure that only changed input (a
    /// new sealed generation) retries.
    pub retries_on_wake: bool,
}

/// Recovery state for a durable generation sealed under a different production
/// owner configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeIndexGenerationRecoveryV1 {
    /// Generation retired from incremental reuse.
    pub incompatible_generation_id: String,
    /// Stable owner-input reason codes reported by the code-index authority.
    pub incompatibilities: Vec<String>,
    /// Whether the prior generation may continue serving while its replacement
    /// builds under the current configuration.
    pub serving: CodeIndexGenerationRecoveryServingV1,
}

/// Serving disposition of an incompatible generation during recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeIndexGenerationRecoveryServingV1 {
    Preserved,
    Refused,
}

/// Interactive graph-serving state for the latest sealed generation.
///
/// A sealed generation can expose truthful census statistics before its graph
/// projection is ready to serve queries, so readiness is reported separately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CodeGraphServingReadinessV1 {
    /// No graph-serving authority exists for this worktree or generation.
    Unavailable { reason: String },
    /// The sealed generation exists, but graph activation has not completed.
    Pending,
    /// Graph activation completed without a serving projection.
    Refused { reason: String },
    /// The verified graph projection is installed for interactive reads.
    Ready,
}

/// Freshness/generation state for one mounted worktree.
///
/// `Deserialize` is part of the wire contract: the CLI status command decodes
/// exactly this type back out of the daemon's `tracedecay_status` response,
/// keeping one authority for the freshness shape.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct CodeIndexWorktreeFreshnessV1 {
    /// Display path of the mounted worktree root.
    pub worktree_root: String,
    /// Stable repository identity resolved by the scheduler.
    pub repository_id: Option<String>,
    /// Stable worktree identity resolved by the scheduler.
    pub worktree_id: Option<String>,
    /// Exact source reference captured by the sealed generation.
    pub source_reference: Option<String>,
    /// Exact source revision captured by the sealed generation.
    pub source_revision: Option<String>,
    /// Latest sealed generation identity, when a complete generation exists.
    pub latest_generation_id: Option<String>,
    /// Whether that generation's verified graph projection can serve reads.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_graph_serving: Option<CodeGraphServingReadinessV1>,
    /// Content identity of the complete source snapshot.
    pub snapshot_content_identity: Option<String>,
    /// Time the complete generation was durably sealed.
    pub sealed_at_micros: Option<i64>,
    /// Last reconcile observation time (microseconds since the Unix epoch).
    pub last_reconcile_micros: Option<i64>,
    /// Staleness-ladder state from the last scheduler execution. `fresh` means
    /// the scheduler most recently observed a fresh source; the status read
    /// does not probe the worktree to revalidate that observation.
    pub staleness_state: Option<String>,
    /// Whether the exact scheduler route owns a reconcile pass or has a
    /// pending wake. A stale seated generation with this false is stalled,
    /// not in a routine rebuild window.
    #[serde(default)]
    pub rebuild_in_flight: bool,
    /// Pending hook-hint count, when cheaply available.
    pub hook_hint_count: Option<u64>,
    /// Whether this read covers the complete mounted scheduler state.
    pub coverage: String,
    /// Latest committed progress for the active generation, if one is mounted.
    pub progress: Option<CodeIndexBuildProgressV1>,
    /// Deterministic contract violation currently parking background
    /// convergence, when one is observed. `staleness_state` reads `parked`
    /// while this is set and no generation serves.
    pub parked: Option<CodeIndexConvergenceParkedV1>,
    /// One-shot owner-configuration recovery currently replacing an
    /// incompatible durable generation.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_recovery: Option<CodeIndexGenerationRecoveryV1>,
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

const LIVE_NOTE: &str = "last daemon scheduler execution state; generation and scope come from the durable sealed generation";
const UNMOUNTED_NOTE: &str =
    "the daemon scheduler registry has no mounted scheduler for this project";
const UNAVAILABLE_NOTE: &str =
    "the dashboard is not attached to a daemon-owned code-index scheduler registry";

impl CodeIndexFreshnessPayloadV1 {
    /// Payload after the daemon scheduler registry answered for this project.
    ///
    /// The `note` is owned here — MCP dispatch and the HTTP freshness route
    /// must not spell it a second time.
    pub fn from_scheduler_observation(
        worktrees: impl IntoIterator<Item = CodeIndexWorktreeFreshnessV1>,
    ) -> Self {
        Self {
            worktrees: worktrees.into_iter().collect(),
            note: LIVE_NOTE.to_owned(),
        }
    }

    /// Reader attached, but no mounted scheduler for this project.
    pub fn from_unmounted_scheduler() -> Self {
        Self {
            worktrees: Vec::new(),
            note: UNMOUNTED_NOTE.to_owned(),
        }
    }

    /// No scheduler-registry reader is attached.
    pub fn from_unattached_registry() -> Self {
        Self {
            worktrees: Vec::new(),
            note: UNAVAILABLE_NOTE.to_owned(),
        }
    }

    /// One scheduler read: a live worktree, or the unmounted-empty payload.
    pub fn from_scheduler_read(worktree: Option<CodeIndexWorktreeFreshnessV1>) -> Self {
        match worktree {
            Some(worktree) => Self::from_scheduler_observation([worktree]),
            None => Self::from_unmounted_scheduler(),
        }
    }
}

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
    use super::*;
    use crate::read_model::DashboardDomainStateV1;

    async fn state_for_test() -> (tempfile::TempDir, DashboardState) {
        crate::events_api::dashboard_state_fixture("project.dashboard-code-index").await
    }

    #[test]
    fn scheduler_observation_payload_owns_the_live_note() {
        let payload = CodeIndexFreshnessPayloadV1::from_scheduler_observation([]);
        assert_eq!(payload.note, LIVE_NOTE);
        assert!(payload.worktrees.is_empty());
    }

    #[test]
    fn unmounted_and_unattached_payloads_own_their_notes() {
        let unmounted = CodeIndexFreshnessPayloadV1::from_unmounted_scheduler();
        assert_eq!(unmounted.note, UNMOUNTED_NOTE);
        assert!(unmounted.worktrees.is_empty());
        assert_eq!(
            CodeIndexFreshnessPayloadV1::from_scheduler_read(None).note,
            UNMOUNTED_NOTE
        );
        let unattached = CodeIndexFreshnessPayloadV1::from_unattached_registry();
        assert_eq!(unattached.note, UNAVAILABLE_NOTE);
        assert!(unattached.worktrees.is_empty());
    }

    #[test]
    fn graph_serving_readiness_is_additive_for_older_daemon_responses() {
        let mut value = serde_json::to_value(CodeIndexWorktreeFreshnessV1::default())
            .expect("freshness serializes");
        value
            .as_object_mut()
            .expect("freshness object")
            .remove("code_graph_serving");
        value
            .as_object_mut()
            .expect("freshness object")
            .remove("rebuild_in_flight");

        let decoded: CodeIndexWorktreeFreshnessV1 =
            serde_json::from_value(value).expect("older response remains readable");
        assert_eq!(decoded.code_graph_serving, None);
        assert!(!decoded.rebuild_in_flight);

        let ready = serde_json::to_value(CodeGraphServingReadinessV1::Ready)
            .expect("ready state serializes");
        assert_eq!(ready, serde_json::json!({ "state": "ready" }));
    }

    #[test]
    fn generation_recovery_serializes_typed_serving_disposition() {
        let recovery = CodeIndexGenerationRecoveryV1 {
            incompatible_generation_id: "generation.config-a".to_owned(),
            incompatibilities: vec!["policy_revision".to_owned()],
            serving: CodeIndexGenerationRecoveryServingV1::Refused,
        };

        assert_eq!(
            serde_json::to_value(recovery).expect("generation recovery serializes"),
            serde_json::json!({
                "incompatible_generation_id": "generation.config-a",
                "incompatibilities": ["policy_revision"],
                "serving": "refused"
            })
        );
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
