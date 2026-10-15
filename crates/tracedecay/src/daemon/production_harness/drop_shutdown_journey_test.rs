//! A test body that ends without `shutdown().await` drops its harness and
//! then stops its runtime. The composition must still release the session
//! runtime registry, or the registry, every store it mounted, and their
//! reader and writer threads stay alive for the rest of the test process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::ProductionProjectCompositionHarnessV1;
use super::journey_test_support::initialize_project;

const RELEASE_DEADLINE: Duration = Duration::from_secs(60);

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
