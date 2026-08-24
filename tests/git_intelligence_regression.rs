//! Named acceptance matrix for the typed read-only Git query surface.
//!
//! Every case here drives real native Git in an isolated temporary repository
//! through the same two layers a product surface uses: the fixed read-only
//! adapter (`NativeGitIntelligence`) and the generation-aware query engine
//! (`GitQueryEngine`). Nothing is mocked, and no daemon, store, or project
//! runtime is mounted — these are application/use-case reads only.
//!
//! The matrix covers the Git-query acceptance listed in
//! `docs/plans/tracedecay-v2/05-query-crate.md`: working-tree, staged, and
//! committed-range diffs kept distinct; rename detection; deletion; binary
//! classification without a text diff; merge history; blame across two
//! authorship layers; hunk queries whose `HunkRef` identity replays
//! identically after the ref moves; and the dual-provenance rule that a Git
//! watermark disagreeing with a code-generation watermark is reported as
//! typed staleness rather than silently merged.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use tracedecay::git_intelligence::{
    GitBlameRequest, GitHistoryRequest, GitIntelligenceError, NativeGitIntelligence,
};
use tracedecay::git_query::{
    GenerationBoundGitQueryV1, GenerationStalenessV1, GitQueryBounds, GitQueryEngine, GitQueryError,
};
use tracedecay_domain::CodeGenerationId;
use tracedecay_domain::git::{
    GitBlameAvailabilityV1, GitChangeKindV1, GitDiffScopeV1, GitOidV1, GitOperationStateV1,
    HunkDirectionV1,
};
use tracedecay_domain::research::{ManifestDigest, RepositoryId, WorktreeId};

