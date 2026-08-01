use crate::automation::config_error;
use crate::errors::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tracedecay_domain::{ObservationSourceRangeV1, SessionId, UtcMicros};

pub(super) fn hook_now() -> UtcMicros {
    UtcMicros(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |duration| {
                duration.as_micros().min(i64::MAX as u128) as i64
            }),
    )
}

pub(super) fn hook_v2_envelope(
    args: &Value,
    action: &str,
) -> Result<tracedecay_hooks::HookEventEnvelopeV2> {
    let envelope = args
        .get("envelope")
        .cloned()
        .ok_or_else(|| config_error(format!("{action} requires envelope")))
        .and_then(|value| {
            serde_json::from_value::<tracedecay_hooks::HookEventEnvelopeV2>(value)
                .map_err(|error| config_error(format!("invalid Hook V2 envelope: {error}")))
        })?;
    Ok(envelope)
}

pub(super) fn hook_v2_native_session_id(
    args: &Value,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> Option<SessionId> {
    let session = SessionId::new(args.get("native_session_id")?.as_str()?.to_owned()).ok()?;
    (crate::hooks::hook_v2_protected_session_id_for_native(session.as_str())
        == envelope.protected_session_id)
        .then_some(session)
}

pub(super) fn hook_v2_requires_producer_work(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> bool {
    matches!(
        &envelope.event,
        tracedecay_hooks::HookEventV2::SavedEdit { .. }
            | tracedecay_hooks::HookEventV2::SessionBoundary {
                boundary: tracedecay_hooks::HookBoundaryV1::End
                    | tracedecay_hooks::HookBoundaryV1::TurnComplete
            }
    )
}

pub(super) fn hook_v2_lifecycle_range(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    receipt: tracedecay_hooks::HookAdmissionLedgerReceiptV1,
) -> Option<ObservationSourceRangeV1> {
    let start = match envelope.ordering {
        tracedecay_hooks::HookOrderingV1::ProviderSequence(sequence) if sequence > 0 => sequence,
        tracedecay_hooks::HookOrderingV1::ProviderSequence(_) => return None,
        tracedecay_hooks::HookOrderingV1::Unknown => receipt.order.checked_add(1)?,
    };
    ObservationSourceRangeV1::new(start, start.checked_add(1)?).ok()
}

fn daemon_mint_hook_v2_id(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    domain: &[u8],
    native_id: [u8; 16],
) -> [u8; 16] {
    let producer = envelope.producer.hook_key().as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.hook-v2.daemon-id.v1");
    hasher.update(envelope.binding_token);
    hasher.update(envelope.project_id);
    hasher.update(envelope.repository_id);
    hasher.update(envelope.worktree_id);
    hasher.update(envelope.worktree_epoch.to_le_bytes());
    hasher.update(envelope.protected_session_id);
    hasher.update((producer.len() as u64).to_le_bytes());
    hasher.update(producer);
    hasher.update((domain.len() as u64).to_le_bytes());
    hasher.update(domain);
    hasher.update(native_id);
    let digest = hasher.finalize();
    let mut canonical_id = [0; 16];
    canonical_id.copy_from_slice(&digest[..16]);
    canonical_id
}

fn hook_v2_event_identity_domain(event: &tracedecay_hooks::HookEventV2) -> &'static [u8] {
    use tracedecay_hooks::{HookBoundaryV1, HookEventV2, HookLifecyclePhaseV1};

    match event {
        HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::Start,
        } => b"event.session.start",
        HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::End,
        } => b"event.session.end",
        HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::TurnComplete,
        } => b"event.session.turn-complete",
        HookEventV2::PromptBoundary => b"event.prompt",
        HookEventV2::ToolLifecycle {
            phase: HookLifecyclePhaseV1::Started,
            ..
        } => b"event.tool.started",
        HookEventV2::ToolLifecycle {
            phase: HookLifecyclePhaseV1::Completed,
            ..
        } => b"event.tool.completed",
        HookEventV2::ToolLifecycle {
            phase: HookLifecyclePhaseV1::Failed,
            ..
        } => b"event.tool.failed",
        HookEventV2::ToolLifecycle {
            phase: HookLifecyclePhaseV1::Cancelled,
            ..
        } => b"event.tool.cancelled",
        HookEventV2::SavedEdit { .. } => b"event.saved-edit",
        HookEventV2::TestLifecycle {
            phase: HookLifecyclePhaseV1::Started,
            ..
        } => b"event.test.started",
        HookEventV2::TestLifecycle {
            phase: HookLifecyclePhaseV1::Completed,
            ..
        } => b"event.test.completed",
        HookEventV2::TestLifecycle {
            phase: HookLifecyclePhaseV1::Failed,
            ..
        } => b"event.test.failed",
        HookEventV2::TestLifecycle {
            phase: HookLifecyclePhaseV1::Cancelled,
            ..
        } => b"event.test.cancelled",
    }
}

