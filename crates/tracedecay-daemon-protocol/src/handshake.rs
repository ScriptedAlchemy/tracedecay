//! Per-connection daemon handshake metadata and version-skew detection.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::client_identity::DaemonClientIdentity;
use tracedecay_domain::errors::Result;

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

/// Stable discriminator for the pre-handshake refusal frame.
pub const DAEMON_HANDSHAKE_REFUSAL_PROTOCOL: &str = "tracedecay.daemon.handshake-refusal";

/// Why a daemon refused a connection before serving any request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonHandshakeRefusalReason {
    /// The handshake line was valid JSON but not this daemon's handshake
    /// shape — the signature of wire drift between build revisions.
    UnsupportedRevision,
    /// The handshake line was not even JSON.
    InvalidHandshake,
    /// The client's auth preface was missing, unparseable, or carried a token
    /// this daemon did not mint. Closing without this frame made an auth
    /// rejection indistinguishable from a transport failure ("connection
    /// closed, outcome unknown") on the client.
    AuthenticationRejected,
}

/// One JSON line the daemon writes before closing a connection whose
/// handshake it cannot serve.
///
/// Without this frame the client's pending read ends in a raw
/// `Connection reset by peer`, which hides version skew behind a transport
/// error. Old daemons never send it; new clients treat an unparseable
/// response line that parses as this frame as a typed refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHandshakeRefusal {
    pub protocol: String,
    pub refusal: DaemonHandshakeRefusalReason,
    pub daemon_version: String,
}

impl DaemonHandshakeRefusal {
    /// Classify one unparseable handshake line into its refusal frame.
    pub fn for_unparseable_handshake(line: &str, daemon_version: &str) -> Self {
        let refusal = if serde_json::from_str::<serde_json::Value>(line.trim()).is_ok() {
            DaemonHandshakeRefusalReason::UnsupportedRevision
        } else {
            DaemonHandshakeRefusalReason::InvalidHandshake
        };
        Self {
            protocol: DAEMON_HANDSHAKE_REFUSAL_PROTOCOL.to_owned(),
            refusal,
            daemon_version: daemon_version.to_owned(),
        }
    }

    /// The refusal frame for a client whose auth preface this daemon rejected.
    ///
    /// The daemon answers with this one frame and then closes; it never echoes
    /// the supplied token or names the expected one.
    pub fn for_rejected_authentication(daemon_version: &str) -> Self {
        Self {
            protocol: DAEMON_HANDSHAKE_REFUSAL_PROTOCOL.to_owned(),
            refusal: DaemonHandshakeRefusalReason::AuthenticationRejected,
            daemon_version: daemon_version.to_owned(),
        }
    }

    pub fn to_line(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Parse a refusal frame; `None` when the line is any other shape.
    pub fn from_line(line: &str) -> Option<Self> {
        let refusal = serde_json::from_str::<Self>(line.trim()).ok()?;
        (refusal.protocol == DAEMON_HANDSHAKE_REFUSAL_PROTOCOL).then_some(refusal)
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

fn release_version(version: &str) -> Option<semver::Version> {
    semver::Version::parse(version.strip_prefix('v').unwrap_or(version)).ok()
}

pub fn version_skew_action(daemon_version: &str, client_version: &str) -> &'static str {
    // Precedence ordering (semver: prerelease identifiers ordered, build
    // metadata ignored) so a `0.1.0-beta.36` daemon under a `0.1.0-beta.37`
    // client is correctly named the stale side.
    match release_version(daemon_version)
        .zip(release_version(client_version))
        .map(|(daemon, client)| daemon.cmp_precedence(&client))
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

#[cfg(test)]
mod handshake_refusal_tests {
    use super::*;

    #[test]
    fn version_skew_action_orders_prerelease_builds() {
        assert_eq!(
            version_skew_action("0.1.0-beta.36+aaaa", "0.1.0-beta.37+bbbb"),
            "run `tracedecay daemon restart` to load the current daemon binary"
        );
        assert_eq!(
            version_skew_action("0.1.0-beta.37+aaaa", "0.1.0-beta.36+bbbb"),
            "restart or reconnect the MCP host so it loads the current TraceDecay client and tool catalog"
        );
        assert_eq!(
            version_skew_action("0.1.0-beta.37+aaaa", "0.1.0-beta.37+bbbb"),
            "restart or reconnect whichever TraceDecay component is stale"
        );
    }

    #[test]
    fn wire_drifted_handshakes_classify_as_unsupported_revision() {
        let refusal = DaemonHandshakeRefusal::for_unparseable_handshake(
            r#"{"future_handshake_shape": true}"#,
            "0.1.0-beta.99+cafe",
        );
        assert_eq!(
            refusal.refusal,
            DaemonHandshakeRefusalReason::UnsupportedRevision
        );
        assert_eq!(refusal.daemon_version, "0.1.0-beta.99+cafe");
        let line = refusal.to_line().expect("refusal wire line");
        assert_eq!(
            DaemonHandshakeRefusal::from_line(&line),
            Some(refusal),
            "the refusal frame must round-trip through its one wire line"
        );
    }

    #[test]
    fn non_json_handshakes_classify_as_invalid_handshake() {
        let refusal =
            DaemonHandshakeRefusal::for_unparseable_handshake("GET / HTTP/1.1", "0.1.0-beta.99");
        assert_eq!(
            refusal.refusal,
            DaemonHandshakeRefusalReason::InvalidHandshake
        );
    }

    #[test]
    fn rejected_authentication_round_trips_and_names_no_token() {
        let refusal = DaemonHandshakeRefusal::for_rejected_authentication("0.1.0-beta.99+cafe");
        assert_eq!(
            refusal.refusal,
            DaemonHandshakeRefusalReason::AuthenticationRejected
        );
        assert_eq!(refusal.daemon_version, "0.1.0-beta.99+cafe");
        let line = refusal.to_line().expect("refusal wire line");
        assert!(
            line.contains("authentication_rejected"),
            "the frame must carry the snake_case reason: {line}"
        );
        assert_eq!(
            DaemonHandshakeRefusal::from_line(&line),
            Some(refusal),
            "the auth refusal frame must round-trip through its one wire line"
        );
    }

    #[test]
    fn foreign_lines_never_parse_as_refusal_frames() {
        assert_eq!(DaemonHandshakeRefusal::from_line("{}"), None);
        assert_eq!(
            DaemonHandshakeRefusal::from_line(
                r#"{"protocol":"tracedecay.daemon.invocation","revision":1}"#
            ),
            None
        );
        assert_eq!(DaemonHandshakeRefusal::from_line("not json"), None);
    }
}
