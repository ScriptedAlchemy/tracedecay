//! The kernel's registered-schema port must stay fail-closed for every build
//! that is not `tracedecay-runtime-core`'s own unit-test binary.
//!
//! `tracedecay_runtime_core::ports::registered_schema` refuses to initialise a
//! profile- or session-scoped shard until the composition root registers the
//! real schema installer, because an uninitialised registered store is not safe
//! to publish. The kernel relaxes that to an empty sidecar under `cfg(test)`
//! for *its own* fixtures only — it sits below `tracedecay-global-db` and can
//! never install the real schema.
//!
//! This lives in its own integration-test binary on purpose:
//!
//! - it links `tracedecay-runtime-core` as an ordinary dependency, so the
//!   `cfg(test)` relaxation is not compiled in and the production arm is the
//!   one under test;
//! - the installer slot is a process-global `OnceLock`, so the assertion is
//!   only meaningful in a process where nothing has registered. Keeping this
//!   file free of any harness that opens a registered store guarantees that.
//!
//! Deleting or weakening this test removes the only executable proof that a
//! production process still refuses an unregistered shard.

use tracedecay_runtime_core::db::engine::TestConnection;
use tracedecay_runtime_core::errors::TraceDecayError;

#[tokio::test]
async fn unregistered_installer_refuses_to_initialize_a_registered_shard() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let connection = TestConnection::open(&directory.path().join("registered.db"));

    let error =
        tracedecay_runtime_core::ports::registered_schema::ensure_registered_schema(&connection)
            .await
            .expect_err("an unregistered installer must fail closed, never converge silently");

    assert!(
        matches!(error, TraceDecayError::Database { .. }),
        "fail-closed error must be a Database error, got: {error:?}"
    );
    let rendered = error.to_string();
    assert!(
        rendered.contains("no registered global/session schema installer is registered"),
        "unexpected fail-closed message: {rendered}"
    );
    assert!(
        rendered.contains("create initialized global/session schema"),
        "fail-closed error must name the refused operation: {rendered}"
    );
}
