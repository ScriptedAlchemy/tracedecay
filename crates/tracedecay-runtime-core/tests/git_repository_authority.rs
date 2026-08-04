#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;
use tracedecay_domain::git::{
    GitChangeKindV1, GitDegradationV1, GitHeadStateV1, GitObjectFormatV1, GitStatusEntryV1,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git_repository::{
    GitHistoryBudget, GitHistoryOptions, GitHistoryTermination, GitRepositoryAuthority,
    GitRepositoryError,
};

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn init(object_format: &str) -> Self {
        let fixture = Self {
            directory: tempfile::tempdir().expect("temporary repository"),
        };
        fixture.git(&[
            "init",
            "--quiet",
            "-b",
            "main",
            &format!("--object-format={object_format}"),
        ]);
        fixture
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.path().join(path);
        std::fs::create_dir_all(path.parent().expect("file parent")).expect("create parent");
        std::fs::write(path, content).expect("write fixture file");
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
            ])
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("git executable");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("UTF-8 git output")
    }

    fn commit(&self, subject: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", subject]);
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn import_linear_history(&self, commits: usize) {
        let mut child = Command::new("git")
            .arg("fast-import")
            .arg("--quiet")
            .current_dir(self.path())
            .stdin(Stdio::piped())
            .spawn()
            .expect("git fast-import");
        let input = child.stdin.as_mut().expect("fast-import stdin");
        for index in 0..commits {
            let message = format!("commit {index}");
            writeln!(input, "commit refs/heads/main").expect("commit command");
            writeln!(input, "mark :{}", index + 1).expect("commit mark");
            writeln!(
                input,
                "author Fixture <fixture@example.com> {} +0000",
                1_000_000_000 + index
            )
            .expect("author");
            writeln!(
                input,
                "committer Fixture <fixture@example.com> {} +0000",
                1_000_000_000 + index
            )
            .expect("committer");
            writeln!(input, "data {}", message.len()).expect("message size");
            writeln!(input, "{message}").expect("message");
            if index > 0 {
                writeln!(input, "from :{index}").expect("parent");
            }
            let content = format!("{index}\n");
            writeln!(input, "M 100644 inline tracked.txt").expect("file command");
            writeln!(input, "data {}", content.len()).expect("content size");
            write!(input, "{content}").expect("content");
        }
        writeln!(input, "done").expect("done");
        drop(child.stdin.take());
        assert!(child.wait().expect("fast-import exit").success());
    }
}

fn snapshot_git_dir(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![root.join(".git")];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().display().to_string(),
                    std::fs::read(path).unwrap(),
                ));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn authority_reports_sha256_identity_head_and_refs() {
    let fixture = Fixture::init("sha256");
    fixture.write("README.md", "sha256\n");
    let head = fixture.commit("initial");
    fixture.write("dirty.txt", "live without sync\n");
    let before = snapshot_git_dir(fixture.path());

    let authority = GitRepositoryAuthority::discover(fixture.path()).expect("open authority");
    assert_eq!(
        authority.object_format().unwrap(),
        GitObjectFormatV1::Sha256
    );
    assert_eq!(
        authority.worktree_root(),
        Some(fixture.path().canonicalize().unwrap().as_path())
    );
    assert_eq!(
        authority.common_dir(),
        fixture.path().join(".git").canonicalize().unwrap()
    );
    assert!(matches!(
        authority.head().unwrap(),
        GitHeadStateV1::Attached { branch, commit }
            if branch == "main" && commit.as_str() == head
    ));
    assert!(authority.references().unwrap().iter().any(|reference| {
        reference.name == "refs/heads/main"
            && reference
                .target
                .as_ref()
                .is_some_and(|target| target.as_str() == head)
    }));
    assert_eq!(authority.status().unwrap().entries.len(), 1);
    assert_eq!(
        authority
            .history(&GitHistoryOptions {
                max_count: 10,
                first_parent: false,
                path: None,
                follow_renames: false,
            })
            .unwrap()
            .commits
            .len(),
        1
    );
    assert_eq!(snapshot_git_dir(fixture.path()), before);
}

