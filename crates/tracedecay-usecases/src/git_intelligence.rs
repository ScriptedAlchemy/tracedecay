//! query read-only native Git intelligence adapter (Plan 36).
//!
//! This module is the fixed internal adapter for the typed read-only Git
//! contracts in [`tracedecay_domain::git`]: repository status,
//! working/staged/range diff with file and hunk structure, bounded history,
//! blame/line provenance, and `HunkRef` identity minting.
//!
//! Read-only is enforced structurally:
//!
//! - Every spawn goes through [`NativeGitIntelligence::run_git`], which admits
//!   only a closed list of read subcommands (`status`, `diff`, `log`, `blame`,
//!   `rev-parse`, `symbolic-ref`, `ls-files`, `ls-tree`, `hash-object`
//!   without `-w`, `config --get`, `check-attr`) and refuses everything else
//!   with [`GitIntelligenceError::ReadOnlyViolation`]. No public method
//!   accepts raw Git arguments; options come from fixed internal profiles.
//! - Ambient `GIT_*` environment (index/object-dir/worktree redirection) is
//!   scrubbed, and `GIT_OPTIONAL_LOCKS=0` is pinned so Git never takes
//!   optional index locks on our behalf.
//! - The adapter never writes the index, objects, refs, config, or the
//!   worktree; `hash-object` is always invoked without `-w` (content hashing
//!   only, no object write). Plan 34/36 mutation paths (staging, apply,
//!   index transactions) are out of scope and unrepresentable here.
//!
//! Degraded repository states (ignored collision, conflicted, detached,
//! unborn, sparse, split-index, submodule, shallow boundary) are reported
//! through typed [`GitCoverageV1`] degradations, never guessed clean.

#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Instant;

use gix::bstr::ByteSlice;
use serde::Serialize;
pub use tracedecay_application::git::{
    GIT_HISTORICAL_BLOB_MAX_BYTES, GIT_HISTORY_MAX_COUNT_LIMIT, GitBlameRequest,
    GitHistoricalBlobReadPort, GitHistoricalBlobRequestV1, GitHistoricalBlobV1, GitHistoryRequest,
    GitIntelligenceError, GitReadPort, NativeHistoricalBlobReaderV1,
};
use tracedecay_code_index::git_projection::{
    GitTopologyProjectionError, GitTopologyProjectionV1, GitTopologyRefV1,
    git_topology_ref_watermark,
};
use tracedecay_domain::git::{
    GitBlameAvailabilityV1, GitBlameLineV1, GitBlamePreviousV1, GitBlameV1, GitBlobExpectationV1,
    GitChangeKindV1, GitCommitIdentityV1, GitCoverageV1, GitDegradationV1, GitDiffScopeV1,
    GitDiffV1, GitFileDiffV1, GitFileModeV1, GitHeadStateV1, GitHistoryV1, GitHunkV1,
    GitIndexEntryExpectationV1, GitObjectFormatV1, GitOidV1, GitOperationStateV1, GitStatusV1,
    HUNK_REF_SCHEMA_VERSION_V1, HunkDirectionV1, HunkRefV1, full_hunk_selection_bitmap,
};
use tracedecay_domain::research::time::UtcMicros;
use tracedecay_domain::research::{
    ManifestDigest, RefId, RepositoryId, WorktreeId, canonical_sha256,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git_repository::{
    GitHistoryOptions as RepositoryHistoryOptions, GitReference, GitRepositoryAuthority,
};

/// Read subcommands the adapter is allowed to run. Anything outside this
/// list is refused before spawn, which makes index/ref/worktree/config
/// mutation structurally unrepresentable through this adapter.
const READ_ONLY_SUBCOMMANDS: &[&str] = &[
    "status",
    "diff",
    "log",
    "blame",
    "rev-parse",
    "symbolic-ref",
    "ls-files",
    "ls-tree",
    "hash-object",
    "config",
    "check-attr",
];

const EMPTY_TREE_SHA1: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const EMPTY_TREE_SHA256: &str = "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321";

/// One parsed `--raw -z` record.
#[derive(Debug)]
struct RawFileEntry {
    path: String,
    original_path: Option<String>,
    change: GitChangeKindV1,
    old_mode: Option<GitFileModeV1>,
    new_mode: Option<GitFileModeV1>,
    old_blob: Option<GitOidV1>,
    new_blob: Option<GitOidV1>,
}

/// One parsed hunk with its retained body lines (adapter-internal only;
/// bodies never leave this module — the domain value carries digests).
#[derive(Debug)]
struct ParsedHunk {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
    section: Option<String>,
    body: Vec<String>,
}

impl ParsedHunk {
    fn normalized_header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        )
    }

    fn insertions(&self) -> u32 {
        self.body
            .iter()
            .filter(|line| line.starts_with('+'))
            .count() as u32
    }

    fn deletions(&self) -> u32 {
        self.body
            .iter()
            .filter(|line| line.starts_with('-'))
            .count() as u32
    }
}

#[derive(Serialize)]
struct HunkPatchDigestInput<'a> {
    header: &'a str,
    body: &'a [String],
}

fn hunk_patch_digest(hunk: &ParsedHunk) -> Result<ManifestDigest, GitIntelligenceError> {
    Ok(canonical_sha256(&HunkPatchDigestInput {
        header: &hunk.normalized_header(),
        body: &hunk.body,
    })?)
}

fn hunk_context_digest(hunk: &ParsedHunk) -> Result<ManifestDigest, GitIntelligenceError> {
    let context: Vec<&str> = hunk
        .body
        .iter()
        .filter(|line| line.starts_with(' '))
        .map(String::as_str)
        .collect();
    Ok(canonical_sha256(&context)?)
}

/// One parsed `--patch` file section.
#[derive(Debug)]
struct PatchFile {
    binary: bool,
    submodule: bool,
    hunks: Vec<ParsedHunk>,
}

/// Raw/patch join result for one diff scope.
#[derive(Debug)]
struct JoinedDiffEntry {
    raw: RawFileEntry,
    /// `None` for a coalesced unmerged path (combined diff, not typed as
    /// ordinary hunks).
    patch: Option<PatchFile>,
}

#[derive(Debug)]
struct JoinedDiff {
    entries: Vec<JoinedDiffEntry>,
    conflicted: bool,
}

struct RepositoryReadSnapshot {
    git_dir: PathBuf,
    object_format: GitObjectFormatV1,
    head: GitHeadStateV1,
    head_degradations: BTreeSet<GitDegradationV1>,
    degradations: BTreeSet<GitDegradationV1>,
    index_has_unmerged: bool,
}

