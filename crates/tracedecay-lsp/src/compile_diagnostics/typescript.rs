//! `tsc --pretty false --noEmit` driver.
//!
//! tsc emits diagnostics as one-per-line text:
//!
//! ```text
//! src/lib.ts(4,15): error TS2322: Type 'string' is not assignable to type 'number'.
//! ```
//!
//! The parser extracts file, line, column, level, code, and message from
//! that shape. Multi-line `error: …` continuations are concatenated into
//! the prior diagnostic. We don't follow `tsc --build` references because
//! the resolver only ever asks for definitions on the explicitly opened
//! tsconfig.
//!
//! The compiler is the project's own `node_modules/.bin/tsc`, so the check
//! runs the TypeScript version the project pins rather than whatever happens
//! to be on the daemon's `PATH`. A `tsconfig.json` without it is a typed
//! [`TypeScriptProducerState::CompilerMissing`], not an empty success, because
//! a caller that asks for diagnostics must learn that nothing checked the
//! project.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;

use super::{Diagnostic, Driver, Scope, is_diagnostic_level};
use tracedecay_domain::errors::{Result, TraceDecayError};

/// The exact command that installs the project's own compiler where the
/// producer looks for it.
pub const TYPESCRIPT_INSTALL_COMMAND: &str = "npm install --save-dev typescript";

/// Whether the TypeScript diagnostics producer can run for a project root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeScriptProducerState {
    /// `tsconfig.json` is present and the project's own
    /// `node_modules/.bin/tsc` exists.
    Configured { compiler: PathBuf },
    /// `tsconfig.json` is present but the project has no compiler of its own.
    /// [`TYPESCRIPT_INSTALL_COMMAND`] adds one.
    CompilerMissing,
    /// No `tsconfig.json` at the project root; there is nothing to type-check.
    NoTsconfig,
}

/// Resolves the producer state for `project_root` from the filesystem alone.
pub fn typescript_producer_state(project_root: &Path) -> TypeScriptProducerState {
    if !project_root.join("tsconfig.json").is_file() {
        return TypeScriptProducerState::NoTsconfig;
    }
    match resolve_compiler(project_root) {
        Some(compiler) => TypeScriptProducerState::Configured { compiler },
        None => TypeScriptProducerState::CompilerMissing,
    }
}

fn resolve_compiler(project_root: &Path) -> Option<PathBuf> {
    let local = project_root
        .join("node_modules")
        .join(".bin")
        .join(if cfg!(windows) { "tsc.cmd" } else { "tsc" });
    local.is_file().then_some(local)
}

pub struct TscDriver;

impl Driver for TscDriver {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn detect(&self, project_root: &Path) -> bool {
        project_root.join("tsconfig.json").exists()
    }

    fn run<'a>(
        &'a self,
        project_root: &'a Path,
        _scope: &'a Scope,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Diagnostic>>> + Send + 'a>> {
        Box::pin(hotpath::future!(
            async move {
                let compiler = match typescript_producer_state(project_root) {
                    TypeScriptProducerState::Configured { compiler } => compiler,
                    TypeScriptProducerState::CompilerMissing => {
                        return Err(TraceDecayError::Config {
                            message: format!(
                                "no TypeScript compiler for '{}': run `{TYPESCRIPT_INSTALL_COMMAND}`",
                                project_root.display()
                            ),
                        });
                    }
                    TypeScriptProducerState::NoTsconfig => {
                        return Err(TraceDecayError::Config {
                            message: format!(
                                "'{}' has no tsconfig.json to type-check",
                                project_root.display()
                            ),
                        });
                    }
                };
                run_compiler(&compiler, project_root).await
            },
            label = "compile_diagnostics.typescript.tsc"
        ))
    }
}

/// Runs one resolved compiler over the project and parses its report.
///
/// tsc exits 0 for a clean project and 1 or 2 (`DiagnosticsPresent_*`) when it
/// reported diagnostics; all three are answers. Any other exit (an invalid
/// project, a reference cycle), or a diagnostic without a file location (a
/// `tsconfig.json` that names no inputs, an unreadable option), means the
/// project was not checked, and that is a typed failure rather than a clean
/// page.
pub async fn run_compiler(compiler: &Path, project_root: &Path) -> Result<Vec<Diagnostic>> {
    let mut cmd = tokio::process::Command::new(compiler);
    cmd.arg("--noEmit")
        .arg("--pretty")
        .arg("false")
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = cmd
        .output()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to spawn `{}`: {error}", compiler.display()),
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(global) = stdout
        .lines()
        .map(str::trim)
        .find(|line| is_global_error(line))
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "`{}` could not check the project: {global}",
                compiler.display()
            ),
        });
    }
    if !matches!(output.status.code(), Some(0..=2)) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(TraceDecayError::Config {
            message: format!(
                "`{}` exited with {}: {}",
                compiler.display(),
                output.status,
                stderr.trim().lines().next().unwrap_or("no output")
            ),
        });
    }
    Ok(parse_tsc_output(&stdout))
}

/// A tsc report line that carries no file location (`error TS18003: …`);
/// such lines describe the check itself failing, not a source finding.
fn is_global_error(line: &str) -> bool {
    line.starts_with("error TS")
}

