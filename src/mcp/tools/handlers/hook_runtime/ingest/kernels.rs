//! Provider capture kernels for `ingest_transcript`.
//!
//! Every supported `(provider, user_scope)` route resolves to one kernel that
//! owns all of that provider's capture logic and reports its result through the
//! shared [`TranscriptCaptureOutcome`]. The dispatch site therefore carries no
//! per-provider control flow: it looks the kernel up in
//! [`transcript_capture_kernel`], awaits it, and assembles the response from
//! whichever optional fields the outcome carries.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::Value;
use tracedecay_domain::ObservationScopeV1;

use crate::application::host_admission::HostAdmissionFacade;
use crate::application::observation::ObservationCancellation;
use crate::automation::config_error;
use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::sessions::claude_observation::ClaudeObservationIngestStats;
use crate::sessions::snapshot_observation::SnapshotCaptureOutcome;
use crate::tracedecay::TraceDecay;

use super::super::super::SessionAuthorities;
use super::super::errors::{map_claude_observation_ingest_error, map_transcript_ingest_error};
use super::super::{required_str, required_user_db};
use super::{
    admit_codex_project_rollouts, drain_host_observation_projections, project_observation_id,
};

/// Everything a capture kernel may borrow for one ingest pass.
#[derive(Clone, Copy)]
pub(super) struct TranscriptCaptureContext<'a> {
    pub(super) cg: Option<&'a TraceDecay>,
    pub(super) args: &'a Value,
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
    let registry_authority = crate::store::GlobalDbSessionIngestAuthority::new(global_db);
    crate::sessions::registered_project_roots_from(&registry_authority)
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
    KiroProfileKernelV1 => capture_kiro_profile,
    CodexProjectKernelV1 => capture_codex_project,
    CursorProjectKernelV1 => capture_cursor_project,
    KiroProjectKernelV1 => capture_kiro_project,
}

/// The `(provider, user_scope)` capture registry.
const TRANSCRIPT_CAPTURE_KERNELS: &[(&str, bool, &dyn TranscriptCaptureKernelV1)] = &[
    ("claude", true, &ClaudeProfileKernelV1),
    ("codex", true, &CodexProfileKernelV1),
    ("cursor", true, &CursorProfileKernelV1),
    ("kiro", true, &KiroProfileKernelV1),
    ("codex", false, &CodexProjectKernelV1),
    ("cursor", false, &CursorProjectKernelV1),
    ("kiro", false, &KiroProjectKernelV1),
];

/// Resolves the capture kernel registered for one transcript route, if any.
pub(super) fn transcript_capture_kernel(
    provider: &str,
    user_scope: bool,
) -> Option<&'static dyn TranscriptCaptureKernelV1> {
    TRANSCRIPT_CAPTURE_KERNELS
        .iter()
        .find(|(id, scope, _)| *id == provider && *scope == user_scope)
        .map(|(_, _, kernel)| *kernel)
}

async fn capture_claude_profile(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let profile_root = ctx.profile_root()?;
    let global_db = ctx.global_db()?;
    let session_id = required_str(ctx.args, "session_id")?.to_string();
    required_user_db(ctx.session_authorities)?;
    let roots = registered_project_roots(global_db).await?;
    let stats = crate::sessions::claude_observation::ingest_user_sessions_with_admission(
        profile_root,
        Some(session_id),
        roots,
        ctx.facade,
        Some(
            ctx.max_new_bytes
                .unwrap_or(crate::sessions::claude_observation::CLAUDE_HOOK_MAX_NEW_BYTES),
        ),
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
    let stats = crate::sessions::try_ingest_user_codex_sessions_with_db_and_admission(
        profile_root,
        Some(session_id),
        roots,
        ctx.facade,
    )
    .await
    .map_err(|error| map_transcript_ingest_error(&error))?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted: stats.messages_upserted,
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
        crate::sessions::cursor::try_ingest_cursor_user_transcript_event_capped_with_admission(
            event_json,
            ctx.facade,
            ctx.max_new_bytes,
            &roots,
        )
        .await
        .map_err(|error| map_transcript_ingest_error(&error))?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted: stats.messages_upserted,
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_kiro_profile(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let profile_root = ctx.profile_root()?;
    let global_db = ctx.global_db()?;
    let source = crate::sessions::kiro::KiroSource::new()
        .ok_or_else(|| config_error("Kiro transcript source is unavailable"))?;
    let roots = registered_project_roots(global_db).await?;
    let source = source.for_user_scope(roots);
    let capture = crate::sessions::kiro::capture_kiro_snapshot_observations(
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

async fn capture_codex_project(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let cg = ctx.project()?;
    let source = crate::sessions::codex::CodexSource::new()
        .ok_or_else(|| config_error("Codex transcript source is unavailable"))?;
    let project_id = project_observation_id(cg)?;
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let source_deferred = admit_codex_project_rollouts(
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
        source_deferred,
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_cursor_project(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let cg = ctx.project()?;
    let event_json = required_str(ctx.args, "event_json")?;
    let stats = crate::sessions::cursor::try_ingest_cursor_transcript_event_capped_with_admission(
        event_json,
        project_observation_id(cg)?,
        ctx.facade,
        ctx.max_new_bytes,
    )
    .await
    .map_err(|error| map_transcript_ingest_error(&error))?;
    Ok(TranscriptCaptureOutcome {
        messages_upserted: stats.messages_upserted,
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_kiro_project(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let cg = ctx.project()?;
    let source = crate::sessions::kiro::KiroSource::new()
        .ok_or_else(|| config_error("Kiro transcript source is unavailable"))?;
    let project_id = project_observation_id(cg)?;
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let capture = crate::sessions::kiro::capture_kiro_snapshot_observations(
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
