//! Project-scoped, read-only GitHub release acquisition.
//!
//! The authority reuses the exact repository access mounted for the active
//! profile. It can issue only bounded `GET` requests to the repository release
//! collection, accepts no continuation URL from callers, and never follows a
//! provider redirect.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_application::now_micros;
use tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1;
use tracedecay_domain::{ManifestDigest, ProjectId, RepositoryId, UserProfileId, UtcMicros};
use url::Url;

use super::{
    GitHubCiRepositoryTargetV1, GitHubHttpReadConfigV1, GitHubReadOnlyCredentialV1,
    GitHubReadPermissionV1, ProfileGitHubReadOnlyCredentialMountOutcomeV1,
    RegisteredGitHubReadOnlyCredentialV1, mount_profile_github_read_only_credential_authority_v1,
    resolve_registered_github_read_only_credential_v1,
};

const GITHUB_RELEASE_PAGE_SIZE_V1: usize = 100;
const MAX_GITHUB_RELEASE_PAGES_V1: u32 = 20;
const MAX_GITHUB_RELEASES_V1: usize =
    GITHUB_RELEASE_PAGE_SIZE_V1 * MAX_GITHUB_RELEASE_PAGES_V1 as usize;
const MAX_GITHUB_RELEASE_RESPONSE_BYTES_V1: usize = 2 * 1024 * 1024;
const MAX_GITHUB_RELEASE_READ_DURATION_V1: Duration = Duration::from_secs(15);
const MAX_GITHUB_RELEASE_ASSETS_V1: usize = 256;
const MAX_GITHUB_RELEASE_TAG_BYTES_V1: usize = 255;
const MAX_GITHUB_RELEASE_TEXT_BYTES_V1: usize = 4096;

/// Validated provider tag identity retained by the release projection.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct GitHubReleaseTagV1(String);

impl GitHubReleaseTagV1 {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_provider_text(&value, MAX_GITHUB_RELEASE_TAG_BYTES_V1).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One immutable asset reference published on a GitHub release.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct GitHubReleaseAssetV1 {
    pub asset_id: u64,
    pub name: String,
    pub label: Option<String>,
    pub content_type: String,
    pub size_bytes: u64,
    pub download_count: u64,
    pub download_url: String,
    pub digest: Option<ManifestDigest>,
    pub created_at: UtcMicros,
    pub updated_at: UtcMicros,
}

/// One typed release returned by the exact configured repository.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct GitHubReleaseV1 {
    pub release_id: u64,
    pub tag: GitHubReleaseTagV1,
    pub name: Option<String>,
    pub html_url: String,
    pub draft: bool,
    pub prerelease: bool,
    pub created_at: UtcMicros,
    pub published_at: Option<UtcMicros>,
    pub assets: Vec<GitHubReleaseAssetV1>,
}

/// Bounded release page for one exact project and repository identity.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct ProjectGitHubReleasePageV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub releases: Vec<GitHubReleaseV1>,
    pub truncated: bool,
}

/// The caller supplies scope and a result bound, never a URL or HTTP method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectGitHubReleaseReadRequestV1 {
    pub profile_id: UserProfileId,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub max_releases: usize,
}

impl ProjectGitHubReleaseReadRequestV1 {
    fn validate(&self) -> bool {
        self.profile_id.validate().is_ok()
            && self.project_id.validate().is_ok()
            && self.repository_id.validate().is_ok()
            && (1..=MAX_GITHUB_RELEASES_V1).contains(&self.max_releases)
    }
}

