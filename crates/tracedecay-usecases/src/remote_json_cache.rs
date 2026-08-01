//! TTL'd on-disk cache of a remote JSON table.
//!
//! The two model-pricing tables quote deliberately different sources —
//! `accounting::pricing` prices Claude turns from `LiteLLM`, the dashboard's
//! `savings_pricing` prices all-vendor estimates from `OpenRouter` — but they
//! kept identical *mechanism*: GET a public JSON URL, refuse to cache a body
//! that does not parse, write it beside its parent directories, and decide
//! staleness from the cache file's age. That mechanism lives here so the two
//! tables cannot drift apart on the parts that were never meant to differ.
//!
//! Everything that *is* meant to differ stays with each caller: the URL, the
//! request timeout, the parser, the cache path (and any env override), the
//! TTL bookkeeping, the offline gate, and whether the refresh runs inline or
//! on a background task.

use std::path::Path;

/// Fetches `url` through `agent` and writes the response body to
/// `cache_path`, but only when `body_is_usable` accepts it — an unparsable
/// response must never replace a good cache. Parent directories are created
/// on demand.
///
/// Returns `true` only when the cache file was written. Best-effort by
/// design: every failure (network error, timeout, unusable body, unwritable
/// path) returns `false` and leaves any existing cache intact.
pub fn refresh_cached_json(
    agent: &ureq::Agent,
    url: &str,
    cache_path: &Path,
    body_is_usable: impl FnOnce(&str) -> bool,
) -> bool {
    let Ok(mut resp) = agent.get(url).call() else {
        return false;
    };
    let Ok(body) = resp.body_mut().read_to_string() else {
        return false;
    };
    if !body_is_usable(&body) {
        return false;
    }
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(cache_path, body).is_ok()
}

/// Seconds since the Unix epoch; `0` if the clock reads before it.
#[must_use]
pub fn unix_now() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// Unix mtime of a file, when readable.
#[must_use]
pub fn file_mtime_unix(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(secs).ok()
}

/// True when the cache file is absent, unreadable, or at least `ttl_secs`
/// old. A caller with no resolvable cache path is always stale.
#[must_use]
pub fn cache_is_stale(cache_path: Option<&Path>, ttl_secs: i64) -> bool {
    match cache_path.and_then(file_mtime_unix) {
        Some(mtime) => unix_now() - mtime >= ttl_secs,
        None => true,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn missing_cache_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.json");
        assert!(cache_is_stale(Some(&missing), 86_400));
        assert!(cache_is_stale(None, 86_400));
    }

    #[test]
    fn fresh_cache_is_not_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prices.json");
        std::fs::write(&path, "{}").unwrap();
        assert!(!cache_is_stale(Some(&path), 86_400));
        // A zero TTL makes any existing file stale immediately.
        assert!(cache_is_stale(Some(&path), 0));
    }

    #[test]
    fn mtime_is_readable_and_recent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prices.json");
        std::fs::write(&path, "{}").unwrap();
        let mtime = file_mtime_unix(&path).unwrap();
        assert!((unix_now() - mtime).abs() < 60);
        assert!(file_mtime_unix(&dir.path().join("absent.json")).is_none());
    }
}
