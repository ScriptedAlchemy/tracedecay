//! Test-only composition-root fixtures. Compiled under `cfg(test)` or
//! `test-helpers`, never into a default or `production` library build.

use std::path::PathBuf;

use tokio::time::Duration;
use tracedecay_domain::errors::{Result, TraceDecayError};

pub mod git;
#[cfg(any(test, feature = "test-transport"))]
pub mod host_admission;

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
