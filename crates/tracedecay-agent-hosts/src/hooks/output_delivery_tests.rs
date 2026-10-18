use std::time::Duration;

use tracedecay_domain::NativeHostIdentityV1;
use tracedecay_private_fs::framed_log::sync_latency;
use tracedecay_runtime_core::config::ProfileRoot;

use super::{hook_output_owner_event_id, store_layout, write_hook_output};

const HOOKS: usize = 8;

#[tokio::test]
async fn hook_output_does_not_recreate_a_store_removed_by_maintenance() {
    let home = tempfile::tempdir().unwrap();
    let profile = ProfileRoot::under_home(home.path());
    let project = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let project_id = "proj_hook_output_maintenance";
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(&project_root, project_id)
        .unwrap();
    let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        &project_root,
        profile.data_dir(),
        project_id,
    )
    .unwrap();
    std::fs::create_dir_all(&layout.data_root).unwrap();
    assert!(store_layout::enrolled_layout(profile.data_dir(), &project_root).is_some());
    let host = NativeHostIdentityV1::Hermes;
    let event = r#"{"session_id":"maintenance","hook":"stop"}"#;
    let spool = tracedecay_hooks::hook_delivery_receipt_spool_root(&layout.data_root, host);
    let maintenance = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        profile.data_dir(),
        "wipe",
    )
    .unwrap();

    assert!(!write_hook_output(&profile, Some(&project_root), host, event, "{}").await);
    assert!(!spool.exists());
    std::fs::remove_dir_all(&layout.data_root).unwrap();
    assert!(!write_hook_output(&profile, Some(&project_root), host, event, "{}").await);
    assert!(!layout.data_root.exists());

    drop(maintenance);
    assert!(!write_hook_output(&profile, Some(&project_root), host, event, "{}").await);
    assert!(!layout.data_root.exists());

    std::fs::create_dir_all(&layout.data_root).unwrap();
    assert!(write_hook_output(&profile, Some(&project_root), host, event, "{}").await);
    let receipts = tracedecay_hooks::HookDeliveryReceiptSpoolV1::open(&spool, Duration::ZERO)
        .unwrap()
        .pending(8)
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].settlement.attempt.owner_event_id,
        hook_output_owner_event_id(host, event, "{}").unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_hooks_on_a_slow_disk_all_succeed_with_one_receipt_each() {
    let profile_home = tempfile::tempdir().unwrap();
    let profile = ProfileRoot::under_home(profile_home.path());
    let project = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        "proj_hook_output_contention",
    )
    .unwrap();
    let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        &project_root,
        profile.data_dir(),
        "proj_hook_output_contention",
    )
    .unwrap();
    assert!(store_layout::enrolled_layout(profile.data_dir(), &project_root).is_some());
    let host = NativeHostIdentityV1::Hermes;
    let spool = tracedecay_hooks::hook_delivery_receipt_spool_root(&layout.data_root, host);
    drop(tracedecay_hooks::HookDeliveryReceiptSpoolV1::open(&spool, Duration::ZERO).unwrap());
    // Every receipt-spool durability barrier waits like a loaded disk (#2659).
    let _slow_disk = sync_latency::inject(&spool, Duration::from_millis(20));

    let events = (0..HOOKS)
        .map(|index| format!(r#"{{"session_id":"session-{index}","hook":"stop"}}"#))
        .collect::<Vec<_>>();
    let hooks = events
        .iter()
        .map(|event| {
            let profile = profile.clone();
            let project_root = project_root.clone();
            let event = event.clone();
            tokio::spawn(async move {
                write_hook_output(&profile, Some(&project_root), host, &event, "{}").await
            })
        })
        .collect::<Vec<_>>();
    let mut succeeded = Vec::new();
    for hook in hooks {
        succeeded.push(hook.await.unwrap());
    }

    assert_eq!(succeeded, vec![true; HOOKS]);
    let mut settled = tracedecay_hooks::HookDeliveryReceiptSpoolV1::open(&spool, Duration::ZERO)
        .unwrap()
        .pending(64)
        .unwrap()
        .into_iter()
        .map(|receipt| receipt.settlement.attempt.owner_event_id)
        .collect::<Vec<_>>();
    let mut expected = events
        .iter()
        .map(|event| hook_output_owner_event_id(host, event, "{}").unwrap())
        .collect::<Vec<_>>();
    settled.sort();
    expected.sort();
    assert_eq!(settled, expected);
}
