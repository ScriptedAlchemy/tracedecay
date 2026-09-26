//! Provider capture kernels for `ingest_transcript`.
//!
//! Every supported `(provider, user_scope, payload_route)` route resolves to
//! one kernel that owns all of that provider's capture logic and reports its
//! result through the shared [`TranscriptCaptureOutcome`]. The dispatch site
//! therefore carries no per-provider control flow: it looks the kernel up in
//! [`transcript_capture_kernel`], awaits it, and assembles the response from
//! whichever optional fields the outcome carries.
//!
//! `payload_route` is part of the key because a provider can be reached two
//! ways: `TraceDecay` scanning the host's own on-disk sources, or the host
//! inlining a turn's messages in the request. Both are capture routes and both
//! belong in the registry, expressing the second one as a branch above the
//! lookup is what previously let it skip admission entirely.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::Value;
use tracedecay_domain::ObservationScopeV1;

use tracedecay_automation_runtime::automation::config_error;
use tracedecay_domain::errors::Result;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_host_admission::HostAdmissionFacade;
use tracedecay_project::project::TraceDecay;
use tracedecay_sessions::admission::HostAdmissionStatus;
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_sessions::runtime::hosts::claude_observation::ClaudeObservationIngestStats;
use tracedecay_sessions::runtime::hosts::hermes::HermesSweepOutcome;
use tracedecay_sessions::runtime::snapshot_observation::SnapshotCaptureOutcome;

use super::super::{required_str, required_user_db};
use super::{
    admit_codex_project_rollouts, drain_host_observation_projections, project_observation_id,
};
use crate::handlers::SessionAuthorities;
use crate::{
    hook_admission_error, map_claude_observation_ingest_error, map_transcript_ingest_error,
};

/// Which payload shape a hook ingest request carries.
///
/// Part of the capture-registry key, so an inline-payload route is a first
/// class entry rather than a branch that bypasses the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TranscriptPayloadRouteV1 {
    /// `TraceDecay` scans the host's own on-disk transcript sources.
    SourceScan,
    /// The host inlined this turn's messages in the request.
    InlineMessages,
}

impl TranscriptPayloadRouteV1 {
    pub(super) fn from_args(args: &Value) -> Self {
        if args.get("messages").is_some() {
            Self::InlineMessages
        } else {
            Self::SourceScan
        }
    }
}

/// Everything a capture kernel may borrow for one ingest pass.
#[derive(Clone)]
pub(super) struct TranscriptCaptureContext<'a> {
    pub(super) cg: Option<&'a TraceDecay>,
    pub(super) args: &'a Value,
    pub(super) user_scope: bool,
    pub(super) profile_root: Option<&'a Path>,
    pub(super) global_db: Option<&'a RegisteredGlobalDb>,
    pub(super) session_authorities: SessionAuthorities<'a>,
    pub(super) facade: &'a HostAdmissionFacade<'a>,
    pub(super) max_new_bytes: Option<u64>,
    pub(super) cancellation: &'a ObservationCancellation,
}

impl<'a> TranscriptCaptureContext<'a> {
    fn profile_root(&self) -> Result<&'a Path> {
        self.profile_root
            .ok_or_else(|| config_error("missing client profile"))
    }

    fn global_db(&self) -> Result<&'a RegisteredGlobalDb> {
        self.global_db
            .ok_or_else(|| config_error("missing client registry"))
    }

    fn project(&self) -> Result<&'a TraceDecay> {
        self.cg
            .ok_or_else(|| config_error("project transcript ingest requires a project"))
    }
}

/// Registered project roots as seen by the daemon session registry.
async fn registered_project_roots(global_db: &RegisteredGlobalDb) -> Result<Vec<PathBuf>> {
    let registry_authority =
        tracedecay_host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority::new(
            global_db,
        );
    tracedecay_sessions::runtime::registered_project_roots_from(&registry_authority)
        .await
        .ok_or_else(|| config_error("daemon project registry is unavailable"))
}

