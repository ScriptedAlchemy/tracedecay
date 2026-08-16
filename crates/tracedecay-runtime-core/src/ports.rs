//! Injection points for subsystems that stay above the kernel.
//!
//! The one-shot crate split moved the runtime kernel down but left some
//! collaborators above it: the registered global database and the daemon
//! session registry. Each is expressed here as a port the
//! root crate registers into, so the kernel never names an upward module path.
//!
//! The store-runtime registry is no longer one of them. `StoreRuntimeSource`
//! existed only because `daemon::store_runtime` had stayed in the root; that
//! tree now lives in `crate::store_runtime`, so `db::connection` retains the
//! concrete `store_runtime::registry::StoreRuntimeClientLease` directly and the port
//! was deleted.
//!
//! Every port fails closed (or degrades to a documented no-op) when the root
//! never registers, which keeps unit tests of the kernel alone runnable.

/// Installer for the registered global/session schema.
///
/// `store_runtime::registry` initialises a freshly created profile- or
/// session-scoped shard by running the registered global-database schema
/// against the attachment it just opened. That schema lives in
/// `tracedecay-global-db`, which depends on this crate — so the kernel cannot
/// name it without a Cargo cycle.
///
/// This port **fails closed**: an uninitialised profile or session store is
/// not safe to publish, so an unregistered installer refuses the open instead
/// of pretending it converged.
pub mod registered_schema {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::OnceLock;

    use crate::db::engine::{
        Connection, Executor, QueryExecutor, Transaction, TransactionBehavior,
    };
    use crate::errors::{Result, TraceDecayError};
    use tracedecay_store::StoreRuntimeBindingV1;

    /// A sealed, initialization-only capability over the exact connection
    /// authorized while a registered Store runtime is being opened.
    ///
    /// It has no public constructor and never exposes its engine connection,
    /// authority, runtime, or exact SQL handle. The Store-open path creates it
    /// only after binding, locator, file identity, and write authority have
    /// been validated for a final-schema installation.
    pub struct RegisteredSchemaInstallationV1 {
        connection: Connection,
    }

