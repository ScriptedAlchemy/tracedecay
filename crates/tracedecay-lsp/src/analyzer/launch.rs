//! Resolves the executable an analyzer spawn may run without mutating the host.
//!
//! A configured command such as `rust-analyzer` is usually a rustup proxy. The
//! proxy resolves the project's active toolchain from the spawn directory and,
//! by default, downloads that toolchain when it is missing — so opening a
//! dashboard on a project whose parent pins an uninstalled toolchain started a
//! network install into the rustup home. Every spawn therefore goes through
//! [`resolve_analyzer_launch`]: a proxy is replaced by the real toolchain
//! binary `rustup which` reports (with auto-install disabled), a missing
//! component is a typed [`AnalyzerLaunchError::NotInstalledForToolchain`], and
//! the spawned environment always carries [`RUSTUP_AUTO_INSTALL_ENV`]`=0` so a
//! child `cargo` the analyzer runs cannot install either.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// rustup honours this variable since 1.28.1; `0` refuses every implicit
/// toolchain install, from proxies and from `rustup which` alike.
pub const RUSTUP_AUTO_INSTALL_ENV: &str = "RUSTUP_AUTO_INSTALL";
pub const RUSTUP_AUTO_INSTALL_DISABLED: &str = "0";

/// Operator-facing sentence for a proxy whose toolchain lacks the analyzer.
pub const NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE: &str =
    "rust-analyzer is not installed for this toolchain";

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

/// Why no analyzer may be started for a configured command.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnalyzerLaunchError {
    #[error("LSP command '{command}' is not available on PATH")]
    CommandUnavailable { command: String },
    /// The command is a rustup proxy and the toolchain active in the project
    /// root has no such component. No install was attempted.
    #[error("{NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE}")]
    NotInstalledForToolchain { command: String },
    /// `rustup which` could not be run or answered without a path.
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
            Self::CommandUnavailable { .. } | Self::ProbeFailed { .. } => self.to_string(),
        }
    }
}

/// Resolves how `command` is started for a project rooted at `project_root`.
///
/// The probe runs from `project_root` because that is where rustup reads the
/// active toolchain override a spawn would see.
pub fn resolve_analyzer_launch(
    command: &str,
    project_root: &Path,
) -> Result<AnalyzerLaunch, AnalyzerLaunchError> {
    let Some(located) = locate_command(command) else {
        return Err(AnalyzerLaunchError::CommandUnavailable {
            command: command.to_owned(),
        });
    };
    let Some(rustup) = rustup_proxy_owner(&located) else {
        return Ok(AnalyzerLaunch::direct(command));
    };
    let output = Command::new(&rustup)
        .arg("which")
        .arg(command_stem(command))
        .current_dir(project_root)
        .env(RUSTUP_AUTO_INSTALL_ENV, RUSTUP_AUTO_INSTALL_DISABLED)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| AnalyzerLaunchError::ProbeFailed {
            command: command.to_owned(),
            reason: error.kind().to_string(),
        })?;
    if !output.status.success() {
        return Err(AnalyzerLaunchError::NotInstalledForToolchain {
            command: command.to_owned(),
        });
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
/// `rustup` identifies one. A real analyzer dropped into the same directory
/// matches none of them.
fn rustup_proxy_owner(located: &Path) -> Option<PathBuf> {
    let rustup = located
        .parent()?
        .join(format!("rustup{}", std::env::consts::EXE_SUFFIX));
    let rustup_metadata = std::fs::metadata(&rustup).ok()?;
    if !rustup_metadata.is_file() {
        return None;
    }
    let located_metadata = std::fs::metadata(located).ok()?;
    let same_canonical = matches!(
        (located.canonicalize(), rustup.canonicalize()),
        (Ok(left), Ok(right)) if left == right
    );
    let same_bytes = located_metadata.len() == rustup_metadata.len();
    (same_canonical || same_bytes || same_inode(&located_metadata, &rustup_metadata))
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
/// and working directory, and `rust-analyzer` is a symlink to it exactly as
/// rustup installs its proxies.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod fake_rustup {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// With `resolved` set, `rustup which` succeeds and prints that path;
    /// without it, it fails the way rustup does for an uninstalled toolchain.
    pub(crate) fn install(resolved: Option<&Path>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("invocations.log");
        let body = match resolved {
            Some(path) => format!(
                "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" \"AUTO_INSTALL=${{RUSTUP_AUTO_INSTALL-unset}}\" \"PWD=$PWD\" >> '{}'\nprintf '%s\\n' '{}'\n",
                record.display(),
                path.display()
            ),
            None => format!(
                "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" \"AUTO_INSTALL=${{RUSTUP_AUTO_INSTALL-unset}}\" \"PWD=$PWD\" >> '{}'\necho \"info: syncing channel updates for 1.95.0\" >&2\necho \"error: toolchain '1.95.0' is not installed\" >&2\nexit 1\n",
                record.display()
            ),
        };
        let rustup = dir.path().join("rustup");
        std::fs::write(&rustup, body).unwrap();
        std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&rustup, dir.path().join("rust-analyzer")).unwrap();
        dir
    }

    pub(crate) fn invocations(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("invocations.log")).unwrap_or_default()
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
    fn not_installed_engine_error_is_one_sentence_without_stderr() {
        let error = AnalyzerLaunchError::NotInstalledForToolchain {
            command: "rust-analyzer".to_owned(),
        };
        assert_eq!(error.to_string(), NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE);
        let engine_error = error.engine_error();
        assert!(engine_error.starts_with(NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE));
        assert!(engine_error.contains("rustup component add rust-analyzer"));
        assert!(!engine_error.contains('\n'));
    }

    #[cfg(unix)]
    mod unix {
        use super::super::fake_rustup::{install as fake_rustup_dir, invocations};
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
            assert!(log.contains("AUTO_INSTALL=0"), "{log}");
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
            assert!(invocations(dir.path()).contains("AUTO_INSTALL=0"));
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
    }
}
