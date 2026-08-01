//! Host-neutral Hook V2 transport contracts.
//!
//! This crate deliberately contains no database, query, policy, model, Git, or
//! host-configuration authority. A hook decodes a native event into the closed
//! metadata-only envelope below, validates a daemon-issued binding, attempts
//! delivery, optionally spools the exact validated envelope, and renders only
//! daemon-approved guidance.

#![forbid(unsafe_code)]

pub mod admission_ledger;
pub mod config;
pub mod core_events;
pub mod native;
pub mod runtime;
pub mod spool;

pub use admission_ledger::{
    HookAdmissionDecisionV1, HookAdmissionLedgerError, HookAdmissionLedgerLimitsV1,
    HookAdmissionLedgerOpenReportV1, HookAdmissionLedgerReceiptV1, HookAdmissionLedgerV1,
    hook_admission_digest,
};
pub use config::{
    HOOK_CONFIGURATION_SCHEMA_VERSION, HookConfigurationFileReaderV1,
    HookConfigurationFileWriterV1, HookConfigurationPublicationError,
    HookConfigurationPublicationOutcomeV1, HookConfigurationPublicationStoreV1,
    HookConfigurationPublisherV1, HookConfigurationReadOutcomeV1, HookConfigurationReadStoreV1,
    HookConfigurationSnapshotV1, HookConfigurationSubscriberV1, MAX_HOOK_CONFIGURATION_BYTES,
    hook_configuration_path,
};
pub use core_events::{
    DaemonHookEvent, HOOK_EVENT_METHOD, HookAgent, HookEventNotifyOutcomeV1, HookRouteMetadata,
    HookTerminalReceipt,
};
pub use native::{
    DecodedNativeHookEventV1, DecodedOpenCodeLspEventV1, NativeEnvelopeMaterialV1,
    NativeHookDecodeError, NativeHookSignalV1, OpenCodePluginSurfaceV1,
    decode_bound_native_hook_event, decode_native_hook_event, decode_opencode_lsp_event,
    decode_opencode_plugin_event,
};
pub use runtime::{
    AsyncHookAdmissionPortV1, AsyncHookFeedbackDeliveryPortV1, HOOK_SYNCHRONOUS_BUDGET_MICROS,
    HookAdmissionFutureV1, HookAdmissionReceiptV1, HookDeliveryFutureV1,
    HookFeedbackDeliveryOutcomeV1, HookFeedbackDeliveryPortV1, HookFeedbackDeliveryRouteV1,
    HookFeedbackDeliveryV1, HookFeedbackRollbackSwitchV1, HookGuidanceDispositionV1,
    HookGuidanceStateV1, HookImmediateAdmissionStateV1, HookImmediateAdmissionV1,
    HookReadyGuidanceV1, HookRuntimeControlV1, HookRuntimeErrorV1, HookRuntimeStatusV1,
    HookScopedFeedbackV1, HookSynchronousDeadlineV1, HookSynchronousResultV1,
    admit_async_exact_scope, deliver_feedback_with_rollback, deliver_feedback_with_rollback_async,
    deliver_hook_feedback, finish_synchronous_hook,
};
pub use spool::{
    HookReplayBatchV1, HookSpoolAckDispositionV1, HookSpoolAckV1, HookSpoolConfigV1,
    HookSpoolError, HookSpoolLimitsV1, HookSpoolOpenReportV1, HookSpoolRecordV1, HookSpoolV1,
    HookSpoolWriterLeaseV1, hook_spool_checksum,
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

/// Event families that a host hook itself may emit in PR13.
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

/// Checked-in PR13 host matrix. `Unavailable` is truthful absence and never
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
        (HookHostV1::Codex, SessionBoundary) => Native,
        (HookHostV1::Codex, SavedEdit | TestLifecycle) => ReceiptDerived,
        (HookHostV1::Codex, PromptBoundary | ToolLifecycle) => Unavailable,
        (HookHostV1::CursorDesktop, SessionBoundary | SavedEdit) => Native,
        (HookHostV1::CursorCloud, SessionBoundary) => Native,
        (HookHostV1::CursorDesktop | HookHostV1::CursorCloud, TestLifecycle) => ReceiptDerived,
        (HookHostV1::CursorDesktop, PromptBoundary | ToolLifecycle) => Unavailable,
        (HookHostV1::CursorCloud, PromptBoundary | ToolLifecycle | SavedEdit) => Unavailable,
        (HookHostV1::Hermes, SessionBoundary | ToolLifecycle) => Native,
        (HookHostV1::Hermes, SavedEdit | TestLifecycle) => ReceiptDerived,
        (HookHostV1::Hermes, PromptBoundary) => Unavailable,
        (HookHostV1::Kiro, PromptBoundary) => Native,
        (HookHostV1::Kiro, SessionBoundary | ToolLifecycle | SavedEdit | TestLifecycle) => {
            Unavailable
        }
        (HookHostV1::KimiCode, SessionBoundary | ToolLifecycle | SavedEdit) => Native,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizationEraHookEventEnvelopeV2 {
    schema_version: u16,
    event_id: [u8; 16],
    producer: HookHostV1,
    protected_session_id: [u8; 32],
    project_id: [u8; 16],
    repository_id: [u8; 16],
    worktree_id: [u8; 16],
    worktree_epoch: u64,
    authorization_epoch: u64,
    capability_revision: u32,
    binding_token: [u8; 32],
    ordering: HookOrderingV1,
    observed_at: UtcMicros,
    event: AuthorizationEraHookEventV2,
    payload_digest: [u8; 32],
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AuthorizationEraHookEventV2 {
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
        content_digest: [u8; 32],
        changed_range_count: u8,
    },
    TestLifecycle {
        test_run_id: [u8; 16],
        test_count: u8,
        phase: HookLifecyclePhaseV1,
        receipt_id: Option<[u8; 16]>,
    },
}

impl AuthorizationEraHookEventV2 {
    fn into_current(self) -> HookEventV2 {
        match self {
            Self::SessionBoundary { boundary } => HookEventV2::SessionBoundary { boundary },
            Self::PromptBoundary => HookEventV2::PromptBoundary,
            Self::ToolLifecycle {
                tool_id,
                phase,
                effect_receipt_id,
            } => HookEventV2::ToolLifecycle {
                tool_id,
                phase,
                effect_receipt_id,
            },
            Self::SavedEdit {
                file_id,
                content_digest,
                changed_range_count,
            } => {
                let _ = content_digest;
                HookEventV2::SavedEdit {
                    file_id,
                    changed_range_count,
                }
            }
            Self::TestLifecycle {
                test_run_id,
                test_count,
                phase,
                receipt_id,
            } => HookEventV2::TestLifecycle {
                test_run_id,
                test_count,
                phase,
                receipt_id,
            },
        }
    }
}

pub fn decode_hook_event_envelope_compat(
    bytes: &[u8],
) -> Result<HookEventEnvelopeV2, HookContractError> {
    if let Ok(envelope) = serde_json::from_slice(bytes) {
        return Ok(envelope);
    }
    let legacy: AuthorizationEraHookEventEnvelopeV2 =
        serde_json::from_slice(bytes).map_err(|_| HookContractError::MalformedEnvelope)?;
    if legacy.schema_version != HOOK_EVENT_SCHEMA_VERSION {
        return Err(HookContractError::UnsupportedSchemaVersion);
    }
    let _retired_authority = (
        legacy.authorization_epoch,
        legacy.capability_revision,
        legacy.payload_digest,
    );
    Ok(HookEventEnvelopeV2 {
        schema_version: legacy.schema_version,
        event_id: legacy.event_id,
        producer: legacy.producer,
        protected_session_id: legacy.protected_session_id,
        project_id: legacy.project_id,
        repository_id: legacy.repository_id,
        worktree_id: legacy.worktree_id,
        worktree_epoch: legacy.worktree_epoch,
        binding_token: legacy.binding_token,
        ordering: legacy.ordering,
        observed_at: legacy.observed_at,
        event: legacy.event.into_current(),
    })
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
    fn five_host_matrix_does_not_emulate_kiro_tool_or_edit_events() {
        assert_eq!(
            stock_event_support(HookHostV1::Kiro, HookEventFamily::ToolLifecycle),
            HookEventSupportV1::Unavailable
        );
        assert_eq!(
            stock_event_support(HookHostV1::Kiro, HookEventFamily::SavedEdit),
            HookEventSupportV1::Unavailable
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
            HookEventSupportV1::Unavailable,
            "Codex has no checked-in authentic PostToolUse capture"
        );
        assert_eq!(
            stock_event_support(HookHostV1::CursorDesktop, HookEventFamily::SavedEdit),
            HookEventSupportV1::Native,
            "the checked-in Cursor afterFileEdit capture proves this native family"
        );
    }

    #[test]
    fn retained_authorization_era_envelope_migrates_to_current_wire() {
        let envelope = decode_hook_event_envelope_compat(include_bytes!(
            "../fixtures/envelopes/authorization-era-saved-edit.json"
        ))
        .expect("retained Hook V2 envelope should migrate");

        assert_eq!(envelope.schema_version, HOOK_EVENT_SCHEMA_VERSION);
        assert_eq!(envelope.event_id, [9; 16]);
        assert_eq!(envelope.repository_id, [2; 16]);
        assert_eq!(envelope.worktree_id, [3; 16]);
        assert_eq!(envelope.worktree_epoch, 4);
        assert_eq!(
            envelope.event,
            HookEventV2::SavedEdit {
                file_id: [11; 16],
                changed_range_count: 1,
            }
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
