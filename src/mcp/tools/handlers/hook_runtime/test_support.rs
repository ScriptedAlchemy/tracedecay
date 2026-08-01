use serde_json::{Value, json};
use tracedecay_domain::UtcMicros;

pub(super) fn admission_test_envelope(
    event_id: u8,
    epoch: u64,
) -> tracedecay_hooks::HookEventEnvelopeV2 {
    tracedecay_hooks::HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [event_id; 16],
        producer: tracedecay_hooks::HookHostV1::ClaudeCode,
        protected_session_id: [5; 32],
        project_id: [1; 16],
        repository_id: [2; 16],
        worktree_id: [3; 16],
        worktree_epoch: epoch,
        binding_token: [4; 32],
        ordering: tracedecay_hooks::HookOrderingV1::Unknown,
        observed_at: UtcMicros(11),
        event: tracedecay_hooks::HookEventV2::SessionBoundary {
            boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
        },
    }
}

pub(super) fn admission_test_binding(epoch: u64) -> tracedecay_hooks::HookScopeBindingV1 {
    let host = tracedecay_hooks::HookHostV1::ClaudeCode;
    tracedecay_hooks::HookScopeBindingV1 {
        host,
        project_id: [1; 16],
        repository_id: [2; 16],
        worktree_id: [3; 16],
        worktree_epoch: epoch,
        binding_token: [4; 32],
        capabilities: [
            tracedecay_hooks::HookEventFamily::SessionBoundary,
            tracedecay_hooks::HookEventFamily::PromptBoundary,
            tracedecay_hooks::HookEventFamily::ToolLifecycle,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            tracedecay_hooks::HookEventFamily::TestLifecycle,
        ]
        .into_iter()
        .map(|family| tracedecay_hooks::HookCapabilityV1 {
            family,
            support: tracedecay_hooks::stock_event_support(host, family),
        })
        .collect(),
    }
}

pub(super) fn retained_claim(
    id: u8,
) -> crate::agents::context_scout_v2::ContextScoutDurableClaimV1 {
    use crate::agents::context_scout_v2::{
        ContextScoutAddressV1, ContextScoutCandidateV1, ContextScoutCategoryV1,
        ContextScoutDeliveryWindowV1, ContextScoutDurableClaimV1, ContextScoutDurableQueueEntryV1,
        ContextScoutEvidenceBindingV1, ContextScoutEvidenceGenerationV1, ContextScoutLeaseV1,
        ContextScoutModelRunOutcomeV1, ContextScoutRouteV1, ContextScoutSuggestionEnvelopeV1,
        ContextScoutWorkV1,
    };

    let address = ContextScoutAddressV1 {
        profile_id: [1; 16],
        provider_id: [2; 16],
        protected_session_id: [3; 32],
        thread_id: [4; 16],
        turn_id: [5; 16],
        agent_id: [6; 16],
        logical_message_id: [id; 16],
        project_id: [201; 16],
    };
    let input_watermark = [7; 32];
    let envelope = ContextScoutSuggestionEnvelopeV1 {
        envelope_id: [id; 16],
        address,
        input_watermark,
        configuration_revision: [8; 32],
        delivery_window: ContextScoutDeliveryWindowV1::Immediate,
        candidate: ContextScoutCandidateV1 {
            dedupe_key: [id; 32],
            category: ContextScoutCategoryV1::Diagnostic,
            relevance_score: 1,
            suggestion_text: "bounded".to_owned(),
            evidence: vec![ContextScoutEvidenceBindingV1 {
                anchor_id: [9; 16],
                content_identity: [10; 32],
                generation: ContextScoutEvidenceGenerationV1::SavedContent,
            }],
            expires_at: UtcMicros(2_000),
        },
    };
    ContextScoutDurableClaimV1 {
        entry: ContextScoutDurableQueueEntryV1 {
            work: ContextScoutWorkV1 {
                address,
                generation: 1,
                input_watermark,
            },
            route: ContextScoutRouteV1::Deterministic,
            model_outcome: ContextScoutModelRunOutcomeV1::NotRequested,
            model_receipt: None,
            envelope,
        },
        lease: ContextScoutLeaseV1 {
            lease_id: [id; 16],
            expires_at: UtcMicros(1_000),
        },
    }
}

pub(super) fn hook_v2_snapshot() -> tracedecay_hooks::HookConfigurationSnapshotV1 {
    tracedecay_hooks::HookConfigurationSnapshotV1 {
        schema_version: tracedecay_hooks::HOOK_CONFIGURATION_SCHEMA_VERSION,
        revision: 1,
        published_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        binding: tracedecay_hooks::HookScopeBindingV1 {
            host: tracedecay_hooks::HookHostV1::ClaudeCode,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 4,
            binding_token: [5; 32],
            capabilities: vec![tracedecay_hooks::HookCapabilityV1 {
                family: tracedecay_hooks::HookEventFamily::SessionBoundary,
                support: tracedecay_hooks::HookEventSupportV1::Native,
            }],
        },
    }
}

pub(super) fn hook_v2_envelope_for_test() -> tracedecay_hooks::HookEventEnvelopeV2 {
    tracedecay_hooks::HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [6; 16],
        producer: tracedecay_hooks::HookHostV1::ClaudeCode,
        protected_session_id: [7; 32],
        project_id: [1; 16],
        repository_id: [2; 16],
        worktree_id: [3; 16],
        worktree_epoch: 4,
        binding_token: [5; 32],
        ordering: tracedecay_hooks::HookOrderingV1::Unknown,
        observed_at: UtcMicros(2),
        event: tracedecay_hooks::HookEventV2::SessionBoundary {
            boundary: tracedecay_hooks::HookBoundaryV1::Start,
        },
    }
}

pub(super) fn hermes_turn_completed_event(session_id: &str, watermark: &str) -> Value {
    json!({
        "agent": "hermes",
        "event": "turnCompleted",
        "route": { "session_id": session_id },
        "receipt": {
            "status": "success",
            "transcript_watermark": watermark
        }
    })
}

pub(super) fn valid_hermes_terminal_receipt_payload(session_id: &str, watermark: &str) -> Vec<u8> {
    let plan = crate::mcp::hook_events::HookEventPlan::RecordTerminalReceipt {
        route: Some(crate::daemon::HookRouteMetadata {
            session_id: Some(session_id.to_string()),
            thread_id: None,
            cwd: None,
            worktree: None,
            branch: None,
        }),
        receipt: crate::daemon::HookTerminalReceipt {
            tool_call_id: None,
            turn_id: None,
            status: Some("success".to_string()),
            duration_ms: Some(1),
            transcript_watermark: Some(watermark.to_string()),
        },
    };
    crate::mcp::hook_events::encode_durable_hook_event_plan(&plan).unwrap()
}
