//! Injection points for subsystems that stay above the kernel.
//!
//! The one-shot crate split moved the runtime kernel down but left some
//! collaborators above it: the registered global database, the daemon session
//! registry, and branch-admin recovery. Each is expressed here as a port the
//! root crate registers into, so the kernel never names an upward module path.
//!
//! The store-runtime registry is no longer one of them. `StoreRuntimeSource`
//! existed only because `daemon::store_runtime` had stayed in the root; that
//! tree now lives in `crate::store_runtime`, so `db::connection` retains the
//! concrete `store_runtime::registry::StoreRuntimeHandle` directly and the port
//! was deleted.
//!
//! Every port fails closed (or degrades to a documented no-op) when the root
//! never registers, which keeps unit tests of the kernel alone runnable.
//! `crates/tracedecay-runtime-core/SEAMS.md` tracks which registration sites
//! the landing still owes.

/// Gate that refuses a branch-add lock while a branch-admin mutation is still
/// pending recovery.
///
/// The journal reader lives in the root crate's `branch::admin::transaction`
/// module, which did not move. Until the root registers, the gate is a no-op:
/// locking still serializes correctly, it just does not refuse a lock during
/// an unfinished admin mutation.
pub mod branch_admin_recovery {
    use std::path::Path;
    use std::sync::OnceLock;

    use crate::errors::Result;

    /// Signature of the pending-recovery gate.
    pub type Gate = fn(&Path) -> Result<()>;

    static GATE: OnceLock<Gate> = OnceLock::new();

    /// Registers the root crate's pending branch-admin recovery gate.
    ///
    /// Idempotent: the first registration wins and later ones are ignored, so
    /// concurrent daemon and CLI initialisation cannot fight over it.
    pub fn register(gate: Gate) {
        let _ = GATE.set(gate);
    }

    /// Runs the registered gate, or succeeds when none is registered.
    pub fn ensure_no_pending_recovery(tracedecay_dir: &Path) -> Result<()> {
        match GATE.get() {
            Some(gate) => gate(tracedecay_dir),
            None => Ok(()),
        }
    }
}

/// Installer for the registered global/session schema.
///
/// `store_runtime::registry` initialises a freshly created profile- or
/// session-scoped shard by running the registered global-database schema
/// against the attachment it just opened. That schema lives in
/// `tracedecay-global-db`, which already depends on `tracedecay-migrate`, which
/// depends on this crate — so the kernel cannot name it without a Cargo cycle.
///
/// Unlike [`branch_admin_recovery`], this port **fails closed**: an
/// uninitialised profile or session store is not safe to publish, so an
/// unregistered installer refuses the open instead of pretending it converged.
pub mod registered_schema {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::OnceLock;

    use crate::db::engine::Connection;
    use crate::errors::{Result, TraceDecayError};

    /// Signature of the schema installer, boxed because it is stored as a
    /// plain function pointer rather than a generic.
    pub type Installer =
        for<'a> fn(&'a Connection) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

    static INSTALLER: OnceLock<Installer> = OnceLock::new();

    /// Registers the root crate's registered-schema installer.
    ///
    /// Idempotent: the first registration wins, so concurrent daemon and CLI
    /// initialisation cannot fight over it.
    pub fn register(installer: Installer) {
        let _ = INSTALLER.set(installer);
    }

    /// The fail-closed error returned when no installer is registered.
    ///
    /// Kept as a standalone constructor so the fail-closed contract can be
    /// asserted in a unit test without depending on the process-global
    /// [`INSTALLER`] slot, which any earlier test in the binary may already have
    /// populated.
    fn missing_installer_error() -> TraceDecayError {
        TraceDecayError::Database {
            message: "no registered global/session schema installer is registered; \
                      the root crate must call \
                      tracedecay_runtime_core::ports::registered_schema::register \
                      before opening a profile or session shard"
                .to_owned(),
            operation: "create initialized global/session schema".to_owned(),
        }
    }

    /// What an unregistered port does.
    ///
    /// Production, and every dependent crate's test build, fails closed: an
    /// uninitialised profile or session store must never be published.
    #[cfg(not(test))]
    fn unregistered_outcome() -> Result<()> {
        Err(missing_installer_error())
    }

    /// What an unregistered port does inside *this crate's own* unit tests.
    ///
    /// The kernel sits **below** `tracedecay-global-db`, which owns the real
    /// schema, so `cargo test -p tracedecay-runtime-core` cannot install it
    /// without a Cargo cycle. Every kernel fixture that reaches this port does
    /// so incidentally: `Database::publish_test_runtime` materialises a
    /// *profile* sidecar beside the graph shard the test actually exercises,
    /// and no kernel test reads a registered-schema table out of that sidecar.
    /// An empty sidecar is therefore the honest fixture, and it spares ~40
    /// kernel tests from hand-registering a schema they never query.
    ///
    /// This arm is compiled **only** for this crate's own test binary.
    /// Dependent crates build the kernel without `cfg(test)`, so they keep the
    /// fail-closed error until they register the real installer, and no
    /// production or `--all-features` binary is affected — `test-helpers` and
    /// `test-transport` deliberately do not reach it.
    #[cfg(test)]
    fn unregistered_outcome() -> Result<()> {
        Ok(())
    }

    /// Installs the registered global/session schema through `connection`.
    ///
    /// # Errors
    /// Returns [`TraceDecayError::Database`] when no installer is registered,
    /// or whatever the registered installer reports.
    pub async fn ensure_registered_schema(connection: &Connection) -> Result<()> {
        match INSTALLER.get() {
            Some(installer) => installer(connection).await,
            None => unregistered_outcome(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The port stays fail-closed: with no installer registered, the open
        /// path yields a `Database` error naming the missing registrar. This
        /// guards the production contract that an uninitialised profile or
        /// session store is never silently published.
        #[test]
        fn missing_installer_is_fail_closed() {
            let error = missing_installer_error();
            assert!(
                matches!(error, TraceDecayError::Database { .. }),
                "fail-closed error must be a Database error, got: {error:?}"
            );
            let rendered = error.to_string();
            assert!(
                rendered.contains("no registered global/session schema installer is registered"),
                "unexpected fail-closed message: {rendered}"
            );
        }
    }
}