/// Cancellation and deadline owner shared with a retained blocking task.
#[derive(Clone)]
pub struct GitHubReleaseReadControlV1 {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl GitHubReleaseReadControlV1 {
    pub fn bounded(deadline: Instant) -> Self {
        Self {
            deadline,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn remaining(&self) -> Option<Duration> {
        if self.cancelled.load(Ordering::Acquire) {
            return None;
        }
        self.deadline.checked_duration_since(Instant::now())
    }
}

impl Drop for GitHubReleaseReadControlV1 {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Typed release-read states. Provider denial and rate limiting never collapse
/// into an empty successful timeline.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectGitHubReleaseReadOutcomeV1 {
    Ready {
        page: ProjectGitHubReleasePageV1,
    },
    RateLimited {
        checkpoint: Option<GitHubReviewRateLimitCheckpointV1>,
        retry_at: Option<UtcMicros>,
    },
    Denied,
    Unavailable,
}

/// Project-open result before the authority can be retained by daemon state.
pub enum ProjectGitHubReleaseAuthorityOpenOutcomeV1 {
    Ready(ProjectGitHubReleaseReadAuthorityV1),
    Denied,
    Unavailable,
}

/// Read authority bound to one project, repository identity, and configured
/// GitHub repository. It owns no scheduler, cache, update policy, or installer.
pub struct ProjectGitHubReleaseReadAuthorityV1 {
    profile_id: UserProfileId,
    project_id: ProjectId,
    repository_id: RepositoryId,
    target: GitHubCiRepositoryTargetV1,
    credential: GitHubReadOnlyCredentialV1,
    access: GitHubReleaseAccessV1,
    config: GitHubHttpReadConfigV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitHubReleaseAccessV1 {
    Public,
    Private,
}

/// Opens the release authority from the existing exact-profile repository
/// access mount. Public repositories use anonymous GitHub reads only after the
/// profile mount has explicitly classified them as public.
pub fn open_project_github_release_read_authority_v1(
    profile_id: &UserProfileId,
    project_id: ProjectId,
    repository_id: RepositoryId,
    target: GitHubCiRepositoryTargetV1,
    config: GitHubHttpReadConfigV1,
) -> ProjectGitHubReleaseAuthorityOpenOutcomeV1 {
    if profile_id.validate().is_err()
        || project_id.validate().is_err()
        || repository_id.validate().is_err()
        || !target.validate()
        || !valid_http_config(&config)
    {
        return ProjectGitHubReleaseAuthorityOpenOutcomeV1::Unavailable;
    }
    let (credential, access) = match mount_profile_github_read_only_credential_authority_v1(
        profile_id,
        &target.owner,
        &target.repository,
    ) {
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Public => (
            GitHubReadOnlyCredentialV1::anonymous(),
            GitHubReleaseAccessV1::Public,
        ),
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted => {
            match resolve_registered_github_read_only_credential_v1(
                &target.owner,
                &target.repository,
            ) {
                RegisteredGitHubReadOnlyCredentialV1::Verified(credential) => {
                    (credential, GitHubReleaseAccessV1::Private)
                }
                RegisteredGitHubReadOnlyCredentialV1::Missing => {
                    return ProjectGitHubReleaseAuthorityOpenOutcomeV1::Unavailable;
                }
                RegisteredGitHubReadOnlyCredentialV1::Rejected => {
                    return ProjectGitHubReleaseAuthorityOpenOutcomeV1::Denied;
                }
            }
        }
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured => {
            return ProjectGitHubReleaseAuthorityOpenOutcomeV1::Unavailable;
        }
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Rejected => {
            return ProjectGitHubReleaseAuthorityOpenOutcomeV1::Denied;
        }
    };
    if !credential.permits(GitHubReadPermissionV1::Contents) {
        return ProjectGitHubReleaseAuthorityOpenOutcomeV1::Denied;
    }
    ProjectGitHubReleaseAuthorityOpenOutcomeV1::Ready(ProjectGitHubReleaseReadAuthorityV1 {
        profile_id: profile_id.clone(),
        project_id,
        repository_id,
        target,
        credential,
        access,
        config,
    })
}

impl ProjectGitHubReleaseReadAuthorityV1 {
    pub fn read(
        &self,
        request: &ProjectGitHubReleaseReadRequestV1,
        control: &GitHubReleaseReadControlV1,
    ) -> ProjectGitHubReleaseReadOutcomeV1 {
        if !request.validate()
            || request.profile_id != self.profile_id
            || request.project_id != self.project_id
            || request.repository_id != self.repository_id
        {
            return ProjectGitHubReleaseReadOutcomeV1::Denied;
        }
        if control.remaining().is_none() {
            return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
        }
        if !self.access_is_current() || !self.credential.permits(GitHubReadPermissionV1::Contents) {
            return ProjectGitHubReleaseReadOutcomeV1::Denied;
        }

        let endpoint = format!(
            "{}/repos/{}/{}/releases",
            self.config.rest_base_uri.trim_end_matches('/'),
            self.target.owner,
            self.target.repository,
        );
        let mut releases = Vec::with_capacity(request.max_releases.min(100));
        let mut release_ids = BTreeSet::new();
        let mut tags = BTreeSet::new();
        let mut page = 1_u32;

        loop {
            let Some(remaining) = control.remaining() else {
                return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
            };
            if page > MAX_GITHUB_RELEASE_PAGES_V1 {
                return self.ready_page(releases, true, control);
            }
            let request_timeout = self.config.request_timeout.min(remaining);
            if request_timeout.is_zero() {
                return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
            }
            let Some(agent) = release_agent(&self.config, request_timeout) else {
                return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
            };
            let authorization = match self
                .credential
                .authorization_header_for(GitHubReadPermissionV1::Contents)
            {
                Ok(authorization) => authorization,
                Err(()) => return ProjectGitHubReleaseReadOutcomeV1::Denied,
            };
            let mut provider_request = agent
                .get(format!(
                    "{endpoint}?per_page={GITHUB_RELEASE_PAGE_SIZE_V1}&page={page}"
                ))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("User-Agent", "tracedecay-github-read");
            if let Some(authorization) = authorization.as_ref() {
                provider_request = provider_request.header("Authorization", authorization.as_str());
            }
            let Ok(mut response) = provider_request.call() else {
                return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
            };
            if control.remaining().is_none() {
                return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
            }
            if !self.access_is_current()
                || !self.credential.permits(GitHubReadPermissionV1::Contents)
            {
                return ProjectGitHubReleaseReadOutcomeV1::Denied;
            }
            let checkpoint = rate_limit_checkpoint(response.headers());
            match classify_status(response.status().as_u16(), response.headers(), checkpoint) {
                ReleaseHttpDispositionV1::Read => {}
                ReleaseHttpDispositionV1::RateLimited {
                    checkpoint,
                    retry_at,
                } => {
                    return ProjectGitHubReleaseReadOutcomeV1::RateLimited {
                        checkpoint,
                        retry_at,
                    };
                }
                ReleaseHttpDispositionV1::Denied => {
                    return ProjectGitHubReleaseReadOutcomeV1::Denied;
                }
                ReleaseHttpDispositionV1::Unavailable => {
                    return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
                }
            }
            let next_page = match next_release_page(
                response.headers(),
                &self.config.rest_base_uri,
                &endpoint,
                page,
            ) {
                Ok(next_page) => next_page,
                Err(()) => return ProjectGitHubReleaseReadOutcomeV1::Unavailable,
            };
            let Ok(body) = response
                .body_mut()
                .with_config()
                .limit((MAX_GITHUB_RELEASE_RESPONSE_BYTES_V1 + 1) as u64)
                .read_to_vec()
            else {
                return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
            };
            if body.len() > MAX_GITHUB_RELEASE_RESPONSE_BYTES_V1 {
                return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
            }
            if control.remaining().is_none() {
                return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
            }
            if !self.access_is_current()
                || !self.credential.permits(GitHubReadPermissionV1::Contents)
            {
                return ProjectGitHubReleaseReadOutcomeV1::Denied;
            }
            let Some(provider_releases) = decode_provider_page(&body, &self.target, &self.config)
            else {
                return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
            };
            let page_len = provider_releases.len();
            for release in provider_releases {
                if !release_ids.insert(release.release_id) || !tags.insert(release.tag.clone()) {
                    return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
                }
                if releases.len() == request.max_releases {
                    return self.ready_page(releases, true, control);
                }
                releases.push(release);
            }
            if releases.len() == request.max_releases {
                return self.ready_page(
                    releases,
                    next_page.is_some() || page_len == GITHUB_RELEASE_PAGE_SIZE_V1,
                    control,
                );
            }
            let next_page = match next_page {
                Some(next_page) => Some(next_page),
                None if page_len == GITHUB_RELEASE_PAGE_SIZE_V1 => page.checked_add(1),
                None => None,
            };
            let Some(next_page) = next_page else {
                return self.ready_page(releases, false, control);
            };
            if next_page > MAX_GITHUB_RELEASE_PAGES_V1 {
                return self.ready_page(releases, true, control);
            }
            page = next_page;
        }
    }

    fn access_is_current(&self) -> bool {
        matches!(
            (
                self.access,
                mount_profile_github_read_only_credential_authority_v1(
                    &self.profile_id,
                    &self.target.owner,
                    &self.target.repository,
                )
            ),
            (
                GitHubReleaseAccessV1::Public,
                ProfileGitHubReadOnlyCredentialMountOutcomeV1::Public
            ) | (
                GitHubReleaseAccessV1::Private,
                ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted
            )
        )
    }

    fn ready_page(
        &self,
        releases: Vec<GitHubReleaseV1>,
        truncated: bool,
        control: &GitHubReleaseReadControlV1,
    ) -> ProjectGitHubReleaseReadOutcomeV1 {
        if control.remaining().is_none() {
            return ProjectGitHubReleaseReadOutcomeV1::Unavailable;
        }
        if !self.access_is_current() || !self.credential.permits(GitHubReadPermissionV1::Contents) {
            return ProjectGitHubReleaseReadOutcomeV1::Denied;
        }
        ProjectGitHubReleaseReadOutcomeV1::Ready {
            page: ProjectGitHubReleasePageV1 {
                project_id: self.project_id.clone(),
                repository_id: self.repository_id.clone(),
                releases,
                truncated,
            },
        }
    }
}

fn release_agent(
    config: &GitHubHttpReadConfigV1,
    request_timeout: Duration,
) -> Option<ureq::Agent> {
    valid_http_config(config).then(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(request_timeout))
            .timeout_connect(Some(config.connect_timeout.min(request_timeout)))
            .timeout_recv_response(Some(config.socket_timeout.min(request_timeout)))
            .timeout_recv_body(Some(config.socket_timeout.min(request_timeout)))
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into()
    })
}

#[derive(Debug, Deserialize)]
struct ProviderReleaseV1 {
    id: u64,
    tag_name: String,
    name: Option<String>,
    html_url: String,
    draft: bool,
    prerelease: bool,
    created_at: String,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<ProviderReleaseAssetV1>,
}

#[derive(Debug, Deserialize)]
struct ProviderReleaseAssetV1 {
    id: u64,
    name: String,
    label: Option<String>,
    content_type: String,
    size: u64,
    download_count: u64,
    browser_download_url: String,
    digest: Option<String>,
    created_at: String,
    updated_at: String,
}

fn decode_provider_page(
    body: &[u8],
    target: &GitHubCiRepositoryTargetV1,
    config: &GitHubHttpReadConfigV1,
) -> Option<Vec<GitHubReleaseV1>> {
    let provider = serde_json::from_slice::<Vec<ProviderReleaseV1>>(body).ok()?;
    if provider.len() > GITHUB_RELEASE_PAGE_SIZE_V1 {
        return None;
    }
    provider
        .into_iter()
        .map(|release| normalize_release(release, target, config))
        .collect()
}

fn normalize_release(
    provider: ProviderReleaseV1,
    target: &GitHubCiRepositoryTargetV1,
    config: &GitHubHttpReadConfigV1,
) -> Option<GitHubReleaseV1> {
    let tag = GitHubReleaseTagV1::new(provider.tag_name)?;
    if provider.id == 0
        || provider.assets.len() > MAX_GITHUB_RELEASE_ASSETS_V1
        || !valid_release_html_url(&provider.html_url, target, config, &tag)
        || provider
            .name
            .as_ref()
            .is_some_and(|value| !valid_provider_text(value, MAX_GITHUB_RELEASE_TEXT_BYTES_V1))
    {
        return None;
    }
    let created_at = parse_provider_time(&provider.created_at)?;
    let published_at = match provider.published_at.as_deref() {
        Some(value) => Some(parse_provider_time(value)?),
        None => None,
    };
    let mut asset_ids = BTreeSet::new();
    let mut asset_names = BTreeSet::new();
    let assets = provider
        .assets
        .into_iter()
        .map(|asset| {
            normalize_asset(
                asset,
                &mut asset_ids,
                &mut asset_names,
                target,
                config,
                &tag,
            )
        })
        .collect::<Option<Vec<_>>>()?;
    Some(GitHubReleaseV1 {
        release_id: provider.id,
        tag,
        name: provider.name,
        html_url: provider.html_url,
        draft: provider.draft,
        prerelease: provider.prerelease,
        created_at,
        published_at,
        assets,
    })
}

fn normalize_asset(
    provider: ProviderReleaseAssetV1,
    asset_ids: &mut BTreeSet<u64>,
    asset_names: &mut BTreeSet<String>,
    target: &GitHubCiRepositoryTargetV1,
    config: &GitHubHttpReadConfigV1,
    tag: &GitHubReleaseTagV1,
) -> Option<GitHubReleaseAssetV1> {
    if provider.id == 0
        || !asset_ids.insert(provider.id)
        || !asset_names.insert(provider.name.clone())
        || !valid_provider_text(&provider.name, MAX_GITHUB_RELEASE_TEXT_BYTES_V1)
        || provider
            .label
            .as_ref()
            .is_some_and(|value| !valid_provider_text(value, MAX_GITHUB_RELEASE_TEXT_BYTES_V1))
        || !valid_provider_text(&provider.content_type, MAX_GITHUB_RELEASE_TEXT_BYTES_V1)
        || !valid_asset_download_url(
            &provider.browser_download_url,
            target,
            config,
            tag,
            &provider.name,
        )
    {
        return None;
    }
    let digest = provider.digest.map(ManifestDigest::new).transpose().ok()?;
    let created_at = parse_provider_time(&provider.created_at)?;
    let updated_at = parse_provider_time(&provider.updated_at)?;
    Some(GitHubReleaseAssetV1 {
        asset_id: provider.id,
        name: provider.name,
        label: provider.label,
        content_type: provider.content_type,
        size_bytes: provider.size,
        download_count: provider.download_count,
        download_url: provider.browser_download_url,
        digest,
        created_at,
        updated_at,
    })
}

enum ReleaseHttpDispositionV1 {
    Read,
    RateLimited {
        checkpoint: Option<GitHubReviewRateLimitCheckpointV1>,
        retry_at: Option<UtcMicros>,
    },
    Denied,
    Unavailable,
}

fn classify_status(
    status: u16,
    headers: &ureq::http::HeaderMap,
    checkpoint: Option<GitHubReviewRateLimitCheckpointV1>,
) -> ReleaseHttpDispositionV1 {
    match status {
        200 => ReleaseHttpDispositionV1::Read,
        401 | 404 => ReleaseHttpDispositionV1::Denied,
        403 => {
            let retry_at = retry_at(headers);
            if checkpoint
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.remaining == 0)
                || retry_at.is_some()
            {
                ReleaseHttpDispositionV1::RateLimited {
                    checkpoint,
                    retry_at,
                }
            } else {
                ReleaseHttpDispositionV1::Denied
            }
        }
        429 => ReleaseHttpDispositionV1::RateLimited {
            checkpoint,
            retry_at: retry_at(headers),
        },
        _ => ReleaseHttpDispositionV1::Unavailable,
    }
}

fn next_release_page(
    headers: &ureq::http::HeaderMap,
    rest_base_uri: &str,
    endpoint: &str,
    current_page: u32,
) -> Result<Option<u32>, ()> {
    let Some(link) = header(headers, "link") else {
        return Ok(None);
    };
    let mut next_entries = link
        .split(',')
        .filter(|entry| entry.contains("rel=\"next\""));
    let Some(next) = next_entries.next() else {
        return Ok(None);
    };
    if next_entries.next().is_some() {
        return Err(());
    }
    let url = next
        .split_once('<')
        .and_then(|(_, value)| value.split_once('>'))
        .map(|(value, _)| value)
        .and_then(|value| Url::parse(value).ok())
        .ok_or(())?;
    let base = Url::parse(rest_base_uri).map_err(|_| ())?;
    let expected = Url::parse(endpoint).map_err(|_| ())?;
    if url.scheme() != "https"
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || url.path() != expected.path()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    let mut page = None;
    let mut has_page_size = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "page" if page.is_none() => page = value.parse::<u32>().ok(),
            "per_page" if !has_page_size && value == GITHUB_RELEASE_PAGE_SIZE_V1.to_string() => {
                has_page_size = true;
            }
            _ => return Err(()),
        }
    }
    has_page_size
        .then_some(page)
        .flatten()
        .filter(|page| Some(*page) == current_page.checked_add(1))
        .map(Some)
        .ok_or(())
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

