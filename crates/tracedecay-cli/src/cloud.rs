//! HTTP client for the worldwide counter Cloudflare Worker and GitHub release
//! version checking.
//!
//! Counter operations are best-effort: failures surface as `None` / empty.
//! Release lookups classify every failure as a [`ReleaseLookupError`]. All
//! are synchronous `ureq` calls that block the calling thread for up to their
//! own timeout, which an enclosing Tokio deadline cannot cut short. A caller
//! on an async or deadline-bound path must run them on a blocking thread and
//! bound the join itself (see the CLI status command).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use tracedecay_application::http_agent::http_agent;
use tracedecay_dashboard_api::cloud::ReleaseLookupError;

/// The Cloudflare Worker endpoint URL.
const WORKER_URL: &str = "https://tracedecay-counter.enzinol.workers.dev";

/// Base URL of the GitHub REST API release lookups are issued against.
pub(crate) const GITHUB_API_URL: &str = "https://api.github.com";
const GITHUB_OWNER: &str = "ScriptedAlchemy";
const GITHUB_REPOSITORY: &str = "tracedecay";

/// Timeout for flush (upload) requests.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Timeout for fetching the worldwide total (used in status).
const FETCH_TIMEOUT: Duration = Duration::from_secs(1);

/// Response from the worker's POST /increment and GET /total endpoints.
#[derive(serde::Deserialize)]
struct WorkerResponse {
    total: u64,
}

/// Creates a ureq agent with the given timeout.
pub fn agent_with_timeout(timeout: Duration) -> ureq::Agent {
    http_agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .build(),
    )
}

/// Uploads pending tokens to the worldwide counter.
/// Returns the new worldwide total on success, or None on any failure.
#[hotpath::measure(label = "cloud.flush_pending")]
pub fn flush_pending(amount: u64) -> Option<u64> {
    if amount == 0 {
        return None;
    }
    let body = serde_json::json!({ "amount": amount });
    let agent = agent_with_timeout(FLUSH_TIMEOUT);
    let parsed: WorkerResponse = agent
        .post(&format!("{WORKER_URL}/increment"))
        .send_json(&body)
        .ok()?
        .body_mut()
        .read_json()
        .ok()?;
    Some(parsed.total)
}

/// Fetches the current worldwide total from the worker.
/// Returns None on timeout, network error, or parse failure.
#[hotpath::measure(label = "cloud.fetch_worldwide_total")]
pub fn fetch_worldwide_total() -> Option<u64> {
    let agent = agent_with_timeout(FETCH_TIMEOUT);
    let parsed: WorkerResponse = agent
        .get(&format!("{WORKER_URL}/total"))
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()?;
    Some(parsed.total)
}

/// Response from the worker's GET /countries endpoint.
#[derive(serde::Deserialize)]
struct CountriesResponse {
    flags: Vec<String>,
}

/// Fetches country flags from the worldwide counter.
/// Returns a list of emoji flags, or an empty vec on failure.
#[hotpath::measure(label = "cloud.fetch_country_flags")]
pub fn fetch_country_flags() -> Vec<String> {
    let agent = agent_with_timeout(Duration::from_millis(500));
    let Ok(mut resp) = agent.get(&format!("{WORKER_URL}/countries")).call() else {
        return Vec::new();
    };
    let Ok(parsed): Result<CountriesResponse, _> = resp.body_mut().read_json() else {
        return Vec::new();
    };
    parsed.flags
}

/// Response from GitHub releases API (only the fields we need).
#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(serde::Deserialize)]
struct GitHubAsset {
    name: String,
}

/// Returns the platform slug matching the CI release matrix. Must stay in
/// sync with the `matrix.name` field in `.github/workflows/release.yml`
/// and `release-beta.yml`.
pub fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "aarch64-macos"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        "x86_64-macos"
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        "x86_64-linux"
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        "aarch64-linux"
    } else if cfg!(target_os = "windows") {
        "x86_64-windows"
    } else {
        "unknown"
    }
}

