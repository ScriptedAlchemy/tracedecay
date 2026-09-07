//! Executable lookup on a `PATH`-shaped search list.
//!
//! Automation's backend probes and the host installers both need "the first
//! executable spelling of `program` on this `PATH`"; the answer depends on
//! nothing host-owned, so it lives here rather than behind a host port.

use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};

/// First executable match for `program` across `path_var`.
pub fn resolve_on_path(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
) -> Result<Option<PathBuf>> {
    let Some(path_var) = path_var else {
        return Ok(None);
    };
    for dir in std::env::split_paths(path_var) {
        for name in candidate_file_names(program) {
            let candidate = dir.join(&name);
            if is_executable_file(&candidate)? {
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

/// Executable spellings to try for a bare program name.
fn candidate_file_names(program: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{program}.exe"),
            format!("{program}.cmd"),
            format!("{program}.bat"),
            program.to_string(),
        ]
    } else {
        vec![program.to_string()]
    }
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => Ok(false),
        Ok(metadata) if metadata.permissions().mode() & 0o111 != 0 => Ok(true),
        Ok(_) => Err(TraceDecayError::Config {
            message: format!(
                "host CLI candidate `{}` exists but is not executable",
                path.display()
            ),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(TraceDecayError::Io(error)),
    }
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(TraceDecayError::Io(error)),
    }
}
