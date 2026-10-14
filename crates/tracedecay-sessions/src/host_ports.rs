//! Ports for the few host-side capabilities the session runtime reaches for
//! but must not own.
//!
//! The transcript runtime needs the agent host installation layout
//! (`tracedecay-agent-hosts`), which lives above this crate. Depending on it
//! from here would point the session layer back at the composition root.

use std::path::{Path, PathBuf};

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
