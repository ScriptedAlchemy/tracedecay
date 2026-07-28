use std::ffi::OsStr;
use std::path::Path;

use crate::sessions::source::{TranscriptCursorKey, canonical_framed_sha256};

const CLAUDE_CURSOR_KEY_PREFIX: &str = "tracedecay-claude-cursor-v1";
const CLAUDE_SOURCE_ID_PREFIX: &str = "tracedecay-claude-source-v1";
const CLAUDE_OBSERVATION_SOURCE_ID_PREFIX: &str = "tracedecay-claude-observation-source-v1-sha256";
const CLAUDE_OBSERVATION_SOURCE_ID_DOMAIN: &[u8] = b"tracedecay.claude.observation-source.v1\0";

pub(super) fn claude_source_id(path: &Path) -> Option<String> {
    path.file_stem().map(claude_source_component)
}

pub(super) fn claude_observation_source_id(path: &Path) -> String {
    let transcript_id = path.file_stem().unwrap_or(path.as_os_str());
    digest_claude_observation_source_id(claude_source_component(transcript_id).as_bytes())
}

fn digest_claude_observation_source_id(native_transcript_id: &[u8]) -> String {
    format!(
        "{CLAUDE_OBSERVATION_SOURCE_ID_PREFIX}-{}",
        canonical_framed_sha256(CLAUDE_OBSERVATION_SOURCE_ID_DOMAIN, &[native_transcript_id])
    )
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

    TranscriptCursorKey::opaque(claude_non_unicode_cursor_key(path), path)
}

fn claude_non_unicode_cursor_key(path: &Path) -> String {
    encode_claude_cursor_key(
        claude_native_platform(),
        &crate::os_str_bytes::native_os_str_bytes(path.as_os_str()),
    )
}

fn claude_non_unicode_source_id(component: &OsStr) -> String {
    encode_claude_source_id(
        claude_native_platform(),
        &crate::os_str_bytes::native_os_str_bytes(component),
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

pub(super) fn encode_claude_cursor_key(platform: &str, native_path: &[u8]) -> String {
    format!(
        "{CLAUDE_CURSOR_KEY_PREFIX}-{platform}-{}",
        hex::encode(native_path)
    )
}

pub(super) fn encode_claude_source_id(platform: &str, native_stem: &[u8]) -> String {
    format!(
        "{CLAUDE_SOURCE_ID_PREFIX}-{platform}-{}",
        hex::encode(native_stem)
    )
}
