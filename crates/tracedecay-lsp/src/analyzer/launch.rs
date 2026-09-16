//! Resolves the executable an analyzer spawn may run without mutating the host.
//!
//! A configured command such as `rust-analyzer` is usually a rustup proxy. The
//! proxy resolves the project's active toolchain from the spawn directory and,
//! by default, downloads that toolchain when it is missing — so opening a
//! dashboard on a project whose parent pins an uninstalled toolchain started a
//! network install into the rustup home. Every spawn therefore goes through
//! [`AnalyzerLaunchResolver::resolve`]: a proxy is replaced by the real
//! toolchain binary `rustup which` reports (with auto-install disabled), a
//! missing toolchain or component is a typed [`AnalyzerLaunchError`], and the
//! spawned environment always carries [`RUSTUP_AUTO_INSTALL_ENV`]`=0` so a
//! child `cargo` the analyzer runs cannot install either.
//!
//! Every rustup probe is bounded by [`RUSTUP_PROBE_DEADLINE`] and runs in a
//! blocking section handed off the async worker, because the broker calls
//! into here while its callers hold the broker lock on the Tokio executor.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// rustup honours this variable since 1.28.1; `0` refuses every implicit
/// toolchain install, from proxies and from `rustup which` alike. Older rustup
/// ignores it, so [`MIN_RUSTUP_VERSION_FOR_NO_INSTALL`] is enforced before any
/// probe that could resolve a toolchain.
pub const RUSTUP_AUTO_INSTALL_ENV: &str = "RUSTUP_AUTO_INSTALL";
pub const RUSTUP_AUTO_INSTALL_DISABLED: &str = "0";

/// The first rustup release that honours [`RUSTUP_AUTO_INSTALL_ENV`].
pub const MIN_RUSTUP_VERSION_FOR_NO_INSTALL: RustupVersion = RustupVersion {
    major: 1,
    minor: 28,
    patch: 1,
};

/// Upper bound on one `rustup --version` or `rustup which` probe. A probe that
/// has not answered by then is killed and reported as a typed refusal.
pub const RUSTUP_PROBE_DEADLINE: Duration = Duration::from_secs(3);

/// Operator-facing sentence for a proxy whose toolchain lacks the analyzer.
pub const NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE: &str =
    "rust-analyzer is not installed for this toolchain";
/// Operator-facing sentence for a proxy whose pinned toolchain is absent.
pub const TOOLCHAIN_NOT_INSTALLED_MESSAGE: &str =
    "the toolchain this project pins is not installed";

/// The exact program and environment one analyzer spawn uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalyzerLaunch {
    /// The configured command, or the real toolchain binary behind a rustup
    /// proxy.
    pub program: PathBuf,
    /// Environment the spawn sets on top of the daemon's own.
    pub env: Vec<(String, String)>,
}

impl AnalyzerLaunch {
    /// Spawns `command` directly, with the no-install environment applied.
    pub fn direct(command: &str) -> Self {
        Self {
            program: PathBuf::from(command),
            env: no_install_env(),
        }
    }
}