#[cfg(test)]
thread_local! {
    static GIT_SUBPROCESS_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// Fixed read-only native Git adapter for one repository checkout.
pub struct NativeGitIntelligence {
    repo_root: PathBuf,
    repository: RepositoryId,
    worktree: WorktreeId,
    command_bounds: tracedecay_runtime_core::git::GitCommandBounds,
}

pub trait GitTopologyReadPort {
    fn topology_ref_watermark(&self) -> Result<ManifestDigest, GitIntelligenceError>;
}

fn map_repository_error(
    error: tracedecay_runtime_core::git_repository::GitRepositoryError,
) -> GitIntelligenceError {
    use tracedecay_runtime_core::git_repository::GitRepositoryError;

    match error {
        GitRepositoryError::NotARepository { path } => GitIntelligenceError::NotARepository(path),
        GitRepositoryError::UnreadableRepository { detail, .. } => {
            GitIntelligenceError::GitFailed {
                operation: "repository",
                status: "gix".to_owned(),
                stderr: detail,
            }
        }
        GitRepositoryError::UnreadableHead { detail } => GitIntelligenceError::GitFailed {
            operation: "HEAD",
            status: "gix".to_owned(),
            stderr: detail,
        },
        GitRepositoryError::Operation { operation, detail } => GitIntelligenceError::GitFailed {
            operation,
            status: "gix".to_owned(),
            stderr: detail,
        },
        GitRepositoryError::Domain(error) => GitIntelligenceError::Domain(error),
    }
}

impl NativeGitIntelligence {
    pub fn new(
        repo_root: impl Into<PathBuf>,
        repository: RepositoryId,
        worktree: WorktreeId,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            repository,
            worktree,
            command_bounds: tracedecay_runtime_core::git::GitCommandBounds::default(),
        }
    }

    /// Bind subprocess fallbacks to the request's live deadline,
    /// cancellation token, and serialized-result byte ceiling.
    #[must_use]
    pub fn with_execution_bounds(
        mut self,
        deadline: Option<Instant>,
        cancel: Option<CancellationToken>,
        max_output_bytes: usize,
    ) -> Self {
        if let Some(deadline) = deadline {
            self.command_bounds.deadline = deadline;
        }
        self.command_bounds.cancel = cancel;
        self.command_bounds.max_stdout_bytes = max_output_bytes;
        self
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn repository(&self) -> &RepositoryId {
        &self.repository
    }

    pub fn worktree(&self) -> &WorktreeId {
        &self.worktree
    }

    /// Current HEAD state for this exact admitted repository/worktree.
    ///
    /// This is the canonical lightweight identity read for feedback, LSP, and
    /// managed-test consumers that do not need worktree status entries. It
    /// reuses the native repository snapshot, so it spawns no subprocess on
    /// repositories `gix` can open — including linked worktrees, where the
    /// snapshot resolves the checkout-specific HEAD rather than the common
    /// directory's.
    pub fn head(&self) -> Result<GitHeadStateV1, GitIntelligenceError> {
        Ok(self.repository_snapshot()?.head)
    }

    pub fn topology_ref_watermark(&self) -> Result<ManifestDigest, GitIntelligenceError> {
        let authority =
            GitRepositoryAuthority::discover(&self.repo_root).map_err(map_repository_error)?;
        let (_, _, watermark) = self.topology_ref_snapshot(&authority)?;
        Ok(watermark)
    }

    pub fn topology_projection(
        &self,
        max_count: u32,
    ) -> Result<GitTopologyProjectionV1, GitIntelligenceError> {
        let authority =
            GitRepositoryAuthority::discover(&self.repo_root).map_err(map_repository_error)?;
        let (head, refs, ref_watermark) = self.topology_ref_snapshot(&authority)?;
        let history = authority
            .history(&RepositoryHistoryOptions {
                max_count: max_count.clamp(1, GIT_HISTORY_MAX_COUNT_LIMIT),
                first_parent: false,
                path: None,
                follow_renames: false,
            })
            .map_err(map_repository_error)?;
        let (verified_head, verified_refs, verified_watermark) =
            self.topology_ref_snapshot(&authority)?;
        if head != verified_head || refs != verified_refs || ref_watermark != verified_watermark {
            return Err(GitIntelligenceError::MalformedOutput {
                operation: "topology projection",
                detail: "repository refs changed during topology capture".to_owned(),
            });
        }
        let projection = GitTopologyProjectionV1 {
            repository: self.repository.clone(),
            head,
            refs,
            history: GitHistoryV1 {
                repository: self.repository.clone(),
                commits: history.commits,
                truncated: history.truncated,
                coverage: GitCoverageV1::degraded(history.degradations.into_iter().collect()),
            },
            ref_watermark,
            branch_stacks: Vec::new(),
            worktree_occupancies: Vec::new(),
        };
        projection.validate().map_err(map_topology_error)?;
        Ok(projection)
    }

    fn topology_ref_snapshot(
        &self,
        authority: &GitRepositoryAuthority,
    ) -> Result<(GitHeadStateV1, Vec<GitTopologyRefV1>, ManifestDigest), GitIntelligenceError> {
        let head = authority.head().map_err(map_repository_error)?;
        let refs = authority
            .references()
            .map_err(map_repository_error)?
            .into_iter()
            .filter(|reference| reference.name.starts_with("refs/"))
            .map(topology_ref)
            .collect::<Result<Vec<_>, _>>()?;
        let watermark = git_topology_ref_watermark(&self.repository, &head, &refs)
            .map_err(map_topology_error)?;
        Ok((head, refs, watermark))
    }

    /// Read one exact commit/path blob through the mounted Plan 36 authority.
    ///
    /// The `gix` read itself lives beside its port in
    /// [`tracedecay_application::NativeHistoricalBlobReaderV1`] so extracted
    /// crates mount the same production read. It opens no subprocess and
    /// exposes no revision expression, traversal, ref mutation, or object
    /// write surface.
    pub fn historical_blob(
        &self,
        request: &GitHistoricalBlobRequestV1,
    ) -> Result<GitHistoricalBlobV1, GitIntelligenceError> {
        NativeHistoricalBlobReaderV1::new(
            self.repo_root.clone(),
            self.repository.clone(),
            self.worktree.clone(),
        )
        .read(request)
    }

    /// Spawn `git <args>` under the structural read-only guard.
    ///
    /// The first argument must be an admitted read subcommand; ambient
    /// `GIT_*` environment is scrubbed and `GIT_OPTIONAL_LOCKS=0` is pinned.
    fn run_git(
        &self,
        operation: &'static str,
        args: &[&str],
    ) -> Result<Output, GitIntelligenceError> {
        let subcommand = args.first().copied().unwrap_or("");
        if !READ_ONLY_SUBCOMMANDS.contains(&subcommand) {
            return Err(GitIntelligenceError::ReadOnlyViolation(
                args.first().map_or("<empty>", |arg| arg).to_owned(),
            ));
        }
        if subcommand == "hash-object" && args.iter().any(|arg| *arg == "-w" || *arg == "--write") {
            return Err(GitIntelligenceError::ReadOnlyViolation(
                "hash-object -w".to_owned(),
            ));
        }
        if subcommand == "config" && !args.contains(&"--get") {
            return Err(GitIntelligenceError::ReadOnlyViolation(
                "config without --get".to_owned(),
            ));
        }

        #[cfg(test)]
        GIT_SUBPROCESS_COUNT.set(GIT_SUBPROCESS_COUNT.get() + 1);
        let output = tracedecay_runtime_core::git::bounded_git_output(
            &self.repo_root,
            args,
            &self.command_bounds,
        )
        .map_err(|error| match error {
            tracedecay_runtime_core::git::GitCommandError::Unavailable(error) => {
                GitIntelligenceError::GitUnavailable(error.to_string())
            }
            tracedecay_runtime_core::git::GitCommandError::Cancelled => {
                GitIntelligenceError::Cancelled
            }
            tracedecay_runtime_core::git::GitCommandError::DeadlineExceeded => {
                GitIntelligenceError::DeadlineExceeded
            }
            tracedecay_runtime_core::git::GitCommandError::OutputLimitExceeded {
                stream,
                bound,
            } => GitIntelligenceError::OutputLimitExceeded { stream, bound },
            error => GitIntelligenceError::GitUnavailable(error.to_string()),
        })?;
        if output.status.success() {
            return Ok(output);
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if stderr.contains("not a git repository") {
            return Err(GitIntelligenceError::NotARepository(
                self.repo_root.display().to_string(),
            ));
        }
        Err(GitIntelligenceError::GitFailed {
            operation,
            status: output.status.to_string(),
            stderr,
        })
    }

    fn stdout(
        &self,
        operation: &'static str,
        args: &[&str],
    ) -> Result<String, GitIntelligenceError> {
        let output = self.run_git(operation, args)?;
        String::from_utf8(output.stdout).map_err(|_| GitIntelligenceError::MalformedOutput {
            operation,
            detail: "stdout was not UTF-8".to_owned(),
        })
    }

    /// In-progress native operation state from repository metadata.
    fn operation_state(git_dir: &Path) -> GitOperationStateV1 {
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

    /// Capture repository metadata once in-process for one public read.
    ///
    /// Exact porcelain remains the authority for mutable status/diff/blame
    /// payloads, but HEAD, object format, operation state, configuration, and
    /// index conflict state do not require subprocesses.
    fn repository_snapshot(&self) -> Result<RepositoryReadSnapshot, GitIntelligenceError> {
        let Ok(repo) = gix::open(&self.repo_root) else {
            return self.cli_repository_snapshot();
        };
        if repo.object_hash() != gix::hash::Kind::Sha1 {
            return self.cli_repository_snapshot();
        }
        self.repository_snapshot_from_open(&repo)
    }

    fn repository_snapshot_from_open(
        &self,
        repo: &gix::Repository,
    ) -> Result<RepositoryReadSnapshot, GitIntelligenceError> {
        let git_dir = repo.git_dir().to_path_buf();
        let (head, head_degradations) = Self::head_state_from_repository(repo)?;
        let mut degradations = BTreeSet::new();

        if repo
            .config_snapshot()
            .boolean("core.sparseCheckout")
            .unwrap_or(false)
        {
            degradations.insert(GitDegradationV1::SparseCheckout);
        }

        let split_index = std::fs::read_dir(&git_dir).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sharedindex.")
            })
        });
        if split_index {
            degradations.insert(GitDegradationV1::SplitIndex);
        }

        if self.repo_root.join(".gitmodules").is_file() {
            degradations.insert(GitDegradationV1::SubmoduleState);
        }

        let operation = Self::operation_state(&git_dir);
        if operation != GitOperationStateV1::None {
            degradations.insert(GitDegradationV1::InProgressOperation);
        }
        let index_has_unmerged = repo.open_index().is_ok_and(|index| {
            index
                .entries()
                .iter()
                .any(|entry| entry.stage() != gix::index::entry::Stage::Unconflicted)
        });
        Ok(RepositoryReadSnapshot {
            git_dir,
            object_format: GitObjectFormatV1::Sha1,
            head,
            head_degradations,
            degradations,
            index_has_unmerged,
        })
    }

    /// Bounded compatibility read for object formats or repository layouts
    /// that this build of gix cannot open. This path is uncommon and keeps
    /// SHA-256 repositories truthful without claiming native support.
    fn cli_repository_snapshot(&self) -> Result<RepositoryReadSnapshot, GitIntelligenceError> {
        let git_dir = PathBuf::from(
            self.stdout("rev-parse", &["rev-parse", "--absolute-git-dir"])?
                .trim(),
        );
        let object_format = match self
            .stdout("rev-parse", &["rev-parse", "--show-object-format"])?
            .trim()
        {
            "sha1" => GitObjectFormatV1::Sha1,
            "sha256" => GitObjectFormatV1::Sha256,
            unsupported => {
                return Err(GitIntelligenceError::MalformedOutput {
                    operation: "rev-parse",
                    detail: format!("unsupported object format {unsupported:?}"),
                });
            }
        };
        let (head, head_degradations) = self.cli_head_state()?;
        let mut degradations = BTreeSet::new();
        if self
            .stdout(
                "config",
                &["config", "--get", "--bool", "core.sparseCheckout"],
            )
            .is_ok_and(|value| value.trim() == "true")
        {
            degradations.insert(GitDegradationV1::SparseCheckout);
        }
        if std::fs::read_dir(&git_dir).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sharedindex.")
            })
        }) {
            degradations.insert(GitDegradationV1::SplitIndex);
        }
        if self.repo_root.join(".gitmodules").is_file() {
            degradations.insert(GitDegradationV1::SubmoduleState);
        }
        let operation = Self::operation_state(&git_dir);
        if operation != GitOperationStateV1::None {
            degradations.insert(GitDegradationV1::InProgressOperation);
        }
        let index_has_unmerged = !self
            .run_git("ls-files", &["ls-files", "-u"])?
            .stdout
            .is_empty();
        Ok(RepositoryReadSnapshot {
            git_dir,
            object_format,
            head,
            head_degradations,
            degradations,
            index_has_unmerged,
        })
    }

    fn cli_head_state(
        &self,
    ) -> Result<(GitHeadStateV1, BTreeSet<GitDegradationV1>), GitIntelligenceError> {
        let mut degradations = BTreeSet::new();
        match self.run_git("rev-parse", &["rev-parse", "--verify", "HEAD"]) {
            Ok(output) => {
                let commit = GitOidV1::new(String::from_utf8_lossy(&output.stdout).trim())?;
                let branch = self
                    .stdout("symbolic-ref", &["symbolic-ref", "--short", "-q", "HEAD"])
                    .map(|name| name.trim().to_owned())
                    .ok();
                if let Some(branch) = branch {
                    Ok((GitHeadStateV1::Attached { branch, commit }, degradations))
                } else {
                    degradations.insert(GitDegradationV1::DetachedHead);
                    Ok((GitHeadStateV1::Detached { commit }, degradations))
                }
            }
            Err(error) => {
                let branch = self
                    .stdout("symbolic-ref", &["symbolic-ref", "--short", "-q", "HEAD"])
                    .map(|name| name.trim().to_owned());
                match branch {
                    Ok(branch) if !branch.is_empty() => {
                        degradations.insert(GitDegradationV1::UnbornBranch);
                        Ok((GitHeadStateV1::Unborn { branch }, degradations))
                    }
                    _ => Err(error),
                }
            }
        }
    }

    fn head_state_from_repository(
        repo: &gix::Repository,
    ) -> Result<(GitHeadStateV1, BTreeSet<GitDegradationV1>), GitIntelligenceError> {
        let mut degradations = BTreeSet::new();
        let head = repo
            .head()
            .map_err(|error| GitIntelligenceError::MalformedOutput {
                operation: "head",
                detail: error.to_string(),
            })?;
        let branch = head
            .referent_name()
            .and_then(|name| name.as_bstr().to_str().ok())
            .and_then(|name| name.strip_prefix("refs/heads/"))
            .map(str::to_owned);
        if head.is_unborn() {
            let Some(branch) = branch else {
                return Err(GitIntelligenceError::MalformedOutput {
                    operation: "head",
                    detail: "unborn HEAD had no local branch referent".to_owned(),
                });
            };
            degradations.insert(GitDegradationV1::UnbornBranch);
            return Ok((GitHeadStateV1::Unborn { branch }, degradations));
        }
        let commit = head
            .id()
            .ok_or_else(|| GitIntelligenceError::MalformedOutput {
                operation: "head",
                detail: "born HEAD had no object id".to_owned(),
            })
            .and_then(|id| GitOidV1::new(id.to_hex().to_string()).map_err(Into::into))?;
        if let Some(branch) = branch {
            Ok((GitHeadStateV1::Attached { branch, commit }, degradations))
        } else {
            degradations.insert(GitDegradationV1::DetachedHead);
            Ok((GitHeadStateV1::Detached { commit }, degradations))
        }
    }

    /// Typed repository status with staged, unstaged, untracked, ignored,
    /// renamed, conflicted, submodule, sparse, split-index, and file-mode
    /// state plus explicit coverage.
    pub fn status(&self) -> Result<GitStatusV1, GitIntelligenceError> {
        let authority = tracedecay_runtime_core::git_repository::GitRepositoryAuthority::discover(
            &self.repo_root,
        )
        .map_err(map_repository_error)?;
        let _object_format = authority.object_format().map_err(map_repository_error)?;
        let snapshot = authority.status().map_err(map_repository_error)?;

        let status = GitStatusV1 {
            repository: self.repository.clone(),
            head: snapshot.head,
            operation: snapshot.operation,
            entries: snapshot.entries,
            coverage: GitCoverageV1::degraded(snapshot.degradations.into_iter().collect()),
        };
        status.validate()?;
        Ok(status)
    }

    /// Typed diff for one scope with file and hunk structure.
    pub fn diff(&self, scope: &GitDiffScopeV1) -> Result<GitDiffV1, GitIntelligenceError> {
        let snapshot = self.repository_snapshot()?;
        let joined = self.diff_internal(scope, &snapshot)?;
        let mut files = Vec::with_capacity(joined.entries.len());
        for entry in joined.entries {
            let raw = entry.raw;
            match entry.patch {
                None => {
                    // Coalesced unmerged path: the worktree diff is a native
                    // combined diff, which is not typed as ordinary hunks.
                    let submodule = raw
                        .new_mode
                        .as_ref()
                        .or(raw.old_mode.as_ref())
                        .is_some_and(GitFileModeV1::is_submodule);
                    files.push(GitFileDiffV1 {
                        path: raw.path,
                        original_path: None,
                        change: GitChangeKindV1::Unmerged,
                        old_mode: raw.old_mode,
                        new_mode: raw.new_mode,
                        old_blob: None,
                        new_blob: None,
                        binary: false,
                        submodule,
                        insertions: Some(0),
                        deletions: Some(0),
                        hunks: vec![],
                    });
                }
                Some(patch_file) => {
                    let submodule = raw
                        .new_mode
                        .as_ref()
                        .or(raw.old_mode.as_ref())
                        .is_some_and(GitFileModeV1::is_submodule)
                        || patch_file.submodule;
                    let binary = patch_file.binary;
                    let opaque = binary || submodule;
                    let mut hunks = Vec::with_capacity(patch_file.hunks.len());
                    let mut insertions = 0u32;
                    let mut deletions = 0u32;
                    for hunk in &patch_file.hunks {
                        insertions += hunk.insertions();
                        deletions += hunk.deletions();
                        hunks.push(GitHunkV1 {
                            old_start: hunk.old_start,
                            old_lines: hunk.old_lines,
                            new_start: hunk.new_start,
                            new_lines: hunk.new_lines,
                            section: hunk.section.clone(),
                            patch_digest: hunk_patch_digest(hunk)?,
                        });
                    }
                    files.push(GitFileDiffV1 {
                        path: raw.path,
                        original_path: raw.original_path,
                        change: raw.change,
                        old_mode: raw.old_mode,
                        new_mode: raw.new_mode,
                        old_blob: raw.old_blob,
                        new_blob: raw.new_blob,
                        binary,
                        submodule,
                        insertions: (!opaque).then_some(insertions),
                        deletions: (!opaque).then_some(deletions),
                        hunks,
                    });
                }
            }
        }

        let mut degradations = snapshot.degradations;
        // An unborn branch still supports a worktree diff against the index;
        // the UnbornBranch degradation records the state explicitly.
        degradations.extend(snapshot.head_degradations);
        if joined.conflicted || snapshot.index_has_unmerged {
            degradations.insert(GitDegradationV1::ConflictedState);
        }

        let diff = GitDiffV1 {
            repository: self.repository.clone(),
            scope: scope.clone(),
            files,
            coverage: GitCoverageV1::degraded(degradations.into_iter().collect()),
        };
        diff.validate()?;
        Ok(diff)
    }

    /// Run the fixed raw + patch profiles for a scope and join them in raw
    /// emission order. Conflicted paths surface as `U` + `M` raw records and
    /// a combined `diff --cc` patch section; they are coalesced into one
    /// unmerged entry (patch `None`) because combined-diff hunks are not
    /// ordinary `GitHunkV1` values. Normal entries pair 1:1 with normal
    /// patch sections; a divergence means the repository changed mid-read
    /// and is reported rather than silently misjoined.
    fn diff_internal(
        &self,
        scope: &GitDiffScopeV1,
        snapshot: &RepositoryReadSnapshot,
    ) -> Result<JoinedDiff, GitIntelligenceError> {
        let mut scope_args: Vec<String> = Vec::new();
        match scope {
            GitDiffScopeV1::WorkingTree => {}
            GitDiffScopeV1::Staged => {
                // With an unborn HEAD, `--cached` diffs against the empty
                // tree so newly staged files are reported truthfully.
                if matches!(snapshot.head, GitHeadStateV1::Unborn { .. }) {
                    scope_args.push(
                        match snapshot.object_format {
                            GitObjectFormatV1::Sha1 => EMPTY_TREE_SHA1,
                            GitObjectFormatV1::Sha256 => EMPTY_TREE_SHA256,
                        }
                        .to_owned(),
                    );
                }
                scope_args.push("--cached".to_owned());
            }
            GitDiffScopeV1::CommitRange { base, head } => {
                scope_args.push(base.as_str().to_owned());
                scope_args.push(head.as_str().to_owned());
            }
        }
        let scope_refs: Vec<&str> = scope_args.iter().map(String::as_str).collect();
        let abbrev = match snapshot.object_format {
            GitObjectFormatV1::Sha1 => "--abbrev=40",
            GitObjectFormatV1::Sha256 => "--abbrev=64",
        };

        let mut args = vec![
            "diff",
            "--raw",
            "-z",
            "--patch",
            "-M",
            abbrev,
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
        ];
        args.extend(scope_refs);
        args.push("--");
        let output = self.run_git("diff", &args)?;
        let (raw, patch) = split_combined_diff_output(output.stdout)?;

        let raw_entries = parse_diff_raw(&raw)?;
        let patch_files = parse_diff_patch(&patch);

        let unmerged_paths: BTreeSet<String> = raw_entries
            .iter()
            .filter(|entry| entry.change == GitChangeKindV1::Unmerged)
            .map(|entry| entry.path.clone())
            .collect();
        let conflicted = !unmerged_paths.is_empty();

        let mut patch_iter = patch_files.into_iter();
        let mut coalesced_unmerged: BTreeSet<String> = BTreeSet::new();
        let mut entries = Vec::with_capacity(raw_entries.len());
        for raw_entry in raw_entries {
            if unmerged_paths.contains(&raw_entry.path) {
                if coalesced_unmerged.insert(raw_entry.path.clone()) {
                    entries.push(JoinedDiffEntry {
                        raw: raw_entry,
                        patch: None,
                    });
                }
                continue;
            }
            let section = patch_iter.next().ok_or(GitIntelligenceError::MalformedOutput {
                operation: "diff",
                detail: format!(
                    "patch output ended before raw entry {:?}; repository may have changed mid-read",
                    raw_entry.path
                ),
            })?;
            entries.push(JoinedDiffEntry {
                raw: raw_entry,
                patch: Some(section),
            });
        }
        if patch_iter.next().is_some() {
            return Err(GitIntelligenceError::MalformedOutput {
                operation: "diff",
                detail: "patch emitted more file sections than raw; repository may have changed mid-read".to_owned(),
            });
        }
        Ok(JoinedDiff {
            entries,
            conflicted,
        })
    }

    /// Bounded commit history in native traversal order.
    pub fn history(
        &self,
        request: &GitHistoryRequest,
    ) -> Result<GitHistoryV1, GitIntelligenceError> {
        let max_count = request.max_count.clamp(1, GIT_HISTORY_MAX_COUNT_LIMIT);
        let authority = tracedecay_runtime_core::git_repository::GitRepositoryAuthority::discover(
            &self.repo_root,
        )
        .map_err(map_repository_error)?;
        let _object_format = authority.object_format().map_err(map_repository_error)?;
        let snapshot = authority
            .history(
                &tracedecay_runtime_core::git_repository::GitHistoryOptions {
                    max_count,
                    first_parent: request.first_parent,
                    path: request.path.clone(),
                    follow_renames: request.follow,
                },
            )
            .map_err(map_repository_error)?;

        let history = GitHistoryV1 {
            repository: self.repository.clone(),
            commits: snapshot.commits,
            truncated: snapshot.truncated,
            coverage: GitCoverageV1::degraded(snapshot.degradations.into_iter().collect()),
        };
        history.validate()?;
        Ok(history)
    }

    /// Blame/line provenance for one path with boundary, rename-following,
    /// and typed unavailable states.
    pub fn blame(&self, request: &GitBlameRequest) -> Result<GitBlameV1, GitIntelligenceError> {
        let snapshot = self.repository_snapshot()?;
        let mut degradations = snapshot.degradations;
        degradations.extend(snapshot.head_degradations);

        if matches!(snapshot.head, GitHeadStateV1::Unborn { .. }) {
            return self.blame_unavailable(
                request,
                GitBlameAvailabilityV1::UnbornBranch,
                degradations,
            );
        }
        if snapshot.git_dir.join("shallow").is_file() {
            degradations.insert(GitDegradationV1::ShallowBoundary);
        }

        let mut args = vec!["blame", "--line-porcelain"];
        if request.follow_renames {
            args.push("-M");
            args.push("-C");
        }
        args.push("--");
        args.push(request.path.as_str());

        let text = match self.run_git("blame", &args) {
            Ok(output) => String::from_utf8(output.stdout).map_err(|_| {
                GitIntelligenceError::MalformedOutput {
                    operation: "blame",
                    detail: "stdout was not UTF-8".to_owned(),
                }
            })?,
            Err(error) => {
                // Truthful typed unavailability: distinguish an untracked
                // path and a binary file from a genuine read failure.
                let tracked = self
                    .run_git(
                        "ls-files",
                        &["ls-files", "--error-unmatch", "--", &request.path],
                    )
                    .is_ok();
                if !tracked {
                    return self.blame_unavailable(
                        request,
                        GitBlameAvailabilityV1::PathNotTracked,
                        degradations,
                    );
                }
                if looks_binary(&self.repo_root.join(&request.path)) {
                    return self.blame_unavailable(
                        request,
                        GitBlameAvailabilityV1::BinaryFile,
                        degradations,
                    );
                }
                return Err(error);
            }
        };

        let lines = parse_blame_porcelain(&text)?;
        let blame = GitBlameV1 {
            repository: self.repository.clone(),
            path: request.path.clone(),
            lines,
            availability: GitBlameAvailabilityV1::Available,
            coverage: GitCoverageV1::degraded(degradations.into_iter().collect()),
        };
        blame.validate()?;
        Ok(blame)
    }

    fn blame_unavailable(
        &self,
        request: &GitBlameRequest,
        availability: GitBlameAvailabilityV1,
        degradations: BTreeSet<GitDegradationV1>,
    ) -> Result<GitBlameV1, GitIntelligenceError> {
        let blame = GitBlameV1 {
            repository: self.repository.clone(),
            path: request.path.clone(),
            lines: vec![],
            availability,
            coverage: GitCoverageV1::degraded(degradations.into_iter().collect()),
        };
        blame.validate()?;
        Ok(blame)
    }

    /// Mint immutable `HunkRefV1` identity for every hunk of a working-tree
    /// or staged diff, against the exact current index/HEAD/worktree state.
    ///
    /// These references are read-only identity evidence: applying them is a
    /// daemon Git mutation path. Commit-range diffs are never mintable.
    /// Per-file binary, submodule, symlink, mode-only, rename/copy,
    /// attribute-driven, and unmerged entries remain explicit read-only
    /// capability evidence in [`GitDiffV1`] and are omitted here without
    /// suppressing safe text refs from the same diff.
    pub fn hunk_refs(
        &self,
        scope: &GitDiffScopeV1,
        preview_id: &str,
        snapshot_digest: &ManifestDigest,
    ) -> Result<Vec<HunkRefV1>, GitIntelligenceError> {
        let direction = match scope {
            GitDiffScopeV1::WorkingTree => HunkDirectionV1::WorkingTreeToIndex,
            GitDiffScopeV1::Staged => HunkDirectionV1::IndexToHead,
            GitDiffScopeV1::CommitRange { .. } => {
                return Err(GitIntelligenceError::HunkRefNotMintable);
            }
        };

        let snapshot = self.repository_snapshot()?;
        let joined = self.diff_internal(scope, &snapshot)?;
        let mut references = Vec::new();

        for joined_entry in joined.entries {
            let entry = joined_entry.raw;
            let Some(patch_file) = joined_entry.patch else {
                continue;
            };
            let submodule = entry
                .new_mode
                .as_ref()
                .or(entry.old_mode.as_ref())
                .is_some_and(GitFileModeV1::is_submodule)
                || patch_file.submodule;
            if submodule {
                continue;
            }
            if patch_file.binary {
                continue;
            }
            if patch_file.hunks.is_empty() {
                continue;
            }
            if matches!(
                entry.change,
                GitChangeKindV1::Renamed | GitChangeKindV1::Copied
            ) {
                continue;
            }
            if entry
                .old_mode
                .iter()
                .chain(entry.new_mode.iter())
                .any(GitFileModeV1::is_symlink)
            {
                continue;
            }

            let index_entry = self.index_entry_expectation(&entry.path)?;
            if index_entry.unmerged_stage.is_some() {
                continue;
            }
            if index_entry
                .mode
                .as_ref()
                .is_some_and(GitFileModeV1::is_symlink)
            {
                continue;
            }
            let (attributes_digest, special_attributes) =
                self.attributes_digest_and_special_state(&entry.path)?;
            if special_attributes {
                continue;
            }

            let expected_base_blob = match direction {
                HunkDirectionV1::WorkingTreeToIndex => index_entry.blob.clone(),
                HunkDirectionV1::IndexToHead => {
                    let base_path = entry.original_path.as_deref().unwrap_or(&entry.path);
                    self.head_blob_expectation(base_path, &snapshot)?
                }
            };

            let (expected_worktree_blob, expected_worktree_mode) = match direction {
                HunkDirectionV1::WorkingTreeToIndex => {
                    let mode = worktree_mode(&self.repo_root.join(&entry.path));
                    if mode.as_ref().is_some_and(GitFileModeV1::is_symlink) {
                        continue;
                    }
                    (Some(self.worktree_blob_expectation(&entry.path)?), mode)
                }
                HunkDirectionV1::IndexToHead => (None, None),
            };

            for hunk in &patch_file.hunks {
                let reference = HunkRefV1 {
                    repository: self.repository.clone(),
                    worktree: self.worktree.clone(),
                    direction,
                    path: entry.path.clone(),
                    original_path: entry.original_path.clone(),
                    expected_base_blob: expected_base_blob.clone(),
                    expected_index_entry: index_entry.clone(),
                    expected_worktree_blob: expected_worktree_blob.clone(),
                    expected_worktree_mode: expected_worktree_mode.clone(),
                    hunk_header: hunk.normalized_header(),
                    context_digest: hunk_context_digest(hunk)?,
                    patch_digest: hunk_patch_digest(hunk)?,
                    selected_line_bitmap: full_hunk_selection_bitmap(
                        hunk.old_lines.max(hunk.new_lines),
                    ),
                    attributes_digest: Some(attributes_digest.clone()),
                    preview_id: preview_id.to_owned(),
                    schema_version: HUNK_REF_SCHEMA_VERSION_V1.to_owned(),
                    snapshot_digest: snapshot_digest.clone(),
                };
                reference.validate()?;
                references.push(reference);
            }
        }

        Ok(references)
    }

    /// Current index entry expectation for a path (blob, mode, stage).
    fn index_entry_expectation(
        &self,
        path: &str,
    ) -> Result<GitIndexEntryExpectationV1, GitIntelligenceError> {
        let output = self.run_git("ls-files", &["ls-files", "-s", "-z", "--", path])?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut stages = Vec::new();
        for record in text.split('\0').filter(|record| !record.is_empty()) {
            stages.push(parse_ls_files_stage(record)?);
        }
        let Some((mode, blob, _)) = stages.first().cloned() else {
            return Ok(GitIndexEntryExpectationV1 {
                blob: GitBlobExpectationV1::AbsentFile,
                mode: None,
                unmerged_stage: None,
            });
        };
        // A stage-0 entry is merged (None); any stage 1-3 record means the
        // path is unmerged.
        let unmerged_stage = stages
            .iter()
            .map(|(_, _, stage)| *stage)
            .find(|stage| *stage > 0);
        Ok(GitIndexEntryExpectationV1 {
            blob: GitBlobExpectationV1::Present(blob),
            mode: Some(mode),
            unmerged_stage,
        })
    }

    /// HEAD blob expectation for a path (absent when not in HEAD's tree).
    fn head_blob_expectation(
        &self,
        path: &str,
        snapshot: &RepositoryReadSnapshot,
    ) -> Result<GitBlobExpectationV1, GitIntelligenceError> {
        if matches!(snapshot.head, GitHeadStateV1::Unborn { .. }) {
            return Ok(GitBlobExpectationV1::AbsentFile);
        }
        let output = self.run_git("ls-tree", &["ls-tree", "-z", "HEAD", "--", path])?;
        let text = String::from_utf8_lossy(&output.stdout);
        let record = text.split('\0').find(|record| !record.is_empty());
        let Some(record) = record else {
            return Ok(GitBlobExpectationV1::AbsentFile);
        };
        // "<mode> <type> <oid>\t<path>"
        let (meta, _) = record
            .split_once('\t')
            .ok_or(GitIntelligenceError::MalformedOutput {
                operation: "ls-tree",
                detail: format!("missing path separator in {record:?}"),
            })?;
        let oid_text =
            meta.split_whitespace()
                .nth(2)
                .ok_or(GitIntelligenceError::MalformedOutput {
                    operation: "ls-tree",
                    detail: format!("missing object id in {record:?}"),
                })?;
        Ok(GitBlobExpectationV1::Present(GitOidV1::new(oid_text)?))
    }

    /// Native content identity or explicit absence of a worktree file.
    /// Present content is hashed by `git hash-object` WITHOUT `-w` — hashing
    /// only, no object write.
    fn worktree_blob_expectation(
        &self,
        path: &str,
    ) -> Result<GitBlobExpectationV1, GitIntelligenceError> {
        if std::fs::symlink_metadata(self.repo_root.join(path)).is_err() {
            return Ok(GitBlobExpectationV1::AbsentFile);
        }
        let output = self.stdout("hash-object", &["hash-object", "--", path])?;
        Ok(GitBlobExpectationV1::Present(GitOidV1::new(output.trim())?))
    }

    /// Capture the exact attribute identity and classify paths whose
    /// clean/smudge or end-of-line behavior lacks a proven native round trip.
    fn attributes_digest_and_special_state(
        &self,
        path: &str,
    ) -> Result<(ManifestDigest, bool), GitIntelligenceError> {
        let output = self.run_git("check-attr", &["check-attr", "-z", "-a", "--", path])?;
        let digest = canonical_sha256(&String::from_utf8_lossy(&output.stdout).into_owned())?;
        if output.stdout.is_empty() {
            return Ok((digest, false));
        }
        let fields = output.stdout.split(|byte| *byte == 0).collect::<Vec<_>>();
        let Some((terminator, records)) = fields.split_last() else {
            return Err(GitIntelligenceError::MalformedOutput {
                operation: "check-attr",
                detail: "attribute output was empty".to_owned(),
            });
        };
        if !terminator.is_empty() || !records.len().is_multiple_of(3) {
            return Err(GitIntelligenceError::MalformedOutput {
                operation: "check-attr",
                detail: "attribute output was not complete NUL-delimited triples".to_owned(),
            });
        }
        let special = records.chunks_exact(3).any(|record| {
            let attribute = record[1];
            let value = record[2];
            if attribute == b"filter" {
                value != b"unset"
            } else {
                attribute == b"text" || attribute == b"eol" || attribute == b"working-tree-encoding"
            }
        });
        Ok((digest, special))
    }
}

