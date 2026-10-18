//! Process-state factories for the daemon handshake wire contract.
//!
//! The handshake type lives in `tracedecay-daemon-protocol`. Construction that
//! reads portable process identity lives in `tracedecay-daemon-control`; this
//! root adapter supplies the registered product runtime's binary version and
//! root open type.

use std::path::PathBuf;

use tracedecay_daemon_protocol::DaemonHandshake;
use tracedecay_domain::errors::Result;

/// Handshake for `profile`'s client identity and this binary's version.
pub fn handshake_for_current_client(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: Option<PathBuf>,
    scope_prefix: Option<String>,
    timings: bool,
    allow_init: bool,
) -> Result<DaemonHandshake> {
    tracedecay_daemon_control::handshake_for_current_client(
        profile,
        binary_version()?,
        project_path,
        scope_prefix,
        timings,
        allow_init,
    )
}

pub fn handshake_open_options(
    handshake: &DaemonHandshake,
) -> tracedecay_project::project::TraceDecayOpenOptions {
    tracedecay_project::project::TraceDecayOpenOptions {
        profile_root: Some(handshake.client_identity.profile_root.clone()),
        global_db_path: Some(handshake.client_identity.global_db_path.clone()),
    }
}

/// Version of this tracedecay binary, advertised in daemon handshakes and
/// compared against peers to detect stale daemons after `tracedecay update`.
///
/// This is the build version, not the released one: two checkout builds of
/// the same release differ only by commit, and a daemon left running from the
/// previous build is exactly the skew this comparison exists to catch. It is
/// fallible because it reads the registered product runtime: a process whose
/// entry point never registered one has no truthful version to advertise.
pub(crate) fn binary_version()
-> std::result::Result<&'static str, tracedecay_project::product_runtime::ProductRuntimeError> {
    tracedecay_project::version::build_version()
}
