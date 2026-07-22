#![allow(dead_code)] // in-flight feature APIs not yet wired; see clippy sweep
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracedecay_domain::UtcMicros;
use tracedecay_hooks::{
    AsyncHookAdmissionPortV1, HookAdmissionFutureV1, HookConfigurationFileReaderV1,
    HookConfigurationReadOutcomeV1, HookConfigurationSubscriberV1, HookEventEnvelopeV2,
    HookGuidanceStateV1, HookHostV1, HookImmediateAdmissionV1, HookReadyGuidanceV1,
    HookRuntimeControlV1, HookScopeBindingV1, HookSpoolConfigV1, HookSpoolError, HookSpoolV1,
    HookSynchronousDeadlineV1, HookTransportDispositionV1, NativeEnvelopeMaterialV1,
    NativeHookDecodeError, SpoolAppendOutcomeV1, admit_async_exact_scope,
    decode_bound_native_hook_event, finish_synchronous_hook,
};

pub(crate) enum HookV2Dispatch {
    NotApplicable,
    Handled {
        guidance: Option<String>,
        disposition: HookTransportDispositionV1,
    },
}

pub(crate) fn project_id_for_layout(layout: &crate::storage::StoreLayout) -> Option<[u8; 16]> {
    layout
        .identity
        .project_id
        .as_deref()
        .map(|project_id| domain_hash16(project_id, "project"))
}

pub(crate) fn publish_daemon_bindings(
    layout: &crate::storage::StoreLayout,
) -> crate::errors::Result<()> {
    const HOSTS: &[HookHostV1] = &[
        HookHostV1::ClaudeCode,
        HookHostV1::Codex,
        HookHostV1::CursorDesktop,
        HookHostV1::Hermes,
        HookHostV1::Kiro,
        HookHostV1::KimiCode,
        HookHostV1::OpenCode,
    ];
    let project_key = layout.identity.project_id.as_deref().ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: "cannot publish Hook V2 binding without typed project identity".to_owned(),
        }
    })?;
    let project_id =
        project_id_for_layout(layout).ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: "cannot derive Hook V2 typed project identity".to_owned(),
        })?;
    let now = now_utc();
    for host in HOSTS {
        let capabilities = [
            tracedecay_hooks::HookEventFamily::SessionBoundary,
            tracedecay_hooks::HookEventFamily::PromptBoundary,
            tracedecay_hooks::HookEventFamily::ToolLifecycle,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            tracedecay_hooks::HookEventFamily::TestLifecycle,
        ]
        .into_iter()
        .map(|family| tracedecay_hooks::HookCapabilityV1 {
            family,
            support: tracedecay_hooks::stock_event_support(*host, family),
        })
        .collect();
        let snapshot = tracedecay_hooks::HookConfigurationSnapshotV1 {
            schema_version: tracedecay_hooks::HOOK_CONFIGURATION_SCHEMA_VERSION,
            revision: now.0.max(1) as u64,
            published_at: now,
            expires_at: UtcMicros(now.0.saturating_add(24 * 60 * 60 * 1_000_000)),
            binding: HookScopeBindingV1 {
                host: *host,
                project_id,
                repository_id: domain_hash16(project_key, "repository"),
                worktree_id: domain_hash16(project_key, "worktree"),
                worktree_epoch: 1,
                binding_token: domain_hash32(project_key, host.as_key()),
                capabilities,
            },
        };
        let writer = tracedecay_hooks::HookConfigurationFileWriterV1::new(
            tracedecay_hooks::hook_configuration_path(&layout.data_root, *host),
        );
        tracedecay_hooks::HookConfigurationPublisherV1::new(writer)
            .publish(snapshot)
            .map_err(|error| crate::errors::TraceDecayError::Config {
                message: format!(
                    "failed to publish {} Hook V2 binding: {error}",
                    host.as_key()
                ),
            })?;
    }
    Ok(())
}

#[derive(Default, Deserialize)]
struct NativeIdentityFields {
    session_id: Option<String>,
    conversation_id: Option<String>,
    generation_id: Option<String>,
    prompt_id: Option<String>,
    turn_id: Option<String>,
    tool_use_id: Option<String>,
    tool_call_id: Option<String>,
    call_id: Option<String>,
    file_path: Option<String>,
    tool_name: Option<String>,
    edits: Option<Vec<serde_json::Value>>,
}

struct DaemonAdmissionPort<'a> {
    project_root: &'a Path,
}