/// Archive naming convention per platform. Must stay in sync with the
/// `tar czf` / `Compress-Archive` invocations in `.github/workflows/release.yml`
/// and `release-beta.yml`:
///
/// - Stable: `tracedecay-v{version}-{platform}.{ext}`
/// - Beta:   `tracedecay-beta-v{version}-{platform}.{ext}`
pub fn asset_name(version: &str, is_beta: bool) -> String {
    let prefix = if is_beta {
        "tracedecay-beta"
    } else {
        "tracedecay"
    };
    platform_asset_name(prefix, version)
}

fn platform_asset_name(prefix: &str, version: &str) -> String {
    let platform = current_platform();
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    format!("{prefix}-v{version}-{platform}.{ext}")
}

/// First major version of the pre-reset release line that already shipped
/// `tracedecay-*` named assets (the 4.x–6.x era used the new asset names
/// before the version reset to 0.0.2). Latest-version detection refuses any
/// release tagged at or above this major so a restored pre-reset release can
/// never be offered as an "upgrade" to a post-reset binary. Revisit (raise or
/// remove) if the post-reset line ever legitimately approaches 4.0.
const PRE_RESET_EPOCH_MIN_MAJOR: u64 = 4;

/// True when `tag_name` (e.g. `v6.1.3`) belongs to the pre-reset release
/// epoch, see [`PRE_RESET_EPOCH_MIN_MAJOR`].
fn release_is_pre_reset_epoch(tag_name: &str) -> bool {
    let version = tag_name.trim_start_matches('v');
    let major = version
        .split(['.', '-'])
        .next()
        .and_then(|m| m.parse::<u64>().ok());
    major.is_some_and(|m| m >= PRE_RESET_EPOCH_MIN_MAJOR)
}

/// True when the release is a valid upgrade candidate for the current
/// platform: it must carry the `tracedecay-v*` / `tracedecay-beta-v*` asset
/// name for this platform and not belong to the pre-reset version epoch.
/// Pre-reset `tracedecay-*` releases (4.x-6.x) must never be offered as the
/// "latest" upgrade, even if old releases reappear.
fn release_has_current_platform_asset(release: &GitHubRelease) -> bool {
    if release_is_pre_reset_epoch(&release.tag_name) {
        return false;
    }
    let version = release.tag_name.trim_start_matches('v');
    let required = asset_name(version, release.prerelease);
    release.assets.iter().any(|a| a.name == required)
}

/// The credential release lookups send: the local GitHub login (`GH_TOKEN`,
/// `gh auth token`, or the git credential helper), which raises the quota
/// from 60 to 5000 requests per hour. `None` reads anonymously.
pub(crate) fn github_authorization() -> Option<String> {
    tracedecay_application::advisory::public_repository_read_authorization_v1(
        GITHUB_OWNER,
        GITHUB_REPOSITORY,
    )
    .map(|header| header.as_str().to_owned())
}

/// The REST URL of this repository's release collection under `api_base`.
pub(crate) fn releases_url(api_base: &str) -> String {
    format!("{api_base}/repos/{GITHUB_OWNER}/{GITHUB_REPOSITORY}/releases")
}

fn channel_name(is_beta: bool) -> &'static str {
    if is_beta { "beta" } else { "stable" }
}

/// Issues one release-metadata `GET` and classifies the answer. `Ok(None)` is
/// GitHub's 404 for `url`; every other non-success is a typed refusal.
pub(crate) fn get_release_json<T: DeserializeOwned>(
    url: &str,
    authorization: Option<&str>,
    timeout: Duration,
) -> Result<Option<T>, ReleaseLookupError> {
    get_release_json_over(
        &http_agent(release_lookup_config(timeout)),
        url,
        authorization,
    )
}

fn release_lookup_config(timeout: Duration) -> ureq::config::Config {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
}

