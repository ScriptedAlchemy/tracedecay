use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;
use tracedecay_domain::feedback::{GitHubPullRequestIdV1, GitHubReviewRateLimitCheckpointV1};
use tracedecay_domain::{CommitId, UtcMicros};
use url::Url;

use super::dto::valid_full_git_oid;
use super::protocol::{rate_limit_checkpoint, retry_after_at};
use super::{
    GitHubHttpReadConfigV1, GitHubReadOnlyCredentialV1, GitHubReadPermissionV1,
    GitHubRepositoryTargetV1,
};

const GITHUB_DISCOVERY_PAGE_SIZE_V1: usize = 100;
const MAX_GITHUB_DISCOVERY_RESPONSE_BYTES_V1: usize = 1024 * 1024;

/// Pull requests of the checkout's repository whose head branch has the
/// checkout's branch name, wherever that head lives. GitHub's
/// commit-associated pull-request routes only see heads pushed to the queried
/// repository, so a fork-headed pull request is found by its head ref and then
/// pinned by exact head commit.
const GITHUB_HEAD_REF_PULL_REQUESTS_QUERY_V1: &str = r"
query TraceDecayHeadRefPullRequests($owner: String!, $name: String!, $head: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: 100, headRefName: $head) {
      pageInfo { hasNextPage }
      nodes {
        databaseId
        number
        headRefOid
        baseRefOid
        headRepositoryOwner { login }
        headRepository { name }
        baseRepository { name owner { login } }
      }
    }
  }
}
";

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
/// commit. `target` is the repository the pull request is filed against (the
/// checkout's remote); the head repository is resolved from the pull request
/// itself and differs from it for a fork.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubExactCommitPullRequestV1 {
    pub target: GitHubRepositoryTargetV1,
    pub head_repository_owner: String,
    pub head_repository_name: String,
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

#[derive(Deserialize)]
struct HeadRefEnvelopeV1 {
    data: Option<HeadRefDataV1>,
}

