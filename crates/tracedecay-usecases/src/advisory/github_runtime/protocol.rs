//! Canonical GitHub REST protocol envelope shared by every github_runtime
//! transport: header access, rate-limit checkpoints, `Retry-After` deadlines,
//! and `Link` rel="next" continuation parsing.
//!
//! Retry and continuation parsing fail closed. A `Retry-After` delay outside
//! `0..=24h` is provider noise or a hostile wedge and is discarded. A `Link`
//! header that does not name exactly the next sequential page of the issuing
//! endpoint — one rel="next" entry, same https host, no credentials or
//! fragment, only the expected `page`/`per_page` query, and a path that is
//! either byte-identical to the request path or GitHub's documented
//! `/repos/{owner}/{repo}` → `/repositories/{numeric id}` rewrite with the
//! same remainder — is an error, so a malformed or malicious continuation
//! can never steer a pagination loop.

use tracedecay_application::now_micros;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1;
use url::Url;

const MAX_RETRY_AFTER_SECONDS_V1: i64 = 24 * 60 * 60;

pub(super) fn header(headers: &ureq::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

pub(super) fn rate_limit_checkpoint(
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

/// Absolute deadline derived from a `Retry-After` delay header.
pub(super) fn retry_after_at(headers: &ureq::http::HeaderMap) -> Option<UtcMicros> {
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

/// The exact page request a `Link` rel="next" continuation must extend.
/// `endpoint` is the URL of the request that produced the response; only its
/// path is compared, so a query string on it is ignored.
pub(super) struct GitHubLinkPageScopeV1<'a> {
    pub(super) rest_base_uri: &'a str,
    pub(super) endpoint: &'a str,
    pub(super) current_page: u32,
    pub(super) page_size: usize,
}

/// Typed failure for a `Link` header that exists but is not a valid next page.
/// Callers map this to their closed unavailable outcome and must not consume
/// the response body as a successful page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InvalidGitHubLinkContinuationV1;

/// Parses the `Link` rel="next" continuation for the scoped page request.
/// `Ok(None)` means the provider offered no continuation;
/// `Err(InvalidGitHubLinkContinuationV1)` means the header exists but does
/// not name exactly the next sequential page of the issuing endpoint, and the
/// read must fail closed. The failure is also traced so the discarded 200 is
/// observable.
pub(super) fn link_next_page(
    headers: &ureq::http::HeaderMap,
    scope: &GitHubLinkPageScopeV1<'_>,
) -> Result<Option<u32>, InvalidGitHubLinkContinuationV1> {
    match parse_link_next_page(headers, scope) {
        Ok(next_page) => Ok(next_page),
        Err(InvalidGitHubLinkContinuationV1) => {
            tracing::warn!(
                event = "github_link_next_page_rejected",
                endpoint = scope.endpoint,
                current_page = scope.current_page,
                page_size = scope.page_size,
                "GitHub Link rel=next failed validation; the received page is discarded and the exchange fails closed"
            );
            Err(InvalidGitHubLinkContinuationV1)
        }
    }
}

fn parse_link_next_page(
    headers: &ureq::http::HeaderMap,
    scope: &GitHubLinkPageScopeV1<'_>,
) -> Result<Option<u32>, InvalidGitHubLinkContinuationV1> {
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
        return Err(InvalidGitHubLinkContinuationV1);
    }
    let url = next
        .split_once('<')
        .and_then(|(_, value)| value.split_once('>'))
        .map(|(value, _)| value)
        .and_then(|value| Url::parse(value).ok())
        .ok_or(InvalidGitHubLinkContinuationV1)?;
    let base = Url::parse(scope.rest_base_uri).map_err(|_| InvalidGitHubLinkContinuationV1)?;
    let expected = Url::parse(scope.endpoint).map_err(|_| InvalidGitHubLinkContinuationV1)?;
    if url.scheme() != "https"
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || !github_link_path_matches_request(url.path(), expected.path())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(InvalidGitHubLinkContinuationV1);
    }
    let mut page = None;
    let mut has_page_size = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "page" if page.is_none() => page = value.parse::<u32>().ok(),
            "per_page" if !has_page_size && value == scope.page_size.to_string() => {
                has_page_size = true;
            }
            _ => return Err(InvalidGitHubLinkContinuationV1),
        }
    }
    has_page_size
        .then_some(page)
        .flatten()
        .filter(|page| Some(*page) == scope.current_page.checked_add(1))
        .map(Some)
        .ok_or(InvalidGitHubLinkContinuationV1)
}

