#[cfg(unix)]
use super::document_paths::{open_project_file, validated_document_path};
use std::io::Read;
use std::path::{Component, Path};

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use tempfile::TempDir;
use url::Url;

// The symlink-escape test that exercises open_project_file is unix-only.
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use tracedecay_runtime_core::path_safety::canonical_root_identity;

fn admitted_root() -> (TempDir, std::path::PathBuf, Url, Dir) {
    let temp = TempDir::new().expect("temporary directory");
    let root = temp.path().join("root");
    std::fs::create_dir(&root).expect("create admitted root");
    let root = root.canonicalize().expect("canonical admitted root");
    let root_url = Url::from_directory_path(&root).expect("root file URI");
    let root_dir = Dir::open_ambient_dir(&root, ambient_authority()).expect("open admitted root");
    (temp, root, root_url, root_dir)
}

#[test]
fn document_paths_reject_parent_and_encoded_traversal() {
    let (_temp, root, root_url, root_dir) = admitted_root();
    for suffix in [
        "../outside.rs",
        "%2e%2e/outside.rs",
        "%2E%2E/outside.rs",
        "src/./lib.rs",
        "src/%2e/lib.rs",
    ] {
        let uri = format!("{}{suffix}", root_url.as_str());
        assert!(
            validated_document_path(
                &root,
                &canonical_root_identity(&root),
                &root_url,
                &root_dir,
                &uri
            )
            .is_err(),
            "accepted noncanonical URI path {uri}"
        );
    }
}

#[test]
fn document_paths_reject_encoded_separators_and_nul() {
    let (_temp, root, root_url, root_dir) = admitted_root();
    for suffix in [
        "src%2flib.rs",
        "src%2Flib.rs",
        "src%5clib.rs",
        "src%00lib.rs",
    ] {
        let uri = format!("{}{suffix}", root_url.as_str());
        assert!(
            validated_document_path(
                &root,
                &canonical_root_identity(&root),
                &root_url,
                &root_dir,
                &uri
            )
            .is_err(),
            "accepted encoded separator or NUL in {uri}"
        );
    }
}

#[test]
fn document_paths_reject_sibling_prefixes_before_join() {
    let (temp, root, root_url, root_dir) = admitted_root();
    let sibling = temp.path().join("root-sibling").join("src").join("lib.rs");
    let sibling_uri = Url::from_file_path(sibling).expect("sibling file URI");
    assert!(
        validated_document_path(
            &root,
            &canonical_root_identity(&root),
            &root_url,
            &root_dir,
            sibling_uri.as_str()
        )
        .is_err()
    );
}

#[test]
fn unsaved_overlay_keeps_a_normal_relative_path_without_existing() {
    let (_temp, root, root_url, root_dir) = admitted_root();
    let uri = root_url.join("new/nested/overlay.rs").expect("overlay URI");
    let document = validated_document_path(
        &root,
        &canonical_root_identity(&root),
        &root_url,
        &root_dir,
        uri.as_str(),
    )
    .expect("overlay");

    assert_eq!(document.absolute, root.join("new/nested/overlay.rs"));
    assert_eq!(document.relative, Path::new("new/nested/overlay.rs"));
    assert!(
        document
            .relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    );
}

