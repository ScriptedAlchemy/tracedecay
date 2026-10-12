//! Ports for the few host-side capabilities the session runtime reaches for
//! but must not own.
//!
//! Three things the transcript runtime needs live above this crate: the agent
//! host installation layout (`tracedecay-agent-hosts`) and the hook process
//! spawner (root `src/hooks/`). Depending on either from here would point the
//! session layer back at the composition root.
//!
//! The session review scheduler is an explicit [`session_review::SessionReviewPort`] value that a user
//! ingest pass receives through its `SessionIngestAuthority`, so an unwired
//! pass is a typed refusal rather than a silent skip.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

/// Schedules the post-ingest user session review.
///
/// The review hint is delivered through the root-owned daemon client. This
/// crate must not know the daemon transport or scheduling policy.
pub mod session_review {
    use super::{Future, Pin};

    pub type Scheduler =
        for<'a> fn(&'a str, Option<&'a str>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

    /// The root-owned review scheduler as an explicit handle.
    ///
    /// Constructing one requires the scheduler, and a user ingest pass must be
    /// handed one by its authority; there is no process-global slot that can
    /// be left empty and read back as "do nothing".
    #[derive(Clone, Copy, Debug)]
    pub struct SessionReviewPort {
        scheduler: Scheduler,
    }

    impl SessionReviewPort {
        #[must_use]
        pub const fn new(scheduler: Scheduler) -> Self {
            Self { scheduler }
        }

        /// Requests a review pass for the sessions the ingest pass just wrote.
        pub async fn schedule(self, provider: &str, session_id: Option<&str>) {
            (self.scheduler)(provider, session_id).await;
        }
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
        // Joined per component: this root's text becomes durable transcript
        // cursor keys, so it must carry the native spelling.
        home.join("AppData").join("Roaming").join(product)
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
