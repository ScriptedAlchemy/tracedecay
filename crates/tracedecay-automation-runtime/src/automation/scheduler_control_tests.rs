#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::Path;
use std::sync::Barrier;

use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, with_owned_temp_publish};
use tracedecay_runtime_core::native_runtime;
use tracedecay_runtime_core::storage::with_durable_namespace_sync_fault_for_test;

use super::{
    AutomationSchedulerControl, load_scheduler_control, publish_scheduler_control,
    save_scheduler_control, scheduler_control_path,
};

fn load(root: &Path) -> Result<AutomationSchedulerControl, String> {
    native_runtime::block_on(load_scheduler_control(root))
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

fn save(root: &Path, paused: bool) -> Result<(), String> {
    native_runtime::block_on(save_scheduler_control(
        root,
        &AutomationSchedulerControl { paused },
    ))
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

#[test]
fn missing_control_defaults_but_corruption_is_not_an_unpause() {
    let root = tempfile::tempdir().unwrap();
    assert!(!load(root.path()).unwrap().paused);
    std::fs::write(scheduler_control_path(root.path()), b"{").unwrap();
    assert!(load(root.path()).is_err());
}

#[test]
fn acknowledged_updates_are_read_back_without_staging_residue() {
    let root = tempfile::tempdir().unwrap();
    let dashboard = root.path().join("new/dashboard");
    for paused in [true, false, true] {
        save(&dashboard, paused).unwrap();
        assert_eq!(load(&dashboard).unwrap().paused, paused);
        let entries = std::fs::read_dir(&dashboard)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![scheduler_control_path(&dashboard)]);
    }
}

#[test]
fn readers_never_observe_a_partially_published_control() {
    let root = tempfile::tempdir().unwrap();
    save(root.path(), false).unwrap();
    let barrier = Barrier::new(4);
    let failures = std::thread::scope(|scope| {
        let readers = (0..3)
            .map(|_| {
                scope.spawn(|| {
                    let mut failures = Vec::new();
                    for _ in 0..64 {
                        barrier.wait();
                        if let Err(error) = load(root.path()) {
                            failures.push(error);
                        }
                        barrier.wait();
                    }
                    failures
                })
            })
            .collect::<Vec<_>>();
        let mut failures = Vec::new();
        for index in 0..64 {
            barrier.wait();
            if let Err(error) = save(root.path(), index % 2 == 0) {
                failures.push(error);
            }
            barrier.wait();
        }
        for reader in readers {
            failures.extend(reader.join().unwrap());
        }
        failures
    });
    assert!(failures.is_empty(), "publication failures: {failures:?}");
}

#[test]
fn a_failed_namespace_barrier_preserves_the_prior_control() {
    let root = tempfile::tempdir().unwrap();
    save(root.path(), true).unwrap();
    let outcome = with_durable_namespace_sync_fault_for_test(1, || {
        publish_scheduler_control(root.path(), br#"{"paused":false}"#)
    });
    assert!(outcome.is_err(), "a failed durability barrier cannot be acknowledged");
    assert!(load(root.path()).unwrap().paused);
}

#[test]
fn a_failed_staged_publish_preserves_old_bytes_and_cleans_its_temp() {
    let root = tempfile::tempdir().unwrap();
    save(root.path(), true).unwrap();
    let target = scheduler_control_path(root.path());
    let before = std::fs::read(&target).unwrap();
    let failure = with_owned_temp_publish(
        &target,
        "scheduler-control",
        |_temporary, _destination| Err(std::io::Error::other("injected before publication")),
        |output| output.write_all(br#"{"paused":false}"#),
        DirectorySyncPolicy::Strict,
    );
    assert!(failure.is_err());
    assert_eq!(std::fs::read(&target).unwrap(), before);
    assert!(load(root.path()).unwrap().paused);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn published_control_is_owner_private() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    save(root.path(), true).unwrap();
    let permissions = std::fs::metadata(scheduler_control_path(root.path()))
        .unwrap()
        .permissions();
    assert_eq!(permissions.mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn a_control_symlink_cannot_overwrite_an_unrelated_file() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let external = other.path().join("unrelated.json");
    std::fs::write(&external, b"unrelated bytes").unwrap();
    std::os::unix::fs::symlink(&external, scheduler_control_path(root.path())).unwrap();
    assert!(save(root.path(), true).is_err());
    assert_eq!(std::fs::read(&external).unwrap(), b"unrelated bytes");
}
