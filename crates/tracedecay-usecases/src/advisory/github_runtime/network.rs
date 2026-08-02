use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::json;
use tracedecay_application::feedback::FeedbackPortFuture;
#[cfg(test)]
use tracedecay_application::feedback::GitHubReviewReadRequestV1;
use tracedecay_application::{RequestAdmission, RequestContext, now_micros};
use tracedecay_domain::feedback::{
    GitHubReviewCursorV1, GitHubReviewEtagV1, GitHubReviewRateLimitCheckpointV1,
    GitHubReviewReadOperationV1,
};
use tracedecay_domain::{UserProfileId, UtcMicros};
use url::Url;
use zeroize::Zeroizing;

use super::dto::{
    GraphQlCommentPageNodeV1, GraphQlResponseV1, RestPullRequestV1, RestReviewCommentV1,
    RestReviewV1,
};
use super::{
    GitHubGraphQlReadRequestV1, GitHubReadNetworkMetadataV1, GitHubReadNetworkOutcomeV1,
    GitHubReadNetworkResponseV1, GitHubReadNetworkStatusV1, GitHubReadOnlyNetworkAuthorityV1,
    GitHubRestReadRequestV1, MAX_GITHUB_READ_RESPONSE_BYTES_V1,
};

pub const GITHUB_REVIEW_THREADS_QUERY_V1: &str = r"
query TraceDecayPR13ReviewThreads(
  $owner: String!
  $repository: String!
  $number: Int!
  $threadAfter: String
  $commentThreadId: ID!
  $commentAfter: String
  $loadThreads: Boolean!
  $loadComments: Boolean!
) {
  repository(owner: $owner, name: $repository) @include(if: $loadThreads) {
    pullRequest(number: $number) {
      baseRefOid
      headRefOid
      reviewThreads(first: 100, after: $threadAfter) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path line originalLine startLine originalStartLine
          comments(first: 100) {
            pageInfo { hasNextPage endCursor }
            nodes {
              databaseId url bodyText createdAt updatedAt authorAssociation
              replyTo { databaseId }
              author { __typename login }
              pullRequestReview { databaseId state commit { oid } }
              originalCommit { oid }
            }
          }
        }
      }
    }
  }
  node(id: $commentThreadId) @include(if: $loadComments) {
    ... on PullRequestReviewThread {
      id
      comments(first: 100, after: $commentAfter) {
        pageInfo { hasNextPage endCursor }
        nodes {
          databaseId url bodyText createdAt updatedAt authorAssociation
          replyTo { databaseId }
          author { __typename login }
          pullRequestReview { databaseId state commit { oid } }
          originalCommit { oid }
        }
      }
    }
  }
}
";

const MAX_REVIEW_ITEMS_V1: usize = 2_000;
const MAX_NESTED_COMMENT_PAGES_V1: usize = 20;
const MAX_REVIEW_SCAN_PAGES_V1: u32 = 20;
const MAX_CI_RESPONSE_BYTES_V1: usize = 2 * 1024 * 1024;

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

struct RegisteredGitHubReadOnlyCredentialAuthorityV1 {
    authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
    active: Arc<AtomicBool>,
    generation: u64,
}

enum ProfileGitHubReadOnlyCredentialAuthorityV1 {
    Public,
    Private {
        authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
    },
}

type ProfileGitHubReadOnlyCredentialAuthorityMapV1 =
    BTreeMap<(UserProfileId, String, String), ProfileGitHubReadOnlyCredentialAuthorityV1>;
type ProfileGitHubReadOnlyCredentialAuthoritiesLockV1 =
    Mutex<ProfileGitHubReadOnlyCredentialAuthorityMapV1>;
type RegisteredGitHubReadOnlyCredentialAuthorityMapV1 =
    BTreeMap<(String, String), RegisteredGitHubReadOnlyCredentialAuthorityV1>;
type RegisteredGitHubReadOnlyCredentialAuthoritiesLockV1 =
    Mutex<RegisteredGitHubReadOnlyCredentialAuthorityMapV1>;

fn profile_github_read_only_credential_authorities()
-> &'static ProfileGitHubReadOnlyCredentialAuthoritiesLockV1 {
    static AUTHORITIES: OnceLock<ProfileGitHubReadOnlyCredentialAuthoritiesLockV1> =
        OnceLock::new();
    AUTHORITIES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn registered_github_read_only_credential_authorities()
