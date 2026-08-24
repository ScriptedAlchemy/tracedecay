//! Read-only GitHub credential authority backed by the user's existing `gh`
//! CLI login.
//!
//! An unauthenticated GitHub client is allowed 60 requests per hour. The same
//! requests carrying the token that `gh auth token` already holds are allowed
//! 5,000 per hour. This module turns that local login into a
//! [`GitHubReadOnlyCredentialAuthorityV1`] so public-repository reads stop
//! burning the anonymous budget, without `TraceDecay` ever storing, logging, or
//! persisting a token byte.
//!
//! # Token handling
//!
//! * Token bytes exist only inside [`Zeroizing`] containers and the
//!   [`GitHubReadOnlyCredentialSecretV1`] newtype, which itself derives neither
//!   `Debug` nor Serde.
//! * The probe is invoked as an argv array, never a shell string, with
//!   `stdin` and `stderr` both `Stdio::null()` so provider diagnostics can
//!   never reach a `TraceDecay` log.
//! * Every failure mode - `gh` absent, not logged in, non-zero exit, empty or
//!   oversized or non-UTF-8 output, a hung child, a poisoned lock - degrades to
//!   "no credential", never to an error. The caller then reads anonymously.
//! * Under `cfg(test)` and the `test-transport` feature the default source is
//!   a stub that returns `None`, so no test build can spawn `gh` or observe a
//!   developer's real login.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(any(test, feature = "test-transport")))]
use std::io::Read;
#[cfg(not(any(test, feature = "test-transport")))]
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use super::network::{
    GitHubReadOnlyCredentialAuthorityOutcomeV1, GitHubReadOnlyCredentialAuthorityV1,
    GitHubReadOnlyCredentialSecretV1, GitHubReadOnlyCredentialV1, GitHubReadPermissionV1,
    RegisteredGitHubReadOnlyCredentialV1, register_github_read_only_credential_authority_v1,
    resolve_registered_github_read_only_credential_v1,
    unregister_github_read_only_credential_authority_v1,
};

/// Executable probed for an existing login. Never a shell string.
#[cfg(not(any(test, feature = "test-transport")))]
const GH_EXECUTABLE_V1: &str = "gh";
/// Exact argv passed to `GH_EXECUTABLE_V1`.
#[cfg(not(any(test, feature = "test-transport")))]
const GH_AUTH_TOKEN_ARGV_V1: [&str; 2] = ["auth", "token"];
/// Upper bound on accepted token bytes, matching the secret newtype's own cap.
const MAX_GH_TOKEN_BYTES_V1: usize = 4096;
/// A local credential helper that has not answered within this bound is killed.
#[cfg(not(any(test, feature = "test-transport")))]
const MAX_GH_PROBE_DURATION_V1: Duration = Duration::from_secs(3);
/// Poll interval while waiting for the probe to exit.
#[cfg(not(any(test, feature = "test-transport")))]
const GH_PROBE_POLL_INTERVAL_V1: Duration = Duration::from_millis(10);
/// How long one probe outcome - positive or negative - is reused.
const GH_TOKEN_CACHE_TTL_V1: Duration = Duration::from_secs(300);
/// Bound on retained per-repository authorities.
const MAX_RETAINED_GH_AUTHORITIES_V1: usize = 256;

/// Local process boundary that can yield an existing GitHub token.
///
/// Implementations must return `None` for every failure. No implementor may
/// derive `Debug` or Serde: the returned value is secret material.
pub trait GhCliTokenSourceV1: Send + Sync {
    fn token(&self) -> Option<Zeroizing<String>>;
}

/// Production source: runs `gh auth token` as a bounded child process.
///
/// Absent from every test build, so a test cannot construct the one type that
/// can spawn a provider probe.
#[cfg(not(any(test, feature = "test-transport")))]
pub struct GhAuthTokenCommandSourceV1;

#[cfg(not(any(test, feature = "test-transport")))]
impl GhCliTokenSourceV1 for GhAuthTokenCommandSourceV1 {
    fn token(&self) -> Option<Zeroizing<String>> {
        probe_gh_auth_token_v1()
    }
}

/// Test-build source. Always `None`, so no test can spawn `gh`.
#[cfg(any(test, feature = "test-transport"))]
pub struct NullGhCliTokenSourceV1;

#[cfg(any(test, feature = "test-transport"))]
impl GhCliTokenSourceV1 for NullGhCliTokenSourceV1 {
    fn token(&self) -> Option<Zeroizing<String>> {
        None
    }
}

