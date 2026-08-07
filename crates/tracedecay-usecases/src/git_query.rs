//! query generation-aware Git query core (Plan 36, `query/21-git-query-core`).
//!
//! This module is the typed query layer above the fixed read-only adapter in
//! [`crate::git_intelligence`]. It composes the adapter's [`GitReadPort`] into
//! generation-aware queries:
//!
//! - **Typed queries**: current status summary, scoped diff (working tree,
//!   staged, or commit range), bounded history, path blame, and `HunkRef`
//!   enumeration. Every result returns the Plan 36 domain values with their
//!   [`GitCoverageV1`] degradation attached; query-level truncation is folded
//!   into the envelope coverage as [`GitDegradationV1::TruncatedOutput`].
//! - **Generation-aware joins**: a [`GenerationBoundGitQueryV1`] names the
//!   code generation under audit plus the revision evidence that generation
//!   claims (HEAD oid, worktree digest). The join re-queries the repository
//!   through the adapter, attaches fresh [`GitRevisionEvidenceV1`], and
//!   classifies drift as typed [`GenerationStalenessV1`]
//!   (`GenerationBehindHead`, `WorktreeDiverged`, `HistoryRewritten`) instead
//!   of silently mismatching.
//! - **Cancellation and bounding**: every query takes [`GitQueryBounds`] —
//!   max entries, max bytes, an optional deadline, and an optional
//!   cooperative cancellation flag. Entry bounds truncate truthfully (never
//!   silently), byte bounds fail truthfully, and no query performs an
//!   unbounded history walk.
//!
//! Read-only is structural: this module calls only [`GitReadPort`] methods.
//! It spawns no subprocess of its own, holds no repository handle, and cannot
//! mutate the index, refs, objects, config, or the worktree.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_code_index::git_projection::{
    GitTopologyProjectionError, GitTopologyProjectionStore,
};
use tracedecay_domain::code_intelligence::CodeGenerationId;
use tracedecay_domain::git::{
    GitBlameV1, GitChangeKindV1, GitCoverageV1, GitDegradationV1, GitDiffScopeV1, GitDiffV1,
    GitFileModeV1, GitHeadStateV1, GitHistoryV1, GitOidV1, GitOperationStateV1, GitStatusEntryV1,
    HunkRefV1,
};
use tracedecay_domain::research::{ManifestDigest, RepositoryId, canonical_sha256};
use tracedecay_graph_db::GraphCancellation;

use crate::git_intelligence::{
    GIT_HISTORY_MAX_COUNT_LIMIT, GitBlameRequest, GitHistoryRequest, GitIntelligenceError,
    GitReadPort, GitTopologyReadPort,
};

/// Schema version label for the query-layer generation-join payloads.
pub const GIT_QUERY_SCHEMA_VERSION_V1: &str = "tracedecay.git-query.v1";

/// Domain separator for the query-layer worktree digest. This digest is
/// query-layer evidence over typed status and worktree-diff identity — it is
/// not a native Git tree id and never authorizes object reconstruction.
pub const WORKTREE_DIGEST_DOMAIN: &str = "tracedecay.git-query.worktree.v1";

/// Default entry bound (files, status paths, commits, blame lines, or
/// hunk references retained by one query).
pub const GIT_QUERY_DEFAULT_MAX_ENTRIES: u32 = 1_000;

/// Default byte bound for one serialized query result.
pub const GIT_QUERY_DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Per-query resource bounds and cancellation.
///
/// - `max_entries` caps retained entries per query; overflow truncates and
///   records [`GitDegradationV1::TruncatedOutput`] in the envelope coverage.
/// - `max_bytes` caps the serialized result; overflow fails with
///   [`GitQueryError::ByteBoundExceeded`] rather than returning a silently
///   degraded payload.
/// - `deadline` is checked before and after every adapter call; expiry fails
///   with [`GitQueryError::DeadlineExceeded`].
/// - `cancel` is a cooperative flag polled at the same checkpoints; a set
///   flag fails with [`GitQueryError::Cancelled`].
///
/// Adapter calls are one-shot reads: cancellation and the deadline bracket
/// each call, they do not interrupt a native read in flight.
#[derive(Clone, Debug)]
pub struct GitQueryBounds {
    pub max_entries: u32,
    pub max_bytes: u64,
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for GitQueryBounds {
    fn default() -> Self {
        Self {
            max_entries: GIT_QUERY_DEFAULT_MAX_ENTRIES,
            max_bytes: GIT_QUERY_DEFAULT_MAX_BYTES,
            deadline: None,
            cancel: None,
        }
    }
}

impl GitQueryBounds {
    /// Poll the cancellation and deadline checkpoints.
    pub fn check(&self) -> Result<(), GitQueryError> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(GitQueryError::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(GitQueryError::DeadlineExceeded);
        }
        Ok(())
    }

