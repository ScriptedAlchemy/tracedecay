//! Client-owned conditional-response state for GitHub CI reads.
//!
//! A CI client retains bodies beside their provider ETags so an unchanged
//! response can be recovered after 304 Not Modified. The cache belongs to the
//! concrete client and is shared only by its clones; opening a client for a new
//! credential creates fresh state.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracedecay_domain::feedback::GitHubReviewEtagV1;

const MAX_CACHED_CI_BODY_BYTES_V1: usize = 2 * 1024 * 1024;
const MAX_CACHED_CI_TOTAL_BYTES_V1: usize = 16 * 1024 * 1024;
const MAX_CACHED_CI_ENTRIES_V1: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CachedCiResponseV1 {
    pub(super) etag: GitHubReviewEtagV1,
    pub(super) body: Arc<[u8]>,
    pub(super) revision: u64,
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

struct RetainedCiResponseV1 {
    response: CachedCiResponseV1,
    last_used: u64,
}

struct CiResponseCacheStateV1 {
    entries: BTreeMap<String, RetainedCiResponseV1>,
    total_bytes: usize,
    next_revision: u64,
    next_recency: u64,
}

impl Default for CiResponseCacheStateV1 {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            total_bytes: 0,
            next_revision: 1,
            next_recency: 1,
        }
    }
}

#[derive(Default)]
pub(super) struct CiResponseCacheV1 {
    state: Mutex<CiResponseCacheStateV1>,
}

impl CiResponseCacheV1 {
    pub(super) fn get(&self, url: &str) -> CiResponseCacheReadOutcomeV1 {
        let Ok(mut state) = self.state.lock() else {
            return CiResponseCacheReadOutcomeV1::Unavailable;
        };
        let Some(response) = state.entries.get(url).map(|entry| entry.response.clone()) else {
            return CiResponseCacheReadOutcomeV1::Miss;
        };
        if let Some(recency) = Self::take_recency(&mut state)
            && let Some(entry) = state.entries.get_mut(url)
        {
            entry.last_used = recency;
        }
        CiResponseCacheReadOutcomeV1::Hit(response)
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
        let Ok(mut state) = self.state.lock() else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        let Some(revision) = Self::take_revision(&mut state) else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        let Some(recency) = Self::take_recency(&mut state) else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        if let Some(previous) = state.entries.remove(url) {
            state.total_bytes = state
                .total_bytes
                .saturating_sub(previous.response.body.len());
        }
        while state.entries.len().saturating_add(1) > MAX_CACHED_CI_ENTRIES_V1
            || state.total_bytes.saturating_add(body.len()) > MAX_CACHED_CI_TOTAL_BYTES_V1
        {
            let Some(evicted) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(url, _)| url.clone())
            else {
                return CiResponseCacheWriteOutcomeV1::Unavailable;
            };
            if let Some(entry) = state.entries.remove(&evicted) {
                state.total_bytes = state.total_bytes.saturating_sub(entry.response.body.len());
            }
        }
        state.total_bytes = state.total_bytes.saturating_add(body.len());
        state.entries.insert(
            url.to_owned(),
            RetainedCiResponseV1 {
                response: CachedCiResponseV1 {
                    etag: etag.clone(),
                    body: Arc::from(body),
                    revision,
                },
                last_used: recency,
            },
        );
        CiResponseCacheWriteOutcomeV1::Stored
    }

    pub(super) fn refresh_etag_if_current(
        &self,
        url: &str,
        expected_revision: u64,
        expected_etag: &GitHubReviewEtagV1,
        new_etag: &GitHubReviewEtagV1,
    ) -> CiResponseCacheWriteOutcomeV1 {
        if expected_revision == 0
            || expected_etag.validate().is_err()
            || new_etag.validate().is_err()
        {
            return CiResponseCacheWriteOutcomeV1::Ignored;
        }
        let Ok(mut state) = self.state.lock() else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        if !state.entries.get(url).is_some_and(|entry| {
            entry.response.revision == expected_revision && entry.response.etag == *expected_etag
        }) {
            return CiResponseCacheWriteOutcomeV1::Ignored;
        }
        let Some(revision) = Self::take_revision(&mut state) else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        let Some(recency) = Self::take_recency(&mut state) else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        let Some(entry) = state.entries.get_mut(url) else {
            return CiResponseCacheWriteOutcomeV1::Ignored;
        };
        entry.response.etag = new_etag.clone();
        entry.response.revision = revision;
        entry.last_used = recency;
        CiResponseCacheWriteOutcomeV1::Stored
    }

    pub(super) fn forget(&self, url: &str) -> CiResponseCacheWriteOutcomeV1 {
        let Ok(mut state) = self.state.lock() else {
            return CiResponseCacheWriteOutcomeV1::Unavailable;
        };
        if let Some(entry) = state.entries.remove(url) {
            state.total_bytes = state.total_bytes.saturating_sub(entry.response.body.len());
        }
        CiResponseCacheWriteOutcomeV1::Stored
    }

    fn take_revision(state: &mut CiResponseCacheStateV1) -> Option<u64> {
        let revision = state.next_revision;
        state.next_revision = revision.checked_add(1)?;
        Some(revision)
    }

    fn take_recency(state: &mut CiResponseCacheStateV1) -> Option<u64> {
        let recency = state.next_recency;
        state.next_recency = recency.checked_add(1)?;
        Some(recency)
    }
}

