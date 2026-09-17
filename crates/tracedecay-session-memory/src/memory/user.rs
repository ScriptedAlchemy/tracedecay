//! Profile-level durable memory for conversations without a code project.

use std::path::{Path, PathBuf};

pub(crate) const USER_MEMORY_DB_FILENAME: &str = "user-memory.db";

pub fn user_memory_db_path(profile_root: &Path) -> PathBuf {
    profile_root.join(USER_MEMORY_DB_FILENAME)
}

// The daemon registry owns opening this path because registry/profile
// lifecycle sits above the runtime kernel.