    /// Clamp a requested entry count to this query's bound.
    pub fn clamp_entries(&self, requested: u32) -> u32 {
        requested.min(self.max_entries).max(1)
    }
}

/// Errors from the generation-aware Git query layer.
#[derive(Debug, Error)]
pub enum GitQueryError {
    /// Hunk enumeration was not bound to a daemon-captured preview input.
    #[error("git hunk query lacks daemon preview binding")]
    DaemonPreviewBindingAbsent,
    /// The underlying read-only adapter failed.
    #[error(transparent)]
    Adapter(#[from] GitIntelligenceError),
    /// The cooperative cancellation flag was set.
    #[error("git query cancelled")]
    Cancelled,
    /// The query deadline expired.
    #[error("git query deadline exceeded")]
    DeadlineExceeded,
    /// The serialized result exceeded the query byte bound.
    #[error("git query byte bound exceeded: {actual} bytes > bound {bound}")]
    ByteBoundExceeded { bound: u64, actual: u64 },
    /// The result could not be serialized for byte measurement.
    #[error("git query result serialization failed: {0}")]
    Serialization(String),
    /// A topology traversal was requested before its verified projection mounted.
    #[error("verified Git topology projection is unavailable: {0}")]
    TopologyUnavailable(String),
    /// The mounted topology projection no longer matches the repository refs.
    #[error("verified Git topology projection is stale")]
    TopologyStale {
        projected: ManifestDigest,
        current: ManifestDigest,
    },
    /// The verified topology projection failed closed.
    #[error("verified Git topology projection failed: {0}")]
    TopologyFailed(String),
}

// The typed query envelope and status summary are canonical public wire
// contracts owned by the application crate (`git::public_wire`), where SDK
// schema generation and root transport parsing share one authority.
pub use tracedecay_application::git::{GitQueryEnvelopeV1, GitStatusSummaryV1};

/// A generation-bound query: the generation under audit plus the revision
/// evidence it claims to have been built from. Claims are optional; an absent
/// claim simply cannot contradict the queried evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationBoundGitQueryV1 {
    pub generation_id: CodeGenerationId,
    /// HEAD oid the generation was captured against, when known.
    pub claimed_head: Option<GitOidV1>,
    /// Query-layer worktree digest captured with the generation, when known.
    pub claimed_worktree_digest: Option<ManifestDigest>,
    pub schema_version: String,
}

impl GenerationBoundGitQueryV1 {
    pub fn new(
        generation_id: CodeGenerationId,
        claimed_head: Option<GitOidV1>,
        claimed_worktree_digest: Option<ManifestDigest>,
    ) -> Self {
        Self {
            generation_id,
            claimed_head,
            claimed_worktree_digest,
            schema_version: GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
        }
    }
}

/// One worktree file's identity evidence for the worktree digest: path,
/// change kind, and the new-side blob/mode when native Git reported them.
#[derive(Serialize)]
struct WorktreeFileIdentity<'a> {
    path: &'a str,
    change: GitChangeKindV1,
    new_blob: Option<&'a GitOidV1>,
    new_mode: Option<&'a GitFileModeV1>,
}

#[derive(Serialize)]
struct WorktreeDigestInput<'a> {
    domain: &'static str,
    entries: &'a [GitStatusEntryV1],
    files: &'a [WorktreeFileIdentity<'a>],
}

/// Fresh git-side revision evidence for a generation join: HEAD state and
/// oid plus the query-layer worktree digest, all captured read-only through
/// the adapter at query time.
///
/// `worktree_digest` is a canonical digest over typed status entries and
/// worktree-diff blob/mode identity. Tracked content, mode, rename, and
/// deletion changes move it; untracked paths contribute their names (native
/// Git reports no blob identity for them through the read-only status view).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitRevisionEvidenceV1 {
    pub repository: RepositoryId,
    pub head: GitHeadStateV1,
    /// HEAD commit oid when the branch is born; `None` for an unborn branch.
    pub head_oid: Option<GitOidV1>,
    pub worktree_digest: ManifestDigest,
    pub coverage: GitCoverageV1,
    pub schema_version: String,
}

/// Typed staleness of a generation against freshly queried git evidence.
/// A mismatch is never silent: exactly one variant describes the drift.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "staleness", rename_all = "snake_case")]
pub enum GenerationStalenessV1 {
    /// Every claim the query carried matches the queried evidence.
    Current,
    /// The claimed HEAD is an ancestor of the current HEAD: the generation
    /// predates reachable commits.
    GenerationBehindHead {
        claimed: GitOidV1,
        current: GitOidV1,
    },
    /// HEAD matches the claim but the worktree digest moved.
    WorktreeDiverged {
        claimed: ManifestDigest,
        current: ManifestDigest,
    },
    /// The claimed HEAD is not reachable from the current HEAD within the
    /// query history bound: history was rewritten (or the claim is foreign
    /// to this repository). When the walk was bound-truncated, the join
    /// coverage records [`GitDegradationV1::TruncatedOutput`].
    HistoryRewritten { claimed_head: GitOidV1 },
}

