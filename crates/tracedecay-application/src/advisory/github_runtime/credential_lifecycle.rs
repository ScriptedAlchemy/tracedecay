use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tracedecay_domain::UserProfileId;
use tracedecay_session_memory::user_config::{ConfigSaveError, read_profile_config};
use zeroize::Zeroizing;

use super::{
    GitHubReadOnlyCredentialAuthorityOutcomeV1, GitHubReadOnlyCredentialAuthorityV1,
    GitHubReadOnlyCredentialSecretV1, GitHubReadPermissionV1,
    ProfileGitHubReadOnlyCredentialMountOutcomeV1,
    mount_profile_github_read_only_credential_authority_v1,
    register_profile_github_public_repository_v1,
    register_profile_github_read_only_credential_authority_v1,
    unmount_profile_github_read_only_credential_authority_v1,
    unregister_profile_github_public_repository_v1,
    unregister_profile_github_read_only_credential_authority_v1,
};
use crate::http_agent::http_agent;

type ProfileRepositoryCredentialKeyV1 = (UserProfileId, String, String);

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConfiguredGitHubAccessV1 {
    Public,
    OsKeyring,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfiguredGitHubRepositoryV1 {
    owner: String,
    repository: String,
    access: ConfiguredGitHubAccessV1,
    #[serde(default)]
    keyring_service: Option<String>,
    #[serde(default)]
    keyring_account: Option<String>,
}

#[derive(Default, Deserialize)]
struct ConfiguredGitHubRepositoriesV1 {
    #[serde(default)]
    github_review_sources: Vec<ConfiguredGitHubRepositoryV1>,
}

pub trait GitHubSecretReadPortV1: Send + Sync {
    fn read_secret(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<String>>, GitHubSecretReadErrorV1>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitHubSecretReadErrorV1 {
    Unavailable,
}

pub trait GitHubReadOnlyCredentialPermissionVerifierV1: Send + Sync {
    fn verify(
        &self,
        secret: &str,
        repository_owner: &str,
    ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1;
}

#[derive(Clone)]
struct GitHubProviderPermissionVerifierV1 {
    agent: ureq::Agent,
    base_uri: String,
}

impl GitHubProviderPermissionVerifierV1 {
    fn production() -> Self {
        Self {
            agent: http_agent(
                ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(20)))
                    .timeout_connect(Some(Duration::from_secs(10)))
                    .timeout_recv_response(Some(Duration::from_secs(20)))
                    .timeout_recv_body(Some(Duration::from_secs(20)))
                    .https_only(true)
                    .max_redirects(0)
                    .http_status_as_error(false)
                    .build(),
            ),
            base_uri: "https://api.github.com".to_owned(),
        }
    }

    #[cfg(test)]
    fn local(base_uri: String) -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .max_redirects(0)
                .http_status_as_error(false)
                .build()
                .into(),
            base_uri,
        }
    }

    #[hotpath::measure(label = "application.github_credential.verify")]
    fn verify(
        &self,
        secret: &str,
        repository_owner: &str,
    ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
        let authorization = Zeroizing::new(format!("Bearer {secret}"));
        let response = self
            .agent
            .get(format!(
                "{}/user/installations?per_page=100&page=1",
                self.base_uri.trim_end_matches('/')
            ))
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", authorization.as_str())
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-credential-verifier")
            .call();
        let Ok(mut response) = response else {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        };
        if response.status().as_u16() != 200 {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        }
        let Ok(body) = response
            .body_mut()
            .with_config()
            .limit(256 * 1024)
            .read_to_vec()
        else {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        };
        let Ok(envelope) = serde_json::from_slice::<GitHubInstallationsEnvelopeV1>(&body) else {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        };
        let mut matching = envelope.installations.into_iter().filter(|installation| {
            installation.account.login == repository_owner
                && installation.suspended_at.is_none()
                && installation.repository_selection == "all"
        });
        let Some(installation) = matching.next() else {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        };
        if matching.next().is_some() {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        }
        let mut exact_permissions = BTreeSet::new();
        for (permission, access) in installation.permissions {
            match access.as_str() {
                "write" | "admin" => {
                    return GitHubReadOnlyCredentialAuthorityOutcomeV1::WriteCapable;
                }
                "read" => {}
                _ => return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate,
            }
            let known = match permission.as_str() {
                "metadata" => GitHubReadPermissionV1::Metadata,
                "pull_requests" => GitHubReadPermissionV1::PullRequests,
                "contents" => GitHubReadPermissionV1::Contents,
                "actions" => GitHubReadPermissionV1::Actions,
                "checks" => GitHubReadPermissionV1::Checks,
                _ => return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate,
            };
            exact_permissions.insert(known);
        }
        if !exact_permissions.contains(&GitHubReadPermissionV1::PullRequests) {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        }
        let Some(secret) =
            GitHubReadOnlyCredentialSecretV1::from_zeroizing(Zeroizing::new(secret.to_owned()))
        else {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        };
        GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
            secret,
            exact_permissions,
        }
    }
}

