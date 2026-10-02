//! `tracedecay_status` and `tracedecay_active_project` over admitted authorities.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_application::advisory::github_runtime::github_source_status_v1;
use tracedecay_application::tracedecay::BranchDiagnostics;
use tracedecay_contracts::code_index_freshness::{
    CodeIndexReadinessWaitOutcomeV1, CodeIndexReadinessWaitReadV1, CodeIndexStalenessStateV1,
    CodeIndexWorktreeFreshnessV1,
};
use tracedecay_contracts::doctor::ResidentMemoryHolderReadV1;
use tracedecay_contracts::retrieval::{
    ActiveProjectBranchV1, ActiveProjectResolutionSourceV1, ActiveProjectResultV1,
    ActiveProjectStorageV1, ProjectStatusV1, StatusAdmissionV1, StatusBranchMismatchV1,
    StatusCodeIndexFreshnessV1, StatusGitStalenessUnavailableV1, StatusGitStalenessV1,
    StatusMemoryOwnerV1, StatusMemoryPressureV1, StatusMemoryV1, StatusResultV1,
    StatusRetrievalServingV1, StatusSchemaConvergenceStateV1, StatusSchemaConvergenceV1,
    StatusServingConditionV1, StatusServingFreshnessV1, StatusSessionGitEvidenceUnavailableV1,
    StatusSessionGitEvidenceV1, StatusSurfaceRequestV1,
};
use tracedecay_contracts::storage::{SchemaConvergenceFindingV1, SchemaConvergenceStateV1};
use tracedecay_domain::ProjectId;
use tracedecay_domain::errors::Result;
use tracedecay_global_db::{GlobalDbGitCorrelationStore, RegisteredGlobalDb, SessionIngestHealth};
use tracedecay_runtime_core::resident_memory::{
    RESIDENT_OWNER_SHED_ORDER_V1, ResidentMemoryPressureStateV1, ResidentMemoryPressureV1,
    ResidentOwnerKindV1, ResidentOwnersV1, process_resident_memory_pressure_v1,
    process_resident_owners_v1, sampled_memory_pressure_some_avg10_v1,
};
use tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot;
use tracedecay_runtime_core::storage::{StorageMode, StoreKind};
use tracedecay_session_runtime::retained::lcm_doctor_projection;
use tracedecay_sessions::runtime::git_correlation::CorrelationIndexHealth;
use tracedecay_sessions::serving::SessionProjectionServingStatus;

use crate::McpToolContext;
use crate::handlers::workflow::current_head_commit_id;
use crate::tools::render::Md;

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

/// Project what a readiness wait observed onto the caller-facing outcome.
/// `last_state` is the `code_index_freshness.status` label of the last
/// reading, or `not_mounted` when no scheduler was mounted for the root.
#[must_use]
pub fn readiness_wait_outcome(
    read: CodeIndexReadinessWaitReadV1,
) -> CodeIndexReadinessWaitOutcomeV1 {
    match read {
        CodeIndexReadinessWaitReadV1::Reached { .. } => CodeIndexReadinessWaitOutcomeV1::Reached,
        CodeIndexReadinessWaitReadV1::TimedOut { last } => {
            CodeIndexReadinessWaitOutcomeV1::TimedOut {
                last_state: last
                    .as_ref()
                    .map_or("not_mounted", |freshness| {
                        code_index_freshness_projection(freshness).0.as_str()
                    })
                    .to_owned(),
            }
        }
        CodeIndexReadinessWaitReadV1::Unreachable { reason } => {
            CodeIndexReadinessWaitOutcomeV1::Unavailable { reason }
        }
    }
}

fn schema_convergence_status(findings: &[SchemaConvergenceFindingV1]) -> StatusSchemaConvergenceV1 {
    let status = if findings
        .iter()
        .any(|finding| finding.state == SchemaConvergenceStateV1::Degraded)
    {
        StatusSchemaConvergenceStateV1::Degraded
    } else if findings.iter().any(|finding| {
        matches!(
            finding.state,
            SchemaConvergenceStateV1::PendingSchemaMigration
                | SchemaConvergenceStateV1::ReleasedShapeConvergenceInProgress
        )
    }) {
        StatusSchemaConvergenceStateV1::InProgress
    } else {
        StatusSchemaConvergenceStateV1::Completed
    };
    StatusSchemaConvergenceV1 {
        status,
        findings: findings.to_vec(),
    }
}

/// Whether exact-scope code retrieval can serve at all, derived from the same
/// sealed-generation census the retrieval lanes enforce.
///
/// `serving_branch` is store provenance, but readers take it as a serving
/// claim, on a fresh daemon it named a branch seconds into enrollment while
/// every retrieval lane truthfully refused `generation_rebuilding`. Status
/// must report the same serving truth the lanes enforce: the branch claim is
/// gated on a sealed complete generation existing, and the typed
/// `retrieval_serving` field carries the lane-level answer either way. No
/// census authority attached (a non-daemon server) leaves it absent: status
/// can then neither claim nor deny lane-serving truth.
fn branch_servable(retrieval_serving: Option<&StatusRetrievalServingV1>) -> bool {
    !matches!(
        retrieval_serving,
        Some(StatusRetrievalServingV1::Unavailable { .. })
    )
}

/// Whole seconds elapsed since a recorded microsecond timestamp, clamped at
/// zero. `None` when the source never recorded the observation.
fn age_seconds(recorded_at_micros: Option<i64>) -> Option<i64> {
    let recorded = recorded_at_micros?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(recorded, |elapsed| elapsed.as_micros() as i64);
    Some(now.saturating_sub(recorded).max(0) / 1_000_000)
}

#[derive(Clone, Copy)]
struct ReadyServingSourceV1<'a> {
    reference: &'a str,
    revision: Option<&'a str>,
    current_source_verified: bool,
}

fn ready_serving_source(
    payload: Option<&tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1>,
) -> Option<ReadyServingSourceV1<'_>> {
    let freshness = payload?.worktrees.first()?;
    if freshness.latest_generation_id.is_none()
        || !matches!(
            freshness.code_graph_serving,
            Some(tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Ready)
        )
    {
        return None;
    }
    Some(ReadyServingSourceV1 {
        reference: freshness.source_reference.as_deref()?,
        revision: freshness.source_revision.as_deref(),
        current_source_verified: freshness.coverage.covers_indexable_sources()
            && freshness.staleness_state == Some(CodeIndexStalenessStateV1::Fresh),
    })
}

fn attach_compact_branch_summary(
    open_active_branch: Option<String>,
    serving_branch: Option<String>,
    status: &mut ProjectStatusV1,
) {
    // Both status shapes consume the serving identity reconciled with the
    // ready generation source below.
    // Do not alias open/active into current/live: those are distinct under drift.
    status.active_branch = open_active_branch;
    if branch_servable(status.retrieval_serving.as_ref()) {
        status.serving_branch = serving_branch;
    }
}

