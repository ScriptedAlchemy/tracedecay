use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitHubReadPermissionV1 {
    Metadata,
    PullRequests,
    Contents,
    Actions,
    Checks,
}

impl GitHubReadPermissionV1 {
    pub fn parse(scope: &str) -> Option<Self> {
        match scope {
            "metadata:read" => Some(Self::Metadata),
            "pull_requests:read" => Some(Self::PullRequests),
            "contents:read" => Some(Self::Contents),
            "actions:read" => Some(Self::Actions),
            "checks:read" => Some(Self::Checks),
            _ => None,
        }
    }
}

/// Secret token material returned only by a trusted credential authority.
///
/// This type intentionally implements neither `Debug` nor Serde traits.
#[derive(Clone)]
pub struct GitHubReadOnlyCredentialSecretV1(Zeroizing<String>);

impl GitHubReadOnlyCredentialSecretV1 {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.is_empty()
            && value.len() <= 4096
            && !value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace()))
        .then(|| Self(Zeroizing::new(value)))
    }

    fn authorization_header(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("Bearer {}", self.0.as_str()))
    }

    pub fn from_zeroizing(value: Zeroizing<String>) -> Option<Self> {
        (!value.is_empty()
            && value.len() <= 4096
            && !value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace()))
        .then_some(Self(value))
    }
}

/// Result supplied by an authority that has already verified the provider's
/// effective permissions. `TraceDecay` never treats local scope labels as proof.
pub enum GitHubReadOnlyCredentialAuthorityOutcomeV1 {
    Verified {
        secret: GitHubReadOnlyCredentialSecretV1,
        exact_permissions: BTreeSet<GitHubReadPermissionV1>,
    },
    NotConfigured,
    WriteCapable,
    Indeterminate,
}

