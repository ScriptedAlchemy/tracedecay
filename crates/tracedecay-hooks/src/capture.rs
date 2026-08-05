//! Minimal native hook capture into the bounded replay spool.
//!
//! This path reads only a daemon-published binding and writes only the
//! content-free transport spool. It has no daemon, database, query, model,
//! session, memory, sync, or indexing authority.

use std::path::Path;

use tracedecay_domain::{UtcMicros, framed_log::checksum};

use crate::{
    DecodedNativeHookEventV1, HookConfigurationFileReaderV1, HookConfigurationReadOutcomeV1,
    HookConfigurationSubscriberV1, HookEventFamily, HookHostV1, HookSpoolConfigV1, HookSpoolError,
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

pub fn capture_native_event_for_replay(
    data_root: &Path,
    source: NativeHookCaptureSourceV1,
    payload: &[u8],
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
    let Some(material) = native_material(&decoded, now) else {
        return NativeHookCaptureOutcomeV1::Rejected;
    };
    let envelope = match decoded.into_envelope(&snapshot.binding, material) {
        Ok(envelope) => envelope,
        Err(_) => return NativeHookCaptureOutcomeV1::Rejected,
    };
    let spool_root = data_root.join("hook-v2-spool").join(host.hook_key());
    let mut spool = match HookSpoolV1::open(spool_root, HookSpoolConfigV1::stock(host), now) {
        Ok((spool, _)) => spool,
        Err(HookSpoolError::SpoolFull) => return NativeHookCaptureOutcomeV1::Full,
        Err(HookSpoolError::ResetRequired) => {
            return NativeHookCaptureOutcomeV1::ResetRequired;
        }
        Err(_) => return NativeHookCaptureOutcomeV1::Unavailable,
    };
    match spool.append(envelope, &snapshot.binding, now) {
        Ok(_) => NativeHookCaptureOutcomeV1::Captured,
        Err(HookSpoolError::SpoolFull) => NativeHookCaptureOutcomeV1::Full,
        Err(HookSpoolError::ResetRequired) => NativeHookCaptureOutcomeV1::ResetRequired,
        Err(_) => NativeHookCaptureOutcomeV1::Unavailable,
    }
}

fn native_material(
    decoded: &DecodedNativeHookEventV1,
    observed_at: UtcMicros,
) -> Option<NativeEnvelopeMaterialV1> {
    let session = decoded.native_session_id()?;
    let family = decoded.family();
    let event_id = hash16(decoded.native_event_key()?.as_bytes());
    let file_id = (family == HookEventFamily::SavedEdit)
        .then(|| {
            decoded
                .native_file_path()
                .map(|path| hash16(path.as_bytes()))
        })
        .flatten();
    Some(NativeEnvelopeMaterialV1 {
        event_id,
        protected_session_id: checksum(session.as_bytes()),
        observed_at,
        tool_id: (family == HookEventFamily::ToolLifecycle).then_some(event_id),
        effect_receipt_id: decoded
            .native_call_id()
            .map(|value| hash16(value.as_bytes())),
        file_id,
        changed_range_count: decoded.changed_range_count(),
    })
}

fn hash16(bytes: &[u8]) -> [u8; 16] {
    let digest = checksum(bytes);
    let mut output = [0; 16];
    output.copy_from_slice(&digest[..16]);
    output
}