#[test]
fn authority_observes_dirty_files_without_sync() {
    let fixture = Fixture::init("sha1");
    fixture.write("tracked.txt", "before\n");
    fixture.commit("initial");

    fixture.write("tracked.txt", "after\n");
    fixture.write("staged.txt", "staged\n");
    fixture.git(&["add", "staged.txt"]);
    fixture.write("untracked.txt", "untracked\n");

    let status = GitRepositoryAuthority::discover(fixture.path())
        .unwrap()
        .status()
        .unwrap();
    assert!(status.entries.iter().any(|entry| matches!(
        entry,
        GitStatusEntryV1::Tracked(tracked)
            if tracked.path == "tracked.txt"
                && tracked.worktree == GitChangeKindV1::Modified
    )));
    assert!(status.entries.iter().any(|entry| matches!(
        entry,
        GitStatusEntryV1::Tracked(tracked)
            if tracked.path == "staged.txt"
                && tracked.index == GitChangeKindV1::Added
    )));
    assert!(status.entries.iter().any(|entry| matches!(
        entry,
        GitStatusEntryV1::Untracked { path } if path == "untracked.txt"
    )));
    assert_eq!(
        GitRepositoryAuthority::discover(fixture.path())
            .unwrap()
            .history(&GitHistoryOptions {
                max_count: 10,
                first_parent: false,
                path: None,
                follow_renames: false,
            })
            .unwrap()
            .commits
            .len(),
        1
    );
}

#[test]
fn authority_keeps_linked_worktree_common_identity_and_exact_head() {
    let fixture = Fixture::init("sha1");
    fixture.write("README.md", "main\n");
    fixture.commit("initial");
    fixture.git(&["branch", "feature"]);
    let linked = fixture.path().join(".worktrees").join("feature");
    std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
    fixture.git(&[
        "worktree",
        "add",
        "--quiet",
        linked.to_str().expect("UTF-8 worktree"),
        "feature",
    ]);

    let authority = GitRepositoryAuthority::discover(&linked).unwrap();
    assert_eq!(
        authority.common_dir(),
        fixture.path().join(".git").canonicalize().unwrap()
    );
    assert_eq!(
        authority.worktree_root(),
        Some(linked.canonicalize().unwrap().as_path())
    );
    assert!(matches!(
        authority.head().unwrap(),
        GitHeadStateV1::Attached { branch, .. } if branch == "feature"
    ));
}

#[test]
fn authority_rev_walk_is_bounded_and_reports_truncation() {
    let fixture = Fixture::init("sha1");
    for index in 0..3 {
        fixture.write("counter.txt", &format!("{index}\n"));
        fixture.commit(&format!("commit {index}"));
    }

    let history = GitRepositoryAuthority::discover(fixture.path())
        .unwrap()
        .history(&GitHistoryOptions {
            max_count: 2,
            first_parent: false,
            path: None,
            follow_renames: false,
        })
        .unwrap();
    assert_eq!(history.commits.len(), 2);
    assert!(history.truncated);
    assert_eq!(history.commits[0].subject, "commit 2");
    assert_eq!(history.commits[1].subject, "commit 1");
}

#[test]
fn authority_does_not_report_truncation_at_exact_scan_boundary() {
    let fixture = Fixture::init("sha1");
    fixture.import_linear_history(1_024);

    let history = GitRepositoryAuthority::discover(fixture.path())
        .unwrap()
        .history(&GitHistoryOptions {
            max_count: 1,
            first_parent: false,
            path: Some("missing.txt".to_owned()),
            follow_renames: false,
        })
        .unwrap();
    assert!(history.commits.is_empty());
    assert!(!history.truncated);
}

fn _assert_send_sync<T: Send + Sync>() {}

#[test]
fn authority_is_send_sync() {
    _assert_send_sync::<GitRepositoryAuthority>();
}