/// Structured result of one capture kernel.
///
/// Providers that surface more than a message count report it here rather than
/// through out-params at the dispatch site: Claude fills `claude_observation`,
/// snapshot providers fill `snapshot`, and byte-capped scans that left work
/// behind set `source_deferred`.
#[derive(Default)]
pub(super) struct TranscriptCaptureOutcome {
    pub(super) messages_upserted: u64,
    pub(super) snapshot: Option<SnapshotCaptureOutcome>,
    pub(super) claude_observation: Option<ClaudeObservationIngestStats>,
    pub(super) source_deferred: bool,
    /// Observations the route durably admitted, whoever later projects them.
    /// `messages_upserted` counts only the projections this pass drained
    /// itself, which a peer drainer can legitimately take first.
    pub(super) observations_committed: u64,
    /// This route's admission tally is the commit. The projection drain is a
    /// shared per-scope queue, so its residual must not enter the terminal
    /// status. Routes that have no admission tally leave this false and keep
    /// using their own message counts.
    pub(super) admission_owns_commit: bool,
    /// The route committed nothing because its observations were already
    /// durable. Kept apart from `messages_upserted == 0`, which cannot tell an
    /// already-committed replay from a pass that captured nothing.
    pub(super) exact_duplicate: bool,
}

type TranscriptCaptureFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TranscriptCaptureOutcome>> + Send + 'a>>;

/// One provider's capture logic for one admission scope.
pub(super) trait TranscriptCaptureKernelV1: Sync {
    fn capture<'a>(&self, ctx: TranscriptCaptureContext<'a>) -> TranscriptCaptureFuture<'a>;
}

macro_rules! transcript_capture_kernels {
    ($($kernel:ident => $capture:path),+ $(,)?) => {
        $(
            struct $kernel;

            impl TranscriptCaptureKernelV1 for $kernel {
                fn capture<'a>(
                    &self,
                    ctx: TranscriptCaptureContext<'a>,
                ) -> TranscriptCaptureFuture<'a> {
                    Box::pin($capture(ctx))
                }
            }
        )+
    };
}

transcript_capture_kernels! {
    ClaudeProfileKernelV1 => capture_claude_profile,
    CodexProfileKernelV1 => capture_codex_profile,
    CursorProfileKernelV1 => capture_cursor_profile,
    HermesProfileKernelV1 => capture_hermes_profile,
    KiroProfileKernelV1 => capture_kiro_profile,
    CodexProjectKernelV1 => capture_codex_project,
    CursorProjectKernelV1 => capture_cursor_project,
    HermesProjectKernelV1 => capture_hermes_project,
    KiroProjectKernelV1 => capture_kiro_project,
    PiProjectKernelV1 => capture_pi_project,
    HermesCallbackKernelV1 => capture_hermes_callback,
}

/// The `(provider, user_scope, payload_route)` capture registry.
const TRANSCRIPT_CAPTURE_KERNELS: &[(
    &str,
    bool,
    TranscriptPayloadRouteV1,
    &dyn TranscriptCaptureKernelV1,
)] = &[
    (
        "claude",
        true,
        TranscriptPayloadRouteV1::SourceScan,
        &ClaudeProfileKernelV1,
    ),
    (
        "codex",
        true,
        TranscriptPayloadRouteV1::SourceScan,
        &CodexProfileKernelV1,
    ),
    (
        "cursor",
        true,
        TranscriptPayloadRouteV1::SourceScan,
        &CursorProfileKernelV1,
    ),
    (
        "hermes",
        true,
        TranscriptPayloadRouteV1::SourceScan,
        &HermesProfileKernelV1,
    ),
    (
        "kiro",
        true,
        TranscriptPayloadRouteV1::SourceScan,
        &KiroProfileKernelV1,
    ),
    (
        "codex",
        false,
        TranscriptPayloadRouteV1::SourceScan,
        &CodexProjectKernelV1,
    ),
    (
        "cursor",
        false,
        TranscriptPayloadRouteV1::SourceScan,
        &CursorProjectKernelV1,
    ),
    (
        "hermes",
        false,
        TranscriptPayloadRouteV1::SourceScan,
        &HermesProjectKernelV1,
    ),
    (
        "kiro",
        false,
        TranscriptPayloadRouteV1::SourceScan,
        &KiroProjectKernelV1,
    ),
    (
        "pi",
        false,
        TranscriptPayloadRouteV1::SourceScan,
        &PiProjectKernelV1,
    ),
    (
        "hermes",
        true,
        TranscriptPayloadRouteV1::InlineMessages,
        &HermesCallbackKernelV1,
    ),
    (
        "hermes",
        false,
        TranscriptPayloadRouteV1::InlineMessages,
        &HermesCallbackKernelV1,
    ),
];