/// An isolated repository fixture driven by the real `git` executable.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn git_available() -> bool {
        Command::new(tracedecay_runtime_core::git::git_program())
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn init() -> Option<Self> {
        if !Self::git_available() {
            return None;
        }
        let fixture = Self {
            dir: TempDir::new().unwrap(),
        };
        fixture.git_ok(&["init", "-b", "main"]);
        Some(fixture)
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git_as(&self, name: &str, email: &str, args: &[&str]) -> Output {
        Command::new(tracedecay_runtime_core::git::git_program())
            .args([
                "-c",
                &format!("user.name={name}"),
                "-c",
                &format!("user.email={email}"),
                "-c",
                "commit.gpgsign=false",
                "-c",
                "merge.ff=false",
            ])
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("git spawn failed")
    }

    fn git_ok_as(&self, name: &str, email: &str, args: &[&str]) -> String {
        let output = self.git_as(name, email, args);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn git_ok(&self, args: &[&str]) -> String {
        self.git_ok_as("Fixture", "fixture@example.com", args)
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn write_bytes(&self, rel: &str, content: &[u8]) {
        let path = self.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn commit_all(&self, message: &str) -> String {
        self.commit_all_as("Fixture", "fixture@example.com", message)
    }

    fn commit_all_as(&self, name: &str, email: &str, message: &str) -> String {
        self.git_ok_as(name, email, &["add", "-A"]);
        self.git_ok_as(name, email, &["commit", "-m", message]);
        self.head_oid()
    }

    fn head_oid(&self) -> String {
        self.git_ok(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn adapter(&self) -> NativeGitIntelligence {
        NativeGitIntelligence::new(
            self.path(),
            RepositoryId::new("repository.fixture").unwrap(),
            WorktreeId::new("worktree.fixture").unwrap(),
        )
    }
}

fn oid(value: &str) -> GitOidV1 {
    GitOidV1::new(value.trim()).unwrap()
}

fn snapshot_digest(fill: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
}

/// Paths of a typed diff, sorted for order-independent comparison.
fn diff_paths(diff: &tracedecay_domain::git::GitDiffV1) -> Vec<String> {
    let mut paths: Vec<String> = diff.files.iter().map(|file| file.path.clone()).collect();
    paths.sort();
    paths
}

#[test]
fn status_reports_sequencer_directory() {
    let repository = TempDir::new().unwrap();
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(repository.path())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::create_dir(repository.path().join(".git/sequencer")).unwrap();
    let adapter = NativeGitIntelligence::new(
        repository.path(),
        RepositoryId::new("repository.fixture").unwrap(),
        WorktreeId::new("worktree.fixture").unwrap(),
    );

    let status = adapter.status().unwrap();

    assert_eq!(status.operation, GitOperationStateV1::Sequencer);
}

/// Working tree, index, and an exact commit range are three different
/// questions. A change made in one must not leak into the answers of the
/// other two.
#[test]
fn working_staged_and_committed_range_diffs_stay_distinct() {
    let Some(fixture) = Fixture::init() else {
        return;
    };
    fixture.write("alpha.txt", "alpha-1\n");
    fixture.write("beta.txt", "beta-1\n");
    fixture.write("gamma.txt", "gamma-1\n");
    let base = fixture.commit_all("initial");

    fixture.write("gamma.txt", "gamma-2\n");
    let head = fixture.commit_all("committed change to gamma");

    // Staged-only change.
    fixture.write("beta.txt", "beta-2\n");
    fixture.git_ok(&["add", "beta.txt"]);
    // Unstaged-only change.
    fixture.write("alpha.txt", "alpha-2\n");

    let adapter = fixture.adapter();
    let engine = GitQueryEngine::new(&adapter);
    let bounds = GitQueryBounds::default();

    let working = engine
        .scoped_diff(&bounds, &GitDiffScopeV1::WorkingTree)
        .unwrap();
    let staged = engine
        .scoped_diff(&bounds, &GitDiffScopeV1::Staged)
        .unwrap();
    let range = engine
        .scoped_diff(
            &bounds,
            &GitDiffScopeV1::CommitRange {
                base: oid(&base),
                head: oid(&head),
            },
        )
        .unwrap();

    working.value.validate().unwrap();
    staged.value.validate().unwrap();
    range.value.validate().unwrap();

    assert_eq!(diff_paths(&working.value), vec!["alpha.txt".to_owned()]);
    assert_eq!(diff_paths(&staged.value), vec!["beta.txt".to_owned()]);
    assert_eq!(diff_paths(&range.value), vec!["gamma.txt".to_owned()]);

    // Each envelope carries the scope it answered, so a caller cannot mistake
    // one view for another after the fact.
    assert_eq!(working.value.scope, GitDiffScopeV1::WorkingTree);
    assert_eq!(staged.value.scope, GitDiffScopeV1::Staged);
    assert_eq!(
        range.value.scope,
        GitDiffScopeV1::CommitRange {
            base: oid(&base),
            head: oid(&head),
        }
    );
    for envelope in [&working, &staged, &range] {
        assert!(!envelope.truncated_by_bound);
        assert!(envelope.coverage.is_complete(), "{:?}", envelope.coverage);
    }
}

/// Native rename detection has to survive the typed projection: the new path,
/// the original path, and the `Renamed` kind all reach the caller.
#[test]
fn rename_detection_is_carried_through_the_typed_diff() {
    let Some(fixture) = Fixture::init() else {
        return;
    };
    fixture.write(
        "src/original.txt",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n",
    );
    let base = fixture.commit_all("initial");
    fixture.git_ok(&["mv", "src/original.txt", "src/moved.txt"]);

    let adapter = fixture.adapter();
    let engine = GitQueryEngine::new(&adapter);
    let bounds = GitQueryBounds::default();

    let staged = engine
        .scoped_diff(&bounds, &GitDiffScopeV1::Staged)
        .unwrap();
    staged.value.validate().unwrap();
    assert_eq!(staged.value.files_changed(), 1);
    let renamed = &staged.value.files[0];
    assert_eq!(renamed.change, GitChangeKindV1::Renamed);
    assert_eq!(renamed.path, "src/moved.txt");
    assert_eq!(renamed.original_path.as_deref(), Some("src/original.txt"));
    // A pure rename keeps blob identity on both sides.
    assert_eq!(renamed.old_blob, renamed.new_blob);

    // The same rename survives a committed range read, where there is no index
    // relationship at all.
    let head = fixture.commit_all("rename original to moved");
    let range = engine
        .scoped_diff(
            &bounds,
            &GitDiffScopeV1::CommitRange {
                base: oid(&base),
                head: oid(&head),
            },
        )
        .unwrap();
    range.value.validate().unwrap();
    assert_eq!(range.value.files_changed(), 1);
    assert_eq!(range.value.files[0].change, GitChangeKindV1::Renamed);
    assert_eq!(
        range.value.files[0].original_path.as_deref(),
        Some("src/original.txt")
    );
}

/// A deleted file is a typed `Deleted` record with an absent new side, not a
/// modification that happens to remove every line.
#[test]
fn deletion_is_typed_with_an_absent_new_side() {
    let Some(fixture) = Fixture::init() else {
        return;
    };
    fixture.write("doomed.txt", "one\ntwo\nthree\n");
    fixture.write("kept.txt", "kept\n");
    let base = fixture.commit_all("initial");
    std::fs::remove_file(fixture.path().join("doomed.txt")).unwrap();

    let adapter = fixture.adapter();
    let engine = GitQueryEngine::new(&adapter);
    let bounds = GitQueryBounds::default();

    let working = engine
        .scoped_diff(&bounds, &GitDiffScopeV1::WorkingTree)
        .unwrap();
    working.value.validate().unwrap();
    assert_eq!(diff_paths(&working.value), vec!["doomed.txt".to_owned()]);
    let deleted = &working.value.files[0];
    assert_eq!(deleted.change, GitChangeKindV1::Deleted);
    assert!(deleted.old_blob.is_some());
    assert_eq!(deleted.new_blob, None);
    assert_eq!(deleted.new_mode, None);
    assert_eq!(deleted.insertions, Some(0));
    assert_eq!(deleted.deletions, Some(3));
    assert_eq!(deleted.hunks.len(), 1);
    // A deletion-only hunk has an empty new side; native Git addresses it by
    // the line after which content would be inserted.
    assert_eq!(deleted.hunks[0].new_lines, 0);
    assert_eq!(deleted.hunks[0].old_lines, 3);

    // The deletion is equally typed once committed and read as a range.
    let head = fixture.commit_all("delete doomed");
    let range = engine
        .scoped_diff(
            &bounds,
            &GitDiffScopeV1::CommitRange {
                base: oid(&base),
                head: oid(&head),
            },
        )
        .unwrap();
    range.value.validate().unwrap();
    assert_eq!(range.value.files[0].change, GitChangeKindV1::Deleted);
    assert_eq!(range.value.files[0].new_blob, None);
}

/// Binary content is classified, never text-diffed: no hunks and no line
/// totals, so no caller can read a line count that does not exist.
#[test]
fn binary_content_is_classified_instead_of_text_diffed() {
    let Some(fixture) = Fixture::init() else {
        return;
    };
    fixture.write_bytes("assets/blob.bin", &[0u8, 159, 146, 150, 0, 1, 2, 3]);
    fixture.write("readme.txt", "text\n");
    let base = fixture.commit_all("initial");
    fixture.write_bytes("assets/blob.bin", &[0u8, 9, 9, 9, 0, 4, 5, 6]);
    fixture.write("readme.txt", "text\nmore text\n");

    let adapter = fixture.adapter();
    let engine = GitQueryEngine::new(&adapter);
    let bounds = GitQueryBounds::default();

    let working = engine
        .scoped_diff(&bounds, &GitDiffScopeV1::WorkingTree)
        .unwrap();
    working.value.validate().unwrap();
    assert_eq!(
        diff_paths(&working.value),
        vec!["assets/blob.bin".to_owned(), "readme.txt".to_owned()]
    );

    let binary = working
        .value
        .files
        .iter()
        .find(|file| file.path == "assets/blob.bin")
        .expect("binary record");
    assert!(binary.binary);
    assert!(!binary.submodule);
    assert!(binary.hunks.is_empty());
    assert_eq!(binary.insertions, None);
    assert_eq!(binary.deletions, None);
    // The committed side is exact blob identity; native Git reports no
    // worktree-side blob for an unhashed working-tree change, and the typed
    // record says `None` instead of inventing one.
    assert!(binary.old_blob.is_some());
    assert_eq!(binary.new_blob, None);

    // The text file beside it keeps full line-level structure, so the binary
    // classification is per file and not a whole-diff downgrade.
    let text = working
        .value
        .files
        .iter()
        .find(|file| file.path == "readme.txt")
        .expect("text record");
    assert!(!text.binary);
    assert_eq!(text.insertions, Some(1));
    assert!(!text.hunks.is_empty());

    // Diff-level totals ignore the binary record rather than inventing zeros
    // for it: one text insertion, no deletions.
    assert_eq!(working.value.insertions(), 1);
    assert_eq!(working.value.deletions(), 0);

    let head = fixture.commit_all("touch binary and text");
    let range = engine
        .scoped_diff(
            &bounds,
            &GitDiffScopeV1::CommitRange {
                base: oid(&base),
                head: oid(&head),
            },
        )
        .unwrap();
    range.value.validate().unwrap();
    let ranged_binary = range
        .value
        .files
        .iter()
        .find(|file| file.path == "assets/blob.bin")
        .expect("binary range record");
    assert!(ranged_binary.binary);
    assert!(ranged_binary.hunks.is_empty());
    // Both sides of a committed range are exact objects, so the binary record
    // still carries full blob identity without ever becoming a text diff.
    assert!(ranged_binary.old_blob.is_some());
    assert!(ranged_binary.new_blob.is_some());
    assert_eq!(ranged_binary.insertions, None);
}

/// A real merge commit must be traversable: both parents are reported, and a
/// first-parent walk answers a different — and smaller — question than the
/// full walk.
#[test]
fn merge_history_traversal_reports_both_parents() {
    let Some(fixture) = Fixture::init() else {
        return;
    };
    fixture.write("trunk.txt", "root\n");
    let root = fixture.commit_all("root");

    fixture.git_ok(&["checkout", "-b", "side"]);
    fixture.write("side.txt", "side\n");
    let side = fixture.commit_all("side work");

    fixture.git_ok(&["checkout", "main"]);
    fixture.write("trunk.txt", "root\ntrunk work\n");
    let trunk = fixture.commit_all("trunk work");

    fixture.git_ok(&["merge", "--no-ff", "-m", "merge side into main", "side"]);
    let merge = fixture.head_oid();
    assert_ne!(merge, trunk);

    let adapter = fixture.adapter();
    let engine = GitQueryEngine::new(&adapter);
    let bounds = GitQueryBounds::default();

    let history = engine
        .bounded_history(&bounds, &GitHistoryRequest::default())
        .unwrap();
    history.value.validate().unwrap();
    assert!(!history.value.truncated);

    let commits: Vec<&str> = history
        .value
        .commits
        .iter()
        .map(|commit| commit.commit.as_str())
        .collect();
    assert_eq!(commits.len(), 4, "root, side, trunk, merge: {commits:?}");
    assert!(commits.contains(&root.as_str()));
    assert!(commits.contains(&side.as_str()));
    assert!(commits.contains(&trunk.as_str()));

    let merge_commit = history
        .value
        .commits
        .iter()
        .find(|commit| commit.commit.as_str() == merge)
        .expect("merge commit in history");
    assert_eq!(merge_commit.subject, "merge side into main");
    let parents: Vec<&str> = merge_commit.parents.iter().map(GitOidV1::as_str).collect();
    assert_eq!(parents.len(), 2, "a merge commit has two parents");
    assert_eq!(parents[0], trunk, "first parent is the merged-into tip");
    assert_eq!(parents[1], side, "second parent is the merged-from tip");

    // A first-parent walk skips the side branch entirely.
    let first_parent = engine
        .bounded_history(
            &bounds,
            &GitHistoryRequest {
                first_parent: true,
                ..GitHistoryRequest::default()
            },
        )
        .unwrap();
    first_parent.value.validate().unwrap();
    let first_parent_commits: Vec<&str> = first_parent
        .value
        .commits
        .iter()
        .map(|commit| commit.commit.as_str())
        .collect();
    assert_eq!(
        first_parent_commits,
        vec![merge.as_str(), trunk.as_str(), root.as_str()]
    );
    assert!(!first_parent_commits.contains(&side.as_str()));

    // The merge introduced the side file into the trunk: a range diff across
    // the merge sees it.
    let range = engine
        .scoped_diff(
            &bounds,
            &GitDiffScopeV1::CommitRange {
                base: oid(&trunk),
                head: oid(&merge),
            },
        )
        .unwrap();
    range.value.validate().unwrap();
    assert_eq!(diff_paths(&range.value), vec!["side.txt".to_owned()]);
}

/// Blame over a file written by two authors attributes each line to the
/// commit and identity that produced it.
#[test]
fn blame_separates_two_authorship_layers() {
    let Some(fixture) = Fixture::init() else {
        return;
    };
    fixture.write("shared.txt", "first-a\nfirst-b\nfirst-c\n");
    let first = fixture.commit_all_as("First Author", "first@example.com", "first layer");

    fixture.write("shared.txt", "first-a\nsecond-b\nfirst-c\nsecond-d\n");
    let second = fixture.commit_all_as("Second Author", "second@example.com", "second layer");
    assert_ne!(first, second);

    let adapter = fixture.adapter();
    let engine = GitQueryEngine::new(&adapter);
    let bounds = GitQueryBounds::default();

    let blame = engine
        .path_blame(
            &bounds,
            &GitBlameRequest {
                path: "shared.txt".to_owned(),
                follow_renames: false,
            },
        )
        .unwrap();
    blame.value.validate().unwrap();
    assert_eq!(blame.value.availability, GitBlameAvailabilityV1::Available);
    assert!(!blame.truncated_by_bound);
    assert_eq!(blame.value.lines.len(), 4);

    let attribution: Vec<(u32, &str, &str)> = blame
        .value
        .lines
        .iter()
        .map(|line| {
            (
                line.final_line,
                line.commit.as_str(),
                line.author.name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        attribution,
        vec![
            (1, first.as_str(), "First Author"),
            (2, second.as_str(), "Second Author"),
            (3, first.as_str(), "First Author"),
            (4, second.as_str(), "Second Author"),
        ]
    );

    // Two distinct authorship layers, not one collapsed blame.
    let mut commits: Vec<&str> = blame
        .value
        .lines
        .iter()
        .map(|line| line.commit.as_str())
        .collect();
    commits.sort_unstable();
    commits.dedup();
    assert_eq!(commits.len(), 2);
}

/// A `HunkRef` is anchored on blob identity and hunk content, not on a
/// mutable ref. Committing unrelated work moves HEAD; re-resolving the same
/// unstaged hunk afterwards must mint a byte-identical reference.
#[test]
fn hunk_references_replay_identically_after_the_ref_moves() {
    let Some(fixture) = Fixture::init() else {
        return;
    };
    fixture.write(
        "src/target.txt",
        "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\n",
    );
    fixture.commit_all("initial");
    let before_head = fixture.head_oid();

    // One unstaged edit, far enough from the file edges to stay a single hunk.
    fixture.write(
        "src/target.txt",
        "line1\nline2\nline3\nEDITED\nline5\nline6\nline7\nline8\n",
    );

    let adapter = fixture.adapter();
    let engine = GitQueryEngine::new(&adapter);
    let bounds = GitQueryBounds::default();
    let digest = snapshot_digest('a');

    let first = engine
        .hunk_refs(
            &bounds,
            &GitDiffScopeV1::WorkingTree,
            "preview.replay",
            &digest,
        )
        .unwrap();
    assert_eq!(first.value.len(), 1);
    let reference = &first.value[0];
    reference.validate().unwrap();
    assert_eq!(reference.direction, HunkDirectionV1::WorkingTreeToIndex);
    assert_eq!(reference.path, "src/target.txt");
    let anchor = reference.compute_digest().unwrap();

    // Move the ref: commit unrelated work on top. HEAD changes; the anchored
    // hunk does not.
    fixture.write("src/unrelated.txt", "unrelated\n");
    fixture.git_ok(&["add", "src/unrelated.txt"]);
    fixture.git_ok(&["commit", "-m", "unrelated work"]);
    let after_head = fixture.head_oid();
    assert_ne!(
        before_head, after_head,
        "the fixture must actually move the ref"
    );

    let replayed = engine
        .hunk_refs(
            &bounds,
            &GitDiffScopeV1::WorkingTree,
            "preview.replay",
            &digest,
        )
        .unwrap();
    assert_eq!(replayed.value.len(), 1);
    let replayed_reference = &replayed.value[0];
    replayed_reference.validate().unwrap();

    assert_eq!(
        replayed_reference.compute_digest().unwrap(),
        anchor,
        "HunkRef identity must not move with the ref"
    );
    assert_eq!(reference, replayed_reference);
    replayed_reference.verify_digest(&anchor).unwrap();

    // A commit range carries no index relationship, so it cannot mint an
    // applicable reference: the refusal is typed, not an empty result.
    let refused = engine.hunk_refs(
        &bounds,
        &GitDiffScopeV1::CommitRange {
            base: oid(&before_head),
            head: oid(&after_head),
        },
        "preview.replay",
        &digest,
    );
    assert!(
        matches!(
            refused,
            Err(GitQueryError::Adapter(
                GitIntelligenceError::HunkRefNotMintable
            ))
        ),
        "range scope must refuse to mint, got {refused:?}"
    );
}

/// Dual provenance: Git-side revision evidence and a code-generation's claimed
/// revision are separate watermarks. When they disagree the join reports the
/// exact disagreement as typed staleness — it never returns `Current`, and it
/// never merges the two provenances into one clean answer.
#[test]
fn generation_join_reports_watermark_disagreement_instead_of_merging() {
    let Some(fixture) = Fixture::init() else {
        return;
    };
    fixture.write("src/tracked.txt", "one\n");
    let older = fixture.commit_all("older");
    fixture.write("src/tracked.txt", "one\ntwo\n");
    let current = fixture.commit_all("current");
    assert_ne!(older, current);

    let adapter = fixture.adapter();
    let engine = GitQueryEngine::new(&adapter);
    let bounds = GitQueryBounds::default();
    let generation = CodeGenerationId::new("generation.fixture").unwrap();

    let evidence = engine.revision_evidence(&bounds).unwrap();
    assert_eq!(
        evidence.head_oid.as_ref().map(GitOidV1::as_str),
        Some(current.as_str())
    );

    // Agreeing watermarks: the join is current, and both sides are reported.
    let agreeing = engine
        .join_generation(
            &bounds,
            &GenerationBoundGitQueryV1::new(
                generation.clone(),
                Some(oid(&current)),
                Some(evidence.worktree_digest.clone()),
            ),
        )
        .unwrap();
    assert!(agreeing.is_current());
    assert_eq!(agreeing.staleness, GenerationStalenessV1::Current);
    assert_eq!(agreeing.generation_id, generation);
    assert_eq!(agreeing.evidence.head_oid, evidence.head_oid);

    // Git moved past the generation's claimed HEAD: reported as behind, with
    // both the claimed and the observed watermark preserved.
    let behind = engine
        .join_generation(
            &bounds,
            &GenerationBoundGitQueryV1::new(
                generation.clone(),
                Some(oid(&older)),
                Some(evidence.worktree_digest.clone()),
            ),
        )
        .unwrap();
    assert!(!behind.is_current());
    assert_eq!(
        behind.staleness,
        GenerationStalenessV1::GenerationBehindHead {
            claimed: oid(&older),
            current: oid(&current),
        }
    );

    // Same HEAD, different worktree watermark: reported as diverged, again
    // carrying both digests rather than picking one.
    let foreign_digest = snapshot_digest('c');
    let diverged = engine
        .join_generation(
            &bounds,
            &GenerationBoundGitQueryV1::new(
                generation.clone(),
                Some(oid(&current)),
                Some(foreign_digest.clone()),
            ),
        )
        .unwrap();
    assert!(!diverged.is_current());
    assert_eq!(
        diverged.staleness,
        GenerationStalenessV1::WorktreeDiverged {
            claimed: foreign_digest,
            current: evidence.worktree_digest.clone(),
        }
    );

    // A claimed HEAD unreachable from the observed HEAD is never quietly
    // accepted as an ancestor.
    let foreign_head = oid(&"f".repeat(40));
    let rewritten = engine
        .join_generation(
            &bounds,
            &GenerationBoundGitQueryV1::new(
                generation.clone(),
                Some(foreign_head.clone()),
                Some(evidence.worktree_digest.clone()),
            ),
        )
        .unwrap();
    assert!(!rewritten.is_current());
    assert_eq!(
        rewritten.staleness,
        GenerationStalenessV1::HistoryRewritten {
            claimed_head: foreign_head,
        }
    );

    // An uncommitted edit moves the Git-side worktree watermark on its own, so
    // a generation captured against the clean tree is reported as diverged
    // even though HEAD never moved.
    fixture.write("src/tracked.txt", "one\ntwo\nthree\n");
    let dirty = engine
        .join_generation(
            &bounds,
            &GenerationBoundGitQueryV1::new(
                generation,
                Some(oid(&current)),
                Some(evidence.worktree_digest.clone()),
            ),
        )
        .unwrap();
    assert!(!dirty.is_current());
    assert!(
        matches!(
            dirty.staleness,
            GenerationStalenessV1::WorktreeDiverged { ref claimed, .. }
                if *claimed == evidence.worktree_digest
        ),
        "a dirty worktree must move the git-side watermark: {:?}",
        dirty.staleness
    );
    assert_ne!(dirty.evidence.worktree_digest, evidence.worktree_digest);
}