#[test]
fn authority_distinguishes_detached_unborn_and_unreadable_head() {
    let unborn = Fixture::init("sha1");
    let authority = GitRepositoryAuthority::discover(unborn.path()).unwrap();
    assert!(matches!(
        authority.head().unwrap(),
        GitHeadStateV1::Unborn { branch } if branch == "main"
    ));

    let detached = Fixture::init("sha1");
    detached.write("README.md", "detached\n");
    let commit = detached.commit("initial");
    detached.git(&["checkout", "--quiet", "--detach", "HEAD"]);
    let authority = GitRepositoryAuthority::discover(detached.path()).unwrap();
    assert!(matches!(
        authority.head().unwrap(),
        GitHeadStateV1::Detached { commit: actual } if actual.as_str() == commit
    ));

    let corrupt = Fixture::init("sha1");
    std::fs::write(corrupt.path().join(".git/HEAD"), b"not a head\n").unwrap();
    let authority = GitRepositoryAuthority::discover(corrupt.path()).unwrap();
    assert!(matches!(
        authority.head(),
        Err(GitRepositoryError::UnreadableHead { .. })
    ));
}

#[test]
fn authority_distinguishes_absent_and_unreadable_repository_paths() {
    let empty = tempfile::tempdir().unwrap();
    assert!(matches!(
        GitRepositoryAuthority::discover(empty.path()),
        Err(GitRepositoryError::NotARepository { .. })
    ));
    let missing = empty.path().join("missing");
    assert!(matches!(
        GitRepositoryAuthority::discover(&missing),
        Err(GitRepositoryError::UnreadableRepository { .. })
    ));
}