#[cfg(not(any(test, feature = "test-transport")))]
fn probe_gh_auth_token_v1() -> Option<Zeroizing<String>> {
    let mut child = Command::new(GH_EXECUTABLE_V1)
        .args(GH_AUTH_TOKEN_ARGV_V1)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + MAX_GH_PROBE_DURATION_V1;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(GH_PROBE_POLL_INTERVAL_V1),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut stdout = child.stdout.take()?;
    if !status.success() {
        return None;
    }
    let mut bytes = Zeroizing::new(Vec::<u8>::new());
    stdout
        .by_ref()
        .take(MAX_GH_TOKEN_BYTES_V1 as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_GH_TOKEN_BYTES_V1 {
        return None;
    }
    let text = Zeroizing::new(String::from_utf8(bytes.to_vec()).ok()?);
    let trimmed = Zeroizing::new(text.trim().to_owned());
    (!trimmed.is_empty()).then_some(trimmed)
}

fn default_gh_cli_token_source_v1() -> Arc<dyn GhCliTokenSourceV1> {
    #[cfg(any(test, feature = "test-transport"))]
    {
        Arc::new(NullGhCliTokenSourceV1)
    }
    #[cfg(not(any(test, feature = "test-transport")))]
    {
        Arc::new(GhAuthTokenCommandSourceV1)
    }
}

type GhCliTokenSourceLockV1 = Mutex<Option<Arc<dyn GhCliTokenSourceV1>>>;

fn installed_gh_cli_token_source_v1() -> &'static GhCliTokenSourceLockV1 {
    static SOURCE: OnceLock<GhCliTokenSourceLockV1> = OnceLock::new();
    SOURCE.get_or_init(|| Mutex::new(None))
}

fn active_gh_cli_token_source_v1() -> Option<Arc<dyn GhCliTokenSourceV1>> {
    let installed = installed_gh_cli_token_source_v1().lock().ok()?;
    Some(
        installed
            .as_ref()
            .map_or_else(default_gh_cli_token_source_v1, Arc::clone),
    )
}

/// One probe outcome retained for [`GH_TOKEN_CACHE_TTL_V1`].
///
/// `secret == None` is the retained negative result: a machine without `gh`,
/// or without a login, must not spawn a child process per HTTP request.
struct CachedGhTokenV1 {
    secret: Option<GitHubReadOnlyCredentialSecretV1>,
    expires_at: Instant,
}

/// Credential authority whose secret is the user's existing `gh` login.
///
/// # Deviation from [`GitHubReadOnlyCredentialAuthorityV1`]
///
/// The trait asks implementations to establish effective provider permissions
/// before returning `Verified`, and offers `WriteCapable` for a token that can
/// write. A `gh` login usually *is* write-capable, and its effective scopes
/// cannot be established without spending a provider request - the exact cost
/// this authority exists to save. This implementation therefore reports
/// `Verified` with the five read permissions the read-only clients ask for,
/// and accepts the deviation because it is bounded on every side:
///
/// * it is only ever mounted for a repository the profile mount has already
///   classified `Public`, so it grants no data access the `anonymous()`
///   credential it replaces did not already have;
/// * it feeds only the fixed-`GET` read clients plus one static GraphQL
///   *query* document, so a write-capable token cannot express a mutation
///   through this surface;
/// * the alternative - one `gh auth status` scope check per TTL window -
///   itself costs a provider request and still reports OAuth scope labels
///   rather than effective permissions, which the trait already rejects as
///   insufficient evidence.
pub struct GhCliGitHubReadOnlyCredentialAuthorityV1 {
    source: Arc<dyn GhCliTokenSourceV1>,
    ttl: Duration,
    cache: Mutex<Option<CachedGhTokenV1>>,
}

impl GhCliGitHubReadOnlyCredentialAuthorityV1 {
    fn new(source: Arc<dyn GhCliTokenSourceV1>, ttl: Duration) -> Self {
        Self {
            source,
            ttl,
            cache: Mutex::new(None),
        }
    }

    fn read_permissions() -> BTreeSet<GitHubReadPermissionV1> {
        BTreeSet::from([
            GitHubReadPermissionV1::Metadata,
            GitHubReadPermissionV1::PullRequests,
            GitHubReadPermissionV1::Contents,
            GitHubReadPermissionV1::Actions,
            GitHubReadPermissionV1::Checks,
        ])
    }
}

