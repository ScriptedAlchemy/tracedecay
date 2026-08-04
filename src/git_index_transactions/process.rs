use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use tracedecay_application::DirectorySyncPolicy;
use tracedecay_domain::{
    GitBlobExpectationV1, GitFileModeV1, GitIndexEntryExpectationV1, GitOidV1, GitOperationStateV1,
    HunkDirectionV1, HunkRefV1, canonical_sha256,
};
use tracedecay_runtime_core::git::{GitCommandBounds, bounded_command_output, git_program};

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

#[derive(Clone, Debug)]
pub(super) struct GitProcess {
    #[cfg(test)]
    spawned_commands: Arc<AtomicUsize>,
}

impl Default for GitProcess {
    fn default() -> Self {
        Self {
            #[cfg(test)]
            spawned_commands: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl GitProcess {
    pub(super) fn command(&self, repository_root: &Path) -> Command {
        let mut command = Command::new(git_program());
        command.current_dir(repository_root);
        for (key, _) in env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C");
        command
    }

    pub(super) fn output(
        &self,
        command: Command,
        input: Option<&[u8]>,
    ) -> Result<Output, NativeGitIndexError> {
        #[cfg(test)]
        self.spawned_commands.fetch_add(1, Ordering::Relaxed);
        bounded_command_output(command, input, &GitCommandBounds::default())
            .map_err(NativeGitIndexError::Process)
    }

    #[cfg(test)]
    pub(super) fn reset_spawned_command_count(&self) {
        self.spawned_commands.store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn spawned_command_count(&self) -> usize {
        self.spawned_commands.load(Ordering::Relaxed)
    }

    pub(super) fn verify_hunks(
        &self,
        repository_root: &Path,
        hunks: &[&HunkRefV1],
    ) -> Result<(), NativeGitIndexError> {
        let paths = hunks
            .iter()
            .map(|hunk| hunk.path.as_str())
            .collect::<BTreeSet<_>>();
        let index_entries = self.index_entries(repository_root, &paths)?;
        let head_paths = hunks
            .iter()
            .filter(|hunk| hunk.direction == HunkDirectionV1::IndexToHead)
            .map(|hunk| hunk.original_path.as_deref().unwrap_or(&hunk.path))
            .collect::<BTreeSet<_>>();
        let head_blobs = self.head_blobs(repository_root, &head_paths)?;
        let worktree_blobs = self.worktree_blobs(repository_root, hunks)?;
        let attributes = self.attribute_digests(repository_root, &paths)?;

        for hunk in hunks {
            let index_entry = index_entries
                .get(&hunk.path)
                .cloned()
                .unwrap_or_else(absent_index_entry);
            if index_entry != hunk.expected_index_entry {
                return Err(NativeGitIndexError::StaleRepositoryState);
            }
            let base = match hunk.direction {
                HunkDirectionV1::WorkingTreeToIndex => index_entry.blob,
                HunkDirectionV1::IndexToHead => head_blobs
                    .get(hunk.original_path.as_deref().unwrap_or(&hunk.path))
                    .cloned()
                    .unwrap_or(GitBlobExpectationV1::AbsentFile),
            };
            if base != hunk.expected_base_blob {
                return Err(NativeGitIndexError::StaleRepositoryState);
            }
            if let Some(expected) = &hunk.expected_worktree_blob
                && worktree_blobs.get(&hunk.path) != Some(expected)
            {
                return Err(NativeGitIndexError::StaleRepositoryState);
            }
            let path = repository_root.join(&hunk.path);
            if hunk.expected_worktree_blob.is_some()
                && worktree_mode(&path).as_ref() != hunk.expected_worktree_mode.as_ref()
            {
                return Err(NativeGitIndexError::StaleRepositoryState);
            }
            if hunk.attributes_digest.as_ref() != attributes.get(&hunk.path) {
                return Err(NativeGitIndexError::StaleRepositoryState);
            }
        }
        Ok(())
    }

    pub(super) fn paths_have_filters(
        &self,
        repository_root: &Path,
        paths: &BTreeSet<&str>,
    ) -> Result<bool, NativeGitIndexError> {
        let mut command = self.command(repository_root);
        command
            .args([
                "check-attr",
                "-z",
                "filter",
                "text",
                "eol",
                "working-tree-encoding",
                "--",
            ])
            .args(paths);
        let output = checked(self.output(command, None)?, "check-attr")?;
        let fields = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        let mut records = fields.chunks_exact(3);
        let has_filter = records.any(|record| {
            let value = record[2];
            value != b"unspecified" && value != b"unset" && value != b"false"
        });
        if !records.remainder().is_empty() {
            return Err(NativeGitIndexError::MalformedOutput {
                operation: "check-attr",
            });
        }
        Ok(has_filter)
    }

    fn index_entries(
        &self,
        repository_root: &Path,
        paths: &BTreeSet<&str>,
    ) -> Result<BTreeMap<String, GitIndexEntryExpectationV1>, NativeGitIndexError> {
        let mut command = self.command(repository_root);
        command.args(["ls-files", "-s", "-z", "--"]).args(paths);
        let output = checked(self.output(command, None)?, "ls-files")?;
        let mut entries = BTreeMap::<String, Vec<(GitFileModeV1, GitOidV1, u8)>>::new();
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|row| !row.is_empty())
        {
            let tab = record.iter().position(|byte| *byte == b'\t').ok_or(
                NativeGitIndexError::MalformedOutput {
                    operation: "ls-files",
                },
            )?;
            let metadata = std::str::from_utf8(&record[..tab]).map_err(|_| {
                NativeGitIndexError::MalformedOutput {
                    operation: "ls-files",
                }
            })?;
            let path = std::str::from_utf8(&record[tab + 1..])
                .map_err(|_| NativeGitIndexError::MalformedOutput {
                    operation: "ls-files",
                })?
                .to_owned();
            let mut fields = metadata.split_whitespace();
            let mode =
                GitFileModeV1::new(fields.next().ok_or(NativeGitIndexError::MalformedOutput {
                    operation: "ls-files",
                })?)?;
            let blob =
                GitOidV1::new(fields.next().ok_or(NativeGitIndexError::MalformedOutput {
                    operation: "ls-files",
                })?)?;
            let stage = fields
                .next()
                .and_then(|value| value.parse::<u8>().ok())
                .ok_or(NativeGitIndexError::MalformedOutput {
                    operation: "ls-files",
                })?;
            entries.entry(path).or_default().push((mode, blob, stage));
        }
        entries
            .into_iter()
            .map(|(path, entries)| {
                let (mode, blob, _) =
                    entries
                        .first()
                        .cloned()
                        .ok_or(NativeGitIndexError::MalformedOutput {
                            operation: "ls-files",
                        })?;
                Ok((
                    path,
                    GitIndexEntryExpectationV1 {
                        blob: GitBlobExpectationV1::Present(blob),
                        mode: Some(mode),
                        unmerged_stage: entries
                            .iter()
                            .map(|(_, _, stage)| *stage)
                            .find(|stage| *stage > 0),
                    },
                ))
            })
            .collect()
    }

    fn head_blobs(
        &self,
        repository_root: &Path,
        paths: &BTreeSet<&str>,
    ) -> Result<BTreeMap<String, GitBlobExpectationV1>, NativeGitIndexError> {
        if paths.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut command = self.command(repository_root);
        command.args(["ls-tree", "-rz", "HEAD", "--"]).args(paths);
        let output = checked(self.output(command, None)?, "ls-tree")?;
        let mut blobs = BTreeMap::new();
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|row| !row.is_empty())
        {
            let tab = record.iter().position(|byte| *byte == b'\t').ok_or(
                NativeGitIndexError::MalformedOutput {
                    operation: "ls-tree",
                },
            )?;
            let metadata = std::str::from_utf8(&record[..tab]).map_err(|_| {
                NativeGitIndexError::MalformedOutput {
                    operation: "ls-tree",
                }
            })?;
            let path = std::str::from_utf8(&record[tab + 1..])
                .map_err(|_| NativeGitIndexError::MalformedOutput {
                    operation: "ls-tree",
                })?
                .to_owned();
            let oid =
                metadata
                    .split_whitespace()
                    .nth(2)
                    .ok_or(NativeGitIndexError::MalformedOutput {
                        operation: "ls-tree",
                    })?;
            blobs.insert(path, GitBlobExpectationV1::Present(GitOidV1::new(oid)?));
        }
        Ok(blobs)
    }

    fn worktree_blobs(
        &self,
        repository_root: &Path,
        hunks: &[&HunkRefV1],
    ) -> Result<BTreeMap<String, GitBlobExpectationV1>, NativeGitIndexError> {
        let mut blobs = BTreeMap::new();
        let present = hunks
            .iter()
            .filter(|hunk| hunk.expected_worktree_blob.is_some())
            .map(|hunk| hunk.path.as_str())
            .collect::<BTreeSet<_>>();
        let mut existing = Vec::new();
        for path in present {
            if std::fs::symlink_metadata(repository_root.join(path)).is_ok() {
                existing.push(path);
            } else {
                blobs.insert(path.to_owned(), GitBlobExpectationV1::AbsentFile);
            }
        }
        if existing.is_empty() {
            return Ok(blobs);
        }
        let mut input = existing.join("\n").into_bytes();
        input.push(b'\n');
        let mut command = self.command(repository_root);
        command.args(["hash-object", "--stdin-paths"]);
        let output = checked(self.output(command, Some(&input))?, "hash-object")?;
        let oids = std::str::from_utf8(&output.stdout)
            .map_err(|_| NativeGitIndexError::MalformedOutput {
                operation: "hash-object",
            })?
            .lines()
            .collect::<Vec<_>>();
        if oids.len() != existing.len() {
            return Err(NativeGitIndexError::MalformedOutput {
                operation: "hash-object",
            });
        }
        for (path, oid) in existing.into_iter().zip(oids) {
            blobs.insert(
                path.to_owned(),
                GitBlobExpectationV1::Present(GitOidV1::new(oid)?),
            );
        }
        Ok(blobs)
    }

    fn attribute_digests(
        &self,
        repository_root: &Path,
        paths: &BTreeSet<&str>,
    ) -> Result<BTreeMap<String, tracedecay_domain::ManifestDigest>, NativeGitIndexError> {
        let mut command = self.command(repository_root);
        command.args(["check-attr", "-z", "-a", "--"]).args(paths);
        let output = checked(self.output(command, None)?, "check-attr")?;
        let fields = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        let mut chunks = fields.chunks_exact(3);
        let mut grouped = BTreeMap::<String, Vec<u8>>::new();
        for record in &mut chunks {
            let path = std::str::from_utf8(record[0])
                .map_err(|_| NativeGitIndexError::MalformedOutput {
                    operation: "check-attr",
                })?
                .to_owned();
            let bytes = grouped.entry(path).or_default();
            for field in record {
                bytes.extend_from_slice(field);
                bytes.push(0);
            }
        }
        if !chunks.remainder().is_empty() {
            return Err(NativeGitIndexError::MalformedOutput {
                operation: "check-attr",
            });
        }
        paths
            .iter()
            .map(|path| {
                let bytes = grouped.remove(*path).unwrap_or_default();
                canonical_sha256(&String::from_utf8_lossy(&bytes).into_owned())
                    .map(|digest| ((*path).to_owned(), digest))
                    .map_err(Into::into)
            })
            .collect()
    }
}

fn absent_index_entry() -> GitIndexEntryExpectationV1 {
    GitIndexEntryExpectationV1 {
        blob: GitBlobExpectationV1::AbsentFile,
        mode: None,
        unmerged_stage: None,
    }
}

fn checked(output: Output, operation: &'static str) -> Result<Output, NativeGitIndexError> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(NativeGitIndexError::GitFailed {
            operation,
            status: output.status.to_string(),
        })
    }
}

pub(super) fn run_git_at(
    process: &GitProcess,
    repository_root: &Path,
    operation: &'static str,
    args: &[&str],
) -> Result<String, NativeGitIndexError> {
    let mut command = process.command(repository_root);
    command.args(args);
    let output = process.output(command, None)?;
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
    process: &GitProcess,
    command: Command,
    operation: &'static str,
    input: &[u8],
) -> Result<Output, NativeGitIndexError> {
    let output = process.output(command, Some(input))?;
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