impl GitHubReadOnlyCredentialPermissionVerifierV1 for GitHubProviderPermissionVerifierV1 {
    fn verify(
        &self,
        secret: &str,
        repository_owner: &str,
    ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
        self.verify(secret, repository_owner)
    }
}

#[derive(Deserialize)]
struct GitHubInstallationsEnvelopeV1 {
    installations: Vec<GitHubInstallationV1>,
}

#[derive(Deserialize)]
struct GitHubInstallationV1 {
    account: GitHubInstallationAccountV1,
    repository_selection: String,
    permissions: BTreeMap<String, String>,
    suspended_at: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GitHubInstallationAccountV1 {
    login: String,
}

struct OsKeyringGitHubReadOnlyCredentialAuthorityV1 {
    repository_owner: String,
    repository_name: String,
    keyring_service: String,
    keyring_account: String,
    secrets: Arc<dyn GitHubSecretReadPortV1>,
    verifier: Arc<dyn GitHubReadOnlyCredentialPermissionVerifierV1>,
}

impl GitHubReadOnlyCredentialAuthorityV1 for OsKeyringGitHubReadOnlyCredentialAuthorityV1 {
    fn resolve(
        &self,
        repository_owner: &str,
        repository_name: &str,
    ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
        if repository_owner != self.repository_owner || repository_name != self.repository_name {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        }
        let secret = match self
            .secrets
            .read_secret(&self.keyring_service, &self.keyring_account)
        {
            Ok(Some(secret)) => secret,
            Ok(None) => return GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured,
            Err(GitHubSecretReadErrorV1::Unavailable) => {
                return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
            }
        };
        if GitHubReadOnlyCredentialSecretV1::from_zeroizing(secret.clone()).is_none() {
            return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
        }
        self.verifier.verify(secret.as_str(), repository_owner)
    }
}

enum ProfileRepositoryCredentialRegistrationV1 {
    Public(ProfileRepositoryCredentialKeyV1),
    Private {
        key: ProfileRepositoryCredentialKeyV1,
        authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
    },
}

#[derive(Clone)]
pub struct GitHubReadOnlyCredentialLifecycleV1 {
    registrations: Arc<Mutex<Vec<ProfileRepositoryCredentialRegistrationV1>>>,
    mounts: Arc<Mutex<BTreeSet<ProfileRepositoryCredentialKeyV1>>>,
    verifier: Arc<dyn GitHubReadOnlyCredentialPermissionVerifierV1>,
}

impl Default for GitHubReadOnlyCredentialLifecycleV1 {
    fn default() -> Self {
        Self::with_permission_verifier(Arc::new(GitHubProviderPermissionVerifierV1::production()))
    }
}

impl GitHubReadOnlyCredentialLifecycleV1 {
    #[must_use]
    pub fn with_permission_verifier(
        verifier: Arc<dyn GitHubReadOnlyCredentialPermissionVerifierV1>,
    ) -> Self {
        Self {
            registrations: Arc::default(),
            mounts: Arc::default(),
            verifier,
        }
    }

