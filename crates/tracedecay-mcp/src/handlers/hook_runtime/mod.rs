use serde_json::Value;
use std::path::Path;
use tracedecay_automation_runtime::automation::config_error;
use tracedecay_contracts::retrieval::{HookRuntimeResultV1, HookRuntimeSurfaceRequestV1};
use tracedecay_domain::errors::Result;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_host_admission::SharedHostAdmissionBroker;
use tracedecay_project::project::TraceDecay;
use tracedecay_sessions::admission::HostAdmissionOutcome;

use crate::handlers::SessionAuthorities;

mod admission;
mod context_scout;
mod envelope;
mod hermes;
mod ingest;

#[cfg(test)]
mod entry_tests;
#[cfg(test)]
mod test_support;

pub use admission::{
    HookV2AdmissionLedgerUnavailable, HookV2AdmissionOutcomeV1, admit_hook_v2_envelope,
    admit_hook_v2_replayed_envelope_with_lifecycle, hook_v2_pending_work_envelopes,
};
pub use envelope::daemon_mint_hook_v2_file_id;
pub use hermes::replay_projectless_hermes_host_admission;

use crate::map_host_admission_outcome;
use admission::{hook_v2_admit, hook_v2_profile_admit};
use context_scout::{hook_v2_delivery_receipt, hook_v2_feedback_notice_delivery};
use hermes::{hermes_receipt, user_review_unavailable};
use ingest::{claude_compact, codex_compact, cursor_compact, ingest_transcript};

const TOOL_NAME: &str = "tracedecay_hook_runtime";

/// Decodes a `tracedecay_hook_runtime` call against its typed request.
/// `session_id` is a payload field here, so only presentation keys are
/// removed first.
pub fn decode_hook_runtime_request(args: &Value) -> Result<HookRuntimeSurfaceRequestV1> {
    let mut request = args.clone();
    if let Some(object) = request.as_object_mut() {
        for key in ["format", "__mcp_request_id", "_meta"] {
            object.remove(key);
        }
    }
    serde_json::from_value(request)
        .map_err(|error| config_error(format!("invalid arguments for {TOOL_NAME}: {error}")))
}

fn required_field<'a>(value: Option<&'a str>, key: &str) -> Result<&'a str> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error(format!("missing required parameter `{key}`")))
}

fn requires_projectless_routing(action: &str) -> tracedecay_domain::errors::TraceDecayError {
    config_error(format!(
        "hook action `{action}` requires projectless daemon routing"
    ))
}

/// Runs one hook-runtime action for the served project.
#[tracing::instrument(name = "mcp.hook_runtime.total", level = "trace", skip_all)]
pub async fn compute_hook_runtime(
    cg: &TraceDecay,
    request: HookRuntimeSurfaceRequestV1,
    profile_root: Option<&Path>,
    global_db: Option<&RegisteredGlobalDb>,
    session_authorities: SessionAuthorities<'_>,
) -> Result<HookRuntimeResultV1> {
    use HookRuntimeSurfaceRequestV1 as Request;
    Ok(match request {
        Request::ResetCounter {} => {
            cg.reset_local_counter().await?;
            HookRuntimeResultV1::ResetCounter { reset: true }
        }
        Request::HookV2Admit(request) => {
            HookRuntimeResultV1::HookV2Admit(hook_v2_admit(cg, request, session_authorities).await?)
        }
        Request::HookV2DeliveryReceipt { receipt } => HookRuntimeResultV1::HookV2DeliveryReceipt {
            status: hook_v2_delivery_receipt(cg, receipt).await?,
        },
        Request::HookV2FeedbackNoticeDelivery {
            envelope,
            feedback_notice,
        } => HookRuntimeResultV1::HookV2FeedbackNoticeDelivery(hook_v2_feedback_notice_delivery(
            cg,
            envelope,
            feedback_notice,
        )?),
        Request::IngestTranscript(request) => {
            if request.user_scope {
                return Err(config_error(
                    "user transcript ingest requires projectless daemon routing",
                ));
            }
            // Boxed: transcript ingest composes the deepest session-runtime
            // future in the handler tree; inlining it into the dispatch frame
            // overflows the perf-profile worker stack.
            HookRuntimeResultV1::IngestTranscript(Box::new(
                Box::pin(ingest_transcript(
                    Some(cg),
                    &request,
                    profile_root,
                    global_db,
                    session_authorities,
                ))
                .await?,
            ))
        }
        Request::CodexCompact { event_json } => HookRuntimeResultV1::CodexCompact(
            codex_compact(cg, &event_json, session_authorities).await?,
        ),
        Request::ClaudeCompact {
            event_json,
            user_scope,
        } => {
            if user_scope {
                return Err(requires_projectless_routing("claude_compact"));
            }
            HookRuntimeResultV1::ClaudeCompact(claude_compact(&event_json)?)
        }
        Request::CursorCompact { event_json } => HookRuntimeResultV1::CursorCompact(
            cursor_compact(&event_json, session_authorities).await?,
        ),
        Request::UserReview { .. } => return Err(requires_projectless_routing("user_review")),
        Request::HermesReceipt { .. } => {
            return Err(requires_projectless_routing("hermes_receipt"));
        }
        Request::HookV2ProfileAdmit { .. } => {
            return Err(requires_projectless_routing("hook_v2_profile_admit"));
        }
    })
}

