use std::path::Path;
use tokensave::tokensave::TokenSave;

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

/// Fallback for `serve`: when CWD-based discovery fails, check the global DB
/// for registered projects. When multiple projects exist, pick the best match
/// against cwd: prefer a project that is an ancestor of cwd (cwd is inside the
/// project), then a project that is a descendant of cwd (project is under cwd).
/// Among multiple matches, the deepest (most specific) path wins.
pub async fn resolve_serve_from_global_db() -> Option<std::path::PathBuf> {
    let gdb = tokensave::global_db::GlobalDb::open().await?;
    let mut paths: Vec<String> = gdb.list_project_paths().await;
    // Keep only projects whose .tokensave dir still exists on disk.
    paths.retain(|p| {
        std::path::Path::new(p)
            .join(".tokensave/tokensave.db")
            .exists()
    });
    if paths.len() == 1 {
        return Some(std::path::PathBuf::from(paths.remove(0)));
    }
    if paths.is_empty() {
        return None;
    }

    // Multiple projects — try to resolve using cwd.
    let cwd = std::env::current_dir().ok()?;
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
    ancestors.sort_by_key(|a| std::cmp::Reverse(a.0)); // deepest first
    if let Some((_, best)) = ancestors.into_iter().next() {
        return Some(std::path::PathBuf::from(best));
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
    descendants.sort_by_key(|a| a.0); // shallowest first
    if let Some((_, best)) = descendants.into_iter().next() {
        return Some(std::path::PathBuf::from(best));
    }

    // No cwd-based match — report the ambiguity.
    eprintln!("Multiple tokensave projects found — pass -p <path> to select one:");
    for p in &paths {
        eprintln!("  {p}");
    }
    None
}

/// Last-resort fallback for `serve`: peek at the first stdin line to read the
/// MCP `initialize` request's `roots` array.  If a root matches a registered
/// project, return its path.  The raw line is stored in `out` so the caller
/// can replay it into the MCP transport (the server still needs to see it).
pub async fn resolve_serve_from_mcp_roots(out: &mut Option<String>) -> Option<std::path::PathBuf> {
    use tokio::io::AsyncReadExt;
    let mut stdin = tokio::io::stdin();
    let mut line: Vec<u8> = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match stdin.read(&mut byte).await {
            Ok(0) => return None, // EOF
            Ok(_) => match byte[0] {
                b'\n' if line.iter().any(|b| !b.is_ascii_whitespace()) => break,
                b'\n' => line.clear(),
                b'\r' => {}
                b => line.push(b),
            },
            Err(_) => return None,
        }
    }
    let line = String::from_utf8(line).ok()?;
    *out = Some(line.trim().to_string());

    let mut registered: Vec<String> = match tokensave::global_db::GlobalDb::open().await {
        Some(gdb) => gdb.list_project_paths().await,
        None => Vec::new(),
    };
    registered.retain(|p| {
        std::path::Path::new(p)
            .join(".tokensave/tokensave.db")
            .exists()
    });
    resolve_project_from_mcp_initialize_line(&line, &registered)
}

fn resolve_project_from_mcp_initialize_line(
    line: &str,
    registered: &[String],
) -> Option<std::path::PathBuf> {
    let parsed: serde_json::Value = serde_json::from_str(line).ok()?;
    let roots = parsed.pointer("/params/roots").and_then(|v| v.as_array())?;

    // Try each root URI — first match wins.
    for root in roots {
        let uri = root.get("uri").and_then(|v| v.as_str()).unwrap_or_default();
        let root_path = uri.strip_prefix("file://").unwrap_or(uri);
        let root_path = std::path::Path::new(root_path);
        // Exact match: the root IS a registered project.
        if let Some(hit) = registered
            .iter()
            .find(|p| std::path::Path::new(p) == root_path)
        {
            return Some(std::path::PathBuf::from(hit));
        }
        // Walk up from the root to find the nearest enclosing project.
        if let Some(discovered) = tokensave::config::discover_project_root(root_path) {
            return Some(discovered);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn initialized_project() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".tokensave")).unwrap();
        std::fs::write(dir.path().join(".tokensave/tokensave.db"), b"").unwrap();
        dir
    }

    #[test]
    fn mcp_initialize_roots_resolve_initialized_project_without_global_db() {
        let active = initialized_project();
        let line = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "roots": [{
                    "uri": format!("file://{}", active.path().display()),
                    "name": "active"
                }]
            }
        })
        .to_string();

        let resolved = resolve_project_from_mcp_initialize_line(&line, &[]).unwrap();
        assert_eq!(resolved, active.path());
    }
}
