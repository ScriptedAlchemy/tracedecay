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
//! ways: TraceDecay scanning the host's own on-disk sources, or the host
//! inlining a turn's messages in the request. Both are capture routes and both
//! belong in the registry — expressing the second one as a branch above the
//! lookup is what previously let it skip admission entirely.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::Value;
use tracedecay_domain::ObservationScopeV1;

use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;
use tracedecay_agent_hosts::automation::config_error;
use tracedecay_sessions::runtime::claude_observation::ClaudeObservationIngestStats;
use tracedecay_sessions::runtime::snapshot_observation::SnapshotCaptureOutcome;
use tracedecay_usecases::host_admission::{HostAdmissionFacade, HostAdmissionOutcome};
use tracedecay_usecases::observation::ObservationCancellation;
use tracedecay_usecases::session::lcm::{
    LcmAuthorityOutcome, LcmAuthorityPayload, LcmAuthorityRequest, LcmAuthorityResponse,
    LcmTranscriptIngestCommand,
};

use super::super::super::SessionAuthorities;
use super::super::errors::{map_claude_observation_ingest_error, map_transcript_ingest_error};
use super::super::{required_str, required_user_db};
use super::{
    admit_codex_project_rollouts, compaction_unavailable_reason,
    drain_host_observation_projections, project_observation_id,
};

