//! Per-connection daemon handshake metadata and version-skew detection.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{DaemonClientIdentity, Result};

/// Per-connection metadata sent before JSON-RPC traffic.
///
/// The daemon process is shared. This handshake tells that shared process which
/// project, scope, timing preference, and client profile should apply to this
/// connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHandshake {
    pub project_path: Option<PathBuf>,
    pub scope_prefix: Option<String>,
    pub timings: bool,
    pub allow_init: bool,
    #[serde(default)]
    pub allow_initialize_root_routing: bool,
    pub client_identity: DaemonClientIdentity,
    /// Version of the tracedecay binary that opened this connection.
    ///
    /// `#[serde(default)]` keeps mixed-version pairs interoperable: a new
    /// daemon reads handshakes from old clients (missing field → empty), and
    /// old daemons ignore the extra field. The daemon uses it to detect and
    /// log version skew, e.g. a stale daemon still serving after
    /// `tracedecay update` replaced the binary.
    #[serde(default)]
    pub client_version: String,
    /// Stable id for the connecting client process. A stdio MCP proxy reuses
    /// this across its per-request daemon connections, allowing one
    /// generation-local catalog refresh notification instead of one per
    /// request. Old clients omit it and deserialize to an empty string.
    #[serde(default)]
    pub client_instance_id: String,
    /// Whether this proxy already forwarded an initialize response declaring
    /// `tools.listChanged=true` to its MCP host.
    #[serde(default)]
    pub tool_list_changed_capable: bool,
    /// Daemon version whose initialize response established the host's
    /// current tool catalog. A nonempty value proves explicit negotiation;
    /// generation-local daemon state decides whether a refresh is due.
    #[serde(default)]
    pub catalog_version: String,
}

impl DaemonHandshake {
    pub fn for_current_client(
        project_path: Option<PathBuf>,
        scope_prefix: Option<String>,
        timings: bool,
        allow_init: bool,
    ) -> Result<Self> {
        Ok(Self {
            project_path,
            scope_prefix,
            timings,
            allow_init,
            allow_initialize_root_routing: false,
            client_identity: DaemonClientIdentity::current()?,
            client_version: binary_version().to_string(),
            client_instance_id: crate::runtime_identity::process_run_id().to_string(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
        })
    }

    pub(crate) fn open_options(&self) -> crate::tracedecay::TraceDecayOpenOptions {
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(self.client_identity.profile_root.clone()),
            global_db_path: Some(self.client_identity.global_db_path.clone()),
        }
    }

    pub fn to_line(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_line(line: &str) -> Result<Self> {
        Ok(serde_json::from_str(line.trim())?)
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

/// The client version to report as skewed, or `None` when the versions match.
///
/// Old clients send no version (empty string); that is indistinguishable from
/// "same version before this field existed", so it never counts as skew.
#[cfg(unix)]
pub(crate) fn client_version_skew(client_version: &str, daemon_version: &str) -> Option<String> {
    if client_version.is_empty() || client_version == daemon_version {
        return None;
    }
    Some(client_version.to_string())
}

#[cfg(unix)]
fn release_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version
        .strip_prefix('v')
        .unwrap_or(version)
        .split(['-', '+'])
        .next()?;
    let mut parts = core.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

#[cfg(unix)]
pub(crate) fn version_skew_action(daemon_version: &str, client_version: &str) -> &'static str {
    match release_version(daemon_version)
        .zip(release_version(client_version))
        .map(|(daemon, client)| daemon.cmp(&client))
    {
        Some(std::cmp::Ordering::Greater) => {
            "restart or reconnect the MCP host so it loads the current TraceDecay client and tool catalog"
        }
        Some(std::cmp::Ordering::Less) => {
            "run `tracedecay daemon restart` to load the current daemon binary"
        }
        _ => "restart or reconnect whichever TraceDecay component is stale",
    }
}
