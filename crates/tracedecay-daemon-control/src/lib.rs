//! OS service lifecycle and readiness control for the TraceDecay daemon.
//!
//! This crate owns the generated systemd, launchd, and Windows service
//! definitions together with install, stop, restore, and authenticated
//! readiness probing. Daemon request dispatch remains in
//! `tracedecay-daemon-service`.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use tracedecay_domain::errors::{Result, TraceDecayError};

/// Canonical systemd user-unit name for the managed daemon.
pub const SERVICE_NAME: &str = "tracedecay.service";

pub use tracedecay_daemon_protocol::SOCKET_ENV;

/// Explicit network boundary for the enrolled Remote Brain protocol over TLS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteBrainTlsConfig {
    listen: SocketAddr,
    certificate_chain: PathBuf,
    private_key: PathBuf,
}

impl RemoteBrainTlsConfig {
    pub fn from_optional_parts(
        listen: Option<SocketAddr>,
        certificate_chain: Option<PathBuf>,
        private_key: Option<PathBuf>,
    ) -> Result<Option<Self>> {
        match (listen, certificate_chain, private_key) {
            (None, None, None) => Ok(None),
            (Some(listen), Some(certificate_chain), Some(private_key)) => {
                if listen.ip().is_unspecified() {
                    return Err(TraceDecayError::Config {
                        message: "Remote Brain TLS listener requires an explicit interface address; wildcard addresses are refused".to_owned(),
                    });
                }
                if certificate_chain.as_os_str().is_empty() || private_key.as_os_str().is_empty() {
                    return Err(TraceDecayError::Config {
                        message: "Remote Brain TLS certificate and private-key paths must be non-empty".to_owned(),
                    });
                }
                Ok(Some(Self {
                    listen,
                    certificate_chain,
                    private_key,
                }))
            }
            _ => Err(TraceDecayError::Config {
                message: "Remote Brain TLS listener requires --remote-listen, --remote-tls-cert, and --remote-tls-key together".to_owned(),
            }),
        }
    }

    pub fn listen(&self) -> SocketAddr {
        self.listen
    }

    pub fn certificate_chain(&self) -> &Path {
        &self.certificate_chain
    }

    pub fn private_key(&self) -> &Path {
        &self.private_key
    }
}

mod core_handshake;
mod service;

pub use core_handshake::handshake_for_current_client;
pub use service::{
    DaemonServiceSpec, DaemonServiceState, MaintenanceWindowOutcome, QuiescedDaemonLifecycle,
    daemon_reachable, default_socket_path, install_service, install_service_under_lease,
    installed_service_socket_path, installed_service_state, prepare_scoop_package_service,
    quiesce_installed_service_before_lease, refresh_installed_service_under_lease_with_state,
    restore_installed_service_after_update, restore_scoop_package_service, service_spec,
    service_spec_with_remote_tls, service_status, socket_path_or_default, start_service,
    stop_service, unavailable_daemon_socket_advice, uninstall_service,
    verify_installed_service_quiesced_under_lease, wait_for_installed_service_state,
    with_exclusive_maintenance_window, with_quiesced_installed_service,
};