fn attach_full_branch_status(
    branch_diagnostics: &BranchDiagnostics,
    status: &mut ProjectStatusV1,
) -> Result<()> {
    status.branch_diagnostics = Some(serde_json::to_value(branch_diagnostics)?);
    status
        .active_branch
        .clone_from(&branch_diagnostics.open_active_branch);
    status
        .current_branch
        .clone_from(&branch_diagnostics.current_branch);
    status
        .live_branch
        .clone_from(&branch_diagnostics.current_branch);
    if branch_servable(status.retrieval_serving.as_ref()) {
        status
            .serving_branch
            .clone_from(&branch_diagnostics.serving_branch);
    }
    status.parent_branch = branch_diagnostics
        .branches
        .iter()
        .find(|entry| entry.is_serving)
        .and_then(|entry| entry.parent.clone());
    status.branch_drifted = Some(branch_diagnostics.branch_drifted);
    status.branch_resolution = Some(branch_diagnostics.branch_resolution.clone());
    status.tracked_branch_count = Some(branch_diagnostics.tracked_branch_count);
    if branch_diagnostics.branch_drifted {
        status.branch_mismatch = Some(StatusBranchMismatchV1 {
            git_branch: branch_diagnostics.current_branch.clone(),
            indexed_branch: branch_diagnostics.open_active_branch.clone(),
            serving_branch: branch_diagnostics.serving_branch.clone(),
        });
    }
    if !branch_diagnostics.warnings.is_empty() {
        status.branch_warnings = Some(branch_diagnostics.warnings.clone());
    }
    Ok(())
}

/// The daemon's resident memory as one project sees it: process RSS against
/// the admission ceiling and pressure line, the daemon-wide retained totals,
/// and this project's retained owners with their bytes, idle time, and
/// whether pressure may shed them. Other projects' owners belong to the
/// daemon-wide Doctor inventory, never to a project read.
fn project_memory_value(project_id: &ProjectId) -> StatusMemoryV1 {
    memory_value(
        process_resident_memory_pressure_v1(),
        process_resident_owners_v1(),
        sampled_memory_pressure_some_avg10_v1(),
        std::time::Instant::now(),
        project_id,
    )
}

fn memory_value(
    pressure: &ResidentMemoryPressureV1,
    owners: &ResidentOwnersV1,
    psi_some_avg10: Option<f64>,
    now: std::time::Instant,
    project_id: &ProjectId,
) -> StatusMemoryV1 {
    let (status, resident_bytes) = match pressure.state() {
        ResidentMemoryPressureStateV1::Unobserved => (StatusMemoryPressureV1::Unobserved, None),
        ResidentMemoryPressureStateV1::Nominal { observed_bytes, .. } => {
            (StatusMemoryPressureV1::Nominal, Some(observed_bytes))
        }
        ResidentMemoryPressureStateV1::OverBudget { observed_bytes, .. } => {
            (StatusMemoryPressureV1::OverBudget, Some(observed_bytes))
        }
    };
    let report = owners.report(now);
    let owners = report
        .owners
        .iter()
        .filter(|row| row.project_id == *project_id)
        .map(|row| StatusMemoryOwnerV1 {
            project_id: row.project_id.as_str().to_owned(),
            kind: row.kind.as_str().to_owned(),
            holders: row
                .holders
                .iter()
                .map(|holder| ResidentMemoryHolderReadV1 {
                    worktree_id: holder.worktree_id.as_str().to_owned(),
                    holding: holder.holding.as_str().to_owned(),
                })
                .collect(),
            content_digest: row
                .content_digest
                .as_ref()
                .map(|digest| digest.as_str().to_owned()),
            bytes: row.bytes.measured(),
            measured: row.bytes.measured().is_some(),
            idle_seconds: row.idle_for.as_secs(),
            protected: row.protected,
        })
        .collect();
    StatusMemoryV1 {
        status,
        resident_bytes,
        limit_bytes: pressure.limit_bytes(),
        high_watermark_bytes: pressure.high_watermark_bytes(),
        low_watermark_bytes: pressure.low_watermark_bytes(),
        psi_some_avg10,
        idle_window_seconds: report.idle_window.as_secs(),
        shed_order: RESIDENT_OWNER_SHED_ORDER_V1
            .map(|kind: ResidentOwnerKindV1| kind.as_str().to_owned())
            .to_vec(),
        retained_bytes: report.measured_bytes,
        unmeasured_owners: report.unmeasured_owners,
        owners,
    }
}

/// Serialize the generation census exactly as the CLI decoder reads it back.
///
/// [`GenerationCensusSnapshot`] is the single wire authority for the
/// `graph_statistics` field: this route serializes it and `tracedecay status`
/// deserializes the same Rust type, so the two sides cannot drift.
pub fn graph_statistics_value(census: Option<&GenerationCensusSnapshot>) -> Result<Value> {
    let census = census.cloned().unwrap_or(
        GenerationCensusSnapshot::Unavailable {
            reason:
                tracedecay_runtime_core::runtime_telemetry::GenerationCensusUnavailableReason::AuthorityUnavailable,
        },
    );
    Ok(serde_json::to_value(&census)?)
}

/// The code-index freshness section and the lane-level serving truth it
/// implies. The lanes serve exactly when a sealed complete generation exists
/// for the worktree; until the first seal every retrieval lane refuses
/// `generation_rebuilding`.
fn code_index_freshness_status(
    freshness_payload: Option<
        &tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1,
    >,
) -> (
    StatusCodeIndexFreshnessV1,
    Option<String>,
    Option<StatusRetrievalServingV1>,
) {
    let Some(payload) = freshness_payload else {
        return (
            StatusCodeIndexFreshnessV1::Unavailable {
                reason: "code_index_scheduler_authority_not_attached".to_owned(),
            },
            None,
            None,
        );
    };
    let Some(freshness) = payload.worktrees.first() else {
        if let Some(failure) = &payload.mount_failure {
            return (
                StatusCodeIndexFreshnessV1::MountFailed {
                    message: failure.message.clone(),
                    remediation: failure.remediation.clone(),
                },
                Some(format!("{}; {}", failure.message, failure.remediation)),
                Some(StatusRetrievalServingV1::Unavailable {
                    reason: "code_index_mount_failed".to_owned(),
                }),
            );
        }
        return (
            StatusCodeIndexFreshnessV1::Unavailable {
                reason: "code_index_scheduler_not_mounted".to_owned(),
            },
            None,
            Some(StatusRetrievalServingV1::Unavailable {
                reason: "code_index_scheduler_not_mounted".to_owned(),
            }),
        );
    };
    let (label, warning) = code_index_freshness_projection(freshness);
    let retrieval_serving = if freshness.latest_generation_id.is_some() {
        let (serving_freshness, condition) = match freshness.staleness_state {
            Some(CodeIndexStalenessStateV1::Fresh) => (StatusServingFreshnessV1::Current, None),
            Some(CodeIndexStalenessStateV1::Restoring) => (
                StatusServingFreshnessV1::Restoring,
                Some(StatusServingConditionV1::ArtifactRestore),
            ),
            Some(CodeIndexStalenessStateV1::Verifying) => (
                StatusServingFreshnessV1::LastCompleteStale,
                Some(StatusServingConditionV1::SourceVerification),
            ),
            Some(_) if freshness.rebuild_in_flight => (
                StatusServingFreshnessV1::LastCompleteStale,
                Some(StatusServingConditionV1::Rebuilding),
            ),
            Some(_) => (
                StatusServingFreshnessV1::LastCompleteStale,
                Some(StatusServingConditionV1::Stalled),
            ),
            None => (StatusServingFreshnessV1::Unknown, None),
        };
        StatusRetrievalServingV1::Serving {
            freshness: serving_freshness,
            condition,
            seated_generation_age_seconds: age_seconds(freshness.sealed_at_micros),
            last_reconcile_age_seconds: age_seconds(freshness.last_reconcile_micros),
        }
    } else {
        let reason = if freshness.staleness_state == Some(CodeIndexStalenessStateV1::Restoring) {
            "generation_restoring"
        } else {
            "generation_rebuilding"
        };
        StatusRetrievalServingV1::Unavailable {
            reason: reason.to_owned(),
        }
    };
    (
        label.with_worktree(freshness.clone()),
        warning,
        Some(retrieval_serving),
    )
}

