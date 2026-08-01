//! Ports for the few host-side capabilities the session runtime reaches for
//! but must not own.
//!
//! Three things the transcript runtime needs live above this crate: the agent
//! host installation layout (`tracedecay-agent-hosts`), the hook process
//! spawner (root `src/hooks/`), and the user profile configuration loader
//! (root `src/user_config.rs`). Depending on any of them from here would point
//! the session layer back at the composition root.
//!
//! Each capability is therefore a process-global slot the composition root
//! fills once during startup, in the same shape as
//! `tracedecay_runtime_core::ports::branch_admin_recovery`. Every slot has a
//! conservative default so an unwired process still runs — it just does less.
//!
//! Root wiring needed: `tracedecay::sessions` install must call
//! [`hermes_profile_pin::register`], [`session_review::register`], and
//! [`lcm_redaction::register`] before any transcript ingest runs.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Reads the pinned TraceDecay project root out of a Hermes profile config.
///
/// The parser is a host-bundle concern (`tracedecay-agent-hosts`); only the
/// answer matters here. An unregistered slot reports "no pin", which makes
/// legacy Hermes state stores skip rather than mis-attribute.
pub mod hermes_profile_pin {
    use super::{OnceLock, Path};

    /// Resolver installed by the composition root.
    pub type Resolver = fn(&Path) -> Option<String>;

    static RESOLVER: OnceLock<Resolver> = OnceLock::new();

    /// Installs the host-bundle resolver. First call wins.
    pub fn register(resolver: Resolver) {
        let _ = RESOLVER.set(resolver);
    }

    /// Reads the pinned project root, or `None` when unwired.
    pub fn resolve(config_path: &Path) -> Option<String> {
        RESOLVER.get().and_then(|resolver| resolver(config_path))
    }
}

/// Schedules the post-ingest user session review.
///
/// The review runs as a reaped hook child process, which is a root concern:
/// this crate must not know the binary path or the hook argv.
pub mod session_review {
    use super::OnceLock;

    /// Scheduler installed by the composition root.
    pub type Scheduler = fn(&str, Option<&str>);

    static SCHEDULER: OnceLock<Scheduler> = OnceLock::new();

    /// Installs the hook scheduler. First call wins.
    pub fn register(scheduler: Scheduler) {
        let _ = SCHEDULER.set(scheduler);
    }

    /// Requests a review pass; a no-op when unwired.
    pub fn schedule(provider: &str, session_id: Option<&str>) {
        if let Some(scheduler) = SCHEDULER.get() {
            scheduler(provider, session_id);
        }
    }
}

/// Owner-configured sensitive-value redaction policy for LCM raw ingest.
///
/// Redaction is irreversible, so the unwired default is "off with no
/// patterns" — exactly what the root `UserConfig` default produces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LcmRedactionPolicy {
    /// Whether owner-opted-in sensitive-value redaction is active.
    pub enabled: bool,
    /// Lowercased sensitive key patterns; empty means "use the built-ins".
    pub patterns: Vec<String>,
}

/// Supplies [`LcmRedactionPolicy`] from the user profile configuration.
pub mod lcm_redaction {
    use super::{LcmRedactionPolicy, OnceLock};

    /// Policy provider installed by the composition root.
    pub type Provider = fn() -> LcmRedactionPolicy;

    static PROVIDER: OnceLock<Provider> = OnceLock::new();

    /// Installs the profile-backed provider. First call wins.
    pub fn register(provider: Provider) {
        let _ = PROVIDER.set(provider);
    }

    /// Resolves the active policy, defaulting to "redaction off".
    pub fn resolve() -> LcmRedactionPolicy {
        PROVIDER
            .get()
            .map_or_else(LcmRedactionPolicy::default, |provider| provider())
    }
}

/// Builds an *unregistered* admission facade — one with no durable authority
/// behind it, which fails closed on every capture.
///
/// Standalone callers (a CLI invocation with no daemon-owned registry mount)
/// still need an admission handle to walk a transcript and count what it
/// *would* admit. Only the composition root can build one, so it installs the
/// factory here.
pub mod unregistered_admission {
    use super::OnceLock;
    use crate::admission::HostAdmission;
    use tracedecay_domain::ProjectId;

    /// Scope an unregistered facade is nominally bound to.
    #[derive(Clone, Debug)]
    pub enum Scope {
        /// Bound to one project identity, without a registered database.
        Project(ProjectId),
        /// Bound to the profile, without a registered database.
        Profile,
    }

    /// Factory installed by the composition root.
    pub type Factory = fn(Scope) -> Box<dyn HostAdmission>;

    static FACTORY: OnceLock<Factory> = OnceLock::new();

    /// Installs the unregistered-facade factory. First call wins.
    pub fn register(factory: Factory) {
        let _ = FACTORY.set(factory);
    }

    /// Builds an unregistered facade, or `None` when unwired.
    #[must_use]
    pub fn create(scope: Scope) -> Option<Box<dyn HostAdmission>> {
        FACTORY.get().map(|factory| factory(scope))
    }
}

/// VS Code user-data root for `home`.
///
/// Cline, Roo Code, and the other VS Code-hosted providers store transcripts
/// under this directory. The layout is fixed by VS Code itself, so the session
/// discovery walk resolves it directly rather than depending on the host
/// bundle crate.
#[must_use]
pub fn vscode_data_dir(home: &Path) -> PathBuf {
    platform_data_dir(home, "Code")
}

/// Kiro user-data root for `home`.
#[must_use]
pub fn kiro_data_dir(home: &Path) -> PathBuf {
    platform_data_dir(home, "Kiro")
}

fn platform_data_dir(home: &Path, product: &str) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support").join(product)
    }
    #[cfg(target_os = "linux")]
    {
        home.join(".config").join(product)
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let appdata_path = PathBuf::from(&appdata);
            if appdata_path.starts_with(home) {
                return appdata_path.join(product);
            }
        }
        home.join("AppData/Roaming").join(product)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        home.join(".config").join(product)
    }
}

/// Parses an ISO 8601 / RFC 3339 timestamp into unix epoch seconds.
///
/// Provider record parsers all normalize timestamps through the kernel's
/// zero-dependency parser; this is the unsigned-seconds shape they want.
#[must_use]
pub fn parse_timestamp(ts: &str) -> Option<u64> {
    let secs = tracedecay_runtime_core::timeutil::parse_rfc3339_timestamp(ts)?;
    u64::try_from(secs).ok()
}
