use super::*;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use zeroize::Zeroizing;

const GITHUB_HTTP_READ_CONCURRENCY_V1: usize = 4;

#[derive(Clone)]
pub struct GitHubHttpReadClientV1 {
    pub(super) client: reqwest::Client,
    pub(super) permits: Arc<tokio::sync::Semaphore>,
    pub(in crate::advisory::github_runtime) config: GitHubHttpReadConfigV1,
}

impl GitHubHttpReadClientV1 {
    pub fn new(config: GitHubHttpReadConfigV1) -> Option<Self> {
        config
            .validate()
            .then(|| Self::build(config, true))
            .flatten()
    }

    pub(super) fn build(config: GitHubHttpReadConfigV1, https_only: bool) -> Option<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.socket_timeout)
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(https_only)
            .build()
            .ok()?;
        Some(Self {
            client,
            permits: Arc::new(tokio::sync::Semaphore::new(GITHUB_HTTP_READ_CONCURRENCY_V1)),
            config,
        })
    }

    pub(in crate::advisory::github_runtime) async fn execute<F>(
        &self,
        context: Option<&RequestContext>,
        maximum: usize,
        authorize: F,
        build: impl FnOnce(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> HttpResponseV1
    where
        F: Fn() -> Result<Option<Zeroizing<String>>, ()>,
    {
        if context.is_some_and(|context| !request_context_admitted(context)) || authorize().is_err()
        {
            return HttpResponseV1::Denied;
        }
        let permit = match wait_for_context(context, self.permits.acquire()).await {
            Some(Ok(permit)) => permit,
            _ => return HttpResponseV1::Unavailable,
        };
        let authorization = match authorize() {
            Ok(authorization) => authorization,
            Err(()) => return HttpResponseV1::Denied,
        };
        let mut request = build(&self.client);
        if let Some(authorization) = authorization.as_ref() {
            request = request.header("Authorization", authorization.as_str());
        }
        let response = match wait_for_context(context, request.send()).await {
            Some(Ok(response)) => response,
            _ => return HttpResponseV1::Unavailable,
        };
        let status = response.status();
        let headers = response.headers().clone();
        if response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
        {
            return HttpResponseV1::Unavailable;
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        loop {
            let next = match wait_for_context(context, stream.next()).await {
                Some(next) => next,
                None => return HttpResponseV1::Unavailable,
            };
            let Some(chunk) = next else {
                break;
            };
            let Ok(chunk) = chunk else {
                return HttpResponseV1::Unavailable;
            };
            if body.len().saturating_add(chunk.len()) > maximum {
                return HttpResponseV1::Unavailable;
            }
            body.extend_from_slice(&chunk);
        }
        if context.is_some_and(|context| !request_context_admitted(context)) {
            return HttpResponseV1::Unavailable;
        }
        if authorize().is_err() {
            return HttpResponseV1::Denied;
        }
        drop(permit);
        decode_http_response(status, &headers, body)
    }

    #[cfg(test)]
    pub(super) fn available_permits(&self) -> usize {
        self.permits.available_permits()
    }
}

pub(in crate::advisory::github_runtime) enum HttpResponseV1 {
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
    NotFound,
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

pub(super) fn decode_http_response(
    status: reqwest::StatusCode,
    headers: &HeaderMap,
    body: Vec<u8>,
) -> HttpResponseV1 {
    let rate_limit = rate_limit_checkpoint(headers);
    match status.as_u16() {
        200 => {
            let etag =
                header(headers, "etag").and_then(|value| GitHubReviewEtagV1::new(value).ok());
            let next_page = next_page(headers);
            HttpResponseV1::Ok {
                body,
                etag,
                next_page,
                rate_limit,
            }
        }
        304 => HttpResponseV1::NotModified {
            etag: header(headers, "etag").and_then(|value| GitHubReviewEtagV1::new(value).ok()),
            rate_limit,
        },
        401 => HttpResponseV1::Denied,
        404 => HttpResponseV1::NotFound,
        403 | 429 => {
            let retry_at = retry_after_at(headers);
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

async fn wait_for_context<T>(
    context: Option<&RequestContext>,
    future: impl std::future::Future<Output = T>,
) -> Option<T> {
    match context {
        Some(context) => {
            tokio::select! {
                result = future => Some(result),
                () = wait_for_interruption(context) => None,
            }
        }
        None => Some(future.await),
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

pub(super) fn next_page(headers: &HeaderMap) -> Option<u32> {
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
    headers: &HeaderMap,
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

pub(super) fn retry_after_at(headers: &HeaderMap) -> Option<UtcMicros> {
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

pub(super) fn header(headers: &HeaderMap, name: &str) -> Option<String> {
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
