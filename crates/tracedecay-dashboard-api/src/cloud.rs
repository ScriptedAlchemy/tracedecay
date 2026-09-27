use std::cmp::Ordering;
use std::sync::OnceLock;
use std::time::{Duration, UNIX_EPOCH};

use semver::Version;

/// Whether `release_version` names a prerelease (beta-channel) build.
///
/// Takes the product release version because `env!` expands against the
/// crate that writes it: evaluating `CARGO_PKG_VERSION` here would bake this
/// crate's own version and report "stable" for every beta product build.
/// Only the version core is inspected, semver build metadata after `+`
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
    // Beta and stable are separate channels, never suggest cross-channel updates.
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

/// Why a GitHub release lookup produced no installable version.
///
/// The updater, `doctor`, and the version warning share this one
/// classification, so a refusal GitHub answered is never reported as a
/// missing asset or as being offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseLookupError {
    /// GitHub refused the request because the API quota is exhausted.
    /// `reset_at` is the Unix second the quota resets at, from
    /// `x-ratelimit-reset` (or `retry-after`), when GitHub sent one.
    RateLimited { reset_at: Option<u64> },
    /// GitHub refused the request's credentials (HTTP 401 or 403).
    Unauthorized { status: u16 },
    /// No HTTP exchange completed: DNS, connect, TLS, or proxy failure.
    NetworkUnreachable { detail: String },
    /// GitHub did not answer within the lookup's deadline.
    TimedOut,
    /// GitHub answered successfully with a body that is not release metadata.
    MalformedResponse { detail: String },
    /// GitHub answered with a status the lookup has no meaning for.
    UnexpectedStatus { status: u16 },
    /// GitHub answered, and no release on `channel` publishes an asset for
    /// `platform` yet.
    NoAssetForPlatform {
        channel: &'static str,
        platform: &'static str,
    },
}

impl ReleaseLookupError {
    /// The outcome's stable name.
    pub fn state(&self) -> &'static str {
        match self {
            Self::RateLimited { .. } => "rate_limited",
            Self::Unauthorized { .. } => "unauthorized",
            Self::NetworkUnreachable { .. } => "network_unreachable",
            Self::TimedOut => "timed_out",
            Self::MalformedResponse { .. } => "malformed_response",
            Self::UnexpectedStatus { .. } => "unexpected_status",
            Self::NoAssetForPlatform { .. } => "no_asset_for_platform",
        }
    }

    /// What the operator can do about this outcome.
    pub fn remedy(&self) -> String {
        match self {
            Self::RateLimited { reset_at } => {
                let retry = reset_at.map_or_else(
                    || "retry later".to_owned(),
                    |reset_at| format!("retry after {}", http_date(reset_at)),
                );
                format!(
                    "authenticate with `gh auth login` or set GH_TOKEN (5000 requests/hour \
                     instead of 60), or {retry}"
                )
            }
            Self::Unauthorized { .. } => "refresh the GitHub login with `gh auth login`, or set \
                 GH_TOKEN to a valid token (or unset an invalid one)"
                .to_owned(),
            Self::NetworkUnreachable { .. } => {
                "check the network connection and proxy settings, then retry".to_owned()
            }
            Self::TimedOut => "retry when the connection to GitHub is responsive".to_owned(),
            Self::MalformedResponse { .. } | Self::UnexpectedStatus { .. } => {
                "retry later; if it persists, report it at \
                 https://github.com/ScriptedAlchemy/tracedecay/issues"
                    .to_owned()
            }
            Self::NoAssetForPlatform { .. } => {
                "release CI may still be uploading binaries, retry in a few minutes".to_owned()
            }
        }
    }

    fn cause(&self) -> String {
        match self {
            Self::RateLimited { .. } => "GitHub API rate limit is exhausted".to_owned(),
            Self::Unauthorized { status } => {
                format!("GitHub refused the request's credentials (HTTP {status})")
            }
            Self::NetworkUnreachable { detail } => format!("GitHub is unreachable: {detail}"),
            Self::TimedOut => "GitHub did not answer in time".to_owned(),
            Self::MalformedResponse { detail } => {
                format!("GitHub answered with malformed release metadata: {detail}")
            }
            Self::UnexpectedStatus { status } => format!("GitHub answered HTTP {status}"),
            Self::NoAssetForPlatform { channel, platform } => {
                format!("no {channel} release publishes a {platform} asset")
            }
        }
    }
}

impl std::fmt::Display for ReleaseLookupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: {}; {}",
            self.state(),
            self.cause(),
            self.remedy()
        )
    }
}

impl std::error::Error for ReleaseLookupError {}

fn http_date(unix_seconds: u64) -> String {
    httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(unix_seconds))
}

type FlushPending = fn(u64) -> Option<u64>;
type FetchLatestVersion = fn() -> Result<String, ReleaseLookupError>;

static FLUSH_PENDING: OnceLock<FlushPending> = OnceLock::new();
static FETCH_LATEST_VERSION: OnceLock<FetchLatestVersion> = OnceLock::new();

/// Admits the CLI binary's sync ureq implementations. A second admission is
/// ignored so tests that re-enter process start stay idempotent. Unregistered
/// lookups return `None`.
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

pub fn fetch_latest_version() -> Option<Result<String, ReleaseLookupError>> {
    FETCH_LATEST_VERSION.get().map(|fetch| fetch())
}
