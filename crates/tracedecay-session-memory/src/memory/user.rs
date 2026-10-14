//! Profile-level durable memory for conversations without a code project.

use std::path::{Path, PathBuf};

use tracedecay_runtime_core::storage::USER_MEMORY_DB_FILENAME;

pub fn user_memory_db_path(profile_root: &Path) -> PathBuf {
    profile_root.join(USER_MEMORY_DB_FILENAME)
}

// The daemon registry owns opening this path because registry/profile
// lifecycle sits above the runtime kernel.
