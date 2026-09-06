//! Minimal native hook capture into the bounded replay spool.
//!
//! This path reads only a daemon-published binding and writes only the
//! content-free transport spool. It has no daemon, database, query, model,
//! session, memory, sync, or indexing authority.

use std::path::Path;

use tracedecay_domain::UtcMicros;

use crate::{
    HookConfigurationFileReaderV1, HookConfigurationReadOutcomeV1, HookConfigurationSubscriberV1,
    HookEventEnvelopeV2, HookHostV1, HookScopeBindingV1, HookSpoolConfigV1, HookSpoolError,
    HookSpoolV1, NativeEnvelopeMaterialV1, NativeHookDecodeError, OpenCodePluginSurfaceV1,
    decode_native_hook_event, decode_opencode_plugin_event, hook_configuration_path,
};

/// The real host surface that supplied native hook bytes.
///
/// OpenCode's direct tool callback has a distinct checked-in wire shape even
/// though it produces the same host-neutral envelope as its event-bus route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeHookCaptureSourceV1 {
    Host(HookHostV1),
    OpenCodeToolExecuteAfter,
}

impl NativeHookCaptureSourceV1 {
    pub const fn host(self) -> HookHostV1 {
        match self {
            Self::Host(host) => host,
            Self::OpenCodeToolExecuteAfter => HookHostV1::OpenCode,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeHookCaptureOutcomeV1 {
    Captured,
    Unsupported,
    Unbound,
    Rejected,
    Full,
    ResetRequired,
    Unavailable,
}

/// Single entry point for every native hook payload a host captures. This is
/// the coarse boundary that matters for hook latency: it decodes, binds, and
/// spools one event, so its cost and outcome mix stand in for the whole
/// capture path without measuring the decode/bind/spool internals separately.
#[hotpath::measure(label = "hooks.capture.native_event")]
pub fn capture_native_event_for_replay(
    data_root: &Path,
    source: NativeHookCaptureSourceV1,
    payload: &[u8],
    material: NativeEnvelopeMaterialV1,
    now: UtcMicros,
) -> NativeHookCaptureOutcomeV1 {
    let outcome = capture_native_event_for_replay_inner(data_root, source, payload, material, now);
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!(match outcome {
            NativeHookCaptureOutcomeV1::Captured => "hooks.capture.outcome.captured",
            NativeHookCaptureOutcomeV1::Unsupported => "hooks.capture.outcome.unsupported",
            NativeHookCaptureOutcomeV1::Unbound => "hooks.capture.outcome.unbound",
            NativeHookCaptureOutcomeV1::Rejected => "hooks.capture.outcome.rejected",
            NativeHookCaptureOutcomeV1::Full => "hooks.capture.outcome.full",
            NativeHookCaptureOutcomeV1::ResetRequired => "hooks.capture.outcome.reset_required",
            NativeHookCaptureOutcomeV1::Unavailable => "hooks.capture.outcome.unavailable",
        })
        .inc(1);
    }
    outcome
}

fn capture_native_event_for_replay_inner(
    data_root: &Path,
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
        hook_configuration_path(data_root, host),
    ));
    let HookConfigurationReadOutcomeV1::Bound(snapshot) = subscriber.load_current(host, now) else {
        return NativeHookCaptureOutcomeV1::Unbound;
    };
    let envelope = match decoded.into_envelope(&snapshot.binding, material) {
        Ok(envelope) => envelope,
        Err(_) => return NativeHookCaptureOutcomeV1::Rejected,
    };
    let spool_root = data_root.join("hook-v2-spool").join(host.hook_key());
    let mut spool = match HookSpoolV1::open(spool_root, HookSpoolConfigV1::stock(host), now) {
        Ok((spool, _)) => spool,
        Err(HookSpoolError::SpoolFull) => return NativeHookCaptureOutcomeV1::Full,
        Err(HookSpoolError::ResetRequired { .. }) => {
            return NativeHookCaptureOutcomeV1::ResetRequired;
        }
        Err(_) => return NativeHookCaptureOutcomeV1::Unavailable,
    };
    let envelope = redelivered_envelope(&mut spool, &snapshot.binding, envelope);
    match spool.append(envelope, &snapshot.binding, now) {
        Ok(_) => NativeHookCaptureOutcomeV1::Captured,
        Err(HookSpoolError::SpoolFull) => NativeHookCaptureOutcomeV1::Full,
        Err(HookSpoolError::ResetRequired { .. }) => NativeHookCaptureOutcomeV1::ResetRequired,
        Err(_) => NativeHookCaptureOutcomeV1::Unavailable,
    }
}

/// Reuses the queued envelope when this callback is a redelivery of an event
/// the spool already holds.
///
/// The event ID is derived from the host's own identity fields, so one host
/// event delivered twice — a retried callback, or sibling hooks firing
/// concurrently for the same edit — produces two envelopes that differ only in
/// the instant each process observed it. `HookSpoolV1::append` is idempotent
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
