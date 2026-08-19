//! Host-neutral Hook V2 transport contracts.
//!
//! This crate deliberately contains no database, query, policy, model, Git, or
//! host-configuration authority. A hook decodes a native event into the closed
//! metadata-only envelope below, validates a daemon-issued binding, attempts
//! delivery, optionally spools the exact validated envelope, and renders only
//! daemon-approved guidance.

#![forbid(unsafe_code)]

pub mod admission_ledger;
pub mod capture;
pub mod config;
pub mod core_events;
pub mod delivery_spool;
pub mod native;
pub mod runtime;
pub mod spool;

pub use admission_ledger::{
    HookAdmissionDecisionV1, HookAdmissionLedgerLimitsV1, HookAdmissionLedgerReceiptV1,
    HookAdmissionLedgerV1,
};
pub use capture::{
    NativeHookCaptureOutcomeV1, NativeHookCaptureSourceV1, capture_native_event_for_replay,
};
pub use config::{
    HOOK_CONFIGURATION_SCHEMA_VERSION, HookConfigurationFileReaderV1,
    HookConfigurationFileWriterV1, HookConfigurationPublisherV1, HookConfigurationReadOutcomeV1,
    HookConfigurationSnapshotV1, HookConfigurationSubscriberV1, hook_configuration_path,
};
pub use core_events::{
    DaemonHookEvent, HOOK_EVENT_METHOD, HookAgent, HookEventNotifyOutcomeV1, HookRouteMetadata,
    HookTerminalReceipt,
};
pub use delivery_spool::{
    HookDeliveryReceiptSpoolV1, HookDeliverySourceReceiptV1, hook_delivery_receipt_spool_root,
};
pub use native::{
    DecodedNativeHookEventV1, NativeEnvelopeMaterialV1, NativeHookDecodeError,
    OpenCodePluginSurfaceV1, ProfileScopedNativeHookAdmissionV1, decode_bound_native_hook_event,
    decode_native_hook_event, decode_opencode_lsp_event, decode_opencode_plugin_event,
};
pub use runtime::{
    AsyncHookAdmissionPortV1, AsyncHookFeedbackDeliveryPortV1, HookAdmissionFutureV1,
    HookAdmissionReceiptV1, HookDeliveryFutureV1, HookFeedbackDeliveryOutcomeV1,
    HookFeedbackDeliveryPortV1, HookFeedbackDeliveryRouteV1, HookFeedbackDeliveryV1,
    HookFeedbackRollbackSwitchV1, HookGuidanceDispositionV1, HookGuidanceStateV1,
    HookImmediateAdmissionStateV1, HookImmediateAdmissionV1, HookReadyGuidanceV1,
    HookRuntimeControlV1, HookRuntimeErrorV1, HookScopedFeedbackV1, HookSynchronousDeadlineV1,
    admit_async_exact_scope, deliver_feedback_with_rollback, deliver_hook_feedback,
    finish_synchronous_hook,
};
pub use spool::{
    HookSpoolAckDispositionV1, HookSpoolAckV1, HookSpoolConfigV1, HookSpoolError,
    HookSpoolRecordV1, HookSpoolV1,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{NativeHostIdentityV1, UtcMicros};

pub const HOOK_EVENT_SCHEMA_VERSION: u16 = 2;
pub const MAX_HOOK_PAYLOAD_BYTES: usize = 16 * 1024;
pub const MAX_SPOOL_RECORDS_PER_HOST: u32 = 4_096;
pub const MAX_SPOOL_BYTES_PER_HOST: u64 = 32 * 1024 * 1024;
pub const MAX_SPOOL_RECORDS_PER_SESSION: u32 = 1_024;
pub const MAX_SPOOL_BYTES_PER_SESSION: u64 = 8 * 1024 * 1024;
pub const MAX_SPOOL_AGE_MICROS: i64 = 24 * 60 * 60 * 1_000_000;
pub const MAX_REPLAY_BATCH_RECORDS: u16 = 64;
pub const MAX_REPLAY_BATCH_BYTES: u32 = 256 * 1024;
pub const MAX_SUGGESTION_BYTES: usize = 4 * 1024;

/// Canonical native host identity used by hook decoding, configuration, and
/// persisted spool state. The alias preserves the Hook V2 API name while
/// preventing a second host vocabulary from drifting from the domain catalog.
pub type HookHostV1 = NativeHostIdentityV1;

/// Event families that a host hook itself may emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventFamily {
    SessionBoundary,
    PromptBoundary,
    ToolLifecycle,
    SavedEdit,
    TestLifecycle,
}

