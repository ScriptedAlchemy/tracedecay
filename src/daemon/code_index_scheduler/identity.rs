//! Typed indexing-identity resolution for the code-index scheduler.
//!
//! Paths and branch labels only *locate* candidate work. They never provide
//! identity and never authorize reuse of another worktree's parse, chunk, or
//! generation artifacts. Every reconciliation resolves an exact identity bundle
//! *before* indexing, and cross-worktree reuse is refused unless the resolved
//! repository and worktree identities match bit-for-bit.
//!
//! Git-metadata fingerprints (`.git/HEAD`, `.git/index`, `packed-refs`) are a
//! cheap staleness signal only. They tell the scheduler *when* to re-run truth
//! (gix status + identity resolution); they are never themselves the changed
//! content authority.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::{CommitId, RefId, RepositoryId, TreeId, WorktreeId};

/// Failure to resolve an exact indexing identity from a checkout.
#[derive(Debug, thiserror::Error)]
pub(crate) enum IdentityErrorV1 {
    #[error("code-index identity: repository open failed: {0}")]
    Git(String),
    #[error("code-index identity: canonical id construction failed: {0}")]
    Domain(String),
}

/// The exact project/repository/worktree/ref/commit/tree identity of one
/// checkout, resolved before any indexing work runs.
///
/// `repository_id` and `worktree_id` are stable structural identity. The
/// `head_*` fields describe the *current* source revision the worktree points
/// at; they may legitimately move under the same worktree (commit, checkout,
/// rebase) without changing reuse authorization, but a move is recorded so a
/// served generation is never silently mis-attributed to a newer revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexingIdentityV1 {
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    head_ref: Option<RefId>,
    head_commit: Option<CommitId>,
    head_tree: Option<TreeId>,
}

impl IndexingIdentityV1 {
    /// Resolve the exact indexing identity of `project_root` through gix.
    ///
    /// The repository identity is anchored on the *common* git directory so all
    /// linked worktrees of one repository share it, while the worktree identity
    /// is anchored on the canonical checkout path so linked worktrees never
    /// collapse into one another.
    pub(crate) fn resolve(project_root: &Path) -> Result<Self, IdentityErrorV1> {
        let repository_id = repository_id_for(project_root)?;
        let worktree_id = worktree_id_for(project_root)?;

        // HEAD/commit/tree are best-effort: an unborn or detached HEAD is a
        // truthful `None`, never a fabricated placeholder.
        let repository =
            gix::open(project_root).map_err(|error| IdentityErrorV1::Git(error.to_string()))?;
        let head_ref = repository
            .head()
            .ok()
            .filter(|head| !head.is_detached())
            .and_then(|head| {
                head.referent_name()
                    .and_then(|name| std::str::from_utf8(name.as_bstr()).ok())
                    .map(str::to_owned)
            })
            .and_then(|name| RefId::new(name).ok());
        let head_commit = repository
            .head_commit()
            .ok()
            .and_then(|commit| CommitId::new(commit.id().to_hex().to_string()).ok());
        let head_tree = repository
            .head_commit()
            .ok()
            .and_then(|commit| commit.tree_id().ok())
            .and_then(|tree| TreeId::new(tree.to_hex().to_string()).ok());

        Ok(Self {
            repository_id,
            worktree_id,
            head_ref,
            head_commit,
            head_tree,
        })
    }

    pub(crate) fn repository_id(&self) -> &RepositoryId {
        &self.repository_id
    }

    pub(crate) fn worktree_id(&self) -> &WorktreeId {
        &self.worktree_id
    }

    pub(crate) fn head_ref(&self) -> Option<&RefId> {
        self.head_ref.as_ref()
    }

    pub(crate) fn head_commit(&self) -> Option<&CommitId> {
        self.head_commit.as_ref()
    }

    pub(crate) fn head_tree(&self) -> Option<&TreeId> {
        self.head_tree.as_ref()
    }

