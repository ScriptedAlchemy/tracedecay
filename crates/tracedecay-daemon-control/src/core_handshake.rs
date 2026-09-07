//! Portable process-state factory for the daemon handshake wire contract.

use std::path::PathBuf;

use tracedecay_daemon_protocol::{DaemonClientIdentity, DaemonHandshake, MovedStoreAdoption};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::config::{global_db_path, user_data_dir};

fn current_daemon_client_identity() -> Result<DaemonClientIdentity> {
    let profile_root = user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let global_db_path = global_db_path().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay global database path".to_string(),
    })?;
    Ok(DaemonClientIdentity::new(profile_root, global_db_path))
}

/// Handshake for this process's current client identity and caller-supplied
/// binary version.
pub fn handshake_for_current_client(
    client_version: &str,
    project_path: Option<PathBuf>,
    scope_prefix: Option<String>,
    timings: bool,
    allow_init: bool,
) -> Result<DaemonHandshake> {
    Ok(DaemonHandshake {
        project_path,
        scope_prefix,
        timings,
        allow_init,
        allow_initialize_root_routing: false,
        client_identity: current_daemon_client_identity()?,
        client_version: client_version.to_owned(),
        client_instance_id: tracedecay_runtime_core::runtime_identity::process_run_id().to_string(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: MovedStoreAdoption::Never,
    })
}