/// A parsed `rustup --version`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RustupVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl RustupVersion {
    /// Parses the first line of `rustup --version` (`rustup 1.29.0 (hash date)`).
    fn parse_output(stdout: &str) -> Option<Self> {
        let first_line = stdout.lines().next()?;
        let version = first_line.strip_prefix("rustup ")?.split(' ').next()?;
        let mut parts = version.split('.').map(|part| part.parse::<u32>().ok());
        let (Some(major), Some(minor), Some(patch)) = (parts.next()?, parts.next()?, parts.next()?)
        else {
            return None;
        };
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for RustupVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Why no analyzer may be started for a configured command.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnalyzerLaunchError {
    #[error("LSP command '{command}' is not available on PATH")]
    CommandUnavailable { command: String },
    /// The command is a rustup proxy, the project's toolchain is installed,
    /// and it has no such component. No install was attempted.
    #[error("{NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE}")]
    NotInstalledForToolchain { command: String },
    /// The command is a rustup proxy and the toolchain the project pins is not
    /// installed at all. No install was attempted.
    #[error("{TOOLCHAIN_NOT_INSTALLED_MESSAGE}")]
    ToolchainNotInstalled { command: String },
    /// The rustup behind the proxy predates `RUSTUP_AUTO_INSTALL`, so no probe
    /// that resolves a toolchain may run through it.
    #[error("rustup {found} predates {RUSTUP_AUTO_INSTALL_ENV} support")]
    RustupTooOldForNoInstall { found: RustupVersion },
    /// A rustup probe did not answer within [`RUSTUP_PROBE_DEADLINE`].
    #[error("toolchain probe for '{command}' timed out")]
    ProbeTimedOut { command: String },
    /// `rustup` could not be run or answered without a recognisable result.
    #[error("toolchain probe for '{command}' failed: {reason}")]
    ProbeFailed { command: String, reason: String },
}

impl AnalyzerLaunchError {
    /// One-sentence engine error for the dashboard; never carries stderr.
    pub fn engine_error(&self) -> String {
        match self {
            Self::NotInstalledForToolchain { command } => format!(
                "{NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE}; add it with `rustup component add {}` (no toolchain install was attempted)",
                command_stem(command)
            ),
            Self::ToolchainNotInstalled { .. } => format!(
                "{TOOLCHAIN_NOT_INSTALLED_MESSAGE}; install it with `rustup toolchain install` (no toolchain install was attempted)"
            ),
            Self::RustupTooOldForNoInstall { found } => format!(
                "rustup {found} predates {RUSTUP_AUTO_INSTALL_ENV} (rustup {MIN_RUSTUP_VERSION_FOR_NO_INSTALL} or newer); update it with `rustup self update` before analyzers can start (no toolchain probe was run)"
            ),
            Self::ProbeTimedOut { command } => format!(
                "toolchain probe for '{}' did not answer within {} seconds (no toolchain install was attempted)",
                command_stem(command),
                RUSTUP_PROBE_DEADLINE.as_secs()
            ),
            Self::CommandUnavailable { .. } | Self::ProbeFailed { .. } => self.to_string(),
        }
    }
}

/// Resolves analyzer launches, remembering what each rustup binary answered
/// to `rustup --version`. The version of a binary does not change while the
/// broker holds it, so the answer lives as long as the broker's launch cache.
#[derive(Debug, Default)]
pub struct AnalyzerLaunchResolver {
    rustup_versions: std::collections::BTreeMap<PathBuf, RustupVersion>,
}

impl AnalyzerLaunchResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets every remembered rustup version.
    pub fn clear(&mut self) {
        self.rustup_versions.clear();
    }

    /// Resolves how `command` is started for an analyzer rooted at
    /// `workspace_root`.
    ///
    /// The `which` probe runs from `workspace_root` because that is where
    /// rustup reads the active toolchain override a spawn would see. Both
    /// probes run bounded and off the async worker.
    pub fn resolve(
        &mut self,
        command: &str,
        workspace_root: &Path,
    ) -> Result<AnalyzerLaunch, AnalyzerLaunchError> {
        let Some(located) = locate_command(command) else {
            return Err(AnalyzerLaunchError::CommandUnavailable {
                command: command.to_owned(),
            });
        };
        let Some(rustup) = rustup_proxy_owner(&located) else {
            return Ok(AnalyzerLaunch::direct(command));
        };
        run_blocking_probe_section(|| {
            self.require_no_install_rustup(&rustup, command)?;
            resolve_through_rustup(&rustup, command, workspace_root)
        })
    }

    /// Refuses a rustup that ignores [`RUSTUP_AUTO_INSTALL_ENV`] before any
    /// toolchain-resolving probe runs through it.
    fn require_no_install_rustup(
        &mut self,
        rustup: &Path,
        command: &str,
    ) -> Result<(), AnalyzerLaunchError> {
        let version = if let Some(version) = self.rustup_versions.get(rustup) {
            *version
        } else {
            let version = rustup_version(rustup, command)?;
            self.rustup_versions.insert(rustup.to_path_buf(), version);
            version
        };
        if version < MIN_RUSTUP_VERSION_FOR_NO_INSTALL {
            return Err(AnalyzerLaunchError::RustupTooOldForNoInstall { found: version });
        }
        Ok(())
    }
}