/// The latest sealed generation's watermark, the commit its source snapshot
/// was captured from, against the worktree's checked-out commit.
fn git_staleness(
    freshness_payload: Option<
        &tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1,
    >,
    project_root: &Path,
) -> StatusGitStalenessV1 {
    let Some(sealed) = freshness_payload
        .and_then(|payload| payload.worktrees.first())
        .filter(|freshness| freshness.latest_generation_id.is_some())
    else {
        return StatusGitStalenessV1::Unavailable {
            reason: StatusGitStalenessUnavailableV1::NoSealedGeneration,
        };
    };
    let Some(watermark) = sealed.source_revision.clone() else {
        return StatusGitStalenessV1::Unavailable {
            reason: StatusGitStalenessUnavailableV1::SealedGenerationHasNoCommit,
        };
    };
    let Some(head) = current_head_commit_id(project_root) else {
        return StatusGitStalenessV1::Unavailable {
            reason: StatusGitStalenessUnavailableV1::GitHeadUnreadable,
        };
    };
    if head.as_str() == watermark {
        StatusGitStalenessV1::Current { watermark }
    } else {
        StatusGitStalenessV1::Stale {
            watermark,
            head: head.as_str().to_owned(),
        }
    }
}

/// Computes `tracedecay_status`. `server_stats` is the serving MCP server's
/// request counters; `session_projection` is the project refresh worker's
/// serving status; `wait` is the readiness wait the owner held the read
/// for, when the request asked for one, and `reached_freshness` the reading
/// that satisfied it, which the payload reports instead of a later reading.
#[hotpath::measure(label = "mcp.info.status.total")]
pub async fn compute_status(
    ctx: &McpToolContext<'_>,
    request: &StatusSurfaceRequestV1,
    server_stats: Option<Value>,
    session_projection: SessionProjectionServingStatus,
    scope_prefix: Option<&str>,
    wait: Option<CodeIndexReadinessWaitOutcomeV1>,
    reached_freshness: Option<CodeIndexWorktreeFreshnessV1>,
) -> Result<StatusResultV1> {
    if request.admission_only {
        return Ok(StatusResultV1::Admission(StatusAdmissionV1 {
            project_admitted: true,
            project_root: display_path(ctx.project_root()),
            server: server_stats,
            scope_prefix: scope_prefix.map(str::to_owned),
        }));
    }

    // Compact by default. The CLI already skips these sections because they
    // commonly push status over the response-frame budget, and the truncated
    // body is not something the caller should reassemble into context. Opt in
    // when the full diagnostic section is the thing being asked for.
    let freshness_payload = match reached_freshness {
        Some(reading) => Some(
            tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1::from_scheduler_read(
                Some(reading),
            ),
        ),
        None => {
            hotpath::future!(
                ctx.freshness(),
                label = "mcp.info.status.code_index_freshness"
            )
            .await
        }
    };
    let (code_index_freshness, code_index_freshness_warning, retrieval_serving) =
        code_index_freshness_status(freshness_payload.as_ref());
    let github_source = match github_source_status_v1(ctx.project_root()) {
        Some(source) => serde_json::to_value(&source)?,
        None => json!({
            "state": "absent",
            "reason": "the checkout has no GitHub origin, or its advisory owner has not mounted in this daemon",
        }),
    };
    let storage_health = if request.include_storage_health {
        let mut storage_health = serde_json::to_value(
            hotpath::future!(
                crate::handlers::health::collect_database_snapshot(ctx, false, None),
                label = "mcp.info.status.storage_health"
            )
            .await?,
        )?;
        if server_stats.is_some() {
            storage_health["daemon_owner_pid"] = json!(std::process::id());
            storage_health["daemon_generation"] =
                json!(tracedecay_runtime_core::runtime_identity::process_run_id());
        }
        Some(storage_health)
    } else {
        None
    };
    let mut status = ProjectStatusV1 {
        project_root: display_path(ctx.project_root()),
        graph_statistics: graph_statistics_value(ctx.generation_census())?,
        memory: project_memory_value(&ctx.admitted_scope().project_id),
        schema_convergence: schema_convergence_status(
            &ctx.store_runtime()
                .registered_schema_convergence_observations(),
        ),
        reset_required_stores: ctx.store_runtime().reset_required_stores(),
        code_index_freshness,
        code_index_freshness_warning,
        retrieval_serving,
        github_source,
        storage_health,
        server: server_stats,
        branch_diagnostics: None,
        active_branch: None,
        current_branch: None,
        live_branch: None,
        serving_branch: None,
        parent_branch: None,
        branch_drifted: None,
        branch_resolution: None,
        tracked_branch_count: None,
        branch_mismatch: None,
        branch_warnings: None,
        session_ingest: None,
        session_history_catch_up: None,
        session_projection: lcm_doctor_projection(session_projection),
        session_git_evidence: hotpath::future!(
            session_git_evidence(ctx),
            label = "mcp.info.status.session_git_evidence"
        )
        .await,
        git_staleness: request
            .include_staleness
            .then(|| git_staleness(freshness_payload.as_ref(), ctx.project_root())),
        scope_prefix: scope_prefix.map(str::to_owned),
        wait,
    };

    let ready_serving_source = ready_serving_source(freshness_payload.as_ref());
    let (source_reference, source_revision, source_is_current) = (
        ready_serving_source.map(|source| source.reference),
        ready_serving_source.and_then(|source| source.revision),
        ready_serving_source.is_some_and(|source| source.current_source_verified),
    );
    if request.include_branch_diagnostics {
        let branch_diagnostics = ctx.branch_diagnostics_for_serving_source(
            source_reference,
            source_revision,
            source_is_current,
        );
        attach_full_branch_status(&branch_diagnostics, &mut status)?;
    } else {
        let (open_active_branch, serving_branch) = ctx.serving_branch_identity_for_serving_source(
            source_reference,
            source_revision,
            source_is_current,
        );
        attach_compact_branch_summary(open_active_branch, serving_branch, &mut status);
    }

    // Session-transcript ingest health (recall trust): last ingest time and
    // any un-ingested transcript backlog from the admitted project session
    // authority. Match tracedecay_runtime: consult the lease directly rather
    // than gating on the layout path existing on disk (fixtures and some
    // retained mounts hold an open authority before the path is observed).
    if request.include_session_ingest {
        match ctx.authorized_project_session_db() {
            None => {
                // Attached means admitted; absent is the typed
                // unavailable/denied state. Fail closed instead of
                // opening a second connection here.
                status.session_ingest = Some(json!({
                    "status": "unavailable",
                    "reason": "session_store_denied",
                    "message": "this request is not authorized to read the admitted project session store",
                }));
            }
            Some((lease, _)) => {
                let db = lease.as_ref();
                match hotpath::future!(
                    db.cursor_session_ingest_health(),
                    label = "mcp.info.status.session_ingest"
                )
                .await
                {
                    Ok(ingest) => {
                        status.session_ingest = Some(serde_json::to_value(&ingest)?);
                        // `session_ingest` stays cursor-scoped so it keeps matching the
                        // doctor-owned signal. Historical catch-up is measured across
                        // providers and remains explicitly partial while the retained
                        // daemon authority drains its bounded backlog.
                        status.session_history_catch_up = Some(
                            hotpath::future!(
                                historical_session_catch_up(db),
                                label = "mcp.info.status.session_history"
                            )
                            .await,
                        );
                    }
                    Err(error) => {
                        status.session_ingest = Some(json!({
                            "status": "unavailable",
                            "reason": "session_ingest_query_failed",
                            "message": error,
                        }));
                    }
                }
            }
        }
    }

    Ok(StatusResultV1::Project(Box::new(status)))
}

