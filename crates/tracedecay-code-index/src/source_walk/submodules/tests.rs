use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use ignore::Error as WalkError;
use tempfile::TempDir;
use tracedecay_domain::IndexPathPolicyV1;

use super::LiveSourceBoundaries;
use crate::source_walk::{SourceWalk, source_walk};

fn git(root: &Path, args: &[&str]) {
    let program = tracedecay_runtime_core::git::try_git_program().expect("fixture Git");
    let mut command = Command::new(program);
    command.current_dir(root);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join(".fixture-gitconfig"))
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("run fixture Git");
    assert!(
        output.status.success(),
        "Git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> TempDir {
    let fixture = TempDir::new().unwrap();
    let parent = fixture.path().join("parent");
    let child = fixture.path().join("child");
    fs::create_dir_all(&parent).unwrap();
    fs::create_dir_all(child.join("nested")).unwrap();
    git(&parent, &["init", "-q"]);
    git(&child, &["init", "-q"]);
    fs::write(parent.join("parent.rs"), "pub fn parent() {}\n").unwrap();
    fs::write(child.join("first.rs"), "pub fn first() {}\n").unwrap();
    fs::write(child.join("nested/second.rs"), "pub fn second() {}\n").unwrap();
    git(&child, &["add", "."]);
    git(&child, &["commit", "-qm", "child source"]);
    git(
        &parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            "../child",
            "app",
        ],
    );
    git(&parent, &["add", "."]);
    git(&parent, &["commit", "-qm", "parent with child"]);
    fixture
}

fn walk(root: &Path) -> SourceWalk {
    let policy = IndexPathPolicyV1::new(vec![], vec![])
        .unwrap()
        .with_submodules(true);
    source_walk(root, None, &policy).expect("authenticated source walk")
}

fn assert_live_control(root: &Path) {
    let files = walk(root)
        .map(|entry| entry.expect("unchanged checkout"))
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.path().strip_prefix(root).unwrap().to_path_buf())
        .collect::<BTreeSet<_>>();
    for source in ["parent.rs", "app/first.rs", "app/nested/second.rs"] {
        assert!(files.contains(Path::new(source)), "{source}: {files:?}");
    }
}

fn assert_binding_error(error: &WalkError) {
    assert_eq!(
        error.io_error().expect("typed binding refusal").kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        error.to_string().contains("checkout binding changed"),
        "{error}"
    );
}

fn assert_refused_walk(root: &Path, walk: SourceWalk) {
    let mut files = BTreeSet::new();
    let mut errors = 0;
    for entry in walk {
        match entry {
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => {
                files.insert(entry.path().strip_prefix(root).unwrap().to_path_buf());
            }
            Ok(_) => {}
            Err(error) => {
                assert_binding_error(&error);
                errors += 1;
            }
        }
    }
    assert_eq!(
        errors, 1,
        "binding loss must not be a successful empty walk"
    );
    assert!(files.contains(Path::new("parent.rs")));
    assert!(files.iter().all(|path| !path.starts_with("app")));
}

fn marker_change_before_iteration(replacement: Option<&str>) {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    assert_live_control(&root);
    let boundaries = LiveSourceBoundaries::open(&root, true).unwrap();
    let path = Path::new("app/first.rs");
    assert!(boundaries.resolve(path).unwrap().is_some());
    let pending = walk(&root);
    let marker = root.join("app/.git");
    match replacement {
        Some(contents) => fs::write(&marker, contents).unwrap(),
        None => fs::remove_file(&marker).unwrap(),
    }
    assert!(
        root.join(path).is_file(),
        "the child bytes are still present"
    );
    assert!(boundaries.resolve(path).unwrap().is_none());
    assert_refused_walk(&root, pending);
}