impl GitTopologyReadPort for NativeGitIntelligence {
    fn topology_ref_watermark(&self) -> Result<ManifestDigest, GitIntelligenceError> {
        NativeGitIntelligence::topology_ref_watermark(self)
    }
}

impl GitHistoricalBlobReadPort for NativeGitIntelligence {
    fn historical_blob(
        &self,
        request: &GitHistoricalBlobRequestV1,
    ) -> Result<GitHistoricalBlobV1, GitIntelligenceError> {
        NativeGitIntelligence::historical_blob(self, request)
    }
}

impl GitReadPort for NativeGitIntelligence {
    fn status(&self) -> Result<GitStatusV1, GitIntelligenceError> {
        NativeGitIntelligence::status(self)
    }

    fn diff(&self, scope: &GitDiffScopeV1) -> Result<GitDiffV1, GitIntelligenceError> {
        NativeGitIntelligence::diff(self, scope)
    }

    fn history(&self, request: &GitHistoryRequest) -> Result<GitHistoryV1, GitIntelligenceError> {
        NativeGitIntelligence::history(self, request)
    }

    fn blame(&self, request: &GitBlameRequest) -> Result<GitBlameV1, GitIntelligenceError> {
        NativeGitIntelligence::blame(self, request)
    }

    fn hunk_refs(
        &self,
        scope: &GitDiffScopeV1,
        preview_id: &str,
        snapshot_digest: &ManifestDigest,
    ) -> Result<Vec<HunkRefV1>, GitIntelligenceError> {
        NativeGitIntelligence::hunk_refs(self, scope, preview_id, snapshot_digest)
    }
}