impl AsyncHookAdmissionPortV1 for DaemonAdmissionPort<'_> {
    fn try_admit_async<'a>(
        &'a self,
        envelope: &'a HookEventEnvelopeV2,
        _deadline: HookSynchronousDeadlineV1,
    ) -> HookAdmissionFutureV1<'a> {
        Box::pin(async move {
            let Ok(envelope) = serde_json::to_value(envelope) else {
                return HookImmediateAdmissionV1::Unavailable;
            };
            let Ok(response) = super::daemon_hook_action(
                Some(self.project_root),
                serde_json::json!({
                    "action": "hook_v2_admit",
                    "envelope": envelope,
                }),
                None,
            )
            .await
            else {
                return HookImmediateAdmissionV1::Unavailable;
            };
            match response.get("status").and_then(serde_json::Value::as_str) {
                Some("accepted" | "committed" | "exact_duplicate") => {
                    let ready_guidance =
                        response.get("ready_guidance").cloned().and_then(|value| {
                            serde_json::from_value::<HookReadyGuidanceV1>(value).ok()
                        });
                    HookImmediateAdmissionV1::Accepted {
                        admitted_at: now_utc(),
                        ready_guidance,
                    }
                }
                Some("backpressured") => HookImmediateAdmissionV1::Backpressured,
                _ => HookImmediateAdmissionV1::Unavailable,
            }
        })
    }
}

pub(crate) async fn dispatch(
    host: HookHostV1,
    event_json: &str,
    project_root: &Path,
) -> HookV2Dispatch {
    let decoded = match tracedecay_hooks::decode_native_hook_event(host, event_json.as_bytes()) {
        Ok(decoded) => decoded,
        Err(
            NativeHookDecodeError::UnsupportedNativeEvent
            | NativeHookDecodeError::UnsupportedNativeFamily,
        ) => {
            return HookV2Dispatch::NotApplicable;
        }
        Err(_) => {
            return HookV2Dispatch::Handled {
                guidance: None,
                disposition: HookTransportDispositionV1::CatchupRequired,
            };
        }
    };
    dispatch_decoded(host, event_json, project_root, decoded).await
}

pub(crate) async fn dispatch_opencode_tool_after(
    event_json: &str,
    project_root: &Path,
) -> HookV2Dispatch {
    let decoded = match tracedecay_hooks::decode_opencode_plugin_event(
        tracedecay_hooks::OpenCodePluginSurfaceV1::ToolExecuteAfter,
        event_json.as_bytes(),
    ) {
        Ok(decoded) => decoded,
        Err(
            NativeHookDecodeError::UnsupportedNativeEvent
            | NativeHookDecodeError::UnsupportedNativeFamily,
        ) => {
            return HookV2Dispatch::NotApplicable;
        }
        Err(_) => return unavailable(),
    };
    dispatch_decoded(HookHostV1::OpenCode, event_json, project_root, decoded).await
}

pub(crate) async fn lookup_ready_guidance(
    envelope: &HookEventEnvelopeV2,
    project_root: &Path,
) -> Option<HookReadyGuidanceV1> {
    let response = super::daemon_hook_action(
        Some(project_root),
        serde_json::json!({
            "action": "hook_v2_guidance_lookup",
            "envelope": envelope,
        }),
        None,
    )
    .await
    .ok()?;
    response
        .get("ready_guidance")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

async fn dispatch_decoded(
    host: HookHostV1,
    event_json: &str,
    project_root: &Path,
    decoded: tracedecay_hooks::DecodedNativeHookEventV1,
) -> HookV2Dispatch {
    let Ok(layout) = crate::storage::resolve_layout_for_current_profile(project_root) else {
        return unavailable();
    };
    let config_path = tracedecay_hooks::hook_configuration_path(&layout.data_root, host);
    let subscriber =
        HookConfigurationSubscriberV1::new(HookConfigurationFileReaderV1::new(config_path));
    let now = now_utc();
    let HookConfigurationReadOutcomeV1::Bound(snapshot) = subscriber.load_current(host, now) else {
        return unavailable();
    };
    let binding = &snapshot.binding;
    let material = native_material(event_json, decoded.family(), now);
    let Ok(envelope) =
        decode_bound_native_hook_event(host, event_json.as_bytes(), binding, material)
    else {
        return unavailable();
    };

    let started = Instant::now();
    let port = DaemonAdmissionPort { project_root };
    let immediate = match tokio::time::timeout(
        Duration::from_millis(25),
        admit_async_exact_scope(
            &envelope,
            binding,
            HookSynchronousDeadlineV1::start(),
            &port,
        ),
    )
    .await
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => HookImmediateAdmissionV1::Unavailable,
        Err(_) => HookImmediateAdmissionV1::TimedOut,
    };
    let replay = if matches!(immediate, HookImmediateAdmissionV1::Accepted { .. }) {
        None
    } else {
        Some(append_for_replay(
            &layout.data_root,
            host,
            &envelope,
            binding,
            now,
        ))
    };
    let control = HookRuntimeControlV1::from_configuration(&snapshot, HookGuidanceStateV1::Active);
    match finish_synchronous_hook(
        &envelope,
        binding,
        control,
        immediate,
        replay,
        now_utc(),
        started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
    ) {
        Ok(result) => HookV2Dispatch::Handled {
            guidance: result.rendered_guidance,
            disposition: result.receipt.disposition,
        },
        Err(_) => unavailable(),
    }
}

