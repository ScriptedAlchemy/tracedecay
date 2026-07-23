//! Daemon-owned libsql local openers for profile/project/session compatibility.
//!
//! Concrete `Builder::new_local` calls for non-graph stores live here so
//! production callers cannot open a second authority. S11 removes this module
//! once every live handle originates from the rusqlite runtime registry.
//!
//! # Allowlist — who may call into this module, and why
//!
//! This is the only non-graph `Builder::new_local` seam. Until S11 lands, the
//! following callers are explicitly permitted; nothing else may open owned
//! stores directly (the `tests/storage_runtime_open_boundary.rs` gate enforces
//! this against `tests/fixtures/storage_runtime/direct_open_allowlist.json`):
//!
//! - **Owned-store seam (pending S11):** `crate::global_db::GlobalDb::open_local`
//!   is the single physical open for owned global/profile/session stores. The
//!   `repair_session_temporal_store` path is the one intentional bypass — it
//!   needs a bare pre-schema connection over a possibly-damaged store (see the
//!   `// S11:` note there).
//! - **Snapshot / backup-staging reads (S5-retained):**
//!   `crate::sqlite_read_snapshot` opens frozen, read-only immutable copies for
//!   side-effect-free logical inspection; it never opens live authority.
//! - **Offline/cold doctor (pending S11):** `crate::daemon::core_doctor` cold
//!   paths inspect a store by path when no live daemon (and therefore no
//!   `StoreRuntimeHandle`) is reachable (see the `// S11:` notes there).
//! - **Foreign host databases (permanent):** Hermes state (`sessions::hermes`)
//!   and Cursor (`sessions::cursor_composer`) read databases `TraceDecay` does not
//!   own; these are never routed through the runtime registry.
//! - **Backup verification (permanent):** `sessions::lcm::maintenance` opens
//!   staged backup copies to verify them.
//! - **Migration inspection (temporary):** `crate::migrate` opens legacy stores
//!   for one-shot inspection/consolidation.
//! - **Isolated test fixtures:** `#[cfg(test)]` fixtures across the crate.

use std::path::Path;

use libsql::{Builder, Database, OpenFlags};

/// Opens a local libsql database for an already-authorized profile/project/
/// session path. Callers must hold [`crate::db::DatabaseAuthority`] separately.
pub(crate) async fn open_local_database(
    db_path: &Path,
    read_only: bool,
) -> Result<Database, libsql::Error> {
    let builder = if read_only {
        Builder::new_local(db_path).flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
    } else {
        Builder::new_local(db_path)
    };
    builder.build().await
}

/// Opens a local libsql database with caller-supplied flags (URI/immutable
/// snapshots and other specialized read paths).
pub(crate) async fn open_local_database_with_flags(
    db_path_or_uri: impl AsRef<Path>,
    flags: OpenFlags,
) -> Result<Database, libsql::Error> {
    Builder::new_local(db_path_or_uri.as_ref())
        .flags(flags)
        .build()
        .await
}
