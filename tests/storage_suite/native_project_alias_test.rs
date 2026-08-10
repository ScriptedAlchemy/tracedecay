#![cfg(any(unix, windows))]

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;

#[cfg(unix)]
fn non_unicode_alias_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    (
        root.join(OsString::from_vec(vec![b'p', 0x80])),
        root.join(OsString::from_vec(vec![b'p', 0x81])),
    )
}

#[cfg(windows)]
fn non_unicode_alias_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;

    (
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd800])),
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd801])),
    )
}

#[tokio::test]
async fn project_alias_lookup_preserves_distinct_native_paths() {
    let dir = TempDir::new().unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path().join("profile"))
        .await
        .unwrap();
    let (first, second) = non_unicode_alias_paths(dir.path());
    assert_eq!(
        first.to_string_lossy(),
        second.to_string_lossy(),
        "fixture paths must collide under lossy Unicode conversion"
    );

    db.upsert_code_project("proj_native_first", &first, None, None, None)
        .await
        .unwrap();
    db.upsert_code_project("proj_native_second", &second, None, None, None)
        .await
        .unwrap();

    let first_context = db
        .project_registry_context_by_alias(&first)
        .await
        .unwrap()
        .unwrap();
    let second_context = db
        .project_registry_context_by_alias(&second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_context.project.project_id, "proj_native_first");
    assert_eq!(second_context.project.project_id, "proj_native_second");
    assert_ne!(
        first_context.aliases[0].alias_path,
        second_context.aliases[0].alias_path
    );
    db.checkpoint_profile_database_for_test().await;
    drop(db);
}
