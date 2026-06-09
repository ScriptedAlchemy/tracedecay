use std::path::Path;
use tokensave::tokensave::TokenSave;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeGlobalDbResolution {
    Found(std::path::PathBuf),
    Ambiguous(Vec<String>),
    None,
}

pub fn global_db_ambiguity_message(paths: &[String]) -> String {
    let mut message =
        "Multiple tokensave projects found — pass -p <path> to select one:".to_string();
    for path in paths {
        message.push_str("\n  ");
        message.push_str(path);
    }
    message
}

/// Opens an existing project, or tells the user to run `tokensave init` first.
pub async fn ensure_initialized(project_path: &Path) -> tokensave::errors::Result<TokenSave> {
    if TokenSave::is_initialized(project_path) {
        return TokenSave::open(project_path).await;
    }
    Err(tokensave::errors::TokenSaveError::Config {
        message: format!(
            "no TokenSave index found at '{}' — run 'tokensave init' first",
            project_path.display()
        ),
    })
}

fn initialized_project_paths(mut paths: Vec<String>) -> Vec<String> {
    paths.retain(|path| TokenSave::is_initialized(Path::new(path)));
    paths
}

/// Fallback for `serve`: when CWD-based discovery fails, check the global DB
/// for registered projects. When multiple projects exist, pick the best match
/// against cwd: prefer a project that is an ancestor of cwd (cwd is inside the
/// project), then a project that is a descendant of cwd (project is under cwd).
/// Ties at the winning depth are ambiguous and require an explicit path.
pub async fn resolve_serve_from_global_db() -> ServeGlobalDbResolution {
    let Some(gdb) = tokensave::global_db::GlobalDb::open().await else {
        return ServeGlobalDbResolution::None;
    };
    let mut paths = initialized_project_paths(gdb.list_project_paths().await);
    paths.sort();
    if paths.len() == 1 {
        return ServeGlobalDbResolution::Found(std::path::PathBuf::from(paths.remove(0)));
    }
    if paths.is_empty() {
        return ServeGlobalDbResolution::None;
    }

    // Multiple projects — try to resolve using cwd.
    let Ok(cwd) = std::env::current_dir() else {
        return ServeGlobalDbResolution::Ambiguous(paths);
    };
    let cwd = cwd.canonicalize().unwrap_or(cwd);

    // Priority 1: cwd is inside a project (project is ancestor of cwd).
    // Pick the deepest ancestor (most specific match).
    let mut ancestors: Vec<_> = paths
        .iter()
        .filter_map(|p| {
            let pp = std::path::Path::new(p).canonicalize().ok()?;
            cwd.starts_with(&pp)
                .then(|| (pp.components().count(), p.clone()))
        })
        .collect();
    ancestors.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if let Some((depth, _)) = ancestors.first() {
        if ancestors.get(1).is_some_and(|next| next.0 == *depth) {
            return ServeGlobalDbResolution::Ambiguous(
                ancestors.into_iter().map(|(_, p)| p).collect(),
            );
        }
        return ServeGlobalDbResolution::Found(std::path::PathBuf::from(ancestors.remove(0).1));
    }

    // Priority 2: a project is under cwd (cwd is ancestor of project).
    // Pick the shallowest descendant (closest child).
    let mut descendants: Vec<_> = paths
        .iter()
        .filter_map(|p| {
            let pp = std::path::Path::new(p).canonicalize().ok()?;
            pp.starts_with(&cwd)
                .then(|| (pp.components().count(), p.clone()))
        })
        .collect();
    descendants.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    if let Some((depth, _)) = descendants.first() {
        if descendants.get(1).is_some_and(|next| next.0 == *depth) {
            return ServeGlobalDbResolution::Ambiguous(
                descendants.into_iter().map(|(_, p)| p).collect(),
            );
        }
        return ServeGlobalDbResolution::Found(std::path::PathBuf::from(descendants.remove(0).1));
    }

    // No cwd-based match — the global DB alone cannot disambiguate.
    ServeGlobalDbResolution::Ambiguous(paths)
}

/// Last-resort fallback for `serve`: peek at the first stdin line to read the
/// MCP `initialize` request's `roots` array.  If a root matches a registered
/// project, return its path.  The raw line is stored in `out` so the caller
/// can replay it into the MCP transport (the server still needs to see it).
pub async fn resolve_serve_from_mcp_roots(out: &mut Option<String>) -> Option<std::path::PathBuf> {
    let line = read_first_non_empty_stdin_line().await?;
    *out = Some(line.trim().to_string());

    let parsed: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let roots = parsed.pointer("/params/roots").and_then(|v| v.as_array())?;

    let gdb = tokensave::global_db::GlobalDb::open().await?;
    let registered = initialized_project_paths(gdb.list_project_paths().await);

    // Try each root URI — first match wins.
    for root in roots {
        let uri = root.get("uri").and_then(|v| v.as_str()).unwrap_or_default();
        let Some(root_path) = local_path_from_mcp_root_uri(uri) else {
            continue;
        };
        // Exact match: the root IS a registered project.
        if let Some(hit) = registered
            .iter()
            .find(|p| std::path::Path::new(p) == root_path.as_path())
        {
            return Some(std::path::PathBuf::from(hit));
        }
        // Walk up from the root to find the nearest enclosing project.
        if let Some(discovered) = tokensave::config::discover_project_root(&root_path) {
            return Some(discovered);
        }
    }
    None
}

async fn read_first_non_empty_stdin_line() -> Option<String> {
    use tokio::io::AsyncReadExt;

    let mut stdin = tokio::io::stdin();
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    // Avoid buffering past the first line; the normal stdio transport must still
    // receive any later JSON-RPC messages from stdin.
    loop {
        match stdin.read(&mut byte).await {
            Ok(0) if line.is_empty() => return None,
            Ok(0) => {
                let text = String::from_utf8(line).ok()?;
                return (!text.trim().is_empty()).then_some(text);
            }
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    let text = String::from_utf8(std::mem::take(&mut line)).ok()?;
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
            Err(_) => return None,
        }
    }
}

fn local_path_from_mcp_root_uri(uri: &str) -> Option<std::path::PathBuf> {
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
    percent_decode_path(&path).map(std::path::PathBuf::from)
}

fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = *bytes.get(i + 1)?;
            let lo = *bytes.get(i + 2)?;
            decoded.push((hex_value(hi)? << 4) | hex_value(lo)?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