#[cfg(test)]
mod tests {
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
    fn concurrent_cache_snapshots_share_retained_body_storage() {
        let cache = CiResponseCacheV1::default();
        cache.retain("https://fixture/runs/shared", &etag("E-shared"), b"shared");
        let CiResponseCacheReadOutcomeV1::Hit(first) = cache.get("https://fixture/runs/shared")
        else {
            panic!("expected first snapshot");
        };
        let CiResponseCacheReadOutcomeV1::Hit(second) = cache.get("https://fixture/runs/shared")
        else {
            panic!("expected second snapshot");
        };

        assert_eq!(first.body.as_ptr(), second.body.as_ptr());
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
    fn replacing_an_entry_at_capacity_preserves_unrelated_entries() {
        let cache = CiResponseCacheV1::default();
        for index in 0..MAX_CACHED_CI_ENTRIES_V1 {
            cache.retain(
                &format!("https://fixture/runs/{index}"),
                &etag(&format!("W/fixture-{index}")),
                b"{}",
            );
        }

        cache.retain(
            "https://fixture/runs/0",
            &etag("W/fixture-replaced"),
            b"{\"replacement\":true}",
        );

        assert!(
            matches!(
                cache.get(&format!(
                    "https://fixture/runs/{}",
                    MAX_CACHED_CI_ENTRIES_V1 - 1
                )),
                CiResponseCacheReadOutcomeV1::Hit(_)
            ),
            "replacing one key must not flush an unrelated cache entry"
        );
    }

    #[test]
    fn a_refreshed_validator_preserves_the_body() {
        let cache = CiResponseCacheV1::default();
        cache.retain("https://fixture/runs/3", &etag("W/fixture-3a"), b"{}");
        assert_eq!(
            cache.refresh_etag_if_current(
                "https://fixture/runs/3",
                1,
                &etag("W/fixture-3a"),
                &etag("W/fixture-3b"),
            ),
            CiResponseCacheWriteOutcomeV1::Stored
        );
        assert_eq!(
            cache.get("https://fixture/runs/3"),
            CiResponseCacheReadOutcomeV1::Hit(CachedCiResponseV1 {
                etag: etag("W/fixture-3b"),
                body: Arc::from(b"{}".as_slice()),
                revision: 2,
            })
        );
    }

    #[test]
    fn a_stale_304_cannot_rewrite_a_newer_body_validator() {
        let cache = Arc::new(CiResponseCacheV1::default());
        let url = "https://fixture/runs/stale-304";
        cache.retain(url, &etag("E1"), b"B1");
        let CiResponseCacheReadOutcomeV1::Hit(first) = cache.get(url) else {
            panic!("expected first cached response");
        };

        let retained = Arc::new(std::sync::Barrier::new(2));
        let newer_cache = Arc::clone(&cache);
        let newer_retained = Arc::clone(&retained);
        let newer = std::thread::spawn(move || {
            newer_cache.retain(url, &etag("E2"), b"B2");
            newer_retained.wait();
        });
        retained.wait();
        assert_eq!(
            cache.refresh_etag_if_current(url, first.revision, &first.etag, &etag("E1-refreshed")),
            CiResponseCacheWriteOutcomeV1::Ignored
        );
        newer.join().unwrap();
        assert_eq!(
            cache.get(url),
            CiResponseCacheReadOutcomeV1::Hit(CachedCiResponseV1 {
                etag: etag("E2"),
                body: Arc::from(b"B2".as_slice()),
                revision: 2,
            })
        );
    }

    #[test]
    fn poisoned_client_cache_is_typed_unavailable() {
        let cache = Arc::new(CiResponseCacheV1::default());
        let poison = Arc::clone(&cache);
        let _ = std::thread::spawn(move || {
            let _guard = poison.state.lock().unwrap();
            panic!("poison fixture");
        })
        .join();
        assert_eq!(
            cache.get("https://fixture/runs/4"),
            CiResponseCacheReadOutcomeV1::Unavailable
        );
    }
}
