//! Process-state factories for the daemon handshake wire contract.
//!
//! The handshake type lives in `tracedecay-daemon-protocol`. Construction that
//! reads this process's identity, build version, and run id stays here.

use std::path::PathBuf;

use tracedecay_daemon_protocol::{DaemonHandshake, MovedStoreAdoption};
use tracedecay_runtime_core::errors::Result;

use crate::client_identity::current_daemon_client_identity;

pub use tracedecay_daemon_protocol::{client_version_skew, version_skew_action};

/// Handshake for this process's current client identity and binary version.
pub fn handshake_for_current_client(
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
        client_version: binary_version().to_string(),
        client_instance_id: tracedecay_runtime_core::runtime_identity::process_run_id().to_string(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: MovedStoreAdoption::Never,
    })
}

pub fn handshake_open_options(
    handshake: &DaemonHandshake,
) -> crate::tracedecay::TraceDecayOpenOptions {
    crate::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(handshake.client_identity.profile_root.clone()),
        global_db_path: Some(handshake.client_identity.global_db_path.clone()),
    }
}

/// Version of this tracedecay binary, advertised in daemon handshakes and
/// compared against peers to detect stale daemons after `tracedecay update`.
///
/// This is the build version, not the released one: two checkout builds of
/// the same release differ only by commit, and a daemon left running from the
/// previous build is exactly the skew this comparison exists to catch.
pub(crate) fn binary_version() -> &'static str {
    crate::version::build_version()
}
