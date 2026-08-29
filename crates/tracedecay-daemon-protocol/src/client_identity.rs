use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Per-client profile identity sent in each daemon handshake.
///
/// This is not the identity of the daemon process. A single daemon socket serves
/// many clients, and each client identity scopes profile-backed state such as
/// project caches, registries, and accounting databases.
///
/// Process-state construction (`current()`) stays in the composition root: it
/// reads the user-data directory and global-db path from the running process.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DaemonClientIdentity {
    pub profile_root: PathBuf,
    pub global_db_path: PathBuf,
}

impl DaemonClientIdentity {
    pub fn new(profile_root: PathBuf, global_db_path: PathBuf) -> Self {
        Self {
            profile_root,
            global_db_path,
        }
    }
}