fn get_release_json_over<T: DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
    authorization: Option<&str>,
) -> Result<Option<T>, ReleaseLookupError> {
    let mut request = agent
        .get(url)
        .header("User-Agent", "tracedecay")
        .header("Accept", "application/vnd.github+json");
    if let Some(authorization) = authorization {
        request = request.header("Authorization", authorization);
    }
    let mut response = request.call().map_err(transport_failure)?;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
    };
    match status {
        200..=299 => {}
        404 => return Ok(None),
        403 | 429
            if status == 429
                || header("x-ratelimit-remaining") == Some("0")
                || header("retry-after").is_some() =>
        {
            let reset_at = header("x-ratelimit-reset")
                .and_then(|reset| reset.parse::<u64>().ok())
                .or_else(|| {
                    let retry_after = header("retry-after")?.parse::<u64>().ok()?;
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
                    Some(now.as_secs().saturating_add(retry_after))
                });
            return Err(ReleaseLookupError::RateLimited { reset_at });
        }
        401 | 403 => return Err(ReleaseLookupError::Unauthorized { status }),
        _ => return Err(ReleaseLookupError::UnexpectedStatus { status }),
    }
    response
        .body_mut()
        .read_json()
        .map(Some)
        .map_err(|error| match error {
            ureq::Error::Timeout(_) => ReleaseLookupError::TimedOut,
            ureq::Error::Io(ref io) if io.kind() == std::io::ErrorKind::TimedOut => {
                ReleaseLookupError::TimedOut
            }
            error => ReleaseLookupError::MalformedResponse {
                detail: error.to_string(),
            },
        })
}

fn transport_failure(error: ureq::Error) -> ReleaseLookupError {
    match error {
        ureq::Error::Timeout(_) => ReleaseLookupError::TimedOut,
        ureq::Error::Io(ref io) if io.kind() == std::io::ErrorKind::TimedOut => {
            ReleaseLookupError::TimedOut
        }
        error => ReleaseLookupError::NetworkUnreachable {
            detail: error.to_string(),
        },
    }
}

/// Fetches the latest release version on the running build's channel: a
/// beta build sees only prereleases, a stable build only stable releases.
/// Releases whose CI hasn't yet uploaded the current-platform binary are
/// skipped, see `release_has_current_platform_asset`.
pub fn fetch_latest_version() -> Result<String, ReleaseLookupError> {
    fetch_latest_channel_version(is_beta())
}

/// Fetches the latest installable version of one channel from GitHub.
pub fn fetch_latest_channel_version(is_beta: bool) -> Result<String, ReleaseLookupError> {
    latest_release_version(GITHUB_API_URL, is_beta, github_authorization().as_deref())
}

#[hotpath::measure(label = "cloud.latest_release_version")]
fn latest_release_version(
    api_base: &str,
    is_beta: bool,
    authorization: Option<&str>,
) -> Result<String, ReleaseLookupError> {
    let releases = releases_url(api_base);
    let candidates: Vec<GitHubRelease> = if is_beta {
        get_release_json(
            &format!("{releases}?per_page=10"),
            authorization,
            FETCH_TIMEOUT,
        )?
        .unwrap_or_default()
    } else {
        get_release_json::<GitHubRelease>(
            &format!("{releases}/latest"),
            authorization,
            FETCH_TIMEOUT,
        )?
        .into_iter()
        .collect()
    };
    // GitHub lists releases newest-first, so the first installable match is
    // the latest. Releases whose CI is still in progress are skipped, they
    // will be picked up on the next check.
    candidates
        .into_iter()
        .find(|release| {
            release.prerelease == is_beta && release_has_current_platform_asset(release)
        })
        .map(|release| release.tag_name.trim_start_matches('v').to_string())
        .ok_or(ReleaseLookupError::NoAssetForPlatform {
            channel: channel_name(is_beta),
            platform: current_platform(),
        })
}

/// Returns true if the current build is a beta/prerelease version.
///
/// `env!` is evaluated here, in the product crate whose package version is
/// the workspace release version; the channel test itself is the shared
/// dashboard-api helper so there is one prerelease-detection rule.
pub fn is_beta() -> bool {
    tracedecay_dashboard_api::cloud::is_beta(env!("CARGO_PKG_VERSION"))
}

/// Admits these ureq implementations into the composition library so MCP
/// flush/version checks can run after the binary starts. Unregistered
/// lookups stay `None` (already best-effort).
pub fn admit_sync_probes() {
    tracedecay_dashboard_api::cloud::admit_sync_cloud_probes(flush_pending, fetch_latest_version);
}

pub fn doctor_network_probes() -> tracedecay::doctor::AdmittedDoctorNetworkProbes {
    tracedecay::doctor::AdmittedDoctorNetworkProbes {
        fetch_worldwide_total,
        fetch_latest_version,
    }
}