-> &'static RegisteredGitHubReadOnlyCredentialAuthoritiesLockV1 {
    static AUTHORITIES: OnceLock<RegisteredGitHubReadOnlyCredentialAuthoritiesLockV1> =
        OnceLock::new();
    AUTHORITIES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn registered_github_read_only_credential_generation_matches_v1(
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

fn next_github_credential_generation_v1() -> u64 {
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
enum GitHubReadOnlyCredentialKindV1 {
    Anonymous,
    VerifiedPrivate {
        authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
        repository_owner: String,
        repository_name: String,
        active: Arc<AtomicBool>,
        generation: u64,
    },
}

enum GitHubCredentialAuthorizationV1 {
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

    fn verified_private(
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

    fn authorization_for_target(
        &self,
        target: &GitHubRepositoryTargetV1,
        permission: GitHubReadPermissionV1,
    ) -> GitHubCredentialAuthorizationV1 {
        self.authorization_for_repository(&target.owner, &target.repository, permission)
    }

    fn authorization_for_repository(
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

    fn authorization_for_stored_repository(
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

    pub(super) fn authorization_header_for(
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubRepositoryTargetV1 {
    pub owner: String,
    pub repository: String,
    pub pull_request_number: u64,
    pub pull_request_id: tracedecay_domain::feedback::GitHubPullRequestIdV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubCiRepositoryTargetV1 {
    pub owner: String,
    pub repository: String,
}

impl GitHubCiRepositoryTargetV1 {
    pub fn validate(&self) -> bool {
        valid_path_segment(&self.owner) && valid_path_segment(&self.repository)
    }
}

impl GitHubRepositoryTargetV1 {
    pub fn validate(&self) -> bool {
        valid_path_segment(&self.owner)
            && valid_path_segment(&self.repository)
            && self.pull_request_number > 0
            && i32::try_from(self.pull_request_number).is_ok()
            && self.pull_request_id.validate().is_ok()
    }
}

#[derive(Clone, Debug)]
pub struct GitHubHttpReadConfigV1 {
    pub rest_base_uri: String,
    pub graphql_uri: String,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub socket_timeout: Duration,
}

impl Default for GitHubHttpReadConfigV1 {
    fn default() -> Self {
        Self {
            rest_base_uri: "https://api.github.com".to_owned(),
            graphql_uri: "https://api.github.com/graphql".to_owned(),
            request_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            socket_timeout: Duration::from_secs(20),
        }
    }
}

impl GitHubHttpReadConfigV1 {
    fn validate(&self) -> bool {
        let (Ok(rest), Ok(graphql)) = (
            Url::parse(&self.rest_base_uri),
            Url::parse(&self.graphql_uri),
        ) else {
            return false;
        };
        rest.scheme() == "https"
            && graphql.scheme() == "https"
            && rest.host_str() == graphql.host_str()
            && rest.port_or_known_default() == graphql.port_or_known_default()
            && !self.request_timeout.is_zero()
            && !self.connect_timeout.is_zero()
            && !self.socket_timeout.is_zero()
    }
}

#[derive(Clone)]
pub struct GitHubReadOnlyClientV1 {
    agent: ureq::Agent,
    target: GitHubRepositoryTargetV1,
    credential: GitHubReadOnlyCredentialV1,
    config: GitHubHttpReadConfigV1,
}

impl GitHubReadOnlyClientV1 {
    pub fn new(
        target: GitHubRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<Self> {
        if matches!(
            credential.authorization_for_target(&target, GitHubReadPermissionV1::PullRequests),
            GitHubCredentialAuthorizationV1::Denied
        ) {
            return None;
        }
        Self::build(target, credential, config)
    }

    pub fn new_for_ci(
        target: GitHubCiRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<GitHubCiReadOnlyClientV1> {
        GitHubCiReadOnlyClientV1::new(target, credential, config)
    }

    fn build(
        target: GitHubRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<Self> {
        if !target.validate() || !config.validate() {
            return None;
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(config.request_timeout))
            .timeout_connect(Some(config.connect_timeout))
            .timeout_recv_response(Some(config.socket_timeout))
            .timeout_recv_body(Some(config.socket_timeout))
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Some(Self {
            agent,
            target,
            credential,
            config,
        })
    }

    fn execute_rest(
        &self,
        context: &RequestContext,
        request: &GitHubRestReadRequestV1,
    ) -> GitHubReadNetworkOutcomeV1 {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
            || request.pull_request_id != self.target.pull_request_id
        {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        let Some(page) = page_from_cursor(request.resume.cursor.as_ref()) else {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        };
        let suffix = match request.descriptor.operation {
            GitHubReviewReadOperationV1::RestGetPullRequest => String::new(),
            GitHubReviewReadOperationV1::RestListPullRequestReviews => {
                format!("/reviews?per_page=100&page={page}")
            }
            GitHubReviewReadOperationV1::RestListPullRequestReviewComments => {
                format!("/comments?per_page=100&page={page}")
            }
            GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads => {
                return GitHubReadNetworkOutcomeV1::Unavailable;
            }
        };
        let url = format!(
            "{}/repos/{}/{}/pulls/{}{}",
            self.config.rest_base_uri.trim_end_matches('/'),
            self.target.owner,
            self.target.repository,
            self.target.pull_request_number,
            suffix
        );
        if !request_context_admitted(context) {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        let response = self.get(
            &url,
            (page == 1)
                .then_some(request.resume.etag.as_ref())
                .flatten(),
            GitHubReadPermissionV1::PullRequests,
        );
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        Self::decode_rest_response(response, request.descriptor.operation, page)
    }

    fn execute_graphql(
        &self,
        context: &RequestContext,
        request: &GitHubGraphQlReadRequestV1,
    ) -> GitHubReadNetworkOutcomeV1 {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
            || request.pull_request_id != self.target.pull_request_id
            || request
                .resume
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.as_str().starts_with("rest-page:"))
        {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        let variables = json!({
            "owner": self.target.owner,
            "repository": self.target.repository,
            "number": self.target.pull_request_number,
            "threadAfter": request.resume.cursor.as_ref().map(GitHubReviewCursorV1::as_str),
            "commentThreadId": "unused",
            "commentAfter": null,
            "loadThreads": true,
            "loadComments": false,
        });
        let (mut envelope, mut rate_limit) = match self.graphql(context, &variables) {
            Ok(page) => page,
            Err(failure) => return network_failure(failure),
        };
        if !envelope.errors.is_empty() {
            if let Some(checkpoint) = rate_limit
                .as_ref()
                .filter(|checkpoint| checkpoint.remaining == 0)
                .cloned()
            {
                return network_failure(HttpResponseV1::RateLimited {
                    checkpoint: Some(checkpoint),
                    retry_at: None,
                });
            }
            return GitHubReadNetworkOutcomeV1::Unavailable;
        }
        if let Err(failure) =
            self.complete_nested_comment_pages(context, &mut envelope, &mut rate_limit)
        {
            return network_failure(failure);
        }
        let next_cursor = envelope
            .data
            .as_ref()
            .and_then(|data| data.repository.as_ref())
            .and_then(|repository| repository.pull_request.as_ref())
            .and_then(|pull_request| {
                pull_request
                    .review_threads
                    .page_info
                    .has_next_page
                    .then_some(pull_request.review_threads.page_info.end_cursor.as_deref())
                    .flatten()
            })
            .and_then(|cursor| GitHubReviewCursorV1::new(cursor).ok());
        let Ok(body) = serde_json::to_vec(&envelope) else {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        };
        if body.len() > MAX_GITHUB_READ_RESPONSE_BYTES_V1 {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        }
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
            metadata: GitHubReadNetworkMetadataV1 {
                status: GitHubReadNetworkStatusV1::Ok,
                etag: None,
                next_cursor,
                rate_limit,
                retry_at: None,
            },
            body,
        })
    }

    fn complete_nested_comment_pages(
        &self,
        context: &RequestContext,
        envelope: &mut GraphQlResponseV1,
        rate_limit: &mut Option<GitHubReviewRateLimitCheckpointV1>,
    ) -> Result<(), HttpResponseV1> {
        let Some(threads) = envelope
            .data
            .as_mut()
            .and_then(|data| data.repository.as_mut())
            .and_then(|repository| repository.pull_request.as_mut())
            .map(|pull_request| &mut pull_request.review_threads.nodes)
        else {
            return Err(HttpResponseV1::Unavailable);
        };
        if threads.len() > 100 {
            return Err(HttpResponseV1::Unavailable);
        }
        let mut total = threads
            .iter()
            .map(|thread| thread.comments.nodes.len())
            .sum::<usize>();
        if total > MAX_REVIEW_ITEMS_V1 {
            return Err(HttpResponseV1::Unavailable);
        }
        for thread in threads {
            let mut pages = 0_usize;
            while thread.comments.page_info.has_next_page {
                pages += 1;
                if pages > MAX_NESTED_COMMENT_PAGES_V1 || total >= MAX_REVIEW_ITEMS_V1 {
                    return Err(HttpResponseV1::Unavailable);
                }
                let Some(comment_after) = thread.comments.page_info.end_cursor.clone() else {
                    return Err(HttpResponseV1::Unavailable);
                };
                let variables = json!({
                    "owner": self.target.owner,
                    "repository": self.target.repository,
                    "number": self.target.pull_request_number,
                    "threadAfter": null,
                    "commentThreadId": thread.id.clone(),
                    "commentAfter": comment_after,
                    "loadThreads": false,
                    "loadComments": true,
                });
                let (page, page_rate_limit) = self.graphql(context, &variables)?;
                merge_rate_limit(rate_limit, page_rate_limit);
                if !page.errors.is_empty() {
                    if let Some(checkpoint) = rate_limit
                        .as_ref()
                        .filter(|checkpoint| checkpoint.remaining == 0)
                        .cloned()
                    {
                        return Err(HttpResponseV1::RateLimited {
                            checkpoint: Some(checkpoint),
                            retry_at: None,
                        });
                    }
                    return Err(HttpResponseV1::Unavailable);
                }
                let Some(GraphQlCommentPageNodeV1 { id, comments }) =
                    page.data.and_then(|data| data.node)
                else {
                    return Err(HttpResponseV1::Unavailable);
                };
                if id != thread.id || comments.nodes.is_empty() {
                    return Err(HttpResponseV1::Unavailable);
                }
                total = total.saturating_add(comments.nodes.len());
                if total > MAX_REVIEW_ITEMS_V1 {
                    return Err(HttpResponseV1::Unavailable);
                }
                thread.comments.nodes.extend(comments.nodes);
                thread.comments.page_info = comments.page_info;
            }
        }
        Ok(())
    }

    fn graphql(
        &self,
        context: &RequestContext,
        variables: &serde_json::Value,
    ) -> Result<(GraphQlResponseV1, Option<GitHubReviewRateLimitCheckpointV1>), HttpResponseV1>
    {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return Err(HttpResponseV1::Denied);
        }
        let payload = json!({
            "query": GITHUB_REVIEW_THREADS_QUERY_V1,
            "variables": variables,
        });
        let response = self.post_static_graphql(&payload);
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return Err(HttpResponseV1::Denied);
        }
        match response {
            HttpResponseV1::Ok {
                body, rate_limit, ..
            } => serde_json::from_slice(&body)
                .map(|envelope| (envelope, rate_limit))
                .map_err(|_| HttpResponseV1::Unavailable),
            failure => Err(failure),
        }
    }

    fn decode_rest_response(
        response: HttpResponseV1,
        operation: GitHubReviewReadOperationV1,
        current_page: u32,
    ) -> GitHubReadNetworkOutcomeV1 {
        match response {
            HttpResponseV1::Ok {
                body,
                etag,
                next_page,
                rate_limit,
            } => {
                let (valid, item_count) = match operation {
                    GitHubReviewReadOperationV1::RestGetPullRequest => (
                        parse_bounded::<RestPullRequestV1>(&body).is_some() && next_page.is_none(),
                        None,
                    ),
                    GitHubReviewReadOperationV1::RestListPullRequestReviews => {
                        match parse_bounded::<Vec<RestReviewV1>>(&body) {
                            Some(items) if items.len() <= 100 => (true, Some(items.len())),
                            _ => (false, None),
                        }
                    }
                    GitHubReviewReadOperationV1::RestListPullRequestReviewComments => {
                        match parse_bounded::<Vec<RestReviewCommentV1>>(&body) {
                            Some(items) if items.len() <= 100 => (true, Some(items.len())),
                            _ => (false, None),
                        }
                    }
                    GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads => {
                        (false, None)
                    }
                };
                if !valid {
                    return GitHubReadNetworkOutcomeV1::Unavailable;
                }
                let next_page = match next_page {
                    Some(next)
                        if next == current_page.saturating_add(1)
                            && next <= MAX_REVIEW_SCAN_PAGES_V1 =>
                    {
                        Some(next)
                    }
                    Some(_) => return GitHubReadNetworkOutcomeV1::Unavailable,
                    None if item_count == Some(100) => {
                        let Some(next) = current_page.checked_add(1) else {
                            return GitHubReadNetworkOutcomeV1::Unavailable;
                        };
                        if next > MAX_REVIEW_SCAN_PAGES_V1 {
                            return GitHubReadNetworkOutcomeV1::Unavailable;
                        }
                        Some(next)
                    }
                    None => None,
                };
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::Ok,
                        etag,
                        next_cursor: next_page.and_then(|page| {
                            GitHubReviewCursorV1::new(format!("rest-page:{page}")).ok()
                        }),
                        rate_limit,
                        retry_at: None,
                    },
                    body,
                })
            }
            HttpResponseV1::NotModified { etag, rate_limit } => {
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::NotModified,
                        etag,
                        next_cursor: None,
                        rate_limit,
                        retry_at: None,
                    },
                    body: Vec::new(),
                })
            }
            HttpResponseV1::RateLimited {
                checkpoint,
                retry_at,
            } => GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                metadata: GitHubReadNetworkMetadataV1 {
                    status: GitHubReadNetworkStatusV1::RateLimited,
                    etag: None,
                    next_cursor: None,
                    rate_limit: checkpoint,
                    retry_at,
                },
                body: Vec::new(),
            }),
            HttpResponseV1::Denied => GitHubReadNetworkOutcomeV1::Denied,
            HttpResponseV1::Unavailable => GitHubReadNetworkOutcomeV1::Unavailable,
        }
    }

    fn get(
        &self,
        url: &str,
        etag: Option<&GitHubReviewEtagV1>,
        permission: GitHubReadPermissionV1,
    ) -> HttpResponseV1 {
        let authorization = self
            .credential
            .authorization_for_target(&self.target, permission);
        if matches!(&authorization, GitHubCredentialAuthorizationV1::Denied) {
            return HttpResponseV1::Denied;
        }
        let mut request = self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-read");
        if let GitHubCredentialAuthorizationV1::Private(authorization) = &authorization {
            request = request.header("Authorization", authorization.as_str());
        }
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag.as_str());
        }
        decode_ureq_response(request.call(), MAX_GITHUB_READ_RESPONSE_BYTES_V1)
    }

    fn post_static_graphql(&self, payload: &serde_json::Value) -> HttpResponseV1 {
        let authorization = self
            .credential
            .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests);
        if matches!(&authorization, GitHubCredentialAuthorizationV1::Denied) {
            return HttpResponseV1::Denied;
        }
        let mut request = self
            .agent
            .post(&self.config.graphql_uri)
            .header("Accept", "application/json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-read");
        if let GitHubCredentialAuthorizationV1::Private(authorization) = &authorization {
            request = request.header("Authorization", authorization.as_str());
        }
        decode_ureq_response(
            request.send_json(payload),
            MAX_GITHUB_READ_RESPONSE_BYTES_V1,
        )
    }
}