impl GitHubReadOnlyCredentialAuthorityV1 for GhCliGitHubReadOnlyCredentialAuthorityV1 {
    /// A `gh` login is account-wide, so the repository identity is not part of
    /// the probe. Repository binding is enforced by the registry slot this
    /// authority occupies, not by the probe.
    fn resolve(
        &self,
        _repository_owner: &str,
        _repository_name: &str,
    ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
        // A poisoned lock degrades to anonymous rather than erroring.
        let Ok(mut cache) = self.cache.lock() else {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured;
        };
        let now = Instant::now();
        let fresh = cache
            .as_ref()
            .filter(|entry| entry.expires_at > now)
            .map(|entry| entry.secret.clone());
        // The probe runs while the lock is held so concurrent resolutions
        // collapse into one child process instead of a stampede.
        let secret = match fresh {
            Some(secret) => secret,
            None => {
                let probed = self
                    .source
                    .token()
                    .and_then(GitHubReadOnlyCredentialSecretV1::from_zeroizing);
                *cache = Some(CachedGhTokenV1 {
                    secret: probed.clone(),
                    expires_at: now + self.ttl,
                });
                probed
            }
        };
        drop(cache);
        secret.map_or(
            GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured,
            |secret| GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                secret,
                exact_permissions: Self::read_permissions(),
            },
        )
    }
}

type RetainedGhAuthorityMapV1 =
    BTreeMap<(String, String), Arc<GhCliGitHubReadOnlyCredentialAuthorityV1>>;
type RetainedGhAuthorityLockV1 = Mutex<RetainedGhAuthorityMapV1>;

fn retained_gh_authorities_v1() -> &'static RetainedGhAuthorityLockV1 {
    static AUTHORITIES: OnceLock<RetainedGhAuthorityLockV1> = OnceLock::new();
    AUTHORITIES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn retained_gh_authority_v1(
    repository_owner: &str,
    repository_name: &str,
) -> Option<Arc<dyn GitHubReadOnlyCredentialAuthorityV1>> {
    let source = active_gh_cli_token_source_v1()?;
    let mut authorities = retained_gh_authorities_v1().lock().ok()?;
    let key = (repository_owner.to_owned(), repository_name.to_owned());
    if let Some(existing) = authorities.get(&key) {
        return Some(Arc::clone(existing) as Arc<dyn GitHubReadOnlyCredentialAuthorityV1>);
    }
    if authorities.len() >= MAX_RETAINED_GH_AUTHORITIES_V1 {
        return None;
    }
    let authority = Arc::new(GhCliGitHubReadOnlyCredentialAuthorityV1::new(
        source,
        GH_TOKEN_CACHE_TTL_V1,
    ));
    authorities.insert(key, Arc::clone(&authority));
    Some(authority as Arc<dyn GitHubReadOnlyCredentialAuthorityV1>)
}

fn retained_gh_authority_if_present_v1(
    repository_owner: &str,
    repository_name: &str,
) -> Option<Arc<dyn GitHubReadOnlyCredentialAuthorityV1>> {
    let authorities = retained_gh_authorities_v1().lock().ok()?;
    authorities
        .get(&(repository_owner.to_owned(), repository_name.to_owned()))
        .map(|authority| Arc::clone(authority) as Arc<dyn GitHubReadOnlyCredentialAuthorityV1>)
}

/// Withdraws only the exact retained `gh` authority from the application
/// registry slot for this repository.
///
/// The eviction is `Arc::ptr_eq`-exact, so a real private credential occupying
/// the same slot is never displaced.
pub(super) fn withdraw_gh_cli_github_read_only_credential_v1(
    repository_owner: &str,
    repository_name: &str,
) -> bool {
    let Some(authority) = retained_gh_authority_if_present_v1(repository_owner, repository_name)
    else {
        return false;
    };
    unregister_github_read_only_credential_authority_v1(
        repository_owner,
        repository_name,
        &authority,
    )
}

