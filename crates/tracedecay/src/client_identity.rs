use tracedecay_daemon_protocol::DaemonClientIdentity;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

/// Process-state factory for the handshake identity contract.
///
/// The identity type lives in `tracedecay-daemon-protocol`. Construction reads
/// this process's user-data directory and global-db path.
pub fn current_daemon_client_identity() -> Result<DaemonClientIdentity> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let global_db_path =
        tracedecay_global_db::global_db_path().ok_or_else(|| TraceDecayError::Config {
            message: "could not determine TraceDecay global database path".to_string(),
        })?;
    Ok(DaemonClientIdentity::new(profile_root, global_db_path))
}
