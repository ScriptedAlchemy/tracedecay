use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeGlobalDbResolution {
    Found(PathBuf),
    Ambiguous(Vec<String>),
    None,
}

#[derive(Debug, Clone, Copy)]
enum CwdProjectMatch {
    ProjectContainsCwd,
    ProjectUnderCwd,
}

impl CwdProjectMatch {
    fn matches(self, project_path: &Path, cwd: &Path) -> bool {
        match self {
            Self::ProjectContainsCwd => cwd.starts_with(project_path),
            Self::ProjectUnderCwd => project_path.starts_with(cwd),
        }
    }

    fn sort_matches(self, matches: &mut [(usize, String)]) {
        match self {
            Self::ProjectContainsCwd => {
                matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            }
            Self::ProjectUnderCwd => {
                matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            }
        }
    }
}

/// Returns the first unexpanded `${...}` template variable in a `--path`
/// argument (e.g. `${workspaceFolder}`), or `None` when the value contains no
/// template syntax. A bare `$` without braces is not template syntax — real
/// directories can contain `$` in their names.
pub fn unexpanded_template_variable(path: &str) -> Option<&str> {
    let start = path.find("${")?;
    let end = path[start..].find('}')?;
    Some(&path[start..=start + end])
}

/// Filters a `serve --path` CLI argument that the MCP host failed to expand.
///
/// Some hosts pass config template variables through literally instead of
/// expanding them — Cursor's headless agent-session scopes spawn
/// `serve --path ${workspaceFolder}` verbatim, and Cursor never retries an
/// MCP scope whose process exited, so a fatal config error here permanently
/// breaks the connection. A literal `${...}` value can never name a real
/// project, so it is discarded with a stderr warning and `serve` falls back
/// to its normal no-path discovery chain (cwd walk-up, MCP initialize roots,
/// global project registry).
pub fn sanitize_serve_path_arg(path: Option<String>) -> Option<String> {
    let raw = path?;
    let Some(variable) = unexpanded_template_variable(&raw) else {
        return Some(raw);
    };
    eprintln!(
        "warning: --path '{raw}' contains the unexpanded template variable '{variable}' \
         (the MCP host did not expand it); ignoring --path and falling back to project discovery"
    );
    None
}

pub fn global_db_ambiguity_message(paths: &[String]) -> String {
    let mut message =
        "Multiple tracedecay projects found — pass -p <path> to select one:".to_string();
    for path in paths {
        message.push_str("\n  ");
        message.push_str(path);
    }
    message
}

/// Opens an existing project, or tells the user to run `tracedecay init` first.
pub async fn ensure_initialized(project_path: &Path) -> Result<TraceDecay> {
    ensure_initialized_with_options(project_path, TraceDecayOpenOptions::default()).await
}

pub async fn ensure_initialized_with_options(
    project_path: &Path,
    open_options: TraceDecayOpenOptions,
) -> Result<TraceDecay> {
    match TraceDecay::open_with_options(project_path, open_options.clone()).await {
        Ok(cg) => return Ok(cg),
        Err(open_err) => {
            match TraceDecay::open_read_only_with_options(project_path, open_options).await {
                Ok(cg) => {
                    cg.ensure_schema_current().await?;
                    return Ok(cg);
                }
                Err(_) => {
                    if !matches!(open_err, TraceDecayError::Config { .. }) {
                        return Err(open_err);
                    }
                }
            }
        }
    }
    Err(TraceDecayError::Config {
        message: format!(
            "no TraceDecay index found at '{}' — run 'tracedecay init' first",
            project_path.display()
        ),
    })
}

fn initialized_project_paths(mut paths: Vec<String>) -> Vec<String> {
    paths.retain(|path| TraceDecay::is_initialized(Path::new(path)));
    paths
}

fn cwd_match_resolution(
    paths: &[String],
    cwd: &Path,
    match_kind: CwdProjectMatch,
) -> Option<ServeGlobalDbResolution> {
    let mut matches: Vec<_> = paths
        .iter()
        .filter_map(|p| {
            let project_path = Path::new(p).canonicalize().ok()?;
            match_kind
                .matches(&project_path, cwd)
                .then(|| (project_path.components().count(), p.clone()))
        })
        .collect();
    match_kind.sort_matches(&mut matches);

    let (depth, _) = matches.first()?;
    if matches.get(1).is_some_and(|next| next.0 == *depth) {
        return Some(ServeGlobalDbResolution::Ambiguous(
            matches.into_iter().map(|(_, p)| p).collect(),
        ));
    }
    Some(ServeGlobalDbResolution::Found(PathBuf::from(
        matches.remove(0).1,
    )))
}