/// Resolves `command` for `workspace_root` with a fresh resolver. Callers that
/// resolve repeatedly hold an [`AnalyzerLaunchResolver`] instead.
pub fn resolve_analyzer_launch(
    command: &str,
    workspace_root: &Path,
) -> Result<AnalyzerLaunch, AnalyzerLaunchError> {
    AnalyzerLaunchResolver::new().resolve(command, workspace_root)
}

/// `rustup --version`, run from the system temporary directory so no project
/// override is in scope: rustup follows the version line by resolving the
/// active toolchain, which an old rustup would install.
fn rustup_version(rustup: &Path, command: &str) -> Result<RustupVersion, AnalyzerLaunchError> {
    let mut probe = Command::new(rustup);
    probe
        .arg("--version")
        .current_dir(std::env::temp_dir())
        .env(RUSTUP_AUTO_INSTALL_ENV, RUSTUP_AUTO_INSTALL_DISABLED);
    let output = run_bounded(probe, command)?;
    RustupVersion::parse_output(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        AnalyzerLaunchError::ProbeFailed {
            command: command.to_owned(),
            reason: "rustup --version reported no version".to_owned(),
        }
    })
}

fn resolve_through_rustup(
    rustup: &Path,
    command: &str,
    workspace_root: &Path,
) -> Result<AnalyzerLaunch, AnalyzerLaunchError> {
    let mut probe = Command::new(rustup);
    probe
        .arg("which")
        .arg(command_stem(command))
        .current_dir(workspace_root)
        .env(RUSTUP_AUTO_INSTALL_ENV, RUSTUP_AUTO_INSTALL_DISABLED);
    let output = run_bounded(probe, command)?;
    if !output.status.success() {
        return Err(classify_which_refusal(
            &String::from_utf8_lossy(&output.stderr),
            command,
        ));
    }
    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if resolved.is_empty() || !Path::new(&resolved).is_file() {
        return Err(AnalyzerLaunchError::ProbeFailed {
            command: command.to_owned(),
            reason: "rustup which reported no executable".to_owned(),
        });
    }
    Ok(AnalyzerLaunch {
        program: PathBuf::from(resolved),
        env: no_install_env(),
    })
}

/// Classifies a failed `rustup which` from its stderr without repeating it.
/// rustup 1.29 prints `error: unknown binary '<name>' in toolchain '<tc>'`
/// for a missing component and `error: toolchain '<tc>' is not installed`
/// for a missing toolchain.
fn classify_which_refusal(stderr: &str, command: &str) -> AnalyzerLaunchError {
    let command = command.to_owned();
    if stderr.contains("unknown binary") {
        AnalyzerLaunchError::NotInstalledForToolchain { command }
    } else if stderr.contains("is not installed") {
        AnalyzerLaunchError::ToolchainNotInstalled { command }
    } else {
        AnalyzerLaunchError::ProbeFailed {
            command,
            reason: "rustup which refused without a recognised reason".to_owned(),
        }
    }
}

struct ProbeOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Runs `probe` to completion or kills it at [`RUSTUP_PROBE_DEADLINE`].
///
/// stdout and stderr are drained on their own threads so a chatty child can
/// never block on a full pipe while the deadline is polled.
fn run_bounded(mut probe: Command, command: &str) -> Result<ProbeOutput, AnalyzerLaunchError> {
    let probe_failed = |reason: String| AnalyzerLaunchError::ProbeFailed {
        command: command.to_owned(),
        reason,
    };
    let mut child = probe
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| probe_failed(error.kind().to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| probe_failed("probe stdout was not captured".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| probe_failed("probe stderr was not captured".to_owned()))?;
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    };
    let stdout = drain(Box::new(stdout));
    let stderr = drain(Box::new(stderr));
    let deadline = Instant::now() + RUSTUP_PROBE_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AnalyzerLaunchError::ProbeTimedOut {
                    command: command.to_owned(),
                });
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(probe_failed("probe status could not be read".to_owned()));
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(ProbeOutput {
        status,
        stdout,
        stderr,
    })
}