#[derive(Clone)]
pub struct GitHubCiReadOnlyClientV1 {
    agent: ureq::Agent,
    target: GitHubCiRepositoryTargetV1,
    credential: GitHubReadOnlyCredentialV1,
    config: GitHubHttpReadConfigV1,
}

impl GitHubCiReadOnlyClientV1 {
    fn new(
        target: GitHubCiRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<Self> {
        if !target.validate()
            || !config.validate()
            || matches!(
                credential.authorization_for_repository(
                    &target.owner,
                    &target.repository,
                    GitHubReadPermissionV1::Actions,
                ),
                GitHubCredentialAuthorizationV1::Denied
            )
            || matches!(
                credential.authorization_for_repository(
                    &target.owner,
                    &target.repository,
                    GitHubReadPermissionV1::Checks,
                ),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return None;
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(config.request_timeout))
            .timeout_connect(Some(config.connect_timeout))
            .timeout_recv_response(Some(config.socket_timeout))
            .timeout_recv_body(Some(config.socket_timeout))
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Some(Self {
            agent,
            target,
            credential,
            config,
        })
    }

    fn get(&self, url: &str, permission: GitHubReadPermissionV1) -> HttpResponseV1 {
        let authorization = self.credential.authorization_for_repository(
            &self.target.owner,
            &self.target.repository,
            permission,
        );
        if matches!(&authorization, GitHubCredentialAuthorizationV1::Denied) {
            return HttpResponseV1::Denied;
        }
        let mut request = self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-read");
        if let GitHubCredentialAuthorizationV1::Private(authorization) = &authorization {
            request = request.header("Authorization", authorization.as_str());
        }
        decode_ureq_response(request.call(), MAX_GITHUB_READ_RESPONSE_BYTES_V1)
    }

    pub(crate) fn read_workflow_run<'a>(
        &'a self,
        context: &'a RequestContext,
        run_id: u64,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if run_id == 0 {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/runs/{run_id}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository
            ),
        )
    }

    pub(crate) fn read_workflow_runs_for_head<'a>(
        &'a self,
        context: &'a RequestContext,
        head_sha: &str,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if !valid_full_commit_id(head_sha) || !valid_ci_page(page) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        let encoded_head =
            url::form_urlencoded::byte_serialize(head_sha.as_bytes()).collect::<String>();
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/runs?head_sha={encoded_head}&per_page=100&page={}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page
            ),
        )
    }

    pub(crate) fn read_check_run<'a>(
        &'a self,
        context: &'a RequestContext,
        check_run_id: u64,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if check_run_id == 0 {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Checks,
            format!(
                "{}/repos/{}/{}/check-runs/{check_run_id}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository
            ),
        )
    }

    pub(crate) fn read_workflow_job<'a>(
        &'a self,
        context: &'a RequestContext,
        job_id: u64,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if job_id == 0 {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/jobs/{job_id}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository
            ),
        )
    }

    #[allow(dead_code)] // Plan 37 PR-review CI jobs — staged
    pub(crate) fn read_workflow_jobs<'a>(
        &'a self,
        context: &'a RequestContext,
        run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if run_id == 0 || !valid_ci_page(page) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/runs/{run_id}/jobs?per_page=100&page={}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page
            ),
        )
    }

    pub(crate) fn read_check_runs<'a>(
        &'a self,
        context: &'a RequestContext,
        check_suite_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if check_suite_id == 0 || !valid_ci_page(page) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Checks,
            format!(
                "{}/repos/{}/{}/check-suites/{check_suite_id}/check-runs?status=completed&filter=latest&per_page=100&page={}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page
            ),
        )
    }

    pub(crate) fn read_check_annotations<'a>(
        &'a self,
        context: &'a RequestContext,
        check_run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if check_run_id == 0 || !valid_ci_page(page) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Checks,
            format!(
                "{}/repos/{}/{}/check-runs/{check_run_id}/annotations?per_page=100&page={}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page
            ),
        )
    }

    fn read_ci_get<'a>(
        &'a self,
        context: &'a RequestContext,
        permission: GitHubReadPermissionV1,
        url: String,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if !matches!(
            context.admission_at(now_micros()),
            RequestAdmission::Admitted
        ) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        if matches!(
            self.credential.authorization_for_repository(
                &self.target.owner,
                &self.target.repository,
                permission,
            ),
            GitHubCredentialAuthorizationV1::Denied
        ) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Denied });
        }
        let client = self.clone();
        let context_for_read = context.clone();
        Box::pin(async move {
            let task = tokio::task::spawn_blocking(move || {
                if request_context_admitted(&context_for_read)
                    && !matches!(
                        client.credential.authorization_for_repository(
                            &client.target.owner,
                            &client.target.repository,
                            permission,
                        ),
                        GitHubCredentialAuthorizationV1::Denied
                    )
                {
                    let response = client.get(&url, permission);
                    if request_context_admitted(&context_for_read)
                        && !matches!(
                            client.credential.authorization_for_repository(
                                &client.target.owner,
                                &client.target.repository,
                                permission,
                            ),
                            GitHubCredentialAuthorizationV1::Denied
                        )
                    {
                        response
                    } else {
                        HttpResponseV1::Denied
                    }
                } else {
                    HttpResponseV1::Denied
                }
            });
            match wait_for_read(context, task).await {
                Some(HttpResponseV1::Ok { body, .. }) if body.len() <= MAX_CI_RESPONSE_BYTES_V1 => {
                    GitHubCiTransportOutcomeV1::Response(body)
                }
                Some(HttpResponseV1::RateLimited {
                    checkpoint: Some(limit),
                    ..
                }) => GitHubCiTransportOutcomeV1::RateLimited(limit),
                Some(HttpResponseV1::Denied) => GitHubCiTransportOutcomeV1::Denied,
                _ => GitHubCiTransportOutcomeV1::Unavailable,
            }
        })
    }
}

