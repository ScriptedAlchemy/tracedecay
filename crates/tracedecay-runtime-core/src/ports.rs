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
/// above the kernel because it depends on `db::DatabaseAuthority`. The concrete
/// handle is therefore retained opaquely: the kernel only needs it to stay
/// alive for as long as the facade does, and to answer the few identity
/// questions below.
///
/// The root crate implements this for
/// `daemon::store_runtime::registry::StoreRuntimeHandle`.
pub trait StoreRuntimeSource: std::fmt::Debug + Send + Sync + 'static {
    /// Descriptor-derived identity of the file this runtime attached to.
    fn opened_file_identity(&self) -> Option<u64>;

    /// Canonical path of the attached database.
    fn canonical_path(&self) -> std::path::PathBuf;

    /// Whether schema migrations already ran for this attachment.
    fn schema_migrated(&self) -> bool;

    /// Re-verifies that the attachment still refers to the file it opened.
    ///
    /// `operation` names the caller's intent for the error message.
    fn validate_registered_read(&self, operation: &'static str) -> Result<(), String>;
}

/// Shared handle to a daemon-owned store runtime.
pub type StoreRuntimeSourceHandle = std::sync::Arc<dyn StoreRuntimeSource>;