/// The result of a generation-aware join: fresh revision evidence plus the
/// typed staleness classification and merged coverage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitJoinV1 {
    pub generation_id: CodeGenerationId,
    pub evidence: GitRevisionEvidenceV1,
    pub staleness: GenerationStalenessV1,
    pub coverage: GitCoverageV1,
    pub schema_version: String,
}

impl GenerationGitJoinV1 {
    pub fn is_current(&self) -> bool {
        matches!(self.staleness, GenerationStalenessV1::Current)
    }
}

/// Generation-aware query engine over a borrowed read-only Git port.
pub struct GitQueryEngine<'a, P: GitReadPort> {
    port: &'a P,
    topology: Option<&'a GitTopologyProjectionStore>,
}

impl<'a, P: GitReadPort> GitQueryEngine<'a, P> {
    pub fn new(port: &'a P) -> Self {
        Self {
            port,
            topology: None,
        }
    }

    pub fn with_topology(port: &'a P, topology: &'a GitTopologyProjectionStore) -> Self {
        Self {
            port,
            topology: Some(topology),
        }
    }

    /// Current repository status summary with per-class counts and a bounded
    /// changed-path sample.
    pub fn status_summary(
        &self,
        bounds: &GitQueryBounds,
    ) -> Result<GitQueryEnvelopeV1<GitStatusSummaryV1>, GitQueryError> {
        bounds.check()?;
        let status = self.port.status()?;
        bounds.check()?;

        let mut changed_paths: Vec<String> = status
            .entries
            .iter()
            .map(|entry| entry.path().to_owned())
            .collect();
        changed_paths.sort_unstable();
        changed_paths.dedup();
        let truncated = changed_paths.len() > bounds.max_entries as usize;
        if truncated {
            changed_paths.truncate(bounds.max_entries as usize);
        }

        let summary = GitStatusSummaryV1 {
            repository: status.repository.clone(),
            head: status.head.clone(),
            operation: status.operation,
            staged: status.staged_count() as u32,
            unstaged: status.unstaged_count() as u32,
            conflicted: status.conflicted_count() as u32,
            untracked: status.untracked_count() as u32,
            ignored: status.ignored_count() as u32,
            changed_paths,
            schema_version: GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
        };
        check_bytes(bounds, &summary)?;
        Ok(envelope(summary, status.coverage, truncated))
    }

    /// Scoped diff (working tree, staged, or commit range), entry-bounded at
    /// the file level.
    pub fn scoped_diff(
        &self,
        bounds: &GitQueryBounds,
        scope: &GitDiffScopeV1,
    ) -> Result<GitQueryEnvelopeV1<GitDiffV1>, GitQueryError> {
        bounds.check()?;
        let mut diff = self.port.diff(scope)?;
        bounds.check()?;

        let truncated = diff.files.len() > bounds.max_entries as usize;
        if truncated {
            diff.files.truncate(bounds.max_entries as usize);
        }
        check_bytes(bounds, &diff)?;
        let coverage = diff.coverage.clone();
        Ok(envelope(diff, coverage, truncated))
    }

    /// Bounded history. The requested count is clamped to the query entry
    /// bound and the adapter's hard limit before the walk, so no query walks
    /// unbounded history. Adapter-reported truncation surfaces as
    /// `truncated_by_bound` plus the adapter's own `TruncatedOutput` coverage.
    pub fn bounded_history(
        &self,
        bounds: &GitQueryBounds,
        request: &GitHistoryRequest,
    ) -> Result<GitQueryEnvelopeV1<GitHistoryV1>, GitQueryError> {
        bounds.check()?;
        let clamped = GitHistoryRequest {
            max_count: bounds
                .clamp_entries(request.max_count)
                .min(GIT_HISTORY_MAX_COUNT_LIMIT),
            path: request.path.clone(),
            follow: request.follow,
            first_parent: request.first_parent,
        };
        let history = self.port.history(&clamped)?;
        bounds.check()?;

        check_bytes(bounds, &history)?;
        let truncated = history.truncated;
        let coverage = history.coverage.clone();
        Ok(envelope(history, coverage, truncated))
    }

