//! Canonical GitHub REST protocol envelope shared by every github_runtime
//! transport: header access, rate-limit checkpoints, `Retry-After` deadlines,
//! and `Link` rel="next" continuation parsing.
//!
//! Retry and continuation parsing fail closed. A `Retry-After` delay outside
//! `0..=24h` is provider noise or a hostile wedge and is discarded. A `Link`
//! header that does not name exactly the next sequential page of the issuing
//! endpoint — one rel="next" entry, same https host and path, no credentials
//! or fragment, only the expected `page`/`per_page` query — is an error, so a
//! malformed or malicious continuation can never steer a pagination loop.

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

/// Parses the `Link` rel="next" continuation for the scoped page request.
/// `Ok(None)` means the provider offered no continuation; `Err(())` means the
/// header exists but does not name exactly the next sequential page of the
/// issuing endpoint, and the read must fail closed.
pub(super) fn link_next_page(
    headers: &ureq::http::HeaderMap,
    scope: &GitHubLinkPageScopeV1<'_>,
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
    let base = Url::parse(scope.rest_base_uri).map_err(|_| ())?;
    let expected = Url::parse(scope.endpoint).map_err(|_| ())?;
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
            "per_page" if !has_page_size && value == scope.page_size.to_string() => {
                has_page_size = true;
            }
            _ => return Err(()),
        }
    }
    has_page_size
        .then_some(page)
        .flatten()
        .filter(|page| Some(*page) == scope.current_page.checked_add(1))
        .map(Some)
        .ok_or(())
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
        assert_eq!(link_next_page(&headers, &scope(ENDPOINT, 2)), Err(()));
        let skip = headers_with(
            "link",
            &format!("<{ENDPOINT}?per_page=100&page=3>; rel=\"next\""),
        );
        assert_eq!(link_next_page(&skip, &scope(ENDPOINT, 1)), Err(()));
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
        assert_eq!(link_next_page(&headers, &scope(ENDPOINT, 1)), Err(()));
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
                Err(()),
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
                Err(()),
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
}