/// The `code_index_freshness.status` label of one freshness reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreshnessLabelV1 {
    Current,
    Stale,
    Restoring,
    Warming,
    Parked,
}

impl FreshnessLabelV1 {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Restoring => "restoring",
            Self::Warming => "warming",
            Self::Parked => "parked",
        }
    }

    fn with_worktree(self, worktree: CodeIndexWorktreeFreshnessV1) -> StatusCodeIndexFreshnessV1 {
        let worktree = Box::new(worktree);
        match self {
            Self::Current => StatusCodeIndexFreshnessV1::Current { worktree },
            Self::Stale => StatusCodeIndexFreshnessV1::Stale { worktree },
            Self::Restoring => StatusCodeIndexFreshnessV1::Restoring { worktree },
            Self::Warming => StatusCodeIndexFreshnessV1::Warming { worktree },
            Self::Parked => StatusCodeIndexFreshnessV1::Parked { worktree },
        }
    }
}

/// Project one freshness reading into the operator-facing status label and
/// optional warning.
///
/// Only the first is retryable-by-waiting: a `warming` read converges on its
/// own, while a `parked` read names a deterministic contract violation the
/// background worker re-checks every wake but can never fix by waiting, so
/// the warning carries the exact reason and remediation instead of a
/// wait-longer message.
fn code_index_freshness_projection(
    freshness: &CodeIndexWorktreeFreshnessV1,
) -> (FreshnessLabelV1, Option<String>) {
    let authoritative = freshness.is_authoritative();
    if let Some(parked) = freshness.parked.as_ref() {
        let warning = format!(
            "code-index background convergence is parked: {}; {}",
            parked.reason, parked.remediation
        );
        let status = if freshness.staleness_state == Some(CodeIndexStalenessStateV1::Parked) {
            FreshnessLabelV1::Parked
        } else if authoritative {
            FreshnessLabelV1::Current
        } else {
            FreshnessLabelV1::Warming
        };
        return (status, Some(warning));
    }
    if authoritative {
        let warning = freshness.omitted_sources.as_ref().map(|omitted| {
            format!(
                "{} captured source file(s) are not indexed; code_index_freshness.worktree.omitted_sources names them and why",
                omitted.count
            )
        });
        (FreshnessLabelV1::Current, warning)
    } else if freshness.staleness_state == Some(CodeIndexStalenessStateV1::Restoring) {
        let warning = if freshness.restore_progress.is_some() {
            "the sealed lexical artifact is completing bounded authentication before serving"
        } else {
            "the sealed generation is restoring its serving seats before serving"
        };
        (FreshnessLabelV1::Restoring, Some(warning.to_owned()))
    } else if freshness.staleness_state == Some(CodeIndexStalenessStateV1::Verifying) {
        (
            FreshnessLabelV1::Stale,
            Some(
                "the last complete code index remains available while the scheduler verifies source freshness"
                    .to_owned(),
            ),
        )
    } else {
        (
            FreshnessLabelV1::Warming,
            Some(
                "graph counts are not authoritative until the scheduler seals a complete fresh generation"
                    .to_owned(),
            ),
        )
    }
}

async fn session_git_evidence(ctx: &McpToolContext<'_>) -> StatusSessionGitEvidenceV1 {
    let Some((lease, _)) = ctx.authorized_project_session_db() else {
        return StatusSessionGitEvidenceV1::Unavailable {
            reason: StatusSessionGitEvidenceUnavailableV1::SessionStoreDenied,
            message: None,
        };
    };
    match GlobalDbGitCorrelationStore::new(lease.as_ref())
        .correlation_index_health()
        .await
    {
        Ok(health) => session_git_evidence_state(health),
        Err(error) => StatusSessionGitEvidenceV1::Unavailable {
            reason: StatusSessionGitEvidenceUnavailableV1::ReadFailed,
            message: Some(error.to_string()),
        },
    }
}

fn session_git_evidence_state(health: CorrelationIndexHealth) -> StatusSessionGitEvidenceV1 {
    match (health.generation, health.source_watermark) {
        (Some(generation), Some(source_watermark)) if health.projection_available => {
            StatusSessionGitEvidenceV1::Recorded {
                generation,
                source_watermark,
                span_count: health.span_count,
                commit_count: health.commit_count,
                backfill_watermark: health.backfill_watermark,
            }
        }
        _ => StatusSessionGitEvidenceV1::Unrecorded {
            backfill_watermark: health.backfill_watermark,
        },
    }
}

/// Reports daemon-owned historical warming when any provider's backlog exceeds
/// the ordinary catch-up threshold, so partial recall is never read as current.
async fn historical_session_catch_up(db: &RegisteredGlobalDb) -> Value {
    match db.session_ingest_health_for_provider(None).await {
        Ok(ingest) => historical_session_catch_up_state(&ingest),
        Err(error) => json!({
            "status": "unavailable",
            "coverage": "unknown",
            "authority": "daemon",
            "reason": "historical_backlog_measurement_failed",
            "message": error,
        }),
    }
}