#[test]
fn authority_ignores_ambient_git_environment_for_linked_head() {
    const CHILD_ROOT: &str = "TRACEDECAY_GIX_AUTHORITY_CHILD_ROOT";
    const CHILD_BRANCH: &str = "TRACEDECAY_GIX_AUTHORITY_CHILD_BRANCH";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let authority = GitRepositoryAuthority::discover(Path::new(&root)).unwrap();
        let expected = std::env::var(CHILD_BRANCH).unwrap();
        assert!(matches!(
            authority.head().unwrap(),
            GitHeadStateV1::Attached { branch, .. } if branch == expected
        ));
        return;
    }

    let fixture = Fixture::init("sha1");
    fixture.write("README.md", "main\n");
    fixture.commit("initial");
    fixture.git(&["branch", "feature"]);
    let linked = fixture.path().join(".worktrees/feature");
    std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
    fixture.git(&[
        "worktree",
        "add",
        "--quiet",
        linked.to_str().unwrap(),
        "feature",
    ]);

    let poison = Fixture::init("sha1");
    poison.write("README.md", "poison\n");
    poison.commit("poison");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "authority_ignores_ambient_git_environment_for_linked_head",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &linked)
        .env(CHILD_BRANCH, "feature")
        .env("GIT_DIR", poison.path().join(".git"))
        .env("GIT_WORK_TREE", poison.path())
        .env(
            "GIT_OBJECT_DIRECTORY",
            poison.path().join(".git").join("objects"),
        )
        .env("GIT_CEILING_DIRECTORIES", poison.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn authority_uses_gix_rename_resolution_for_ambiguous_equal_blobs() {
    let fixture = Fixture::init("sha1");
    fixture.write("a.txt", "same\n");
    fixture.write("b.txt", "same\n");
    fixture.commit("initial");
    std::fs::remove_file(fixture.path().join("a.txt")).unwrap();
    std::fs::remove_file(fixture.path().join("b.txt")).unwrap();
    fixture.write("c.txt", "same\n");
    fixture.commit("rename");

    let history = GitRepositoryAuthority::discover(fixture.path())
        .unwrap()
        .history(&GitHistoryOptions {
            max_count: 10,
            first_parent: false,
            path: Some("c.txt".to_owned()),
            follow_renames: true,
        })
        .unwrap();
    assert_eq!(
        history
            .commits
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect::<Vec<_>>(),
        ["rename", "initial"]
    );
    assert_eq!(history.termination, GitHistoryTermination::Complete);
}

#[test]
fn authority_history_honors_cancellation_and_each_budget() {
    let fixture = Fixture::init("sha1");
    for index in 0..3 {
        fixture.write("tracked.txt", &format!("{index}\n"));
        fixture.commit(&format!("commit {index}"));
    }
    let authority = GitRepositoryAuthority::discover(fixture.path()).unwrap();
    let options = GitHistoryOptions {
        max_count: 10,
        first_parent: false,
        path: None,
        follow_renames: false,
    };
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let history = authority
        .history_with_control(
            &options,
            GitHistoryBudget {
                commits: 10,
                trees: 10,
                objects: 10,
            },
            &cancelled,
        )
        .unwrap();
    assert_eq!(history.termination, GitHistoryTermination::Cancelled);
    assert!(history.commits.is_empty());

    let live = CancellationToken::new();
    let history = authority
        .history_with_control(
            &options,
            GitHistoryBudget {
                commits: 1,
                trees: 10,
                objects: 10,
            },
            &live,
        )
        .unwrap();
    assert_eq!(history.termination, GitHistoryTermination::CommitBudget);
    assert_eq!(history.commits.len(), 1);

    let path_options = GitHistoryOptions {
        path: Some("tracked.txt".to_owned()),
        ..options
    };
    let history = authority
        .history_with_control(
            &path_options,
            GitHistoryBudget {
                commits: 10,
                trees: 0,
                objects: 10,
            },
            &live,
        )
        .unwrap();
    assert_eq!(history.termination, GitHistoryTermination::TreeBudget);
    let history = authority
        .history_with_control(
            &path_options,
            GitHistoryBudget {
                commits: 10,
                trees: 10,
                objects: 1,
            },
            &live,
        )
        .unwrap();
    assert_eq!(history.termination, GitHistoryTermination::ObjectBudget);
}

#[test]
fn authority_bounds_nested_tree_inventory_before_rename_diff() {
    let fixture = Fixture::init("sha1");
    let nested = format!(
        "{}/tracked.txt",
        (0..32)
            .map(|index| format!("level-{index}"))
            .collect::<Vec<_>>()
            .join("/")
    );
    fixture.write(&nested, "one\n");
    fixture.commit("initial");
    fixture.write(&nested, "two\n");
    fixture.commit("modified");

    let history = GitRepositoryAuthority::discover(fixture.path())
        .unwrap()
        .history_with_control(
            &GitHistoryOptions {
                max_count: 10,
                first_parent: false,
                path: Some(nested),
                follow_renames: true,
            },
            GitHistoryBudget {
                commits: 10,
                trees: 8,
                objects: 100,
            },
            &CancellationToken::new(),
        )
        .unwrap();
    assert!(history.commits.is_empty());
    assert!(history.truncated);
    assert_eq!(history.termination, GitHistoryTermination::TreeBudget);
}

#[test]
fn authority_preserves_partial_history_when_an_object_is_missing() {
    let fixture = Fixture::init("sha1");
    fixture.write("tracked.txt", "one\n");
    let root = fixture.commit("root");
    fixture.write("tracked.txt", "two\n");
    fixture.commit("tip");
    let object = fixture
        .path()
        .join(".git/objects")
        .join(&root[..2])
        .join(&root[2..]);
    std::fs::remove_file(object).unwrap();

    let history = GitRepositoryAuthority::discover(fixture.path())
        .unwrap()
        .history(&GitHistoryOptions {
            max_count: 10,
            first_parent: false,
            path: None,
            follow_renames: false,
        })
        .unwrap();
    assert_eq!(history.commits.len(), 1);
    assert!(history.truncated);
    assert!(matches!(
        history.termination,
        GitHistoryTermination::UnreadableObject { .. }
    ));
    assert!(
        history
            .degradations
            .contains(&GitDegradationV1::UnreadableState)
    );
}

#[test]
fn authority_reports_shallow_history_as_truncated_evidence() {
    let source = Fixture::init("sha1");
    for index in 0..3 {
        source.write("tracked.txt", &format!("{index}\n"));
        source.commit(&format!("commit {index}"));
    }
    let shallow = tempfile::tempdir().unwrap();
    let output = Command::new("git")
        .args([
            "clone",
            "--quiet",
            "--depth",
            "1",
            &format!("file://{}", source.path().display()),
            shallow.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let history = GitRepositoryAuthority::discover(shallow.path())
        .unwrap()
        .history(&GitHistoryOptions {
            max_count: 10,
            first_parent: false,
            path: None,
            follow_renames: false,
        })
        .unwrap();
    assert_eq!(history.commits.len(), 1);
    assert!(history.truncated);
    assert_eq!(history.termination, GitHistoryTermination::ShallowBoundary);
}