impl GitHubReadOnlyNetworkAuthorityV1 for GitHubReadOnlyClientV1 {
    fn get<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubRestReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return Box::pin(async { GitHubReadNetworkOutcomeV1::Denied });
        }
        let client = self.clone();
        let context = context.clone();
        let request = request.clone();
        Box::pin(async move {
            let wait_context = context.clone();
            let task = tokio::task::spawn_blocking(move || client.execute_rest(&context, &request));
            wait_for_read(&wait_context, task)
                .await
                .unwrap_or(GitHubReadNetworkOutcomeV1::Unavailable)
        })
    }

    fn query<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubGraphQlReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
        if !request_context_admitted(context)
            || matches!(
                self.credential
                    .authorization_for_target(&self.target, GitHubReadPermissionV1::PullRequests),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return Box::pin(async { GitHubReadNetworkOutcomeV1::Denied });
        }
        let client = self.clone();
        let context = context.clone();
        let request = request.clone();
        Box::pin(async move {
            let wait_context = context.clone();
            let task =
                tokio::task::spawn_blocking(move || client.execute_graphql(&context, &request));
            wait_for_read(&wait_context, task)
                .await
                .unwrap_or(GitHubReadNetworkOutcomeV1::Unavailable)
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubCiTransportOutcomeV1 {
    Response(Vec<u8>),
    RateLimited(GitHubReviewRateLimitCheckpointV1),
    Denied,
    Unavailable,
}

enum HttpResponseV1 {
    Ok {
        body: Vec<u8>,
        etag: Option<GitHubReviewEtagV1>,
        next_page: Option<u32>,
        rate_limit: Option<GitHubReviewRateLimitCheckpointV1>,
    },
    NotModified {
        etag: Option<GitHubReviewEtagV1>,
        rate_limit: Option<GitHubReviewRateLimitCheckpointV1>,
    },
    RateLimited {
        checkpoint: Option<GitHubReviewRateLimitCheckpointV1>,
        retry_at: Option<UtcMicros>,
    },
    Denied,
    Unavailable,
}

fn network_failure(failure: HttpResponseV1) -> GitHubReadNetworkOutcomeV1 {
    match failure {
        HttpResponseV1::RateLimited {
            checkpoint,
            retry_at,
        } => GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
            metadata: GitHubReadNetworkMetadataV1 {
                status: GitHubReadNetworkStatusV1::RateLimited,
                etag: None,
                next_cursor: None,
                rate_limit: checkpoint,
                retry_at,
            },
            body: Vec::new(),
        }),
        HttpResponseV1::Denied => GitHubReadNetworkOutcomeV1::Denied,
        _ => GitHubReadNetworkOutcomeV1::Unavailable,
    }
}

fn decode_ureq_response(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    maximum: usize,
) -> HttpResponseV1 {
    let Ok(mut response) = response else {
        return HttpResponseV1::Unavailable;
    };
    let rate_limit = rate_limit_checkpoint(response.headers());
    match response.status().as_u16() {
        200 => {
            let etag = header(response.headers(), "etag")
                .and_then(|value| GitHubReviewEtagV1::new(value).ok());
            let next_page = next_page(response.headers());
            let Ok(body) = response
                .body_mut()
                .with_config()
                .limit(maximum as u64)
                .read_to_vec()
            else {
                return HttpResponseV1::Unavailable;
            };
            HttpResponseV1::Ok {
                body,
                etag,
                next_page,
                rate_limit,
            }
        }
        304 => HttpResponseV1::NotModified {
            etag: header(response.headers(), "etag")
                .and_then(|value| GitHubReviewEtagV1::new(value).ok()),
            rate_limit,
        },
        401 => HttpResponseV1::Denied,
        403 | 429 => {
            let retry_at = retry_after_at(response.headers());
            let checkpoint = retry_after_checkpoint(rate_limit.as_ref(), retry_at)
                .or_else(|| rate_limit.filter(|limit| limit.remaining == 0));
            if checkpoint.is_some() || retry_at.is_some() {
                HttpResponseV1::RateLimited {
                    checkpoint,
                    retry_at,
                }
            } else {
                HttpResponseV1::Denied
            }
        }
        _ => HttpResponseV1::Unavailable,
    }
}

async fn wait_for_read<T: Send + 'static>(
    context: &RequestContext,
    task: tokio::task::JoinHandle<T>,
) -> Option<T> {
    tokio::select! {
        result = task => result.ok(),
        () = wait_for_interruption(context) => None,
    }
}

