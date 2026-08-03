#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;
use tracedecay_domain::git::{
    GitChangeKindV1, GitHeadStateV1, GitObjectFormatV1, GitStatusEntryV1,
};
use tracedecay_runtime_core::git_repository::{GitHistoryOptions, GitRepositoryAuthority};

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
    assert_eq!(authority.native_git_invocations(), 0);
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
    assert_eq!(authority.native_git_invocations(), 1);
}

#[test]
fn authority_rev_walk_is_bounded_and_reports_truncation() {
    let fixture = Fixture::init("sha1");
    for index in 0..4 {
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
    assert_eq!(history.commits[0].subject, "commit 3");
    assert_eq!(history.commits[1].subject, "commit 2");
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