/// Mint canonical daemon-owned identities only after binding validation.
/// Hook-provided fixed-size values are typed native identity material, never
/// canonical ledger or orchestration identities.
pub(super) fn daemon_mint_hook_v2_envelope(
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
) -> tracedecay_hooks::HookEventEnvelopeV2 {
    let mut canonical = envelope.clone();
    canonical.event_id = daemon_mint_hook_v2_id(
        envelope,
        hook_v2_event_identity_domain(&envelope.event),
        envelope.event_id,
    );
    canonical.event = match &envelope.event {
        tracedecay_hooks::HookEventV2::SessionBoundary { boundary } => {
            tracedecay_hooks::HookEventV2::SessionBoundary {
                boundary: *boundary,
            }
        }
        tracedecay_hooks::HookEventV2::PromptBoundary => {
            tracedecay_hooks::HookEventV2::PromptBoundary
        }
        tracedecay_hooks::HookEventV2::ToolLifecycle {
            tool_id,
            phase,
            effect_receipt_id,
        } => tracedecay_hooks::HookEventV2::ToolLifecycle {
            tool_id: daemon_mint_hook_v2_id(envelope, b"tool", *tool_id),
            phase: *phase,
            effect_receipt_id: effect_receipt_id
                .map(|id| daemon_mint_hook_v2_id(envelope, b"effect-receipt", id)),
        },
        tracedecay_hooks::HookEventV2::SavedEdit {
            file_id,
            changed_range_count,
        } => tracedecay_hooks::HookEventV2::SavedEdit {
            file_id: daemon_mint_hook_v2_id(envelope, b"file", *file_id),
            changed_range_count: *changed_range_count,
        },
        tracedecay_hooks::HookEventV2::TestLifecycle {
            test_run_id,
            test_count,
            phase,
            receipt_id,
        } => tracedecay_hooks::HookEventV2::TestLifecycle {
            test_run_id: daemon_mint_hook_v2_id(envelope, b"test-run", *test_run_id),
            test_count: *test_count,
            phase: *phase,
            receipt_id: receipt_id.map(|id| daemon_mint_hook_v2_id(envelope, b"test-receipt", id)),
        },
    };
    canonical
}

/// Short, stable label for the dashboard's activity payload. Kept here rather
/// than on the domain type: it is a display detail of the live tap, not part of
/// the hook contract.
pub(super) const fn hook_v2_family_label(
    family: tracedecay_hooks::HookEventFamily,
) -> &'static str {
    match family {
        tracedecay_hooks::HookEventFamily::SessionBoundary => "session_boundary",
        tracedecay_hooks::HookEventFamily::PromptBoundary => "prompt_boundary",
        tracedecay_hooks::HookEventFamily::ToolLifecycle => "tool_lifecycle",
        tracedecay_hooks::HookEventFamily::SavedEdit => "saved_edit",
        tracedecay_hooks::HookEventFamily::TestLifecycle => "test_lifecycle",
    }
}

#[cfg(test)]
mod tests;