#[derive(Deserialize)]
struct HeadRefDataV1 {
    repository: Option<HeadRefRepositoryV1>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadRefRepositoryV1 {
    pull_requests: HeadRefConnectionV1,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadRefConnectionV1 {
    page_info: HeadRefPageInfoV1,
    nodes: Vec<HeadRefPullRequestV1>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadRefPageInfoV1 {
    has_next_page: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadRefPullRequestV1 {
    database_id: u64,
    number: u64,
    head_ref_oid: String,
    base_ref_oid: String,
    head_repository_owner: Option<HeadRefLoginV1>,
    head_repository: Option<HeadRefNameV1>,
    base_repository: Option<HeadRefBaseRepositoryV1>,
}

#[derive(Deserialize)]
struct HeadRefLoginV1 {
    login: String,
}

#[derive(Deserialize)]
struct HeadRefNameV1 {
    name: String,
}

#[derive(Deserialize)]
struct HeadRefBaseRepositoryV1 {
    name: String,
    owner: HeadRefLoginV1,
}

/// Discovers the unique pull request filed against `owner/repository` whose
/// head branch is `head_ref_name` at exactly `head_commit`, including heads
/// that live in a fork.
///
/// Acquisition is one bounded static GraphQL query per scan; the scan result
/// must agree across scans before it is trusted. GitHub's GraphQL API refuses
/// unauthenticated reads, so an anonymous credential yields `Denied`.
#[hotpath::measure(label = "usecases.github_network.discover_pr")]
pub fn discover_exact_commit_pull_request_v1(
    owner: &str,
    repository: &str,
    head_ref_name: &str,
    head_commit: &CommitId,
    config: &GitHubHttpReadConfigV1,
    credential: &GitHubReadOnlyCredentialV1,
    control: &GitHubDiscoveryControlV1,
) -> GitHubExactCommitDiscoveryOutcomeV1 {
    if !valid_graphql_uri(&config.graphql_uri) {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    }
    let builder = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false);
    let agent: ureq::Agent = super::instrument_github_ureq_agent(builder).build().into();
    discover_with_agent(
        &agent,
        &DiscoveryRequestV1 {
            owner,
            repository,
            head_ref_name,
            head_commit,
        },
        config,
        credential,
        control,
    )
}

struct DiscoveryRequestV1<'a> {
    owner: &'a str,
    repository: &'a str,
    head_ref_name: &'a str,
    head_commit: &'a CommitId,
}

fn discover_with_agent(
    agent: &ureq::Agent,
    request: &DiscoveryRequestV1<'_>,
    config: &GitHubHttpReadConfigV1,
    credential: &GitHubReadOnlyCredentialV1,
    control: &GitHubDiscoveryControlV1,
) -> GitHubExactCommitDiscoveryOutcomeV1 {
    let scan = || scan_head_ref_pull_requests_v1(agent, request, config, credential, control);
    let first = scan();
    if !discovery_outcome_requires_consensus(&first) {
        return first;
    }
    let second = scan();
    if !discovery_outcome_requires_consensus(&second) {
        return second;
    }
    if let Some(agreed) = discovery_consensus(&first, &second, None) {
        return agreed;
    }
    let third = scan();
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

fn scan_head_ref_pull_requests_v1(
    agent: &ureq::Agent,
    request: &DiscoveryRequestV1<'_>,
    config: &GitHubHttpReadConfigV1,
    credential: &GitHubReadOnlyCredentialV1,
    control: &GitHubDiscoveryControlV1,
) -> GitHubExactCommitDiscoveryOutcomeV1 {
    let Some(remaining) = control.remaining() else {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    };
    let request_timeout = config.request_timeout.min(remaining);
    if !valid_path_segment(request.owner)
        || !valid_path_segment(request.repository)
        || !valid_head_ref_name(request.head_ref_name)
        || !valid_full_git_oid(request.head_commit.as_str())
        || request_timeout.is_zero()
        || config.connect_timeout.is_zero()
        || config.socket_timeout.is_zero()
    {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    }
    let authorization =
        match credential.authorization_header_for(GitHubReadPermissionV1::PullRequests) {
            Ok(authorization) => authorization,
            Err(()) => return GitHubExactCommitDiscoveryOutcomeV1::Denied,
        };
    let mut post = agent
        .post(&config.graphql_uri)
        .config()
        .timeout_global(Some(request_timeout))
        .timeout_connect(Some(config.connect_timeout.min(request_timeout)))
        .timeout_recv_response(Some(config.socket_timeout.min(request_timeout)))
        .timeout_recv_body(Some(config.socket_timeout.min(request_timeout)))
        .build()
        .header("Accept", "application/json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "tracedecay-github-read");
    if let Some(authorization) = authorization.as_ref() {
        post = post.header("Authorization", authorization.as_str());
    }
    let response = post.send_json(json!({
        "query": GITHUB_HEAD_REF_PULL_REQUESTS_QUERY_V1,
        "variables": {
            "owner": request.owner,
            "name": request.repository,
            "head": request.head_ref_name,
        },
    }));
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
            let retry_at = retry_after_at(response.headers());
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
                retry_at: retry_after_at(response.headers()),
                checkpoint,
            };
        }
        _ => return GitHubExactCommitDiscoveryOutcomeV1::Unavailable,
    }
    let Ok(body) = response
        .body_mut()
        .with_config()
        .limit(MAX_GITHUB_DISCOVERY_RESPONSE_BYTES_V1 as u64)
        .read_to_vec()
    else {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    };
    let Ok(envelope) = serde_json::from_slice::<HeadRefEnvelopeV1>(&body) else {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    };
    let Some(data) = envelope.data else {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    };
    // GitHub answers a repository the credential cannot see as `null`.
    let Some(repository) = data.repository else {
        return GitHubExactCommitDiscoveryOutcomeV1::NotFound;
    };
    let connection = repository.pull_requests;
    if connection.page_info.has_next_page || connection.nodes.len() > GITHUB_DISCOVERY_PAGE_SIZE_V1
    {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    }
    let mut matches = Vec::new();
    for pull in connection.nodes {
        if pull.head_ref_oid != request.head_commit.as_str() {
            continue;
        }
        let Some(found) = exact_pull_request(request, pull) else {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        };
        matches.push(found);
        if matches.len() > 1 {
            return GitHubExactCommitDiscoveryOutcomeV1::Ambiguous;
        }
    }
    matches
        .pop()
        .map_or(GitHubExactCommitDiscoveryOutcomeV1::NotFound, |pull| {
            GitHubExactCommitDiscoveryOutcomeV1::Found(pull)
        })
}

fn exact_pull_request(
    request: &DiscoveryRequestV1<'_>,
    pull: HeadRefPullRequestV1,
) -> Option<GitHubExactCommitPullRequestV1> {
    let base_repository = pull.base_repository?;
    if pull.database_id == 0
        || pull.number == 0
        || !base_repository
            .owner
            .login
            .eq_ignore_ascii_case(request.owner)
        || !base_repository
            .name
            .eq_ignore_ascii_case(request.repository)
    {
        return None;
    }
    let head_repository_owner = pull.head_repository_owner?.login;
    let head_repository_name = pull.head_repository?.name;
    if !valid_path_segment(&head_repository_owner) || !valid_path_segment(&head_repository_name) {
        return None;
    }
    let base_commit_id = CommitId::new(pull.base_ref_oid).ok()?;
    let head_commit_id = CommitId::new(pull.head_ref_oid).ok()?;
    if !valid_full_git_oid(base_commit_id.as_str()) || !valid_full_git_oid(head_commit_id.as_str())
    {
        return None;
    }
    let target = GitHubRepositoryTargetV1 {
        owner: request.owner.to_owned(),
        repository: request.repository.to_owned(),
        pull_request_number: pull.number,
        pull_request_id: GitHubPullRequestIdV1::new(pull.database_id.to_string()).ok()?,
    };
    target.validate().then_some(GitHubExactCommitPullRequestV1 {
        target,
        head_repository_owner,
        head_repository_name,
        base_commit_id,
        head_commit_id,
    })
}

fn valid_graphql_uri(value: &str) -> bool {
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

/// A branch name as Git permits it in a ref, bounded; GraphQL carries it as a
/// variable, so this only rejects values no pull request head can have.
fn valid_head_ref_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('/')
        && !value.ends_with('/')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'~' | b'^' | b':' | b'\\'))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::super::network::test_support::{read_http_request_with_headers, write_http_json};
    use super::*;

    const FORK_HEAD_FIXTURE: &str = include_str!("../fixtures/fork_head_pull_request.json");

    fn found(number: u64, base: &str) -> GitHubExactCommitDiscoveryOutcomeV1 {
        GitHubExactCommitDiscoveryOutcomeV1::Found(GitHubExactCommitPullRequestV1 {
            target: GitHubRepositoryTargetV1 {
                owner: "owner".to_owned(),
                repository: "repository".to_owned(),
                pull_request_number: number,
                pull_request_id: GitHubPullRequestIdV1::new(number.to_string()).unwrap(),
            },
            head_repository_owner: "owner".to_owned(),
            head_repository_name: "repository".to_owned(),
            base_commit_id: CommitId::new(base).unwrap(),
            head_commit_id: CommitId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        })
    }

    /// Serves the cached GitHub answers for dtolnay/anyhow#463 to `requests`
    /// discovery requests: the commit-associated REST route answers `[]` for
    /// a fork head, the head-ref GraphQL query names the pull request.
    fn serve_fork_head_fixture(requests: usize) -> (String, std::thread::JoinHandle<()>) {
        let fixture: serde_json::Value = serde_json::from_str(FORK_HEAD_FIXTURE).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().unwrap();
                let (headers, _) = read_http_request_with_headers(&mut stream);
                let response = if headers.starts_with("POST /graphql ") {
                    &fixture["graphql_head_ref"]["response"]
                } else {
                    &fixture["rest_commit_pulls"]["response"]
                };
                write_http_json(&mut stream, response);
            }
        });
        (format!("http://{address}"), server)
    }

    fn discover_against(base_uri: &str, head_commit: &str) -> GitHubExactCommitDiscoveryOutcomeV1 {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(false)
            .http_status_as_error(false)
            .build()
            .into();
        discover_with_agent(
            &agent,
            &DiscoveryRequestV1 {
                owner: "dtolnay",
                repository: "anyhow",
                head_ref_name: "fix/462-new-with-backtrace",
                head_commit: &CommitId::new(head_commit).unwrap(),
            },
            &GitHubHttpReadConfigV1 {
                rest_base_uri: base_uri.to_owned(),
                graphql_uri: format!("{base_uri}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
            &GitHubReadOnlyCredentialV1::anonymous(),
            &GitHubDiscoveryControlV1::bounded(Instant::now() + Duration::from_secs(15)),
        )
    }

    #[test]
    fn fork_headed_pull_request_is_found_with_its_head_repository() {
        let (base_uri, server) = serve_fork_head_fixture(4);

        assert_eq!(
            discover_against(&base_uri, "c7b210a78b32ea90b10860c58983f8c0a742ed03"),
            GitHubExactCommitDiscoveryOutcomeV1::Found(GitHubExactCommitPullRequestV1 {
                target: GitHubRepositoryTargetV1 {
                    owner: "dtolnay".to_owned(),
                    repository: "anyhow".to_owned(),
                    pull_request_number: 463,
                    pull_request_id: GitHubPullRequestIdV1::new("4597599038").unwrap(),
                },
                head_repository_owner: "sb123sb123".to_owned(),
                head_repository_name: "anyhow".to_owned(),
                base_commit_id: CommitId::new("c63b279f3f4af2b02ca6267d9eb47d6d10497f69").unwrap(),
                head_commit_id: CommitId::new("c7b210a78b32ea90b10860c58983f8c0a742ed03").unwrap(),
            })
        );
        assert_eq!(
            discover_against(&base_uri, "0000000000000000000000000000000000000001"),
            GitHubExactCommitDiscoveryOutcomeV1::NotFound,
            "a local head the pull request no longer points at must not admit it"
        );
        server.join().unwrap();
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
