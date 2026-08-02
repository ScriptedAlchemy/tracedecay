use super::*;

pub(super) enum HttpResponseV1 {
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

pub(super) fn network_failure(failure: HttpResponseV1) -> GitHubReadNetworkOutcomeV1 {
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

pub(super) fn decode_ureq_response(
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

pub(super) async fn wait_for_read<T: Send + 'static>(
    context: &RequestContext,
    task: tokio::task::JoinHandle<T>,
) -> Option<T> {
    tokio::select! {
        result = task => result.ok(),
        () = wait_for_interruption(context) => None,
    }
}

pub(super) async fn wait_for_interruption(context: &RequestContext) {
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

pub(super) fn request_context_admitted(context: &RequestContext) -> bool {
    matches!(
        context.admission_at(now_micros()),
        RequestAdmission::Admitted
    )
}

pub(super) fn page_from_cursor(cursor: Option<&GitHubReviewCursorV1>) -> Option<u32> {
    match cursor {
        Some(cursor) => cursor
            .as_str()
            .strip_prefix("rest-page:")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|page| (1..=MAX_REVIEW_SCAN_PAGES_V1).contains(page)),
        None => Some(1),
    }
}

pub(super) fn next_page(headers: &ureq::http::HeaderMap) -> Option<u32> {
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

pub(super) fn retry_after_checkpoint(
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

pub(super) fn retry_after_at(headers: &ureq::http::HeaderMap) -> Option<UtcMicros> {
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

pub(super) fn merge_rate_limit(
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

pub(super) fn parse_bounded<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    (bytes.len() <= MAX_GITHUB_READ_RESPONSE_BYTES_V1)
        .then(|| serde_json::from_slice(bytes).ok())
        .flatten()
}

pub(super) fn header(headers: &ureq::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

pub(super) fn valid_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && value != "."
        && value != ".."
}

pub(super) fn valid_full_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn valid_ci_page(page: u32) -> bool {
    (1..=MAX_REVIEW_SCAN_PAGES_V1).contains(&page)
}
