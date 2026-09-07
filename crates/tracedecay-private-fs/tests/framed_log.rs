use std::fs;

use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write_prepared, read_bounded};

#[test]
fn prepared_publish_is_private_and_readable_through_the_adapter() {
    let root = tempfile::tempdir().expect("publish fixture root");
    let destination = root.path().join("config.json");
    let mut prepared = 0_u32;

    atomic_write_prepared(
        &destination,
        "fixture",
        b"published",
        |temporary| {
            prepared += 1;
            assert!(temporary.exists(), "prepare observes the staging file");
            Ok(())
        },
        DirectorySyncPolicy::TolerateUnsupported,
    )
    .expect("prepared publish");

    assert_eq!(prepared, 1);
    assert_eq!(
        read_bounded(&destination, 64).expect("bounded read"),
        Some(b"published".to_vec())
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&destination)
                .expect("published metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