/// GitHub REST rewrites `/repos/{owner}/{repo}` to `/repositories/{id}` in
/// `Link` headers. The numeric id is not rebound here — GitHub does not echo
/// owner/repo on that form — so only the designator prefix and the remainder
/// derived from the request path are compared. Suffix matching is not used.
fn github_link_path_matches_request(actual: &str, expected: &str) -> bool {
    if actual == expected {
        return true;
    }
    let Some(expected_remainder) = remainder_after_repos_owner_repo(expected) else {
        return false;
    };
    remainder_after_repositories_numeric_id(actual) == Some(expected_remainder)
}

fn remainder_after_repos_owner_repo(path: &str) -> Option<&str> {
    let after_repos = path.strip_prefix("/repos/")?;
    let (owner, after_owner) = after_repos.split_once('/')?;
    if owner.is_empty() || after_owner.is_empty() {
        return None;
    }
    Some(
        after_owner
            .find('/')
            .map_or("", |index| &after_owner[index..]),
    )
}

fn remainder_after_repositories_numeric_id(path: &str) -> Option<&str> {
    let after_prefix = path.strip_prefix("/repositories/")?;
    let id_len = after_prefix.find('/').unwrap_or(after_prefix.len());
    let id = &after_prefix[..id_len];
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(&after_prefix[id_len..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(name: &'static str, value: &str) -> ureq::http::HeaderMap {
        let mut headers = ureq::http::HeaderMap::new();
        headers.insert(name, value.parse().unwrap());
        headers
    }

    fn scope<'a>(endpoint: &'a str, current_page: u32) -> GitHubLinkPageScopeV1<'a> {
        GitHubLinkPageScopeV1 {
            rest_base_uri: "https://api.github.com",
            endpoint,
            current_page,
            page_size: 100,
        }
    }

    const ENDPOINT: &str = "https://api.github.com/repos/owner/repository/releases";

    #[test]
    fn retry_after_rejects_negative_and_beyond_24h_delays() {
        assert!(retry_after_at(&headers_with("retry-after", "-1")).is_none());
        assert!(retry_after_at(&headers_with("retry-after", "-9999999999")).is_none());
        assert!(retry_after_at(&headers_with("retry-after", "86401")).is_none());
        assert!(retry_after_at(&headers_with("retry-after", "9999999999")).is_none());
        assert!(retry_after_at(&headers_with("retry-after", "not-a-number")).is_none());
        assert!(retry_after_at(&headers_with("retry-after", "0")).is_some());
        assert!(retry_after_at(&headers_with("retry-after", "60")).is_some());
        assert!(retry_after_at(&headers_with("retry-after", "86400")).is_some());
        assert!(retry_after_at(&ureq::http::HeaderMap::new()).is_none());
    }

    #[test]
    fn rate_limit_checkpoint_requires_all_three_valid_headers() {
        let mut headers = ureq::http::HeaderMap::new();
        headers.insert("x-ratelimit-limit", "5000".parse().unwrap());
        headers.insert("x-ratelimit-remaining", "4999".parse().unwrap());
        assert!(rate_limit_checkpoint(&headers).is_none());
        headers.insert("x-ratelimit-reset", "2000000000".parse().unwrap());
        let checkpoint = rate_limit_checkpoint(&headers).unwrap();
        assert_eq!(checkpoint.limit, 5000);
        assert_eq!(checkpoint.remaining, 4999);
        assert_eq!(checkpoint.reset_at, UtcMicros(2_000_000_000_000_000));
    }

    #[test]
    fn link_next_page_accepts_only_the_exact_next_sequential_page() {
        let headers = headers_with(
            "link",
            &format!("<{ENDPOINT}?per_page=100&page=2>; rel=\"next\""),
        );
        assert_eq!(link_next_page(&headers, &scope(ENDPOINT, 1)), Ok(Some(2)));
        // A skipped page is a steered continuation, not a next page.
        assert_eq!(
            link_next_page(&headers, &scope(ENDPOINT, 2)),
            Err(InvalidGitHubLinkContinuationV1)
        );
        let skip = headers_with(
            "link",
            &format!("<{ENDPOINT}?per_page=100&page=3>; rel=\"next\""),
        );
        assert_eq!(
            link_next_page(&skip, &scope(ENDPOINT, 1)),
            Err(InvalidGitHubLinkContinuationV1)
        );
    }

    #[test]
    fn link_next_page_without_a_next_entry_is_the_final_page() {
        assert_eq!(
            link_next_page(&ureq::http::HeaderMap::new(), &scope(ENDPOINT, 1)),
            Ok(None)
        );
        let last_only = headers_with(
            "link",
            &format!("<{ENDPOINT}?per_page=100&page=9>; rel=\"last\""),
        );
        assert_eq!(link_next_page(&last_only, &scope(ENDPOINT, 1)), Ok(None));
    }

    #[test]
    fn link_next_page_rejects_multiple_next_entries() {
        let headers = headers_with(
            "link",
            &format!(
                "<{ENDPOINT}?per_page=100&page=2>; rel=\"next\", <{ENDPOINT}?per_page=100&page=3>; rel=\"next\""
            ),
        );
        assert_eq!(
            link_next_page(&headers, &scope(ENDPOINT, 1)),
            Err(InvalidGitHubLinkContinuationV1)
        );
    }

    #[test]
    fn link_next_page_rejects_credentials_fragments_and_foreign_authority() {
        for hostile in [
            "<https://user:secret@api.github.com/repos/owner/repository/releases?per_page=100&page=2>; rel=\"next\"",
            "<https://user@api.github.com/repos/owner/repository/releases?per_page=100&page=2>; rel=\"next\"",
            "<https://api.github.com/repos/owner/repository/releases?per_page=100&page=2#frag>; rel=\"next\"",
            "<https://evil.example/repos/owner/repository/releases?per_page=100&page=2>; rel=\"next\"",
            "<http://api.github.com/repos/owner/repository/releases?per_page=100&page=2>; rel=\"next\"",
            "<https://api.github.com:8443/repos/owner/repository/releases?per_page=100&page=2>; rel=\"next\"",
            "<https://api.github.com/repos/owner/other/releases?per_page=100&page=2>; rel=\"next\"",
        ] {
            let headers = headers_with("link", hostile);
            assert_eq!(
                link_next_page(&headers, &scope(ENDPOINT, 1)),
                Err(InvalidGitHubLinkContinuationV1),
                "hostile continuation must fail closed: {hostile}",
            );
        }
    }

    #[test]
    fn link_next_page_rejects_unexpected_query_and_page_size() {
        for hostile in [
            format!("<{ENDPOINT}?per_page=1&page=2>; rel=\"next\""),
            format!("<{ENDPOINT}?page=2>; rel=\"next\""),
            format!("<{ENDPOINT}?per_page=100>; rel=\"next\""),
            format!("<{ENDPOINT}?per_page=100&page=2&extra=1>; rel=\"next\""),
            format!("<{ENDPOINT}?per_page=100&page=2&page=3>; rel=\"next\""),
        ] {
            let headers = headers_with("link", &hostile);
            assert_eq!(
                link_next_page(&headers, &scope(ENDPOINT, 1)),
                Err(InvalidGitHubLinkContinuationV1),
                "unexpected continuation query must fail closed: {hostile}",
            );
        }
    }

    #[test]
    fn link_next_page_compares_only_the_path_of_a_query_bearing_endpoint() {
        let endpoint = format!("{ENDPOINT}?per_page=100&page=4");
        let headers = headers_with(
            "link",
            &format!("<{ENDPOINT}?per_page=100&page=5>; rel=\"next\""),
        );
        assert_eq!(link_next_page(&headers, &scope(&endpoint, 4)), Ok(Some(5)));
    }

    #[test]
    fn link_next_page_accepts_github_repositories_numeric_rewrite() {
        // Live api.github.com Link headers rewrite /repos/{owner}/{repo} to
        // /repositories/{numeric id} while leaving the collection remainder
        // byte-identical. 724712 is the live-shaped fixture id.
        for (endpoint, next_path) in [
            (
                "https://api.github.com/repos/owner/repository/releases",
                "/repositories/724712/releases",
            ),
            (
                "https://api.github.com/repos/owner/repository/commits/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/pulls",
                "/repositories/724712/commits/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/pulls",
            ),
            (
                "https://api.github.com/repos/owner/repository/pulls/707/reviews?per_page=100&page=1",
                "/repositories/724712/pulls/707/reviews",
            ),
            (
                "https://api.github.com/repos/owner/repository/pulls/707/comments?per_page=100&page=1",
                "/repositories/724712/pulls/707/comments",
            ),
        ] {
            let headers = headers_with(
                "link",
                &format!("<https://api.github.com{next_path}?per_page=100&page=2>; rel=\"next\""),
            );
            assert_eq!(
                link_next_page(&headers, &scope(endpoint, 1)),
                Ok(Some(2)),
                "documented repositories rewrite must be accepted: {endpoint} -> {next_path}",
            );
        }
    }

    #[test]
    fn link_next_page_rejects_non_numeric_rewrite_and_different_remainder() {
        for next_path in [
            "/repositories/tracedecay/releases",
            "/repositories/724712a/releases",
            "/repositories//releases",
            "/repositories/724712/pulls",
            "/repositories/724712/releases/extra",
            "/repositories/724712/repos/owner/repository/releases",
        ] {
            let headers = headers_with(
                "link",
                &format!("<https://api.github.com{next_path}?per_page=100&page=2>; rel=\"next\""),
            );
            assert_eq!(
                link_next_page(&headers, &scope(ENDPOINT, 1)),
                Err(InvalidGitHubLinkContinuationV1),
                "rewrite must stay strict: {next_path}",
            );
        }
    }
}