    pub fn mount(
        &self,
        profile_id: &UserProfileId,
        repository_owner: &str,
        repository_name: &str,
    ) -> ProfileGitHubReadOnlyCredentialMountOutcomeV1 {
        let Ok(mut mounts) = self.mounts.lock() else {
            return ProfileGitHubReadOnlyCredentialMountOutcomeV1::Rejected;
        };
        let outcome = mount_profile_github_read_only_credential_authority_v1(
            profile_id,
            repository_owner,
            repository_name,
        );
        if outcome == ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted {
            mounts.insert((
                profile_id.clone(),
                repository_owner.to_owned(),
                repository_name.to_owned(),
            ));
        }
        outcome
    }

    pub fn configure_profile(
        &self,
        profile_id: &UserProfileId,
        profile_root: &Path,
        secrets: Arc<dyn GitHubSecretReadPortV1>,
    ) {
        self.configure_profile_with(
            profile_id,
            profile_root,
            secrets,
            Arc::clone(&self.verifier),
        );
    }

    #[hotpath::measure(label = "application.github_credential.configure")]
    fn configure_profile_with(
        &self,
        profile_id: &UserProfileId,
        profile_root: &Path,
        secrets: Arc<dyn GitHubSecretReadPortV1>,
        verifier: Arc<dyn GitHubReadOnlyCredentialPermissionVerifierV1>,
    ) {
        let sources = match read_profile_config::<ConfiguredGitHubRepositoriesV1>(profile_root) {
            Ok(configured) => review_sources(configured).0,
            Err(error) => {
                tracing::warn!(
                    event = "github_review_sources_unreadable",
                    %error,
                    "no GitHub review source is registered for this profile"
                );
                return;
            }
        };
        let Ok(mut registrations) = self.registrations.lock() else {
            return;
        };
        for source in sources {
            match source {
                RegistrableGitHubReviewSourceV1::Public { owner, repository } => {
                    let key = (profile_id.clone(), owner, repository);
                    if register_profile_github_public_repository_v1(
                        key.0.clone(),
                        key.1.clone(),
                        key.2.clone(),
                    ) {
                        registrations.push(ProfileRepositoryCredentialRegistrationV1::Public(key));
                    }
                }
                RegistrableGitHubReviewSourceV1::OsKeyring {
                    owner,
                    repository,
                    keyring_service,
                    keyring_account,
                } => {
                    let key = (profile_id.clone(), owner, repository);
                    let authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> =
                        Arc::new(OsKeyringGitHubReadOnlyCredentialAuthorityV1 {
                            repository_owner: key.1.clone(),
                            repository_name: key.2.clone(),
                            keyring_service,
                            keyring_account,
                            secrets: Arc::clone(&secrets),
                            verifier: Arc::clone(&verifier),
                        });
                    if register_profile_github_read_only_credential_authority_v1(
                        key.0.clone(),
                        key.1.clone(),
                        key.2.clone(),
                        &authority,
                    ) {
                        registrations.push(ProfileRepositoryCredentialRegistrationV1::Private {
                            key,
                            authority,
                        });
                    }
                }
            }
        }
    }

    pub fn shutdown(&self) {
        let mounts = match self.mounts.lock() {
            Ok(mut mounts) => std::mem::take(&mut *mounts),
            Err(_) => return,
        };
        for (profile_id, repository_owner, repository_name) in mounts {
            let _ = unmount_profile_github_read_only_credential_authority_v1(
                &profile_id,
                &repository_owner,
                &repository_name,
            );
        }
        let registrations = match self.registrations.lock() {
            Ok(mut registrations) => std::mem::take(&mut *registrations),
            Err(_) => return,
        };
        for registration in registrations {
            match registration {
                ProfileRepositoryCredentialRegistrationV1::Public((
                    profile_id,
                    repository_owner,
                    repository_name,
                )) => {
                    let _ = unregister_profile_github_public_repository_v1(
                        &profile_id,
                        &repository_owner,
                        &repository_name,
                    );
                }
                ProfileRepositoryCredentialRegistrationV1::Private { key, authority } => {
                    let _ = unregister_profile_github_read_only_credential_authority_v1(
                        &key.0, &key.1, &key.2, &authority,
                    );
                }
            }
        }
    }
}

/// A configured `github_review_sources` entry the daemon does not register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubReviewSourceFindingV1 {
    /// Listed more than once, so none of its entries registers.
    Duplicate { owner: String, repository: String },
    /// `os_keyring` access without both `keyring_service` and `keyring_account`.
    MissingKeyringLocator { owner: String, repository: String },
    /// A keyring locator that is empty, padded, over 512 bytes, or carries a
    /// control character.
    InvalidKeyringLocator { owner: String, repository: String },
    /// `public` access that also names a keyring locator.
    PublicWithKeyringLocator { owner: String, repository: String },
}