/// Parse the full tsc stdout into a flat diagnostic list. Top-level so it
/// can be unit-tested without spawning tsc.
#[hotpath::measure(label = "compile_diagnostics.typescript.parse")]
pub fn parse_tsc_output(stdout: &str) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    for line in stdout.lines() {
        if let Some(diag) = parse_tsc_line(line) {
            out.push(diag);
            continue;
        }
        // Continuation line: append to the prior diagnostic if its text
        // doesn't start a new file(line,col): error pattern.
        if let Some(last) = out.last_mut() {
            let trimmed = line.trim_end();
            if !trimmed.is_empty() {
                last.message.push(' ');
                last.message.push_str(trimmed);
            }
        }
    }
    out
}

/// Parse a single tsc output line into a `Diagnostic`. Returns `None` for
/// non-diagnostic lines (banners, summary lines, blanks).
pub fn parse_tsc_line(line: &str) -> Option<Diagnostic> {
    // file(line,col): level TSnnnn: message
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    let after = &line[close + 1..];
    let after = after.strip_prefix(':')?.trim_start();

    let file = line[..open].to_string();
    let location = &line[open + 1..close];
    let mut parts = location.splitn(2, ',');
    let line_no: u32 = parts.next()?.trim().parse().ok()?;
    let column: u32 = parts.next()?.trim().parse().unwrap_or(1);

    // after = "error TS2322: Type ..." or "warning TS####: ..."
    let mut tokens = after.splitn(3, ' ');
    let level = tokens.next()?.to_string();
    if !is_diagnostic_level(&level) {
        return None;
    }
    let code_token = tokens.next()?;
    let message = tokens.next()?.trim().to_string();
    let code = code_token.trim_end_matches(':').to_string();

    Some(Diagnostic {
        file,
        line_start: line_no,
        line_end: line_no,
        column: column.max(1),
        level,
        code,
        message,
        driver: "typescript",
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_error_line() {
        let line =
            "src/lib.ts(4,15): error TS2322: Type 'string' is not assignable to type 'number'.";
        let d = parse_tsc_line(line).expect("should parse");
        assert_eq!(d.file, "src/lib.ts");
        assert_eq!(d.line_start, 4);
        assert_eq!(d.column, 15);
        assert_eq!(d.level, "error");
        assert_eq!(d.code, "TS2322");
        assert!(d.message.contains("not assignable"));
        assert_eq!(d.driver, "typescript");
    }

    #[test]
    fn parse_returns_none_for_blank_lines() {
        assert!(parse_tsc_line("").is_none());
        assert!(parse_tsc_line("   ").is_none());
    }

    #[test]
    fn parse_returns_none_for_summary_line() {
        // tsc summary lines like "Found 3 errors."
        assert!(parse_tsc_line("Found 3 errors.").is_none());
    }

    #[test]
    fn parse_continuation_appends_to_prior() {
        let stdout = "src/a.ts(1,1): error TS2322: Outer message.\n  Inner detail line.\n";
        let diags = parse_tsc_output(stdout);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Outer message"));
        assert!(diags[0].message.contains("Inner detail"));
    }

    #[test]
    fn parse_multiple_diagnostics() {
        let stdout = "\
src/a.ts(1,1): error TS2322: First.
src/b.ts(2,2): warning TS6133: Second.
";
        let diags = parse_tsc_output(stdout);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].file, "src/a.ts");
        assert_eq!(diags[1].level, "warning");
    }

    #[test]
    fn producer_state_distinguishes_no_tsconfig_missing_compiler_and_local_compiler() {
        let project = tempfile::tempdir().unwrap();
        assert_eq!(
            typescript_producer_state(project.path()),
            TypeScriptProducerState::NoTsconfig
        );

        std::fs::write(project.path().join("tsconfig.json"), "{}").unwrap();
        // The state never depends on whatever `tsc` the host happens to have.
        assert_eq!(
            typescript_producer_state(project.path()),
            TypeScriptProducerState::CompilerMissing
        );

        let bin = project.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let local = bin.join(if cfg!(windows) { "tsc.cmd" } else { "tsc" });
        std::fs::write(&local, "").unwrap();
        assert_eq!(
            typescript_producer_state(project.path()),
            TypeScriptProducerState::Configured { compiler: local },
        );
    }

    /// `tsc --noEmit` exits 2 (`DiagnosticsPresent_OutputsGenerated`) for a
    /// project with errors; treating only 0 and 1 as answers turned every
    /// real finding into a producer failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn run_compiler_accepts_every_diagnostics_present_exit_code() {
        use std::os::unix::fs::PermissionsExt;

        for exit in [0, 1, 2] {
            let project = tempfile::tempdir().unwrap();
            let compiler = project.path().join("tsc");
            std::fs::write(
                &compiler,
                format!(
                    "#!/bin/sh\necho \"src/index.ts(3,14): error TS4023: Exported variable 'value' cannot be named.\"\nexit {exit}\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o755)).unwrap();

            let diagnostics = run_compiler(&compiler, project.path())
                .await
                .unwrap_or_else(|error| panic!("exit {exit} is a checked project: {error}"));
            assert_eq!(diagnostics.len(), 1, "exit {exit}");
            assert_eq!(diagnostics[0].code, "TS4023");
            assert_eq!((diagnostics[0].line_start, diagnostics[0].column), (3, 14));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_compiler_refuses_a_check_that_named_no_inputs() {
        use std::os::unix::fs::PermissionsExt;

        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        std::fs::write(
            &compiler,
            "#!/bin/sh\necho \"error TS18003: No inputs were found in config file.\"\nexit 3\n",
        )
        .unwrap();
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o755)).unwrap();

        let error = run_compiler(&compiler, project.path())
            .await
            .expect_err("a file-less tsc error is not a clean project");
        assert!(
            error.to_string().contains("TS18003"),
            "the refusal names the compiler's own error: {error}"
        );
    }
}
