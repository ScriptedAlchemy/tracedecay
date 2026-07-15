use std::ffi::OsStr;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::sessions::source::TranscriptCursorKey;

const CLAUDE_CURSOR_KEY_PREFIX: &str = "tracedecay-claude-cursor-v1";
const CLAUDE_SOURCE_ID_PREFIX: &str = "tracedecay-claude-source-v1";
const CLAUDE_OBSERVATION_SOURCE_ID_PREFIX: &str = "tracedecay-claude-observation-source-v1-sha256";
const CLAUDE_OBSERVATION_SOURCE_ID_DOMAIN: &[u8] = b"tracedecay.claude.observation-source.v1\0";

pub(super) fn claude_source_id(path: &Path) -> Option<String> {
    path.file_stem().map(claude_source_component)
}

pub(super) fn claude_observation_source_id(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        digest_claude_observation_source_id("unix-path", canonical.as_os_str().as_bytes())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let bytes: Vec<u8> = canonical
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        digest_claude_observation_source_id("windows-path-utf16le", &bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        digest_claude_observation_source_id("rust-path", canonical.as_os_str().as_encoded_bytes())
    }
}

fn digest_claude_observation_source_id(platform: &str, native_path: &[u8]) -> String {
    let mut hasher = Sha256::new();
    for frame in [
        CLAUDE_OBSERVATION_SOURCE_ID_DOMAIN,
        platform.as_bytes(),
        native_path,
    ] {
        hasher.update((frame.len() as u64).to_be_bytes());
        hasher.update(frame);
    }
    format!(
        "{CLAUDE_OBSERVATION_SOURCE_ID_PREFIX}-{}",
        hex::encode(hasher.finalize())
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

#[cfg(unix)]
fn claude_non_unicode_cursor_key(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    encode_claude_cursor_key("unix-bytes", path.as_os_str().as_bytes())
}

#[cfg(unix)]
fn claude_non_unicode_source_id(component: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;

    encode_claude_source_id("unix-bytes", component.as_bytes())
}

#[cfg(windows)]
fn claude_non_unicode_cursor_key(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;

    let bytes: Vec<u8> = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    encode_claude_cursor_key("windows-utf16le", &bytes)
}

#[cfg(windows)]
fn claude_non_unicode_source_id(component: &OsStr) -> String {
    use std::os::windows::ffi::OsStrExt;

    let bytes: Vec<u8> = component.encode_wide().flat_map(u16::to_le_bytes).collect();
    encode_claude_source_id("windows-utf16le", &bytes)
}

#[cfg(not(any(unix, windows)))]
fn claude_non_unicode_cursor_key(path: &Path) -> String {
    encode_claude_cursor_key("rust-os-str", path.as_os_str().as_encoded_bytes())
}

#[cfg(not(any(unix, windows)))]
fn claude_non_unicode_source_id(component: &OsStr) -> String {
    encode_claude_source_id("rust-os-str", component.as_encoded_bytes())
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
