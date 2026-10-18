//! Minimal native hook capture into the bounded replay spool.
//!
//! This path reads only a daemon-published binding and writes only the
//! content-free transport spool. It has no daemon, database, query, model,
//! session, memory, sync, or indexing authority.

use std::path::Path;

use tracedecay_domain::UtcMicros;

use crate::{
    HOOK_SYNCHRONOUS_BUDGET, HookConfigurationFileReaderV1, HookConfigurationReadOutcomeV1,
    HookConfigurationSubscriberV1, HookEventEnvelopeV2, HookScopeBindingV1, HookSpoolConfigV1,
    HookSpoolError, HookSpoolResetReasonV1, HookSpoolV1, NativeContextScoutLifecycleV1,
    NativeEnvelopeMaterialV1, NativeHookDecodeError, OpenCodePluginSurfaceV1,
    decode_native_hook_event, decode_opencode_plugin_event, hook_configuration_path,
    hook_v2_spool_root,
};
use tracedecay_domain::NativeHostIdentityV1;

/// The real host surface that supplied native hook bytes.
///
/// OpenCode's direct tool callback has a distinct checked-in wire shape even
/// though it produces the same host-neutral envelope as its event-bus route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeHookCaptureSourceV1 {
    Host(NativeHostIdentityV1),
    OpenCodeToolExecuteAfter,
}

impl NativeHookCaptureSourceV1 {
    pub const fn host(self) -> NativeHostIdentityV1 {
        match self {
            Self::Host(host) => host,
            Self::OpenCodeToolExecuteAfter => NativeHostIdentityV1::OpenCode,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeHookCaptureOutcomeV1 {
    Captured,
    Unsupported,
    Unbound,
    Rejected,
    Full,
    ResetRequired(HookSpoolResetReasonV1),
    /// The spool refused the event for a cause other than capacity, reset, or
    /// the writer admission deadline.
    Unavailable(HookSpoolError),
    /// The callback could not resolve the project scope it captures for (its
    /// working directory, enrolled layout, worktree identity, or the clock).
    ScopeUnavailable,
    AdmissionTimedOut,
}

impl std::fmt::Display for NativeHookCaptureOutcomeV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ResetRequired(reason) => write!(formatter, "ResetRequired ({reason})"),
            Self::Unavailable(cause) => write!(formatter, "Unavailable({cause:?}: {cause})"),
            outcome => write!(formatter, "{outcome:?}"),
        }
    }
}

/// Single entry point for every native hook payload a host captures. This is
/// the coarse boundary that matters for hook latency: it decodes, binds, and
/// spools one event, so its cost and outcome mix stand in for the whole
/// capture path without measuring the decode/bind/spool internals separately.
#[tracing::instrument(name = "hooks.capture.native_event", level = "trace", skip_all)]
pub fn capture_native_event_for_replay(
    data_root: &Path,
    worktree_id: [u8; 16],
    source: NativeHookCaptureSourceV1,
    payload: &[u8],
    material: NativeEnvelopeMaterialV1,
    now: UtcMicros,
) -> NativeHookCaptureOutcomeV1 {
    let host = source.host();
    let decoded_result = match source {
        NativeHookCaptureSourceV1::Host(host) => decode_native_hook_event(host, payload),
        NativeHookCaptureSourceV1::OpenCodeToolExecuteAfter => {
            decode_opencode_plugin_event(OpenCodePluginSurfaceV1::ToolExecuteAfter, payload)
        }
    };
    let decoded = match decoded_result {
        Ok(decoded) => decoded,
        Err(
            NativeHookDecodeError::UnsupportedNativeEvent
            | NativeHookDecodeError::UnsupportedNativeFamily,
        ) => return NativeHookCaptureOutcomeV1::Unsupported,
        Err(_) => return NativeHookCaptureOutcomeV1::Rejected,
    };
    let subscriber = HookConfigurationSubscriberV1::new(HookConfigurationFileReaderV1::new(
        hook_configuration_path(data_root, worktree_id, host),
    ));
    let HookConfigurationReadOutcomeV1::Bound(snapshot) = subscriber.load_current(host, now) else {
        return NativeHookCaptureOutcomeV1::Unbound;
    };
    let envelope = match decoded.into_envelope(&snapshot.binding, material) {
        Ok(envelope) => envelope,
        Err(_) => return NativeHookCaptureOutcomeV1::Rejected,
    };
    match spool_hook_event(
        &hook_v2_spool_root(data_root, host),
        HookSpoolConfigV1::stock(host),
        envelope,
        None,
        &snapshot.binding,
        now,
    ) {
        Ok(()) => NativeHookCaptureOutcomeV1::Captured,
        Err(error) => spool_refusal(error),
    }
}