/// Mounts the existing `gh` login as the read credential for one repository.
///
/// Returns `None` - never an error - when there is no usable login, when the
/// registry slot already belongs to another authority, or when the resolved
/// credential does not permit `Contents`.
pub(super) fn mount_gh_cli_github_read_only_credential_v1(
    repository_owner: &str,
    repository_name: &str,
) -> Option<GitHubReadOnlyCredentialV1> {
    let authority = retained_gh_authority_v1(repository_owner, repository_name)?;
    // Probe through the authority's own cache before touching the registry so
    // a machine with no `gh` login never churns the registered slot.
    if !matches!(
        authority.resolve(repository_owner, repository_name),
        GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified { .. }
    ) {
        return None;
    }
    if !register_github_read_only_credential_authority_v1(
        repository_owner,
        repository_name,
        &authority,
    ) {
        return None;
    }
    match resolve_registered_github_read_only_credential_v1(repository_owner, repository_name) {
        RegisteredGitHubReadOnlyCredentialV1::Verified(credential)
            if credential.permits(GitHubReadPermissionV1::Contents) =>
        {
            Some(credential)
        }
        RegisteredGitHubReadOnlyCredentialV1::Verified(_)
        | RegisteredGitHubReadOnlyCredentialV1::Missing
        | RegisteredGitHubReadOnlyCredentialV1::Rejected => {
            withdraw_gh_cli_github_read_only_credential_v1(repository_owner, repository_name);
            None
        }
    }
}

/// Resolves the credential a *public* repository read should use.
///
/// Precedence: an already-registered real credential that permits `Contents`,
/// then the local `gh` login, then anonymous.
pub(super) fn public_repository_read_credential_v1(
    repository_owner: &str,
    repository_name: &str,
) -> GitHubReadOnlyCredentialV1 {
    if let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
        resolve_registered_github_read_only_credential_v1(repository_owner, repository_name)
        && credential.permits(GitHubReadPermissionV1::Contents)
    {
        return credential;
    }
    mount_gh_cli_github_read_only_credential_v1(repository_owner, repository_name)
        .unwrap_or_else(GitHubReadOnlyCredentialV1::anonymous)
}

/// Test-only guard installing an exact token source and clearing every
/// retained authority (and its cached probe) on both ends of its lifetime.
#[cfg(any(test, feature = "test-transport"))]
pub struct GhCliTokenSourceGuardV1;

#[cfg(any(test, feature = "test-transport"))]
impl GhCliTokenSourceGuardV1 {
    pub fn install(source: Arc<dyn GhCliTokenSourceV1>) -> Self {
        clear_retained_gh_authorities_v1();
        if let Ok(mut installed) = installed_gh_cli_token_source_v1().lock() {
            *installed = Some(source);
        }
        Self
    }
}

#[cfg(any(test, feature = "test-transport"))]
impl Drop for GhCliTokenSourceGuardV1 {
    fn drop(&mut self) {
        if let Ok(mut installed) = installed_gh_cli_token_source_v1().lock() {
            *installed = None;
        }
        clear_retained_gh_authorities_v1();
    }
}

