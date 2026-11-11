//! Windows cold-pointer checks must refuse retired or rewritten inode bytes.
use std::fs::{self, File};
use std::io::{Read, Write};

use tracedecay_code_index::production::CodeIndexPublicationStoreErrorV1;

use super::DaemonCodeIndexPublicationStoreV1;

#[test]
fn cold_pointer_witness_refuses_same_byte_windows_inode_rotation() {
    let root = tempfile::tempdir().expect("isolated pointer root");
    let pointer = root.path().join("active-code-generation-v1.json");
    let retired = root.path().join("retired-pointer");
    let replacement = root.path().join("replacement-pointer");
    let bytes = br#"{"published":"actual-pointer"}"#;
    fs::write(&pointer, bytes).expect("write original pointer");
    let mut pinned = File::open(&pointer).expect("pin the original inode");
    let mut readback = Vec::new();
    pinned
        .read_to_end(&mut readback)
        .expect("read original bytes");
    assert_eq!(readback, bytes);
    let witness = DaemonCodeIndexPublicationStoreV1::require_non_unix_pointer_witness;
    assert!(witness(&pointer, &mut pinned, &readback).is_ok());

    fs::rename(&pointer, &retired).expect("retire pathname with descriptor still open");
    assert!(matches!(
        witness(&pointer, &mut pinned, &readback),
        Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap)
    ));
    fs::write(&replacement, bytes).expect("same-byte new inode");
    fs::rename(&replacement, &pointer).expect("publish replacement inode");
    assert_eq!(fs::read(&pointer).expect("replacement bytes"), bytes);
    assert!(matches!(
        witness(&pointer, &mut pinned, &readback),
        Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap)
    ));
    drop(pinned);
    let mut fresh = File::open(&pointer).expect("open actual current pointer");
    assert!(witness(&pointer, &mut fresh, bytes).is_ok());
}

#[test]
fn cold_pointer_witness_refuses_in_place_windows_rewrite() {
    let root = tempfile::tempdir().expect("isolated pointer root");
    let pointer = root.path().join("active-code-generation-v1.json");
    let original = b"active code-index pointer v1";
    let changed = b"mutant code-index pointer v1";
    assert_eq!(original.len(), changed.len());
    fs::write(&pointer, original).expect("original pointer bytes");
    let mut pinned = File::open(&pointer).expect("opened pointer descriptor");
    assert!(
        DaemonCodeIndexPublicationStoreV1::require_non_unix_pointer_witness(
            &pointer,
            &mut pinned,
            original,
        )
        .is_ok()
    );

    let mut writer = File::options()
        .write(true)
        .open(&pointer)
        .expect("rewrite same inode");
    writer.write_all(changed).expect("same-length rewrite");
    drop(writer);
    assert_eq!(
        fs::read(&pointer).expect("changed pointer survives"),
        changed
    );
    assert!(matches!(
        DaemonCodeIndexPublicationStoreV1::require_non_unix_pointer_witness(
            &pointer,
            &mut pinned,
            original,
        ),
        Err(CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_))
    ));
}

#[test]
fn warm_pointer_witness_reuses_a_pinned_file_and_refuses_rotation() {
    let root = tempfile::tempdir().expect("isolated warm pointer root");
    let pointer = root.path().join("active-code-generation-v1.json");
    let retired = root.path().join("retired-pointer");
    let bytes = br#"{"generation":"stable-pointer"}"#;
    fs::write(&pointer, bytes).expect("original pointer");
    let mut pinned = File::open(&pointer).expect("one original pointer open");
    let digest = DaemonCodeIndexPublicationStoreV1::state_digest(bytes);

    for _ in 0..17 {
        let current = DaemonCodeIndexPublicationStoreV1::read_cached_non_unix_pointer(
            &pointer,
            &mut pinned,
            &digest,
        )
        .expect("warm read of the same authenticated handle");
        assert_eq!(current, bytes);
    }
    fs::rename(&pointer, &retired).expect("retire the pinned pathname");
    fs::write(&pointer, bytes).expect("same bytes in a fresh inode");
    assert!(matches!(
        DaemonCodeIndexPublicationStoreV1::read_cached_non_unix_pointer(
            &pointer,
            &mut pinned,
            &digest,
        ),
        Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap)
    ));
    assert_eq!(fs::read(&pointer).expect("replacement preserved"), bytes);
}

#[test]
fn warm_pointer_witness_refuses_same_inode_changed_bytes() {
    let root = tempfile::tempdir().expect("isolated pointer root");
    let pointer = root.path().join("active-code-generation-v1.json");
    let original = b"active code-index pointer v1";
    let changed = b"mutant code-index pointer v1";
    assert_eq!(original.len(), changed.len());
    fs::write(&pointer, original).expect("original pointer");
    let mut pinned = File::open(&pointer).expect("pinned pointer");
    let digest = DaemonCodeIndexPublicationStoreV1::state_digest(original);
    assert_eq!(
        DaemonCodeIndexPublicationStoreV1::read_cached_non_unix_pointer(
            &pointer,
            &mut pinned,
            &digest,
        )
        .expect("original unchanged bytes"),
        original
    );
    let mut writer = File::options()
        .write(true)
        .open(&pointer)
        .expect("rewrite the same inode");
    writer.write_all(changed).expect("mutated pointer bytes");
    drop(writer);
    assert!(matches!(
        DaemonCodeIndexPublicationStoreV1::read_cached_non_unix_pointer(
            &pointer,
            &mut pinned,
            &digest,
        ),
        Err(CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_))
    ));
    assert_eq!(fs::read(&pointer).expect("changed bytes preserved"), changed);
}