/// Durably spools one hook envelope, waiting on another writer's spool lock
/// for at most [`HOOK_SYNCHRONOUS_BUDGET`]. When the writer lease or the
/// commit lock stays held past it, the envelope is staged without a lock for
/// the drain to import, so a busy spool delays the event instead of losing it.
pub fn spool_hook_event(
    root: &Path,
    config: HookSpoolConfigV1,
    envelope: HookEventEnvelopeV2,
    native_lifecycle: Option<NativeContextScoutLifecycleV1>,
    binding: &HookScopeBindingV1,
    now: UtcMicros,
) -> Result<(), HookSpoolError> {
    let envelope = match HookSpoolV1::open_within(root, config, now, HOOK_SYNCHRONOUS_BUDGET) {
        Ok((mut spool, _)) => {
            let envelope = redelivered_envelope(&mut spool, binding, envelope);
            spool.append_with_native_lifecycle(
                envelope.clone(),
                native_lifecycle.clone(),
                binding,
                now,
            )?;
            match spool.commit_within(HOOK_SYNCHRONOUS_BUDGET) {
                Err(HookSpoolError::AdmissionTimedOut) => envelope,
                committed => return committed,
            }
        }
        Err(HookSpoolError::AdmissionTimedOut) => envelope,
        Err(error) => return Err(error),
    };
    HookSpoolV1::stage(root, config, envelope, native_lifecycle, binding)
}

fn spool_refusal(error: HookSpoolError) -> NativeHookCaptureOutcomeV1 {
    match error {
        HookSpoolError::AdmissionTimedOut => NativeHookCaptureOutcomeV1::AdmissionTimedOut,
        HookSpoolError::SpoolFull => NativeHookCaptureOutcomeV1::Full,
        HookSpoolError::ResetRequired { reason } => {
            NativeHookCaptureOutcomeV1::ResetRequired(reason)
        }
        error => NativeHookCaptureOutcomeV1::Unavailable(error),
    }
}

/// Reuses the queued envelope when this callback is a redelivery of an event
/// the spool already holds.
///
/// The event ID is derived from the host's own identity fields, so one host
/// event delivered twice produces two envelopes that differ only in the
/// instant each process observed it. A retried callback or sibling hooks
/// firing concurrently for the same edit do the same. `HookSpoolV1::append` is idempotent
/// for an identical envelope but reports `EventIdConflict` for that timestamp
/// difference, which the capture lane surfaced as a failed hook exit even
/// though the event was already durable. Normalise the observation instant the
/// way the response path's `replay_envelope_if_pending` does; anything else
/// that differs stays a genuine conflict.
fn redelivered_envelope(
    spool: &mut HookSpoolV1,
    binding: &HookScopeBindingV1,
    envelope: HookEventEnvelopeV2,
) -> HookEventEnvelopeV2 {
    let Ok(Some(queued)) = spool.pending_envelope(envelope.event_id) else {
        return envelope;
    };
    if queued.validate(binding).is_err() {
        return envelope;
    }
    let mut candidate = envelope.clone();
    candidate.observed_at = queued.observed_at;
    if queued == candidate {
        queued
    } else {
        envelope
    }
}
