//! Profile-level durable memory for conversations without a code project.

use std::path::{Path, PathBuf};

pub const USER_MEMORY_DB_FILENAME: &str = "user-memory.db";

pub fn user_memory_db_path(profile_root: &Path) -> PathBuf {
    profile_root.join(USER_MEMORY_DB_FILENAME)
}

// `open_user_memory_db` stayed in the root crate: it borrows the daemon
// session registry (`daemon::store_runtime::session_registry`), which sits
// above this kernel. The root `memory::user` shim re-declares it so
// `crate::memory::user::open_user_memory_db` keeps resolving.
