//! MCP root-URI decoding for the daemon's serve path. The `tracedecay serve`
//! proxy command itself lives in the CLI crate.

use tracedecay_lsp::percent_hex_nibble;

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
