use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};

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
    pub fn current() -> Result<Self> {
        let profile_root =
            crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
                message: "could not determine TraceDecay user data directory".to_string(),
            })?;
        let global_db_path =
            crate::global_db::global_db_path().ok_or_else(|| TraceDecayError::Config {
                message: "could not determine TraceDecay global database path".to_string(),
            })?;
        Ok(Self {
            profile_root,
            global_db_path,
        })
    }
}
