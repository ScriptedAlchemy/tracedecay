//! Per-connection daemon handshake metadata and version-skew detection.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::client_identity::DaemonClientIdentity;
use tracedecay_runtime_core::errors::Result;

/// How first-touch resolution treats a moved non-git store whose registry row
/// no longer resolves at its recorded root.
///
/// Old daemons ignore the handshake field carrying this value and new daemons
/// default a missing field to [`Self::Never`], so mixed-version pairs always
/// degrade to the safe no-adoption behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovedStoreAdoption {
    /// Mint a fresh identity without consulting moved-store candidates.
    #[default]
    Never,
    /// Explicit `tracedecay init` without adoption flags: mint fresh when no
    /// moved store could claim this root; otherwise refuse with the candidates.
    OfferCandidates,
    /// `tracedecay init --yes`: adopt when candidates identify exactly one
    /// moved store; anything ambiguous remains a typed refusal.
    AdoptUnique,
    /// `tracedecay init --adopt-project <id>`: adopt exactly this project.
    AdoptNamed(String),
}

/// Per-connection metadata sent before JSON-RPC traffic.
///
/// The daemon process is shared. This handshake tells that shared process which
/// project, scope, timing preference, and client profile should apply to this
/// connection.
///
/// Factories that read process state (`for_current_client`) stay in the
/// composition root.
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
    #[serde(default)]
    pub client_version: String,
    /// Stable id for the connecting client process.
    #[serde(default)]
    pub client_instance_id: String,
    #[serde(default)]
    pub tool_list_changed_capable: bool,
    #[serde(default)]
    pub catalog_version: String,
    #[serde(default)]
    pub moved_store_adoption: MovedStoreAdoption,
}

impl DaemonHandshake {
    pub fn to_line(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_line(line: &str) -> Result<Self> {
        Ok(serde_json::from_str(line.trim())?)
    }
}

/// The client version to report as skewed, or `None` when the versions match.
///
/// Old clients send no version (empty string); that is indistinguishable from
/// "same version before this field existed", so it never counts as skew.
pub fn client_version_skew(client_version: &str, daemon_version: &str) -> Option<String> {
    if client_version.is_empty() || client_version == daemon_version {
        return None;
    }
    Some(client_version.to_string())
}

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

pub fn version_skew_action(daemon_version: &str, client_version: &str) -> &'static str {
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
