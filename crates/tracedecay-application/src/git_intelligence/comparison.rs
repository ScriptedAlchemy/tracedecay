//! One selection authority for exact source consumers and structured Git diffs.

use std::collections::BTreeSet;
use std::fs::Metadata;
use std::io::Read;
use std::ops::Range;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

use tracedecay_contracts::git::is_canonical_repository_relative_path;
use tracedecay_contracts::retrieval::{GitContextComparisonV1, GitContextSourceConsistencyV1};
use tracedecay_domain::{GitStatusEntryV1, IndexPathPolicyV1};

use super::{
    EMPTY_TREE_SHA1, EMPTY_TREE_SHA256, GIT_HISTORICAL_BLOB_MAX_BYTES, GitChangeKindV1,
    GitCoverageV1, GitDegradationV1, GitDiffFileMetadata, GitDiffScopeV1, GitFileModeV1,
    GitIntelligenceError, GitObjectFormatV1, GitOidV1, NativeGitIntelligence,
    NativeHistoricalBlobReaderV1, RepositoryReadSnapshot, parse_diff_raw, worktree_mode,
};

pub enum GitChangeSelection {
    Diff(GitDiffScopeV1),
    HeadToWorktree,
}

pub struct GitChangeComparison {
    pub selection: GitContextComparisonV1,
    pub files: Vec<GitDiffFileMetadata>,
    pub coverage: GitCoverageV1,
    object_format: GitObjectFormatV1,
    trust_file_mode: bool,
    /// Raw HEAD diff cannot inspect these paths after their index removal,
    /// including rename origins. Their admitted source capture may prove a net
    /// no-op, unlike an ordinary selected modification that disappears between
    /// selection and capture.
    replaced_removals: BTreeSet<String>,
}

impl GitChangeComparison {
    pub fn consistency(&self) -> GitContextSourceConsistencyV1 {
        if self.captures_worktree() {
            GitContextSourceConsistencyV1::PerFileWorktreeCapture
        } else {
            GitContextSourceConsistencyV1::ImmutableBlobs
        }
    }

    fn captures_worktree(&self) -> bool {
        matches!(
            self.selection,
            GitContextComparisonV1::WorkingTree | GitContextComparisonV1::HeadToWorktree { .. }
        )
    }
}

pub(super) struct ResolvedComparison {
    pub args: Vec<String>,
    selection: GitContextComparisonV1,
}

impl ResolvedComparison {
    pub(super) fn new(selection: &GitChangeSelection, snapshot: &RepositoryReadSnapshot) -> Self {
        let head = snapshot.head.commit().cloned();
        let base = head.as_ref().map_or_else(
            || match snapshot.object_format {
                GitObjectFormatV1::Sha1 => EMPTY_TREE_SHA1.to_owned(),
                GitObjectFormatV1::Sha256 => EMPTY_TREE_SHA256.to_owned(),
            },
            |oid| oid.as_str().to_owned(),
        );
        match selection {
            GitChangeSelection::Diff(GitDiffScopeV1::WorkingTree) => Self {
                args: Vec::new(),
                selection: GitContextComparisonV1::WorkingTree,
            },
            GitChangeSelection::Diff(GitDiffScopeV1::Staged) => Self {
                args: vec!["--cached".to_owned(), base],
                selection: GitContextComparisonV1::Staged { head },
            },
            GitChangeSelection::Diff(GitDiffScopeV1::CommitRange { base, head }) => Self {
                args: vec![base.as_str().to_owned(), head.as_str().to_owned()],
                selection: GitContextComparisonV1::CommitRange {
                    base: base.clone(),
                    head: head.clone(),
                },
            },
            GitChangeSelection::HeadToWorktree => Self {
                args: vec![base],
                selection: GitContextComparisonV1::HeadToWorktree { head },
            },
        }
    }
}

#[derive(Debug)]
pub struct GitChangedLines {
    pub before: Range<u32>,
    pub after: Range<u32>,
}

pub struct GitComparedSource {
    pub before: String,
    pub after: String,
    pub before_blob: Option<GitOidV1>,
    pub after_blob: Option<GitOidV1>,
    pub additions: u32,
    pub deletions: u32,
    pub changed_lines: Vec<GitChangedLines>,
}