impl std::fmt::Display for GitHubReviewSourceFindingV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Duplicate { owner, repository } => {
                write!(formatter, "{owner}/{repository} is listed more than once")
            }
            Self::MissingKeyringLocator { owner, repository } => write!(
                formatter,
                "{owner}/{repository} uses os_keyring access without keyring_service and \
                 keyring_account"
            ),
            Self::InvalidKeyringLocator { owner, repository } => write!(
                formatter,
                "{owner}/{repository} names a keyring locator that is empty, padded, over 512 \
                 bytes or carries a control character"
            ),
            Self::PublicWithKeyringLocator { owner, repository } => write!(
                formatter,
                "{owner}/{repository} uses public access but names a keyring locator"
            ),
        }
    }
}

enum RegistrableGitHubReviewSourceV1 {
    Public {
        owner: String,
        repository: String,
    },
    OsKeyring {
        owner: String,
        repository: String,
        keyring_service: String,
        keyring_account: String,
    },
}

/// Splits the configured review sources into the ones the daemon registers
/// and a finding for each entry it does not.
fn review_sources(
    configured: ConfiguredGitHubRepositoriesV1,
) -> (
    Vec<RegistrableGitHubReviewSourceV1>,
    Vec<GitHubReviewSourceFindingV1>,
) {
    let mut findings = Vec::new();
    let mut repositories = BTreeMap::new();
    for repository in configured.github_review_sources {
        match repositories.entry((repository.owner.clone(), repository.repository.clone())) {
            Entry::Vacant(entry) => {
                entry.insert(Some(repository));
            }
            Entry::Occupied(mut entry) => {
                if entry.insert(None).is_some() {
                    findings.push(GitHubReviewSourceFindingV1::Duplicate {
                        owner: repository.owner,
                        repository: repository.repository,
                    });
                }
            }
        }
    }
    let mut sources = Vec::new();
    for ConfiguredGitHubRepositoryV1 {
        owner,
        repository,
        access,
        keyring_service,
        keyring_account,
    } in repositories.into_values().flatten()
    {
        match (access, keyring_service, keyring_account) {
            (ConfiguredGitHubAccessV1::Public, None, None) => {
                sources.push(RegistrableGitHubReviewSourceV1::Public { owner, repository });
            }
            (ConfiguredGitHubAccessV1::Public, _, _) => {
                findings.push(GitHubReviewSourceFindingV1::PublicWithKeyringLocator {
                    owner,
                    repository,
                });
            }
            (ConfiguredGitHubAccessV1::OsKeyring, Some(keyring_service), Some(keyring_account))
                if valid_locator(&keyring_service) && valid_locator(&keyring_account) =>
            {
                sources.push(RegistrableGitHubReviewSourceV1::OsKeyring {
                    owner,
                    repository,
                    keyring_service,
                    keyring_account,
                });
            }
            (ConfiguredGitHubAccessV1::OsKeyring, Some(_), Some(_)) => {
                findings
                    .push(GitHubReviewSourceFindingV1::InvalidKeyringLocator { owner, repository });
            }
            (ConfiguredGitHubAccessV1::OsKeyring, _, _) => {
                findings
                    .push(GitHubReviewSourceFindingV1::MissingKeyringLocator { owner, repository });
            }
        }
    }
    (sources, findings)
}