/// Fallback for `serve`: when CWD-based discovery fails, check the global DB
/// for registered projects. When multiple projects exist, pick the best match
/// against cwd: prefer a project that is an ancestor of cwd (cwd is inside the
/// project), then a project that is a descendant of cwd (project is under cwd).
/// Ties at the winning depth are ambiguous and require an explicit path.
pub async fn resolve_serve_from_global_db() -> ServeGlobalDbResolution {
    let Some(gdb) = GlobalDb::open().await else {
        return ServeGlobalDbResolution::None;
    };
    let mut paths = initialized_project_paths(gdb.list_project_paths().await);
    paths.sort();
    if paths.len() == 1 {
        return ServeGlobalDbResolution::Found(PathBuf::from(paths.remove(0)));
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
    if let Some(resolution) =
        cwd_match_resolution(&paths, &cwd, CwdProjectMatch::ProjectContainsCwd)
    {
        return resolution;
    }

    // Priority 2: a project is under cwd (cwd is ancestor of project).
    // Pick the shallowest descendant (closest child).
    if let Some(resolution) = cwd_match_resolution(&paths, &cwd, CwdProjectMatch::ProjectUnderCwd) {
        return resolution;
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

    let registered = match GlobalDb::open().await {
        Some(gdb) => initialized_project_paths(gdb.list_project_paths().await),
        None => Vec::new(),
    };

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
        if let Some(discovered) = crate::config::discover_project_root(&root_path) {
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
    percent_decode_path(&path)
        .map(strip_windows_drive_slash)
        .map(std::path::PathBuf::from)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_literal_workspace_folder_variable() {
        assert_eq!(
            unexpanded_template_variable("${workspaceFolder}"),
            Some("${workspaceFolder}")
        );
    }

    #[test]
    fn detects_template_variable_with_default_value_syntax() {
        assert_eq!(
            unexpanded_template_variable("${workspaceFolder:-/tmp/fallback}"),
            Some("${workspaceFolder:-/tmp/fallback}")
        );
    }

    #[test]
    fn detects_other_host_template_variables() {
        assert_eq!(
            unexpanded_template_variable("${workspaceRoot}"),
            Some("${workspaceRoot}")
        );
        assert_eq!(
            unexpanded_template_variable("${userHome}"),
            Some("${userHome}")
        );
    }

    #[test]
    fn detects_template_variable_embedded_in_a_longer_path() {
        assert_eq!(
            unexpanded_template_variable("${workspaceFolder}/packages/core"),
            Some("${workspaceFolder}")
        );
        assert_eq!(
            unexpanded_template_variable("/home/user/${workspaceFolderBasename}/src"),
            Some("${workspaceFolderBasename}")
        );
    }

    #[test]
    fn plain_paths_are_not_templates() {
        assert_eq!(unexpanded_template_variable("/home/user/project"), None);
        assert_eq!(unexpanded_template_variable("relative/dir"), None);
        assert_eq!(unexpanded_template_variable(""), None);
    }

    #[test]
    fn dollar_signs_without_brace_syntax_are_not_templates() {
        // Real directories can contain `$` — only `${...}` is template syntax.
        assert_eq!(unexpanded_template_variable("/tmp/pri$ce/data"), None);
        assert_eq!(unexpanded_template_variable("$workspaceFolder"), None);
        assert_eq!(unexpanded_template_variable("/tmp/{braces}/x"), None);
        assert_eq!(unexpanded_template_variable("/tmp/trailing$"), None);
    }

    #[test]
    fn unterminated_template_syntax_is_not_a_template() {
        assert_eq!(unexpanded_template_variable("/tmp/${unclosed"), None);
    }

    #[test]
    fn sanitize_keeps_plain_paths_and_none() {
        assert_eq!(
            sanitize_serve_path_arg(Some("/home/user/project".to_string())),
            Some("/home/user/project".to_string())
        );
        assert_eq!(
            sanitize_serve_path_arg(Some("/tmp/pri$ce".to_string())),
            Some("/tmp/pri$ce".to_string())
        );
        assert_eq!(sanitize_serve_path_arg(None), None);
    }

    #[test]
    fn sanitize_discards_unexpanded_template_paths() {
        assert_eq!(
            sanitize_serve_path_arg(Some("${workspaceFolder}".to_string())),
            None
        );
        assert_eq!(
            sanitize_serve_path_arg(Some("${workspaceFolder}/nested".to_string())),
            None
        );
    }
}