fn topology_ref(reference: GitReference) -> Result<GitTopologyRefV1, GitIntelligenceError> {
    Ok(GitTopologyRefV1 {
        reference: RefId::new(reference.name)?,
        target: reference.target,
    })
}

fn map_topology_error(error: GitTopologyProjectionError) -> GitIntelligenceError {
    GitIntelligenceError::MalformedOutput {
        operation: "topology projection",
        detail: error.to_string(),
    }
}

fn parse_status_char(value: char) -> GitChangeKindV1 {
    match value {
        'M' => GitChangeKindV1::Modified,
        'A' => GitChangeKindV1::Added,
        'D' => GitChangeKindV1::Deleted,
        'R' => GitChangeKindV1::Renamed,
        'C' => GitChangeKindV1::Copied,
        'T' => GitChangeKindV1::TypeChanged,
        'U' => GitChangeKindV1::Unmerged,
        _ => GitChangeKindV1::Unmodified,
    }
}

/// Split one `git diff --raw -z --patch` payload into its raw and patch
/// sections. Git inserts one additional NUL between the raw record stream and
/// the first `diff --git`/`diff --cc` patch header.
fn split_combined_diff_output(output: Vec<u8>) -> Result<(String, String), GitIntelligenceError> {
    let separator = output
        .windows(b"\0\0diff --".len())
        .position(|window| window == b"\0\0diff --");
    let (raw, patch) = match separator {
        Some(position) => (&output[..=position], &output[position + 2..]),
        // The separator exists only when a patch section follows the raw
        // records. An empty diff, and a diff whose every entry is unmerged
        // (native git emits combined raw records and no patch for those),
        // legitimately carry no patch section.
        None if !output.starts_with(b"diff --") => (&output[..], &[][..]),
        None => {
            return Err(GitIntelligenceError::MalformedOutput {
                operation: "diff",
                detail: "combined output had a patch section but no raw records".to_owned(),
            });
        }
    };
    let raw =
        String::from_utf8(raw.to_vec()).map_err(|_| GitIntelligenceError::MalformedOutput {
            operation: "diff",
            detail: "raw output was not UTF-8".to_owned(),
        })?;
    let patch =
        String::from_utf8(patch.to_vec()).map_err(|_| GitIntelligenceError::MalformedOutput {
            operation: "diff",
            detail: "patch output was not UTF-8".to_owned(),
        })?;
    Ok((raw, patch))
}

