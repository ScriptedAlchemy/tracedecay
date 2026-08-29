//! External ast-grep CLI capability detection shared by tool gating.

use serde_json::{Value, json};

const MIN_AST_GREP_OUTLINE_VERSION: (u64, u64, u64) = (0, 44, 0);

#[derive(Debug, Clone)]
pub struct AstGrepDiagnostics {
    pub installed: bool,
    pub version: Option<String>,
    pub rewrite_available: bool,
    pub outline_available: bool,
    pub outline_version_ok: bool,
    pub outline_flags_ok: bool,
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

fn parse_ast_grep_version(text: &str) -> Option<(String, (u64, u64, u64))> {
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
        return Some((format!("{major}.{minor}.{patch}"), (major, minor, patch)));
    }
    None
}

fn ast_grep_diagnostics_uncached() -> AstGrepDiagnostics {
    let version_output = match crate::host_cli::ast_grep_command()
        .arg("--version")
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            return AstGrepDiagnostics {
                installed: false,
                version: None,
                rewrite_available: false,
                outline_available: false,
                outline_version_ok: false,
                outline_flags_ok: false,
                message: format!(
                    "ast-grep is not installed or is not on PATH: {err}. Install ast-grep >= 0.44 for tracedecay_outline and rewrite support."
                ),
            };
        }
    };

    let version_text = ast_grep_output_text(&version_output);
    if !version_output.status.success() {
        return AstGrepDiagnostics {
            installed: true,
            version: parse_ast_grep_version(&version_text).map(|(version, _)| version),
            rewrite_available: false,
            outline_available: false,
            outline_version_ok: false,
            outline_flags_ok: false,
            message: format!(
                "ast-grep --version failed. Install or repair ast-grep >= 0.44. Output: {version_text}"
            ),
        };
    }

    let (version, version_tuple) =
        parse_ast_grep_version(&version_text).unwrap_or_else(|| (version_text.clone(), (0, 0, 0)));
    let outline_version_ok = version_tuple >= MIN_AST_GREP_OUTLINE_VERSION;
    let help_output = crate::host_cli::ast_grep_command()
        .args(["outline", "--help"])
        .output();
    let (outline_flags_ok, help_detail) = match help_output {
        Ok(output) => {
            let help_text = ast_grep_output_text(&output);
            (
                output.status.success()
                    && help_text.contains("--json")
                    && help_text.contains("--items")
                    && help_text.contains("--view"),
                help_text,
            )
        }
        Err(err) => (false, err.to_string()),
    };
    let outline_available = outline_version_ok && outline_flags_ok;
    let message = if outline_available {
        format!("ast-grep {version} is available with outline JSON support")
    } else if !outline_version_ok {
        format!("ast-grep {version} is installed, but tracedecay_outline requires ast-grep >= 0.44")
    } else {
        format!(
            "ast-grep {version} is installed, but `ast-grep outline --help` does not advertise the required --json, --items, and --view flags. Output: {help_detail}"
        )
    };

    AstGrepDiagnostics {
        installed: true,
        version: Some(version),
        rewrite_available: true,
        outline_available,
        outline_version_ok,
        outline_flags_ok,
        message,
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
        "outline_available": diagnostics.outline_available,
        "outline_min_version": "0.44.0",
        "outline_version_ok": diagnostics.outline_version_ok,
        "outline_flags_ok": diagnostics.outline_flags_ok,
        "message": diagnostics.message.clone(),
    })
}

/// Returns true when the external `ast-grep` binary is on PATH and responds to
/// `--version`. Result is cached after the first check so we don't fork a
/// subprocess on every `tools/list` request.
pub fn ast_grep_available() -> bool {
    ast_grep_diagnostics().rewrite_available
}

/// Returns true when the external `ast-grep` CLI supports `outline` JSON output
/// with the flags introduced in ast-grep 0.44.
pub fn ast_grep_outline_available() -> bool {
    ast_grep_diagnostics().outline_available
}