fn append_for_replay(
    data_root: &Path,
    host: HookHostV1,
    envelope: &HookEventEnvelopeV2,
    binding: &HookScopeBindingV1,
    now: UtcMicros,
) -> SpoolAppendOutcomeV1 {
    let root = data_root.join("hook-v2-spool").join(host.as_key());
    if std::fs::create_dir_all(&root).is_err() {
        return SpoolAppendOutcomeV1::Unavailable;
    }
    let Ok((mut spool, _)) = HookSpoolV1::open(root, HookSpoolConfigV1::stock(host), now) else {
        return SpoolAppendOutcomeV1::Unavailable;
    };
    match spool.append(envelope.clone(), binding, now) {
        Ok(_) => SpoolAppendOutcomeV1::Accepted,
        Err(HookSpoolError::SpoolFull) => SpoolAppendOutcomeV1::Full,
        Err(_) => SpoolAppendOutcomeV1::Unavailable,
    }
}

fn native_material(
    event_json: &str,
    family: tracedecay_hooks::HookEventFamily,
    observed_at: UtcMicros,
) -> NativeEnvelopeMaterialV1 {
    let fields = serde_json::from_str::<NativeIdentityFields>(event_json).unwrap_or_default();
    let session = fields
        .session_id
        .as_deref()
        .or(fields.conversation_id.as_deref())
        .unwrap_or("unknown-session");
    let event_key = fields
        .tool_use_id
        .as_deref()
        .or(fields.tool_call_id.as_deref())
        .or(fields.call_id.as_deref())
        .or(fields.generation_id.as_deref())
        .or(fields.prompt_id.as_deref())
        .or(fields.turn_id.as_deref())
        .unwrap_or(event_json);
    let file = fields.file_path.as_deref().unwrap_or(event_key);
    let tool = fields.tool_name.as_deref().unwrap_or(event_key);
    NativeEnvelopeMaterialV1 {
        event_id: hash16(event_key.as_bytes()),
        protected_session_id: hash32(session.as_bytes()),
        observed_at,
        tool_id: (family == tracedecay_hooks::HookEventFamily::ToolLifecycle)
            .then(|| hash16(tool.as_bytes())),
        effect_receipt_id: fields
            .tool_use_id
            .as_deref()
            .or(fields.tool_call_id.as_deref())
            .or(fields.call_id.as_deref())
            .map(|value| hash16(value.as_bytes())),
        file_id: (family == tracedecay_hooks::HookEventFamily::SavedEdit)
            .then(|| hash16(file.as_bytes())),
        changed_range_count: fields.edits.map_or(1, |edits| edits.len().min(64) as u8),
    }
}

fn hash16(bytes: &[u8]) -> [u8; 16] {
    let digest = Sha256::digest(bytes);
    let mut output = [0; 16];
    output.copy_from_slice(&digest[..16]);
    output
}

fn hash32(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn domain_hash16(value: &str, domain: &str) -> [u8; 16] {
    hash16(format!("{domain}:{value}").as_bytes())
}

fn domain_hash32(value: &str, domain: &str) -> [u8; 32] {
    hash32(format!("{domain}:{value}").as_bytes())
}

fn now_utc() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |duration| {
            duration.as_micros().min(i64::MAX as u128) as i64
        });
    UtcMicros(micros.max(1))
}

fn unavailable() -> HookV2Dispatch {
    HookV2Dispatch::Handled {
        guidance: None,
        disposition: HookTransportDispositionV1::CatchupRequired,
    }
}
