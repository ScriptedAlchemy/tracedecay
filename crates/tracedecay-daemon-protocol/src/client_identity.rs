use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracedecay_runtime_core::config::{global_db_path, user_data_dir};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

/// Per-client profile identity sent in each daemon handshake.
///
/// This is not the identity of the daemon process. A single daemon socket serves
/// many clients, and each client identity scopes profile-backed state such as
/// project caches, registries, and accounting databases.
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

/// Process-state factory for the handshake identity contract.
///
/// Construction reads this process's user-data directory and global-db path
/// from [`tracedecay_runtime_core::config`]. The global-db path helper lives
/// there (same `TRACEDECAY_GLOBAL_DB` / `user_data_dir()/global.db` formula
/// previously owned by `tracedecay-global-db`) so this crate does not take a
/// global-db dependency.
pub fn current_daemon_client_identity() -> Result<DaemonClientIdentity> {
    let profile_root = user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let global_db_path = global_db_path().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay global database path".to_string(),
    })?;
    Ok(DaemonClientIdentity::new(profile_root, global_db_path))
}