/// Runs the synchronous rustup probes without stalling the async worker that
/// called into the broker.
///
/// The broker is deliberately synchronous and its callers hold its lock on
/// Tokio workers. `block_in_place` hands the worker's run queue to another
/// thread for the section; it panics outside a multi-thread runtime, so the
/// flavor is checked first and everything else (current-thread runtimes, plain
/// threads) runs the section inline.
fn run_blocking_probe_section<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

fn no_install_env() -> Vec<(String, String)> {
    vec![(
        RUSTUP_AUTO_INSTALL_ENV.to_owned(),
        RUSTUP_AUTO_INSTALL_DISABLED.to_owned(),
    )]
}

/// The binary name rustup dispatches on (`rust-analyzer` for a
/// `/path/rust-analyzer.exe` command).
fn command_stem(command: &str) -> &str {
    let file_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    file_name
        .strip_suffix(std::env::consts::EXE_SUFFIX)
        .filter(|_| !std::env::consts::EXE_SUFFIX.is_empty())
        .unwrap_or(file_name)
}

/// Whether `command` names an executable file, on PATH or by path.
pub fn command_available(command: &str) -> bool {
    locate_command(command).is_some()
}

fn locate_command(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    let paths = std::env::var_os("PATH")?;
    let candidates = command_candidates(command);
    std::env::split_paths(&paths).find_map(|directory| {
        candidates
            .iter()
            .map(|candidate| directory.join(candidate))
            .find(|candidate| candidate.is_file())
    })
}

/// The `rustup` binary owning `located` when it is a rustup proxy.
///
/// rustup installs proxies as symlinks, hard links, or byte copies of itself
/// beside the `rustup` binary, so any of those relations to a sibling
/// `rustup` identifies one. The check runs on the canonical target of
/// `located`: a distro symlink such as `/usr/local/bin/rust-analyzer ->
/// ~/.cargo/bin/rust-analyzer` has no `rustup` beside it, but its target
/// does. A real analyzer dropped into the same directory matches none of
/// the relations.
fn rustup_proxy_owner(located: &Path) -> Option<PathBuf> {
    let target = located
        .canonicalize()
        .unwrap_or_else(|_| located.to_path_buf());
    let rustup = target
        .parent()?
        .join(format!("rustup{}", std::env::consts::EXE_SUFFIX));
    let rustup_metadata = std::fs::metadata(&rustup).ok()?;
    if !rustup_metadata.is_file() {
        return None;
    }
    let target_metadata = std::fs::metadata(&target).ok()?;
    let same_canonical = matches!(rustup.canonicalize(), Ok(canonical) if canonical == target);
    let same_bytes = target_metadata.len() == rustup_metadata.len();
    (same_canonical || same_bytes || same_inode(&target_metadata, &rustup_metadata))
        .then_some(rustup)
}