/// The configured review sources the daemon does not register, read the way
/// it registers them. An error means it registered none.
pub fn check_configured_github_review_sources_v1(
    profile_root: &Path,
) -> Result<Vec<GitHubReviewSourceFindingV1>, ConfigSaveError> {
    read_profile_config::<ConfiguredGitHubRepositoriesV1>(profile_root)
        .map(|configured| review_sources(configured).1)
}

fn valid_locator(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;

    use tracedecay_domain::UserProfileId;
    use zeroize::Zeroizing;

    use super::{
        GitHubProviderPermissionVerifierV1, GitHubReadOnlyCredentialLifecycleV1,
        GitHubReviewSourceFindingV1, GitHubSecretReadErrorV1, GitHubSecretReadPortV1,
        check_configured_github_review_sources_v1,
    };
    use crate::advisory::github_runtime::{
        GitHubReadPermissionV1, ProfileGitHubReadOnlyCredentialMountOutcomeV1,
        RegisteredGitHubReadOnlyCredentialV1,
        mount_profile_github_read_only_credential_authority_v1,
        resolve_registered_github_read_only_credential_v1,
    };

    struct FakeOsSecrets {
        by_account: BTreeMap<String, String>,
    }

    impl GitHubSecretReadPortV1 for FakeOsSecrets {
        fn read_secret(
            &self,
            _service: &str,
            account: &str,
        ) -> Result<Option<Zeroizing<String>>, GitHubSecretReadErrorV1> {
            Ok(self.by_account.get(account).cloned().map(Zeroizing::new))
        }
    }

    #[test]
    fn production_profile_keyring_configuration_verifies_permissions_and_fails_closed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let profile_root = temporary.path().join("profile");
        std::fs::create_dir(&profile_root).expect("profile directory");
        let profile_id = UserProfileId::new("profile.github.lifecycle").expect("profile id");
        std::fs::write(
            profile_root.join("config.toml"),
            r#"
[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "keyring-read"
access = "os_keyring"
keyring_service = "tracedecay.github"
keyring_account = "read"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "keyring-write"
access = "os_keyring"
keyring_service = "tracedecay.github"
keyring_account = "write"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "keyring-indeterminate"
access = "os_keyring"
keyring_service = "tracedecay.github"
keyring_account = "indeterminate"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "keyring-selected"
access = "os_keyring"
keyring_service = "tracedecay.github"
keyring_account = "selected"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "keyring-missing"
access = "os_keyring"
keyring_service = "tracedecay.github"
keyring_account = "missing"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "explicit-public"
access = "public"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "duplicate"
access = "public"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "duplicate"
access = "os_keyring"
keyring_service = "tracedecay.github"
keyring_account = "read"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "public-with-locator"
access = "public"
keyring_service = "tracedecay.github"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "keyring-unnamed"
access = "os_keyring"

[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "keyring-padded"
access = "os_keyring"
keyring_service = " tracedecay.github"
keyring_account = "read"
"#,
        )
        .expect("profile configuration");
        let secrets: Arc<dyn GitHubSecretReadPortV1> = Arc::new(FakeOsSecrets {
            by_account: BTreeMap::from([
                ("read".to_owned(), "github-token-read".to_owned()),
                ("write".to_owned(), "github-token-write".to_owned()),
                (
                    "indeterminate".to_owned(),
                    "github-token-indeterminate".to_owned(),
                ),
                ("selected".to_owned(), "github-token-selected".to_owned()),
            ]),
        });
        let listener = TcpListener::bind("127.0.0.1:0").expect("permission verifier listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().expect("permission request");
                let headers = read_headers(&mut stream).to_ascii_lowercase();
                let repository_selection = if headers.contains("bearer github-token-selected") {
                    "selected"
                } else {
                    "all"
                };
                let permissions = if headers.contains("bearer github-token-read") {
                    serde_json::json!({
                        "metadata": "read",
                        "pull_requests": "read",
                        "contents": "read"
                    })
                } else if headers.contains("bearer github-token-write") {
                    serde_json::json!({
                        "metadata": "read",
                        "pull_requests": "write"
                    })
                } else {
                    serde_json::json!({
                        "metadata": "read",
                        "pull_requests": "read",
                        "mystery_permission": "read"
                    })
                };
                write_json(
                    &mut stream,
                    &serde_json::json!({
                        "installations": [{
                            "account": { "login": "ScriptedAlchemy" },
                            "repository_selection": repository_selection,
                            "permissions": permissions,
                            "suspended_at": null
                        }]
                    }),
                );
            }
        });

        let lifecycle = GitHubReadOnlyCredentialLifecycleV1::default();
        lifecycle.configure_profile_with(
            &profile_id,
            &profile_root,
            secrets,
            Arc::new(GitHubProviderPermissionVerifierV1::local(format!(
                "http://{address}"
            ))),
        );
        assert_eq!(
            mount_profile_github_read_only_credential_authority_v1(
                &profile_id,
                "ScriptedAlchemy",
                "keyring-read",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted
        );
        let RegisteredGitHubReadOnlyCredentialV1::Verified(read) =
            resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", "keyring-read")
        else {
            panic!("provider-verified read-only keyring credential must resolve");
        };
        assert!(read.permits(GitHubReadPermissionV1::PullRequests));
        assert!(!read.permits(GitHubReadPermissionV1::Actions));

        for repository in [
            "keyring-write",
            "keyring-indeterminate",
            "keyring-selected",
            "keyring-missing",
        ] {
            assert_eq!(
                mount_profile_github_read_only_credential_authority_v1(
                    &profile_id,
                    "ScriptedAlchemy",
                    repository,
                ),
                ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted
            );
            assert!(matches!(
                resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", repository,),
                RegisteredGitHubReadOnlyCredentialV1::Rejected
            ));
        }
        assert_eq!(
            mount_profile_github_read_only_credential_authority_v1(
                &profile_id,
                "ScriptedAlchemy",
                "explicit-public",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::Public
        );
        assert_eq!(
            mount_profile_github_read_only_credential_authority_v1(
                &profile_id,
                "ScriptedAlchemy",
                "unconfigured-public",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured
        );
        assert_eq!(
            mount_profile_github_read_only_credential_authority_v1(
                &profile_id,
                "ScriptedAlchemy",
                "duplicate",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured
        );
        for repository in ["public-with-locator", "keyring-unnamed", "keyring-padded"] {
            assert_eq!(
                mount_profile_github_read_only_credential_authority_v1(
                    &profile_id,
                    "ScriptedAlchemy",
                    repository,
                ),
                ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured,
                "{repository}"
            );
        }
        let owner = || "ScriptedAlchemy".to_owned();
        assert_eq!(
            check_configured_github_review_sources_v1(&profile_root)
                .expect("the profile configuration parses"),
            [
                GitHubReviewSourceFindingV1::Duplicate {
                    owner: owner(),
                    repository: "duplicate".to_owned(),
                },
                GitHubReviewSourceFindingV1::InvalidKeyringLocator {
                    owner: owner(),
                    repository: "keyring-padded".to_owned(),
                },
                GitHubReviewSourceFindingV1::MissingKeyringLocator {
                    owner: owner(),
                    repository: "keyring-unnamed".to_owned(),
                },
                GitHubReviewSourceFindingV1::PublicWithKeyringLocator {
                    owner: owner(),
                    repository: "public-with-locator".to_owned(),
                },
            ]
        );
        server.join().expect("permission verifier server");

        lifecycle.shutdown();
        assert!(matches!(
            resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", "keyring-read",),
            RegisteredGitHubReadOnlyCredentialV1::Missing
        ));
        assert_eq!(
            mount_profile_github_read_only_credential_authority_v1(
                &profile_id,
                "ScriptedAlchemy",
                "explicit-public",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured
        );
    }

    fn read_headers(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 2048];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("read request");
            assert!(read > 0, "client closed before headers");
            bytes.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(bytes).expect("UTF-8 headers")
    }

    fn write_json(stream: &mut TcpStream, value: &serde_json::Value) {
        let body = serde_json::to_vec(value).expect("JSON response");
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .expect("write response headers");
        stream.write_all(&body).expect("write response body");
    }
}