fn retry_at(headers: &ureq::http::HeaderMap) -> Option<UtcMicros> {
    let retry_seconds = header(headers, "retry-after")?.parse::<i64>().ok()?;
    if retry_seconds < 0 {
        return None;
    }
    Some(UtcMicros(
        now_micros()
            .0
            .checked_add(retry_seconds.checked_mul(1_000_000)?)?,
    ))
}

fn header(headers: &ureq::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn valid_http_config(config: &GitHubHttpReadConfigV1) -> bool {
    let (Ok(rest), Ok(graphql)) = (
        Url::parse(&config.rest_base_uri),
        Url::parse(&config.graphql_uri),
    ) else {
        return false;
    };
    valid_base_url(&rest)
        && valid_base_url(&graphql)
        && rest.host_str() == graphql.host_str()
        && rest.port_or_known_default() == graphql.port_or_known_default()
        && !config.request_timeout.is_zero()
        && !config.connect_timeout.is_zero()
        && !config.socket_timeout.is_zero()
        && config.request_timeout <= MAX_GITHUB_RELEASE_READ_DURATION_V1
        && config.connect_timeout <= MAX_GITHUB_RELEASE_READ_DURATION_V1
        && config.socket_timeout <= MAX_GITHUB_RELEASE_READ_DURATION_V1
}

fn valid_base_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn valid_release_html_url(
    value: &str,
    target: &GitHubCiRepositoryTargetV1,
    config: &GitHubHttpReadConfigV1,
    tag: &GitHubReleaseTagV1,
) -> bool {
    let Some(expected_path) = release_html_path(target, tag) else {
        return false;
    };
    valid_provider_url(value, config, &expected_path)
}

fn valid_asset_download_url(
    value: &str,
    target: &GitHubCiRepositoryTargetV1,
    config: &GitHubHttpReadConfigV1,
    tag: &GitHubReleaseTagV1,
    asset_name: &str,
) -> bool {
    let Some(expected_path) = asset_download_path(target, tag, asset_name) else {
        return false;
    };
    valid_provider_url(value, config, &expected_path)
}

fn valid_provider_url(value: &str, config: &GitHubHttpReadConfigV1, expected_path: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let Ok(rest) = Url::parse(&config.rest_base_uri) else {
        return false;
    };
    let expected_host = match rest.host_str() {
        Some("api.github.com") => "github.com",
        Some(host) => host,
        None => return false,
    };
    valid_base_url(&url)
        && url.host_str() == Some(expected_host)
        && url.port_or_known_default() == rest.port_or_known_default()
        && url.path() == expected_path
}

fn valid_provider_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn release_html_path(
    target: &GitHubCiRepositoryTargetV1,
    tag: &GitHubReleaseTagV1,
) -> Option<String> {
    encoded_provider_path(&[
        target.owner.as_str(),
        target.repository.as_str(),
        "releases",
        "tag",
        tag.as_str(),
    ])
}

fn asset_download_path(
    target: &GitHubCiRepositoryTargetV1,
    tag: &GitHubReleaseTagV1,
    asset_name: &str,
) -> Option<String> {
    encoded_provider_path(&[
        target.owner.as_str(),
        target.repository.as_str(),
        "releases",
        "download",
        tag.as_str(),
        asset_name,
    ])
}

fn encoded_provider_path(segments: &[&str]) -> Option<String> {
    let mut url = Url::parse("https://github.invalid").ok()?;
    url.path_segments_mut().ok()?.extend(segments);
    Some(url.path().to_owned())
}

fn parse_provider_time(value: &str) -> Option<UtcMicros> {
    let seconds = tracedecay_runtime_core::timeutil::parse_rfc3339_timestamp(value)?;
    Some(UtcMicros(seconds.checked_mul(1_000_000)?))
}

#[cfg(test)]
mod tests;
