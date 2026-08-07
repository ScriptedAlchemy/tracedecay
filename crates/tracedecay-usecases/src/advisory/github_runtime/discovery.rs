use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tracedecay_application::now_micros;
use tracedecay_domain::feedback::{GitHubPullRequestIdV1, GitHubReviewRateLimitCheckpointV1};
use tracedecay_domain::{CommitId, UtcMicros};
use url::Url;

use super::{
    GitHubHttpReadConfigV1, GitHubReadOnlyCredentialV1, GitHubReadPermissionV1,
    GitHubRepositoryTargetV1,
};

const GITHUB_DISCOVERY_PAGE_SIZE_V1: usize = 100;
const MAX_GITHUB_DISCOVERY_PAGES_V1: u32 = 20;
const MAX_GITHUB_DISCOVERY_RESPONSE_BYTES_V1: usize = 1024 * 1024;

#[derive(Clone)]
pub struct GitHubDiscoveryControlV1 {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl GitHubDiscoveryControlV1 {
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

impl Drop for GitHubDiscoveryControlV1 {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// One pull request whose provider head is exactly the requested immutable
/// commit. Repository identity comes from the fixed REST route, not response
/// prose or the caller's current branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubExactCommitPullRequestV1 {
    pub target: GitHubRepositoryTargetV1,
    pub base_commit_id: CommitId,
    pub head_commit_id: CommitId,
}

/// Closed read-side discovery states. This type cannot represent a GitHub
/// mutation, token, arbitrary method, or caller-supplied continuation URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubExactCommitDiscoveryOutcomeV1 {
    Found(GitHubExactCommitPullRequestV1),
    NotFound,
    Ambiguous,
    RateLimited {
        checkpoint: Option<GitHubReviewRateLimitCheckpointV1>,
        retry_at: Option<UtcMicros>,
    },
    Denied,
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct AssociatedPullRequestV1 {
    id: u64,
    number: u64,
    base: AssociatedCommitRefV1,
    head: AssociatedCommitRefV1,
}

#[derive(Debug, Deserialize)]
struct AssociatedCommitRefV1 {
    sha: String,
    repo: Option<AssociatedRepositoryV1>,
}

#[derive(Debug, Deserialize)]
struct AssociatedRepositoryV1 {
    full_name: String,
}

/// Discovers the unique pull request whose head equals `head_commit`.
///
/// Acquisition is structurally limited to bounded `GET` requests against
/// GitHub's commit-associated pull-request endpoint. Anonymous acquisition is
/// used only for repositories this request proves publicly readable; private
/// acquisition requires a registered verified read credential.
pub fn discover_exact_commit_pull_request_v1(
    owner: &str,
    repository: &str,
    head_commit: &CommitId,
    config: &GitHubHttpReadConfigV1,
    credential: &GitHubReadOnlyCredentialV1,
    control: &GitHubDiscoveryControlV1,
) -> GitHubExactCommitDiscoveryOutcomeV1 {
    let first = scan_exact_commit_pull_request_v1(
        owner,
        repository,
        head_commit,
        config,
        credential,
        control,
    );
    if !discovery_outcome_requires_consensus(&first) {
        return first;
    }
    let second = scan_exact_commit_pull_request_v1(
        owner,
        repository,
        head_commit,
        config,
        credential,
        control,
    );
    if !discovery_outcome_requires_consensus(&second) {
        return second;
    }
    if let Some(agreed) = discovery_consensus(&first, &second, None) {
        return agreed;
    }
    let third = scan_exact_commit_pull_request_v1(
        owner,
        repository,
        head_commit,
        config,
        credential,
        control,
    );
    if !discovery_outcome_requires_consensus(&third) {
        return third;
    }
    discovery_consensus(&first, &second, Some(&third))
        .unwrap_or(GitHubExactCommitDiscoveryOutcomeV1::Ambiguous)
}

fn discovery_outcome_requires_consensus(outcome: &GitHubExactCommitDiscoveryOutcomeV1) -> bool {
    matches!(
        outcome,
        GitHubExactCommitDiscoveryOutcomeV1::Found(_)
            | GitHubExactCommitDiscoveryOutcomeV1::NotFound
            | GitHubExactCommitDiscoveryOutcomeV1::Ambiguous
    )
}

fn discovery_consensus(
    first: &GitHubExactCommitDiscoveryOutcomeV1,
    second: &GitHubExactCommitDiscoveryOutcomeV1,
    retry: Option<&GitHubExactCommitDiscoveryOutcomeV1>,
) -> Option<GitHubExactCommitDiscoveryOutcomeV1> {
    if first == second {
        Some(second.clone())
    } else {
        retry.filter(|retry| *retry == second).cloned()
    }
}

fn scan_exact_commit_pull_request_v1(
    owner: &str,
    repository: &str,
    head_commit: &CommitId,
    config: &GitHubHttpReadConfigV1,
    credential: &GitHubReadOnlyCredentialV1,
    control: &GitHubDiscoveryControlV1,
) -> GitHubExactCommitDiscoveryOutcomeV1 {
    let Some(_) = control.remaining() else {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    };
    if !valid_path_segment(owner)
        || !valid_path_segment(repository)
        || !valid_full_commit_id(head_commit.as_str())
        || !valid_rest_base_uri(&config.rest_base_uri)
        || config.request_timeout.is_zero()
        || config.connect_timeout.is_zero()
        || config.socket_timeout.is_zero()
    {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    }

    let expected_repository = format!("{owner}/{repository}");
    let endpoint = format!(
        "{}/repos/{owner}/{repository}/commits/{}/pulls",
        config.rest_base_uri.trim_end_matches('/'),
        head_commit.as_str()
    );
    let mut page = 1_u32;
    let mut visited_pages = BTreeSet::new();
    let mut matches = Vec::new();

    loop {
        let Some(remaining) = control.remaining() else {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        };
        if page > MAX_GITHUB_DISCOVERY_PAGES_V1 || !visited_pages.insert(page) {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        }
        let request_timeout = config.request_timeout.min(remaining);
        if request_timeout.is_zero() {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(request_timeout))
            .timeout_connect(Some(config.connect_timeout.min(request_timeout)))
            .timeout_recv_response(Some(config.socket_timeout.min(request_timeout)))
            .timeout_recv_body(Some(config.socket_timeout.min(request_timeout)))
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        let authorization =
            match credential.authorization_header_for(GitHubReadPermissionV1::PullRequests) {
                Ok(authorization) => authorization,
                Err(()) => return GitHubExactCommitDiscoveryOutcomeV1::Denied,
            };
        let mut request = agent
            .get(format!(
                "{endpoint}?per_page={GITHUB_DISCOVERY_PAGE_SIZE_V1}&page={page}"
            ))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-read");
        if let Some(authorization) = authorization.as_ref() {
            request = request.header("Authorization", authorization.as_str());
        }
        let response = request.call();
        if control.remaining().is_none() {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        }
        let Ok(mut response) = response else {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        };
        if credential
            .authorization_header_for(GitHubReadPermissionV1::PullRequests)
            .is_err()
        {
            return GitHubExactCommitDiscoveryOutcomeV1::Denied;
        }
        let checkpoint = rate_limit_checkpoint(response.headers());
        match response.status().as_u16() {
            200 => {}
            401 => return GitHubExactCommitDiscoveryOutcomeV1::Denied,
            403 => {
                let retry_at = retry_at(response.headers());
                if checkpoint
                    .as_ref()
                    .is_none_or(|checkpoint| checkpoint.remaining != 0)
                    && retry_at.is_none()
                {
                    return GitHubExactCommitDiscoveryOutcomeV1::Denied;
                }
                return GitHubExactCommitDiscoveryOutcomeV1::RateLimited {
                    retry_at,
                    checkpoint,
                };
            }
            429 => {
                return GitHubExactCommitDiscoveryOutcomeV1::RateLimited {
                    retry_at: retry_at(response.headers()),
                    checkpoint,
                };
            }
            404 => return GitHubExactCommitDiscoveryOutcomeV1::NotFound,
            _ => return GitHubExactCommitDiscoveryOutcomeV1::Unavailable,
        }

        let mut next_page =
            match next_page(response.headers(), &config.rest_base_uri, &endpoint, page) {
                Ok(next_page) => next_page,
                Err(()) => return GitHubExactCommitDiscoveryOutcomeV1::Unavailable,
            };
        let Ok(body) = response
            .body_mut()
            .with_config()
            .limit(MAX_GITHUB_DISCOVERY_RESPONSE_BYTES_V1 as u64)
            .read_to_vec()
        else {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        };
        let Ok(pulls) = serde_json::from_slice::<Vec<AssociatedPullRequestV1>>(&body) else {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        };
        if pulls.len() > GITHUB_DISCOVERY_PAGE_SIZE_V1 {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        }
        if pulls.len() == GITHUB_DISCOVERY_PAGE_SIZE_V1 && next_page.is_none() {
            next_page = page.checked_add(1);
            if next_page.is_none_or(|next| next > MAX_GITHUB_DISCOVERY_PAGES_V1) {
                return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
            }
        }
        for pull in pulls {
            if pull.head.sha != head_commit.as_str() {
                continue;
            }
            if pull
                .base
                .repo
                .as_ref()
                .is_none_or(|repo| repo.full_name != expected_repository)
            {
                return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
            }
            let Some(found) = exact_pull_request(owner, repository, head_commit, pull) else {
                return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
            };
            matches.push(found);
            if matches.len() > 1 {
                return GitHubExactCommitDiscoveryOutcomeV1::Ambiguous;
            }
        }

        let Some(next_page) = next_page else {
            break;
        };
        page = next_page;
    }

    matches
        .pop()
        .map_or(GitHubExactCommitDiscoveryOutcomeV1::NotFound, |pull| {
            GitHubExactCommitDiscoveryOutcomeV1::Found(pull)
        })
}

fn exact_pull_request(
    owner: &str,
    repository: &str,
    requested_head: &CommitId,
    pull: AssociatedPullRequestV1,
) -> Option<GitHubExactCommitPullRequestV1> {
    if pull.id == 0 || pull.number == 0 || pull.head.sha != requested_head.as_str() {
        return None;
    }
    let base_commit_id = CommitId::new(pull.base.sha).ok()?;
    let head_commit_id = CommitId::new(pull.head.sha).ok()?;
    if !valid_full_commit_id(base_commit_id.as_str())
        || !valid_full_commit_id(head_commit_id.as_str())
    {
        return None;
    }
    let pull_request_id = GitHubPullRequestIdV1::new(pull.id.to_string()).ok()?;
    let target = GitHubRepositoryTargetV1 {
        owner: owner.to_owned(),
        repository: repository.to_owned(),
        pull_request_number: pull.number,
        pull_request_id,
    };
    target.validate().then_some(GitHubExactCommitPullRequestV1 {
        target,
        base_commit_id,
        head_commit_id,
    })
}

fn next_page(
    headers: &ureq::http::HeaderMap,
    rest_base_uri: &str,
    endpoint: &str,
    current_page: u32,
) -> Result<Option<u32>, ()> {
    let Some(link) = header(headers, "link") else {
        return Ok(None);
    };
    let Some(next) = link.split(',').find(|entry| entry.contains("rel=\"next\"")) else {
        return Ok(None);
    };
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
    {
        return Err(());
    }
    let mut page = None;
    let mut has_page_size = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "page" if page.is_none() => page = value.parse::<u32>().ok(),
            "per_page" if !has_page_size && value == GITHUB_DISCOVERY_PAGE_SIZE_V1.to_string() => {
                has_page_size = true;
            }
            _ => return Err(()),
        }
    }
    has_page_size
        .then_some(page)
        .flatten()
        .filter(|page| *page > current_page)
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

fn valid_rest_base_uri(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn valid_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_full_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(number: u64, base: &str) -> GitHubExactCommitDiscoveryOutcomeV1 {
        GitHubExactCommitDiscoveryOutcomeV1::Found(GitHubExactCommitPullRequestV1 {
            target: GitHubRepositoryTargetV1 {
                owner: "owner".to_owned(),
                repository: "repository".to_owned(),
                pull_request_number: number,
                pull_request_id: GitHubPullRequestIdV1::new(number.to_string()).unwrap(),
            },
            base_commit_id: CommitId::new(base).unwrap(),
            head_commit_id: CommitId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        })
    }

    #[test]
    fn exact_discovery_requires_two_agreeing_full_scans() {
        let scan = found(7, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(discovery_consensus(&scan, &scan, None), Some(scan));
    }

    #[test]
    fn exact_discovery_accepts_one_bounded_retry_only_when_last_scans_agree() {
        let first = found(7, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let stable = found(8, "cccccccccccccccccccccccccccccccccccccccc");
        assert_eq!(
            discovery_consensus(&first, &stable, Some(&stable)),
            Some(stable)
        );
    }

    #[test]
    fn exact_discovery_quarantines_three_disagreeing_scans() {
        let first = found(7, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let second = found(8, "cccccccccccccccccccccccccccccccccccccccc");
        let third = found(9, "dddddddddddddddddddddddddddddddddddddddd");
        assert_eq!(discovery_consensus(&first, &second, Some(&third)), None);
    }

    #[test]
    fn dropping_discovery_owner_cancels_retained_blocking_clones() {
        let owner = GitHubDiscoveryControlV1::bounded(Instant::now() + Duration::from_secs(15));
        let retained = owner.clone();
        drop(owner);
        assert!(retained.remaining().is_none());
    }
}
