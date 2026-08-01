//! Profile-root filenames the store-runtime resolver has to know.
//!
//! `memory::user::USER_MEMORY_DB_FILENAME` already lives in this kernel, but
//! the matching session filename is owned by `tracedecay-sessions`, which the
//! kernel cannot depend on: `tracedecay-global-db` already depends on
//! `tracedecay-migrate`, which depends on this crate, so any edge back up is a
//! Cargo cycle. The canonical value is therefore restated here.
//!
//! The root crate — which sees both sides — pins the two definitions together
//! in `src/daemon/store_runtime.rs`, so a divergence fails the root test suite
//! rather than silently resolving sessions to the wrong file.

use std::path::{Path, PathBuf};

/// Filename of the profile-scoped user session database.
///
/// Must stay equal to `tracedecay_sessions::runtime::USER_SESSIONS_DB_FILENAME`.
pub const USER_SESSIONS_DB_FILENAME: &str = "user-sessions.db";

/// Resolves the profile-scoped user session database inside `profile_root`.
#[must_use]
pub fn user_sessions_db_path(profile_root: &Path) -> PathBuf {
    profile_root.join(USER_SESSIONS_DB_FILENAME)
}
