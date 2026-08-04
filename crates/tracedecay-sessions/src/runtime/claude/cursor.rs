use std::ffi::OsStr;
use std::path::Path;

use tracedecay_capture::claude::{encode_cursor_key, encode_source_id, observation_source_id};

use crate::runtime::source::TranscriptCursorKey;

pub(super) fn claude_source_id(path: &Path) -> Option<String> {
    path.file_stem().map(claude_source_component)
}

pub(super) fn claude_observation_source_id(path: &Path) -> String {
    let transcript_id = path.file_stem().unwrap_or(path.as_os_str());
    observation_source_id(claude_source_component(transcript_id).as_bytes())
}

pub(super) fn claude_source_component(component: &OsStr) -> String {
    if let Some(component) = component.to_str() {
        return component.to_owned();
    }
    claude_non_unicode_source_id(component)
}

pub(super) fn claude_cursor_key(path: &Path) -> TranscriptCursorKey {
    if path.to_str().is_some() {
        return TranscriptCursorKey::for_path(path);
    }

    TranscriptCursorKey::opaque(claude_non_unicode_cursor_key(path))
}

fn claude_non_unicode_cursor_key(path: &Path) -> String {
    encode_cursor_key(
        claude_native_platform(),
        &tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(path.as_os_str()),
    )
}

fn claude_non_unicode_source_id(component: &OsStr) -> String {
    encode_source_id(
        claude_native_platform(),
        &tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(component),
    )
}

#[cfg(unix)]
fn claude_native_platform() -> &'static str {
    "unix-bytes"
}

#[cfg(windows)]
fn claude_native_platform() -> &'static str {
    "windows-utf16le"
}

#[cfg(not(any(unix, windows)))]
fn claude_native_platform() -> &'static str {
    "rust-os-str"
}
