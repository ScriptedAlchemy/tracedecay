//! Cold publication authentication must not return a pointer from a retired inode.
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, symlink};

use tempfile::TempDir;
use tracedecay_code_index::production::CodeIndexPublicationStoreErrorV1;

use super::DaemonCodeIndexPublicationStoreV1;

#[test]
fn cold_pointer_witness_refuses_same_byte_inode_swap_and_symlink_alias() {
    let root = TempDir::new().expect("isolated pointer directory");
    let pointer = root.path().join("active-code-generation-v1.json");
    let retired = root.path().join("retired-pointer");
    let replacement = root.path().join("replacement-pointer");
    let bytes = br#"{"published":"real-cold-pointer-witness"}"#;
    fs::write(&pointer, bytes).expect("write nonempty original pointer");
    let named_before = pointer.symlink_metadata().expect("original pointer inode");
    let mut opened = File::open(&pointer).expect("open original pointer once");
    let mut readback = Vec::new();
    opened
        .read_to_end(&mut readback)
        .expect("read the exact original pointer bytes");
    assert_eq!(readback.as_slice(), bytes);
    assert!(
        DaemonCodeIndexPublicationStoreV1::require_cold_pointer_witness(
            &pointer,
            &mut opened,
            &named_before,
            &readback,
        )
        .is_ok(),
        "unchanged real pointer must authenticate"
    );

    fs::write(&replacement, bytes).expect("different inode with identical bytes");
    fs::rename(&pointer, &retired).expect("retire original pathname");
    fs::rename(&replacement, &pointer).expect("replace pathname with same bytes");
    let replacement_identity = pointer.symlink_metadata().expect("replacement inode");
    assert_ne!(
        (named_before.dev(), named_before.ino()),
        (replacement_identity.dev(), replacement_identity.ino()),
        "a real different inode must be used as the negative control"
    );
    assert_eq!(fs::read(&pointer).expect("replacement bytes"), readback);
    assert!(matches!(
        DaemonCodeIndexPublicationStoreV1::require_cold_pointer_witness(
            &pointer,
            &mut opened,
            &named_before,
            &readback,
        ),
        Err(CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_))
    ));

    fs::remove_file(&pointer).expect("remove replacement");
    symlink(&retired, &pointer).expect("symlink to the exact opened inode");
    let followed = fs::metadata(&pointer).expect("positive symlink target control");
    assert_eq!(
        (named_before.dev(), named_before.ino()),
        (followed.dev(), followed.ino())
    );
    assert!(
        pointer
            .symlink_metadata()
            .expect("symlink entry")
            .file_type()
            .is_symlink()
    );
    assert!(matches!(
        DaemonCodeIndexPublicationStoreV1::require_cold_pointer_witness(
            &pointer,
            &mut opened,
            &named_before,
            &readback,
        ),
        Err(CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_))
    ));

    fs::remove_file(&pointer).expect("remove symlink");
    fs::rename(&retired, &pointer).expect("restore original inode");
    // Rename updates ctime on Unix; authenticate the restored pathname's
    // current metadata while retaining the original open descriptor.
    let restored_identity = pointer.symlink_metadata().expect("restored inode");
    assert_eq!(
        (named_before.dev(), named_before.ino()),
        (restored_identity.dev(), restored_identity.ino())
    );
    assert!(
        DaemonCodeIndexPublicationStoreV1::require_cold_pointer_witness(
            &pointer,
            &mut opened,
            &restored_identity,
            &readback,
        )
        .is_ok(),
        "the original inode and exact bytes must recover without another open"
    );
}