    /// Path blame, entry-bounded at the line level. Truncation keeps the
    /// domain ordering invariant (lines are strictly increasing) and records
    /// `TruncatedOutput` in the envelope coverage.
    pub fn path_blame(
        &self,
        bounds: &GitQueryBounds,
        request: &GitBlameRequest,
    ) -> Result<GitQueryEnvelopeV1<GitBlameV1>, GitQueryError> {
        bounds.check()?;
        let mut blame = self.port.blame(request)?;
        bounds.check()?;

        let truncated = blame.lines.len() > bounds.max_entries as usize;
        if truncated {
            blame.lines.truncate(bounds.max_entries as usize);
        }
        check_bytes(bounds, &blame)?;
        let coverage = blame.coverage.clone();
        Ok(envelope(blame, coverage, truncated))
    }

    /// `HunkRef` enumeration for a working-tree or staged diff, entry-bounded
    /// at the reference level. Range diffs fail truthfully; per-file
    /// read-only kinds remain visible through the paired typed diff and do
    /// not suppress safe text refs.
    pub fn hunk_refs(
        &self,
        bounds: &GitQueryBounds,
        scope: &GitDiffScopeV1,
        preview_id: &str,
        snapshot_digest: &ManifestDigest,
    ) -> Result<GitQueryEnvelopeV1<Vec<HunkRefV1>>, GitQueryError> {
        bounds.check()?;
        let mut references = self.port.hunk_refs(scope, preview_id, snapshot_digest)?;
        bounds.check()?;

        let truncated = references.len() > bounds.max_entries as usize;
        if truncated {
            references.truncate(bounds.max_entries as usize);
        }
        check_bytes(bounds, &references)?;
        Ok(envelope(references, GitCoverageV1::complete(), truncated))
    }

    /// Fresh git-side revision evidence: HEAD state and oid plus the
    /// query-layer worktree digest over status and worktree-diff identity.
    pub fn revision_evidence(
        &self,
        bounds: &GitQueryBounds,
    ) -> Result<GitRevisionEvidenceV1, GitQueryError> {
        bounds.check()?;
        let status = self.port.status()?;
        bounds.check()?;
        let diff = self.port.diff(&GitDiffScopeV1::WorkingTree)?;
        bounds.check()?;

        let mut coverage = status.coverage.clone();
        for degradation in &diff.coverage.degradations {
            coverage.record(*degradation);
        }

        let files: Vec<WorktreeFileIdentity<'_>> = diff
            .files
            .iter()
            .map(|file| WorktreeFileIdentity {
                path: &file.path,
                change: file.change,
                new_blob: file.new_blob.as_ref(),
                new_mode: file.new_mode.as_ref(),
            })
            .collect();
        let worktree_digest = canonical_sha256(&WorktreeDigestInput {
            domain: WORKTREE_DIGEST_DOMAIN,
            entries: &status.entries,
            files: &files,
        })
        .map_err(GitIntelligenceError::from)?;

        let evidence = GitRevisionEvidenceV1 {
            repository: status.repository.clone(),
            head_oid: status.head.commit().cloned(),
            head: status.head,
            worktree_digest,
            coverage,
            schema_version: GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
        };
        check_bytes(bounds, &evidence)?;
        Ok(evidence)
    }

    /// Generation-aware join: re-query revision evidence and classify the
    /// generation's claimed revision against it as typed staleness.
    ///
    /// Classification order is HEAD first, then worktree:
    ///
    /// - claimed HEAD equals current HEAD (or no HEAD claim): a worktree
    ///   digest mismatch classifies `WorktreeDiverged`, else `Current`;
    /// - claimed HEAD differs and is reachable from current HEAD within the
    ///   bounded history walk: `GenerationBehindHead`;
    /// - claimed HEAD differs and is not reachable within the walk (or HEAD
    ///   is unborn): `HistoryRewritten`.
    pub fn join_generation(
        &self,
        bounds: &GitQueryBounds,
        query: &GenerationBoundGitQueryV1,
    ) -> Result<GenerationGitJoinV1, GitQueryError>
    where
        P: GitTopologyReadPort,
    {
        bounds.check()?;
        let evidence = self.revision_evidence(bounds)?;
        let mut coverage = evidence.coverage.clone();

        let staleness = match (&query.claimed_head, &evidence.head_oid) {
            (Some(claimed), Some(current)) if claimed != current => {
                bounds.check()?;
                let reachable = if let Some(topology) = self.topology {
                    let current_watermark = self.port.topology_ref_watermark()?;
                    topology
                        .verify_ref_watermark(&current_watermark)
                        .map_err(map_topology_error)?;
                    if topology.repository() != &evidence.repository {
                        return Err(GitQueryError::TopologyUnavailable(
                            "mounted Git topology belongs to another repository".to_owned(),
                        ));
                    }
                    for degradation in &topology.coverage().degradations {
                        coverage.record(*degradation);
                    }
                    let limit = bounds.max_entries.max(1) as usize;
                    let ancestors = topology
                        .ancestors(
                            current,
                            limit,
                            limit,
                            Arc::new(QueryGraphCancellation {
                                cancelled: bounds.cancel.clone(),
                            }),
                        )
                        .map_err(map_topology_error)?;
                    bounds.check()?;
                    ancestors.contains(claimed)
                } else {
                    let walk = self.bounded_history(
                        bounds,
                        &GitHistoryRequest {
                            max_count: bounds.max_entries.min(GIT_HISTORY_MAX_COUNT_LIMIT),
                            ..GitHistoryRequest::default()
                        },
                    )?;
                    for degradation in &walk.coverage.degradations {
                        coverage.record(*degradation);
                    }
                    walk.value
                        .commits
                        .iter()
                        .any(|commit| &commit.commit == claimed)
                };
                if reachable {
                    GenerationStalenessV1::GenerationBehindHead {
                        claimed: claimed.clone(),
                        current: current.clone(),
                    }
                } else {
                    GenerationStalenessV1::HistoryRewritten {
                        claimed_head: claimed.clone(),
                    }
                }
            }
            (Some(claimed), None) => GenerationStalenessV1::HistoryRewritten {
                claimed_head: claimed.clone(),
            },
            _ => match &query.claimed_worktree_digest {
                Some(claimed) if *claimed != evidence.worktree_digest => {
                    GenerationStalenessV1::WorktreeDiverged {
                        claimed: claimed.clone(),
                        current: evidence.worktree_digest.clone(),
                    }
                }
                _ => GenerationStalenessV1::Current,
            },
        };

        let join = GenerationGitJoinV1 {
            generation_id: query.generation_id.clone(),
            evidence,
            staleness,
            coverage,
            schema_version: GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
        };
        check_bytes(bounds, &join)?;
        Ok(join)
    }
}

