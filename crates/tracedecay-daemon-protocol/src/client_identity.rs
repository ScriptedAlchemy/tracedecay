use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Per-client profile identity sent in each daemon handshake.
///
/// This is not the identity of the daemon process. A single daemon socket serves
/// many clients, and each client identity scopes profile-backed state such as
/// project caches, registries, and accounting databases.
///
/// Process-state construction that reads the runtime-core config path helpers
/// lives above this crate so the wire contract does not depend on the runtime
/// kernel.
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
