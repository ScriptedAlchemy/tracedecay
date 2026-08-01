//! The source-edit candidate reader is the boundary that decides which bytes
//! the preview/apply plan is allowed to observe. Canonicalizing the parent
//! directory alone leaves the final path component free to be a symlink, so
//! these tests pin the descriptor-scoped behavior at the public surface.

use std::fs;
use std::path::Path;

use tempfile::tempdir;
use tracedecay_usecases::tracedecay::{
    read_source_edit_candidate, validate_source_edit_candidate_parent,
};

#[test]
fn reads_a_regular_candidate_beneath_the_worktree() {
    let project = tempdir().expect("project root");
    fs::create_dir(project.path().join("src")).expect("create source directory");
    fs::write(project.path().join("src/lib.rs"), b"inside").expect("seed candidate");

    assert_eq!(
        read_source_edit_candidate(project.path(), Path::new("src/lib.rs"))
            .expect("read candidate"),
        Some(b"inside".to_vec())
    );
}

/// Absence is not a refusal: a candidate that does not exist yet is a normal
/// state for a plan that creates files.
#[test]
fn reports_an_absent_candidate_without_error() {
    let project = tempdir().expect("project root");
    fs::create_dir(project.path().join("src")).expect("create source directory");

    assert_eq!(
        read_source_edit_candidate(project.path(), Path::new("src/lib.rs"))
            .expect("read candidate"),
        None
    );
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_final_component() {
    use std::os::unix::fs::symlink;

    let project = tempdir().expect("project root");
    let outside = tempdir().expect("outside root");
    let secret = outside.path().join("secret.rs");
    fs::write(&secret, b"outside").expect("seed outside file");
    fs::create_dir(project.path().join("src")).expect("create source directory");
    symlink(&secret, project.path().join("src/lib.rs")).expect("plant candidate symlink");

    assert!(read_source_edit_candidate(project.path(), Path::new("src/lib.rs")).is_err());
    assert_eq!(fs::read(&secret).expect("outside file intact"), b"outside");
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_parent_component() {
    use std::os::unix::fs::symlink;

    let project = tempdir().expect("project root");
    let outside = tempdir().expect("outside root");
    fs::write(outside.path().join("lib.rs"), b"outside").expect("seed outside file");
    symlink(outside.path(), project.path().join("src")).expect("plant parent symlink");

    assert!(read_source_edit_candidate(project.path(), Path::new("src/lib.rs")).is_err());
    assert!(
        validate_source_edit_candidate_parent(project.path(), Path::new("src/lib.rs")).is_err()
    );
    assert_eq!(
        fs::read(outside.path().join("lib.rs")).expect("outside file intact"),
        b"outside"
    );
}

/// A directory or other non-regular file must never be read as candidate
/// content, even when it sits at a legitimate path inside the worktree.
#[test]
fn refuses_a_non_regular_candidate() {
    let project = tempdir().expect("project root");
    fs::create_dir_all(project.path().join("src/lib.rs")).expect("create directory candidate");

    assert!(read_source_edit_candidate(project.path(), Path::new("src/lib.rs")).is_err());
}

#[test]
fn refuses_paths_that_escape_the_worktree() {
    let project = tempdir().expect("project root");

    assert!(read_source_edit_candidate(project.path(), Path::new("../escape.rs")).is_err());
    assert!(read_source_edit_candidate(project.path(), Path::new("")).is_err());
    assert!(
        validate_source_edit_candidate_parent(project.path(), Path::new("../escape.rs")).is_err()
    );
}