struct QueryGraphCancellation {
    cancelled: Option<Arc<AtomicBool>>,
}

impl GraphCancellation for QueryGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }
}

fn map_topology_error(error: GitTopologyProjectionError) -> GitQueryError {
    match error {
        GitTopologyProjectionError::Stale { projected, current } => {
            GitQueryError::TopologyStale { projected, current }
        }
        GitTopologyProjectionError::Unavailable(message) => {
            GitQueryError::TopologyUnavailable(message)
        }
        GitTopologyProjectionError::Cancelled => GitQueryError::Cancelled,
        GitTopologyProjectionError::BudgetExhausted => {
            GitQueryError::TopologyFailed("traversal budget was exhausted".to_owned())
        }
        error => GitQueryError::TopologyFailed(error.to_string()),
    }
}

/// Build an envelope, folding query-level truncation into the coverage.
fn envelope<T>(value: T, mut coverage: GitCoverageV1, truncated: bool) -> GitQueryEnvelopeV1<T> {
    if truncated {
        coverage.record(GitDegradationV1::TruncatedOutput);
    }
    GitQueryEnvelopeV1 {
        value,
        coverage,
        truncated_by_bound: truncated,
    }
}

/// Measure the serialized result against the query byte bound.
fn check_bytes<T: Serialize>(bounds: &GitQueryBounds, value: &T) -> Result<(), GitQueryError> {
    let actual = serde_json::to_vec(value)
        .map_err(|error| GitQueryError::Serialization(error.to_string()))?
        .len() as u64;
    if actual > bounds.max_bytes {
        return Err(GitQueryError::ByteBoundExceeded {
            bound: bounds.max_bytes,
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::{Command, Output};
    use std::time::Duration;

    use tempfile::TempDir;
    use tracedecay_domain::git::{GitDegradationV1, HunkDirectionV1};
    use tracedecay_domain::research::WorktreeId;

    use crate::git_intelligence::NativeGitIntelligence;

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

        fn head(&self) -> String {
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
                WorktreeId::new("worktree.fixture").unwrap(),
            )
        }
    }

    /// Repo in a conflicted merge state.
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

    fn digest(label: u8) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", format!("{label:02x}").repeat(32))).unwrap()
    }

    #[test]
    fn query_status_summary_matches_adapter() {
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

        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let envelope = engine.status_summary(&GitQueryBounds::default()).unwrap();
        assert!(!envelope.truncated_by_bound);
        assert!(envelope.coverage.is_complete());

        let summary = &envelope.value;
        assert!(matches!(summary.head, GitHeadStateV1::Attached { .. }));
        assert_eq!(summary.operation, GitOperationStateV1::None);
        assert_eq!(summary.schema_version, GIT_QUERY_SCHEMA_VERSION_V1);

        // Differential against the adapter's typed status.
        let status = adapter.status().unwrap();
        assert_eq!(summary.staged as usize, status.staged_count());
        assert_eq!(summary.unstaged as usize, status.unstaged_count());
        assert_eq!(summary.untracked as usize, status.untracked_count());
        assert_eq!(summary.conflicted as usize, status.conflicted_count());
        assert_eq!(
            summary.changed_paths,
            vec!["src/main.txt", "staged.txt", "untracked.txt"]
        );
    }

    #[test]
    fn query_scoped_diff_round_trips_all_scopes() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let base = fixture.head();
        fixture.write(
            "src/main.txt",
            "line1\nchanged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );

        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let bounds = GitQueryBounds::default();

        let worktree = engine
            .scoped_diff(&bounds, &GitDiffScopeV1::WorkingTree)
            .unwrap();
        assert!(!worktree.truncated_by_bound);
        assert_eq!(worktree.value.files_changed(), 1);
        assert_eq!(worktree.value.files[0].change, GitChangeKindV1::Modified);
        assert_eq!(worktree.value.files[0].hunks.len(), 1);

        // Stage the change, then the staged scope sees it and the working
        // tree scope is empty.
        fixture.git_ok(&["add", "src/main.txt"]);
        let staged = engine
            .scoped_diff(&bounds, &GitDiffScopeV1::Staged)
            .unwrap();
        assert_eq!(staged.value.files_changed(), 1);
        let empty = engine
            .scoped_diff(&bounds, &GitDiffScopeV1::WorkingTree)
            .unwrap();
        assert_eq!(empty.value.files_changed(), 0);

        // Commit and diff the exact range through the query layer.
        let head = fixture.commit_all("second");
        let range = engine
            .scoped_diff(
                &bounds,
                &GitDiffScopeV1::CommitRange {
                    base: GitOidV1::new(base).unwrap(),
                    head: GitOidV1::new(head).unwrap(),
                },
            )
            .unwrap();
        assert_eq!(range.value.files_changed(), 1);
        range.value.validate().unwrap();
    }

    #[test]
    fn query_bounded_history_clamps_and_reports_truncation() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write("second.txt", "two\n");
        fixture.commit_all("second");
        fixture.write("third.txt", "three\n");
        fixture.commit_all("third");

        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);

        let full = engine
            .bounded_history(&GitQueryBounds::default(), &GitHistoryRequest::default())
            .unwrap();
        assert_eq!(full.value.commits.len(), 3);
        assert!(!full.truncated_by_bound);

        // The query entry bound clamps the walk before it starts.
        let bounds = GitQueryBounds {
            max_entries: 2,
            ..GitQueryBounds::default()
        };
        let bounded = engine
            .bounded_history(
                &bounds,
                &GitHistoryRequest {
                    max_count: GIT_HISTORY_MAX_COUNT_LIMIT,
                    ..GitHistoryRequest::default()
                },
            )
            .unwrap();
        assert_eq!(bounded.value.commits.len(), 2);
        assert!(bounded.value.truncated);
        assert!(bounded.truncated_by_bound);
        assert!(bounded.coverage.records(GitDegradationV1::TruncatedOutput));
    }

    #[test]
    fn query_path_blame_round_trips() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write(
            "src/main.txt",
            "line1\nchanged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );
        fixture.commit_all("change line 2");

        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let envelope = engine
            .path_blame(
                &GitQueryBounds::default(),
                &GitBlameRequest {
                    path: "src/main.txt".to_owned(),
                    follow_renames: true,
                },
            )
            .unwrap();
        assert!(!envelope.truncated_by_bound);
        assert!(envelope.value.is_available());
        assert_eq!(envelope.value.lines.len(), 8);
        envelope.value.validate().unwrap();

        // Line-level entry bound truncates truthfully.
        let bounds = GitQueryBounds {
            max_entries: 3,
            ..GitQueryBounds::default()
        };
        let truncated = engine
            .path_blame(
                &bounds,
                &GitBlameRequest {
                    path: "src/main.txt".to_owned(),
                    follow_renames: false,
                },
            )
            .unwrap();
        assert_eq!(truncated.value.lines.len(), 3);
        assert!(truncated.truncated_by_bound);
        assert!(
            truncated
                .coverage
                .records(GitDegradationV1::TruncatedOutput)
        );
        truncated.value.validate().unwrap();
    }

    #[test]
    fn generation_join_classifies_behind_head() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let generation_head = fixture.head();
        fixture.write("second.txt", "two\n");
        let current_head = fixture.commit_all("second");

        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let query = GenerationBoundGitQueryV1::new(
            CodeGenerationId::new("generation.fixture.behind").unwrap(),
            Some(GitOidV1::new(generation_head.clone()).unwrap()),
            None,
        );
        let join = engine
            .join_generation(&GitQueryBounds::default(), &query)
            .unwrap();
        assert!(!join.is_current());
        assert_eq!(
            join.staleness,
            GenerationStalenessV1::GenerationBehindHead {
                claimed: GitOidV1::new(generation_head).unwrap(),
                current: GitOidV1::new(current_head.clone()).unwrap(),
            }
        );
        assert_eq!(
            join.evidence.head_oid,
            Some(GitOidV1::new(current_head).unwrap())
        );
    }

    #[test]
    fn generation_join_classifies_history_rewritten() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let pre_rewrite_head = fixture.head();
        // Amend rewrites HEAD: the pre-amend commit is no longer reachable.
        fixture.git_ok(&["commit", "--amend", "-m", "rewritten"]);

        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let query = GenerationBoundGitQueryV1::new(
            CodeGenerationId::new("generation.fixture.rewritten").unwrap(),
            Some(GitOidV1::new(pre_rewrite_head.clone()).unwrap()),
            None,
        );
        let join = engine
            .join_generation(&GitQueryBounds::default(), &query)
            .unwrap();
        assert_eq!(
            join.staleness,
            GenerationStalenessV1::HistoryRewritten {
                claimed_head: GitOidV1::new(pre_rewrite_head).unwrap(),
            }
        );
    }

    #[test]
    fn generation_join_classifies_worktree_diverged() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);

        // Capture evidence at the clean state as the generation's claim.
        let claimed = engine
            .revision_evidence(&GitQueryBounds::default())
            .unwrap();

        // Diverge the worktree without moving HEAD.
        fixture.write(
            "src/main.txt",
            "line1\ndiverged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );

        let query = GenerationBoundGitQueryV1::new(
            CodeGenerationId::new("generation.fixture.diverged").unwrap(),
            claimed.head_oid.clone(),
            Some(claimed.worktree_digest.clone()),
        );
        let join = engine
            .join_generation(&GitQueryBounds::default(), &query)
            .unwrap();
        assert_eq!(
            join.staleness,
            GenerationStalenessV1::WorktreeDiverged {
                claimed: claimed.worktree_digest.clone(),
                current: join.evidence.worktree_digest.clone(),
            }
        );
        assert_ne!(
            join.evidence.worktree_digest, claimed.worktree_digest,
            "worktree edits must move the digest while HEAD stays put"
        );
        assert_eq!(join.evidence.head_oid, claimed.head_oid);
    }

    #[test]
    fn generation_join_current_when_claims_match() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let evidence = engine
            .revision_evidence(&GitQueryBounds::default())
            .unwrap();

        let query = GenerationBoundGitQueryV1::new(
            CodeGenerationId::new("generation.fixture.current").unwrap(),
            evidence.head_oid.clone(),
            Some(evidence.worktree_digest.clone()),
        );
        let join = engine
            .join_generation(&GitQueryBounds::default(), &query)
            .unwrap();
        assert!(join.is_current());
        assert_eq!(join.staleness, GenerationStalenessV1::Current);
        assert_eq!(join.generation_id.as_str(), "generation.fixture.current");
    }

    #[test]
    fn hunk_ref_mint_through_query_path() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write(
            "src/main.txt",
            "line1\nchanged\nline3\nline4\nline5\nline6\nline7\nline8\n",
        );

        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let snapshot_digest = digest(0xaa);
        let envelope = engine
            .hunk_refs(
                &GitQueryBounds::default(),
                &GitDiffScopeV1::WorkingTree,
                "preview.query",
                &snapshot_digest,
            )
            .unwrap();
        assert!(!envelope.truncated_by_bound);
        assert_eq!(envelope.value.len(), 1);
        let reference = &envelope.value[0];
        reference.validate().unwrap();
        assert_eq!(reference.direction, HunkDirectionV1::WorkingTreeToIndex);
        assert_eq!(reference.path, "src/main.txt");
        assert_eq!(reference.preview_id, "preview.query");
        reference
            .verify_digest(&reference.compute_digest().unwrap())
            .unwrap();

        // Identity matches a direct adapter mint through the port.
        let direct = adapter
            .hunk_refs(
                &GitDiffScopeV1::WorkingTree,
                "preview.query",
                &snapshot_digest,
            )
            .unwrap();
        assert_eq!(
            reference.compute_digest().unwrap(),
            direct[0].compute_digest().unwrap()
        );
    }

    #[test]
    fn conflicted_fixture_degradation_propagates() {
        let Some(fixture) = conflicted_fixture() else {
            return;
        };
        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let bounds = GitQueryBounds::default();

        let summary = engine.status_summary(&bounds).unwrap();
        assert_eq!(summary.value.conflicted, 1);
        assert_eq!(summary.value.operation, GitOperationStateV1::Merge);
        assert!(summary.coverage.records(GitDegradationV1::ConflictedState));

        let diff = engine
            .scoped_diff(&bounds, &GitDiffScopeV1::WorkingTree)
            .unwrap();
        assert!(diff.coverage.records(GitDegradationV1::ConflictedState));

        let evidence = engine.revision_evidence(&bounds).unwrap();
        assert!(evidence.coverage.records(GitDegradationV1::ConflictedState));
        assert!(
            evidence
                .coverage
                .records(GitDegradationV1::InProgressOperation)
        );
    }

    #[test]
    fn entry_bound_truncates_diff_and_records_degradation() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write("a.txt", "a1\n");
        fixture.write("b.txt", "b1\n");
        fixture.commit_all("add two files");
        fixture.write("a.txt", "a2\n");
        fixture.write("b.txt", "b2\n");

        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let bounds = GitQueryBounds {
            max_entries: 1,
            ..GitQueryBounds::default()
        };
        let envelope = engine
            .scoped_diff(&bounds, &GitDiffScopeV1::WorkingTree)
            .unwrap();
        assert_eq!(envelope.value.files.len(), 1);
        assert!(envelope.truncated_by_bound);
        assert!(envelope.coverage.records(GitDegradationV1::TruncatedOutput));
        envelope.value.validate().unwrap();
    }

    #[test]
    fn byte_bound_fails_truthfully() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        fixture.write("src/main.txt", "line1\nchanged\nline3\n");
        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let bounds = GitQueryBounds {
            max_bytes: 8,
            ..GitQueryBounds::default()
        };
        let result = engine.scoped_diff(&bounds, &GitDiffScopeV1::WorkingTree);
        assert!(matches!(
            result,
            Err(GitQueryError::ByteBoundExceeded { bound: 8, .. })
        ));
    }

    #[test]
    fn cancellation_short_circuits_queries() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let flag = Arc::new(AtomicBool::new(true));
        let bounds = GitQueryBounds {
            cancel: Some(flag),
            ..GitQueryBounds::default()
        };
        assert!(matches!(
            engine.status_summary(&bounds),
            Err(GitQueryError::Cancelled)
        ));
        assert!(matches!(
            engine.scoped_diff(&bounds, &GitDiffScopeV1::WorkingTree),
            Err(GitQueryError::Cancelled)
        ));
    }

    #[test]
    fn expired_deadline_fails_before_adapter_call() {
        let Some(fixture) = Fixture::standard() else {
            return;
        };
        let adapter = fixture.adapter();
        let engine = GitQueryEngine::new(&adapter);
        let bounds = GitQueryBounds {
            deadline: Instant::now().checked_sub(Duration::from_secs(1)),
            ..GitQueryBounds::default()
        };
        assert!(matches!(
            engine.bounded_history(&bounds, &GitHistoryRequest::default()),
            Err(GitQueryError::DeadlineExceeded)
        ));
        assert!(matches!(
            engine.revision_evidence(&bounds),
            Err(GitQueryError::DeadlineExceeded)
        ));
    }

    #[test]
    fn query_types_roundtrip_through_serde() {
        let join = GenerationGitJoinV1 {
            generation_id: CodeGenerationId::new("generation.fixture.serde").unwrap(),
            evidence: GitRevisionEvidenceV1 {
                repository: RepositoryId::new("repository.fixture").unwrap(),
                head: GitHeadStateV1::Attached {
                    branch: "main".to_owned(),
                    commit: GitOidV1::new("a".repeat(40)).unwrap(),
                },
                head_oid: Some(GitOidV1::new("a".repeat(40)).unwrap()),
                worktree_digest: digest(0xbb),
                coverage: GitCoverageV1::degraded(vec![GitDegradationV1::TruncatedOutput]),
                schema_version: GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
            },
            staleness: GenerationStalenessV1::GenerationBehindHead {
                claimed: GitOidV1::new("b".repeat(40)).unwrap(),
                current: GitOidV1::new("a".repeat(40)).unwrap(),
            },
            coverage: GitCoverageV1::complete(),
            schema_version: GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
        };
        let wire = serde_json::to_string(&join).unwrap();
        assert_eq!(
            serde_json::from_str::<GenerationGitJoinV1>(&wire).unwrap(),
            join
        );

        let query = GenerationBoundGitQueryV1::new(
            CodeGenerationId::new("generation.fixture.serde").unwrap(),
            None,
            None,
        );
        let wire = serde_json::to_string(&query).unwrap();
        assert_eq!(
            serde_json::from_str::<GenerationBoundGitQueryV1>(&wire).unwrap(),
            query
        );

        let summary = GitStatusSummaryV1 {
            repository: RepositoryId::new("repository.fixture").unwrap(),
            head: GitHeadStateV1::Unborn {
                branch: "main".to_owned(),
            },
            operation: GitOperationStateV1::None,
            staged: 1,
            unstaged: 2,
            conflicted: 0,
            untracked: 3,
            ignored: 1,
            changed_paths: vec!["a.txt".to_owned()],
            schema_version: GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
        };
        let wire = serde_json::to_string(&summary).unwrap();
        assert_eq!(
            serde_json::from_str::<GitStatusSummaryV1>(&wire).unwrap(),
            summary
        );
    }
}