    /// Whether artifacts produced under `prior` may be reused by a build
    /// resolved to `self`.
    ///
    /// Reuse authorization is **structural only**: the repository and the
    /// worktree must be the same identity. A different worktree — even one whose
    /// path or branch label looks identical, or whose blobs are byte-identical —
    /// is never authorized to reuse another's generation, occurrence, or lineage
    /// identity. Content-addressed *bytes* may still be physically shared by the
    /// byte pool; this guard governs *identity* reuse, not byte deduplication.
    pub(crate) fn authorizes_reuse_of(&self, prior: &IndexingIdentityV1) -> bool {
        self.repository_id == prior.repository_id && self.worktree_id == prior.worktree_id
    }

    /// Whether `self` and `other` point at the same source revision. Used to
    /// decide whether a previously published generation is still current for the
    /// resolved HEAD or is stale-but-correctly-attributed to a prior revision.
    pub(crate) fn same_source_revision(&self, other: &IndexingIdentityV1) -> bool {
        self.head_commit == other.head_commit && self.head_tree == other.head_tree
    }

    /// A stable, structural identity key derived from the repository and
    /// worktree identities only. Source revision (HEAD/commit/tree) is
    /// deliberately excluded so the key is invariant across commits within one
    /// worktree, while remaining distinct for every other worktree — even a
    /// byte-identical one. Generations are keyed off this value so
    /// cross-worktree reuse is structurally impossible, not merely checked.
    pub(crate) fn identity_key(&self) -> String {
        format!(
            "sha256:{}",
            super::sha256_hex(
                format!(
                    "{}\0{}",
                    self.repository_id.as_str(),
                    self.worktree_id.as_str()
                )
                .as_bytes()
            )
        )
    }
}

/// Cheap `.git`-metadata staleness fingerprint (tier-1 of the freshness ladder).
///
/// Captures the modification times of the handful of git metadata files that
/// change on every git-mediated mutation (commit, checkout, rebase, pull,
/// fetch, ref update). Its cost is fixed and independent of repository size, so
/// it can be sampled on every query admission without a filesystem watcher.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GitMetadataFingerprintV1 {
    head: Option<SystemTime>,
    index: Option<SystemTime>,
    packed_refs: Option<SystemTime>,
    /// A content signature over the loose `refs/heads` tree: for every ref file
    /// (recursively), its relative name, mtime, and size. A directory mtime
    /// alone misses an *in-place* loose-ref rewrite (`git update-ref` on an
    /// existing branch rewrites the file without changing the parent
    /// directory's mtime), so the per-file signal is required to observe it.
    refs_heads: Option<String>,
    /// The literal contents of `.git/HEAD` (e.g. `ref: refs/heads/main`). A
    /// branch switch changes this even when mtime resolution is too coarse to
    /// observe, so content is compared alongside mtimes.
    head_contents: Option<String>,
}

impl GitMetadataFingerprintV1 {
    /// Sample the current git-metadata fingerprint for `project_root`.
    ///
    /// Missing files (e.g. no `packed-refs` yet) are recorded as `None`, which
    /// still participates in change detection: a file appearing or disappearing
    /// is itself a change.
    pub(crate) fn capture(project_root: &Path) -> Self {
        let (git_dir, common_dir) = git_metadata_dirs(project_root);
        let head_path = git_dir.join("HEAD");
        Self {
            head: mtime(&head_path),
            index: mtime(&git_dir.join("index")),
            packed_refs: mtime(&common_dir.join("packed-refs")),
            refs_heads: refs_heads_signature(&common_dir.join("refs").join("heads")),
            head_contents: std::fs::read_to_string(&head_path).ok(),
        }
    }

    /// Whether the sampled metadata differs from a previously captured value.
    pub(crate) fn differs_from(&self, other: &GitMetadataFingerprintV1) -> bool {
        self != other
    }

