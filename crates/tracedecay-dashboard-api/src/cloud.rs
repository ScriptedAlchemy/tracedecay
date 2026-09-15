use std::cmp::Ordering;
use std::sync::OnceLock;

use semver::Version;

/// Whether `release_version` names a prerelease (beta-channel) build.
///
/// Takes the product release version because `env!` expands against the
/// crate that writes it: evaluating `CARGO_PKG_VERSION` here would bake this
/// crate's own version and report "stable" for every beta product build.
/// Only the version core is inspected — semver build metadata after `+`
/// (source SHA, dirty marker) never selects a channel.
pub fn is_beta(release_version: &str) -> bool {
    release_version
        .split('+')
        .next()
        .is_some_and(|core| core.contains('-'))
}

/// Returns true if `latest` is strictly newer than `current` using `SemVer`
/// precedence (`Version::cmp_precedence`), so build metadata does not affect
/// ordering. Stable and beta remain separate channels: a prerelease never
/// dominates a stable release (or the reverse), even when the numeric core is
/// higher.
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    let Ok(current) = Version::parse(current) else {
        return false;
    };
    let Ok(latest) = Version::parse(latest) else {
        return false;
    };
    // Beta and stable are separate channels — never suggest cross-channel updates.
    if current.pre.is_empty() != latest.pre.is_empty() {
        return false;
    }
    latest.cmp_precedence(&current) == Ordering::Greater
}

/// Returns true if `latest` is a newer version than `current` AND the
/// difference is at least a minor version bump (patch-only bumps return false).
pub fn is_newer_minor_version(current: &str, latest: &str) -> bool {
    let Ok(current) = Version::parse(current) else {
        return false;
    };
    let Ok(latest) = Version::parse(latest) else {
        return false;
    };
    if current.pre.is_empty() != latest.pre.is_empty() {
        return false;
    }
    latest.cmp_precedence(&current) == Ordering::Greater
        && (latest.major, latest.minor) > (current.major, current.minor)
}

type FlushPending = fn(u64) -> Option<u64>;
type FetchLatestVersion = fn() -> Option<String>;

static FLUSH_PENDING: OnceLock<FlushPending> = OnceLock::new();
static FETCH_LATEST_VERSION: OnceLock<FetchLatestVersion> = OnceLock::new();

/// Admits the CLI binary's sync ureq implementations. A second admission is
/// ignored so tests that re-enter process start stay idempotent. Unregistered
/// lookups return `None` — the same best-effort miss as a network failure.
pub fn admit_sync_cloud_probes(
    flush_pending: FlushPending,
    fetch_latest_version: FetchLatestVersion,
) {
    let _ = FLUSH_PENDING.set(flush_pending);
    let _ = FETCH_LATEST_VERSION.set(fetch_latest_version);
}

pub fn flush_pending(amount: u64) -> Option<u64> {
    FLUSH_PENDING.get().and_then(|flush| flush(amount))
}

pub fn fetch_latest_version() -> Option<String> {
    FETCH_LATEST_VERSION.get().and_then(|fetch| fetch())
}
