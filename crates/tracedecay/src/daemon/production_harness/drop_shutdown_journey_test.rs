//! A test body that ends without `shutdown().await` drops its harness and
//! then stops its runtime. The composition must still release the session
//! runtime registry, or the registry, every store it mounted, and their
//! reader and writer threads stay alive for the rest of the test process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::ProductionProjectCompositionHarnessV1;
use super::journey_test_support::initialize_project;
use tracedecay_runtime_core::storage::default_profile_sharded_layout;

const RELEASE_DEADLINE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn a_failed_later_mount_releases_the_whole_composition_before_returning() {
    let isolation = TempDir::new().expect("production harness isolation");
    let first = isolation.path().join("first");
    let refused = isolation.path().join("refused");
    initialize_project(&first);
    initialize_project(&refused);
    let retired = refused.join(".tracedecay");
    std::fs::create_dir(&retired).expect("retired checkout layout");
    std::fs::write(retired.join("config.json"), "{}").expect("retired configuration");

    let error = match ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [first.clone(), refused.clone()],
    )
    .await
    {
        Ok(harness) => {
            harness.shutdown().await;
            panic!("the retired checkout layout must refuse the second mount");
        }
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .unwrap_or_else(|| panic!("expected the retired-layout refusal: {error:?}"));
    assert_eq!(authority, "project store");
    assert!(reason.contains("retired checkout-local layout"), "{reason}");

    let profile = isolation.path().join("profile");
    let first_layout = default_profile_sharded_layout(&first, &profile)
        .expect("the first mount enrolled its exact project identity");
    let stores = [
        profile.join("global.db"),
        profile.join("user-sessions.db"),
        first_layout.data_root.join("sessions.db"),
        first_layout.graph_db_path,
    ];
    for store in &stores {
        assert!(
            store.is_file(),
            "the first mount opened {}",
            store.display()
        );
        let mut wal = store.as_os_str().to_owned();
        wal.push("-wal");
        let bytes =
            std::fs::metadata(std::path::PathBuf::from(wal)).map_or(0, |metadata| metadata.len());
        assert_eq!(
            bytes,
            0,
            "failed-open rollback must close the writer for {} before returning",
            store.display()
        );
    }

    std::fs::remove_file(retired.join("config.json")).expect("remove the fixture refusal");
    std::fs::remove_dir(retired).expect("remove the empty retired fixture directory");
    let harness = ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [first.clone(), refused.clone()],
    )
    .await
    .expect("both exact projects reopen after failed-open rollback");
    assert!(harness.server(&first).is_ok());
    assert!(harness.server(&refused).is_ok());
    harness.shutdown().await;
}

#[test]
fn a_harness_dropped_as_its_runtime_stops_releases_its_session_registry() {
    let isolation = TempDir::new().expect("production harness isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let registry = runtime.block_on(async {
        let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project])
            .await
            .expect("production harness");
        let registry = harness
            .resources
            .as_ref()
            .expect("open harness resources")
            .store_administration
            .session_runtime_registry()
            .await
            .expect("session runtime registry");
        Arc::downgrade(&registry)
    });
    drop(runtime);

    let deadline = Instant::now() + RELEASE_DEADLINE;
    while registry.strong_count() > 0 {
        assert!(
            Instant::now() < deadline,
            "the dropped harness still holds its session runtime registry after {RELEASE_DEADLINE:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