    /// A stable, order-independent textual signature of this fingerprint.
    ///
    /// Used to persist a restore-time freshness witness: two fingerprints share
    /// a signature iff they are structurally equal (`PartialEq`), so comparing
    /// the recomputed signature against a witness recorded at seal time is a
    /// faithful stand-in for `differs_from`. `SystemTime` mtimes are rendered as
    /// nanos-since-epoch so the encoding is deterministic across processes.
    pub(crate) fn stable_signature(&self) -> String {
        fn render_time(time: Option<SystemTime>) -> String {
            time.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or_else(|| "-".to_owned(), |elapsed| elapsed.as_nanos().to_string())
        }
        format!(
            "head={}|index={}|packed_refs={}|refs_heads={}|head_contents={}",
            render_time(self.head),
            render_time(self.index),
            render_time(self.packed_refs),
            self.refs_heads.as_deref().unwrap_or("-"),
            self.head_contents.as_deref().map_or_else(
                || "-".to_owned(),
                |value| super::sha256_hex(value.as_bytes())
            ),
        )
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// Cheap content signature over the loose `refs/heads` tree.
///
/// Walks the tree recursively (branch refs can be nested, e.g.
/// `refs/heads/feature/x`), hashing each ref's relative name together with its
/// content (the target object id). Loose refs are a handful of ~41-byte files,
/// so this is O(number of loose refs) and independent of repository size.
///
/// Content — not the parent directory's mtime — is the signal: an in-place
/// loose-ref rewrite (`git update-ref` on an existing branch) rewrites the ref
/// file's bytes without changing the directory mtime, and often without
/// changing the file's size (object ids are fixed width) or its mtime beyond
/// coarse filesystem resolution. Hashing the bytes catches it unconditionally.
/// Returns `None` when the tree is absent (all refs packed, or not a
/// repository).
fn refs_heads_signature(dir: &Path) -> Option<String> {
    if !dir.exists() {
        return None;
    }
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let content = std::fs::read(&path).unwrap_or_default();
            entries.push((rel, content));
        }
    }
    entries.sort();
    let mut buffer = Vec::new();
    for (name, content) in entries {
        buffer.extend_from_slice(name.as_bytes());
        buffer.push(0);
        buffer.extend_from_slice(&content);
        buffer.push(0xff);
    }
    Some(super::sha256_hex(&buffer))
}

/// Resolve the git-dir (worktree-local) and common-dir (repository-shared)
/// paths, falling back to `<root>/.git` when gix cannot open the checkout so a
/// non-repository path still yields a stable, if empty, fingerprint.
fn git_metadata_dirs(project_root: &Path) -> (PathBuf, PathBuf) {
    if let Ok(repository) = gix::open(project_root) {
        let git_dir = repository.git_dir().to_path_buf();
        let common_dir = {
            let common = repository.common_dir().to_path_buf();
            if common.is_absolute() {
                common
            } else {
                git_dir.join(common)
            }
        };
        return (git_dir, common_dir);
    }
    let git_dir = project_root.join(".git");
    (git_dir.clone(), git_dir)
}

pub(crate) fn repository_id_for(project_root: &Path) -> Result<RepositoryId, IdentityErrorV1> {
    let common =
        crate::worktree::git_common_dir(project_root).unwrap_or_else(|| project_root.to_path_buf());
    let digest = super::sha256_hex(common.to_string_lossy().as_bytes());
    RepositoryId::new(format!("repository.daemon.{digest}"))
        .map_err(|error| IdentityErrorV1::Domain(error.to_string()))
}

