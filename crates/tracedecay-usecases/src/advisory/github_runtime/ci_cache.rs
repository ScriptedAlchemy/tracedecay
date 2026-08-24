//! Conditional-request cache for CI reads.
//!
//! The review-cursor resume path already carries an `ETag` through
//! `GitHubReadResumeV1` and sends `If-None-Match`; the CI path did not, so a
//! poll that finds nothing changed still paid full price for every workflow
//! run, job, check run, and annotation page. A `304 Not Modified` does not
//! count against the rate limit, so retaining the body beside its `ETag` makes
//! repeated polling of an unchanged run nearly free.
//!
//! This module supplies the missing half - the body store. The request half
//! reuses the exact `etag: Option<&GitHubReviewEtagV1>` parameter and
//! `HttpResponseV1::NotModified` decoding the review path already uses; there
//! is no second conditional-request mechanism.
//!
//! Entries are keyed by the mounted credential generation as well as the URL,
//! so a body fetched under one credential is never served to another, and a
//! remount - which always receives a fresh generation - starts from an empty
//! cache. No credential material is stored here, only response bodies the
//! caller already held.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use tracedecay_domain::feedback::GitHubReviewEtagV1;

/// Largest body retained for one URL.
const MAX_CACHED_CI_BODY_BYTES_V1: usize = 2 * 1024 * 1024;
/// Total retained bytes across every entry.
const MAX_CACHED_CI_TOTAL_BYTES_V1: usize = 16 * 1024 * 1024;
/// Bound on retained URLs.
const MAX_CACHED_CI_ENTRIES_V1: usize = 512;

/// One retained CI response body and the `ETag` that validates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CachedCiResponseV1 {
    pub(super) etag: GitHubReviewEtagV1,
    pub(super) body: Vec<u8>,
}

type CiResponseCacheV1 = BTreeMap<(u64, String), CachedCiResponseV1>;

fn ci_response_cache_v1() -> &'static Mutex<CiResponseCacheV1> {
    static CACHE: OnceLock<Mutex<CiResponseCacheV1>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Returns the retained body and `ETag` for one exact credential and URL.
pub(super) fn cached_ci_response_v1(
    credential_generation: u64,
    url: &str,
) -> Option<CachedCiResponseV1> {
    let cache = ci_response_cache_v1().lock().ok()?;
    cache.get(&(credential_generation, url.to_owned())).cloned()
}

/// Retains one body beside the `ETag` the provider issued for it.
///
/// An oversized body, an invalid `ETag`, or a poisoned lock simply retains
/// nothing: the next read is an ordinary unconditional request.
pub(super) fn retain_ci_response_v1(
    credential_generation: u64,
    url: &str,
    etag: &GitHubReviewEtagV1,
    body: &[u8],
) {
    if body.is_empty() || body.len() > MAX_CACHED_CI_BODY_BYTES_V1 || etag.validate().is_err() {
        return;
    }
    let Ok(mut cache) = ci_response_cache_v1().lock() else {
        return;
    };
    let key = (credential_generation, url.to_owned());
    let retained_bytes: usize = cache.values().map(|entry| entry.body.len()).sum();
    if cache.len() >= MAX_CACHED_CI_ENTRIES_V1
        || retained_bytes.saturating_add(body.len()) > MAX_CACHED_CI_TOTAL_BYTES_V1
    {
        cache.clear();
    }
    cache.insert(
        key,
        CachedCiResponseV1 {
            etag: etag.clone(),
            body: body.to_vec(),
        },
    );
}

/// Replaces the `ETag` of a retained body without re-storing the body.
///
/// A `304` may carry a refreshed validator for the same content.
pub(super) fn refresh_ci_response_etag_v1(
    credential_generation: u64,
    url: &str,
    etag: &GitHubReviewEtagV1,
) {
    if etag.validate().is_err() {
        return;
    }
    let Ok(mut cache) = ci_response_cache_v1().lock() else {
        return;
    };
    if let Some(entry) = cache.get_mut(&(credential_generation, url.to_owned())) {
        entry.etag = etag.clone();
    }
}

/// Drops the retained body for one exact credential and URL.
pub(super) fn forget_ci_response_v1(credential_generation: u64, url: &str) {
    let Ok(mut cache) = ci_response_cache_v1().lock() else {
        return;
    };
    cache.remove(&(credential_generation, url.to_owned()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn etag(value: &str) -> GitHubReviewEtagV1 {
        GitHubReviewEtagV1::new(value).unwrap()
    }

    #[test]
    fn a_retained_body_is_returned_for_its_exact_credential_and_url() {
        forget_ci_response_v1(7, "https://fixture/runs/1");
        retain_ci_response_v1(7, "https://fixture/runs/1", &etag("W/fixture-1"), b"{\"a\":1}");
        assert_eq!(
            cached_ci_response_v1(7, "https://fixture/runs/1"),
            Some(CachedCiResponseV1 {
                etag: etag("W/fixture-1"),
                body: b"{\"a\":1}".to_vec(),
            })
        );
        forget_ci_response_v1(7, "https://fixture/runs/1");
    }

    #[test]
    fn a_body_retained_under_one_credential_is_never_served_to_another() {
        forget_ci_response_v1(7, "https://fixture/runs/2");
        forget_ci_response_v1(8, "https://fixture/runs/2");
        retain_ci_response_v1(7, "https://fixture/runs/2", &etag("W/fixture-2"), b"{}");
        assert!(
            cached_ci_response_v1(8, "https://fixture/runs/2").is_none(),
            "a remounted credential must not read the previous one's cached body"
        );
        forget_ci_response_v1(7, "https://fixture/runs/2");
    }

    #[test]
    fn an_oversized_body_is_never_retained() {
        forget_ci_response_v1(7, "https://fixture/runs/3");
        let oversized = vec![b'a'; MAX_CACHED_CI_BODY_BYTES_V1 + 1];
        retain_ci_response_v1(7, "https://fixture/runs/3", &etag("W/fixture-3"), &oversized);
        assert!(cached_ci_response_v1(7, "https://fixture/runs/3").is_none());
    }

    #[test]
    fn a_refreshed_validator_replaces_the_etag_without_dropping_the_body() {
        forget_ci_response_v1(7, "https://fixture/runs/4");
        retain_ci_response_v1(7, "https://fixture/runs/4", &etag("W/fixture-4a"), b"{\"a\":4}");
        refresh_ci_response_etag_v1(7, "https://fixture/runs/4", &etag("W/fixture-4b"));
        assert_eq!(
            cached_ci_response_v1(7, "https://fixture/runs/4"),
            Some(CachedCiResponseV1 {
                etag: etag("W/fixture-4b"),
                body: b"{\"a\":4}".to_vec(),
            })
        );
        forget_ci_response_v1(7, "https://fixture/runs/4");
    }

    #[test]
    fn refreshing_an_absent_entry_never_invents_a_body() {
        forget_ci_response_v1(7, "https://fixture/runs/5");
        refresh_ci_response_etag_v1(7, "https://fixture/runs/5", &etag("W/fixture-5"));
        assert!(cached_ci_response_v1(7, "https://fixture/runs/5").is_none());
    }
}