/// Parse `git diff --raw -z` records.
fn parse_diff_raw(text: &str) -> Result<Vec<RawFileEntry>, GitIntelligenceError> {
    let malformed = |detail: String| GitIntelligenceError::MalformedOutput {
        operation: "diff",
        detail,
    };
    let zero = |value: &str| value.bytes().all(|byte| byte == b'0');

    let mut entries = Vec::new();
    let mut records = text.split('\0').filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        let Some(mut meta) = record.strip_prefix(':') else {
            return Err(malformed(format!(
                "raw record without ':' prefix: {record:?}"
            )));
        };
        // A merge in progress emits combined records — one extra leading ':'
        // per additional parent, with parents+1 modes and blobs — for its
        // unmerged paths (`git diff --raw` during a conflict).
        let mut parents = 1usize;
        while let Some(rest) = meta.strip_prefix(':') {
            parents += 1;
            meta = rest;
        }
        let fields: Vec<&str> = meta.split_whitespace().collect();
        if fields.len() < 2 * (parents + 1) + 1 {
            return Err(malformed(format!("short raw record {record:?}")));
        }
        let status = fields[2 * (parents + 1)];
        let change = if parents > 1 {
            GitChangeKindV1::Unmerged
        } else {
            parse_status_char(status.chars().next().unwrap_or('.'))
        };
        let is_rename_like = matches!(change, GitChangeKindV1::Renamed | GitChangeKindV1::Copied);

        // --raw -z emits `:<meta>\0<path>\0`; for a rename/copy it emits the
        // SOURCE path first, then the destination (verified against native
        // git: `R100\0old\0new`).
        let first = records
            .next()
            .ok_or_else(|| malformed("raw record missing path".to_owned()))?
            .to_owned();
        let (path, original_path) = if is_rename_like {
            let destination = records
                .next()
                .ok_or_else(|| malformed("rename raw record missing destination path".to_owned()))?
                .to_owned();
            (destination, Some(first))
        } else {
            (first, None)
        };

        let mode = |value: &str| -> Result<Option<GitFileModeV1>, GitIntelligenceError> {
            if zero(value) {
                Ok(None)
            } else {
                Ok(Some(GitFileModeV1::new(value)?))
            }
        };
        let blob = |value: &str| -> Result<Option<GitOidV1>, GitIntelligenceError> {
            if zero(value) {
                Ok(None)
            } else {
                Ok(Some(GitOidV1::new(value)?))
            }
        };

        entries.push(RawFileEntry {
            path,
            original_path,
            change,
            old_mode: mode(fields[0])?,
            new_mode: mode(fields[parents])?,
            old_blob: blob(fields[parents + 1])?,
            new_blob: blob(fields[2 * parents + 1])?,
        });
    }
    Ok(entries)
}

/// Parse one `@@ -o[,l] +n[,l] @@[ section]` hunk header.
fn parse_hunk_header(line: &str) -> Option<ParsedHunk> {
    let body = line.strip_prefix("@@ -")?;
    let (old, rest) = body.split_once(' ')?;
    let rest = rest.strip_prefix('+')?;
    let (new, rest) = rest.split_once(" @@")?;
    let parse_range = |range: &str| -> Option<(u32, u32)> {
        match range.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((range.parse().ok()?, 1)),
        }
    };
    let (old_start, old_lines) = parse_range(old)?;
    let (new_start, new_lines) = parse_range(new)?;
    let section = {
        let trimmed = rest.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    };
    Some(ParsedHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        section,
        body: Vec::new(),
    })
}

/// Parse `git diff --patch` output into per-file sections. Combined
/// (`diff --cc` / `diff --combined`) sections for unmerged paths are
/// skipped: their hunk grammar is not the ordinary unified format.
fn parse_diff_patch(text: &str) -> Vec<PatchFile> {
    let mut files: Vec<PatchFile> = Vec::new();
    let mut current_hunk: Option<ParsedHunk> = None;
    let mut skip_section = false;

    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if line.starts_with("diff --git ")
            || line.starts_with("diff --cc ")
            || line.starts_with("diff --combined ")
        {
            if let Some(hunk) = current_hunk.take()
                && let Some(file) = files.last_mut()
            {
                file.hunks.push(hunk);
            }
            skip_section = !line.starts_with("diff --git ");
            if !skip_section {
                files.push(PatchFile {
                    binary: false,
                    submodule: false,
                    hunks: Vec::new(),
                });
            }
            continue;
        }
        if skip_section {
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue; // preamble before the first file section
        };
        if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            file.binary = true;
            continue;
        }
        if line.starts_with("Subproject commit") {
            file.submodule = true;
            continue;
        }
        if line.starts_with("@@ -") {
            if let Some(hunk) = current_hunk.take() {
                file.hunks.push(hunk);
            }
            if let Some(hunk) = parse_hunk_header(line) {
                current_hunk = Some(hunk);
            }
            continue;
        }
        if let Some(hunk) = current_hunk.as_mut()
            && line.starts_with(['+', '-', ' ', '\\'])
        {
            hunk.body.push(line.to_owned());
        }
    }
    if let Some(hunk) = current_hunk.take()
        && let Some(file) = files.last_mut()
    {
        file.hunks.push(hunk);
    }
    files
}

/// Parse `git blame --line-porcelain` output.
fn parse_blame_porcelain(text: &str) -> Result<Vec<GitBlameLineV1>, GitIntelligenceError> {
    let malformed = |detail: String| GitIntelligenceError::MalformedOutput {
        operation: "blame",
        detail,
    };
    let mut lines = Vec::new();
    let mut cursor = text.lines().peekable();

    while let Some(header) = cursor.next() {
        let header_fields: Vec<&str> = header.split_whitespace().collect();
        if header_fields.len() < 3 {
            return Err(malformed(format!("bad blame header {header:?}")));
        }
        let commit = GitOidV1::new(header_fields[0])?;
        let origin_line: u32 = header_fields[1]
            .parse()
            .map_err(|_| malformed(format!("bad origin line in {header:?}")))?;
        let final_line: u32 = header_fields[2]
            .parse()
            .map_err(|_| malformed(format!("bad final line in {header:?}")))?;

        let mut author_name = String::new();
        let mut author_email = String::new();
        let mut author_time = UtcMicros(0);
        let mut boundary = false;
        let mut previous = None;

        for attr in cursor.by_ref() {
            if attr.starts_with('\t') {
                break; // content line terminates this record
            }
            if let Some(value) = attr.strip_prefix("author ") {
                value.clone_into(&mut author_name);
            } else if let Some(value) = attr.strip_prefix("author-mail ") {
                value.trim_matches(['<', '>']).clone_into(&mut author_email);
            } else if let Some(value) = attr.strip_prefix("author-time ") {
                let seconds: i64 = value
                    .parse()
                    .map_err(|_| malformed(format!("bad author time {value:?}")))?;
                author_time = UtcMicros(seconds.saturating_mul(1_000_000));
            } else if attr == "boundary" {
                boundary = true;
            } else if let Some(value) = attr.strip_prefix("previous ") {
                let mut parts = value.splitn(2, ' ');
                let prev_commit = parts.next().unwrap_or_default();
                let prev_path = parts.next().unwrap_or_default();
                previous = Some(GitBlamePreviousV1 {
                    commit: GitOidV1::new(prev_commit)?,
                    path: prev_path.to_owned(),
                });
            }
        }

        lines.push(GitBlameLineV1 {
            final_line,
            origin_line,
            commit,
            author: GitCommitIdentityV1 {
                name: author_name,
                email: author_email,
                at: author_time,
            },
            boundary,
            previous,
        });
    }
    Ok(lines)
}

/// Parse one `ls-files -s` record: "<mode> <oid> <stage>\t<path>".
fn parse_ls_files_stage(
    record: &str,
) -> Result<(GitFileModeV1, GitOidV1, u8), GitIntelligenceError> {
    let malformed = |detail: String| GitIntelligenceError::MalformedOutput {
        operation: "ls-files",
        detail,
    };
    let (meta, _) = record
        .split_once('\t')
        .ok_or_else(|| malformed(format!("missing path separator in {record:?}")))?;
    let fields: Vec<&str> = meta.split_whitespace().collect();
    if fields.len() != 3 {
        return Err(malformed(format!("bad stage record {record:?}")));
    }
    let stage: u8 = fields[2]
        .parse()
        .map_err(|_| malformed(format!("bad stage in {record:?}")))?;
    Ok((
        GitFileModeV1::new(fields[0])?,
        GitOidV1::new(fields[1])?,
        stage,
    ))
}

/// Bounded binary sniff: NUL byte in the first 8 KiB.
fn looks_binary(path: &Path) -> bool {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buffer = [0u8; 8192];
    let read = file.take(8192).read(&mut buffer).unwrap_or(0);
    buffer[..read].contains(&0)
}