#[derive(Debug, thiserror::Error)]
pub enum GitSourceError {
    #[error("source excluded by configured path policy")]
    Excluded,
    #[error("binary source")]
    Binary,
    #[error("non-UTF-8 source")]
    NonUtf8,
    #[error("submodule change")]
    Submodule,
    #[error("symlink change; source target is not followed")]
    Symlink,
    #[error("unmerged index stages")]
    Unmerged,
    #[error(transparent)]
    Read(#[from] GitIntelligenceError),
}

impl GitSourceError {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Read(GitIntelligenceError::Cancelled | GitIntelligenceError::DeadlineExceeded)
        )
    }

    pub fn is_opaque(&self) -> bool {
        matches!(
            self,
            Self::Binary | Self::NonUtf8 | Self::Submodule | Self::Symlink
        )
    }
}

fn source_error(detail: impl Into<String>) -> GitIntelligenceError {
    GitIntelligenceError::MalformedOutput {
        operation: "source comparison",
        detail: detail.into(),
    }
}

impl NativeGitIntelligence {
    /// Native object reads are individually bounded but not interruptible
    /// inside gix; checkpoints run immediately before and after each read.
    pub fn checkpoint(&self) -> Result<(), GitIntelligenceError> {
        if self
            .command_bounds
            .cancel
            .as_ref()
            .is_some_and(|cancel| cancel.is_cancelled())
        {
            return Err(GitIntelligenceError::Cancelled);
        }
        if Instant::now() >= self.command_bounds.deadline {
            return Err(GitIntelligenceError::DeadlineExceeded);
        }
        Ok(())
    }

