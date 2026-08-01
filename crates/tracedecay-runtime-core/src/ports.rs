//! Injection points for subsystems that stay above the kernel.
//!
//! The one-shot crate split moved the runtime kernel down but left four
//! collaborators above it: the daemon store-runtime registry, the registered
//! global database, the daemon session registry, and branch-admin recovery.
//! Each is expressed here as a port the root crate registers into, so the
//! kernel never names an upward module path.
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

/// Source of daemon-owned physical store runtimes.
///
/// `db::connection` publishes a `Database` facade over a runtime the daemon
/// registry already opened; the registry itself (`daemon::store_runtime`) sits
/// above the kernel because it depends on `db::DatabaseAuthority`, which is a
/// kernel type. The concrete handle is therefore retained opaquely: the kernel
/// keeps it alive for as long as the facade lives and asks it only the
/// questions below.
///
/// The root crate implements this for
/// `daemon::store_runtime::registry::StoreRuntimeHandle`. Registry-side
/// failures are surfaced as strings because every kernel call site only
/// `Debug`-formats them into a `TraceDecayError::Database`.
pub trait StoreRuntimeSource: std::fmt::Debug + Send + Sync + 'static {
    /// Descriptor-derived identity of the file this runtime attached to.
    fn opened_file_identity(&self) -> Option<u64>;

    /// Canonical path of the attached database (`locator().path()`).
    fn canonical_path(&self) -> &std::path::Path;

    /// Verified locator this attachment was published against
    /// (`locator().verified()`).
    fn verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1;

    /// Typed shard binding this attachment serves.
    fn binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1;

    /// Whether schema migrations already ran for this attachment.
    fn schema_migrated(&self) -> bool;

    /// Whether the physical attachment currently holds a writer
    /// (`physical_snapshot().writer_present`).
    fn writer_present(&self) -> bool;

    /// Re-verifies that the attachment still refers to the file it opened.
    ///
    /// `operation` names the caller's intent for the error message.
    fn validate_registered_read(&self, operation: &'static str) -> Result<(), String>;

    /// Read-only SQL handle for the retained attachment.
    fn telemetry_read_handle(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle, String>;

    /// Write-authorized SQL handle for the retained attachment.
    fn authorized_migration_sql_handle(
        &self,
        authority: crate::db::DatabaseAuthority,
    ) -> Result<tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle, String>;

    /// Originating database authority retained when this runtime was
    /// published writable.
    fn database_authority(
        &self,
        operation: &'static str,
    ) -> Result<crate::db::DatabaseAuthority, String>;

    /// Samples `(page_count, freelist_count, page_size)` for store-size
    /// telemetry, waiting at most `reader_wait` for a reader slot.
    fn storage_page_counts(
        &self,
        reader_wait: std::time::Duration,
    ) -> Result<(u64, u64, u64), String>;

    /// Runs a bounded incremental vacuum through the canonical writer lane.
    fn run_bounded_incremental_compaction<'a>(
        &'a self,
        max_pages: u64,
        authority: crate::db::DatabaseAuthority,
    ) -> StoreRuntimeFuture<'a, Result<(), String>>;

    /// Runs a WAL checkpoint through the canonical writer lane.
    fn run_checkpoint<'a>(
        &'a self,
        request: tracedecay_rusqlite_runtime::CheckpointRequest,
        authority: crate::db::DatabaseAuthority,
    ) -> StoreRuntimeFuture<'a, Result<tracedecay_rusqlite_runtime::CheckpointOutcome, String>>;

    /// Copies one transactionally consistent snapshot to `destination`.
    fn snapshot_to<'a>(
        &'a self,
        destination: std::path::PathBuf,
        authority: crate::db::DatabaseAuthority,
    ) -> StoreRuntimeFuture<'a, Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt, String>>;

    /// Submits an authorized typed runtime write.
    fn dispatch_submit_authorized<'a>(
        &'a self,
        request: tracedecay_store::RuntimeSubmitRequestV1,
        probe: std::sync::Arc<dyn tracedecay_store::RuntimeRequestProbeV1>,
        authority: crate::db::DatabaseAuthority,
    ) -> StoreRuntimeFuture<'a, Result<tracedecay_store::RuntimeSubmitOutcomeV1, String>>;

    /// Dispatches a typed runtime read.
    fn dispatch_read(
        &self,
        request: tracedecay_store::RuntimeReadRequestV1,
        probe: &dyn tracedecay_store::RuntimeRequestProbeV1,
    ) -> Result<tracedecay_store::RuntimeReadOutcomeV1, String>;

    /// Stable identity of the underlying physical runtime, so two facades can
    /// be compared for attachment sharing without exposing the runtime type.
    fn runtime_identity(&self) -> usize;
}

/// Boxed future returned by the asynchronous [`StoreRuntimeSource`] methods.
/// The port is a trait object, so it cannot use `async fn` directly.
pub type StoreRuntimeFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Shared handle to a daemon-owned store runtime.
pub type StoreRuntimeSourceHandle = std::sync::Arc<dyn StoreRuntimeSource>;