    /// An immediate schema-installation transaction tied to its initializing
    /// capability. The lifetime keeps the authorized installation connection
    /// alive through commit, rollback, or drop.
    pub struct RegisteredSchemaInstallationTransactionV1<'a> {
        transaction: Transaction,
        _installation: &'a RegisteredSchemaInstallationV1,
    }

    impl RegisteredSchemaInstallationV1 {
        fn from_authorized_connection(connection: Connection) -> Self {
            Self { connection }
        }

        /// The exact Store scope being initialized.
        ///
        /// Installers may use this typed identity for pre-write schema-shape
        /// validation. It is not a path, runtime, authority, or SQL handle.
        pub fn binding(&self) -> &StoreRuntimeBindingV1 {
            self.connection.binding()
        }

        pub async fn query<P>(
            &self,
            sql: &str,
            params: P,
        ) -> crate::db::engine::Result<crate::db::engine::Rows>
        where
            P: crate::db::engine::IntoParams,
        {
            self.connection.query(sql, params).await
        }

        pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
        where
            P: crate::db::engine::IntoParams,
        {
            self.connection.execute(sql, params).await
        }

        pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
            self.connection.execute_batch(sql).await
        }

        /// Begins the only ordinary write transaction available to a
        /// registered-schema installer.
        pub async fn begin_immediate(
            &self,
        ) -> crate::db::engine::Result<RegisteredSchemaInstallationTransactionV1<'_>> {
            self.connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .map(|transaction| RegisteredSchemaInstallationTransactionV1 {
                    transaction,
                    _installation: self,
                })
        }

        /// Runs one independently committed long schema batch while the
        /// underlying runtime continuously revalidates initializing authority.
        pub async fn execute_authority_revalidated_batch(
            &self,
            sql: &str,
        ) -> crate::db::engine::Result<()> {
            let transaction = self.connection.authorized_long_lease_transaction().await?;
            match transaction.execute_authority_revalidated_batch(sql).await {
                Ok(()) => transaction.commit().await,
                Err(error) => match transaction.rollback().await {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(crate::db::engine::Error::Runtime(format!(
                        "authority-revalidated schema batch failed: {error}; rollback also failed: {rollback_error}"
                    ))),
                },
            }
        }
    }

    impl QueryExecutor for RegisteredSchemaInstallationV1 {
        async fn query<P>(
            &self,
            sql: &str,
            params: P,
        ) -> crate::db::engine::Result<crate::db::engine::Rows>
        where
            P: crate::db::engine::IntoParams,
        {
            RegisteredSchemaInstallationV1::query(self, sql, params).await
        }
    }

    impl Executor for RegisteredSchemaInstallationV1 {
        async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
        where
            P: crate::db::engine::IntoParams,
        {
            RegisteredSchemaInstallationV1::execute(self, sql, params).await
        }

        async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
            RegisteredSchemaInstallationV1::execute_batch(self, sql).await
        }
    }

    impl RegisteredSchemaInstallationTransactionV1<'_> {
        pub async fn query<P>(
            &self,
            sql: &str,
            params: P,
        ) -> crate::db::engine::Result<crate::db::engine::Rows>
        where
            P: crate::db::engine::IntoParams,
        {
            self.transaction.query(sql, params).await
        }

        pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
        where
            P: crate::db::engine::IntoParams,
        {
            self.transaction.execute(sql, params).await
        }

        pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
            self.transaction.execute_batch(sql).await
        }

        pub async fn commit(self) -> crate::db::engine::Result<()> {
            self.transaction.commit().await
        }

        pub async fn rollback(self) -> crate::db::engine::Result<()> {
            self.transaction.rollback().await
        }
    }

    impl QueryExecutor for RegisteredSchemaInstallationTransactionV1<'_> {
        async fn query<P>(
            &self,
            sql: &str,
            params: P,
        ) -> crate::db::engine::Result<crate::db::engine::Rows>
        where
            P: crate::db::engine::IntoParams,
        {
            RegisteredSchemaInstallationTransactionV1::query(self, sql, params).await
        }
    }

    impl Executor for RegisteredSchemaInstallationTransactionV1<'_> {
        async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
        where
            P: crate::db::engine::IntoParams,
        {
            RegisteredSchemaInstallationTransactionV1::execute(self, sql, params).await
        }

        async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
            RegisteredSchemaInstallationTransactionV1::execute_batch(self, sql).await
        }
    }

    /// Signature of the schema installer, boxed because it is stored as a
    /// plain function pointer rather than a generic.
    pub type Installer = for<'a> fn(
        &'a RegisteredSchemaInstallationV1,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

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

    /// Adapts the Store-open path's already-authorized final-schema connection
    /// into the sealed schema-installation capability.
    ///
    /// This is crate-private so no dependent crate can fabricate an
    /// installation capability before Store publication.
    pub(crate) async fn install_from_authorized_connection(connection: Connection) -> Result<()> {
        let installation = RegisteredSchemaInstallationV1::from_authorized_connection(connection);
        ensure_registered_schema(&installation).await
    }

    /// Installs the registered global/session schema through the sealed
    /// initialization capability.
    ///
    /// # Errors
    /// Returns [`TraceDecayError::Database`] when no installer is registered,
    /// or whatever the registered installer reports.
    pub async fn ensure_registered_schema(
        installation: &RegisteredSchemaInstallationV1,
    ) -> Result<()> {
        match INSTALLER.get() {
            Some(installer) => installer(installation).await,
            None => unregistered_outcome(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn installer_signature_borrows_only_the_sealed_installation_capability() {
            fn installer<'a>(
                _: &'a RegisteredSchemaInstallationV1,
            ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
                Box::pin(async { Ok(()) })
            }

            let _: Installer = installer;
        }

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
