//! `tracedecay_remote_status`, Remote Brain operational-plane read.

use tracedecay_contracts::remote::status::RemoteOperationalStatusReadV1;
use tracedecay_contracts::remote::status::RemoteOperationalStatusReaderV1;

/// Reads the daemon-mounted Remote Brain operational plane.
///
/// Absence of the provider is the typed unmounted-authority outcome
/// [`RemoteOperationalStatusReadV1::Unavailable`], never an empty success.
#[tracing::instrument(name = "mcp.info.remote_status.total", level = "trace", skip_all)]
pub fn read_remote_status(
    provider: Option<&RemoteOperationalStatusReaderV1>,
) -> RemoteOperationalStatusReadV1 {
    match provider {
        Some(provider) => provider(),
        None => RemoteOperationalStatusReadV1::Unavailable,
    }
}
