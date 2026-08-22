use std::io::Write;
use std::path::Path;

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

/// Returns the first plausible unexpanded `${...}` template variable in a
/// `--path` argument (e.g. `${workspaceFolder}`), or `None` when the value
/// contains no template syntax. The brace contents must look like a variable
/// name — a leading ASCII letter followed by word/`.`/`-` characters,
/// optionally with a `:`-introduced modifier such as a default value — so
/// degenerate forms (`${}`, `${ }`, `${a/b}`) and directories that merely
/// contain `$` are not misclassified. A matching value is overwhelmingly more
/// likely to be an unexpanded host template than a real directory name, so
/// callers treat it as "no path given".
pub fn unexpanded_template_variable(path: &str) -> Option<&str> {
    let mut search_from = 0;
    while let Some(offset) = path[search_from..].find("${") {
        let start = search_from + offset;
        let inner_start = start + 2;
        let end = inner_start + path[inner_start..].find('}')?;
        if plausible_template_contents(&path[inner_start..end]) {
            return Some(&path[start..=end]);
        }
        search_from = inner_start;
    }
    None
}

fn plausible_template_contents(contents: &str) -> bool {
    // Variable name, optionally followed by a `:`-introduced modifier
    // (e.g. `${workspaceFolder:-/tmp/fallback}`); only the name is validated.
    let name = contents.split(':').next().unwrap_or_default();
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Filters a `serve --path` CLI argument that the MCP host failed to expand.
///
/// Some hosts pass config template variables like `${workspaceFolder}`
/// through literally instead of expanding them (Cursor's headless
/// agent-session MCP scopes do this; see `plugin/README-cursor.md`). Such a
/// value is discarded with a stderr warning so daemon routing can fall back
/// to project discovery and MCP initialize roots without treating the literal
/// template as a project path.
pub fn sanitize_serve_path_arg(path: Option<String>) -> Option<String> {
    let raw = path?;
    let Some(variable) = unexpanded_template_variable(&raw) else {
        return Some(raw);
    };
    // The host may have spawned us with stderr closed; a failed diagnostic
    // write must not take the server down.
    let _ = writeln!(
        std::io::stderr(),
        "warning: --path '{raw}' contains the unexpanded template variable '{variable}' \
         (the MCP host did not expand it); ignoring --path and falling back to project discovery"
    );
    None
}

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

// ---------------------------------------------------------------------------
// Serve startup orchestration
// ---------------------------------------------------------------------------

/// Runs the `serve` command as a database-free proxy to the managed daemon.
pub async fn run_serve(path_arg: Option<String>, timings: bool) -> Result<()> {
    let original_cwd = std::env::current_dir().ok();
    let socket_path = crate::daemon::default_socket_path()?;
    if !crate::daemon::should_proxy_serve_to_daemon(&socket_path).await {
        return Err(TraceDecayError::Config {
            message: format!(
                "TraceDecay daemon socket '{}' is not available. Run `tracedecay daemon install-service` and ensure the service is running.",
                socket_path.display()
            ),
        });
    }
    let handshake = proxy_serve_handshake(path_arg, original_cwd.as_deref(), timings)?;
    crate::daemon::proxy_stdio_to_daemon(&socket_path, &handshake, None).await
}

/// Builds daemon routing metadata without opening a project or global
/// database. A running daemon is the sole database owner for this serve
/// process; it performs the open (and the same config-gated git auto-init)
/// after receiving the handshake.
fn proxy_serve_handshake(
    path_arg: Option<String>,
    original_cwd: Option<&Path>,
    timings: bool,
) -> Result<crate::daemon::DaemonHandshake> {
    let path = sanitize_serve_path_arg(path_arg);
    let explicit_path = path.is_some();
    let mut project_path = if explicit_path {
        Some(crate::config::resolve_path(path))
    } else {
        original_cwd.and_then(crate::config::discover_project_root)
    };

    let initialized = project_path
        .as_deref()
        .is_some_and(TraceDecay::is_initialized);
    let auto_init_candidate = project_path.as_deref().or(original_cwd);
    let auto_init_root = auto_init_candidate
        .filter(|candidate| !initialized && crate::config::load_sync_config(candidate).auto_init)
        .and_then(crate::worktree::git_worktree_root)
        .filter(|root| !crate::config::is_protected_auto_project_root(root));
    if let Some(root) = auto_init_root.as_ref() {
        project_path = Some(root.clone());
    }

    let scope_prefix = project_path
        .as_deref()
        .and_then(|project_path| serve_scope_prefix(original_cwd, project_path));
    let telemetry_timings = timings
        || project_path
            .as_deref()
            .is_some_and(|path| crate::config::load_telemetry_config(path).timings);
    let mut handshake = crate::daemon::DaemonHandshake::for_current_client(
        project_path,
        scope_prefix,
        telemetry_timings,
        auto_init_root.is_some(),
    )?;
    // An explicit path remains authoritative. Discovery-mode clients may use
    // initialize roots only when cwd neither resolved nor can be auto-inited,
    // matching the local resolver's fallback order.
    handshake.allow_initialize_root_routing =
        !explicit_path && !initialized && auto_init_root.is_none();
    Ok(handshake)
}

/// The scope prefix for a serve session: the relative path from the project
/// root to the directory serve was launched from, when the latter is inside
/// the project.
fn serve_scope_prefix(original_cwd: Option<&Path>, project_root: &Path) -> Option<String> {
    original_cwd.and_then(|cwd| {
        cwd.strip_prefix(project_root)
            .ok()
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(|rel| rel.to_string_lossy().into_owned())
    })
}

/// Legacy marker recognized in existing Cursor logs by doctor diagnostics.
/// Proxy-only `serve` no longer emits it, but older logs remain actionable.
pub const DEGRADED_SERVE_STDERR_MARKER: &str =
    "[tracedecay] serve: staying alive in degraded MCP mode";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projectless_cwd_does_not_become_a_daemon_project() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let cwd = tempfile::tempdir().unwrap();

        let handshake = proxy_serve_handshake(None, Some(cwd.path()), false)
            .expect("build projectless proxy handshake");

        assert_eq!(handshake.project_path, None);
        assert!(!handshake.allow_init);
        assert!(handshake.allow_initialize_root_routing);
    }

    #[test]
    fn user_home_git_cwd_does_not_become_a_daemon_project() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let home = dirs::home_dir().expect("test home");
        let status = std::process::Command::new(crate::git::git_program())
            .args(["init", "-q"])
            .current_dir(&home)
            .status()
            .expect("git init test home");
        assert!(status.success());

        let handshake = proxy_serve_handshake(None, Some(&home), false)
            .expect("build protected-home proxy handshake");

        assert_eq!(handshake.project_path, None);
        assert!(!handshake.allow_init);
        assert!(handshake.allow_initialize_root_routing);
    }

    #[tokio::test]
    async fn direct_project_open_fails_closed() {
        let path = Path::new("/tmp/tracedecay-direct-open-must-not-run");
        let Err(error) = ensure_initialized(path).await else {
            panic!("legacy local open must fail closed");
        };
        assert!(error.to_string().contains("managed TraceDecay daemon"));
    }

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
    fn degenerate_brace_forms_are_not_templates() {
        // Empty / whitespace / path-like brace contents are not plausible
        // variable names, so a directory literally named that way stays a
        // real path.
        assert_eq!(unexpanded_template_variable("${}"), None);
        assert_eq!(unexpanded_template_variable("${ }"), None);
        assert_eq!(unexpanded_template_variable("/tmp/${a/b}/x"), None);
        assert_eq!(unexpanded_template_variable("${1invalid}"), None);
        assert_eq!(unexpanded_template_variable("${_underscore}"), None);
    }

    #[test]
    fn later_plausible_template_wins_over_earlier_degenerate_braces() {
        assert_eq!(
            unexpanded_template_variable("/tmp/${ }/x/${workspaceFolder}"),
            Some("${workspaceFolder}")
        );
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
