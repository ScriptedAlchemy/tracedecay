//! Ports for the few composition-root capabilities this crate reaches for but
//! must not own.
//!
//! Same shape as `tracedecay_sessions::host_ports` and
//! `tracedecay_runtime_core::ports`: a process-global slot the composition
//! root fills once during startup.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use crate::RegisteredGlobalDb;

/// Opens the profile-sessions store the daemon publishes.
///
/// Mounting a registered profile-sessions database means resolving the profile
/// identity authority and driving the daemon's session runtime registry
/// (`src/daemon/{profile_identity, store_runtime::session_registry}`). Both are
/// daemon lifecycle, they sit *above* this crate, and the registry itself
/// consumes [`RegisteredGlobalDb`] — so this crate hands out the mount and the
/// root supplies the runtime behind it.
///
/// Only the test harnesses in this crate consume the port today; production
/// callers already hold a registry. There is deliberately **no** default: an
/// unwired process gets `None` rather than a half-real store.
///
/// Root wiring needed: call [`register`] with an opener that performs
/// `profile_identity::load_or_create` followed by
/// `DaemonSessionRuntimeRegistryV1::open`, returning a handle whose
/// [`ProfileSessionsRuntime::mount`] calls `profile_sessions()`.
pub mod profile_sessions {
    use super::{Arc, Future, OnceLock, PathBuf, Pin, RegisteredGlobalDb};
    use tracedecay_runtime_core::errors::Result;

    /// A live profile-sessions runtime owned by the composition root.
    ///
    /// Holding the value keeps the runtime registered; dropping it releases
    /// the registry exactly as the daemon does.
    pub trait ProfileSessionsRuntime: Send + Sync {
        /// Mounts (or re-mounts) the registered profile-sessions database.
        fn mount(&self) -> MountFuture<'_>;
    }

    /// Result of [`ProfileSessionsRuntime::mount`].
    pub type MountFuture<'a> =
        Pin<Box<dyn Future<Output = Result<Arc<RegisteredGlobalDb>>> + Send + 'a>>;

    /// Result of an [`Opener`] call.
    pub type OpenFuture =
        Pin<Box<dyn Future<Output = Result<Box<dyn ProfileSessionsRuntime>>> + Send>>;

    /// Opener installed by the composition root, keyed by profile root.
    pub type Opener = fn(PathBuf) -> OpenFuture;

    static OPENER: OnceLock<Opener> = OnceLock::new();

    /// Installs the daemon-backed opener. First call wins.
    pub fn register(opener: Opener) {
        let _ = OPENER.set(opener);
    }

    /// Opens the profile-sessions runtime, or `None` when unwired.
    #[must_use]
    pub fn open(profile_root: PathBuf) -> Option<OpenFuture> {
        OPENER.get().map(|opener| opener(profile_root))
    }
}