/// Which payload shape a hook ingest request carries.
///
/// Part of the capture-registry key, so an inline-payload route is a first
/// class entry rather than a branch that bypasses the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TranscriptPayloadRouteV1 {
    /// TraceDecay scans the host's own on-disk transcript sources.
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
#[derive(Clone, Copy)]
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
    let registry_authority = crate::store::GlobalDbSessionIngestAuthority::new(global_db);
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
    /// Set by routes that commit through the LCM authority instead of a source
    /// scan; rendered as `authority_outcome` and `committed_state`.
    pub(super) lcm_receipt: Option<LcmAuthorityResponse>,
    /// Set when the route's own authority refused the pass. Replaces the
    /// replay-completion admission so one status vocabulary reaches the host.
    pub(super) route_admission: Option<HostAdmissionOutcome>,
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
    required_user_db(ctx.session_authorities)?;
    let roots = registered_project_roots(global_db).await?;
    let stats =
        tracedecay_sessions::runtime::claude_observation::ingest_user_sessions_with_admission(
            profile_root,
            Some(session_id),
            roots,
            ctx.facade,
            Some(ctx.max_new_bytes.unwrap_or(
                tracedecay_sessions::runtime::claude_observation::CLAUDE_HOOK_MAX_NEW_BYTES,
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
    let stats = tracedecay_sessions::runtime::try_ingest_user_codex_sessions_with_db_and_admission(
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
        tracedecay_sessions::runtime::cursor::try_ingest_cursor_user_transcript_event_capped_with_admission(
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

async fn capture_hermes_profile(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    ctx.profile_root()?;
    let global_db = ctx.global_db()?;
    let roots = registered_project_roots(global_db).await?;
    let outcome = tracedecay_sessions::runtime::hermes::ingest_user_sessions_capped_with_admission(
        ctx.facade,
        &roots,
        ctx.max_new_bytes,
        ctx.cancellation,
    )
    .await;
    Ok(TranscriptCaptureOutcome {
        messages_upserted: outcome.stats.messages_upserted,
        source_deferred: outcome.deferred_by_byte_cap,
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_kiro_profile(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let profile_root = ctx.profile_root()?;
    let global_db = ctx.global_db()?;
    let source = tracedecay_sessions::runtime::kiro::KiroSource::new()
        .ok_or_else(|| config_error("Kiro transcript source is unavailable"))?;
    let roots = registered_project_roots(global_db).await?;
    let source = source.for_user_scope(roots);
    let capture = tracedecay_sessions::runtime::kiro::capture_kiro_snapshot_observations(
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
    let outcome = tracedecay_sessions::runtime::hermes::ingest_for_project_capped_with_admission_and_cancellation(
        project.project_root(),
        project_observation_id(project)?,
        ctx.facade,
        ctx.max_new_bytes,
        ctx.cancellation,
    )
    .await;
    Ok(TranscriptCaptureOutcome {
        messages_upserted: outcome.stats.messages_upserted,
        source_deferred: outcome.deferred_by_byte_cap,
        ..TranscriptCaptureOutcome::default()
    })
}

async fn capture_codex_project(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let cg = ctx.project()?;
    let source = tracedecay_sessions::runtime::codex::CodexSource::new()
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
    let stats = tracedecay_sessions::runtime::cursor::try_ingest_cursor_transcript_event_capped_with_admission(
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

/// Commits one Hermes turn the host inlined in the request.
///
/// The messages are already in hand, so there is no source to scan: the turn
/// goes to the LCM authority and the authority's disposition is reported
/// through the same outcome every scanning kernel uses. `messages_upserted`
/// is the number of turn messages handed to the authority, which is what makes
/// the shared assembly report a committed replay.
async fn capture_hermes_callback(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let session_id = required_str(ctx.args, "session_id")?;
    let messages = ctx
        .args
        .get("messages")
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .ok_or_else(|| config_error("Hermes turn callback requires non-empty messages"))?
        .clone();
    let message_count = u64::try_from(messages.len()).unwrap_or(u64::MAX);
    let authority = if ctx.user_scope {
        ctx.session_authorities.profile_lcm
    } else {
        ctx.session_authorities.project_lcm
    };
    let Some(authority) = authority else {
        return Ok(lcm_authority_unavailable());
    };
    let event_digest = tracedecay_domain::canonical_sha256(&(&"hermes", &session_id, &messages))
        .map_err(|error| config_error(format!("digest Hermes turn failed: {error}")))?;
    let request = LcmAuthorityRequest::Ingest(LcmTranscriptIngestCommand {
        preflight: tracedecay_sessions::runtime::lcm::LcmPreflightRequest {
            provider: "hermes".to_owned(),
            session_id: session_id.to_owned(),
            messages,
            current_tokens: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
        },
        protocol_revision: "hermes.turn-completed.v1".to_owned(),
        event_digest,
    });
    let Some(response) = authority.execute(request).await else {
        return Ok(lcm_authority_unavailable());
    };
    if response.outcome == LcmAuthorityOutcome::Ready
        && matches!(response.payload, Some(LcmAuthorityPayload::Ingest(_)))
    {
        return Ok(TranscriptCaptureOutcome {
            messages_upserted: message_count,
            lcm_receipt: Some(response),
            ..TranscriptCaptureOutcome::default()
        });
    }
    let reason = compaction_unavailable_reason(&response.outcome);
    Ok(TranscriptCaptureOutcome {
        route_admission: Some(HostAdmissionOutcome::retained_unavailable(reason)),
        lcm_receipt: Some(response),
        ..TranscriptCaptureOutcome::default()
    })
}

fn lcm_authority_unavailable() -> TranscriptCaptureOutcome {
    TranscriptCaptureOutcome {
        route_admission: Some(HostAdmissionOutcome::retained_unavailable(
            "lcm_daemon_authority_unavailable",
        )),
        ..TranscriptCaptureOutcome::default()
    }
}

async fn capture_kiro_project(
    ctx: TranscriptCaptureContext<'_>,
) -> Result<TranscriptCaptureOutcome> {
    let cg = ctx.project()?;
    let source = tracedecay_sessions::runtime::kiro::KiroSource::new()
        .ok_or_else(|| config_error("Kiro transcript source is unavailable"))?;
    let project_id = project_observation_id(cg)?;
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let capture = tracedecay_sessions::runtime::kiro::capture_kiro_snapshot_observations(
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