/// Resolves the capture kernel registered for one transcript route, if any.
pub(super) fn transcript_capture_kernel(
    provider: &str,
    user_scope: bool,
    payload_route: TranscriptPayloadRouteV1,
) -> Option<&'static dyn TranscriptCaptureKernelV1> {
    TRANSCRIPT_CAPTURE_KERNELS
        .iter()
        .find(|(id, scope, route, _)| {
            *id == provider && *scope == user_scope && *route == payload_route
        })
        .map(|(_, _, _, kernel)| *kernel)
}

async fn capture_claude_profile(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let profile_root = ctx.profile_root()?;
    let global_db = ctx.global_db()?;
    let session_id = required_str(ctx.args, "session_id")?.to_string();
    required_user_db(&ctx.session_authorities)?;
    let roots = registered_project_roots(global_db).await?;
    let stats =
        tracedecay_sessions::runtime::hosts::claude_observation::ingest_user_sessions_with_admission(
            profile_root,
            Some(session_id),
            roots,
            ctx.facade,
            Some(ctx.max_new_bytes.unwrap_or(
                tracedecay_sessions::runtime::hosts::claude_observation::CLAUDE_HOOK_MAX_NEW_BYTES,
            )),
            ctx.cancellation.clone(),
        )
        .await
        .map_err(|error| map_claude_observation_ingest_error(&error))?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted: stats.transcript.messages_upserted,
        claude_observation: Some(stats),
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_codex_profile(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let profile_root = ctx.profile_root()?;
    let global_db = ctx.global_db()?;
    let session_id = required_str(ctx.args, "session_id")?.to_string();
    let roots = registered_project_roots(global_db).await?;
    let outcome =
        tracedecay_sessions::runtime::try_ingest_user_codex_sessions_with_db_and_admission(
            profile_root,
            Some(session_id),
            roots,
            ctx.facade,
            Some(
                ctx.max_new_bytes.unwrap_or(
                    tracedecay_sessions::runtime::hosts::codex::CODEX_HOOK_MAX_NEW_BYTES,
                ),
            ),
        )
        .await
        .map_err(|error| map_transcript_ingest_error(&error))?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted: outcome.stats.messages_upserted,
        source_deferred: outcome.source_deferred,
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_cursor_profile(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    ctx.profile_root()?;
    let global_db = ctx.global_db()?;
    let event_json = required_str(ctx.args, "event_json")?;
    let roots = registered_project_roots(global_db).await?;
    let stats =
        tracedecay_sessions::runtime::hosts::cursor::try_ingest_cursor_user_transcript_event_capped_with_admission(
            event_json,
            ctx.facade,
            ctx.max_new_bytes,
            &roots,
        )
        .await
        .map_err(|error| map_transcript_ingest_error(&error))?;
    Ok(cursor_capture_outcome(stats))
}

async fn capture_hermes_profile(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    ctx.profile_root()?;
    let global_db = ctx.global_db()?;
    let roots = registered_project_roots(global_db).await?;
    let outcome =
        tracedecay_sessions::runtime::hosts::hermes::ingest_user_sessions_capped_with_admission(
            ctx.facade,
            &roots,
            ctx.max_new_bytes,
            ctx.cancellation,
        )
        .await
        .ok_or_else(|| config_error("Hermes transcript source is unavailable"))?;
    hermes_capture_outcome(&outcome)
}

async fn capture_kiro_profile(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let profile_root = ctx.profile_root()?;
    let global_db = ctx.global_db()?;
    let source = tracedecay_sessions::runtime::hosts::kiro::KiroSource::new()
        .ok_or_else(|| config_error("Kiro transcript source is unavailable"))?;
    let roots = registered_project_roots(global_db).await?;
    let source = source.for_user_scope(roots);
    let capture = tracedecay_sessions::runtime::hosts::kiro::capture_kiro_snapshot_observations(
        ctx.facade,
        &source,
        profile_root,
        ObservationScopeV1::Profile,
        ctx.max_new_bytes,
        ctx.cancellation,
    )
    .await
    .map_err(|error| map_transcript_ingest_error(&error))?;
    let messages_upserted = drain_host_observation_projections(
        ctx.facade,
        &ObservationScopeV1::Profile,
        ctx.cancellation,
    )
    .await?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted,
        snapshot: Some(capture),
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_hermes_project(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let project = ctx.project()?;
    let outcome = tracedecay_sessions::runtime::hosts::hermes::ingest_for_project_capped_with_admission_and_cancellation(
        project.project_root(),
        project_observation_id(project)?,
        ctx.facade,
        ctx.max_new_bytes,
        ctx.cancellation,
    )
    .await
    .ok_or_else(|| config_error("Hermes transcript source is unavailable"))?;
    hermes_capture_outcome(&outcome)
}

async fn capture_codex_project(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let cg = ctx.project()?;
    let source = tracedecay_sessions::runtime::hosts::codex::CodexSource::new()
        .ok_or_else(|| config_error("Codex transcript source is unavailable"))?;
    let project_id = project_observation_id(cg)?;
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let admitted = admit_codex_project_rollouts(
        ctx.facade,
        &source,
        cg.project_root(),
        project_id,
        ctx.max_new_bytes,
        ctx.cancellation,
    )
    .await?;
    let messages_upserted =
        drain_host_observation_projections(ctx.facade, &scope, ctx.cancellation).await?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted,
        source_deferred: admitted.deferred,
        observations_committed: admitted.observations_committed,
        exact_duplicate: admitted.exact_duplicate,
        admission_owns_commit: true,
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_cursor_project(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let cg = ctx.project()?;
    let event_json = required_str(ctx.args, "event_json")?;
    let stats = tracedecay_sessions::runtime::hosts::cursor::try_ingest_cursor_transcript_event_capped_with_admission(
        event_json,
        project_observation_id(cg)?,
        ctx.facade,
        ctx.max_new_bytes,
    )
    .await
    .map_err(|error| map_transcript_ingest_error(&error))?;
    Ok(cursor_capture_outcome(stats))
}

fn cursor_capture_outcome(
    stats: tracedecay_sessions::runtime::hosts::cursor::CursorTranscriptIngestStats,
) -> TranscriptCaptureOutcome {
    TranscriptCaptureOutcome {
        messages_upserted: stats.messages_upserted,
        source_deferred: stats.source_deferred,
        observations_committed: stats.observations_committed,
        exact_duplicate: stats.exact_duplicate,
        admission_owns_commit: true,
        ..TranscriptCaptureOutcome::default()
    }
}

/// Hook capture of one Hermes sweep. A skipped `state.db` is a retryable
/// source failure. An incomplete projection drain is deferred work, same as
/// a byte-cap stop.
fn hermes_capture_outcome(outcome: &HermesSweepOutcome) -> Result<TranscriptCaptureOutcome> {
    if outcome.source_failures > 0 {
        return Err(hook_admission_error(
            HostAdmissionStatus::Unavailable,
            "source_scan_partial",
            true,
            format!(
                "Hermes transcript source scan failed for {} source(s)",
                outcome.source_failures
            ),
        ));
    }
    Ok(TranscriptCaptureOutcome {
        messages_upserted: outcome.stats.messages_upserted,
        source_deferred: outcome.deferred_by_byte_cap || outcome.projection_drain_deferred,
        ..TranscriptCaptureOutcome::default()
    })
}

/// Commits one Hermes turn the host inlined in the request.
///
/// The messages are already in hand, so there is no source to scan: each one
/// is admitted through the observation authority a `state.db` sweep row goes
/// through, and the scope's projection drain turns it into the raw LCM rows
/// the temporal refresh the caller joins then projects for retrieval.
async fn capture_hermes_callback(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let session_id = required_str(ctx.args, "session_id")?;
    let messages = ctx
        .args
        .get("messages")
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .ok_or_else(|| config_error("Hermes turn callback requires non-empty messages"))?;
    let (scope, project_root) = if ctx.user_scope {
        (ObservationScopeV1::Profile, None)
    } else {
        let project = ctx.project()?;
        (
            ObservationScopeV1::Project {
                project_id: project_observation_id(project)?,
            },
            Some(project.project_root()),
        )
    };
    let admitted = tracedecay_sessions::runtime::hosts::hermes::capture_turn_callback(
        ctx.facade,
        &scope,
        project_root,
        session_id,
        messages,
        ctx.cancellation,
    )
    .await
    .map_err(|error| map_transcript_ingest_error(&error))?;
    drain_host_observation_projections(ctx.facade, &scope, ctx.cancellation).await?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted: admitted.committed,
        observations_committed: admitted.committed,
        exact_duplicate: admitted.committed == 0 && admitted.duplicates > 0,
        admission_owns_commit: true,
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_kiro_project(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let cg = ctx.project()?;
    let source = tracedecay_sessions::runtime::hosts::kiro::KiroSource::new()
        .ok_or_else(|| config_error("Kiro transcript source is unavailable"))?;
    let project_id = project_observation_id(cg)?;
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let capture = tracedecay_sessions::runtime::hosts::kiro::capture_kiro_snapshot_observations(
        ctx.facade,
        &source,
        cg.project_root(),
        scope.clone(),
        ctx.max_new_bytes,
        ctx.cancellation,
    )
    .await
    .map_err(|error| map_transcript_ingest_error(&error))?;
    let messages_upserted =
        drain_host_observation_projections(ctx.facade, &scope, ctx.cancellation).await?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted,
        snapshot: Some(capture),
        ..TranscriptCaptureOutcome::default()
    })
}

/// Lands the one Pi session a lifecycle event names into the project store.
/// A session file whose header cannot be admitted is a typed partial scan,
/// never an empty success.
async fn capture_pi_project(ctx: TranscriptCaptureContext<'_>) -> Result<TranscriptCaptureOutcome> {
    let cg = ctx.project()?;
    let event: Value = serde_json::from_str(required_str(ctx.args, "event_json")?)
        .map_err(|error| config_error(format!("invalid Pi event: {error}")))?;
    let event_str = |key: &str| {
        event
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| config_error(format!("Pi event omitted `{key}`")))
    };
    let session_id = event_str("session_id")?;
    let cwd = Path::new(event_str("cwd")?);
    let source = tracedecay_sessions::runtime::hosts::pi::PiSource::new()
        .ok_or_else(|| config_error("Pi transcript source is unavailable"))?;
    let scope = ObservationScopeV1::Project {
        project_id: project_observation_id(cg)?,
    };
    let capture = tracedecay_sessions::runtime::hosts::pi::capture_pi_session(
        ctx.facade,
        &source,
        cg.project_root(),
        cwd,
        session_id,
        scope.clone(),
        ctx.max_new_bytes,
        ctx.cancellation,
    )
    .await
    .map_err(|error| map_transcript_ingest_error(&error))?;
    if capture.discovery_failures > 0 {
        return Err(hook_admission_error(
            HostAdmissionStatus::Unavailable,
            "source_discovery_partial",
            true,
            format!(
                "Pi session source was refused for {} file(s)",
                capture.discovery_failures
            ),
        ));
    }
    let messages_upserted =
        drain_host_observation_projections(ctx.facade, &scope, ctx.cancellation).await?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted,
        source_deferred: capture.deferred,
        ..TranscriptCaptureOutcome::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_capture_preserves_deferred_projection() {
        let outcome = cursor_capture_outcome(
            tracedecay_sessions::runtime::hosts::cursor::CursorTranscriptIngestStats {
                messages_upserted: 3,
                source_deferred: true,
                ..Default::default()
            },
        );

        assert_eq!(outcome.messages_upserted, 3);
        assert!(outcome.source_deferred);
    }

    #[test]
    fn hermes_skipped_source_is_a_typed_failure() {
        let mut sweep = HermesSweepOutcome {
            source_failures: 1,
            ..HermesSweepOutcome::default()
        };
        sweep.stats.messages_upserted = 3;

        let error = match hermes_capture_outcome(&sweep) {
            Err(error) => error,
            Ok(_) => panic!("skipped Hermes sources must fail the hook capture"),
        };
        let data = crate::structured_hook_error_data(&error).unwrap();

        assert_eq!(data["status"], "unavailable");
        assert_eq!(data["reason_code"], "source_scan_partial");
        assert_eq!(data["retryable"], true);
    }

    #[test]
    fn hermes_deferred_projection_drain_is_source_deferred() {
        let outcome = match hermes_capture_outcome(&HermesSweepOutcome {
            projection_drain_deferred: true,
            ..HermesSweepOutcome::default()
        }) {
            Ok(outcome) => outcome,
            Err(error) => panic!("deferred drain is not a source failure: {error}"),
        };

        assert!(outcome.source_deferred);
    }
}
