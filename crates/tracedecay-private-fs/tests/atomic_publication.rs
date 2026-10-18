//! Windows atomic publication through the crate's public API.
//!
//! The payload-shape unit tests in `src/windows.rs` prove what
//! `SetFileInformationByHandle` is handed; they cannot prove that Windows
//! accepts it. These exercise the two publication shapes the product actually
//! performs, a first publish onto an absent name, and a POSIX replace of an
//! occupied name while a delete-sharing reader holds the displaced file, so a
//! payload Windows rejects (most visibly `ERROR_INVALID_PARAMETER`, 87, from a
//! parent handle in `RootDirectory`) fails in CI rather than in the field.
#![cfg(windows)]

use std::fs;
use std::io::{Read, Write};

use tracedecay_private_fs::{
    create_private_directory, create_private_file, open_private_file, replace_file_atomically,
};

fn staged(directory: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = directory.join(name);
    let mut file = create_private_file(&path).expect("stage a private replacement");
    file.write_all(bytes).expect("write the staged bytes");
    drop(file);
    path
}

/// The first publish: nothing occupies the destination name yet, so the rename
/// creates it. `REPLACE_IF_EXISTS` must not make an absent destination an
/// error, and the returned handle must be the published file.
#[test]
fn a_first_publish_creates_the_destination_and_returns_its_handle() {
    let root = tempfile::tempdir().expect("publication root");
    let store = root.path().join("store");
    create_private_directory(&store).expect("private store directory");
    let destination = store.join("record");
    let source = staged(&store, "record.staging", b"first");
    assert!(!destination.exists());

    let mut published =
        replace_file_atomically(&source, &destination).expect("first publish must succeed");

    let mut contents = Vec::new();
    published
        .read_to_end(&mut contents)
        .expect("read back through the published handle");
    assert_eq!(contents, b"first");
    assert_eq!(fs::read(&destination).expect("published bytes"), b"first");
    assert!(!source.exists(), "the staged name must not survive");
}

/// The republish: a delete-sharing reader holds the file being displaced.
/// POSIX semantics unlink the old name immediately, so the publish succeeds
/// while that reader keeps reading the bytes it opened.
#[test]
fn a_posix_replace_displaces_a_file_a_delete_sharing_reader_still_reads() {
    let root = tempfile::tempdir().expect("publication root");
    let store = root.path().join("store");
    create_private_directory(&store).expect("private store directory");
    let destination = staged(&store, "record", b"old");
    let mut reader = open_private_file(&destination).expect("delete-sharing reader");
    let source = staged(&store, "record.staging", b"new");

    let published =
        replace_file_atomically(&source, &destination).expect("republish must displace the reader");
    drop(published);

    let mut displaced = Vec::new();
    reader
        .read_to_end(&mut displaced)
        .expect("the displaced reader keeps its bytes");
    assert_eq!(displaced, b"old");
    assert_eq!(fs::read(&destination).expect("republished bytes"), b"new");
    assert!(!source.exists(), "the staged name must not survive");
}