fn historical_session_catch_up_state(ingest: &SessionIngestHealth) -> Value {
    use std::collections::BTreeSet;

    const THRESHOLD: u64 =
        tracedecay_sessions::runtime::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;
    let warming = ingest.max_transcript_pending_bytes > THRESHOLD;
    let observed = &ingest.observed_providers;
    let configured = observed
        .iter()
        .map(String::as_str)
        .chain(
            ingest
                .provider_coverage
                .iter()
                .map(|coverage| coverage.provider.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let unobserved = configured
        .iter()
        .copied()
        .filter(|provider| !observed.iter().any(|observed| observed == provider))
        .collect::<Vec<_>>();
    let coverage_incomplete = ingest.provider_coverage.iter().any(|coverage| {
        coverage.state != tracedecay_global_db::SessionProviderCoverageState::Complete
    }) || observed.iter().any(|provider| {
        tracedecay_sessions::runtime::SessionProvider::parse(provider).is_some_and(|provider| {
            provider.writes_typed_history_coverage()
                && !ingest.provider_coverage.iter().any(|coverage| {
                    coverage.provider == provider.id()
                        && coverage.state
                            == tracedecay_global_db::SessionProviderCoverageState::Complete
                })
        })
    });
    let any_provider_available = ingest.provider_coverage.iter().any(|coverage| {
        coverage.state != tracedecay_global_db::SessionProviderCoverageState::Unavailable
    });
    let source_unavailable = observed.is_empty() && !any_provider_available;
    json!({
        "status": if source_unavailable {
            "unavailable"
        } else if warming || coverage_incomplete {
            "warming"
        } else {
            "current"
        },
        "coverage": if source_unavailable || warming || coverage_incomplete {
            "partial"
        } else {
            "complete"
        },
        "authority": "daemon",
        "reason": if source_unavailable {
            "historical_sources_unobserved"
        } else if warming {
            "historical_transcript_backlog"
        } else if coverage_incomplete {
            "historical_provider_coverage_incomplete"
        } else {
            "historical_catch_up_current"
        },
        "providers": observed,
        "provider_coverage": ingest.provider_coverage,
        "unobserved_providers": unobserved,
        "max_transcript_pending_bytes": ingest.max_transcript_pending_bytes,
        "pending_bytes": ingest.pending_bytes,
        "pending_transcripts": ingest.pending_transcripts,
        "message": if source_unavailable {
            "No durable historical source rows or provider frontiers are currently observable."
        } else if warming || coverage_incomplete {
            "Historical session recall is partially available while the daemon continues bounded background catch-up."
        } else {
            "Historical session recall catch-up is current."
        },
    })
}

pub(crate) fn render_status_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Project Status");
    if let Some(obj) = value.as_object() {
        let mut warnings: Vec<String> = Vec::new();
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for k in keys {
            let v = &obj[k];
            if k.contains("warning")
                && let Some(s) = v.as_str()
            {
                warnings.push(s.to_string());
                continue;
            }
            match v {
                Value::String(s) => {
                    md.field(k, s);
                }
                Value::Number(n) => {
                    md.field(k, &n.to_string());
                }
                Value::Bool(b) => {
                    md.field(k, &b.to_string());
                }
                Value::Array(a) => {
                    md.field(k, &format!("{} item(s)", a.len()));
                    if k == "reset_required_stores" {
                        for store in a {
                            md.bullet(&format!(
                                "pending operator action: {} requires reset ({}), run `{}`",
                                store["store"].as_str().unwrap_or_default(),
                                store["reason"].as_str().unwrap_or_default(),
                                store["remedy"].as_str().unwrap_or_default(),
                            ));
                        }
                    }
                }
                Value::Object(o) if k == "session_projection" => {
                    let state = o.get("state").and_then(Value::as_str).unwrap_or_default();
                    match o.get("reason").and_then(Value::as_str) {
                        Some(reason) => md.field(k, &format!("{state} ({reason})")),
                        None => md.field(k, state),
                    };
                }
                Value::Object(o) if k == "wait" => {
                    let outcome = o.get("outcome").and_then(Value::as_str).unwrap_or_default();
                    match o
                        .get("last_state")
                        .or_else(|| o.get("reason"))
                        .and_then(Value::as_str)
                    {
                        Some(detail) => md.field(k, &format!("{outcome} ({detail})")),
                        None => md.field(k, outcome),
                    };
                }
                Value::Object(o) => {
                    if let Some(status) = o.get("status").and_then(Value::as_str) {
                        md.field(&format!("{k}.status"), status);
                    } else {
                        md.field(k, &format!("{{{} field(s)}}", o.len()));
                    }
                    if k == "memory"
                        && let Some(owners) = o.get("owners").and_then(Value::as_array)
                    {
                        for owner in owners {
                            md.bullet(&owner.to_string());
                        }
                    }
                    if k == "schema_convergence"
                        && let Some(findings) = o.get("findings").and_then(Value::as_array)
                    {
                        for finding in findings {
                            md.bullet(&finding.to_string());
                        }
                    }
                }
                Value::Null => {}
            }
        }
        if !warnings.is_empty() {
            md.blank().heading(3, "Warnings");
            for w in &warnings {
                md.bullet(w);
            }
        }
    }
    md.render()
}

fn storage_mode_name(mode: &StorageMode) -> &'static str {
    match mode {
        StorageMode::ProfileSharded => "profile_sharded",
    }
}

fn store_kind_name(kind: &StoreKind) -> &'static str {
    match kind {
        StoreKind::CodeProject => "code_project",
    }
}