async fn wait_for_interruption(context: &RequestContext) {
    loop {
        if !matches!(
            context.admission_at(now_micros()),
            RequestAdmission::Admitted
        ) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn request_context_admitted(context: &RequestContext) -> bool {
    matches!(
        context.admission_at(now_micros()),
        RequestAdmission::Admitted
    )
}

fn page_from_cursor(cursor: Option<&GitHubReviewCursorV1>) -> Option<u32> {
    match cursor {
        Some(cursor) => cursor
            .as_str()
            .strip_prefix("rest-page:")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|page| (1..=MAX_REVIEW_SCAN_PAGES_V1).contains(page)),
        None => Some(1),
    }
}

fn next_page(headers: &ureq::http::HeaderMap) -> Option<u32> {
    let link = header(headers, "link")?;
    let next = link
        .split(',')
        .find(|entry| entry.contains("rel=\"next\""))?;
    let url = next.split_once('<')?.1.split_once('>')?.0;
    Url::parse(url)
        .ok()?
        .query_pairs()
        .find_map(|(key, value)| (key == "page").then(|| value.parse::<u32>().ok()).flatten())
}

fn rate_limit_checkpoint(
    headers: &ureq::http::HeaderMap,
) -> Option<GitHubReviewRateLimitCheckpointV1> {
    let checkpoint = GitHubReviewRateLimitCheckpointV1 {
        limit: header(headers, "x-ratelimit-limit")?.parse().ok()?,
        remaining: header(headers, "x-ratelimit-remaining")?.parse().ok()?,
        reset_at: UtcMicros(
            header(headers, "x-ratelimit-reset")?
                .parse::<i64>()
                .ok()?
                .checked_mul(1_000_000)?,
        ),
    };
    checkpoint.validate().is_ok().then_some(checkpoint)
}

fn retry_after_checkpoint(
    primary: Option<&GitHubReviewRateLimitCheckpointV1>,
    retry_at: Option<UtcMicros>,
) -> Option<GitHubReviewRateLimitCheckpointV1> {
    let primary = primary?;
    let reset_at = retry_at?;
    let checkpoint = GitHubReviewRateLimitCheckpointV1 {
        limit: primary.limit,
        remaining: primary.remaining,
        reset_at,
    };
    checkpoint.validate().is_ok().then_some(checkpoint)
}

fn retry_after_at(headers: &ureq::http::HeaderMap) -> Option<UtcMicros> {
    const MAX_RETRY_AFTER_SECONDS_V1: i64 = 24 * 60 * 60;
    let delay_seconds = header(headers, "retry-after")?.parse::<i64>().ok()?;
    if !(0..=MAX_RETRY_AFTER_SECONDS_V1).contains(&delay_seconds) {
        return None;
    }
    Some(UtcMicros(
        now_micros()
            .0
            .checked_add(delay_seconds.checked_mul(1_000_000)?)?,
    ))
}

fn merge_rate_limit(
    current: &mut Option<GitHubReviewRateLimitCheckpointV1>,
    next: Option<GitHubReviewRateLimitCheckpointV1>,
) {
    let Some(next) = next else {
        return;
    };
    match current {
        Some(current)
            if current.limit == next.limit
                && current.reset_at == next.reset_at
                && current.remaining <= next.remaining => {}
        _ => *current = Some(next),
    }
}

fn parse_bounded<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    (bytes.len() <= MAX_GITHUB_READ_RESPONSE_BYTES_V1)
        .then(|| serde_json::from_slice(bytes).ok())
        .flatten()
}

fn header(headers: &ureq::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn valid_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && value != "."
        && value != ".."
}

fn valid_full_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_ci_page(page: u32) -> bool {
    (1..=MAX_REVIEW_SCAN_PAGES_V1).contains(&page)
}

#[cfg(test)]
mod pagination_contract_tests {
    use super::*;

