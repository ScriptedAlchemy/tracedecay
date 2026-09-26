use std::time::Duration;

use tempfile::TempDir;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

use super::{ALPHA_LIB_V1, CodeIndexSchedulerRegistryV1, GitFixture, test_project_id};

/// Daemon shutdown cancels synchronously while a mount, retirement, or read
/// may hold the async `mounted` map across an await. The early signal must
/// still reach the worker: it exits while that map is held, not at the join.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_the_worker_while_the_mounted_map_is_held() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount worktree");
    super::wait_for_initial_generation(&registry, fixture.path()).await;
    let root = canonical_existing_identity(fixture.path()).expect("canonical root");

    let mounted = registry.mounted.lock().await;
    registry.cancel();
    let worker = &mounted.get(&root).expect("mounted worker").task;
    let mut waited = Duration::ZERO;
    while !worker.is_finished() && waited < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += Duration::from_millis(10);
    }
    let exited_while_map_held = worker.is_finished();
    drop(mounted);
    registry.shutdown().await;

    assert!(
        exited_while_map_held,
        "the worker must observe cancel while another task holds the mounted map"
    );
}
