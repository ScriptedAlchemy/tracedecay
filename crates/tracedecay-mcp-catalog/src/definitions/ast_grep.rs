//! External ast-grep CLI capability detection shared by tool gating.

use serde_json::{Value, json};
use tracedecay_runtime_core::ast_grep::ast_grep_command;

/// Outcome of probing the external `ast-grep` CLI once per process.
///
/// `ast_grep_diagnostics_json` reports every field verbatim to
/// `tracedecay doctor`.
#[derive(Debug, Clone)]
pub struct AstGrepDiagnostics {
    pub installed: bool,
    pub version: Option<String>,
    pub rewrite_available: bool,
    pub message: String,
}

fn ast_grep_output_text(output: &std::process::Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(stderr);
    }
    text
}

fn parse_version_component(component: &str) -> Option<u64> {
    let digits = component
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn parse_ast_grep_version(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let token = token
            .trim_start_matches('v')
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '.');
        let mut parts = token.split('.');
        let Some(major) = parts.next().and_then(parse_version_component) else {
            continue;
        };
        let Some(minor) = parts.next().and_then(parse_version_component) else {
            continue;
        };
        let patch = parts.next().and_then(parse_version_component).unwrap_or(0);
        return Some(format!("{major}.{minor}.{patch}"));
    }
    None
}

fn ast_grep_diagnostics_uncached() -> AstGrepDiagnostics {
    let version_output = match ast_grep_command().arg("--version").output() {
        Ok(output) => output,
        Err(err) => {
            return AstGrepDiagnostics {
                installed: false,
                version: None,
                rewrite_available: false,
                message: format!(
                    "ast-grep is not installed or is not on PATH: {err}. Install ast-grep for rewrite support."
                ),
            };
        }
    };

    let version_text = ast_grep_output_text(&version_output);
    if !version_output.status.success() {
        return AstGrepDiagnostics {
            installed: true,
            version: parse_ast_grep_version(&version_text),
            rewrite_available: false,
            message: format!(
                "ast-grep --version failed. Install or repair ast-grep. Output: {version_text}"
            ),
        };
    }

    let version = parse_ast_grep_version(&version_text).unwrap_or(version_text);
    AstGrepDiagnostics {
        installed: true,
        message: format!("ast-grep {version} is available with rewrite support"),
        version: Some(version),
        rewrite_available: true,
    }
}

pub fn ast_grep_diagnostics() -> &'static AstGrepDiagnostics {
    use std::sync::OnceLock;
    static DIAGNOSTICS: OnceLock<AstGrepDiagnostics> = OnceLock::new();
    DIAGNOSTICS.get_or_init(ast_grep_diagnostics_uncached)
}

pub fn ast_grep_diagnostics_json() -> Value {
    let diagnostics = ast_grep_diagnostics();
    json!({
        "installed": diagnostics.installed,
        "version": diagnostics.version.clone(),
        "rewrite_available": diagnostics.rewrite_available,
        "message": diagnostics.message.clone(),
    })
}

/// Returns true when the external `ast-grep` binary is on PATH and responds to
/// `--version`. Result is cached after the first check so we don't fork a
/// subprocess on every `tools/list` request.
pub fn ast_grep_available() -> bool {
    ast_grep_diagnostics().rewrite_available
}