/// Trusted boundary for private GitHub credentials.
///
/// Implementations must establish effective provider permissions before
/// returning `Verified`; user-declared or environment-declared scope strings
/// are not sufficient evidence.
pub trait GitHubReadOnlyCredentialAuthorityV1: Send + Sync {
    fn resolve(
        &self,
        repository_owner: &str,
        repository_name: &str,
    ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1;
}

pub(super) struct RegisteredGitHubReadOnlyCredentialAuthorityV1 {
    authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
    active: Arc<AtomicBool>,
    generation: u64,
}

pub(super) enum ProfileGitHubReadOnlyCredentialAuthorityV1 {
    Public,
    Private {
        authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
    },
}

pub(super) type ProfileGitHubReadOnlyCredentialAuthorityMapV1 =
    BTreeMap<(UserProfileId, String, String), ProfileGitHubReadOnlyCredentialAuthorityV1>;
pub(super) type ProfileGitHubReadOnlyCredentialAuthoritiesLockV1 =
    Mutex<ProfileGitHubReadOnlyCredentialAuthorityMapV1>;
pub(super) type RegisteredGitHubReadOnlyCredentialAuthorityMapV1 =
    BTreeMap<(String, String), RegisteredGitHubReadOnlyCredentialAuthorityV1>;
pub(super) type RegisteredGitHubReadOnlyCredentialAuthoritiesLockV1 =
    Mutex<RegisteredGitHubReadOnlyCredentialAuthorityMapV1>;

pub(super) fn profile_github_read_only_credential_authorities()
-> &'static ProfileGitHubReadOnlyCredentialAuthoritiesLockV1 {
    static AUTHORITIES: OnceLock<ProfileGitHubReadOnlyCredentialAuthoritiesLockV1> =
        OnceLock::new();
    AUTHORITIES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn registered_github_read_only_credential_authorities()
-> &'static RegisteredGitHubReadOnlyCredentialAuthoritiesLockV1 {
    static AUTHORITIES: OnceLock<RegisteredGitHubReadOnlyCredentialAuthoritiesLockV1> =
        OnceLock::new();
    AUTHORITIES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn registered_github_read_only_credential_generation_matches_v1(
    repository_owner: &str,
    repository_name: &str,
    authority: &Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
    generation: u64,
) -> bool {
    let Ok(authorities) = registered_github_read_only_credential_authorities().lock() else {
        return false;
    };
    authorities
        .get(&(repository_owner.to_owned(), repository_name.to_owned()))
        .is_some_and(|registered| {
            Arc::ptr_eq(&registered.authority, authority) && registered.generation == generation
        })
}

pub(super) fn next_github_credential_generation_v1() -> u64 {
    static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// Registers one retained, exact-repository credential authority.
///
/// Live conflicting authorities are rejected. The application registry
/// retains the authority until exact explicit unregistration.
pub fn register_github_read_only_credential_authority_v1(
    repository_owner: impl Into<String>,
    repository_name: impl Into<String>,
    authority: &Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
) -> bool {
    let repository_owner = repository_owner.into();
    let repository_name = repository_name.into();
    if !valid_path_segment(&repository_owner) || !valid_path_segment(&repository_name) {
        return false;
    }
    let Ok(mut authorities) = registered_github_read_only_credential_authorities().lock() else {
        return false;
    };
    let key = (repository_owner, repository_name);
    if let Some(existing) = authorities.get(&key) {
        return Arc::ptr_eq(&existing.authority, authority);
    }
    authorities.insert(
        key,
        RegisteredGitHubReadOnlyCredentialAuthorityV1 {
            authority: Arc::clone(authority),
            active: Arc::new(AtomicBool::new(true)),
            generation: next_github_credential_generation_v1(),
        },
    );
    true
}

/// Installs one process-local credential authority for an exact daemon profile
/// and repository.
///
/// The authority remains the only owner of secret material. This boundary
/// stores no token bytes and is intentionally separate from durable,
/// redacted configuration metadata.
pub fn register_profile_github_read_only_credential_authority_v1(
    profile_id: UserProfileId,
    repository_owner: impl Into<String>,
    repository_name: impl Into<String>,
    authority: &Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
) -> bool {
    let repository_owner = repository_owner.into();
    let repository_name = repository_name.into();
    if profile_id.validate().is_err()
        || !valid_path_segment(&repository_owner)
        || !valid_path_segment(&repository_name)
    {
        return false;
    }
    let Ok(mut authorities) = profile_github_read_only_credential_authorities().lock() else {
        return false;
    };
    let key = (profile_id, repository_owner, repository_name);
    if let Some(existing) = authorities.get(&key) {
        return matches!(
            existing,
            ProfileGitHubReadOnlyCredentialAuthorityV1::Private {
                authority: existing,
            } if Arc::ptr_eq(existing, authority)
        );
    }
    authorities.insert(
        key,
        ProfileGitHubReadOnlyCredentialAuthorityV1::Private {
            authority: Arc::clone(authority),
        },
    );
    true
}

pub fn register_profile_github_public_repository_v1(
    profile_id: UserProfileId,
    repository_owner: impl Into<String>,
    repository_name: impl Into<String>,
) -> bool {
    let repository_owner = repository_owner.into();
    let repository_name = repository_name.into();
    if profile_id.validate().is_err()
        || !valid_path_segment(&repository_owner)
        || !valid_path_segment(&repository_name)
    {
        return false;
    }
    let Ok(mut authorities) = profile_github_read_only_credential_authorities().lock() else {
        return false;
    };
    let key = (profile_id, repository_owner, repository_name);
    if let Some(existing) = authorities.get(&key) {
        return matches!(existing, ProfileGitHubReadOnlyCredentialAuthorityV1::Public);
    }
    authorities.insert(key, ProfileGitHubReadOnlyCredentialAuthorityV1::Public);
    true
}

pub fn unregister_profile_github_public_repository_v1(
    profile_id: &UserProfileId,
    repository_owner: &str,
    repository_name: &str,
) -> bool {
    let Ok(mut authorities) = profile_github_read_only_credential_authorities().lock() else {
        return false;
    };
    let key = (
        profile_id.clone(),
        repository_owner.to_owned(),
        repository_name.to_owned(),
    );
    if !matches!(
        authorities.get(&key),
        Some(ProfileGitHubReadOnlyCredentialAuthorityV1::Public)
    ) {
        return false;
    }
    authorities.remove(&key).is_some()
}

/// Removes the exact process-local profile credential authority and revokes
/// its mounted application credential, if any.
pub fn unregister_profile_github_read_only_credential_authority_v1(
    profile_id: &UserProfileId,
    repository_owner: &str,
    repository_name: &str,
    authority: &Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
) -> bool {
    let Ok(mut authorities) = profile_github_read_only_credential_authorities().lock() else {
        return false;
    };
    let key = (
        profile_id.clone(),
        repository_owner.to_owned(),
        repository_name.to_owned(),
    );
    if !matches!(
        authorities.get(&key),
        Some(ProfileGitHubReadOnlyCredentialAuthorityV1::Private {
            authority: existing,
        }) if Arc::ptr_eq(existing, authority)
    ) {
        return false;
    }
    let removed = authorities.remove(&key).is_some();
    drop(authorities);
    let _ = unregister_github_read_only_credential_authority_v1(
        repository_owner,
        repository_name,
        authority,
    );
    removed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileGitHubReadOnlyCredentialMountOutcomeV1 {
    Mounted,
    Public,
    NotConfigured,
    Rejected,
}

/// Mounts only the credential configured for the exact active daemon profile.
///
/// Wrong-profile and missing configuration never fall back to another
/// process-local authority. Conflicting live application mounts fail closed.
pub fn mount_profile_github_read_only_credential_authority_v1(
    profile_id: &UserProfileId,
    repository_owner: &str,
    repository_name: &str,
) -> ProfileGitHubReadOnlyCredentialMountOutcomeV1 {
    if profile_id.validate().is_err()
        || !valid_path_segment(repository_owner)
        || !valid_path_segment(repository_name)
    {
        return ProfileGitHubReadOnlyCredentialMountOutcomeV1::Rejected;
    }
    let configured = match profile_github_read_only_credential_authorities().lock() {
        Ok(authorities) => authorities
            .get(&(
                profile_id.clone(),
                repository_owner.to_owned(),
                repository_name.to_owned(),
            ))
            .map(|configured| match configured {
                ProfileGitHubReadOnlyCredentialAuthorityV1::Public => None,
                ProfileGitHubReadOnlyCredentialAuthorityV1::Private { authority } => {
                    Some(Arc::clone(authority))
                }
            }),
        Err(_) => return ProfileGitHubReadOnlyCredentialMountOutcomeV1::Rejected,
    };
    let Some(configured) = configured else {
        return ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured;
    };
    let Some(authority) = configured else {
        return ProfileGitHubReadOnlyCredentialMountOutcomeV1::Public;
    };
    if register_github_read_only_credential_authority_v1(
        repository_owner,
        repository_name,
        &authority,
    ) {
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted
    } else {
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Rejected
    }
}

/// Revokes the mounted application credential for one exact profile and
/// repository without removing the injected profile authority.
pub fn unmount_profile_github_read_only_credential_authority_v1(
    profile_id: &UserProfileId,
    repository_owner: &str,
    repository_name: &str,
) -> bool {
    if profile_id.validate().is_err()
        || !valid_path_segment(repository_owner)
        || !valid_path_segment(repository_name)
    {
        return false;
    }
    let authority = match profile_github_read_only_credential_authorities().lock() {
        Ok(authorities) => authorities
            .get(&(
                profile_id.clone(),
                repository_owner.to_owned(),
                repository_name.to_owned(),
            ))
            .and_then(|configured| match configured {
                ProfileGitHubReadOnlyCredentialAuthorityV1::Public => None,
                ProfileGitHubReadOnlyCredentialAuthorityV1::Private { authority } => {
                    Some(Arc::clone(authority))
                }
            }),
        Err(_) => return false,
    };
    authority.is_some_and(|authority| {
        unregister_github_read_only_credential_authority_v1(
            repository_owner,
            repository_name,
            &authority,
        )
    })
}

/// Removes only the exact authority previously registered for this repository.
pub fn unregister_github_read_only_credential_authority_v1(
    repository_owner: &str,
    repository_name: &str,
    authority: &Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
) -> bool {
    let Ok(mut authorities) = registered_github_read_only_credential_authorities().lock() else {
        return false;
    };
    let key = (repository_owner.to_owned(), repository_name.to_owned());
    if authorities
        .get(&key)
        .is_none_or(|existing| !Arc::ptr_eq(&existing.authority, authority))
    {
        return false;
    }
    let Some(removed) = authorities.remove(&key) else {
        return false;
    };
    removed.active.store(false, Ordering::Release);
    true
}

pub enum RegisteredGitHubReadOnlyCredentialV1 {
    Verified(GitHubReadOnlyCredentialV1),
    Missing,
    Rejected,
}

pub fn resolve_registered_github_read_only_credential_v1(
    repository_owner: &str,
    repository_name: &str,
) -> RegisteredGitHubReadOnlyCredentialV1 {
    if !valid_path_segment(repository_owner) || !valid_path_segment(repository_name) {
        return RegisteredGitHubReadOnlyCredentialV1::Rejected;
    }
    let registered = match registered_github_read_only_credential_authorities().lock() {
        Ok(authorities) => authorities
            .get(&(repository_owner.to_owned(), repository_name.to_owned()))
            .map(|registered| {
                (
                    Arc::clone(&registered.authority),
                    Arc::clone(&registered.active),
                    registered.generation,
                )
            }),
        Err(_) => return RegisteredGitHubReadOnlyCredentialV1::Rejected,
    };
    let Some((authority, active, generation)) = registered else {
        return RegisteredGitHubReadOnlyCredentialV1::Missing;
    };
    match authority.resolve(repository_owner, repository_name) {
        GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
            secret,
            exact_permissions,
        } => {
            drop(secret);
            GitHubReadOnlyCredentialV1::verified_private(
                authority,
                repository_owner.to_owned(),
                repository_name.to_owned(),
                exact_permissions,
                active,
                generation,
            )
            .map_or(
                RegisteredGitHubReadOnlyCredentialV1::Rejected,
                RegisteredGitHubReadOnlyCredentialV1::Verified,
            )
        }
        GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured
        | GitHubReadOnlyCredentialAuthorityOutcomeV1::WriteCapable
        | GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate => {
            RegisteredGitHubReadOnlyCredentialV1::Rejected
        }
    }
}

#[derive(Clone)]
pub(super) enum GitHubReadOnlyCredentialKindV1 {
    Anonymous,
    VerifiedPrivate {
        authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
        repository_owner: String,
        repository_name: String,
        active: Arc<AtomicBool>,
        generation: u64,
    },
}

pub(super) enum GitHubCredentialAuthorizationV1 {
    Anonymous,
    Private(Zeroizing<String>),
    Denied,
}

#[derive(Clone)]
pub struct GitHubReadOnlyCredentialV1 {
    kind: GitHubReadOnlyCredentialKindV1,
}

impl GitHubReadOnlyCredentialV1 {
    pub fn anonymous() -> Self {
        Self {
            kind: GitHubReadOnlyCredentialKindV1::Anonymous,
        }
    }

    pub(super) fn verified_private(
        authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
        repository_owner: String,
        repository_name: String,
        exact_permissions: BTreeSet<GitHubReadPermissionV1>,
        active: Arc<AtomicBool>,
        generation: u64,
    ) -> Option<Self> {
        (valid_path_segment(&repository_owner)
            && valid_path_segment(&repository_name)
            && !exact_permissions.is_empty()
            && active.load(Ordering::Acquire))
        .then_some(Self {
            kind: GitHubReadOnlyCredentialKindV1::VerifiedPrivate {
                authority,
                repository_owner,
                repository_name,
                active,
                generation,
            },
        })
    }

    /// Opaque daemon-generation identity for the mounted credential authority.
    ///
    /// The value never contains credential bytes. A remount receives a fresh
    /// generation even when it targets the same repository.
    pub(crate) fn generation(&self) -> u64 {
        match &self.kind {
            GitHubReadOnlyCredentialKindV1::Anonymous => 0,
            GitHubReadOnlyCredentialKindV1::VerifiedPrivate { generation, .. } => *generation,
        }
    }

    pub fn permits(&self, permission: GitHubReadPermissionV1) -> bool {
        !matches!(
            self.authorization_for_stored_repository(permission),
            GitHubCredentialAuthorizationV1::Denied
        )
    }

    pub(super) fn authorization_for_target(
        &self,
        target: &GitHubRepositoryTargetV1,
        permission: GitHubReadPermissionV1,
    ) -> GitHubCredentialAuthorizationV1 {
        self.authorization_for_repository(&target.owner, &target.repository, permission)
    }

    pub(super) fn authorization_for_repository(
        &self,
        owner: &str,
        repository: &str,
        permission: GitHubReadPermissionV1,
    ) -> GitHubCredentialAuthorizationV1 {
        match &self.kind {
            GitHubReadOnlyCredentialKindV1::Anonymous => GitHubCredentialAuthorizationV1::Anonymous,
            GitHubReadOnlyCredentialKindV1::VerifiedPrivate {
                repository_owner,
                repository_name,
                ..
            } if repository_owner != owner || repository_name != repository => {
                GitHubCredentialAuthorizationV1::Denied
            }
            GitHubReadOnlyCredentialKindV1::VerifiedPrivate { .. } => {
                self.authorization_for_stored_repository(permission)
            }
        }
    }

    pub(super) fn authorization_for_stored_repository(
        &self,
        permission: GitHubReadPermissionV1,
    ) -> GitHubCredentialAuthorizationV1 {
        let mounted_generation = self.generation();
        match &self.kind {
            GitHubReadOnlyCredentialKindV1::Anonymous => GitHubCredentialAuthorizationV1::Anonymous,
            GitHubReadOnlyCredentialKindV1::VerifiedPrivate {
                authority,
                repository_owner,
                repository_name,
                active,
                ..
            } if active.load(Ordering::Acquire)
                && registered_github_read_only_credential_generation_matches_v1(
                    repository_owner,
                    repository_name,
                    authority,
                    mounted_generation,
                ) =>
            {
                match authority.resolve(repository_owner, repository_name) {
                    GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                        secret,
                        exact_permissions,
                    } if exact_permissions.contains(&permission) => {
                        GitHubCredentialAuthorizationV1::Private(secret.authorization_header())
                    }
                    GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified { .. }
                    | GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured
                    | GitHubReadOnlyCredentialAuthorityOutcomeV1::WriteCapable
                    | GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate => {
                        GitHubCredentialAuthorizationV1::Denied
                    }
                }
            }
            GitHubReadOnlyCredentialKindV1::VerifiedPrivate { .. } => {
                GitHubCredentialAuthorizationV1::Denied
            }
        }
    }

    pub(in crate::advisory::github_runtime) fn authorization_header_for(
        &self,
        permission: GitHubReadPermissionV1,
    ) -> Result<Option<Zeroizing<String>>, ()> {
        match self.authorization_for_stored_repository(permission) {
            GitHubCredentialAuthorizationV1::Private(header) => Ok(Some(header)),
            GitHubCredentialAuthorizationV1::Anonymous => Ok(None),
            GitHubCredentialAuthorizationV1::Denied => Err(()),
        }
    }
}