    #[test]
    fn full_rest_page_without_link_continues_bounded_scan() {
        let body = serde_json::to_vec(
            &(1..=100)
                .map(|id| RestReviewV1 {
                    id,
                    node_id: None,
                    state: None,
                    commit_id: None,
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let outcome = GitHubReadOnlyClientV1::decode_rest_response(
            HttpResponseV1::Ok {
                body,
                etag: None,
                next_page: None,
                rate_limit: None,
            },
            GitHubReviewReadOperationV1::RestListPullRequestReviews,
            1,
        );
        let GitHubReadNetworkOutcomeV1::Response(response) = outcome else {
            panic!("full page must continue");
        };
        assert_eq!(
            response.metadata.next_cursor.unwrap().as_str(),
            "rest-page:2"
        );
        assert!(
            page_from_cursor(GitHubReviewCursorV1::new("rest-page:21").ok().as_ref()).is_none()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    use static_assertions::assert_not_impl_any;
    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::feedback::{
        FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewCoverageV1,
        GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1,
        GitHubReviewReadCheckpointV1,
    };
    use tracedecay_domain::{
        ActorId, CommitId, ManifestDigest, ProjectId, ProviderId, RefId, RepositoryId, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::super::store::ProjectGitHubReviewStoreV1;
    use super::super::{
        GitHubReadResumeV1, GitHubReviewAtomicRefreshStoreV1, GitHubReviewReadResponseV1,
        GitHubReviewRefreshStateV1, GitHubReviewRefreshStoreCommitOutcomeV1,
    };
    use super::*;
    use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

    const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const THREAD_CAPTURE: &str =
        include_str!("../fixtures/pr13_branch_pr/review_thread.graphql.json");

    #[derive(Clone, Copy)]
    enum FixtureCredentialAuthorityModeV1 {
        Verified,
        NotConfigured,
        WriteCapable,
        Indeterminate,
    }

    struct FixtureCredentialAuthorityV1 {
        mode: FixtureCredentialAuthorityModeV1,
    }

    impl GitHubReadOnlyCredentialAuthorityV1 for FixtureCredentialAuthorityV1 {
        fn resolve(
            &self,
            _repository_owner: &str,
            _repository_name: &str,
        ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
            match self.mode {
                FixtureCredentialAuthorityModeV1::Verified => {
                    GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                        secret: GitHubReadOnlyCredentialSecretV1::new(
                            "github_pat_fixture_private_read",
                        )
                        .unwrap(),
                        exact_permissions: BTreeSet::from([
                            GitHubReadPermissionV1::PullRequests,
                            GitHubReadPermissionV1::Actions,
                            GitHubReadPermissionV1::Checks,
                        ]),
                    }
                }
                FixtureCredentialAuthorityModeV1::NotConfigured => {
                    GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured
                }
                FixtureCredentialAuthorityModeV1::WriteCapable => {
                    GitHubReadOnlyCredentialAuthorityOutcomeV1::WriteCapable
                }
                FixtureCredentialAuthorityModeV1::Indeterminate => {
                    GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate
                }
            }
        }
    }

    struct MutableFixtureCredentialAuthorityV1 {
        mode: Mutex<FixtureCredentialAuthorityModeV1>,
    }

    impl MutableFixtureCredentialAuthorityV1 {
        fn new(mode: FixtureCredentialAuthorityModeV1) -> Self {
            Self {
                mode: Mutex::new(mode),
            }
        }

        fn set_mode(&self, mode: FixtureCredentialAuthorityModeV1) {
            *self.mode.lock().unwrap() = mode;
        }
    }

    impl GitHubReadOnlyCredentialAuthorityV1 for MutableFixtureCredentialAuthorityV1 {
        fn resolve(
            &self,
            repository_owner: &str,
            repository_name: &str,
        ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
            FixtureCredentialAuthorityV1 {
                mode: *self.mode.lock().unwrap(),
            }
            .resolve(repository_owner, repository_name)
        }
    }

    fn registered_fixture_credential(
        repository: &str,
        mode: FixtureCredentialAuthorityModeV1,
    ) -> (
        Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
        RegisteredGitHubReadOnlyCredentialV1,
    ) {
        let authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> =
            Arc::new(FixtureCredentialAuthorityV1 { mode });
        assert!(register_github_read_only_credential_authority_v1(
            "ScriptedAlchemy",
            repository,
            &authority,
        ));
        let resolution =
            resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", repository);
        (authority, resolution)
    }

    #[test]
    fn credential_remount_receives_a_new_opaque_generation() {
        let repository = "credential-generation";
        let (first_authority, first) =
            registered_fixture_credential(repository, FixtureCredentialAuthorityModeV1::Verified);
        let RegisteredGitHubReadOnlyCredentialV1::Verified(first) = first else {
            panic!("first credential must resolve");
        };
        let first_generation = first.generation();
        assert!(unregister_github_read_only_credential_authority_v1(
            "ScriptedAlchemy",
            repository,
            &first_authority,
        ));

        let (second_authority, second) =
            registered_fixture_credential(repository, FixtureCredentialAuthorityModeV1::Verified);
        let RegisteredGitHubReadOnlyCredentialV1::Verified(second) = second else {
            panic!("second credential must resolve");
        };
        assert_ne!(first_generation, second.generation());
        assert!(unregister_github_read_only_credential_authority_v1(
            "ScriptedAlchemy",
            repository,
            &second_authority,
        ));
    }

    fn captured_get_headers(credential: GitHubReadOnlyCredentialV1, repository: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&buffer[..read]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
            String::from_utf8(bytes).unwrap()
        });
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: repository.to_owned(),
                pull_request_number: 421,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            },
            credential,
            config: GitHubHttpReadConfigV1 {
                rest_base_uri: format!("http://{address}"),
                graphql_uri: format!("http://{address}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
        };
        let _ = client.get(
            &format!("http://{address}/fixture"),
            None,
            GitHubReadPermissionV1::PullRequests,
        );
        server.join().unwrap()
    }

    #[test]
    fn retry_after_without_primary_rate_headers_is_not_authorization_denial() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "retry-after-only".to_owned(),
                pull_request_number: 421,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            },
            credential: GitHubReadOnlyCredentialV1::anonymous(),
            config: GitHubHttpReadConfigV1 {
                rest_base_uri: format!("http://{address}"),
                graphql_uri: format!("http://{address}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
        };

        let response = client.get(
            &format!("http://{address}/fixture"),
            None,
            GitHubReadPermissionV1::PullRequests,
        );
        server.join().unwrap();

        let outcome = network_failure(response);
        assert!(
            matches!(
                outcome,
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::RateLimited,
                        rate_limit: None,
                        retry_at: Some(_),
                        ..
                    },
                    ..
                })
            ),
            "Retry-After is rate-limit evidence, not authorization denial"
        );
    }

    #[test]
    fn github_credentials_are_not_debuggable_or_serializable() {
        assert_not_impl_any!(
            GitHubReadOnlyCredentialSecretV1:
                std::fmt::Debug,
                serde::Serialize,
                serde::de::DeserializeOwned
        );
        assert_not_impl_any!(
            GitHubReadOnlyCredentialV1:
                std::fmt::Debug,
                serde::Serialize,
                serde::de::DeserializeOwned
        );
        assert_not_impl_any!(
            ProfileGitHubReadOnlyCredentialAuthorityV1:
                std::fmt::Debug,
                serde::Serialize,
                serde::de::DeserializeOwned
        );
    }

    #[test]
    fn exact_profile_configuration_mount_authenticates_project_open_review_read() {
        struct PullRequestReadCredential;

        impl GitHubReadOnlyCredentialAuthorityV1 for PullRequestReadCredential {
            fn resolve(
                &self,
                repository_owner: &str,
                repository_name: &str,
            ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
                if repository_owner != "ScriptedAlchemy"
                    || repository_name != "profile-mounted-private"
                {
                    return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
                }
                GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                    secret: GitHubReadOnlyCredentialSecretV1::new(
                        "github_pat_exact_profile_fixture",
                    )
                    .unwrap(),
                    exact_permissions: BTreeSet::from([GitHubReadPermissionV1::PullRequests]),
                }
            }
        }

        let profile_root = tempfile::tempdir().unwrap();
        let exact_profile = UserProfileId::new("profile.github.exact").unwrap();
        let other_profile = UserProfileId::new("profile.github.other").unwrap();
        let authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> =
            Arc::new(PullRequestReadCredential);
        assert!(register_profile_github_read_only_credential_authority_v1(
            exact_profile.clone(),
            "ScriptedAlchemy",
            "profile-mounted-private",
            &authority,
        ));
        assert_eq!(
            mount_profile_github_read_only_credential_authority_v1(
                &other_profile,
                "ScriptedAlchemy",
                "profile-mounted-private",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured
        );
        assert!(matches!(
            resolve_registered_github_read_only_credential_v1(
                "ScriptedAlchemy",
                "profile-mounted-private",
            ),
            RegisteredGitHubReadOnlyCredentialV1::Missing
        ));
        assert_eq!(
            mount_profile_github_read_only_credential_authority_v1(
                &exact_profile,
                "ScriptedAlchemy",
                "profile-mounted-private",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted
        );
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
            resolve_registered_github_read_only_credential_v1(
                "ScriptedAlchemy",
                "profile-mounted-private",
            )
        else {
            panic!("exact-profile project-open mount must resolve");
        };
        assert!(credential.permits(GitHubReadPermissionV1::PullRequests));
        assert!(!credential.permits(GitHubReadPermissionV1::Actions));
        assert!(!credential.permits(GitHubReadPermissionV1::Checks));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (headers, _) = read_http_request_with_headers(&mut stream);
            let fixture: serde_json::Value = serde_json::from_str(THREAD_CAPTURE).unwrap();
            write_http_json(&mut stream, &fixture["response"]);
            headers
        });
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "profile-mounted-private".to_owned(),
                pull_request_number: 421,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            },
            credential,
            config: GitHubHttpReadConfigV1 {
                rest_base_uri: format!("http://{address}"),
                graphql_uri: format!("http://{address}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
        };
        let request_scope = scope("exact-profile-project-open");
        let outcome = client.execute_graphql(
            &context(&request_scope),
            &GitHubGraphQlReadRequestV1 {
                scope: request_scope,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
                resume: GitHubReadResumeV1::empty(),
            },
        );
        let GitHubReadNetworkOutcomeV1::Response(response) = outcome else {
            panic!("exact-profile project-open review read must contribute a response");
        };
        let envelope: GraphQlResponseV1 = serde_json::from_slice(&response.body).unwrap();
        assert!(
            !envelope
                .data
                .unwrap()
                .repository
                .unwrap()
                .pull_request
                .unwrap()
                .review_threads
                .nodes
                .is_empty()
        );
        let headers = server.join().unwrap().to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer github_pat_exact_profile_fixture\r\n"));
        assert!(
            std::fs::read_dir(profile_root.path())
                .unwrap()
                .next()
                .is_none(),
            "credential mount must not persist token material",
        );
        assert!(unregister_profile_github_read_only_credential_authority_v1(
            &exact_profile,
            "ScriptedAlchemy",
            "profile-mounted-private",
            &authority,
        ));
    }

    #[test]
    fn application_registration_retains_and_exactly_unregisters_authority() {
        let authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> =
            Arc::new(FixtureCredentialAuthorityV1 {
                mode: FixtureCredentialAuthorityModeV1::Verified,
            });
        let weak = Arc::downgrade(&authority);
        assert!(register_github_read_only_credential_authority_v1(
            "ScriptedAlchemy",
            "retained-private",
            &authority,
        ));
        drop(authority);
        let retained = weak
            .upgrade()
            .expect("application registry retains authority");
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
            resolve_registered_github_read_only_credential_v1(
                "ScriptedAlchemy",
                "retained-private",
            )
        else {
            panic!("registered authority must issue a verified credential");
        };
        assert!(credential.permits(GitHubReadPermissionV1::PullRequests));
        assert!(unregister_github_read_only_credential_authority_v1(
            "ScriptedAlchemy",
            "retained-private",
            &retained,
        ));
        assert!(!credential.permits(GitHubReadPermissionV1::PullRequests));
        assert!(
            credential
                .authorization_header_for(GitHubReadPermissionV1::PullRequests)
                .is_err()
        );
        drop(credential);
        drop(retained);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn anonymous_requests_never_emit_authorization() {
        let headers = captured_get_headers(GitHubReadOnlyCredentialV1::anonymous(), "tracedecay");
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
    }

    #[test]
    fn verified_private_requests_emit_secret_only_as_authorization() {
        let (_authority, resolution) = registered_fixture_credential(
            "private-read",
            FixtureCredentialAuthorityModeV1::Verified,
        );
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) = resolution else {
            panic!("verified authority must resolve");
        };
        let target = GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "private-read".to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        };
        assert!(
            GitHubReadOnlyClientV1::new(
                target.clone(),
                credential.clone(),
                GitHubHttpReadConfigV1::default(),
            )
            .is_some()
        );
        assert!(
            GitHubReadOnlyClientV1::new_for_ci(
                GitHubCiRepositoryTargetV1 {
                    owner: target.owner,
                    repository: target.repository,
                },
                credential.clone(),
                GitHubHttpReadConfigV1::default(),
            )
            .is_some()
        );
        let headers = captured_get_headers(credential, "private-read").to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer github_pat_fixture_private_read"));
    }

    #[test]
    fn unavailable_write_capable_and_indeterminate_authorities_reject_before_network() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        for (repository, mode) in [
            (
                "not-configured",
                FixtureCredentialAuthorityModeV1::NotConfigured,
            ),
            (
                "write-capable",
                FixtureCredentialAuthorityModeV1::WriteCapable,
            ),
            (
                "indeterminate",
                FixtureCredentialAuthorityModeV1::Indeterminate,
            ),
        ] {
            let (_authority, resolution) = registered_fixture_credential(repository, mode);
            assert!(matches!(
                resolution,
                RegisteredGitHubReadOnlyCredentialV1::Rejected
            ));
        }
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn cached_private_credential_re_resolves_permission_and_repository_binding() {
        let authority = Arc::new(MutableFixtureCredentialAuthorityV1::new(
            FixtureCredentialAuthorityModeV1::Verified,
        ));
        let registered: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> = authority.clone();
        assert!(register_github_read_only_credential_authority_v1(
            "ScriptedAlchemy",
            "permission-drift",
            &registered,
        ));
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
            resolve_registered_github_read_only_credential_v1(
                "ScriptedAlchemy",
                "permission-drift",
            )
        else {
            panic!("initial verified authority must resolve");
        };
        let wrong_repository = GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "other-repository".to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        };
        assert!(
            GitHubReadOnlyClientV1::new(
                wrong_repository,
                credential.clone(),
                GitHubHttpReadConfigV1::default(),
            )
            .is_none()
        );

        authority.set_mode(FixtureCredentialAuthorityModeV1::WriteCapable);
        assert!(!credential.permits(GitHubReadPermissionV1::PullRequests));
        assert!(
            credential
                .authorization_header_for(GitHubReadPermissionV1::PullRequests)
                .is_err()
        );
    }

    #[test]
    fn cached_private_credential_fails_closed_when_authority_expires() {
        let authority = Arc::new(MutableFixtureCredentialAuthorityV1::new(
            FixtureCredentialAuthorityModeV1::Verified,
        ));
        let registered: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> = authority.clone();
        assert!(register_github_read_only_credential_authority_v1(
            "ScriptedAlchemy",
            "expired-private",
            &registered,
        ));
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
            resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", "expired-private")
        else {
            panic!("initial verified authority must resolve");
        };

        authority.set_mode(FixtureCredentialAuthorityModeV1::NotConfigured);

        assert!(!credential.permits(GitHubReadPermissionV1::PullRequests));
        assert!(
            credential
                .authorization_header_for(GitHubReadPermissionV1::PullRequests)
                .is_err()
        );
    }

    #[test]
    fn permission_drift_after_rest_response_blocks_response_publication() {
        let authority = Arc::new(MutableFixtureCredentialAuthorityV1::new(
            FixtureCredentialAuthorityModeV1::Verified,
        ));
        let registered: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> = authority.clone();
        assert!(register_github_read_only_credential_authority_v1(
            "ScriptedAlchemy",
            "publication-drift",
            &registered,
        ));
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
            resolve_registered_github_read_only_credential_v1(
                "ScriptedAlchemy",
                "publication-drift",
            )
        else {
            panic!("initial verified authority must resolve");
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let authority_for_server = Arc::clone(&authority);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut stream);
            authority_for_server.set_mode(FixtureCredentialAuthorityModeV1::WriteCapable);
            write_http_json(
                &mut stream,
                &json!({
                    "id": 4_026_204_542_u64,
                    "number": 421,
                    "base": {"sha": "commit.github.base"},
                    "head": {"sha": "commit.github.head"}
                }),
            );
        });
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "publication-drift".to_owned(),
                pull_request_number: 421,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            },
            credential,
            config: GitHubHttpReadConfigV1 {
                rest_base_uri: format!("http://{address}"),
                graphql_uri: format!("http://{address}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
        };
        let request_scope = scope("publication-drift");
        let outcome = client.execute_rest(
            &context(&request_scope),
            &GitHubRestReadRequestV1 {
                descriptor: super::super::GitHubRestDescriptorV1 {
                    operation: GitHubReviewReadOperationV1::RestGetPullRequest,
                },
                scope: request_scope,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
                resume: GitHubReadResumeV1::empty(),
            },
        );
        server.join().unwrap();

        assert_eq!(outcome, GitHubReadNetworkOutcomeV1::Denied);
    }

    fn scope(suffix: &str) -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: ProjectId::new(format!("project.github.{suffix}")).unwrap(),
            repository_id: RepositoryId::new(format!("repository.github.{suffix}")).unwrap(),
            worktree_id: WorktreeId::new(format!("worktree.github.{suffix}")).unwrap(),
            branch_ref: format!("refs/heads/github-{suffix}"),
            head_commit_id: CommitId::new(format!("commit.github.{suffix}.head")).unwrap(),
        }
    }

    fn context(scope: &FeedbackScopeV1) -> RequestContext {
        let resolved = ResolvedScope::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            Some(RefId::new(scope.branch_ref.clone()).unwrap()),
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.github.owner-bound").unwrap(),
            1,
            ManifestDigest::new(SHA).unwrap(),
            ActorId::new("actor.github.issuer").unwrap(),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            resolved.clone(),
            BTreeSet::from([CapabilityId::new(
                "capability.application.feedback.github-review-ingest",
            )
            .unwrap()]),
            BTreeSet::from([
                UseCaseId::new("use-case.application.feedback.github-review-ingest").unwrap(),
            ]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            ActorId::new("actor.github.owner-bound").unwrap(),
            resolved,
            grant,
            RequestId::new("request.github.owner-bound").unwrap(),
            Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
            CancellationContext::active("cancel.github.owner-bound").unwrap(),
        )
        .unwrap()
    }

    fn request(scope: FeedbackScopeV1) -> GitHubReviewReadRequestV1 {
        GitHubReviewReadRequestV1 {
            operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
            scope,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        }
    }

    fn complete_response(request: &GitHubReviewReadRequestV1) -> GitHubReviewReadResponseV1 {
        GitHubReviewReadResponseV1 {
            ingress: GitHubReviewIngressResultV1 {
                provider: ProviderId::new("provider.github").unwrap(),
                scope: request.scope.clone(),
                pull_request_id: request.pull_request_id.clone(),
                provider_base_commit_id: CommitId::new("commit.github.base").unwrap(),
                provider_head_commit_id: request.scope.head_commit_id.clone(),
                merge_base_commit_id: CommitId::new("commit.github.merge-base").unwrap(),
                operation: request.operation,
                outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
                coverage: GitHubReviewCoverageV1::Complete,
                items: Vec::new(),
                fetched_at: UtcMicros(10),
            },
            checkpoint: GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
        }
    }

    #[tokio::test]
    async fn cancelled_ci_read_makes_no_network_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let client = GitHubCiReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubCiRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "tracedecay".to_owned(),
            },
            credential: GitHubReadOnlyCredentialV1::anonymous(),
            config: GitHubHttpReadConfigV1 {
                rest_base_uri: format!("http://{address}"),
                graphql_uri: format!("http://{address}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
        };
        let request_scope = scope("cancelled-ci");
        let cancelled = context(&request_scope).with_cancellation(
            CancellationContext::cancelled("cancel.github.ci", UtcMicros(1)).unwrap(),
        );

        assert_eq!(
            client
                .read_workflow_runs_for_head(&cancelled, request_scope.head_commit_id.as_str(), 1,)
                .await,
            GitHubCiTransportOutcomeV1::Unavailable
        );
        tokio::task::yield_now().await;
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    fn read_http_request_with_headers(stream: &mut TcpStream) -> (String, serde_json::Value) {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "fixture client closed before request headers");
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "fixture client closed before request body");
            bytes.extend_from_slice(&buffer[..read]);
        }
        let body = if content_length == 0 {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
        };
        (
            String::from_utf8(bytes[..header_end].to_vec()).unwrap(),
            body,
        )
    }

    fn read_http_request(stream: &mut TcpStream) -> serde_json::Value {
        read_http_request_with_headers(stream).1
    }

    fn write_http_json(stream: &mut TcpStream, value: &serde_json::Value) {
        let body = serde_json::to_vec(value).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-RateLimit-Limit: 5000\r\nX-RateLimit-Remaining: 4999\r\nX-RateLimit-Reset: 2000000000\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    }

    #[test]
    fn expired_context_after_first_graphql_page_makes_no_nested_request() {
        let mut first_page: serde_json::Value = serde_json::from_str(THREAD_CAPTURE).unwrap();
        first_page = first_page["response"].take();
        let thread =
            &mut first_page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0];
        thread["comments"]["pageInfo"] = json!({
            "hasNextPage": true,
            "endCursor": "cursor.comments.expired"
        });

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut stream);
            std::thread::sleep(Duration::from_millis(350));
            write_http_json(&mut stream, &first_page);
            listener.set_nonblocking(true).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            match listener.accept() {
                Ok((mut unexpected, _)) => {
                    let _ = read_http_request(&mut unexpected);
                    write_http_json(&mut unexpected, &json!({}));
                    2
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => 1,
                Err(error) => panic!("nested request probe failed: {error}"),
            }
        });
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "tracedecay".to_owned(),
                pull_request_number: 421,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            },
            credential: GitHubReadOnlyCredentialV1::anonymous(),
            config: GitHubHttpReadConfigV1 {
                rest_base_uri: format!("http://{address}"),
                graphql_uri: format!("http://{address}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
        };
        let owner_scope = scope("expired-page");
        let deadline = Deadline::new(UtcMicros(now_micros().0.saturating_add(250_000))).unwrap();
        let expired_during_read = context(&owner_scope).with_deadline(deadline);
        let outcome = client.execute_graphql(
            &expired_during_read,
            &GitHubGraphQlReadRequestV1 {
                scope: owner_scope,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
                resume: GitHubReadResumeV1::empty(),
            },
        );

        assert!(matches!(outcome, GitHubReadNetworkOutcomeV1::Denied));
        assert_eq!(server.join().unwrap(), 1);
    }

    #[test]
    fn unregistered_credential_after_first_graphql_page_makes_no_nested_request() {
        let mut first_page: serde_json::Value = serde_json::from_str(THREAD_CAPTURE).unwrap();
        first_page = first_page["response"].take();
        let thread =
            &mut first_page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0];
        thread["comments"]["pageInfo"] = json!({
            "hasNextPage": true,
            "endCursor": "cursor.comments.revoked"
        });

        let (authority, resolution) = registered_fixture_credential(
            "revoked-after-page",
            FixtureCredentialAuthorityModeV1::Verified,
        );
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) = resolution else {
            panic!("verified authority must resolve");
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut stream);
            assert!(unregister_github_read_only_credential_authority_v1(
                "ScriptedAlchemy",
                "revoked-after-page",
                &authority,
            ));
            write_http_json(&mut stream, &first_page);
            listener.set_nonblocking(true).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            match listener.accept() {
                Ok((mut unexpected, _)) => {
                    let _ = read_http_request(&mut unexpected);
                    write_http_json(&mut unexpected, &json!({}));
                    2
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => 1,
                Err(error) => panic!("nested request probe failed: {error}"),
            }
        });
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "revoked-after-page".to_owned(),
                pull_request_number: 421,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            },
            credential,
            config: GitHubHttpReadConfigV1 {
                rest_base_uri: format!("http://{address}"),
                graphql_uri: format!("http://{address}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
        };
        let owner_scope = scope("revoked-page");
        let outcome = client.execute_graphql(
            &context(&owner_scope),
            &GitHubGraphQlReadRequestV1 {
                scope: owner_scope,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
                resume: GitHubReadResumeV1::empty(),
            },
        );

        assert!(matches!(outcome, GitHubReadNetworkOutcomeV1::Denied));
        assert_eq!(server.join().unwrap(), 1);
    }

    #[tokio::test]
    async fn github_nested_pagination_and_cas_are_owner_bound() {
        let mut first_page: serde_json::Value = serde_json::from_str(THREAD_CAPTURE).unwrap();
        first_page = first_page["response"].take();
        let thread =
            &mut first_page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0];
        thread["comments"]["pageInfo"] = json!({
            "hasNextPage": true,
            "endCursor": "cursor.comments.1"
        });
        let thread_id = thread["id"].as_str().unwrap().to_owned();
        let mut next_comment = thread["comments"]["nodes"][0].clone();
        next_comment["databaseId"] = json!(3_556_767_424_u64);
        next_comment["url"] =
            json!("https://github.com/ScriptedAlchemy/tracedecay/pull/421#discussion_r3556767424");
        serde_json::from_value::<GraphQlResponseV1>(first_page.clone())
            .expect("synthetic first page must satisfy the production response contract");
        let second_page = json!({
            "data": {
                "node": {
                    "id": thread_id.clone(),
                    "comments": {
                        "nodes": [next_comment],
                        "pageInfo": {
                            "hasNextPage": false,
                            "endCursor": null
                        }
                    }
                }
            }
        });

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let server = std::thread::spawn(move || {
            for response in [first_page, second_page] {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "production client did not request the expected GraphQL page"
                            );
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("GraphQL fixture accept failed: {error}"),
                    }
                };
                captured
                    .lock()
                    .unwrap()
                    .push(read_http_request(&mut stream));
                write_http_json(&mut stream, &response);
            }
        });
        let config = GitHubHttpReadConfigV1 {
            rest_base_uri: format!("http://{address}"),
            graphql_uri: format!("http://{address}/graphql"),
            ..GitHubHttpReadConfigV1::default()
        };
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "tracedecay".to_owned(),
                pull_request_number: 421,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            },
            credential: GitHubReadOnlyCredentialV1::anonymous(),
            config,
        };
        let owner_scope = scope("owner");
        let read_request = request(owner_scope.clone());
        let read_context = context(&owner_scope);
        let outcome = client.execute_graphql(
            &read_context,
            &GitHubGraphQlReadRequestV1 {
                scope: owner_scope.clone(),
                pull_request_id: read_request.pull_request_id.clone(),
                resume: GitHubReadResumeV1::empty(),
            },
        );
        server.join().unwrap();
        let GitHubReadNetworkOutcomeV1::Response(response) = outcome else {
            panic!("production GraphQL client must complete nested pagination");
        };
        assert_eq!(
            response.metadata.rate_limit,
            Some(GitHubReviewRateLimitCheckpointV1 {
                limit: 5_000,
                remaining: 4_999,
                reset_at: UtcMicros(2_000_000_000_000_000),
            })
        );
        let envelope: GraphQlResponseV1 = serde_json::from_slice(&response.body).unwrap();
        let comments = &envelope
            .data
            .unwrap()
            .repository
            .unwrap()
            .pull_request
            .unwrap()
            .review_threads
            .nodes[0]
            .comments;
        assert_eq!(comments.nodes.len(), 2);
        assert!(!comments.page_info.has_next_page);
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0]["variables"]["loadThreads"], true);
            assert_eq!(requests[1]["variables"]["loadComments"], true);
            assert_eq!(requests[1]["variables"]["commentThreadId"], thread_id);
            assert_eq!(
                requests[1]["variables"]["commentAfter"],
                "cursor.comments.1"
            );
        }

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("github-owner-bound.db");
        crate::register_test_schema_installer();
        let authority = DatabaseAuthority::acquire_test(&path, "github owner-bound CAS").unwrap();
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        let store =
            ProjectGitHubReviewStoreV1::new(database, owner_scope.clone()).expect("owner store");
        let context = context(&owner_scope);
        let state = GitHubReviewRefreshStateV1::transition(
            &read_request,
            None,
            complete_response(&read_request),
        )
        .unwrap();
        assert_eq!(
            store
                .compare_and_record(&context, &read_request, None, &state)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
        );
        assert_eq!(
            store
                .compare_and_record(&context, &read_request, None, &state)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Duplicate
        );

        let foreign_request = request(scope("foreign"));
        assert_eq!(
            store
                .compare_and_record(&context, &foreign_request, None, &state)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable
        );
        let mut latest = complete_response(&read_request);
        latest.ingress.fetched_at = UtcMicros(11);
        let advanced =
            GitHubReviewRefreshStateV1::transition(&read_request, Some(&state), latest).unwrap();
        assert_eq!(
            store
                .compare_and_record(
                    &context,
                    &read_request,
                    Some(&ManifestDigest::new(SHA).unwrap()),
                    &advanced,
                )
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Conflict
        );
    }
}
