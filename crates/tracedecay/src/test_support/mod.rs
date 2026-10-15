//! Test-only composition-root fixtures. Compiled under `cfg(test)` or
//! `test-helpers`, never into a default or `production` library build.

use std::path::PathBuf;

use tokio::time::Duration;
use tracedecay_code_index::graph_projection::{
    CodeGraphCatalogReleaseV1, CodeGraphProjectionStore,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

pub mod git;
#[cfg(any(test, feature = "test-transport"))]
pub mod host_admission;

/// Tokio-dependent paths this process skipped, plus synchronous writer waits
/// that parked a native worker. An off-Tokio test asserts this does not move.
#[cfg(test)]
pub(crate) fn native_runtime_hazards() -> u64 {
    [
        "daemon-service.operation_events.sse_disconnect",
        "daemon-service.hook_orchestration.admit",
        "daemon-service.work_blocked_interval_recovery.start",
        "daemon-service.observability.mount_drain",
        "daemon-service.observability.settle_drain",
        "daemon-service.observability.producer_drop",
        "daemon-service.work_attempt.spawn_for_worktree",
        "daemon-service.host_admission.bootstrap",
        "daemon-service.shutdown.runner",
        "daemon-service.shutdown_coordination.runner",
    ]
    .into_iter()
    .map(tracedecay_runtime_core::native_runtime::tokio_runtime_missing_count)
    .sum::<u64>()
        + tracedecay_rusqlite_runtime::exact_sql::native_thread_writer_waits()
}

/// Park after core publication when `TRACEDECAY_TEST_HOLD_AFTER_CORE_PUBLISH`
/// names a file. The core graph-tool owner is already registered and the
/// route is ready, so a session action can land before the full server
/// replaces that owner. Removing the file releases the open.
///
/// This module is absent from a default or `production` build, so that
/// variable is not read and cannot pause project open there.
pub async fn hold_after_core_publish_for_test() -> Result<()> {
    hold_for_test("TRACEDECAY_TEST_HOLD_AFTER_CORE_PUBLISH", b"core").await
}

/// Park a project open after it mounted the project session database when
/// `TRACEDECAY_TEST_HOLD_AFTER_PROJECT_SESSIONS` names a file: a stand-in
/// for a store mount that cannot observe cancellation from inside.
pub async fn hold_after_project_sessions_for_test() -> Result<()> {
    hold_for_test("TRACEDECAY_TEST_HOLD_AFTER_PROJECT_SESSIONS", b"sessions").await
}

/// Park a project open after its independent production owners registered,
/// the Hook V2 replay consumer among them, when
/// `TRACEDECAY_TEST_HOLD_AFTER_INDEPENDENT_OWNERS` names a file.
pub async fn hold_after_independent_owners_for_test() -> Result<()> {
    hold_for_test("TRACEDECAY_TEST_HOLD_AFTER_INDEPENDENT_OWNERS", b"owners").await
}

/// Park a project open after capacity admission and before it binds its route
/// into the bounded project-server cache, when
/// `TRACEDECAY_TEST_HOLD_BEFORE_ROUTE_BIND` names a file.
pub async fn hold_before_route_bind_for_test() -> Result<()> {
    hold_for_test("TRACEDECAY_TEST_HOLD_BEFORE_ROUTE_BIND", b"bind").await
}

/// Park a route-bind eviction's detached retirement before it waits out the
/// in-flight opens and retires the evicted graph's process owners, when
/// `TRACEDECAY_TEST_HOLD_BEFORE_EVICTED_OWNER_RETIRE` names a file.
pub async fn hold_before_evicted_owner_retire_for_test() -> Result<()> {
    hold_for_test(
        "TRACEDECAY_TEST_HOLD_BEFORE_EVICTED_OWNER_RETIRE",
        b"evicted-owner-retire",
    )
    .await
}

/// Park unserved-owner retirement after its served check found no published
/// server and before it removes the process owners, when
/// `TRACEDECAY_TEST_HOLD_AFTER_UNSERVED_OWNER_CHECK` names a file.
pub async fn hold_after_unserved_owner_check_for_test() -> Result<()> {
    hold_for_test(
        "TRACEDECAY_TEST_HOLD_AFTER_UNSERVED_OWNER_CHECK",
        b"unserved-owner-check",
    )
    .await
}

/// Park a retention maintenance tick after it listed the mounted stores and
/// before it runs any store unit, when
/// `TRACEDECAY_TEST_HOLD_AFTER_MAINTENANCE_LISTING` names a file.
pub async fn hold_after_maintenance_listing_for_test() -> Result<()> {
    hold_for_test(
        "TRACEDECAY_TEST_HOLD_AFTER_MAINTENANCE_LISTING",
        b"maintenance-listing",
    )
    .await
}

/// Releases `store`'s interactive catalog once nothing holds it. `Busy` means
/// ask again later: after a test drops its own readers, a background catalog
/// warm or residency pass can still hold the catalog for a moment.
pub async fn release_idle_interactive_catalog(
    store: &CodeGraphProjectionStore,
) -> CodeGraphCatalogReleaseV1 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match store.release_interactive_catalog() {
            CodeGraphCatalogReleaseV1::Busy if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            release => return release,
        }
    }
}

/// Records `<hold>.entered`, then waits until the file `variable` names is
/// removed.
async fn hold_for_test(variable: &str, stage: &[u8]) -> Result<()> {
    let Some(hold) = std::env::var_os(variable) else {
        return Ok(());
    };
    let hold = PathBuf::from(hold);
    let entered = PathBuf::from(format!("{}.entered", hold.display()));
    std::fs::write(&entered, stage).map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not record the test hold at {}: {error}",
            entered.display()
        ),
    })?;
    while hold.exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}