/// Worktree file mode evidence (read-only metadata).
fn worktree_mode(path: &Path) -> Option<GitFileModeV1> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let mode = if metadata.file_type().is_symlink() {
        GitFileModeV1::SYMLINK
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
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
    };
    GitFileModeV1::new(mode).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;
    use tracedecay_domain::git::GitStatusEntryV1;

    fn git_available() -> bool {
        Command::new(tracedecay_runtime_core::git::git_program())
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn path(&self) -> &Path {
            self.dir.path()
        }

        fn git(&self, args: &[&str]) -> Output {
            Command::new(tracedecay_runtime_core::git::git_program())
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.com",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(self.path())
                .output()
                .expect("git spawn failed")
        }

        fn git_ok(&self, args: &[&str]) -> String {
            let output = self.git(args);
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }

        fn commit_all(&self, message: &str) -> String {
            self.git_ok(&["add", "-A"]);
            self.git_ok(&["commit", "-m", message]);
            self.git_ok(&["rev-parse", "HEAD"]).trim().to_owned()
        }

        fn init() -> Option<Self> {
            if !git_available() {
                return None;
            }
            let fixture = Self {
                dir: TempDir::new().unwrap(),
            };
            fixture.git_ok(&["init", "-b", "main"]);
            Some(fixture)
        }

        /// Repo with one committed text file on `main`.
        fn standard() -> Option<Self> {
            let fixture = Self::init()?;
            fixture.write(
                "src/main.txt",
                "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\n",
            );
            fixture.commit_all("initial");
            Some(fixture)
        }

        fn adapter(&self) -> NativeGitIntelligence {
            NativeGitIntelligence::new(
                self.path(),
                RepositoryId::new("repository.fixture").unwrap(),
                tracedecay_domain::research::WorktreeId::new("worktree.fixture").unwrap(),
            )
        }
    }

    #[test]
    fn native_adapter_implements_typed_git_read_port() {
        fn assert_port<T: GitReadPort>() {}

        assert_port::<NativeGitIntelligence>();
    }

    #[test]
    fn head_read_uses_exact_linked_worktree_without_spawning_status() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let linked_parent = TempDir::new().unwrap();
        let linked = linked_parent.path().join("feature-worktree");
        fixture.git_ok(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/exact-head",
            linked.to_str().unwrap(),
        ]);
        std::fs::write(linked.join("src/main.txt"), "feature\n").unwrap();
        let output = Command::new(tracedecay_runtime_core::git::git_program())
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
                "-c",
                "commit.gpgsign=false",
                "add",
                "-A",
            ])
            .current_dir(&linked)
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = Command::new(tracedecay_runtime_core::git::git_program())
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "feature",
            ])
            .current_dir(&linked)
            .output()
            .unwrap();
        assert!(output.status.success());
        let feature_head =
            tracedecay_runtime_core::git::git_capture(&linked, &["rev-parse", "HEAD"]).unwrap();
        let repository = RepositoryId::new("repository.exact").unwrap();
        let worktree =
            tracedecay_domain::research::WorktreeId::new("worktree.exact-feature").unwrap();
        let adapter = NativeGitIntelligence::new(&linked, repository.clone(), worktree.clone());

        GIT_SUBPROCESS_COUNT.set(0);
        let head = adapter.head().unwrap();

        assert_eq!(adapter.repository(), &repository);
        assert_eq!(adapter.worktree(), &worktree);
        assert_eq!(GIT_SUBPROCESS_COUNT.get(), 0);
        assert!(matches!(
            head,
            GitHeadStateV1::Attached { branch, commit }
                if branch == "feature/exact-head" && commit.as_str() == feature_head
        ));
    }

    #[test]
    fn common_reads_have_bounded_subprocess_counts() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write("src/main.txt", "staged\n");
        fixture.git_ok(&["add", "--", "src/main.txt"]);
        fixture.write("src/main.txt", "staged\nunstaged\n");
        let adapter = fixture.adapter();

        let count = |read: &dyn Fn()| {
            GIT_SUBPROCESS_COUNT.set(0);
            read();
            GIT_SUBPROCESS_COUNT.get()
        };
        // Status is served natively by the repository authority; no
        // subprocess is spawned at all since the porcelain cutover.
        assert_eq!(count(&|| drop(adapter.status().unwrap())), 0);
        assert_eq!(
            count(&|| drop(adapter.diff(&GitDiffScopeV1::WorkingTree).unwrap())),
            1
        );
        assert_eq!(
            count(&|| drop(adapter.diff(&GitDiffScopeV1::Staged).unwrap())),
            1
        );
        assert_eq!(
            count(&|| drop(adapter.history(&GitHistoryRequest::default()).unwrap())),
            0
        );
        // Path-filtered history is also served by the repository authority's
        // in-process traversal; no `git log` child remains on any history path.
        assert_eq!(
            count(&|| {
                drop(
                    adapter
                        .history(&GitHistoryRequest {
                            path: Some("src/main.txt".to_owned()),
                            ..GitHistoryRequest::default()
                        })
                        .unwrap(),
                );
            }),
            0
        );
        assert_eq!(
            count(&|| {
                drop(
                    adapter
                        .blame(&GitBlameRequest {
                            path: "src/main.txt".to_owned(),
                            follow_renames: false,
                        })
                        .unwrap(),
                );
            }),
            1
        );
    }

    #[test]
    #[ignore = "explicit cold/warm performance evidence"]
    fn measure_one_commit_dirty_worktree_cold_and_warm_reads() {
        const SAMPLES: usize = 12;
        const LABELS: [&str; 6] = [
            "status",
            "working_diff",
            "staged_diff",
            "pathless_history",
            "path_history",
            "blame",
        ];
        let mut cold: [Vec<u128>; 6] = std::array::from_fn(|_| Vec::with_capacity(SAMPLES));
        let mut warm: [Vec<u128>; 6] = std::array::from_fn(|_| Vec::with_capacity(SAMPLES));
        let elapsed = |read: &dyn Fn()| {
            let started = Instant::now();
            read();
            started.elapsed().as_micros()
        };

        for _ in 0..SAMPLES {
            let Some(fixture) = Fixture::standard() else {
                return;
            };
            fixture.write("src/main.txt", "staged\n");
            fixture.git_ok(&["add", "--", "src/main.txt"]);
            fixture.write("src/main.txt", "staged\nunstaged\n");
            let adapter = fixture.adapter();
            let path_history = GitHistoryRequest {
                path: Some("src/main.txt".to_owned()),
                ..GitHistoryRequest::default()
            };
            let blame = GitBlameRequest {
                path: "src/main.txt".to_owned(),
                follow_renames: false,
            };
            let reads: [&dyn Fn(); 6] = [
                &|| drop(adapter.status().unwrap()),
                &|| {
                    drop(adapter.diff(&GitDiffScopeV1::WorkingTree).unwrap());
                },
                &|| drop(adapter.diff(&GitDiffScopeV1::Staged).unwrap()),
                &|| drop(adapter.history(&GitHistoryRequest::default()).unwrap()),
                &|| drop(adapter.history(&path_history).unwrap()),
                &|| drop(adapter.blame(&blame).unwrap()),
            ];
            for (index, read) in reads.into_iter().enumerate() {
                cold[index].push(elapsed(read));
                warm[index].push(elapsed(read));
            }
        }

        let percentile = |values: &[u128], percentile: usize| {
            let mut values = values.to_vec();
            values.sort_unstable();
            let index = ((values.len() - 1) * percentile).div_ceil(100);
            values[index]
        };
        for index in 0..LABELS.len() {
            eprintln!(
                "{} cold_p50={}us cold_p95={}us warm_p50={}us warm_p95={}us",
                LABELS[index],
                percentile(&cold[index], 50),
                percentile(&cold[index], 95),
                percentile(&warm[index], 50),
                percentile(&warm[index], 95),
            );
        }
    }

    fn conflicted_fixture() -> Option<Fixture> {
        let fixture = Fixture::init()?;
        fixture.write("conflict.txt", "base\n");
        fixture.commit_all("base");
        fixture.git_ok(&["checkout", "-b", "side"]);
        fixture.write("conflict.txt", "side\n");
        fixture.commit_all("side");
        fixture.git_ok(&["checkout", "main"]);
        fixture.write("conflict.txt", "main\n");
        fixture.commit_all("main");
        let merge = fixture.git(&["merge", "side"]);
        assert!(!merge.status.success(), "fixture merge must conflict");
        Some(fixture)
    }

    #[test]
    fn status_reports_staged_unstaged_untracked_and_ignored() {
        let Some(fixture) = Fixture::standard() else {
            return; // git unavailable: skip gracefully
        };
        fixture.write(
            "src/main.txt",
            "line1\nchanged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );
        fixture.write("staged.txt", "staged\n");
        fixture.git_ok(&["add", "staged.txt"]);
        fixture.write("untracked.txt", "untracked\n");
        fixture.write(".gitignore", "*.log\n");
        fixture.write("debug.log", "ignored\n");

        let status = fixture.adapter().status().unwrap();
        status.validate().unwrap();
        assert!(matches!(status.head, GitHeadStateV1::Attached { .. }));
        let staged: BTreeSet<&str> = status
            .entries
            .iter()
            .filter_map(|entry| match entry {
                GitStatusEntryV1::Tracked(tracked)
                    if !matches!(
                        tracked.index,
                        GitChangeKindV1::Unmodified | GitChangeKindV1::Unmerged
                    ) =>
                {
                    Some(tracked.path.as_str())
                }
                _ => None,
            })
            .collect();
        let unstaged: BTreeSet<&str> = status
            .entries
            .iter()
            .filter_map(|entry| match entry {
                GitStatusEntryV1::Tracked(tracked)
                    if !matches!(
                        tracked.worktree,
                        GitChangeKindV1::Unmodified | GitChangeKindV1::Unmerged
                    ) =>
                {
                    Some(tracked.path.as_str())
                }
                _ => None,
            })
            .collect();
        let untracked: BTreeSet<&str> = status
            .entries
            .iter()
            .filter_map(|entry| match entry {
                GitStatusEntryV1::Untracked { path } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        let ignored: BTreeSet<&str> = status
            .entries
            .iter()
            .filter_map(|entry| match entry {
                GitStatusEntryV1::Ignored { path } => Some(path.as_str()),
                _ => None,
            })
            .collect();

        assert!(staged.contains("staged.txt"), "staged: {staged:?}");
        assert!(unstaged.contains("src/main.txt"), "unstaged: {unstaged:?}");
        assert!(
            untracked.contains("untracked.txt"),
            "untracked: {untracked:?}"
        );
        assert!(untracked.contains(".gitignore"), "untracked: {untracked:?}");
        assert!(ignored.contains("debug.log"), "ignored: {ignored:?}");

        // Differential: compare typed sets with `git status --porcelain`.
        let cli = fixture.git_ok(&["status", "--porcelain"]);
        let mut cli_staged = BTreeSet::new();
        let mut cli_unstaged = BTreeSet::new();
        let mut cli_untracked = BTreeSet::new();
        for line in cli.lines() {
            let bytes = line.as_bytes();
            if bytes.len() < 4 {
                continue;
            }
            let (x, y) = (bytes[0] as char, bytes[1] as char);
            let path = line[3..].trim();
            match (x, y) {
                ('?', '?') => {
                    cli_untracked.insert(path);
                }
                ('!', '!') => {}
                _ => {
                    if x != ' ' {
                        cli_staged.insert(path);
                    }
                    if y != ' ' {
                        cli_unstaged.insert(path);
                    }
                }
            }
        }
        assert_eq!(staged, cli_staged);
        assert_eq!(unstaged, cli_unstaged);
        assert_eq!(untracked, cli_untracked);
    }

    #[test]
    fn status_reports_detached_head() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let head = fixture.git_ok(&["rev-parse", "HEAD"]).trim().to_owned();
        fixture.git_ok(&["checkout", &head]);

        let status = fixture.adapter().status().unwrap();
        assert!(matches!(status.head, GitHeadStateV1::Detached { .. }));
        assert!(status.coverage.records(GitDegradationV1::DetachedHead));
        assert!(status.is_clean());
    }

    #[test]
    fn status_reports_unborn_branch() {
        let Some(fixture) = Fixture::init() else {
            return;
        };
        fixture.write("fresh.txt", "fresh\n");

        let status = fixture.adapter().status().unwrap();
        assert!(matches!(
            status.head,
            GitHeadStateV1::Unborn { ref branch } if branch == "main"
        ));
        assert!(status.coverage.records(GitDegradationV1::UnbornBranch));
        assert_eq!(status.untracked_count(), 1);
    }

    #[test]
    fn status_reports_conflicted_merge_state() {
        let Some(fixture) = conflicted_fixture() else {
            return;
        };

        let status = fixture.adapter().status().unwrap();
        assert_eq!(status.conflicted_count(), 1);
        assert_eq!(status.operation, GitOperationStateV1::Merge);
        assert!(status.coverage.records(GitDegradationV1::ConflictedState));
        assert!(
            status
                .coverage
                .records(GitDegradationV1::InProgressOperation)
        );
    }

    #[test]
    fn status_reports_submodule_presence_and_state() {
        let Some(submodule) = Fixture::init() else {
            return;
        };
        submodule.write("lib.txt", "v1\n");
        submodule.commit_all("submodule initial");

        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.git_ok(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            submodule.path().to_str().unwrap(),
            "deps/sub",
        ]);
        fixture.commit_all("add submodule");

        let adapter = fixture.adapter();
        let status = adapter.status().unwrap();
        assert!(status.coverage.records(GitDegradationV1::SubmoduleState));
        assert!(status.is_clean());

        // Dirty the submodule from inside: the entry must surface as a
        // flagged submodule record.
        let sub_path = fixture.path().join("deps/sub");
        std::fs::write(sub_path.join("lib.txt"), "v2\n").unwrap();
        Command::new(tracedecay_runtime_core::git::git_program())
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
            ])
            .args(["commit", "-am", "submodule v2"])
            .current_dir(&sub_path)
            .output()
            .unwrap();

        let status = adapter.status().unwrap();
        let entry = status.entries.iter().find_map(|entry| match entry {
            GitStatusEntryV1::Tracked(tracked) if tracked.path == "deps/sub" => Some(tracked),
            _ => None,
        });
        assert!(entry.is_some_and(|tracked| tracked.submodule));
    }

    #[test]
    fn status_reports_ignored_collision() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write(".gitignore", "*.log\n");
        fixture.write("debug.log", "ignored\n");
        fixture.write(
            "src/main.txt",
            "line1\nchanged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );

        let status = fixture.adapter().status().unwrap();
        // debug.log (ignored) shares the root directory with the live
        // .gitignore entry: typed ignored-collision degradation.
        assert!(status.coverage.records(GitDegradationV1::IgnoredCollision));
    }

    #[test]
    fn status_reports_sparse_checkout_and_split_index() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.git_ok(&["sparse-checkout", "init", "--cone"]);
        fixture.git_ok(&["sparse-checkout", "set", "src"]);
        fixture.git_ok(&["update-index", "--split-index"]);

        let status = fixture.adapter().status().unwrap();
        assert!(status.coverage.records(GitDegradationV1::SparseCheckout));
        assert!(status.coverage.records(GitDegradationV1::SplitIndex));
    }

    #[cfg(unix)]
    #[test]
    fn status_reports_file_modes_differentially() {
        use std::os::unix::fs::PermissionsExt as _;

        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let path = fixture.path().join("src/main.txt");
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();

        let status = fixture.adapter().status().unwrap();
        let tracked = status.entries.iter().find_map(|entry| match entry {
            GitStatusEntryV1::Tracked(tracked) if tracked.path == "src/main.txt" => Some(tracked),
            _ => None,
        });
        let tracked = tracked.expect("mode-only change must be reported");
        assert_eq!(
            tracked.head_mode.as_ref().map(GitFileModeV1::as_str),
            Some(GitFileModeV1::REGULAR)
        );
        assert_eq!(
            tracked.index_mode.as_ref().map(GitFileModeV1::as_str),
            Some(GitFileModeV1::REGULAR)
        );
        assert_eq!(
            tracked.worktree_mode.as_ref().map(GitFileModeV1::as_str),
            Some(GitFileModeV1::EXECUTABLE)
        );

        let native = fixture.git_ok(&["diff", "--summary", "--", "src/main.txt"]);
        assert!(
            native.contains("mode change 100644 => 100755"),
            "{native:?}"
        );
    }

    #[test]
    fn status_preserves_tracked_paths_with_spaces() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write("src/with space.txt", "before\n");
        fixture.commit_all("add spaced path");
        fixture.write("src/with space.txt", "after\n");

        let status = fixture.adapter().status().unwrap();
        let typed_paths: BTreeSet<&str> =
            status.entries.iter().map(GitStatusEntryV1::path).collect();
        assert!(
            typed_paths.contains("src/with space.txt"),
            "{typed_paths:?}"
        );

        let native = fixture.git_ok(&["status", "--porcelain=v2", "-z", "--untracked-files=all"]);
        assert!(native.contains("src/with space.txt"), "{native:?}");
    }

    #[test]
    fn diff_unstaged_hunks_match_git_cli() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        // Twenty committed lines with edits at lines 2 and 18: outside
        // default U3 context reach, so git emits exactly two hunks.
        let committed: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        fixture.write("src/big.txt", &format!("{}\n", committed.join("\n")));
        fixture.commit_all("add big file");
        let mut lines = committed;
        lines[1] = "changed2".to_owned();
        lines[17] = "changed18".to_owned();
        fixture.write("src/big.txt", &format!("{}\n", lines.join("\n")));

        let diff = fixture
            .adapter()
            .diff(&GitDiffScopeV1::WorkingTree)
            .unwrap();
        diff.validate().unwrap();
        assert_eq!(diff.files_changed(), 1);
        let file = &diff.files[0];
        assert_eq!(file.path, "src/big.txt");
        assert_eq!(file.change, GitChangeKindV1::Modified);
        assert_eq!(file.hunks.len(), 2);
        assert!(!file.binary && !file.submodule);

        // Differential: hunk headers and numstat against the git CLI.
        let cli = fixture.git_ok(&["diff", "--no-color", "--", "src/big.txt"]);
        let cli_hunks: Vec<&str> = cli.lines().filter(|line| line.starts_with("@@ ")).collect();
        assert_eq!(cli_hunks.len(), diff.files[0].hunks.len());
        for (typed, cli_header) in diff.files[0].hunks.iter().zip(cli_hunks.iter()) {
            assert!(
                cli_header.starts_with(&typed.normalized_header()),
                "typed header {:?} vs CLI {:?}",
                typed.normalized_header(),
                cli_header
            );
        }
        let numstat = fixture.git_ok(&["diff", "--numstat", "--", "src/big.txt"]);
        let totals: Vec<&str> = numstat.split_whitespace().collect();
        assert_eq!(totals[0].parse::<u32>().unwrap(), file.insertions.unwrap());
        assert_eq!(totals[1].parse::<u32>().unwrap(), file.deletions.unwrap());
    }

    #[test]
    fn diff_staged_scope_and_rename() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.git_ok(&["mv", "src/main.txt", "src/renamed.txt"]);

        let worktree = fixture
            .adapter()
            .diff(&GitDiffScopeV1::WorkingTree)
            .unwrap();
        assert_eq!(worktree.files_changed(), 0);

        let staged = fixture.adapter().diff(&GitDiffScopeV1::Staged).unwrap();
        staged.validate().unwrap();
        assert_eq!(staged.files_changed(), 1);
        let file = &staged.files[0];
        assert_eq!(file.change, GitChangeKindV1::Renamed);
        assert_eq!(file.original_path.as_deref(), Some("src/main.txt"));
        assert_eq!(file.path, "src/renamed.txt");
    }

    #[test]
    fn diff_commit_range() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let base = fixture.git_ok(&["rev-parse", "HEAD"]).trim().to_owned();
        fixture.write(
            "src/main.txt",
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nextra\n",
        );
        let head = fixture.commit_all("second");

        let adapter = fixture.adapter();
        let range = adapter
            .diff(&GitDiffScopeV1::CommitRange {
                base: GitOidV1::new(&base).unwrap(),
                head: GitOidV1::new(&head).unwrap(),
            })
            .unwrap();
        assert_eq!(range.files_changed(), 1);
        assert_eq!(range.files[0].insertions, Some(1));

        let empty = adapter
            .diff(&GitDiffScopeV1::CommitRange {
                base: GitOidV1::new(&head).unwrap(),
                head: GitOidV1::new(head).unwrap(),
            })
            .unwrap();
        assert_eq!(empty.files_changed(), 0);
    }

    #[test]
    fn diff_reports_binary_files_without_hunks() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let binary_bytes: Vec<u8> = [0u8, 159, 146, 150, 0, 1, 2, 3].into();
        std::fs::write(fixture.path().join("blob.bin"), &binary_bytes).unwrap();
        fixture.commit_all("add binary");
        let modified: Vec<u8> = [0u8, 9, 9, 9, 0, 4, 5, 6].into();
        std::fs::write(fixture.path().join("blob.bin"), &modified).unwrap();

        let diff = fixture
            .adapter()
            .diff(&GitDiffScopeV1::WorkingTree)
            .unwrap();
        assert_eq!(diff.files_changed(), 1);
        let file = &diff.files[0];
        assert_eq!(file.path, "blob.bin");
        assert!(file.binary);
        assert!(file.hunks.is_empty());
        assert_eq!(file.insertions, None);
        assert_eq!(file.deletions, None);
    }

    #[test]
    fn history_matches_git_cli_order_and_bounds() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write("second.txt", "two\n");
        fixture.commit_all("second");
        fixture.write("third.txt", "three\n");
        fixture.commit_all("third");

        let adapter = fixture.adapter();
        let history = adapter.history(&GitHistoryRequest::default()).unwrap();
        history.validate().unwrap();
        assert_eq!(history.commits.len(), 3);
        assert!(!history.truncated);
        assert_eq!(history.commits[0].subject, "third");
        assert_eq!(history.commits[0].parents.len(), 1);
        assert_eq!(history.commits[2].parents.len(), 0);

        // Differential: exact commit order against the git CLI.
        let cli = fixture.git_ok(&["log", "--format=%H"]);
        let cli_ids: Vec<&str> = cli.lines().collect();
        let typed_ids: Vec<&str> = history
            .commits
            .iter()
            .map(|commit| commit.commit.as_str())
            .collect();
        assert_eq!(typed_ids, cli_ids);

        let bounded = adapter
            .history(&GitHistoryRequest {
                max_count: 2,
                ..GitHistoryRequest::default()
            })
            .unwrap();
        assert_eq!(bounded.commits.len(), 2);
        assert!(bounded.truncated);
        assert!(bounded.coverage.records(GitDegradationV1::TruncatedOutput));
    }

    #[test]
    fn history_reports_unborn_branch_without_error() {
        let Some(fixture) = Fixture::init() else {
            return;
        };
        let history = fixture
            .adapter()
            .history(&GitHistoryRequest::default())
            .unwrap();
        assert!(history.commits.is_empty());
        assert!(!history.truncated);
        assert!(history.coverage.records(GitDegradationV1::UnbornBranch));
    }

    #[test]
    fn blame_tracks_line_provenance_differentially() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write(
            "src/main.txt",
            "line1\nchanged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );
        fixture.commit_all("change line 2");

        let blame = fixture
            .adapter()
            .blame(&GitBlameRequest {
                path: "src/main.txt".to_owned(),
                follow_renames: true,
            })
            .unwrap();
        blame.validate().unwrap();
        assert!(blame.is_available());
        assert_eq!(blame.lines.len(), 8);
        assert_eq!(blame.lines[0].final_line, 1);
        assert!(blame.lines.iter().all(|line| line.author.name == "Fixture"));

        // Differential: per-final-line origin commits against the CLI.
        let cli = fixture.git_ok(&["blame", "--porcelain", "--", "src/main.txt"]);
        let cli_commits: Vec<&str> = cli
            .lines()
            .filter(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                fields.len() >= 3
                    && fields[0].len() == 40
                    && fields[1].bytes().all(|b| b.is_ascii_digit())
                    && fields[2].bytes().all(|b| b.is_ascii_digit())
            })
            .map(|line| line.split_whitespace().next().unwrap())
            .collect();
        let typed_commits: Vec<&str> = blame
            .lines
            .iter()
            .map(|line| line.commit.as_str())
            .collect();
        assert_eq!(typed_commits, cli_commits);

        let first = history_head(&fixture, 1);
        let second = history_head(&fixture, 0);
        assert_eq!(blame.lines[0].commit.as_str(), first.as_str());
        assert_eq!(blame.lines[1].commit.as_str(), second.as_str());
    }

    /// HEAD~skip commit id from the fixture CLI.
    fn history_head(fixture: &Fixture, skip: usize) -> String {
        fixture
            .git_ok(&["rev-parse", &format!("HEAD~{skip}")])
            .trim()
            .to_owned()
    }

    #[test]
    fn blame_reports_untracked_path_and_unborn_branch() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let blame = fixture
            .adapter()
            .blame(&GitBlameRequest {
                path: "missing.txt".to_owned(),
                follow_renames: false,
            })
            .unwrap();
        assert_eq!(blame.availability, GitBlameAvailabilityV1::PathNotTracked);
        assert!(blame.lines.is_empty());

        let Some(unborn) = Fixture::init() else {
            return;
        };
        unborn.write("fresh.txt", "fresh\n");
        let blame = unborn
            .adapter()
            .blame(&GitBlameRequest {
                path: "fresh.txt".to_owned(),
                follow_renames: false,
            })
            .unwrap();
        assert_eq!(blame.availability, GitBlameAvailabilityV1::UnbornBranch);
        assert!(blame.lines.is_empty());
    }

    #[test]
    fn hunk_refs_mint_compare_and_swap_identity() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write(
            "src/main.txt",
            "line1\nchanged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );

        let snapshot_digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let adapter = fixture.adapter();
        let typed_diff = adapter.diff(&GitDiffScopeV1::WorkingTree).unwrap();
        let hunk_new_lines = typed_diff.files[0].hunks[0].new_lines;

        let references = adapter
            .hunk_refs(
                &GitDiffScopeV1::WorkingTree,
                "preview.fixture",
                &snapshot_digest,
            )
            .unwrap();
        assert_eq!(references.len(), 1);
        let reference = &references[0];
        assert_eq!(reference.direction, HunkDirectionV1::WorkingTreeToIndex);
        assert_eq!(reference.path, "src/main.txt");
        assert_eq!(reference.schema_version, HUNK_REF_SCHEMA_VERSION_V1);
        // Full-hunk selection covers exactly the hunk's new-side lines.
        assert_eq!(reference.selected_line_count(), u64::from(hunk_new_lines));
        assert_eq!(
            reference.hunk_header,
            typed_diff.files[0].hunks[0].normalized_header()
        );

        // Index/base identity matches the fixture's own read-only probes.
        let index_blob = fixture.git_ok(&["hash-object", "src/main.txt"]);
        let head_blob = fixture.git_ok(&["rev-parse", "HEAD:src/main.txt"]);
        let worktree_oid = GitOidV1::new(index_blob.trim()).unwrap();
        assert_eq!(
            reference.expected_worktree_blob.as_ref(),
            Some(&GitBlobExpectationV1::Present(worktree_oid))
        );
        assert_eq!(
            reference.expected_base_blob.blob().map(GitOidV1::as_str),
            Some(head_blob.trim())
        );

        // Identity is stable across mints and drifts with any field.
        let again = adapter
            .hunk_refs(
                &GitDiffScopeV1::WorkingTree,
                "preview.fixture",
                &snapshot_digest,
            )
            .unwrap();
        assert_eq!(
            reference.compute_digest().unwrap(),
            again[0].compute_digest().unwrap()
        );
        let drifted = adapter
            .hunk_refs(
                &GitDiffScopeV1::WorkingTree,
                "preview.other",
                &snapshot_digest,
            )
            .unwrap();
        assert_ne!(
            reference.compute_digest().unwrap(),
            drifted[0].compute_digest().unwrap()
        );
    }

    #[test]
    fn hunk_refs_cover_deletion_and_exact_worktree_identity() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        std::fs::remove_file(fixture.path().join("src/main.txt")).unwrap();

        let snapshot_digest = ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
        let adapter = fixture.adapter();
        let diff = adapter.diff(&GitDiffScopeV1::WorkingTree).unwrap();
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].hunks.len(), 1);
        assert_eq!(diff.files[0].hunks[0].new_lines, 0);

        let references = adapter
            .hunk_refs(
                &GitDiffScopeV1::WorkingTree,
                "preview.deletion",
                &snapshot_digest,
            )
            .unwrap();
        assert_eq!(references.len(), 1);
        let reference = &references[0];
        assert_eq!(
            reference.worktree,
            tracedecay_domain::research::WorktreeId::new("worktree.fixture").unwrap()
        );
        assert_eq!(
            reference.expected_worktree_blob,
            Some(GitBlobExpectationV1::AbsentFile)
        );
        assert_eq!(reference.expected_worktree_mode, None);
        assert_eq!(
            reference.selected_line_count(),
            u64::from(diff.files[0].hunks[0].old_lines)
        );
    }

    #[cfg(unix)]
    #[test]
    fn hunk_refs_keep_text_from_mixed_unstaged_diff() {
        use std::os::unix::fs::PermissionsExt;

        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write("a-safe.txt", "before\n");
        fixture.write("mode-only.txt", "mode\n");
        std::fs::write(fixture.path().join("blob.bin"), [0u8, 1, 0, 2]).unwrap();
        fixture.commit_all("add mixed fixtures");

        fixture.write("a-safe.txt", "after\n");
        fixture.write(
            "src/main.txt",
            "line1\nchanged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );
        std::fs::write(fixture.path().join("blob.bin"), [0u8, 3, 0, 4]).unwrap();
        let mode_path = fixture.path().join("mode-only.txt");
        let mut permissions = std::fs::metadata(&mode_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(mode_path, permissions).unwrap();

        let adapter = fixture.adapter();
        let diff = adapter.diff(&GitDiffScopeV1::WorkingTree).unwrap();
        assert!(
            diff.files
                .iter()
                .any(|file| file.path == "blob.bin" && file.binary && file.hunks.is_empty())
        );
        assert!(diff.files.iter().any(|file| {
            file.path == "mode-only.txt" && file.old_mode != file.new_mode && file.hunks.is_empty()
        }));

        let snapshot_digest = ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap();
        let references = adapter
            .hunk_refs(
                &GitDiffScopeV1::WorkingTree,
                "preview.mixed-unstaged",
                &snapshot_digest,
            )
            .unwrap();
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.path.as_str())
                .collect::<Vec<_>>(),
            ["a-safe.txt", "src/main.txt"]
        );
        assert!(references.iter().all(|reference| {
            reference.direction == HunkDirectionV1::WorkingTreeToIndex
                && reference.preview_id == "preview.mixed-unstaged"
                && reference.snapshot_digest == snapshot_digest
                && reference.repository == adapter.repository
                && reference.worktree == adapter.worktree
        }));
    }

    #[cfg(unix)]
    #[test]
    fn hunk_refs_keep_text_from_mixed_staged_diff_without_symlink_ref() {
        use std::os::unix::fs::symlink;

        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write(
            "src/main.txt",
            "line1\nstaged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );
        std::fs::write(fixture.path().join("staged.bin"), [0u8, 5, 0, 6]).unwrap();
        symlink("src/main.txt", fixture.path().join("staged-link")).unwrap();
        fixture.git_ok(&["add", "src/main.txt", "staged.bin", "staged-link"]);

        let adapter = fixture.adapter();
        let diff = adapter.diff(&GitDiffScopeV1::Staged).unwrap();
        assert!(
            diff.files
                .iter()
                .any(|file| file.path == "staged.bin" && file.binary && file.hunks.is_empty())
        );
        assert!(diff.files.iter().any(|file| {
            file.path == "staged-link"
                && file
                    .new_mode
                    .as_ref()
                    .is_some_and(GitFileModeV1::is_symlink)
        }));

        let snapshot_digest = ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap();
        let references = adapter
            .hunk_refs(
                &GitDiffScopeV1::Staged,
                "preview.mixed-staged",
                &snapshot_digest,
            )
            .unwrap();
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.path.as_str())
                .collect::<Vec<_>>(),
            ["src/main.txt"]
        );
        assert_eq!(references[0].direction, HunkDirectionV1::IndexToHead);
        assert_eq!(references[0].preview_id, "preview.mixed-staged");
    }

    #[test]
    fn hunk_refs_omit_rename_and_attribute_driven_paths() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write("safe.txt", "before\n");
        fixture.write(
            "rename-source.txt",
            concat!(
                "line-01\nline-02\nline-03\nline-04\nline-05\n",
                "line-06\nline-07\nline-08\nline-09\nline-10\n",
                "line-11\nline-12\nline-13\nline-14\nline-15\n",
                "line-16\nline-17\nline-18\nline-19\nline-20\n",
            ),
        );
        fixture.write("filtered.txt", "before\n");
        fixture.write(".gitattributes", "filtered.txt filter=fixture\n");
        fixture.commit_all("add special hunk fixtures");

        fixture.write("safe.txt", "after\n");
        fixture.write("filtered.txt", "after\n");
        std::fs::rename(
            fixture.path().join("rename-source.txt"),
            fixture.path().join("rename-target.txt"),
        )
        .unwrap();
        fixture.write(
            "rename-target.txt",
            concat!(
                "line-01\nline-02\nchanged\nline-04\nline-05\n",
                "line-06\nline-07\nline-08\nline-09\nline-10\n",
                "line-11\nline-12\nline-13\nline-14\nline-15\n",
                "line-16\nline-17\nline-18\nline-19\nline-20\n",
            ),
        );
        fixture.git_ok(&["add", "-A"]);

        let adapter = fixture.adapter();
        let diff = adapter.diff(&GitDiffScopeV1::Staged).unwrap();
        assert!(diff.files.iter().any(|file| {
            file.path == "rename-target.txt" && file.change == GitChangeKindV1::Renamed
        }));
        assert!(
            diff.files
                .iter()
                .any(|file| file.path == "filtered.txt" && !file.hunks.is_empty())
        );

        let snapshot_digest = ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).unwrap();
        let references = adapter
            .hunk_refs(
                &GitDiffScopeV1::Staged,
                "preview.special-kinds",
                &snapshot_digest,
            )
            .unwrap();
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.path.as_str())
                .collect::<Vec<_>>(),
            ["safe.txt"]
        );
    }

    #[test]
    fn hunk_refs_reject_range_and_omit_read_only_file_kinds() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let adapter = fixture.adapter();
        let snapshot_digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let head = fixture.git_ok(&["rev-parse", "HEAD"]).trim().to_owned();

        assert!(matches!(
            adapter.hunk_refs(
                &GitDiffScopeV1::CommitRange {
                    base: GitOidV1::new(&head).unwrap(),
                    head: GitOidV1::new(head).unwrap(),
                },
                "preview.fixture",
                &snapshot_digest,
            ),
            Err(GitIntelligenceError::HunkRefNotMintable)
        ));

        std::fs::write(fixture.path().join("blob.bin"), [0u8, 1, 0, 2]).unwrap();
        fixture.commit_all("add binary");
        std::fs::write(fixture.path().join("blob.bin"), [0u8, 3, 0, 4]).unwrap();
        assert!(
            adapter
                .hunk_refs(
                    &GitDiffScopeV1::WorkingTree,
                    "preview.fixture",
                    &snapshot_digest,
                )
                .unwrap()
                .is_empty()
        );

        let Some(conflicted) = conflicted_fixture() else {
            return;
        };
        assert!(
            conflicted
                .adapter()
                .hunk_refs(
                    &GitDiffScopeV1::WorkingTree,
                    "preview.fixture",
                    &snapshot_digest,
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn read_only_guard_rejects_write_operations() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let adapter = fixture.adapter();

        assert!(matches!(
            adapter.run_git("commit", &["commit", "-m", "x"]),
            Err(GitIntelligenceError::ReadOnlyViolation(_))
        ));
        assert!(matches!(
            adapter.run_git("add", &["add", "-A"]),
            Err(GitIntelligenceError::ReadOnlyViolation(_))
        ));
        assert!(matches!(
            adapter.run_git("hash-object", &["hash-object", "-w", "src/main.txt"]),
            Err(GitIntelligenceError::ReadOnlyViolation(_))
        ));
        assert!(matches!(
            adapter.run_git("update-ref", &["update-ref", "HEAD", "HEAD"]),
            Err(GitIntelligenceError::ReadOnlyViolation(_))
        ));
        // config without --get is a write path and is refused.
        assert!(matches!(
            adapter.run_git("config", &["config", "user.name", "x"]),
            Err(GitIntelligenceError::ReadOnlyViolation(_))
        ));
    }

    #[test]
    fn adapter_leaves_repository_byte_identical() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write(
            "src/main.txt",
            "line1\nchanged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );
        fixture.write("new.txt", "new\n");

        let snapshot_tree = |root: &Path| -> Vec<(String, Vec<u8>)> {
            let mut files = Vec::new();
            let mut stack = vec![root.join(".git")];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap() {
                    let entry = entry.unwrap();
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        files.push((
                            path.strip_prefix(root).unwrap().display().to_string(),
                            std::fs::read(&path).unwrap(),
                        ));
                    }
                }
            }
            files.sort();
            files
        };

        let before = snapshot_tree(fixture.path());
        let adapter = fixture.adapter();
        let snapshot_digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();

        adapter.status().unwrap();
        adapter.diff(&GitDiffScopeV1::WorkingTree).unwrap();
        adapter.diff(&GitDiffScopeV1::Staged).unwrap();
        adapter.history(&GitHistoryRequest::default()).unwrap();
        adapter
            .blame(&GitBlameRequest {
                path: "src/main.txt".to_owned(),
                follow_renames: true,
            })
            .unwrap();
        adapter
            .hunk_refs(
                &GitDiffScopeV1::WorkingTree,
                "preview.fixture",
                &snapshot_digest,
            )
            .unwrap();

        let after = snapshot_tree(fixture.path());
        assert_eq!(
            before, after,
            "read-only intelligence mutated repository state"
        );
        assert!(
            !after.iter().any(|(path, _)| path.ends_with(".lock")),
            "adapter left a lock file behind"
        );
    }
}
