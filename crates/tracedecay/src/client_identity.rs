//! Compatibility shim. Root `lib.rs` still declares `pub mod client_identity`
//! (owned by the orchestrator). The factory lives in
//! `tracedecay-daemon-protocol`.

pub use tracedecay_daemon_protocol::current_daemon_client_identity;
