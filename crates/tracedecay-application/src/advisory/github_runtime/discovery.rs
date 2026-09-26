use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracedecay_contracts::GitHubSourceStateV1;
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
// ponytail: each anonymous candidate costs one of the 60 hourly anonymous
// requests, so a head-branch name shared by more open pull requests than this
// (a popular fork's `patch-1`) is Unavailable rather than scanned; a
// credential lifts the bound through the GraphQL route.
const MAX_ANONYMOUS_HEAD_REF_CANDIDATES_V1: usize = 10;

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

/// The source state a discovery with `credential` settled in. `None` is a
/// discovery that was not attempted.
pub fn github_source_state_v1(
    credential: &GitHubReadOnlyCredentialV1,
    discovery: Option<&GitHubExactCommitDiscoveryOutcomeV1>,
) -> GitHubSourceStateV1 {
    if !credential.is_anonymous() {
        GitHubSourceStateV1::Bound
    } else if discovery == Some(&GitHubExactCommitDiscoveryOutcomeV1::Denied) {
        GitHubSourceStateV1::DeniedNoCredential
    } else {
        GitHubSourceStateV1::UnauthenticatedPublic
    }
}

/// Pull-request discovery outcome for the checkout's exact head.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubPullRequestDiscoveryKindV1 {
    Found,
    NotFound,
    Ambiguous,
    RateLimited,
    Denied,
    Unavailable,
    /// GitHub source access was not granted for this scope.
    NotAttempted,
}

/// The GitHub source of one project as status reports it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubSourceStatusV1 {
    /// `owner/name` of the checkout's `origin` remote.
    pub repository: String,
    pub state: GitHubSourceStateV1,
    pub remedy: Option<String>,
    pub pull_request_discovery: GitHubPullRequestDiscoveryKindV1,
    pub pull_request: Option<u64>,
    /// `owner/name` the discovered pull request's head lives in.
    pub head_repository: Option<String>,
}

impl GitHubSourceStatusV1 {
    pub fn observed(
        repository_owner: &str,
        repository_name: &str,
        credential: &GitHubReadOnlyCredentialV1,
        discovery: Option<&GitHubExactCommitDiscoveryOutcomeV1>,
    ) -> Self {
        let state = github_source_state_v1(credential, discovery);
        let (kind, found) = match discovery {
            None => (GitHubPullRequestDiscoveryKindV1::NotAttempted, None),
            Some(GitHubExactCommitDiscoveryOutcomeV1::Found(pull)) => {
                (GitHubPullRequestDiscoveryKindV1::Found, Some(pull))
            }
            Some(GitHubExactCommitDiscoveryOutcomeV1::NotFound) => {
                (GitHubPullRequestDiscoveryKindV1::NotFound, None)
            }
            Some(GitHubExactCommitDiscoveryOutcomeV1::Ambiguous) => {
                (GitHubPullRequestDiscoveryKindV1::Ambiguous, None)
            }
            Some(GitHubExactCommitDiscoveryOutcomeV1::RateLimited { .. }) => {
                (GitHubPullRequestDiscoveryKindV1::RateLimited, None)
            }
            Some(GitHubExactCommitDiscoveryOutcomeV1::Denied) => {
                (GitHubPullRequestDiscoveryKindV1::Denied, None)
            }
            Some(GitHubExactCommitDiscoveryOutcomeV1::Unavailable) => {
                (GitHubPullRequestDiscoveryKindV1::Unavailable, None)
            }
        };
        Self {
            repository: format!("{repository_owner}/{repository_name}"),
            state,
            remedy: state.remedy().map(str::to_owned),
            pull_request_discovery: kind,
            pull_request: found.map(|pull| pull.target.pull_request_number),
            head_repository: found.map(|pull| {
                format!(
                    "{}/{}",
                    pull.head_repository_owner, pull.head_repository_name
                )
            }),
        }
    }
}

type GitHubSourceStatusRegistryV1 = Mutex<BTreeMap<PathBuf, GitHubSourceStatusV1>>;

fn github_source_status_registry_v1() -> &'static GitHubSourceStatusRegistryV1 {
    static REGISTRY: OnceLock<GitHubSourceStatusRegistryV1> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Retains the latest GitHub source observation of the project at