fn binding_change_before_iteration(change: impl FnOnce(&Path)) {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    assert_live_control(&root);
    let boundaries = LiveSourceBoundaries::open(&root, true).unwrap();
    let path = Path::new("app/first.rs");
    assert!(boundaries.resolve(path).unwrap().is_some());
    let pending = walk(&root);
    change(&root);
    assert!(
        root.join(path).is_file(),
        "the same child bytes still exist"
    );
    assert!(root.join("app/.git").is_file(), "child marker is unchanged");
    assert!(boundaries.resolve(path).unwrap().is_none());
    assert_refused_walk(&root, pending);
}

#[test]
fn lazy_submodule_walk_refuses_removed_worktree_registration() {
    binding_change_before_iteration(|root| {
        fs::remove_file(root.join(".gitmodules")).unwrap();
    });
}

#[test]
fn lazy_submodule_walk_refuses_removed_staged_gitlink() {
    binding_change_before_iteration(|root| {
        git(root, &["rm", "-q", "--cached", "app"]);
    });
}

#[test]
fn lazy_submodule_walk_refuses_removed_staged_registration() {
    binding_change_before_iteration(|root| {
        git(root, &["rm", "-q", "--cached", ".gitmodules"]);
    });
}

#[test]
fn lazy_submodule_walk_refuses_updated_staged_child_pin() {
    binding_change_before_iteration(|root| {
        let child = root.join("app");
        fs::write(child.join("first.rs"), "pub fn updated() {}\n").unwrap();
        git(&child, &["add", "first.rs"]);
        git(&child, &["commit", "-qm", "updated child"]);
        git(root, &["add", "app"]);
    });
}

#[test]
fn lazy_submodule_walk_refuses_changed_child_repository_worktree() {
    binding_change_before_iteration(|root| {
        // The marker stays at app/.git and the source bytes remain in app,
        // but the parent-owned child Git admin now advertises another worktree.
        let other = root.parent().unwrap().join("child");
        git(
            &root.join("app"),
            &["config", "core.worktree", other.to_str().unwrap()],
        );
        let parent = tracedecay_runtime_core::git_open::open(root).unwrap();
        assert!(
            !super::live_submodules(&parent).unwrap().contains_key("app"),
            "fresh admission must reject a retargeted child repository worktree"
        );
    });
}

#[test]
fn lazy_submodule_walk_refuses_removed_marker() {
    marker_change_before_iteration(None);
}

#[test]
fn lazy_submodule_walk_refuses_retargeted_marker() {
    // The source repository has identical tracked files but is not the
    // parent's previously authenticated module store.
    marker_change_before_iteration(Some("gitdir: ../../child/.git\n"));
}

#[test]
fn lazy_submodule_walk_stops_after_mid_iteration_binding_loss() {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    assert_live_control(&root);
    let mut pending = walk(&root);
    let first = pending
        .by_ref()
        .map(|entry| entry.expect("unchanged checkout"))
        .find(|entry| {
            entry.file_type().is_some_and(|kind| kind.is_file())
                && entry.path().starts_with(root.join("app"))
        })
        .expect("at least one child source was yielded");
    assert!(first.path().is_file());
    let marker = root.join("app/.git");
    let original = fs::read(&marker).unwrap();
    fs::remove_file(&marker).unwrap();
    let error = pending
        .next()
        .expect("binding refusal after a yielded child file")
        .expect_err("must revalidate before advancing the child walk");
    assert_binding_error(&error);
    fs::write(&marker, original).unwrap();
    assert!(pending.next().is_none(), "a failed walk cannot resume");
}

#[cfg(unix)]
#[test]
fn lazy_submodule_walk_refuses_replaced_root_before_root_entry() {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    assert_live_control(&root);
    let pending = walk(&root);
    let app = root.join("app");
    fs::rename(&app, fixture.path().join("detached-child")).unwrap();
    std::os::unix::fs::symlink(fixture.path().join("child"), &app).unwrap();
    assert!(app.join("first.rs").is_file());
    // ignore does not filter its depth-zero entry. The guard must run
    // before polling this separate child root, not only in filter_entry.
    assert_refused_walk(&root, pending);
}
