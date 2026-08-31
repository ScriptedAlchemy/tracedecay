//! `tracedecay serve`: a database-free stdio proxy to the managed daemon.

use std::io::Write;
use std::path::Path;

use tracedecay::tracedecay::TraceDecay;
use tracedecay_domain::errors::Result;

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

/// Runs the `serve` command as a database-free proxy to the managed daemon.
#[hotpath::measure(label = "cli.serve.proxy", future = true)]
pub async fn run_serve(path_arg: Option<String>, timings: bool) -> Result<()> {
    let original_cwd = std::env::current_dir().ok();
    let socket_path = tracedecay_daemon_control::default_socket_path()?;
    if !tracedecay::daemon::should_proxy_serve_to_daemon(&socket_path).await {
        return Err(tracedecay::daemon::unavailable_error(&socket_path));
    }
    let handshake = proxy_serve_handshake(path_arg, original_cwd.as_deref(), timings)?;
    tracedecay::daemon::proxy_stdio_to_daemon(&socket_path, &handshake, None).await
}

/// Builds daemon routing metadata without opening a project or global
/// database. A running daemon is the sole database owner for this serve
/// process; it performs the open (and the same config-gated git auto-init)
/// after receiving the handshake.
fn proxy_serve_handshake(
    path_arg: Option<String>,
    original_cwd: Option<&Path>,
    timings: bool,
) -> Result<tracedecay_daemon_protocol::DaemonHandshake> {
    let unexpanded_template_path = path_arg
        .as_deref()
        .and_then(unexpanded_template_variable)
        .is_some();
    let path = sanitize_serve_path_arg(path_arg);
    let explicit_path = path.is_some();
    let mut resolved_path = if explicit_path {
        tracedecay::config::resolve_path(path)
    } else {
        tracedecay::config::resolve_path_with_discovery(None)
    };

    let ambient_discovery =
        !explicit_path && tracedecay::config::is_ambient_project_root(&resolved_path);
    let initialized = !ambient_discovery && TraceDecay::is_initialized(&resolved_path);
    // `serve` is a database-free proxy. It may consult only an already-pinned
    // in-memory snapshot; missing authority disables implicit auto-init rather
    // than reading legacy `config.json` from the client process.
    // A never-opened project has no pinned snapshot, so `cached_sync_config`
    // fails. Follow the schema default (auto-init enabled) in that case rather
    // than failing closed: otherwise a discovery-mode client sitting in an
    // unindexed git worktree would surrender routing to MCP initialize roots
    // (or the daemon's own cwd) instead of initializing the client's cwd. This
    // mirrors the same default fallback in `resolve_daemon_initialize_route`.
    let auto_init_root = (!ambient_discovery
        && !initialized
        && tracedecay::config::cached_sync_config(&resolved_path).map_or_else(
            |_| tracedecay::config::SyncConfig::default().auto_init,
            |config| config.auto_init,
        ))
    .then(|| tracedecay_runtime_core::worktree::git_worktree_root(&resolved_path))
    .flatten()
    .filter(|root| !tracedecay::config::is_ambient_project_root(root));
    if let Some(root) = auto_init_root.as_ref() {
        resolved_path.clone_from(root);
    }

    let project_path = (!ambient_discovery).then_some(resolved_path);
    let scope_prefix = project_path
        .as_deref()
        .and_then(|project_path| serve_scope_prefix(original_cwd, project_path));
    let telemetry_timings = timings
        || project_path.as_deref().is_some_and(|path| {
            tracedecay::config::cached_telemetry_config(path)
                .is_ok_and(|telemetry| telemetry.timings)
        });
    let mut handshake = tracedecay::daemon::handshake_for_current_client(
        project_path,
        scope_prefix,
        telemetry_timings,
        auto_init_root.is_some(),
    )?;
    // An explicit path remains authoritative. A literal host template means
    // the host failed to provide that path, so initialize roots must replace
    // any incidental process-cwd discovery (for example Cursor launching the
    // MCP process from $HOME). Ordinary discovery-mode clients retain cwd
    // precedence when cwd resolved or can be auto-initialized.
    handshake.allow_initialize_root_routing = unexpanded_template_path
        || (!explicit_path && (!initialized || ambient_discovery) && auto_init_root.is_none());
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