#[cfg(unix)]
fn same_inode(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_inode(_left: &std::fs::Metadata, _right: &std::fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn command_candidates(command: &str) -> Vec<String> {
    if Path::new(command).extension().is_some() {
        return vec![command.to_string()];
    }

    let pathext = std::env::var_os("PATHEXT").map_or_else(
        || ".COM;.EXE;.BAT;.CMD".to_string(),
        |value| value.to_string_lossy().into_owned(),
    );

    let mut candidates = vec![command.to_string()];
    candidates.extend(pathext.split(';').filter_map(|extension| {
        let extension = extension.trim();
        if extension.is_empty() {
            None
        } else if extension.starts_with('.') {
            Some(format!("{command}{extension}"))
        } else {
            Some(format!("{command}.{extension}"))
        }
    }));
    candidates
}

#[cfg(not(windows))]
fn command_candidates(command: &str) -> Vec<String> {
    vec![command.to_string()]
}

/// A recording stand-in for a rustup installation, shared by the launch and
/// broker tests: `rustup` is a shell script that logs its argv, environment,
/// and working directory, answers `--version`, and `rust-analyzer` is a
/// symlink to it exactly as rustup installs its proxies.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod fake_rustup {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// The version every fake reports unless a test asks for another.
    pub(crate) const CURRENT_VERSION: &str = "1.29.0";

    /// `rustup which` answers from the working directory, as rustup resolves
    /// a toolchain override from where it is run: it prints the contents of
    /// `PER_ROOT_ANALYZER_FILE` in `$PWD` when that file exists and otherwise
    /// fails like rustup does for an uninstalled toolchain.
    pub(crate) const PER_ROOT_ANALYZER_FILE: &str = ".analyzer-for-toolchain";

    /// How the fake answers `rustup which`.
    pub(crate) enum Which {
        /// Prints this path.
        Resolved(std::path::PathBuf),
        /// rustup 1.29's refusal for an installed toolchain without the
        /// component.
        ComponentMissing,
        /// rustup 1.29's refusal for a toolchain that is not installed.
        ToolchainMissing,
        /// Reads `PER_ROOT_ANALYZER_FILE` from `$PWD`.
        PerRoot,
        /// Never answers; the caller must enforce its deadline.
        Hang,
    }

    /// With `resolved` set, `rustup which` succeeds and prints that path;
    /// without it, it fails the way rustup does for an installed toolchain
    /// that lacks the component.
    pub(crate) fn install(resolved: Option<&Path>) -> tempfile::TempDir {
        install_with(
            CURRENT_VERSION,
            resolved.map_or(Which::ComponentMissing, |path| {
                Which::Resolved(path.to_path_buf())
            }),
        )
    }

    pub(crate) fn install_per_root() -> tempfile::TempDir {
        install_with(CURRENT_VERSION, Which::PerRoot)
    }

    pub(crate) fn install_with(version: &str, which: Which) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("invocations.log");
        let which_body = match which {
            Which::Resolved(path) => format!("printf '%s\\n' '{}'\nexit 0\n", path.display()),
            Which::ComponentMissing => {
                "echo \"error: unknown binary 'rust-analyzer' in toolchain '1.95.0-x86_64-unknown-linux-gnu'\" >&2\nexit 1\n".to_owned()
            }
            Which::ToolchainMissing => {
                "echo \"info: syncing channel updates for 1.95.0\" >&2\necho \"error: toolchain '1.95.0-x86_64-unknown-linux-gnu' is not installed\" >&2\necho \"help: run \\`rustup toolchain install\\` to install it\" >&2\nexit 1\n".to_owned()
            }
            Which::PerRoot => format!(
                "if [ -f \"$PWD/{PER_ROOT_ANALYZER_FILE}\" ]; then cat \"$PWD/{PER_ROOT_ANALYZER_FILE}\"; exit 0; fi\necho \"error: toolchain '1.95.0-x86_64-unknown-linux-gnu' is not installed\" >&2\nexit 1\n"
            ),
            Which::Hang => "sleep 60\nexit 0\n".to_owned(),
        };
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" \"AUTO_INSTALL=${{RUSTUP_AUTO_INSTALL-unset}}\" \"PWD=$PWD\" >> '{}'\nif [ \"$1\" = \"--version\" ]; then echo \"rustup {version} (fake 2026-01-01)\"; exit 0; fi\nif [ \"$1\" = \"which\" ]; then\n{which_body}fi\necho \"proxy ran\" >&2\nexit 2\n",
            record.display()
        );
        let rustup = dir.path().join("rustup");
        std::fs::write(&rustup, body).unwrap();
        std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&rustup, dir.path().join("rust-analyzer")).unwrap();
        dir
    }

    pub(crate) fn invocations(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("invocations.log")).unwrap_or_default()
    }

    /// Lines of the invocation log that ran `rustup which`.
    pub(crate) fn which_probes(dir: &Path) -> usize {
        invocations(dir)
            .lines()
            .filter(|line| line.contains(" which "))
            .count()
    }

    /// Lines of the invocation log that ran `rustup --version`.
    pub(crate) fn version_probes(dir: &Path) -> usize {
        invocations(dir)
            .lines()
            .filter(|line| line.contains(" --version"))
            .count()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn direct_command_carries_the_no_install_environment() {
        let launch = AnalyzerLaunch::direct("typescript-language-server");
        assert_eq!(launch.program, PathBuf::from("typescript-language-server"));
        assert_eq!(
            launch.env,
            vec![("RUSTUP_AUTO_INSTALL".to_owned(), "0".to_owned())]
        );
    }

    #[test]
    fn missing_command_is_typed_unavailable() {
        assert_eq!(
            resolve_analyzer_launch("__tracedecay_missing_lsp_for_test__", Path::new(".")),
            Err(AnalyzerLaunchError::CommandUnavailable {
                command: "__tracedecay_missing_lsp_for_test__".to_owned(),
            })
        );
    }

    #[test]
    fn rustup_version_line_parses_and_orders() {
        assert_eq!(
            RustupVersion::parse_output("rustup 1.29.0 (28d1352db 2026-03-05)\ninfo: more\n"),
            Some(RustupVersion {
                major: 1,
                minor: 29,
                patch: 0
            })
        );
        assert_eq!(RustupVersion::parse_output("rustc 1.97.1"), None);
        assert!(
            RustupVersion {
                major: 1,
                minor: 28,
                patch: 0
            } < MIN_RUSTUP_VERSION_FOR_NO_INSTALL
        );
        assert!(
            RustupVersion {
                major: 1,
                minor: 28,
                patch: 1
            } >= MIN_RUSTUP_VERSION_FOR_NO_INSTALL
        );
    }

    #[test]
    fn which_refusals_classify_from_stderr_without_repeating_it() {
        let component = classify_which_refusal(
            "error: unknown binary 'rust-analyzer' in toolchain '1.95.0-x86_64-unknown-linux-gnu'\n",
            "rust-analyzer",
        );
        assert_eq!(
            component,
            AnalyzerLaunchError::NotInstalledForToolchain {
                command: "rust-analyzer".to_owned()
            }
        );
        let toolchain = classify_which_refusal(
            "info: syncing channel updates for 1.95.0\nerror: toolchain '1.95.0-x86_64-unknown-linux-gnu' is not installed\nhelp: run `rustup toolchain install` to install it\n",
            "rust-analyzer",
        );
        assert_eq!(
            toolchain,
            AnalyzerLaunchError::ToolchainNotInstalled {
                command: "rust-analyzer".to_owned()
            }
        );
        let unknown = classify_which_refusal("error: something else entirely\n", "rust-analyzer");
        assert!(matches!(unknown, AnalyzerLaunchError::ProbeFailed { .. }));
        for error in [component, toolchain, unknown] {
            let detail = error.engine_error();
            assert!(!detail.contains('\n'), "{detail}");
            assert!(!detail.contains("x86_64"), "{detail}");
            assert!(!detail.contains("something else"), "{detail}");
        }
    }

    #[test]
    fn every_refusal_is_one_sentence_without_stderr() {
        let not_installed = AnalyzerLaunchError::NotInstalledForToolchain {
            command: "rust-analyzer".to_owned(),
        };
        assert_eq!(
            not_installed.to_string(),
            NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE
        );
        let detail = not_installed.engine_error();
        assert!(detail.starts_with(NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE));
        assert!(detail.contains("rustup component add rust-analyzer"));

        let toolchain = AnalyzerLaunchError::ToolchainNotInstalled {
            command: "rust-analyzer".to_owned(),
        }
        .engine_error();
        assert!(toolchain.starts_with(TOOLCHAIN_NOT_INSTALLED_MESSAGE));
        assert!(toolchain.contains("rustup toolchain install"));
        assert!(!toolchain.contains("component add"));

        let too_old = AnalyzerLaunchError::RustupTooOldForNoInstall {
            found: RustupVersion {
                major: 1,
                minor: 27,
                patch: 1,
            },
        }
        .engine_error();
        assert!(too_old.contains("rustup 1.27.1"), "{too_old}");
        assert!(too_old.contains("1.28.1"), "{too_old}");
        assert!(too_old.contains("rustup self update"), "{too_old}");

        let timed_out = AnalyzerLaunchError::ProbeTimedOut {
            command: "rust-analyzer".to_owned(),
        }
        .engine_error();
        assert!(
            timed_out.contains("did not answer within 3 seconds"),
            "{timed_out}"
        );

        for detail in [detail, toolchain, too_old, timed_out] {
            assert!(!detail.contains('\n'), "{detail}");
            assert!(!detail.contains("syncing channel"), "{detail}");
        }
    }

    #[cfg(unix)]
    mod unix {
        use super::super::fake_rustup::{self, Which, install as fake_rustup_dir, invocations};
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        #[test]
        fn proxy_without_component_is_typed_and_probed_without_install() {
            let dir = fake_rustup_dir(None);
            let project = tempfile::tempdir().unwrap();
            let command = dir.path().join("rust-analyzer");

            let error = resolve_analyzer_launch(command.to_str().unwrap(), project.path())
                .expect_err("missing component must not resolve a launch");

            assert_eq!(
                error,
                AnalyzerLaunchError::NotInstalledForToolchain {
                    command: command.to_string_lossy().into_owned(),
                }
            );
            let log = invocations(dir.path());
            assert!(log.contains("which rust-analyzer"), "{log}");
            assert!(!log.contains("AUTO_INSTALL=unset"), "{log}");
            assert!(
                log.contains(&format!(
                    "PWD={}",
                    project.path().canonicalize().unwrap().display()
                )),
                "{log}"
            );
            assert!(!error.engine_error().contains("syncing channel"));
        }

        #[test]
        fn proxy_with_absent_toolchain_is_typed_toolchain_not_installed() {
            let dir =
                fake_rustup::install_with(fake_rustup::CURRENT_VERSION, Which::ToolchainMissing);
            let project = tempfile::tempdir().unwrap();
            let command = dir.path().join("rust-analyzer");

            let error = resolve_analyzer_launch(command.to_str().unwrap(), project.path())
                .expect_err("missing toolchain must not resolve a launch");

            assert_eq!(
                error,
                AnalyzerLaunchError::ToolchainNotInstalled {
                    command: command.to_string_lossy().into_owned(),
                }
            );
            let detail = error.engine_error();
            assert!(detail.contains("rustup toolchain install"), "{detail}");
            assert!(!detail.contains("component add"), "{detail}");
            assert!(!detail.contains("syncing channel"), "{detail}");
        }

        #[test]
        fn proxy_with_component_launches_the_real_binary() {
            let real = tempfile::tempdir().unwrap();
            let real_binary = real.path().join("rust-analyzer");
            std::fs::write(&real_binary, "#!/bin/sh\nexit 0\n").unwrap();
            let dir = fake_rustup_dir(Some(&real_binary));
            let project = tempfile::tempdir().unwrap();
            let command = dir.path().join("rust-analyzer");

            let launch = resolve_analyzer_launch(command.to_str().unwrap(), project.path())
                .expect("installed component resolves");

            assert_eq!(launch.program, real_binary);
            assert_eq!(
                launch.env,
                vec![("RUSTUP_AUTO_INSTALL".to_owned(), "0".to_owned())]
            );
            let log = invocations(dir.path());
            assert!(!log.contains("AUTO_INSTALL=unset"), "{log}");
            assert!(
                log.contains(" --version") && log.contains(" which rust-analyzer"),
                "{log}"
            );
        }

        #[test]
        fn old_rustup_is_refused_before_any_which_probe() {
            let dir = fake_rustup::install_with(
                "1.27.1",
                Which::Resolved(PathBuf::from("/never/consulted")),
            );
            let project = tempfile::tempdir().unwrap();
            let command = dir.path().join("rust-analyzer");
            let mut resolver = AnalyzerLaunchResolver::new();

            let error = resolver
                .resolve(command.to_str().unwrap(), project.path())
                .expect_err("old rustup must be refused");

            assert_eq!(
                error,
                AnalyzerLaunchError::RustupTooOldForNoInstall {
                    found: RustupVersion {
                        major: 1,
                        minor: 27,
                        patch: 1,
                    },
                }
            );
            assert_eq!(fake_rustup::which_probes(dir.path()), 0);
            assert_eq!(fake_rustup::version_probes(dir.path()), 1);
            let log = invocations(dir.path());
            assert!(!log.contains("AUTO_INSTALL=unset"), "{log}");
            assert!(
                !log.contains(&format!(
                    "PWD={}",
                    project.path().canonicalize().unwrap().display()
                )),
                "the version probe must not run inside the project: {log}"
            );

            let _ = resolver.resolve(command.to_str().unwrap(), project.path());
            assert_eq!(
                fake_rustup::version_probes(dir.path()),
                1,
                "the version is remembered per rustup binary"
            );
        }

        #[test]
        fn resolver_remembers_the_rustup_version_across_roots() {
            let real = tempfile::tempdir().unwrap();
            let real_binary = real.path().join("rust-analyzer");
            std::fs::write(&real_binary, "").unwrap();
            let dir = fake_rustup_dir(Some(&real_binary));
            let command = dir.path().join("rust-analyzer");
            let root_a = tempfile::tempdir().unwrap();
            let root_b = tempfile::tempdir().unwrap();
            let mut resolver = AnalyzerLaunchResolver::new();

            resolver
                .resolve(command.to_str().unwrap(), root_a.path())
                .unwrap();
            resolver
                .resolve(command.to_str().unwrap(), root_b.path())
                .unwrap();

            assert_eq!(fake_rustup::version_probes(dir.path()), 1);
            assert_eq!(fake_rustup::which_probes(dir.path()), 2);
            resolver.clear();
            resolver
                .resolve(command.to_str().unwrap(), root_a.path())
                .unwrap();
            assert_eq!(fake_rustup::version_probes(dir.path()), 2);
        }

        #[test]
        fn hanging_probe_is_killed_at_the_deadline_and_typed() {
            let dir = fake_rustup::install_with(fake_rustup::CURRENT_VERSION, Which::Hang);
            let project = tempfile::tempdir().unwrap();
            let command = dir.path().join("rust-analyzer");
            let started = Instant::now();

            let error = resolve_analyzer_launch(command.to_str().unwrap(), project.path())
                .expect_err("a hanging probe must be refused");

            assert_eq!(
                error,
                AnalyzerLaunchError::ProbeTimedOut {
                    command: command.to_string_lossy().into_owned(),
                }
            );
            let elapsed = started.elapsed();
            assert!(
                elapsed >= RUSTUP_PROBE_DEADLINE && elapsed < RUSTUP_PROBE_DEADLINE * 3,
                "bounded by the deadline, not the child's sleep: {elapsed:?}"
            );
            assert!(!error.engine_error().contains('\n'));
        }

        #[test]
        fn real_binary_beside_rustup_is_not_a_proxy() {
            let dir = fake_rustup_dir(None);
            let real = dir.path().join("gopls");
            std::fs::write(&real, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();

            let launch = resolve_analyzer_launch(real.to_str().unwrap(), dir.path())
                .expect("a non-proxy command launches directly");

            assert_eq!(launch.program, real);
            assert!(invocations(dir.path()).is_empty());
        }

        #[test]
        fn symlink_from_another_directory_to_a_proxy_is_still_a_proxy() {
            let dir = fake_rustup_dir(None);
            let elsewhere = tempfile::tempdir().unwrap();
            let distro_link = elsewhere.path().join("rust-analyzer");
            std::os::unix::fs::symlink(dir.path().join("rust-analyzer"), &distro_link).unwrap();
            let project = tempfile::tempdir().unwrap();

            let error = resolve_analyzer_launch(distro_link.to_str().unwrap(), project.path())
                .expect_err("the link resolves to the proxy, which must be probed");

            assert_eq!(
                error,
                AnalyzerLaunchError::NotInstalledForToolchain {
                    command: distro_link.to_string_lossy().into_owned(),
                }
            );
            assert_eq!(fake_rustup::which_probes(dir.path()), 1);
        }

        #[test]
        fn byte_copy_of_rustup_is_a_proxy() {
            let real = tempfile::tempdir().unwrap();
            let real_binary = real.path().join("rust-analyzer");
            std::fs::write(&real_binary, "").unwrap();
            let dir = fake_rustup_dir(Some(&real_binary));
            let copied = dir.path().join("rust-analyzer-copy");
            std::fs::copy(dir.path().join("rustup"), &copied).unwrap();
            assert!(
                std::fs::symlink_metadata(&copied)
                    .unwrap()
                    .file_type()
                    .is_file(),
                "the fixture is a byte copy, not a link"
            );
            let project = tempfile::tempdir().unwrap();

            let launch = resolve_analyzer_launch(copied.to_str().unwrap(), project.path())
                .expect("the copied proxy resolves through rustup");

            assert_eq!(launch.program, real_binary);
            let log = invocations(dir.path());
            assert!(log.contains(" which rust-analyzer-copy"), "{log}");
        }
    }
}
