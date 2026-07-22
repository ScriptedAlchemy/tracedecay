//! Daemon-owned libsql local openers for profile/project/session compatibility.
//!
//! Concrete `Builder::new_local` calls for non-graph stores live here so
//! production callers cannot open a second authority. S11 removes this module
//! once every live handle originates from the rusqlite runtime registry.

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