pub fn is_newer_version(current: &str, latest: &str) -> bool {
    tracedecay_dashboard_api::cloud::is_newer_version(current, latest)
}

pub fn is_newer_minor_version(current: &str, latest: &str) -> bool {
    tracedecay_dashboard_api::cloud::is_newer_minor_version(current, latest)
}

/// How tracedecay was installed, detected from the binary path.
pub enum InstallMethod {
    Cargo,
    Brew,
    Scoop,
    Unknown,
}

/// Detects how tracedecay was installed by inspecting the binary path.
pub fn detect_install_method() -> InstallMethod {
    let Ok(exe) = std::env::current_exe() else {
        return InstallMethod::Unknown;
    };
    let path = exe.to_string_lossy();
    if path.contains(".cargo/bin") || path.contains(".cargo\\bin") {
        InstallMethod::Cargo
    } else if path.contains("/homebrew/") || path.contains("/Cellar/") {
        InstallMethod::Brew
    } else if path.contains("\\scoop\\") || path.contains("/scoop/") {
        InstallMethod::Scoop
    } else {
        InstallMethod::Unknown
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use tracedecay_application::http_agent::{InterruptFirstReadConnector, http_agent_over};
    use ureq::unversioned::transport::DefaultConnector;

    fn release(tag: &str, prerelease: bool, asset_names: &[&str]) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.to_string(),
            prerelease,
            assets: asset_names
                .iter()
                .map(|n| GitHubAsset {
                    name: (*n).to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn skips_release_missing_current_platform_asset() {
        // Other platforms uploaded but ours hasn't yet (e.g. the macOS leg
        // of the matrix is still running). Detection should treat this as
        // "no upgrade for me" so the user isn't told about a version they
        // cannot install.
        let r = release(
            "v0.9.9",
            false,
            &[
                "tracedecay-v0.9.9-some-other-platform.tar.gz",
                "tracedecay-v0.9.9-yet-another-platform.tar.gz",
            ],
        );
        assert!(!release_has_current_platform_asset(&r));
        let expected = asset_name("0.9.9", false);
        let present = release("v0.9.9", false, &[&expected]);
        assert!(release_has_current_platform_asset(&present));
    }

    #[test]
    fn accepts_beta_release_with_matching_beta_asset() {
        let expected = asset_name("0.9.9-beta.1", true);
        let r = release("v0.9.9-beta.1", true, &[&expected]);
        assert!(release_has_current_platform_asset(&r));
    }

    #[test]
    fn rejects_stable_named_asset_on_beta_release() {
        // If someone uploads a stable-named asset to a prerelease, the
        // filter should still reject, the naming convention says beta
        // releases carry `*-beta-v...` assets.
        let stable_name = asset_name("0.9.9-beta.1", false);
        let r = release("v0.9.9-beta.1", true, &[&stable_name]);
        assert!(!release_has_current_platform_asset(&r));
        let beta_name = asset_name("0.9.9-beta.1", true);
        let accepted = release("v0.9.9-beta.1", true, &[&beta_name]);
        assert!(release_has_current_platform_asset(&accepted));
    }

    #[test]
    fn skips_pre_reset_release_even_with_tracedecay_assets() {
        // The 4.x–6.x pre-reset era already shipped `tracedecay-v*` assets,
        // so the asset-name guard alone would accept a restored release from
        // that line. The epoch ceiling must refuse it.
        for tag in ["v4.0.2", "v6.1.3", "v9.9.9"] {
            let version = tag.trim_start_matches('v');
            let expected = asset_name(version, false);
            let r = release(tag, false, &[&expected]);
            assert!(
                !release_has_current_platform_asset(&r),
                "pre-reset release {tag} must not be an upgrade candidate"
            );
        }
    }

    /// Answers every connection on a loopback port with `response` (a raw
    /// HTTP/1.1 response), or holds it unanswered when `None`, and forwards
    /// each request head it read.
    fn stub(response: Option<&'static str>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (heads, received) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0u8; 4096];
                let read = stream.read(&mut request).unwrap_or(0);
                let _ = heads.send(String::from_utf8_lossy(&request[..read]).into_owned());
                match response {
                    Some(response) => {
                        let _ = stream.write_all(response.as_bytes());
                    }
                    None => std::thread::sleep(Duration::from_secs(10)),
                }
            }
        });
        (base, received)
    }

    fn respond(status: &str, headers: &str, body: &str) -> &'static str {
        format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .leak()
    }

    #[test]
    fn an_exhausted_quota_is_rate_limited_until_the_advertised_reset() {
        let (base, heads) = stub(Some(respond(
            "403 Forbidden",
            "x-ratelimit-limit: 60\r\nx-ratelimit-remaining: 0\r\n\
             x-ratelimit-reset: 1790516841\r\n",
            r#"{"message":"API rate limit exceeded"}"#,
        )));

        let error = latest_release_version(&base, true, Some("Bearer test-token")).unwrap_err();

        assert_eq!(
            error,
            ReleaseLookupError::RateLimited {
                reset_at: Some(1_790_516_841)
            }
        );
        assert_eq!(
            error.to_string(),
            "rate_limited: GitHub API rate limit is exhausted; authenticate with `gh auth \
             login` or set GH_TOKEN (5000 requests/hour instead of 60), or retry after Sun, 27 \
             Sep 2026 13:47:21 GMT"
        );
        let head = heads.recv().unwrap().to_ascii_lowercase();
        assert!(
            head.starts_with("get /repos/scriptedalchemy/tracedecay/releases?per_page=10 "),
            "{head}"
        );
        assert!(
            head.contains("\r\nauthorization: bearer test-token\r\n"),
            "{head}"
        );
    }

    #[test]
    fn a_refused_credential_is_unauthorized() {
        let (base, _heads) = stub(Some(respond(
            "401 Unauthorized",
            "",
            r#"{"message":"Bad credentials"}"#,
        )));

        let error = latest_release_version(&base, true, Some("Bearer revoked")).unwrap_err();

        assert_eq!(error, ReleaseLookupError::Unauthorized { status: 401 });
        assert_eq!(
            error.to_string(),
            "unauthorized: GitHub refused the request's credentials (HTTP 401); refresh the \
             GitHub login with `gh auth login`, or set GH_TOKEN to a valid token (or unset an \
             invalid one)"
        );
    }

    #[test]
    fn a_missing_stable_release_is_no_asset_for_this_platform() {
        let (base, heads) = stub(Some(respond(
            "404 Not Found",
            "",
            r#"{"message":"Not Found"}"#,
        )));

        let error = latest_release_version(&base, false, None).unwrap_err();

        assert_eq!(
            error,
            ReleaseLookupError::NoAssetForPlatform {
                channel: "stable",
                platform: current_platform(),
            }
        );
        assert_eq!(
            error.to_string(),
            format!(
                "no_asset_for_platform: no stable release publishes a {} asset; release CI may \
                 still be uploading binaries, retry in a few minutes",
                current_platform()
            )
        );
        let head = heads.recv().unwrap().to_ascii_lowercase();
        assert!(
            head.starts_with("get /repos/scriptedalchemy/tracedecay/releases/latest "),
            "{head}"
        );
        assert!(!head.contains("authorization:"), "{head}");
    }

    #[test]
    fn only_a_release_carrying_this_platforms_asset_is_installable() {
        let listing = |assets: &str| {
            respond(
                "200 OK",
                "Content-Type: application/json\r\n",
                &format!(
                    r#"[{{"tag_name":"v0.9.9-beta.1","prerelease":true,"assets":[{assets}]}}]"#
                ),
            )
        };
        let installable = format!(r#"{{"name":"{}"}}"#, asset_name("0.9.9-beta.1", true));
        let (base, _heads) = stub(Some(listing(&installable)));
        assert_eq!(
            latest_release_version(&base, true, None).unwrap(),
            "0.9.9-beta.1"
        );

        let (base, _heads) = stub(Some(listing(
            r#"{"name":"tracedecay-beta-v0.9.9-beta.1-other.tar.gz"}"#,
        )));
        assert_eq!(
            latest_release_version(&base, true, None).unwrap_err(),
            ReleaseLookupError::NoAssetForPlatform {
                channel: "beta",
                platform: current_platform(),
            }
        );
    }

    /// A caught signal fails a socket read that has a receive timeout with
    /// `EINTR` even under `SA_RESTART`, and the CLI catches `SIGCHLD` while
    /// tokio supervises any child. The lookup resumes the wait instead of
    /// reporting GitHub as unreachable.
    #[test]
    fn a_signal_interrupting_the_lookup_is_resumed_not_reported_as_unreachable() {
        let tag = "v0.9.9-beta.1";
        let asset = asset_name("0.9.9-beta.1", true);
        let (base, _heads) = stub(Some(respond(
            "200 OK",
            "Content-Type: application/json\r\n",
            &format!(
                r#"[{{"tag_name":"{tag}","prerelease":true,"assets":[{{"name":"{asset}"}}]}}]"#
            ),
        )));
        let agent = http_agent_over(
            release_lookup_config(FETCH_TIMEOUT),
            InterruptFirstReadConnector(DefaultConnector::new()),
        );

        let releases: Vec<GitHubRelease> = get_release_json_over(
            &agent,
            &format!("{}?per_page=10", releases_url(&base)),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            releases
                .iter()
                .map(|release| release.tag_name.as_str())
                .collect::<Vec<_>>(),
            [tag]
        );
    }

    #[test]
    fn a_body_that_is_not_release_metadata_is_malformed() {
        let (base, _heads) = stub(Some(respond("200 OK", "", "<html>garbage</html>")));

        let error = latest_release_version(&base, true, None).unwrap_err();

        assert!(
            matches!(error, ReleaseLookupError::MalformedResponse { .. }),
            "{error:?}"
        );
        assert_eq!(error.state(), "malformed_response");
        assert_eq!(
            error.remedy(),
            "retry later; if it persists, report it at \
             https://github.com/ScriptedAlchemy/tracedecay/issues"
        );
    }

    #[test]
    fn an_unanswered_request_times_out() {
        let (base, _heads) = stub(None);

        let error = latest_release_version(&base, true, None).unwrap_err();

        assert_eq!(error, ReleaseLookupError::TimedOut);
        assert_eq!(
            error.to_string(),
            "timed_out: GitHub did not answer in time; retry when the connection to GitHub is \
             responsive"
        );
    }

    #[test]
    fn a_refused_connection_is_network_unreachable() {
        // A connection that dies at the transport level before any HTTP
        // answer is NetworkUnreachable. Unix refuses a SYN to a port that is
        // bound but never listening instantly (the socket is held so the
        // port cannot be reused, whereas a dropped listener stays
        // connectable while a sibling test's forked child still holds the
        // inherited descriptor). The Windows kernel retries a refused SYN
        // for about two seconds — longer than the lookup budget — so there
        // the same failure class is reached by accepting the connection and
        // resetting it with a zero-linger close.
        #[cfg(unix)]
        let (base, _hold) = {
            let refusing =
                socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
            refusing
                .bind(
                    &"127.0.0.1:0"
                        .parse::<std::net::SocketAddr>()
                        .unwrap()
                        .into(),
                )
                .unwrap();
            (
                format!(
                    "http://{}",
                    refusing.local_addr().unwrap().as_socket().unwrap()
                ),
                refusing,
            )
        };
        #[cfg(windows)]
        let (base, _hold) = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let hold = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let _ = socket2::SockRef::from(&stream).set_linger(Some(Duration::ZERO));
                    drop(stream);
                }
            });
            (base, hold)
        };

        let error = latest_release_version(&base, true, None).unwrap_err();

        assert!(
            matches!(error, ReleaseLookupError::NetworkUnreachable { .. }),
            "{error:?}"
        );
        assert_eq!(
            error.remedy(),
            "check the network connection and proxy settings, then retry"
        );
    }

    #[test]
    fn pre_reset_epoch_boundary() {
        assert!(!release_is_pre_reset_epoch("v0.0.2"));
        assert!(!release_is_pre_reset_epoch("v1.0.0"));
        assert!(!release_is_pre_reset_epoch("v3.9.9"));
        assert!(release_is_pre_reset_epoch("v4.0.0"));
        assert!(release_is_pre_reset_epoch("v6.1.3"));
        assert!(release_is_pre_reset_epoch("v6.2.0-beta.1"));
    }
}