    /// Capture exact object identities without materializing a full patch.
    /// Git's existing bounded raw parser is shared with the structured diff.
    pub fn compare_changes(
        &self,
        selection: &GitChangeSelection,
    ) -> Result<GitChangeComparison, GitIntelligenceError> {
        self.checkpoint()?;
        let snapshot = self.repository_snapshot()?;
        let resolved = ResolvedComparison::new(selection, &snapshot);
        let mut args = vec![
            "diff",
            "--raw",
            "-z",
            "-M",
            "--no-abbrev",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
        ];
        args.extend(resolved.args.iter().map(String::as_str));
        args.push("--");
        let mut files = parse_diff_raw(&self.stdout("diff metadata", &args)?)?;
        let unmerged = files
            .iter()
            .filter(|file| file.change == GitChangeKindV1::Unmerged)
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        files.retain_mut(|file| {
            if unmerged.contains(&file.path) {
                file.change = GitChangeKindV1::Unmerged;
                seen.insert(file.path.clone())
            } else {
                true
            }
        });
        let mut coverage = GitCoverageV1::degraded(snapshot.degradations.into_iter().collect());
        let mut replaced_removals = BTreeSet::new();
        if matches!(selection, GitChangeSelection::HeadToWorktree) {
            // Raw HEAD-to-worktree diff omits untracked files, including
            // replacements for index deletions and rename origins. An existing
            // rename source means the destination is a net addition and that
            // source needs its own HEAD-to-worktree comparison. Preserve actual
            // rename chains: another selected destination already accounts for
            // a source path populated by a different tracked rename.
            let destinations = files
                .iter()
                .map(|file| file.path.clone())
                .collect::<BTreeSet<_>>();
            let mut restored_sources = Vec::new();
            for file in &mut files {
                let path = match file.change {
                    GitChangeKindV1::Deleted => file.path.clone(),
                    GitChangeKindV1::Renamed => {
                        let original = file
                            .original_path
                            .as_ref()
                            .ok_or_else(|| source_error("renamed source has no original path"))?;
                        if destinations.contains(original) {
                            continue;
                        }
                        original.clone()
                    }
                    _ => continue,
                };
                let absolute = self.repo_root.join(&path);
                let mut mode = match std::fs::symlink_metadata(&absolute) {
                    Ok(_) => worktree_mode(&absolute).ok_or_else(|| {
                        source_error(format!("replacement path changed during selection: {path}"))
                    })?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(source_error(error.to_string())),
                };
                if !snapshot.trust_file_mode
                    && regular_mode(Some(&mode))
                    && let Some(old_mode) =
                        file.old_mode.as_ref().filter(|old| regular_mode(Some(old)))
                {
                    mode = old_mode.clone();
                }
                replaced_removals.insert(path.clone());
                if file.change == GitChangeKindV1::Renamed {
                    restored_sources.push(GitDiffFileMetadata {
                        path,
                        original_path: None,
                        change: GitChangeKindV1::Modified,
                        old_mode: file.old_mode.take(),
                        new_mode: Some(mode),
                        old_blob: file.old_blob.take(),
                        new_blob: None,
                    });
                    file.change = GitChangeKindV1::Added;
                    file.original_path = None;
                } else {
                    file.new_mode = Some(mode);
                    file.change = GitChangeKindV1::Modified;
                }
            }
            files.extend(restored_sources);
            let status = self.status()?;
            for degradation in status.coverage.degradations {
                coverage.record(degradation);
            }
            for entry in status.entries {
                match entry {
                    GitStatusEntryV1::Untracked { path } => {
                        if files.iter().any(|file| file.path == path) {
                            continue;
                        }
                        let mode = worktree_mode(&self.repo_root.join(&path)).ok_or_else(|| {
                            source_error(format!("untracked path changed during selection: {path}"))
                        })?;
                        files.push(GitDiffFileMetadata {
                            path,
                            original_path: None,
                            change: GitChangeKindV1::Added,
                            old_mode: None,
                            new_mode: Some(mode),
                            old_blob: None,
                            new_blob: None,
                        });
                    }
                    GitStatusEntryV1::Tracked(tracked) if tracked.is_conflicted() => {
                        if let Some(file) = files.iter_mut().find(|file| file.path == tracked.path)
                        {
                            file.change = GitChangeKindV1::Unmerged;
                        } else {
                            files.push(GitDiffFileMetadata {
                                path: tracked.path,
                                original_path: tracked.original_path,
                                change: GitChangeKindV1::Unmerged,
                                old_mode: tracked.head_mode,
                                new_mode: tracked.worktree_mode,
                                old_blob: None,
                                new_blob: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        if files
            .iter()
            .any(|file| file.change == GitChangeKindV1::Unmerged)
        {
            coverage.record(GitDegradationV1::ConflictedState);
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        self.checkpoint()?;
        Ok(GitChangeComparison {
            selection: resolved.selection,
            files,
            coverage,
            object_format: snapshot.object_format,
            trust_file_mode: snapshot.trust_file_mode,
            replaced_removals,
        })
    }

    pub fn compare_file(
        &self,
        comparison: &GitChangeComparison,
        file: &GitDiffFileMetadata,
        policy: &IndexPathPolicyV1,
    ) -> Result<GitComparedSource, GitSourceError> {
        self.checkpoint()?;
        let policy = policy.without_generated_dir_defaults();
        if policy.excludes(&file.path)
            || file
                .original_path
                .as_ref()
                .is_some_and(|path| policy.excludes(path))
        {
            return Err(GitSourceError::Excluded);
        }
        if file.change == GitChangeKindV1::Unmerged {
            return Err(GitSourceError::Unmerged);
        }
        for mode in [file.old_mode.as_ref(), file.new_mode.as_ref()]
            .into_iter()
            .flatten()
        {
            if mode.is_submodule() {
                return Err(GitSourceError::Submodule);
            }
            if mode.as_str() == GitFileModeV1::SYMLINK {
                return Err(GitSourceError::Symlink);
            }
        }
        let before = self.selected_blob(file.old_mode.as_ref(), file.old_blob.as_ref())?;
        let (after, after_blob) = if comparison.captures_worktree() {
            self.capture_worktree(
                &file.path,
                file.new_mode.as_ref(),
                comparison.object_format,
                comparison.trust_file_mode,
            )?
        } else {
            (
                self.selected_blob(file.new_mode.as_ref(), file.new_blob.as_ref())?,
                file.new_blob.clone(),
            )
        };
        if comparison.captures_worktree()
            && file.change == GitChangeKindV1::Modified
            && file.old_mode == file.new_mode
            && file.old_blob == after_blob
            && !comparison.replaced_removals.contains(&file.path)
        {
            return Err(source_error(
                "selected working-tree change no longer differs from its before blob; repeat the comparison",
            ).into());
        }
        if before.contains(&0) || after.contains(&0) {
            return Err(GitSourceError::Binary);
        }
        let before = String::from_utf8(before).map_err(|_| GitSourceError::NonUtf8)?;
        let after = String::from_utf8(after).map_err(|_| GitSourceError::NonUtf8)?;
        let input = gix::diff::blob::InternedInput::new(before.as_str(), after.as_str());
        let diff =
            gix::diff::blob::diff_with_slider_heuristics(gix::diff::blob::Algorithm::Myers, &input);
        let additions = diff.count_additions();
        let deletions = diff.count_removals();
        let changed_lines = diff
            .hunks()
            .map(|hunk| GitChangedLines {
                before: hunk.before,
                after: hunk.after,
            })
            .collect();
        self.checkpoint()?;
        Ok(GitComparedSource {
            before,
            after,
            before_blob: file.old_blob.clone(),
            after_blob,
            additions,
            deletions,
            changed_lines,
        })
    }

    fn selected_blob(
        &self,
        mode: Option<&GitFileModeV1>,
        oid: Option<&GitOidV1>,
    ) -> Result<Vec<u8>, GitIntelligenceError> {
        if mode.is_none() {
            return Ok(Vec::new());
        }
        let oid = oid.ok_or_else(|| {
            source_error("selected existing source has no immutable blob identity")
        })?;
        self.checkpoint()?;
        let result = NativeHistoricalBlobReaderV1::new(
            &self.repo_root,
            self.repository.clone(),
            self.worktree.clone(),
        )
        .read_blob(oid, GIT_HISTORICAL_BLOB_MAX_BYTES);
        self.checkpoint()?;
        result
    }

    fn capture_worktree(
        &self,
        path: &str,
        expected_mode: Option<&GitFileModeV1>,
        format: GitObjectFormatV1,
        trust_file_mode: bool,
    ) -> Result<(Vec<u8>, Option<GitOidV1>), GitIntelligenceError> {
        if !is_canonical_repository_relative_path(path) {
            return Err(GitIntelligenceError::InvalidHistoricalPath(path.to_owned()));
        }
        self.checkpoint()?;
        let path = self.repo_root.join(path);
        let before = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(source_error(error.to_string())),
        };
        let captured_mode = worktree_mode(&path);
        let mode_matches = captured_mode.as_ref() == expected_mode
            || (!trust_file_mode
                && regular_mode(captured_mode.as_ref())
                && regular_mode(expected_mode));
        if !mode_matches {
            return Err(source_error(
                "working-tree mode or existence changed after selection",
            ));
        }
        if expected_mode.is_none() {
            if before.is_some() {
                return Err(source_error("working-tree source appeared after selection"));
            }
            return Ok((Vec::new(), None));
        }
        let map_io = |error: std::io::Error| source_error(error.to_string());
        let before = before
            .ok_or_else(|| source_error("working-tree source disappeared after selection"))?;
        if !before.is_file() {
            return Err(source_error("working-tree source is not a regular file"));
        }
        let root = self.repo_root.canonicalize().map_err(map_io)?;
        let canonical = path.canonicalize().map_err(map_io)?;
        if !canonical.starts_with(&root) {
            return Err(source_error("working-tree source escaped repository"));
        }
        let file = std::fs::File::open(&canonical).map_err(map_io)?;
        if !same_file(&before, &file.metadata().map_err(map_io)?) {
            return Err(source_error("working-tree source changed while opening"));
        }
        let mut bytes = Vec::new();
        file.take(GIT_HISTORICAL_BLOB_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(map_io)?;
        if bytes.len() as u64 > GIT_HISTORICAL_BLOB_MAX_BYTES {
            return Err(GitIntelligenceError::HistoricalBlobBoundExceeded {
                bound: GIT_HISTORICAL_BLOB_MAX_BYTES,
                actual: bytes.len() as u64,
            });
        }
        let after = std::fs::symlink_metadata(&path).map_err(map_io)?;
        if !same_file(&before, &after)
            || bytes.len() as u64 != after.len()
            || path.canonicalize().map_err(map_io)? != canonical
            || worktree_mode(&path) != captured_mode
        {
            return Err(source_error("working-tree source changed during capture"));
        }
        let kind = match format {
            GitObjectFormatV1::Sha1 => gix::hash::Kind::Sha1,
            GitObjectFormatV1::Sha256 => gix::hash::Kind::Sha256,
        };
        let oid = gix::objs::compute_hash(kind, gix::objs::Kind::Blob, &bytes)
            .map_err(|error| source_error(error.to_string()))?;
        let oid = GitOidV1::new(oid.to_hex().to_string())?;
        self.checkpoint()?;
        Ok((bytes, Some(oid)))
    }
}

/// Git may preserve a tracked executable bit when the filesystem cannot.
/// File kinds still have to match, and physical mode changes during capture
/// remain observable even when Git does not treat them as source changes.
fn regular_mode(mode: Option<&GitFileModeV1>) -> bool {
    mode.is_some_and(|mode| {
        matches!(
            mode.as_str(),
            GitFileModeV1::REGULAR | GitFileModeV1::EXECUTABLE
        )
    })
}

fn same_file(before: &Metadata, after: &Metadata) -> bool {
    let same = before.is_file() == after.is_file()
        && before.len() == after.len()
        && matches!((before.modified(), after.modified()), (Ok(before), Ok(after)) if before == after);
    #[cfg(unix)]
    {
        same && before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.mode() == after.mode()
    }
    #[cfg(not(unix))]
    {
        same
    }
}