/// Native provenance is mandatory. Daemon-derived families are described by
/// conformance data but cannot be emitted by a hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventSupportV1 {
    Native,
    ReceiptDerived,
    DaemonDerived,
    Unavailable,
    Prohibited,
}

/// Checked-in native host matrix. `Unavailable` is truthful absence and never
/// permission to infer an event from command text or another host surface.
pub const fn stock_event_support(host: HookHostV1, family: HookEventFamily) -> HookEventSupportV1 {
    use HookEventFamily::{
        PromptBoundary, SavedEdit, SessionBoundary, TestLifecycle, ToolLifecycle,
    };
    use HookEventSupportV1::{Native, ReceiptDerived, Unavailable};

    match (host, family) {
        (HookHostV1::ClaudeCode, SessionBoundary | ToolLifecycle) => Native,
        (HookHostV1::ClaudeCode, SavedEdit | TestLifecycle) => ReceiptDerived,
        (HookHostV1::ClaudeCode, PromptBoundary) => Unavailable,
        (HookHostV1::Codex, SessionBoundary | ToolLifecycle) => Native,
        (HookHostV1::Codex, SavedEdit | TestLifecycle) => ReceiptDerived,
        (HookHostV1::Codex, PromptBoundary) => Unavailable,
        (HookHostV1::CursorDesktop, SessionBoundary | SavedEdit) => Native,
        (HookHostV1::CursorDesktop, TestLifecycle) => ReceiptDerived,
        (HookHostV1::CursorDesktop, PromptBoundary | ToolLifecycle) => Unavailable,
        (
            HookHostV1::CursorCloud,
            SessionBoundary | PromptBoundary | ToolLifecycle | SavedEdit | TestLifecycle,
        ) => Unavailable,
        (HookHostV1::Hermes, SessionBoundary | ToolLifecycle) => Native,
        (HookHostV1::Hermes, SavedEdit | TestLifecycle) => ReceiptDerived,
        (HookHostV1::Hermes, PromptBoundary) => Unavailable,
        (HookHostV1::Kiro, PromptBoundary) => Native,
        (HookHostV1::Kiro, SessionBoundary | ToolLifecycle | SavedEdit | TestLifecycle) => {
            Unavailable
        }
        (HookHostV1::KimiCode, ToolLifecycle | SavedEdit) => Native,
        (HookHostV1::KimiCode, SessionBoundary) => Native,
        (HookHostV1::KimiCode, PromptBoundary | TestLifecycle) => Unavailable,
        (HookHostV1::OpenCode, SessionBoundary | ToolLifecycle | SavedEdit) => Native,
        (HookHostV1::OpenCode, PromptBoundary | TestLifecycle) => Unavailable,
        (
            HookHostV1::Cline | HookHostV1::RooCode | HookHostV1::Kilo,
            SessionBoundary | PromptBoundary | ToolLifecycle | SavedEdit | TestLifecycle,
        ) => Unavailable,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookOrderingV1 {
    ProviderSequence(u64),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookBoundaryV1 {
    Start,
    End,
    TurnComplete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookLifecyclePhaseV1 {
    Started,
    Completed,
    Failed,
    Cancelled,
}

/// Closed, content-free event body. It cannot represent prompts, commands,
/// arguments, output, logs, source text, paths, credentials, or reasoning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HookEventV2 {
    SessionBoundary {
        boundary: HookBoundaryV1,
    },
    PromptBoundary,
    ToolLifecycle {
        tool_id: [u8; 16],
        phase: HookLifecyclePhaseV1,
        effect_receipt_id: Option<[u8; 16]>,
    },
    SavedEdit {
        file_id: [u8; 16],
        changed_range_count: u8,
    },
    TestLifecycle {
        test_run_id: [u8; 16],
        test_count: u8,
        phase: HookLifecyclePhaseV1,
        receipt_id: Option<[u8; 16]>,
    },
}

impl HookEventV2 {
    pub const fn family(&self) -> HookEventFamily {
        match self {
            Self::SessionBoundary { .. } => HookEventFamily::SessionBoundary,
            Self::PromptBoundary => HookEventFamily::PromptBoundary,
            Self::ToolLifecycle { .. } => HookEventFamily::ToolLifecycle,
            Self::SavedEdit { .. } => HookEventFamily::SavedEdit,
            Self::TestLifecycle { .. } => HookEventFamily::TestLifecycle,
        }
    }

    fn validate(&self) -> Result<(), HookContractError> {
        match self {
            Self::SavedEdit {
                changed_range_count,
                ..
            } if *changed_range_count > 64 => Err(HookContractError::EventBudgetExceeded),
            Self::TestLifecycle { test_count, .. } if *test_count > 128 => {
                Err(HookContractError::EventBudgetExceeded)
            }
            _ => Ok(()),
        }
    }
}

/// Opaque identities are fixed-size so a hook cannot accidentally serialize
/// a path, provider credential, prompt, or other unbounded host value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookEventEnvelopeV2 {
    pub schema_version: u16,
    pub event_id: [u8; 16],
    pub producer: HookHostV1,
    pub protected_session_id: [u8; 32],
    pub project_id: [u8; 16],
    pub repository_id: [u8; 16],
    pub worktree_id: [u8; 16],
    pub worktree_epoch: u64,
    pub binding_token: [u8; 32],
    pub ordering: HookOrderingV1,
    pub observed_at: UtcMicros,
    pub event: HookEventV2,
}

impl HookEventEnvelopeV2 {
    pub fn validate(&self, binding: &HookScopeBindingV1) -> Result<(), HookContractError> {
        if self.schema_version != HOOK_EVENT_SCHEMA_VERSION {
            return Err(HookContractError::UnsupportedSchemaVersion);
        }
        if self.event_id == [0; 16]
            || self.protected_session_id == [0; 32]
            || self.project_id == [0; 16]
            || self.repository_id == [0; 16]
            || self.worktree_id == [0; 16]
            || self.binding_token == [0; 32]
        {
            return Err(HookContractError::InvalidIdentity);
        }
        if self.project_id != binding.project_id
            || self.producer != binding.host
            || self.repository_id != binding.repository_id
            || self.worktree_id != binding.worktree_id
            || self.worktree_epoch != binding.worktree_epoch
            || self.binding_token != binding.binding_token
        {
            return Err(HookContractError::BindingMismatch);
        }
        self.event.validate()?;
        let support = binding.support_for(self.event.family())?;
        if !matches!(
            support,
            HookEventSupportV1::Native | HookEventSupportV1::ReceiptDerived
        ) {
            return Err(HookContractError::UnsupportedFamily);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookCapabilityV1 {
    pub family: HookEventFamily,
    pub support: HookEventSupportV1,
}

/// The daemon-issued, exact-scope binding. No path participates in identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookScopeBindingV1 {
    pub host: HookHostV1,
    pub project_id: [u8; 16],
    pub repository_id: [u8; 16],
    pub worktree_id: [u8; 16],
    pub worktree_epoch: u64,
    pub binding_token: [u8; 32],
    pub capabilities: Vec<HookCapabilityV1>,
}

impl HookScopeBindingV1 {
    pub fn validate(&self) -> Result<(), HookContractError> {
        if self.project_id == [0; 16]
            || self.repository_id == [0; 16]
            || self.worktree_id == [0; 16]
            || self.binding_token == [0; 32]
            || self.capabilities.is_empty()
            || self.capabilities.len() > 5
        {
            return Err(HookContractError::InvalidBinding);
        }
        for (index, capability) in self.capabilities.iter().enumerate() {
            if self.capabilities[..index]
                .iter()
                .any(|existing| existing.family == capability.family)
                || capability.support != stock_event_support(self.host, capability.family)
            {
                return Err(HookContractError::InvalidBinding);
            }
        }
        Ok(())
    }

    fn support_for(
        &self,
        family: HookEventFamily,
    ) -> Result<HookEventSupportV1, HookContractError> {
        self.validate()?;
        self.capabilities
            .iter()
            .find(|capability| capability.family == family)
            .map(|capability| capability.support)
            .ok_or(HookContractError::UnsupportedFamily)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpoolAppendOutcomeV1 {
    Accepted,
    Full,
    ResetRequired,
    Unavailable,
}

/// Acceptance stages are intentionally distinct. Neither variant claims that
/// projection or any application effect completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookTransportDispositionV1 {
    Accepted,
    AcceptedForReplay,
    CatchupRequired,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HookContractError {
    #[error("hook schema version is unsupported")]
    UnsupportedSchemaVersion,
    #[error("hook envelope is malformed")]
    MalformedEnvelope,
    #[error("event family is unsupported by the native host binding")]
    UnsupportedFamily,
    #[error("hook envelope identity is invalid")]
    InvalidIdentity,
    #[error("daemon-issued hook binding is invalid")]
    InvalidBinding,
    #[error("event does not match the exact daemon-issued binding")]
    BindingMismatch,
    #[error("event exceeds its structural budget")]
    EventBudgetExceeded,
    #[error("spool record exceeds its byte budget")]
    SpoolBudgetExceeded,
    #[error("spool quota is full")]
    SpoolFull,
    #[error("spool entry is expired")]
    SpoolExpired,
    #[error("spool checksum does not verify against the exact encoded envelope")]
    SpoolChecksumInvalid,
    #[error("replay batch exceeds its bound")]
    ReplayBatchExceeded,
    #[error("guidance is not approved for render")]
    GuidanceNotApproved,
    #[error("approved guidance exceeds its byte budget")]
    GuidanceBudgetExceeded,
}

pub fn validate_replay_batch(record_count: u16, byte_count: u32) -> Result<(), HookContractError> {
    if record_count == 0
        || record_count > MAX_REPLAY_BATCH_RECORDS
        || byte_count == 0
        || byte_count > MAX_REPLAY_BATCH_BYTES
    {
        return Err(HookContractError::ReplayBatchExceeded);
    }
    Ok(())
}

/// Render only sensitivity-safe text already approved by the application.
pub fn render_approved_guidance(approved: bool, text: &str) -> Result<String, HookContractError> {
    if !approved {
        return Err(HookContractError::GuidanceNotApproved);
    }
    if text.trim().is_empty()
        || text.len() > MAX_SUGGESTION_BYTES
        || text
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(HookContractError::GuidanceBudgetExceeded);
    }
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> HookScopeBindingV1 {
        HookScopeBindingV1 {
            host: HookHostV1::CursorDesktop,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 4,
            binding_token: [7; 32],
            capabilities: vec![HookCapabilityV1 {
                family: HookEventFamily::SessionBoundary,
                support: HookEventSupportV1::Native,
            }],
        }
    }

    fn envelope() -> HookEventEnvelopeV2 {
        HookEventEnvelopeV2 {
            schema_version: HOOK_EVENT_SCHEMA_VERSION,
            event_id: [8; 16],
            producer: HookHostV1::CursorDesktop,
            protected_session_id: [9; 32],
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 4,
            binding_token: [7; 32],
            ordering: HookOrderingV1::Unknown,
            observed_at: UtcMicros(10),
            event: HookEventV2::SessionBoundary {
                boundary: HookBoundaryV1::Start,
            },
        }
    }

    #[test]
    fn envelope_rejects_scope_or_epoch_rebinding() {
        let mut stale = envelope();
        stale.worktree_epoch += 1;
        assert_eq!(
            stale.validate(&binding()).unwrap_err(),
            HookContractError::BindingMismatch
        );
    }

    #[test]
    fn capability_binding_cannot_override_checked_in_host_matrix() {
        let mut binding = binding();
        binding.capabilities[0].support = HookEventSupportV1::DaemonDerived;
        assert_eq!(
            envelope().validate(&binding).unwrap_err(),
            HookContractError::InvalidBinding
        );
    }

    #[test]
    fn host_matrix_matches_checked_in_native_capture_authority() {
        assert_eq!(
            stock_event_support(HookHostV1::Kiro, HookEventFamily::ToolLifecycle),
            HookEventSupportV1::Unavailable
        );
        assert_eq!(
            stock_event_support(HookHostV1::Kiro, HookEventFamily::SavedEdit),
            HookEventSupportV1::Unavailable
        );
        assert_eq!(
            stock_event_support(HookHostV1::Kiro, HookEventFamily::PromptBoundary),
            HookEventSupportV1::Native,
            "the checked-in Kiro userPromptSubmit capture proves this native family"
        );
        assert_eq!(
            stock_event_support(HookHostV1::KimiCode, HookEventFamily::SessionBoundary),
            HookEventSupportV1::Native,
            "the checked-in Kimi Stop capture proves this native family"
        );
        assert_eq!(
            stock_event_support(HookHostV1::CursorCloud, HookEventFamily::SessionBoundary),
            HookEventSupportV1::Unavailable,
            "Cursor Desktop captures cannot prove a Cursor Cloud callback"
        );
        assert_eq!(
            stock_event_support(HookHostV1::Hermes, HookEventFamily::TestLifecycle),
            HookEventSupportV1::ReceiptDerived
        );
        assert_eq!(
            stock_event_support(HookHostV1::ClaudeCode, HookEventFamily::ToolLifecycle),
            HookEventSupportV1::Native,
            "the checked-in Claude PostToolUse capture proves this native family"
        );
        assert_eq!(
            stock_event_support(HookHostV1::Hermes, HookEventFamily::ToolLifecycle),
            HookEventSupportV1::Native,
            "the checked-in Hermes post_tool_call capture proves this native family"
        );
        assert_eq!(
            stock_event_support(HookHostV1::Codex, HookEventFamily::ToolLifecycle),
            HookEventSupportV1::Native,
            "the checked-in Codex PostToolUse capture proves this native family"
        );
        assert_eq!(
            stock_event_support(HookHostV1::CursorDesktop, HookEventFamily::SavedEdit),
            HookEventSupportV1::Native,
            "the checked-in Cursor afterFileEdit capture proves this native family"
        );
    }

    #[test]
    fn closed_hook_wire_rejects_unknown_event_and_capability_fields() {
        let mut wire = serde_json::to_value(envelope()).unwrap();
        wire["event"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<HookEventEnvelopeV2>(wire).is_err());

        let mut capability = serde_json::to_value(HookCapabilityV1 {
            family: HookEventFamily::SessionBoundary,
            support: HookEventSupportV1::Native,
        })
        .unwrap();
        capability["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<HookCapabilityV1>(capability).is_err());
    }

    #[test]
    fn unapproved_or_oversized_guidance_cannot_render() {
        assert_eq!(
            render_approved_guidance(false, "secret").unwrap_err(),
            HookContractError::GuidanceNotApproved
        );
        assert_eq!(
            render_approved_guidance(true, &"x".repeat(MAX_SUGGESTION_BYTES + 1)).unwrap_err(),
            HookContractError::GuidanceBudgetExceeded
        );
    }
}
