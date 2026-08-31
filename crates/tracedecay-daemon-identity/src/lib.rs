//! Daemon identity and credential discovery for TraceDecay.
//!
//! Owns the profile-scoped daemon authority record (single-daemon election
//! under a kernel file lock, epoch advance with compare-and-swap freshness
//! checks, 0o600/0o700 on-disk posture, and a fresh authentication token per
//! acquire), the durable local profile identity, and client-side discovery of
//! the current daemon connection from those records.
//!
//! `tracedecay-daemon-protocol` stays the wire leaf below this crate: it never
//! reads authority records. This crate supplies the protocol crate's
//! sanctioned `DaemonLivenessProbe` inversion so transport reads can observe a
//! superseded authority without the wire crate learning discovery.

pub mod authority;
mod connection;
pub mod profile_identity;

pub use connection::{
    DaemonConnection, client_connection, current_daemon_connection, invocation_client_for_current,
};

#[cfg(unix)]
pub use connection::connection_for_socket_path;