/// Runs one hook-runtime action that has no project route: it lands in the
/// authenticated profile's stores.
#[tracing::instrument(name = "mcp.hook_runtime.projectless", level = "trace", skip_all)]
pub async fn compute_projectless_hook_runtime(
    request: HookRuntimeSurfaceRequestV1,
    profile_root: &Path,
    global_db: &RegisteredGlobalDb,
    session_authorities: SessionAuthorities<'_>,
    host_admission_broker: std::result::Result<&SharedHostAdmissionBroker, HostAdmissionOutcome>,
) -> Result<HookRuntimeResultV1> {
    use HookRuntimeSurfaceRequestV1 as Request;
    Ok(match request {
        // Projectless (user-scope) ingest has no project session store to
        // correlate hint outcomes against; the settlement runs on
        // project-scope ingests only.
        Request::IngestTranscript(request) if request.user_scope => {
            HookRuntimeResultV1::IngestTranscript(Box::new(
                ingest_transcript(
                    None,
                    &request,
                    Some(profile_root),
                    Some(global_db),
                    session_authorities,
                )
                .await?,
            ))
        }
        Request::UserReview { .. } => return Err(user_review_unavailable()),
        Request::HermesReceipt { event } => {
            let host_admission_broker =
                host_admission_broker.map_err(|outcome| map_host_admission_outcome(&outcome))?;
            HookRuntimeResultV1::HermesReceipt {
                status: hermes_receipt(
                    event,
                    profile_root,
                    required_user_db(&session_authorities)?,
                    host_admission_broker,
                )
                .await?,
            }
        }
        Request::HookV2ProfileAdmit { admission } => {
            HookRuntimeResultV1::HookV2ProfileAdmit(hook_v2_profile_admit(
                admission,
                profile_root,
                session_authorities
                    .profile_identity
                    .as_deref()
                    .ok_or_else(|| {
                        config_error(
                            "authenticated profile identity is unavailable for Hook V2 admission",
                        )
                    })?,
            )?)
        }
        Request::ClaudeCompact {
            event_json,
            user_scope: true,
        } => HookRuntimeResultV1::ClaudeCompact(claude_compact(&event_json)?),
        Request::ResetCounter {}
        | Request::HookV2Admit(_)
        | Request::HookV2DeliveryReceipt { .. }
        | Request::HookV2FeedbackNoticeDelivery { .. }
        | Request::IngestTranscript(_)
        | Request::CodexCompact { .. }
        | Request::ClaudeCompact { .. }
        | Request::CursorCompact { .. } => {
            return Err(config_error(
                "projectless hook runtime action requires a project route",
            ));
        }
    })
}

fn required_project_db<'a>(authorities: &SessionAuthorities<'a>) -> Result<&'a RegisteredGlobalDb> {
    authorities
        .project
        .map(AsRef::as_ref)
        .ok_or_else(|| config_error("daemon project session database is unavailable"))
}

fn required_user_db<'a>(authorities: &SessionAuthorities<'a>) -> Result<&'a RegisteredGlobalDb> {
    authorities
        .user
        .map(AsRef::as_ref)
        .ok_or_else(|| config_error("daemon user session database is unavailable"))
}
