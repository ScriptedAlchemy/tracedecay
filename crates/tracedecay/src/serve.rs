//! Daemon-side serve guards: MCP root-URI decoding and the fail-closed
//! direct-open contract. The `tracedecay serve` proxy command itself lives in
//! the CLI crate.

use std::path::Path;

use tracedecay_lsp::percent_hex_nibble;

use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_domain::errors::{Result, TraceDecayError};

/// Legacy compatibility entry point for callers that previously opened a
/// project database in-process.
///
/// Project databases are daemon-owned. Returning a local [`TraceDecay`] would
/// reintroduce a second `SQLite` owner, so this API deliberately fails closed.
#[allow(clippy::unused_async)]
pub async fn ensure_initialized(project_path: &Path) -> Result<TraceDecay> {
    Err(direct_project_open_disabled(project_path))
}

#[allow(clippy::unused_async)]
pub async fn ensure_initialized_with_options(
    project_path: &Path,
    _open_options: TraceDecayOpenOptions,
) -> Result<TraceDecay> {
    Err(direct_project_open_disabled(project_path))
}

fn direct_project_open_disabled(project_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "direct project database access is disabled for '{}'; route the operation through the managed TraceDecay daemon",
            project_path.display()
        ),
    }
}

pub(crate) fn local_path_from_mcp_root_uri(uri: &str) -> Option<std::path::PathBuf> {
    let path = if let Some(rest) = uri.strip_prefix("file://") {
        if let Some(localhost_path) = rest.strip_prefix("localhost/") {
            format!("/{localhost_path}")
        } else if rest == "localhost" {
            "/".to_string()
        } else if rest.starts_with('/') {
            rest.to_string()
        } else {
            return None;
        }
    } else {
        uri.to_string()
    };
    percent_decode_path(&path)
        .map(strip_windows_drive_slash)
        .map(std::path::PathBuf::from)
}

fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = *bytes.get(i + 1)?;
            let lo = *bytes.get(i + 2)?;
            decoded.push((percent_hex_nibble(hi)? << 4) | percent_hex_nibble(lo)?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// A Windows file URI like `file:///C:/work` decodes to `/C:/work`; the
/// leading slash before the drive letter must be dropped to form a usable
/// local path. On other platforms the path is returned unchanged.
#[cfg(windows)]
fn strip_windows_drive_slash(path: String) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        path[1..].to_string()
    } else {
        path
    }
}

#[cfg(not(windows))]
fn strip_windows_drive_slash(path: String) -> String {
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_project_open_fails_closed() {
        let path = Path::new("/tmp/tracedecay-direct-open-must-not-run");
        let Err(error) = ensure_initialized(path).await else {
            panic!("legacy local open must fail closed");
        };
        assert!(error.to_string().contains("managed TraceDecay daemon"));
    }
}
