//! Client-owned conditional-response state for GitHub CI reads.
//!
//! A CI client retains bodies beside their provider ETags so an unchanged
//! response can be recovered after 304 Not Modified. The cache belongs to the
//! concrete client and is shared only by its clones; opening a client for a new
//! credential creates fresh state.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tracedecay_domain::feedback::GitHubReviewEtagV1;

const MAX_CACHED_CI_BODY_BYTES_V1: usize = 2 * 1024 * 1024;
const MAX_CACHED_CI_TOTAL_BYTES_V1: usize = 16 * 1024 * 1024;
const MAX_CACHED_CI_ENTRIES_V1: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CachedCiResponseV1 {
    pub(super) etag: GitHubReviewEtagV1,
    pub(super) body: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CiResponseCacheReadOutcomeV1 {
    Hit(CachedCiResponseV1),
    Miss,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CiResponseCacheWriteOutcomeV1 {
    Stored,
    Ignored,
    Unavailable,
}

#[derive(Default)]
pub(super) struct CiResponseCacheV1 {
    entries: Mutex<BTreeMap<String, CachedCiResponseV1>>,
}

impl CiResponseCacheV1 {
    pub(super) fn get(&self, url: &str) -> CiResponseCacheReadOutcomeV1 {
        let Ok(entries) = self.entries.lock() else {
            return CiResponseCacheReadOutcomeV1::Unavailable;
        };
        entries.get(url).cloned().map_or(
            CiResponseCacheReadOutcomeV1::Miss,
            CiResponseCacheReadOutcomeV1::Hit,
        )
    }

    pub(super) fn retain(
        &self,
        url: &str,
        etag: &GitHubReviewEtagV1,
        body: &[u8],
    ) -> CiResponseCacheWriteOutcomeV1 {
        if body.is_empty() || body.len() > MAX_CACHED_CI_BODY_BYTES_V1 || etag.validate().is_err() {
            return CiResponseCacheWriteOutcomeV1::Ignored;
        }
        let Ok(mut entries) = self.entries.lock() else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        let retained_bytes = entries
            .values()
            .map(|entry| entry.body.len())
            .fold(0_usize, usize::saturating_add);
        if entries.len() >= MAX_CACHED_CI_ENTRIES_V1
            || retained_bytes.saturating_add(body.len()) > MAX_CACHED_CI_TOTAL_BYTES_V1
        {
            entries.clear();
        }
        entries.insert(
            url.to_owned(),
            CachedCiResponseV1 {
                etag: etag.clone(),
                body: body.to_vec(),
            },
        );
        CiResponseCacheWriteOutcomeV1::Stored
    }

    pub(super) fn refresh_etag(
        &self,
        url: &str,
        etag: &GitHubReviewEtagV1,
    ) -> CiResponseCacheWriteOutcomeV1 {
        if etag.validate().is_err() {
            return CiResponseCacheWriteOutcomeV1::Ignored;
        }
        let Ok(mut entries) = self.entries.lock() else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        let Some(entry) = entries.get_mut(url) else {
            return CiResponseCacheWriteOutcomeV1::Ignored;
        };
        entry.etag = etag.clone();
        CiResponseCacheWriteOutcomeV1::Stored
    }

    pub(super) fn forget(&self, url: &str) -> CiResponseCacheWriteOutcomeV1 {
        let Ok(mut entries) = self.entries.lock() else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        entries.remove(url);
        CiResponseCacheWriteOutcomeV1::Stored
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn etag(value: &str) -> GitHubReviewEtagV1 {
        GitHubReviewEtagV1::new(value).unwrap()
    }

    #[test]
    fn retained_bodies_are_local_to_one_client_cache() {
        let first = CiResponseCacheV1::default();
        let second = CiResponseCacheV1::default();
        assert_eq!(
            first.retain("https://fixture/runs/1", &etag("W/fixture-1"), b"{\"a\":1}"),
            CiResponseCacheWriteOutcomeV1::Stored
        );
        assert!(matches!(
            first.get("https://fixture/runs/1"),
            CiResponseCacheReadOutcomeV1::Hit(_)
        ));
        assert_eq!(
            second.get("https://fixture/runs/1"),
            CiResponseCacheReadOutcomeV1::Miss
        );
    }

    #[test]
    fn oversized_bodies_are_not_retained() {
        let cache = CiResponseCacheV1::default();
        let oversized = vec![b'a'; MAX_CACHED_CI_BODY_BYTES_V1 + 1];
        assert_eq!(
            cache.retain("https://fixture/runs/2", &etag("W/fixture-2"), &oversized),
            CiResponseCacheWriteOutcomeV1::Ignored
        );
        assert_eq!(
            cache.get("https://fixture/runs/2"),
            CiResponseCacheReadOutcomeV1::Miss
        );
    }

    #[test]
    fn a_refreshed_validator_preserves_the_body() {
        let cache = CiResponseCacheV1::default();
        cache.retain("https://fixture/runs/3", &etag("W/fixture-3a"), b"{}");
        assert_eq!(
            cache.refresh_etag("https://fixture/runs/3", &etag("W/fixture-3b")),
            CiResponseCacheWriteOutcomeV1::Stored
        );
        assert_eq!(
            cache.get("https://fixture/runs/3"),
            CiResponseCacheReadOutcomeV1::Hit(CachedCiResponseV1 {
                etag: etag("W/fixture-3b"),
                body: b"{}".to_vec(),
            })
        );
    }

    #[test]
    fn poisoned_client_cache_is_typed_unavailable() {
        let cache = Arc::new(CiResponseCacheV1::default());
        let poison = Arc::clone(&cache);
        let _ = std::thread::spawn(move || {
            let _guard = poison.entries.lock().unwrap();
            panic!("poison fixture");
        })
        .join();
        assert_eq!(
            cache.get("https://fixture/runs/4"),
            CiResponseCacheReadOutcomeV1::Unavailable
        );
    }
}
