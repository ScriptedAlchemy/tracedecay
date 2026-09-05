use std::env;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output};

use tracedecay_domain::{GitFileModeV1, GitOidV1, GitOperationStateV1};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_directory};
use tracedecay_runtime_core::path_safety::plain_host_path;

use super::NativeGitIndexError;
use super::patch::ValidatedIndexPatch;

pub fn joined_patch_bytes(patches: &[ValidatedIndexPatch]) -> Vec<u8> {
    let mut patch_bytes = Vec::new();
    for patch in patches {
        patch_bytes.extend_from_slice(patch.bytes());
        if !patch_bytes.ends_with(b"\n") {
            patch_bytes.push(b'\n');
        }
    }
    patch_bytes
}

/// Builds a `git` invocation rooted at `repository_root` with every inherited
/// `GIT_*` variable stripped.
///
/// Every path handed to git is spelled plainly first: this runtime resolves
/// paths with `fs::canonicalize`, which on Windows returns the `\\?\`
/// extended-length form that Git for Windows refuses to normalize.
pub fn git_command(repository_root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(plain_host_path(repository_root));
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

pub fn run_command_with_stdin(
    mut command: Command,
    operation: &'static str,
    input: &[u8],
) -> Result<Output, NativeGitIndexError> {
    let mut child = command
        .spawn()
        .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
    let Some(stdin) = child.stdin.take() else {
        return Err(NativeGitIndexError::Io(
            "native Git stdin was not available".to_owned(),
        ));
    };
    let mut stdin = hotpath::io!(stdin, label = "usecases.git_index_tx.git.stdin");
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

pub fn read_optional_file(path: &Path) -> Result<Vec<u8>, NativeGitIndexError> {
    match File::open(path) {
        Ok(file) => {
            let mut file = hotpath::io!(file, label = "usecases.git_index_tx.optional.file");
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
            Ok(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(NativeGitIndexError::Io(error.to_string())),
    }
}

pub fn parse_git_oid(
    operation: &'static str,
    output: &[u8],
) -> Result<GitOidV1, NativeGitIndexError> {
    let text = std::str::from_utf8(output)
        .map_err(|_| NativeGitIndexError::MalformedOutput { operation })?;
    GitOidV1::new(text.trim()).map_err(|_| NativeGitIndexError::MalformedOutput { operation })
}

pub fn current_operation_state(git_dir: &Path) -> GitOperationStateV1 {
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

#[hotpath::measure(label = "daemon.git.index_tx.fsync.directory")]
pub fn sync_parent_directory(path: &Path) -> Result<(), NativeGitIndexError> {
    let parent = path
        .parent()
        .ok_or_else(|| NativeGitIndexError::Io("Git index has no parent directory".to_owned()))?;
    sync_directory(parent, DirectorySyncPolicy::Strict)
        .map_err(|error| NativeGitIndexError::Io(error.to_string()))
}

pub fn worktree_mode(path: &Path) -> Option<GitFileModeV1> {
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