pub(crate) fn worktree_id_for(project_root: &Path) -> Result<WorktreeId, IdentityErrorV1> {
    WorktreeId::new(format!(
        "worktree.daemon.{}",
        super::sha256_hex(project_root.to_string_lossy().as_bytes())
    ))
    .map_err(|error| IdentityErrorV1::Domain(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git failed: {args:?}");
    }

    fn init_repo(files: &[(&str, &str)]) -> TempDir {
        let root = TempDir::new().expect("root");
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["config", "user.name", "T"]);
        git(root.path(), &["config", "user.email", "t@example.invalid"]);
        for (path, source) in files {
            let full = root.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, source).unwrap();
        }
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "init"]);
        root
    }

    #[test]
    fn resolves_repository_worktree_and_head() {
        let repo = init_repo(&[("src/lib.rs", "pub fn a() {}\n")]);
        let identity = IndexingIdentityV1::resolve(repo.path()).expect("resolve");
        assert!(
            identity
                .repository_id()
                .as_str()
                .starts_with("repository.daemon."),
            "repository id is anchored on the common dir"
        );
        assert!(
            identity
                .worktree_id()
                .as_str()
                .starts_with("worktree.daemon."),
            "worktree id is anchored on the checkout path"
        );
        assert!(identity.head_commit().is_some(), "committed HEAD resolves");
        assert!(identity.head_tree().is_some(), "committed HEAD has a tree");
    }

    #[test]
    fn distinct_checkouts_never_authorize_reuse_even_with_identical_content() {
        let first = init_repo(&[("src/lib.rs", "pub fn shared() {}\n")]);
        let second = init_repo(&[("src/lib.rs", "pub fn shared() {}\n")]);
        let a = IndexingIdentityV1::resolve(first.path()).expect("first");
        let b = IndexingIdentityV1::resolve(second.path()).expect("second");
        assert!(
            !a.authorizes_reuse_of(&b),
            "byte-identical but distinct worktrees must refuse identity reuse"
        );
        assert!(!b.authorizes_reuse_of(&a));
        assert!(a.authorizes_reuse_of(&a), "self reuse is authorized");
    }

    #[test]
    fn head_move_changes_source_revision_but_not_reuse_authorization() {
        let repo = init_repo(&[("src/lib.rs", "pub fn a() {}\n")]);
        let before = IndexingIdentityV1::resolve(repo.path()).expect("before");
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn a() { let _ = 1; }\n",
        )
        .unwrap();
        git(repo.path(), &["commit", "-qam", "second"]);
        let after = IndexingIdentityV1::resolve(repo.path()).expect("after");
        assert!(
            after.authorizes_reuse_of(&before),
            "same worktree still authorizes reuse across a HEAD move"
        );
        assert!(
            !after.same_source_revision(&before),
            "a HEAD move is a different source revision"
        );
    }

    #[test]
    fn git_metadata_fingerprint_detects_a_commit() {
        let repo = init_repo(&[("src/lib.rs", "pub fn a() {}\n")]);
        let before = GitMetadataFingerprintV1::capture(repo.path());
        // Ensure the mtime clock advances on filesystems with coarse resolution.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn a() { let _ = 1; }\n",
        )
        .unwrap();
        git(repo.path(), &["commit", "-qam", "second"]);
        let after = GitMetadataFingerprintV1::capture(repo.path());
        assert!(
            after.differs_from(&before),
            "a commit moves .git/HEAD or refs and must be detected"
        );
    }

    #[test]
    fn git_metadata_fingerprint_detects_in_place_loose_ref_rewrite() {
        let repo = init_repo(&[("src/lib.rs", "pub fn a() {}\n")]);
        // Two commits so an older object id exists to rewind the branch to.
        let first = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(repo.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .expect("utf8 sha")
        .trim()
        .to_owned();
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn a() { let _ = 1; }\n",
        )
        .unwrap();
        git(repo.path(), &["commit", "-qam", "second"]);
        let branch = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(repo.path())
                .args(["symbolic-ref", "--short", "HEAD"])
                .output()
                .expect("symbolic-ref")
                .stdout,
        )
        .expect("utf8 branch")
        .trim()
        .to_owned();

        let before = GitMetadataFingerprintV1::capture(repo.path());
        // Rewrite the current branch ref in place to the older commit without
        // touching HEAD or the index. This changes only the loose ref file's
        // bytes; the parent directory mtime and the 41-byte size are unchanged,
        // so only a content-aware refs/heads signature can observe it.
        git(
            repo.path(),
            &["update-ref", &format!("refs/heads/{branch}"), &first],
        );
        let after = GitMetadataFingerprintV1::capture(repo.path());
        assert!(
            after.differs_from(&before),
            "an in-place loose-ref rewrite must be detected by the refs signature"
        );
    }
}