#[cfg(any(test, feature = "test-transport"))]
fn clear_retained_gh_authorities_v1() {
    let Ok(mut authorities) = retained_gh_authorities_v1().lock() else {
        return;
    };
    for ((owner, repository), authority) in std::mem::take(&mut *authorities) {
        let authority = authority as Arc<dyn GitHubReadOnlyCredentialAuthorityV1>;
        let _ =
            unregister_github_read_only_credential_authority_v1(&owner, &repository, &authority);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that install a process-global token source.
    fn gh_source_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Obvious placeholder. Never a real token shape.
    const FIXTURE_TOKEN_V1: &str = "gh-fixture-not-a-real-token";

    struct CountingTokenSourceV1 {
        token: Option<String>,
        calls: Mutex<u32>,
    }

    impl CountingTokenSourceV1 {
        fn new(token: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                token: token.map(str::to_owned),
                calls: Mutex::new(0),
            })
        }

        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    impl GhCliTokenSourceV1 for CountingTokenSourceV1 {
        fn token(&self) -> Option<Zeroizing<String>> {
            *self.calls.lock().unwrap() += 1;
            self.token.clone().map(Zeroizing::new)
        }
    }

    #[test]
    fn absent_gh_login_degrades_public_reads_to_anonymous() {
        let _serialized = gh_source_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = CountingTokenSourceV1::new(None);
        let _guard =
            GhCliTokenSourceGuardV1::install(Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>);
        let credential = public_repository_read_credential_v1("octo", "absent-login");
        assert_eq!(
            credential.generation(),
            0,
            "absent login must stay anonymous"
        );
        assert!(
            mount_gh_cli_github_read_only_credential_v1("octo", "absent-login").is_none(),
            "absent login must not mount a credential"
        );
    }

    #[test]
    fn existing_gh_login_authenticates_a_public_repository_read() {
        let _serialized = gh_source_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = CountingTokenSourceV1::new(Some(FIXTURE_TOKEN_V1));
        let _guard =
            GhCliTokenSourceGuardV1::install(Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>);
        let credential = public_repository_read_credential_v1("octo", "present-login");
        assert_ne!(
            credential.generation(),
            0,
            "an existing gh login must replace the anonymous credential"
        );
        assert!(credential.permits(GitHubReadPermissionV1::Contents));
        assert!(credential.permits(GitHubReadPermissionV1::Actions));
        assert!(withdraw_gh_cli_github_read_only_credential_v1(
            "octo",
            "present-login"
        ));
    }

    #[test]
    fn repeated_resolution_probes_the_local_source_once_per_ttl_window() {
        let source = CountingTokenSourceV1::new(Some(FIXTURE_TOKEN_V1));
        let authority = GhCliGitHubReadOnlyCredentialAuthorityV1::new(
            Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>,
            Duration::from_secs(300),
        );
        for _ in 0..25 {
            assert!(matches!(
                authority.resolve("octo", "cached"),
                GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified { .. }
            ));
        }
        assert_eq!(
            source.calls(),
            1,
            "cached authority must not spawn one probe per request"
        );
    }

    #[test]
    fn absent_login_is_negatively_cached_within_the_ttl_window() {
        let source = CountingTokenSourceV1::new(None);
        let authority = GhCliGitHubReadOnlyCredentialAuthorityV1::new(
            Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>,
            Duration::from_secs(300),
        );
        for _ in 0..25 {
            assert!(matches!(
                authority.resolve("octo", "negative"),
                GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured
            ));
        }
        assert_eq!(source.calls(), 1, "a negative probe must also be cached");
    }

    #[test]
    fn expired_cache_reprobes_the_local_source() {
        let source = CountingTokenSourceV1::new(Some(FIXTURE_TOKEN_V1));
        let authority = GhCliGitHubReadOnlyCredentialAuthorityV1::new(
            Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>,
            Duration::ZERO,
        );
        assert!(matches!(
            authority.resolve("octo", "expiring"),
            GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified { .. }
        ));
        assert!(matches!(
            authority.resolve("octo", "expiring"),
            GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified { .. }
        ));
        assert_eq!(source.calls(), 2, "an expired window must re-probe");
    }

    #[test]
    fn whitespace_and_empty_provider_output_never_becomes_a_credential() {
        for output in ["", "   ", "\n", "\t\r\n"] {
            let source = CountingTokenSourceV1::new(Some(output));
            let authority = GhCliGitHubReadOnlyCredentialAuthorityV1::new(
                Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>,
                Duration::from_secs(300),
            );
            assert!(
                matches!(
                    authority.resolve("octo", "blank"),
                    GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured
                ),
                "blank provider output must not become a credential"
            );
        }
    }

    #[test]
    fn oversized_provider_output_never_becomes_a_credential() {
        let oversized = "a".repeat(MAX_GH_TOKEN_BYTES_V1 + 1);
        let source = CountingTokenSourceV1::new(Some(&oversized));
        let authority = GhCliGitHubReadOnlyCredentialAuthorityV1::new(
            Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>,
            Duration::from_secs(300),
        );
        assert!(matches!(
            authority.resolve("octo", "oversized"),
            GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured
        ));
    }

    #[test]
    fn default_test_build_source_never_spawns_a_provider_probe() {
        let source = default_gh_cli_token_source_v1();
        assert!(
            source.token().is_none(),
            "test builds must default to a stub source"
        );
    }

    #[test]
    fn a_registered_real_credential_outranks_the_local_login() {
        let _serialized = gh_source_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        struct RealAuthorityV1;
        impl GitHubReadOnlyCredentialAuthorityV1 for RealAuthorityV1 {
            fn resolve(
                &self,
                _owner: &str,
                _repository: &str,
            ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
                GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                    secret: GitHubReadOnlyCredentialSecretV1::new("real-fixture-not-a-token")
                        .unwrap(),
                    exact_permissions: BTreeSet::from([
                        GitHubReadPermissionV1::Contents,
                        GitHubReadPermissionV1::Metadata,
                    ]),
                }
            }
        }
        let source = CountingTokenSourceV1::new(Some(FIXTURE_TOKEN_V1));
        let _guard =
            GhCliTokenSourceGuardV1::install(Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>);
        let real = Arc::new(RealAuthorityV1) as Arc<dyn GitHubReadOnlyCredentialAuthorityV1>;
        assert!(register_github_read_only_credential_authority_v1(
            "octo",
            "already-private",
            &real
        ));
        let credential = public_repository_read_credential_v1("octo", "already-private");
        assert!(credential.permits(GitHubReadPermissionV1::Contents));
        assert_eq!(
            source.calls(),
            0,
            "a registered real credential must not trigger a local probe"
        );
        assert!(unregister_github_read_only_credential_authority_v1(
            "octo",
            "already-private",
            &real
        ));
    }

    /// The hazard this guards: a parked `gh` entry occupies the exact
    /// `(owner, repo)` registry slot, so a later real private mount for the
    /// same repository would otherwise be `Rejected`.
    #[test]
    fn a_parked_gh_entry_never_blocks_a_later_real_private_mount() {
        use super::super::network::{
            ProfileGitHubReadOnlyCredentialMountOutcomeV1,
            mount_profile_github_read_only_credential_authority_v1,
            register_profile_github_read_only_credential_authority_v1,
        };
        use tracedecay_domain::UserProfileId;

        let _serialized = gh_source_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        struct PrivateAuthorityV1;
        impl GitHubReadOnlyCredentialAuthorityV1 for PrivateAuthorityV1 {
            fn resolve(
                &self,
                _owner: &str,
                _repository: &str,
            ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
                GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                    secret: GitHubReadOnlyCredentialSecretV1::new("private-fixture-not-a-token")
                        .unwrap(),
                    exact_permissions: BTreeSet::from([
                        GitHubReadPermissionV1::Contents,
                        GitHubReadPermissionV1::PullRequests,
                    ]),
                }
            }
        }

        let source = CountingTokenSourceV1::new(Some(FIXTURE_TOKEN_V1));
        let _guard =
            GhCliTokenSourceGuardV1::install(Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>);
        // Park a gh login in the slot, exactly as a public read would.
        assert!(
            mount_gh_cli_github_read_only_credential_v1("octo", "later-private").is_some(),
            "fixture login must park an entry in the slot"
        );

        let profile = UserProfileId::new("profile.github.parked").unwrap();
        let private = Arc::new(PrivateAuthorityV1) as Arc<dyn GitHubReadOnlyCredentialAuthorityV1>;
        assert!(register_profile_github_read_only_credential_authority_v1(
            profile.clone(),
            "octo",
            "later-private",
            &private,
        ));
        assert_eq!(
            mount_profile_github_read_only_credential_authority_v1(
                &profile,
                "octo",
                "later-private"
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted,
            "a parked gh entry must not reject a real private mount"
        );
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
            resolve_registered_github_read_only_credential_v1("octo", "later-private")
        else {
            panic!("the real private credential must own the slot after the retry");
        };
        assert!(credential.permits(GitHubReadPermissionV1::PullRequests));
        assert!(
            !credential.permits(GitHubReadPermissionV1::Actions),
            "the resolved credential must be the private one, not the gh login"
        );
        assert!(unregister_github_read_only_credential_authority_v1(
            "octo",
            "later-private",
            &private
        ));
    }

    #[test]
    fn withdrawal_never_evicts_a_foreign_credential_from_the_slot() {
        let _serialized = gh_source_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        struct ForeignAuthorityV1;
        impl GitHubReadOnlyCredentialAuthorityV1 for ForeignAuthorityV1 {
            fn resolve(
                &self,
                _owner: &str,
                _repository: &str,
            ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
                GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured
            }
        }
        let source = CountingTokenSourceV1::new(Some(FIXTURE_TOKEN_V1));
        let _guard =
            GhCliTokenSourceGuardV1::install(Arc::clone(&source) as Arc<dyn GhCliTokenSourceV1>);
        // Retain a gh authority for this repository without registering it.
        assert!(retained_gh_authority_v1("octo", "foreign-slot").is_some());
        let foreign = Arc::new(ForeignAuthorityV1) as Arc<dyn GitHubReadOnlyCredentialAuthorityV1>;
        assert!(register_github_read_only_credential_authority_v1(
            "octo",
            "foreign-slot",
            &foreign
        ));
        assert!(
            !withdraw_gh_cli_github_read_only_credential_v1("octo", "foreign-slot"),
            "withdrawal must not evict a credential it does not own"
        );
        assert!(unregister_github_read_only_credential_authority_v1(
            "octo",
            "foreign-slot",
            &foreign
        ));
    }
}