/// `project_root`, replacing the one a previous open recorded.
pub fn record_github_source_status_v1(project_root: &Path, status: GitHubSourceStatusV1) {
    github_source_status_registry_v1()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root.to_path_buf(), status);
}

/// The GitHub source observation the project at `project_root` last
/// recorded. `None` means none was observed in this daemon: the checkout has
/// no GitHub `origin`, or its advisory owner has not mounted yet.
pub fn github_source_status_v1(project_root: &Path) -> Option<GitHubSourceStatusV1> {
    github_source_status_registry_v1()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .cloned()
}

/// Discovers the unique pull request filed against `owner/repository` whose
/// head branch is `head_ref_name` at exactly `head_commit`, including heads
/// that live in a fork.
///
/// With a credential, acquisition is one bounded static GraphQL query per
/// scan. GitHub's GraphQL API refuses unauthenticated reads, so an anonymous
/// scan uses the REST issue search's `head:` qualifier and reads each
/// candidate pull request for its exact head. Either way the scan result must
/// agree across scans before it is trusted.
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
    if !valid_https_uri(&config.graphql_uri) || !valid_https_uri(&config.rest_base_uri) {
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
    let scan = || match credential.authorization_header_for(GitHubReadPermissionV1::PullRequests) {
        Ok(Some(_)) => scan_head_ref_pull_requests_v1(agent, request, config, credential, control),
        Ok(None) => scan_public_head_ref_pull_requests_v1(agent, request, config, control),
        Err(()) => GitHubExactCommitDiscoveryOutcomeV1::Denied,
    };
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

/// The per-request timeout a scan may still spend, or `None` when the
/// request or the remaining budget cannot admit one.
fn admitted_request_timeout(
    request: &DiscoveryRequestV1<'_>,
    config: &GitHubHttpReadConfigV1,
    control: &GitHubDiscoveryControlV1,
) -> Option<Duration> {
    let request_timeout = config.request_timeout.min(control.remaining()?);
    (valid_path_segment(request.owner)
        && valid_path_segment(request.repository)
        && valid_head_ref_name(request.head_ref_name)
        && valid_full_git_oid(request.head_commit.as_str())
        && !request_timeout.is_zero()
        && !config.connect_timeout.is_zero()
        && !config.socket_timeout.is_zero())
    .then_some(request_timeout)
}

/// A non-success provider status as its discovery state. `None` is 200.
fn refused_status(
    response: &ureq::http::Response<ureq::Body>,
) -> Option<GitHubExactCommitDiscoveryOutcomeV1> {
    let checkpoint = rate_limit_checkpoint(response.headers());
    match response.status().as_u16() {
        200 => None,
        // REST answers a repository the caller cannot see as 404 (a read) or
        // 422 (a search qualifier naming it).
        401 | 404 | 422 => Some(GitHubExactCommitDiscoveryOutcomeV1::Denied),
        403 => {
            let retry_at = retry_after_at(response.headers());
            if checkpoint
                .as_ref()
                .is_none_or(|checkpoint| checkpoint.remaining != 0)
                && retry_at.is_none()
            {
                return Some(GitHubExactCommitDiscoveryOutcomeV1::Denied);
            }
            Some(GitHubExactCommitDiscoveryOutcomeV1::RateLimited {
                retry_at,
                checkpoint,
            })
        }
        429 => Some(GitHubExactCommitDiscoveryOutcomeV1::RateLimited {
            retry_at: retry_after_at(response.headers()),
            checkpoint,
        }),
        _ => Some(GitHubExactCommitDiscoveryOutcomeV1::Unavailable),
    }
}

fn read_bounded_body(response: &mut ureq::http::Response<ureq::Body>) -> Option<Vec<u8>> {
    response
        .body_mut()
        .with_config()
        .limit(MAX_GITHUB_DISCOVERY_RESPONSE_BYTES_V1 as u64)
        .read_to_vec()
        .ok()
}

/// One anonymous REST `GET`, answered as its body or its discovery state.
fn public_rest_get(
    agent: &ureq::Agent,
    url: &str,
    config: &GitHubHttpReadConfigV1,
    request_timeout: Duration,
    control: &GitHubDiscoveryControlV1,
) -> Result<Vec<u8>, Box<GitHubExactCommitDiscoveryOutcomeV1>> {
    let response = agent
        .get(url)
        .config()
        .timeout_global(Some(request_timeout))
        .timeout_connect(Some(config.connect_timeout.min(request_timeout)))
        .timeout_recv_response(Some(config.socket_timeout.min(request_timeout)))
        .timeout_recv_body(Some(config.socket_timeout.min(request_timeout)))
        .build()
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "tracedecay-github-read")
        .call();
    if control.remaining().is_none() {
        return Err(Box::new(GitHubExactCommitDiscoveryOutcomeV1::Unavailable));
    }
    let Ok(mut response) = response else {
        return Err(Box::new(GitHubExactCommitDiscoveryOutcomeV1::Unavailable));
    };
    if let Some(refused) = refused_status(&response) {
        return Err(Box::new(refused));
    }
    read_bounded_body(&mut response)
        .ok_or_else(|| Box::new(GitHubExactCommitDiscoveryOutcomeV1::Unavailable))
}