/// The mirror of `root_alias_documents_...`: there the *client* spelled
/// the root through an alias, here the *daemon* did. `admitted_root`
/// canonicalizes its root, so on Linux nothing else in this module
/// exercises the spelling macOS (`/var` vs `/private/var`) and Windows
/// (native vs `\\?\` verbatim) actually register. An unsaved overlay is
/// the strict case: its suffix cannot be canonicalized at all, so it has
/// to be validated lexically beneath the root's identity.
#[cfg(unix)]
#[test]
fn unsaved_overlay_resolves_under_an_alias_spelled_admitted_root() {
    let temp = TempDir::new().expect("temporary directory");
    let real = temp.path().join("private").join("root");
    std::fs::create_dir_all(&real).expect("create real root");
    symlink(temp.path().join("private"), temp.path().join("var")).expect("host alias");
    let admitted = temp.path().join("var").join("root");
    let root_url = Url::from_directory_path(&admitted).expect("alias root URI");
    let root_dir = Dir::open_ambient_dir(&admitted, ambient_authority())
        .expect("open admitted root through its alias");
    let uri = Url::from_directory_path(real.canonicalize().expect("canonical root"))
        .expect("canonical root URI")
        .join("new/nested/overlay.rs")
        .expect("overlay URI");

    let document = validated_document_path(
        &admitted,
        &canonical_root_identity(&admitted),
        &root_url,
        &root_dir,
        uri.as_str(),
    )
    .expect("an overlay beneath an alias-spelled admitted root");
    assert_eq!(document.relative, Path::new("new/nested/overlay.rs"));

    let sibling = temp.path().join("private").join("root-other");
    std::fs::create_dir(&sibling).expect("create sibling root");
    let outside = Url::from_file_path(sibling.join("lib.rs")).expect("sibling document URI");
    assert!(
        validated_document_path(
            &admitted,
            &canonical_root_identity(&admitted),
            &root_url,
            &root_dir,
            outside.as_str()
        )
        .is_err(),
        "a sibling of the admitted root must still refuse"
    );
}

#[cfg(unix)]
#[test]
fn root_alias_documents_resolve_and_open_under_the_admitted_directory() {
    let (temp, root, root_url, root_dir) = admitted_root();
    let alias = temp.path().join("alias");
    symlink(&root, &alias).expect("root alias");
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn inside() {}\n").unwrap();
    let uri = Url::from_file_path(alias.join("src/lib.rs")).unwrap();
    let saved_document = validated_document_path(
        &root,
        &canonical_root_identity(&root),
        &root_url,
        &root_dir,
        uri.as_str(),
    )
    .expect("client alias resolves to the admitted directory");
    assert_eq!(saved_document.absolute, root.join("src/lib.rs"));
    assert_eq!(saved_document.relative, Path::new("src/lib.rs"));
    let (_, mut file) = open_project_file(&root_dir, &saved_document.relative).unwrap();
    let mut source = String::new();
    file.read_to_string(&mut source).unwrap();
    assert_eq!(source, "pub fn inside() {}\n");

    let unsaved = Url::from_file_path(alias.join("src/new/unsaved.rs")).unwrap();
    let document = validated_document_path(
        &root,
        &canonical_root_identity(&root),
        &root_url,
        &root_dir,
        unsaved.as_str(),
    )
    .expect("unsaved alias buffer resolves through its parent");
    assert_eq!(document.relative, Path::new("src/new/unsaved.rs"));

    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("lib.rs"), "outside evidence").unwrap();
    symlink(&outside, root.join("escape")).unwrap();
    let escaped = Url::from_file_path(alias.join("escape/lib.rs")).unwrap();
    assert!(
        validated_document_path(
            &root,
            &canonical_root_identity(&root),
            &root_url,
            &root_dir,
            escaped.as_str()
        )
        .is_err()
    );

    // A successful resolution does not grant an ambient-path read. The
    // retained directory capability still refuses a replacement escape.
    std::fs::remove_file(root.join("src/lib.rs")).unwrap();
    symlink(outside.join("lib.rs"), root.join("src/lib.rs")).unwrap();
    assert!(open_project_file(&root_dir, &saved_document.relative).is_err());
}

#[cfg(unix)]
#[test]
fn disk_document_open_rejects_symlink_escape() {
    let (temp, _root, _root_url, root_dir) = admitted_root();
    let outside = temp.path().join("outside.rs");
    std::fs::write(&outside, "fn outside() {}\n").expect("write outside document");
    symlink(&outside, temp.path().join("root").join("escape.rs")).expect("create escape");

    assert!(open_project_file(&root_dir, Path::new("escape.rs")).is_err());
}
