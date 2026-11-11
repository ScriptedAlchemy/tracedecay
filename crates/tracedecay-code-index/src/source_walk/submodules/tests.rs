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

#[test]
fn indexed_child_inventory_refuses_revoked_gitlink_before_capture() {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    let repository = tracedecay_runtime_core::git_open::open(&root).unwrap();
    let classified =
        crate::source_walk::classification::WorktreeChangeClassificationV1::for_policy(
            &repository,
            true,
        )
        .unwrap();
    let (_, child) = classified
        .bound_child("app/first.rs")
        .expect("registered child source");
    assert!(child.checkout_is_live().unwrap());
    assert!(classified.child_bindings_live().unwrap());

    git(&root, &["rm", "-q", "--cached", "app"]);
    assert!(root.join("app/.git").is_file());
    assert!(root.join("app/first.rs").is_file());
    assert!(classified.bound_child("app/first.rs").is_some());
    assert!(!child.checkout_is_live().unwrap());
    assert!(
        !classified.child_bindings_live().unwrap(),
        "retained capture authority must expire when the staged gitlink disappears"
    );

    git(&root, &["add", "app"]);
    assert!(child.checkout_is_live().unwrap());
    assert!(classified.child_bindings_live().unwrap());
}

#[test]
fn unavailable_parent_index_refuses_retained_graph_and_lazy_walk() {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    assert_live_control(&root);
    let boundaries = LiveSourceBoundaries::open(&root, true).unwrap();
    let path = Path::new("app/first.rs");
    assert!(boundaries.resolve(path).unwrap().is_some());
    let pending = walk(&root);

    // The marker and child bytes are intact, but parent authority is corrupt.
    // Returning Ok(None) here would make unavailable data look absent.
    fs::write(root.join(".git/index"), b"not a valid Git index").unwrap();
    assert!(root.join(path).is_file());
    let error = boundaries
        .resolve(path)
        .expect_err("unreadable parent index must not hide an admitted child");
    assert_eq!(error.kind(), io::ErrorKind::Other);

    let error = pending
        .filter_map(Result::err)
        .next()
        .expect("the child walk must report unavailable parent authority");
    assert_eq!(error.io_error().unwrap().kind(), io::ErrorKind::Other);
}

#[cfg(unix)]
#[test]
fn repeated_child_reads_do_not_reparse_stable_parent_git_authority() {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    let boundaries = LiveSourceBoundaries::open(&root, true).unwrap();
    let source = Path::new("app/first.rs");
    super::FULL_BINDING_VALIDATIONS.with(|count| count.set(0));

    for _ in 0..128 {
        assert!(boundaries.resolve(source).unwrap().is_some());
    }
    super::FULL_BINDING_VALIDATIONS.with(|count| {
        assert_eq!(
            count.get(),
            1,
            "128 stable source reads must share one full Git binding validation"
        );
    });
    git(&root, &["rm", "-q", "--cached", "app"]);
    assert!(
        boundaries.resolve(source).unwrap().is_none(),
        "a staged gitlink removal must revoke an otherwise unchanged child"
    );
    super::FULL_BINDING_VALIDATIONS.with(|count| {
        assert_eq!(count.get(), 2, "revocation requires a fresh Git validation");
    });
    git(&root, &["add", "app"]);
    assert!(boundaries.resolve(source).unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn cached_child_binding_refuses_equal_length_working_registration_change() {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    let boundaries = LiveSourceBoundaries::open(&root, true).unwrap();
    let source = Path::new("app/first.rs");
    assert!(boundaries.resolve(source).unwrap().is_some());

    let path = root.join(".gitmodules");
    let before = fs::read_to_string(&path).unwrap();
    let changed = before.replacen("path = app", "path = bad", 1);
    assert_ne!(changed, before);
    assert_eq!(changed.len(), before.len());
    fs::write(&path, changed).unwrap();
    assert!(
        boundaries.resolve(source).unwrap().is_none(),
        "a same-length working registration change must not reuse authority"
    );
    fs::write(&path, before).unwrap();
    assert!(boundaries.resolve(source).unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn parent_worktree_config_change_revokes_cached_child_authority() {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    git(&root, &["config", "extensions.worktreeConfig", "true"]);
    let config = root.join(".git/config.worktree");
    let original = format!("[core]\n\tworktree = {}\n", root.display());
    fs::write(&config, &original).unwrap();

    let boundaries = LiveSourceBoundaries::open(&root, true).unwrap();
    let source = Path::new("app/first.rs");
    assert!(boundaries.resolve(source).unwrap().is_some());
    // This change is entirely inside an existing file: neither the parent
    // index nor Git admin directory changes, even though Git's effective
    // worktree authority is redirected to a different checkout.
    let git_directory = root.join(".git");
    let directory_before = super::FileWitness::at(&git_directory).unwrap();
    let index_before = super::FileWitness::at(&git_directory.join("index")).unwrap();
    let foreign = fixture.path().join("foreign");
    fs::create_dir_all(&foreign).unwrap();
    let alternate = format!("[core]\n\tworktree = {}\n", foreign.display());
    fs::write(&config, alternate).unwrap();
    assert_eq!(
        super::FileWitness::at(&git_directory).unwrap(),
        directory_before
    );
    assert_eq!(
        super::FileWitness::at(&git_directory.join("index")).unwrap(),
        index_before
    );
    assert!(root.join(source).is_file(), "child content still exists");
    assert!(
        boundaries.resolve(source).unwrap().is_none(),
        "cached child authority must expire on a worktree-local parent config change"
    );

    fs::write(&config, original).unwrap();
    assert!(boundaries.resolve(source).unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn changing_a_split_index_invalidates_the_child_binding_witness() {
    let fixture = fixture();
    let root = fixture.path().join("parent");
    git(&root, &["update-index", "--split-index"]);
    let boundaries = LiveSourceBoundaries::open(&root, true).unwrap();
    let source = Path::new("app/first.rs");
    assert!(boundaries.resolve(source).unwrap().is_some());
    let pending = walk(&root);
    let (child, _) = boundaries.children.get("app").expect("authenticated child");

    let shared = fs::read_dir(root.join(".git"))
        .unwrap()
        .map(|entry| entry.unwrap())
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("sharedindex.")
        })
        .expect("real Git split index")
        .path();
    let witness = child
        .binding_witness()
        .unwrap()
        .expect("local Git authority");
    assert!(
        witness.paths.contains_key(&shared),
        "the secondary index is part of staged gitlink authority"
    );
    assert!(witness.unchanged().unwrap());

    // Mutate only the secondary index's bytes, not the main index or its
    // parent directory. Watching only .git/index would reuse stale authority.
    let index = root.join(".git/index");
    let original_index = super::FileWitness::at(&index).unwrap();
    let original_directory = super::FileWitness::at(&root.join(".git")).unwrap();
    fs::write(&shared, b"corrupt shared index").unwrap();
    assert_eq!(super::FileWitness::at(&index).unwrap(), original_index);
    assert_eq!(
        super::FileWitness::at(&root.join(".git")).unwrap(),
        original_directory
    );
    assert!(
        !witness.unchanged().unwrap(),
        "a corrupt shared-index inode must invalidate the cached Git authority"
    );
    assert!(
        boundaries.resolve(source).is_err(),
        "a retained graph reader must refuse unreadable split-index authority"
    );
    let error = pending
        .filter_map(Result::err)
        .next()
        .expect("a retained child walk must report unreadable split-index authority");
    assert_eq!(error.io_error().unwrap().kind(), io::ErrorKind::Other);
}