/// Computes `tracedecay_active_project` for the admitted project.
#[hotpath::measure(label = "mcp.info.active_project.total")]
pub async fn compute_active_project(
    ctx: &McpToolContext<'_>,
    scope_prefix: Option<&str>,
) -> ActiveProjectResultV1 {
    let freshness_payload = ctx.freshness().await;
    let ready_serving_source = ready_serving_source(freshness_payload.as_ref());
    let branch = ctx.branch_diagnostics_for_serving_source(
        ready_serving_source.map(|source| source.reference),
        ready_serving_source.and_then(|source| source.revision),
        ready_serving_source.is_some_and(|source| source.current_source_verified),
    );
    let layout = ctx.store_layout();
    let graph_db_path = ctx.graph_db_path();
    ActiveProjectResultV1 {
        project_id: layout.identity.project_id.clone(),
        repository_id: ctx.admitted_scope().repository_id.as_str().to_owned(),
        project_root: display_path(ctx.project_root()),
        resolution_source: ActiveProjectResolutionSourceV1::ActiveProject,
        storage: ActiveProjectStorageV1 {
            class: store_kind_name(&layout.store_kind).to_owned(),
            mode: storage_mode_name(&layout.storage_mode).to_owned(),
            data_root: display_path(&layout.data_root),
            graph_db_path: display_path(graph_db_path),
            graph_db_exists: graph_db_path.exists(),
            graph_db_size_bytes: graph_db_path
                .metadata()
                .map_or(0, |metadata| metadata.len()),
            sessions_db_path: display_path(&layout.sessions_db_path),
            response_handle_root: display_path(&layout.response_handle_root),
            lcm_payload_root: display_path(&layout.lcm_payload_root),
        },
        branch: ActiveProjectBranchV1 {
            current_branch: branch.current_branch,
            open_active_branch: branch.open_active_branch,
            serving_branch: branch.serving_branch,
            branch_resolution: branch.branch_resolution,
            branch_drifted: branch.branch_drifted,
            tracked_branch_count: branch.tracked_branch_count,
            warnings: branch.warnings,
        },
        scope_prefix: scope_prefix.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_global_db::{
        SessionIngestHealth, SessionProviderCoverage, SessionProviderCoverageState,
    };
    use tracedecay_runtime_core::runtime_telemetry::{
        GenerationCensusServingFreshness, GenerationCensusSnapshot, GenerationCensusStatistics,
        GenerationCensusUnavailableReason,
    };

    use super::{
        CodeIndexReadinessWaitReadV1, CorrelationIndexHealth, FreshnessLabelV1,
        code_index_freshness_projection, code_index_freshness_status, git_staleness,
        graph_statistics_value, historical_session_catch_up_state, readiness_wait_outcome,
        render_status_md, schema_convergence_status, session_git_evidence_state,
    };
    use tracedecay_contracts::code_index_freshness::{
        CodeIndexFreshnessCoverageV1, CodeIndexFreshnessPayloadV1, CodeIndexFreshnessReadFailureV1,
        CodeIndexOmittedSourceV1, CodeIndexOmittedSourcesV1, CodeIndexSourceOmissionReasonV1,
        CodeIndexStalenessStateV1,
    };
    use tracedecay_contracts::retrieval::{StatusCodeIndexFreshnessV1, StatusRetrievalServingV1};
    use tracedecay_contracts::storage::{
        SchemaConvergenceFindingV1, SchemaConvergenceProgressV1, SchemaConvergenceStageV1,
        SchemaConvergenceStateV1,
    };

    #[test]
    fn failed_mount_is_a_typed_status_with_operator_remediation() {
        let payload = CodeIndexFreshnessPayloadV1::from_read_failure(
            CodeIndexFreshnessReadFailureV1::MountFailed,
        );

        let (status, warning, retrieval) = code_index_freshness_status(Some(&payload));

        assert_eq!(
            status,
            StatusCodeIndexFreshnessV1::MountFailed {
                message: "the code-index scheduler could not mount for this project".to_owned(),
                remediation: "run `tracedecay sync` to retry the code-index mount".to_owned(),
            }
        );
        assert_eq!(
            retrieval,
            Some(StatusRetrievalServingV1::Unavailable {
                reason: "code_index_mount_failed".to_owned(),
            })
        );
        let warning = warning.expect("a failed mount carries an operator warning");
        assert!(warning.contains("could not mount"));
        assert!(warning.contains("tracedecay sync"));
    }

    #[test]
    fn session_git_evidence_reports_the_installed_generation_or_its_absence() {
        let recorded = session_git_evidence_state(CorrelationIndexHealth {
            projection_available: true,
            generation: Some("git-evidence:3".to_owned()),
            source_watermark: Some("1790000000:42".to_owned()),
            span_count: 4,
            commit_count: 2,
            backfill_watermark: Some(1_790_000_000),
        });
        let unrecorded = session_git_evidence_state(CorrelationIndexHealth {
            projection_available: false,
            generation: None,
            source_watermark: None,
            span_count: 0,
            commit_count: 0,
            backfill_watermark: None,
        });

        assert_eq!(
            serde_json::to_value(recorded).unwrap(),
            serde_json::json!({
                "status": "recorded",
                "generation": "git-evidence:3",
                "source_watermark": "1790000000:42",
                "span_count": 4,
                "commit_count": 2,
                "backfill_watermark": 1_790_000_000,
            })
        );
        assert_eq!(
            serde_json::to_value(unrecorded).unwrap(),
            serde_json::json!({ "status": "unrecorded", "backfill_watermark": null })
        );
        assert_eq!(
            render_status_md(&serde_json::json!({
                "session_git_evidence": { "status": "recorded" },
                "session_projection": { "state": "stale", "reason": "historical_convergence" },
            })),
            "## Project Status\n\
             **session_git_evidence.status:** recorded\n\
             **session_projection:** stale (historical_convergence)\n"
        );
    }

    struct HeldDecode;

    impl tracedecay_runtime_core::resident_memory::ResidentOwnerV1 for HeldDecode {
        fn sample(
            &self,
        ) -> Option<tracedecay_runtime_core::resident_memory::ResidentOwnerSampleV1> {
            Some(
                tracedecay_runtime_core::resident_memory::ResidentOwnerSampleV1 {
                    holding:
                        tracedecay_runtime_core::resident_memory::ResidentHoldingV1::Generation(
                            tracedecay_domain::CodeGenerationId::new("generation.fixture")
                                .expect("generation id"),
                        ),
                    bytes: tracedecay_runtime_core::resident_memory::ResidentOwnerBytesV1::Measured(
                        4_096,
                    ),
                    last_used: std::time::Instant::now(),
                    serving: true,
                    shared: None,
                },
            )
        }

        fn release(&self) -> tracedecay_runtime_core::resident_memory::ResidentOwnerReleaseV1 {
            tracedecay_runtime_core::resident_memory::ResidentOwnerReleaseV1::Busy
        }
    }

    #[test]
    fn status_memory_reports_the_projects_own_owners_and_daemon_totals() {
        use std::sync::Arc;
        use tracedecay_runtime_core::resident_memory::{
            ResidentMemoryPressureV1, ResidentOwnerKindV1, ResidentOwnerScopeV1, ResidentOwnerV1,
            ResidentOwnersV1,
        };
        let pressure =
            ResidentMemoryPressureV1::new(std::num::NonZeroU64::new(10_000).expect("limit"));
        pressure.publish_observed_resident_bytes(6_000);
        let owners = Arc::new(ResidentOwnersV1::new(std::time::Duration::from_mins(10)));
        let owner: Arc<dyn ResidentOwnerV1> = Arc::new(HeldDecode);
        let project = tracedecay_domain::ProjectId::new("project.fixture").expect("project id");
        let _registrations = [
            (project.clone(), "worktree.fixture"),
            (
                tracedecay_domain::ProjectId::new("project.other").expect("project id"),
                "worktree.other",
            ),
        ]
        .map(|(project_id, worktree)| {
            owners
                .register(
                    ResidentOwnerScopeV1 {
                        project_id,
                        worktree_id: tracedecay_domain::WorktreeId::new(worktree)
                            .expect("worktree id"),
                    },
                    ResidentOwnerKindV1::DecodedGeneration,
                    Arc::downgrade(&owner),
                )
                .expect("register")
        });

        let memory = super::memory_value(
            &pressure,
            &owners,
            Some(1.5),
            std::time::Instant::now(),
            &project,
        );

        assert_eq!(
            serde_json::to_value(memory).expect("memory serializes"),
            serde_json::json!({
                "status": "nominal",
                "resident_bytes": 6_000,
                "limit_bytes": 10_000,
                "high_watermark_bytes": 9_000,
                "low_watermark_bytes": 7_500,
                "psi_some_avg10": 1.5,
                "idle_window_seconds": 600,
                "shed_order": ["retained_parses", "superseded_generation", "graph_catalog", "decoded_generation", "graph_engine", "session"],
                "retained_bytes": 8_192,
                "unmeasured_owners": 0,
                "owners": [{
                    "project_id": "project.fixture",
                    "kind": "decoded_generation",
                    "holders": [{
                        "worktree_id": "worktree.fixture",
                        "holding": "generation.fixture",
                    }],
                    "content_digest": null,
                    "bytes": 4_096,
                    "measured": true,
                    "idle_seconds": 0,
                    "protected": true,
                }],
            })
        );
    }

    #[test]
    fn status_markdown_exposes_nested_status_without_expanding_other_objects() {
        let rendered = render_status_md(&serde_json::json!({
            "code_index_freshness": {"status": "stale", "coverage": "partial"},
            "branch": {"current_branch": "main", "tracked_branch_count": 1}
        }));

        assert!(rendered.contains("**code_index_freshness.status:** stale"));
        assert!(rendered.contains("**branch:** {2 field(s)}"));
        assert!(!rendered.contains("coverage"));
    }

    #[test]
    fn status_preserves_each_schema_convergence_state() {
        for (state, expected) in [
            (
                SchemaConvergenceStateV1::PendingSchemaMigration,
                "in_progress",
            ),
            (
                SchemaConvergenceStateV1::ReleasedShapeConvergenceInProgress,
                "in_progress",
            ),
            (SchemaConvergenceStateV1::Degraded, "degraded"),
            (SchemaConvergenceStateV1::Completed, "completed"),
        ] {
            let finding = SchemaConvergenceFindingV1 {
                store: "profile-sessions".to_owned(),
                stage: SchemaConvergenceStageV1::RegisteredSchema,
                state,
                progress: Some(SchemaConvergenceProgressV1::Rows {
                    done: 3,
                    remaining: 7,
                }),
                started_at_micros: 42,
                degraded_row: (state == SchemaConvergenceStateV1::Degraded)
                    .then(|| "observation_id=obs-7".to_owned()),
            };
            let value = serde_json::to_value(schema_convergence_status(&[finding]))
                .expect("schema convergence serializes");
            assert_eq!(value["status"], expected);
            assert_eq!(value["findings"][0]["state"], serde_json::json!(state));
            assert_eq!(value["findings"][0]["started_at_micros"], 42);
            let rendered = render_status_md(&serde_json::json!({"schema_convergence": value}));
            assert!(rendered.contains(state.as_str()));
            assert!(rendered.contains("profile-sessions"));
            assert!(rendered.contains(r#""done":3"#));
            assert!(rendered.contains(r#""remaining":7"#));
            if state == SchemaConvergenceStateV1::Degraded {
                assert!(rendered.contains("observation_id=obs-7"));
            }
        }
    }

    /// The daemon serializes `graph_statistics` and `tracedecay status`
    /// deserializes it as the same Rust type. This round-trip is the wire
    /// contract: if either side drifts, this test fails before a user sees a
    /// `missing field` decode error.
    #[test]
    fn graph_statistics_round_trips_the_cli_status_decode() {
        let absent = graph_statistics_value(None).expect("typed absence serializes");
        let decoded: GenerationCensusSnapshot =
            serde_json::from_value(absent).expect("CLI decodes typed absence");
        assert_eq!(
            decoded,
            GenerationCensusSnapshot::Unavailable {
                reason: GenerationCensusUnavailableReason::AuthorityUnavailable,
            }
        );

        let observed = GenerationCensusSnapshot::Observed {
            generation_id: "generation.fixture".to_owned(),
            freshness: GenerationCensusServingFreshness::LastCompleteStale {
                sealed_at_micros: 42,
                rebuild_in_flight: true,
            },
            statistics: GenerationCensusStatistics {
                source_total_bytes: 1_024,
                symbol_count: 12,
                edge_count: 7,
            },
        };
        let value = graph_statistics_value(Some(&observed)).expect("observed census serializes");
        let decoded: GenerationCensusSnapshot =
            serde_json::from_value(value).expect("CLI decodes observed census");
        assert_eq!(decoded, observed);
    }

    #[test]
    fn a_timed_out_wait_names_the_last_status_label() {
        let seated_graph_pending =
            tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
                worktree_root: "/project".to_owned(),
                latest_generation_id: Some("generation.fixture".to_owned()),
                staleness_state: Some(CodeIndexStalenessStateV1::Fresh),
                coverage: CodeIndexFreshnessCoverageV1::Complete,
                code_graph_serving: Some(
                    tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Pending,
                ),
                ..Default::default()
            };
        let rebuilding = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/project".to_owned(),
            staleness_state: Some(CodeIndexStalenessStateV1::Indexing),
            coverage: CodeIndexFreshnessCoverageV1::PartialRefreshInProgress,
            ..Default::default()
        };
        let reached_reading = Box::new(seated_graph_pending.clone());
        for (last, expected) in [
            (Some(Box::new(seated_graph_pending)), "current"),
            (Some(Box::new(rebuilding)), "warming"),
            (None, "not_mounted"),
        ] {
            assert_eq!(
                serde_json::to_value(readiness_wait_outcome(
                    CodeIndexReadinessWaitReadV1::TimedOut { last }
                ))
                .expect("outcome serializes"),
                serde_json::json!({ "outcome": "timed_out", "last_state": expected })
            );
        }
        assert_eq!(
            serde_json::to_value(readiness_wait_outcome(
                CodeIndexReadinessWaitReadV1::Reached {
                    reading: reached_reading
                }
            ))
            .expect("outcome serializes"),
            serde_json::json!({ "outcome": "reached" })
        );
    }

    #[test]
    fn a_parked_deterministic_violation_reports_parked_not_warming() {
        let freshness = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/project".to_owned(),
            staleness_state: Some(CodeIndexStalenessStateV1::Parked),
            coverage: CodeIndexFreshnessCoverageV1::Complete,
            parked: Some(
                tracedecay_contracts::code_index_freshness::CodeIndexConvergenceParkedV1 {
                    reason: "code text artifacts root is not owner-private (mode 775, need 700)"
                        .to_owned(),
                    blocked_reason: None,
                    remediation: "restore owner-only access".to_owned(),
                    parked_at_micros: 42,
                    observed_passes: 3,
                    retries_on_wake: true,
                },
            ),
            ..Default::default()
        };

        let (status, warning) = code_index_freshness_projection(&freshness);

        assert_eq!(status, FreshnessLabelV1::Parked);
        let warning = warning.expect("a parked read carries the reason");
        assert!(warning.contains("not owner-private (mode 775, need 700)"));
        assert!(warning.contains("restore owner-only access"));
    }

    /// The watermark is the sealed generation's commit: current while HEAD
    /// stays on it, stale with both commits once HEAD moves, and unavailable
    /// before any generation seals.
    #[test]
    fn git_staleness_compares_the_sealed_watermark_with_head() {
        let repository = tempfile::TempDir::new().expect("repository");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(args)
                .current_dir(repository.path())
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {args:?}: {output:?}");
            String::from_utf8(output.stdout)
                .expect("utf-8")
                .trim()
                .to_owned()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "sealed"]);
        let sealed = git(&["rev-parse", "HEAD"]);
        let payload = |latest_generation_id: Option<&str>| {
            tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1 {
                worktrees: vec![
                    tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
                        worktree_root: repository.path().display().to_string(),
                        latest_generation_id: latest_generation_id.map(str::to_owned),
                        source_revision: Some(sealed.clone()),
                        ..Default::default()
                    },
                ],
                note: String::new(),
                mount_failure: None,
            }
        };
        let staleness = |payload| {
            serde_json::to_value(git_staleness(Some(&payload), repository.path()))
                .expect("staleness serializes")
        };

        assert_eq!(
            staleness(payload(Some("generation.fixture"))),
            serde_json::json!({ "status": "current", "watermark": sealed })
        );
        git(&["commit", "-q", "--allow-empty", "-m", "moved"]);
        let head = git(&["rev-parse", "HEAD"]);
        assert_eq!(
            staleness(payload(Some("generation.fixture"))),
            serde_json::json!({ "status": "stale", "watermark": sealed, "head": head })
        );
        assert_eq!(
            staleness(payload(None)),
            serde_json::json!({ "status": "unavailable", "reason": "no_sealed_generation" })
        );
    }

    #[test]
    fn status_freshness_preserves_typed_graph_serving_readiness() {
        let freshness = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/project".to_owned(),
            code_graph_serving: Some(
                tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Ready,
            ),
            clone_index: Some(
                tracedecay_contracts::code_index_freshness::CodeCloneIndexStatusV1::default(),
            ),
            ..Default::default()
        };

        let value = serde_json::to_value(freshness).expect("freshness serializes");
        assert_eq!(
            value["code_graph_serving"],
            serde_json::json!({ "state": "ready" })
        );
        assert_eq!(
            value["clone_index"],
            serde_json::json!({
                "state": "unavailable",
                "reason": "clone index authority is unavailable"
            })
        );
    }

    #[test]
    fn a_serving_worktree_with_a_parked_newer_build_stays_current_but_warns() {
        let freshness = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/project".to_owned(),
            latest_generation_id: Some("generation.fixture".to_owned()),
            staleness_state: Some(CodeIndexStalenessStateV1::Fresh),
            coverage: CodeIndexFreshnessCoverageV1::Complete,
            parked: Some(
                tracedecay_contracts::code_index_freshness::CodeIndexConvergenceParkedV1 {
                    reason: "code text artifacts root is not owner-private".to_owned(),
                    blocked_reason: None,
                    remediation: "restore owner-only access".to_owned(),
                    parked_at_micros: 42,
                    observed_passes: 1,
                    retries_on_wake: true,
                },
            ),
            ..Default::default()
        };

        let (status, warning) = code_index_freshness_projection(&freshness);

        assert_eq!(status, FreshnessLabelV1::Current);
        assert!(warning.expect("the park stays visible").contains("parked"));
    }

    #[test]
    fn an_unparked_incomplete_read_stays_warming() {
        let freshness = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/project".to_owned(),
            staleness_state: Some(CodeIndexStalenessStateV1::Indexing),
            coverage: CodeIndexFreshnessCoverageV1::Complete,
            ..Default::default()
        };

        let (status, warning) = code_index_freshness_projection(&freshness);

        assert_eq!(status, FreshnessLabelV1::Warming);
        assert!(
            warning
                .expect("warming names itself")
                .contains("not authoritative")
        );
    }

    #[test]
    fn bounded_artifact_restore_is_named_without_a_rebuild() {
        let freshness = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/project".to_owned(),
            staleness_state: Some(CodeIndexStalenessStateV1::Restoring),
            rebuild_in_flight: false,
            coverage: CodeIndexFreshnessCoverageV1::PartialArtifactRestore,
            restore_progress: Some(
                tracedecay_contracts::code_index_freshness::CodeIndexRestoreProgressV1 {
                    generation_id: "generation.fixture".to_owned(),
                    artifact_digest: format!("sha256:{}", "a".repeat(64)),
                    authenticated_completed: 4,
                    authenticated_total: 6,
                    authenticated_remaining: 2,
                },
            ),
            ..Default::default()
        };

        let (status, warning) = code_index_freshness_projection(&freshness);

        assert_eq!(status, FreshnessLabelV1::Restoring);
        assert!(!freshness.rebuild_in_flight);
        assert!(
            warning
                .expect("restore names its bounded work")
                .contains("bounded authentication")
        );

        let reseating = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            restore_progress: None,
            ..freshness
        };
        assert_eq!(
            code_index_freshness_projection(&reseating),
            (
                FreshnessLabelV1::Restoring,
                Some(
                    "the sealed generation is restoring its serving seats before serving"
                        .to_owned()
                )
            )
        );
    }

    /// Sources no generation can index leave the read current, but status
    /// says how many captured sources the index does not hold.
    #[test]
    fn a_fresh_read_with_omitted_sources_is_current_and_names_their_count() {
        let freshness = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/project".to_owned(),
            latest_generation_id: Some("generation.fixture".to_owned()),
            staleness_state: Some(CodeIndexStalenessStateV1::Fresh),
            coverage: CodeIndexFreshnessCoverageV1::PartialOmittedSources,
            omitted_sources: Some(CodeIndexOmittedSourcesV1 {
                count: 2,
                sources: vec![CodeIndexOmittedSourceV1 {
                    git_path_bytes: b"src/odd\\name.rs".to_vec(),
                    display_path: "src/odd\\name.rs".to_owned(),
                    reason: CodeIndexSourceOmissionReasonV1::UnrepresentablePath,
                }],
            }),
            ..Default::default()
        };

        let (status, warning) = code_index_freshness_projection(&freshness);

        assert_eq!(status, FreshnessLabelV1::Current);
        assert!(
            warning
                .expect("omitted sources are named")
                .starts_with("2 captured source file(s) are not indexed")
        );
        let complete = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            coverage: CodeIndexFreshnessCoverageV1::Complete,
            omitted_sources: None,
            ..freshness
        };
        assert_eq!(
            code_index_freshness_projection(&complete),
            (FreshnessLabelV1::Current, None)
        );
    }

    #[test]
    fn a_ready_generation_under_source_verification_is_stale_not_warming() {
        let freshness = tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/project".to_owned(),
            latest_generation_id: Some("generation.fixture".to_owned()),
            staleness_state: Some(CodeIndexStalenessStateV1::Verifying),
            coverage: CodeIndexFreshnessCoverageV1::PartialSourceVerification,
            ..Default::default()
        };

        let (status, warning) = code_index_freshness_projection(&freshness);

        assert_eq!(status, FreshnessLabelV1::Stale);
        assert!(
            warning
                .expect("verification is named")
                .contains("verifies source freshness")
        );
    }

    #[test]
    fn historical_backlog_is_typed_daemon_owned_warming() {
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["cursor".into()],
            pending_transcripts: 2,
            pending_bytes: 12_000_000,
            max_transcript_pending_bytes:
                tracedecay_sessions::runtime::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES + 1,
            ..SessionIngestHealth::default()
        });

        assert_eq!(state["status"], "warming");
        assert_eq!(state["coverage"], "partial");
        assert_eq!(state["authority"], "daemon");
        assert!(!state.to_string().contains("sessions ingest"));
    }

    #[test]
    fn historical_status_names_database_and_discovery_backed_providers() {
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["kimi".into(), "opencode".into()],
            ..SessionIngestHealth::default()
        });
        let providers = state["providers"].as_array().unwrap();

        assert!(providers.iter().any(|provider| provider == "kimi"));
        assert!(providers.iter().any(|provider| provider == "opencode"));
        assert_eq!(state["status"], "warming");
        assert_eq!(state["coverage"], "partial");
        assert_eq!(state["reason"], "historical_provider_coverage_incomplete");
    }

    #[test]
    fn historical_status_does_not_wait_for_non_coverage_provider_writers() {
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["cursor".into()],
            ..SessionIngestHealth::default()
        });

        assert_eq!(state["status"], "current");
        assert_eq!(state["coverage"], "complete");
    }

    #[test]
    fn historical_status_is_current_only_after_every_provider_sweep_completes() {
        let provider_coverage = tracedecay_sessions::runtime::SessionProvider::ALL
            .iter()
            .map(|provider| SessionProviderCoverage {
                provider: provider.id().to_owned(),
                state: SessionProviderCoverageState::Complete,
                deferred_units: 0,
                reason: None,
            })
            .collect();
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["kimi".into()],
            provider_coverage,
            ..SessionIngestHealth::default()
        });

        assert_eq!(state["status"], "current");
        assert_eq!(state["coverage"], "complete");
    }

    #[test]
    fn explicit_partial_provider_sweep_never_reports_current() {
        let provider_coverage = tracedecay_sessions::runtime::SessionProvider::ALL
            .iter()
            .map(|provider| SessionProviderCoverage {
                provider: provider.id().to_owned(),
                state: if provider.id() == "opencode" {
                    SessionProviderCoverageState::Partial
                } else {
                    SessionProviderCoverageState::Complete
                },
                deferred_units: u64::from(provider.id() == "opencode"),
                reason: None,
            })
            .collect();
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["opencode".into()],
            provider_coverage,
            ..SessionIngestHealth::default()
        });

        assert_eq!(state["status"], "warming");
        assert_eq!(state["coverage"], "partial");
    }

    #[test]
    fn historical_status_does_not_fabricate_provider_readiness() {
        let state = historical_session_catch_up_state(&SessionIngestHealth::default());

        assert_eq!(state["status"], "unavailable");
        assert_eq!(state["coverage"], "partial");
        assert!(state["providers"].as_array().unwrap().is_empty());
    }
}
