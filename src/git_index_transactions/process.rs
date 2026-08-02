use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tracedecay_application::DirectorySyncPolicy;
use tracedecay_domain::{GitFileModeV1, GitOidV1, GitOperationStateV1};

use super::NativeGitIndexError;
use super::patch::ValidatedIndexPatch;

pub(super) fn joined_patch_bytes(patches: &[ValidatedIndexPatch]) -> Vec<u8> {
    let mut patch_bytes = Vec::new();
    for patch in patches {
        patch_bytes.extend_from_slice(patch.bytes());
        if !patch_bytes.ends_with(b"\n") {
            patch_bytes.push(b'\n');
        }
    }
    patch_bytes
}

pub(super) fn git_command(repository_root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(repository_root);
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

pub(super) fn run_git_at(
    repository_root: &Path,
    operation: &'static str,
    args: &[&str],
) -> Result<String, NativeGitIndexError> {
    let output = git_command(repository_root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
    if !output.status.success() {
        return Err(NativeGitIndexError::GitFailed {
            operation,
            status: output.status.to_string(),
        });
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| NativeGitIndexError::MalformedOutput { operation })
}

pub(super) fn run_command_with_stdin(
    mut command: Command,
    operation: &'static str,
    input: &[u8],
) -> Result<Output, NativeGitIndexError> {
    let mut child = command
        .spawn()
        .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(NativeGitIndexError::Io(
            "native Git stdin was not available".to_owned(),
        ));
    };
    stdin
        .write_all(input)
        .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(NativeGitIndexError::GitFailed {
            operation,
            status: output.status.to_string(),
        })
    }
}

pub(super) fn absolute_git_path(
    repository_root: &Path,
    value: &str,
) -> Result<PathBuf, NativeGitIndexError> {
    if value.is_empty() {
        return Err(NativeGitIndexError::MalformedOutput {
            operation: "rev-parse",
        });
    }
    let path = PathBuf::from(value);
    Ok(if path.is_absolute() {
        path
    } else {
        repository_root.join(path)
    })
}

pub(super) fn read_optional_file(path: &Path) -> Result<Vec<u8>, NativeGitIndexError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(NativeGitIndexError::Io(error.to_string())),
    }
}

pub(super) fn parse_git_oid(
    operation: &'static str,
    output: &[u8],
) -> Result<GitOidV1, NativeGitIndexError> {
    let text = std::str::from_utf8(output)
        .map_err(|_| NativeGitIndexError::MalformedOutput { operation })?;
    GitOidV1::new(text.trim()).map_err(|_| NativeGitIndexError::MalformedOutput { operation })
}

pub(super) fn git_timestamp(micros: i64) -> String {
    format!("@{} +0000", micros.div_euclid(1_000_000))
}

pub(super) fn current_operation_state(git_dir: &Path) -> GitOperationStateV1 {
    if git_dir.join("MERGE_HEAD").is_file() {
        GitOperationStateV1::Merge
    } else if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir() {
        GitOperationStateV1::Rebase
    } else if git_dir.join("CHERRY_PICK_HEAD").is_file() {
        GitOperationStateV1::CherryPick
    } else if git_dir.join("REVERT_HEAD").is_file() {
        GitOperationStateV1::Revert
    } else if git_dir.join("BISECT_LOG").is_file() {
        GitOperationStateV1::Bisect
    } else if git_dir.join("sequencer").is_dir() {
        GitOperationStateV1::Sequencer
    } else {
        GitOperationStateV1::None
    }
}

pub(super) fn sync_parent_directory(path: &Path) -> Result<(), NativeGitIndexError> {
    let parent = path
        .parent()
        .ok_or_else(|| NativeGitIndexError::Io("Git index has no parent directory".to_owned()))?;
    tracedecay_application::sync_directory(parent, DirectorySyncPolicy::Strict)
        .map_err(|error| NativeGitIndexError::Io(error.to_string()))
}

pub(super) fn worktree_mode(path: &Path) -> Option<GitFileModeV1> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let mode = if metadata.file_type().is_symlink() {
        GitFileModeV1::SYMLINK
    } else if metadata.is_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 != 0 {
                GitFileModeV1::EXECUTABLE
            } else {
                GitFileModeV1::REGULAR
            }
        }
        #[cfg(not(unix))]
        {
            GitFileModeV1::REGULAR
        }
    } else {
        return None;
    };
    GitFileModeV1::new(mode).ok()
}

#[cfg(unix)]
pub(super) fn is_executable_hook(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.is_file()
        && std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub(super) fn is_executable_hook(path: &Path) -> bool {
    path.is_file()
}