#[derive(Deserialize)]
struct HeadRefSearchV1 {
    total_count: usize,
    incomplete_results: bool,
    items: Vec<HeadRefSearchItemV1>,
}

#[derive(Deserialize)]
struct HeadRefSearchItemV1 {
    number: u64,
}

#[derive(Deserialize)]
struct RestPullRequestHeadRefV1 {
    id: u64,
    number: u64,
    head: RestPullRequestBranchV1,
    base: RestPullRequestBranchV1,
}

#[derive(Deserialize)]
struct RestPullRequestBranchV1 {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    repo: Option<RestPullRequestRepositoryV1>,
}

#[derive(Deserialize)]
struct RestPullRequestRepositoryV1 {
    name: String,
    owner: HeadRefLoginV1,
}

/// Anonymous discovery: the issue search's `head:` qualifier names every pull
/// request of the repository from a branch of that name, in any fork, and
/// each candidate's own read pins its exact head commit and repository.
fn scan_public_head_ref_pull_requests_v1(
    agent: &ureq::Agent,
    request: &DiscoveryRequestV1<'_>,
    config: &GitHubHttpReadConfigV1,
    control: &GitHubDiscoveryControlV1,
) -> GitHubExactCommitDiscoveryOutcomeV1 {
    let Some(request_timeout) = admitted_request_timeout(request, config, control) else {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    };
    let rest_base = config.rest_base_uri.trim_end_matches('/');
    let Ok(mut search) = Url::parse(&format!("{rest_base}/search/issues")) else {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    };
    search
        .query_pairs_mut()
        .append_pair(
            "q",
            &format!(
                "repo:{}/{} is:pr head:{}",
                request.owner, request.repository, request.head_ref_name
            ),
        )
        .append_pair("per_page", &GITHUB_DISCOVERY_PAGE_SIZE_V1.to_string());
    let body = match public_rest_get(agent, search.as_str(), config, request_timeout, control) {
        Ok(body) => body,
        Err(outcome) => return *outcome,
    };
    let Ok(found) = serde_json::from_slice::<HeadRefSearchV1>(&body) else {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    };
    if found.incomplete_results
        || found.total_count != found.items.len()
        || found.items.len() > MAX_ANONYMOUS_HEAD_REF_CANDIDATES_V1
    {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    }
    let mut matches = Vec::new();
    for candidate in found.items {
        let url = format!(
            "{rest_base}/repos/{}/{}/pulls/{}",
            request.owner, request.repository, candidate.number
        );
        let body = match public_rest_get(agent, &url, config, request_timeout, control) {
            Ok(body) => body,
            Err(outcome) => return *outcome,
        };
        let Ok(pull) = serde_json::from_slice::<RestPullRequestHeadRefV1>(&body) else {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        };
        if pull.number != candidate.number {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        }
        if pull.head.sha != request.head_commit.as_str() || pull.head.name != request.head_ref_name
        {
            continue;
        }
        let (Some(head_repository), Some(base_repository)) = (pull.head.repo, pull.base.repo)
        else {
            return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
        };
        let Some(found) = exact_pull_request(
            request,
            HeadRefPullRequestV1 {
                database_id: pull.id,
                number: pull.number,
                head_ref_oid: pull.head.sha,
                base_ref_oid: pull.base.sha,
                head_repository_owner: Some(head_repository.owner),
                head_repository: Some(HeadRefNameV1 {
                    name: head_repository.name,
                }),
                base_repository: Some(HeadRefBaseRepositoryV1 {
                    name: base_repository.name,
                    owner: base_repository.owner,
                }),
            },
        ) else {
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

fn scan_head_ref_pull_requests_v1(
    agent: &ureq::Agent,
    request: &DiscoveryRequestV1<'_>,
    config: &GitHubHttpReadConfigV1,
    credential: &GitHubReadOnlyCredentialV1,
    control: &GitHubDiscoveryControlV1,
) -> GitHubExactCommitDiscoveryOutcomeV1 {
    let Some(request_timeout) = admitted_request_timeout(request, config, control) else {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    };
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
    if matches!(response.status().as_u16(), 404 | 422) {
        return GitHubExactCommitDiscoveryOutcomeV1::Unavailable;
    }
    if let Some(refused) = refused_status(&response) {
        return refused;
    }
    let Some(body) = read_bounded_body(&mut response) else {
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

fn valid_https_uri(value: &str) -> bool {
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
    use std::collections::BTreeSet;
    use std::net::TcpListener;

    use super::super::network::test_support::{
        read_http_request_with_headers, write_http_json, write_http_response,
    };
    use super::super::{
        GitHubReadOnlyCredentialAuthorityOutcomeV1, GitHubReadOnlyCredentialAuthorityV1,
        GitHubReadOnlyCredentialSecretV1, RegisteredGitHubReadOnlyCredentialV1,
        register_github_read_only_credential_authority_v1,
        resolve_registered_github_read_only_credential_v1,
        unregister_github_read_only_credential_authority_v1,
    };
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

    /// Serves the cached GitHub answers for dtolnay/anyhow#463 as GitHub gave
    /// them: GraphQL refuses a request without `Authorization` (rate limit 0)
    /// and answers the head-ref query for one with it; the REST issue search
    /// and pull request read answer anonymously.
    fn serve_fork_head_fixture(requests: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let fixture: serde_json::Value = serde_json::from_str(FORK_HEAD_FIXTURE).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().unwrap();
                let (headers, _) = read_http_request_with_headers(&mut stream);
                let request_line = headers.lines().next().unwrap_or_default().to_owned();
                let authorized = headers.to_ascii_lowercase().contains("\r\nauthorization:");
                if request_line.starts_with("POST /graphql ") && !authorized {
                    let refusal = &fixture["graphql_anonymous"];
                    let headers = refusal["headers"]
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str().unwrap()))
                        .collect::<Vec<_>>();
                    write_http_response(&mut stream, 403, &headers, &refusal["response"]);
                } else if request_line.starts_with("POST /graphql ") {
                    write_http_json(&mut stream, &fixture["graphql_head_ref"]["response"]);
                } else if request_line.starts_with("GET /search/issues?") {
                    write_http_json(&mut stream, &fixture["rest_head_ref_search"]["response"]);
                } else if request_line.starts_with("GET /repos/dtolnay/anyhow/pulls/463 ") {
                    write_http_json(&mut stream, &fixture["rest_pull_request"]["response"]);
                } else {
                    write_http_response(&mut stream, 404, &[], &serde_json::json!({}));
                }
                seen.push(request_line);
            }
            seen
        });
        (format!("http://{address}"), server)
    }

    fn discover_against(
        base_uri: &str,
        head_commit: &str,
        credential: &GitHubReadOnlyCredentialV1,
    ) -> GitHubExactCommitDiscoveryOutcomeV1 {
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
            credential,
            &GitHubDiscoveryControlV1::bounded(Instant::now() + Duration::from_secs(15)),
        )
    }

    fn anyhow_463() -> GitHubExactCommitDiscoveryOutcomeV1 {
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
    }

    struct FixtureTokenAuthority;

    impl GitHubReadOnlyCredentialAuthorityV1 for FixtureTokenAuthority {
        fn resolve(
            &self,
            _repository_owner: &str,
            _repository_name: &str,
        ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
            GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                secret: GitHubReadOnlyCredentialSecretV1::new("github_pat_fixture_discovery")
                    .unwrap(),
                exact_permissions: BTreeSet::from([GitHubReadPermissionV1::PullRequests]),
            }
        }
    }

    #[test]
    fn anonymous_discovery_finds_a_fork_headed_pull_request_by_rest_head_ref_search() {
        let (base_uri, server) = serve_fork_head_fixture(8);
        let anonymous = GitHubReadOnlyCredentialV1::anonymous();

        let found = discover_against(
            &base_uri,
            "c7b210a78b32ea90b10860c58983f8c0a742ed03",
            &anonymous,
        );
        assert_eq!(found, anyhow_463());
        assert_eq!(
            GitHubSourceStatusV1::observed("dtolnay", "anyhow", &anonymous, Some(&found)),
            GitHubSourceStatusV1 {
                repository: "dtolnay/anyhow".to_owned(),
                state: GitHubSourceStateV1::UnauthenticatedPublic,
                remedy: Some(
                    "reads are anonymous (60 requests/hour); run `gh auth login` or set GH_TOKEN to read with a credential"
                        .to_owned()
                ),
                pull_request_discovery: GitHubPullRequestDiscoveryKindV1::Found,
                pull_request: Some(463),
                head_repository: Some("sb123sb123/anyhow".to_owned()),
            }
        );
        assert_eq!(
            discover_against(
                &base_uri,
                "0000000000000000000000000000000000000001",
                &anonymous,
            ),
            GitHubExactCommitDiscoveryOutcomeV1::NotFound,
            "a local head the pull request no longer points at must not admit it"
        );
        let seen = server.join().unwrap();
        assert!(
            seen.iter().all(|line| line.starts_with("GET ")),
            "anonymous discovery must never ask GraphQL: {seen:?}"
        );
    }

    #[test]
    fn credentialed_discovery_reads_the_head_ref_graphql_query() {
        let authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> =
            Arc::new(FixtureTokenAuthority);
        assert!(register_github_read_only_credential_authority_v1(
            "dtolnay", "anyhow", &authority,
        ));
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
            resolve_registered_github_read_only_credential_v1("dtolnay", "anyhow")
        else {
            panic!("the fixture token must resolve");
        };
        let (base_uri, server) = serve_fork_head_fixture(2);

        let found = discover_against(
            &base_uri,
            "c7b210a78b32ea90b10860c58983f8c0a742ed03",
            &credential,
        );
        assert!(unregister_github_read_only_credential_authority_v1(
            "dtolnay", "anyhow", &authority,
        ));
        assert_eq!(found, anyhow_463());
        assert_eq!(
            github_source_state_v1(&credential, Some(&found)),
            GitHubSourceStateV1::Bound
        );
        assert_eq!(
            server.join().unwrap(),
            ["POST /graphql HTTP/1.1", "POST /graphql HTTP/1.1"]
        );
    }

    #[test]
    fn anonymous_discovery_of_a_repository_github_will_not_show_is_denied_no_credential() {
        let fixture: serde_json::Value = serde_json::from_str(FORK_HEAD_FIXTURE).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_http_request_with_headers(&mut stream);
            let refusal = &fixture["rest_head_ref_search_unseen_repository"];
            write_http_response(&mut stream, 422, &[], &refusal["response"]);
        });
        let anonymous = GitHubReadOnlyCredentialV1::anonymous();

        let refused = discover_against(
            &format!("http://{address}"),
            "c7b210a78b32ea90b10860c58983f8c0a742ed03",
            &anonymous,
        );
        server.join().unwrap();
        assert_eq!(refused, GitHubExactCommitDiscoveryOutcomeV1::Denied);
        assert_eq!(
            github_source_state_v1(&anonymous, Some(&refused)),
            GitHubSourceStateV1::DeniedNoCredential
        );
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
