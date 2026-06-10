//! Model pricing for the Savings & Cost dashboard tab.
//!
//! Price source order (cheapest sufficient wins, never blocks a request on
//! the network):
//!
//! 1. **Cached `OpenRouter` fetch** at `~/.tokensave/model-prices.json`
//!    (override with `TOKENSAVE_MODEL_PRICES_PATH`). Served immediately even
//!    when stale; a background refresh re-fetches at most once per process
//!    when the file is older than 24h.
//! 2. **Bundled static snapshot** (`model_prices_fallback.json`, a curated
//!    subset of the live response) so the tab works offline / on first run.
//!
//! `TOKENSAVE_OFFLINE=1` skips the network entirely (cache/fallback only).
//!
//! The table is served raw to the UI (`GET /api/plugins/savings/pricing`);
//! fuzzy model-id → `OpenRouter`-slug resolution lives in the frontend
//! (`dashboard/savings/src/pricing.ts`), which labels unknown models as
//! "no price data" instead of guessing.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Map, Value};

/// `OpenRouter` public model list (pricing metadata needs no authentication).
const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// Timeout for the background pricing fetch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// Cache TTL before a background refresh is attempted: 24 hours.
pub(crate) const CACHE_TTL_SECS: i64 = 86_400;

/// Set to `1` to disable all network access for pricing.
const OFFLINE_ENV: &str = "TOKENSAVE_OFFLINE";

/// Overrides the on-disk cache path (tests use a temp file).
const CACHE_PATH_ENV: &str = "TOKENSAVE_MODEL_PRICES_PATH";

/// Curated static snapshot of the `OpenRouter` response (same JSON shape).
const FALLBACK_JSON: &str = include_str!("model_prices_fallback.json");

/// USD per million tokens for one `OpenRouter` model.
#[derive(Debug, Clone, PartialEq)]
// The shared postfix is the unit; these names are the API contract the
// frontend price table consumes verbatim.
#[allow(clippy::struct_field_names)]
pub(crate) struct ModelPrice {
    pub(crate) prompt_per_mtok: f64,
    pub(crate) completion_per_mtok: f64,
    pub(crate) cache_read_per_mtok: Option<f64>,
    pub(crate) cache_write_per_mtok: Option<f64>,
}

/// A loaded pricing table plus provenance for honest UI labeling.
pub(crate) struct PriceTable {
    /// `OpenRouter` slug (e.g. `anthropic/claude-fable-5`) → per-MTok prices.
    pub(crate) models: BTreeMap<String, ModelPrice>,
    /// `"cache"` (disk copy of a live fetch) or `"fallback"` (bundled snapshot).
    pub(crate) source: &'static str,
    /// Unix mtime of the cache file backing the table (None for the snapshot).
    pub(crate) fetched_at: Option<i64>,
}

fn cache_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(CACHE_PATH_ENV) {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    dirs::home_dir().map(|h| h.join(".tokensave").join("model-prices.json"))
}

fn offline() -> bool {
    std::env::var(OFFLINE_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Reads a price field that `OpenRouter` serves as a per-token decimal string
/// (sometimes a bare number) and converts it to USD per million tokens.
fn price_per_mtok(pricing: &Value, key: &str) -> Option<f64> {
    let raw = pricing.get(key)?;
    let per_token = match raw {
        Value::String(s) => s.parse::<f64>().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    if !per_token.is_finite() || per_token < 0.0 {
        return None;
    }
    Some(per_token * 1_000_000.0)
}

/// Parses an `OpenRouter` `/api/v1/models` response (or the bundled snapshot,
/// which uses the identical shape) into a slug → price map. Returns `None`
/// when nothing usable was found, so callers never cache garbage.
pub(crate) fn parse_openrouter_json(body: &str) -> Option<BTreeMap<String, ModelPrice>> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    let entries = parsed.get("data")?.as_array()?;

    let mut models = BTreeMap::new();
    for entry in entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        // `~vendor/model-latest` ids are floating aliases; skip them so the
        // table only carries stable slugs.
        if id.starts_with('~') {
            continue;
        }
        let Some(pricing) = entry.get("pricing") else {
            continue;
        };
        let prompt = price_per_mtok(pricing, "prompt");
        let completion = price_per_mtok(pricing, "completion");
        let (Some(prompt), Some(completion)) = (prompt, completion) else {
            continue;
        };
        models.insert(
            id.to_string(),
            ModelPrice {
                prompt_per_mtok: prompt,
                completion_per_mtok: completion,
                cache_read_per_mtok: price_per_mtok(pricing, "input_cache_read"),
                cache_write_per_mtok: price_per_mtok(pricing, "input_cache_write"),
            },
        );
    }

    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

/// Unix mtime of a file, when readable.
fn file_mtime_unix(path: &std::path::Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(secs).ok()
}

/// Loads the current pricing table: disk cache first (served even when
/// stale), bundled snapshot otherwise. Cheap enough to call per request —
/// the dashboard is a local single-user server.
pub(crate) fn load_table() -> PriceTable {
    if let Some(path) = cache_path() {
        if let Ok(body) = std::fs::read_to_string(&path) {
            if let Some(models) = parse_openrouter_json(&body) {
                return PriceTable {
                    models,
                    source: "cache",
                    fetched_at: file_mtime_unix(&path),
                };
            }
        }
    }
    PriceTable {
        models: parse_openrouter_json(FALLBACK_JSON).unwrap_or_default(),
        source: "fallback",
        fetched_at: None,
    }
}

/// True when the disk cache is missing or older than the TTL.
fn cache_is_stale() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    match cache_path().as_deref().and_then(file_mtime_unix) {
        Some(mtime) => now - mtime >= CACHE_TTL_SECS,
        None => true,
    }
}

/// Fetches fresh pricing from `OpenRouter` and writes the cache file.
/// Best-effort: validates the payload before writing, returns `false` on any
/// failure (offline, timeout, bad body, unwritable cache).
fn refresh_pricing_blocking() -> bool {
    let agent = crate::cloud::agent_with_timeout(FETCH_TIMEOUT);
    let Ok(mut resp) = agent.get(OPENROUTER_MODELS_URL).call() else {
        return false;
    };
    let Ok(body) = resp.body_mut().read_to_string() else {
        return false;
    };
    if parse_openrouter_json(&body).is_none() {
        return false;
    }
    let Some(path) = cache_path() else {
        return false;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, body).is_ok()
}

/// Kicks off at most one background pricing refresh per process, and only
/// when the cache is stale and networking is allowed. Requests keep serving
/// the cached/static table while this runs — the fetch never blocks anyone.
pub(crate) fn ensure_background_refresh() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if offline() || !cache_is_stale() {
        return;
    }
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::task::spawn_blocking(|| {
        if refresh_pricing_blocking() {
            eprintln!("Savings dashboard: refreshed model prices from OpenRouter.");
        }
    });
}

/// JSON payload for `GET /api/plugins/savings/pricing`.
pub(crate) fn pricing_payload() -> Value {
    let table = load_table();
    let mut models = Map::new();
    for (slug, price) in &table.models {
        models.insert(
            slug.clone(),
            json!({
                "prompt_per_mtok": price.prompt_per_mtok,
                "completion_per_mtok": price.completion_per_mtok,
                "cache_read_per_mtok": price.cache_read_per_mtok,
                "cache_write_per_mtok": price.cache_write_per_mtok,
            }),
        );
    }
    json!({
        "source": table.source,
        "fetched_at": table.fetched_at,
        "ttl_secs": CACHE_TTL_SECS,
        "offline": offline(),
        "cache_path": cache_path().map(|p| p.display().to_string()),
        "model_count": table.models.len(),
        "models": Value::Object(models),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn fallback_snapshot_parses_and_covers_common_vendors() {
        let models = parse_openrouter_json(FALLBACK_JSON).unwrap();
        assert!(models.len() > 50, "snapshot too small: {}", models.len());
        for slug in [
            "anthropic/claude-fable-5",
            "anthropic/claude-opus-4.8",
            "openai/gpt-5.5",
            "openai/gpt-5.3-codex",
            "google/gemini-3.5-flash",
        ] {
            let price = models.get(slug).unwrap_or_else(|| panic!("missing {slug}"));
            assert!(price.prompt_per_mtok > 0.0);
            assert!(price.completion_per_mtok > 0.0);
        }
    }

    #[test]
    fn parse_converts_per_token_strings_to_per_mtok() {
        let body = r#"{"data": [
            {"id": "vendor/model-a",
             "pricing": {"prompt": "0.000003", "completion": "1.5e-05",
                         "input_cache_read": "3e-07"}},
            {"id": "~vendor/model-latest",
             "pricing": {"prompt": "0.000003", "completion": "0.000015"}},
            {"id": "vendor/free-model",
             "pricing": {"prompt": "0", "completion": "0"}},
            {"id": "vendor/broken", "pricing": {"prompt": "n/a"}}
        ]}"#;
        let models = parse_openrouter_json(body).unwrap();
        assert_eq!(models.len(), 2, "alias skipped, broken skipped");
        let a = &models["vendor/model-a"];
        assert!((a.prompt_per_mtok - 3.0).abs() < 1e-9);
        assert!((a.completion_per_mtok - 15.0).abs() < 1e-9);
        assert!((a.cache_read_per_mtok.unwrap() - 0.3).abs() < 1e-9);
        assert!(a.cache_write_per_mtok.is_none());
        // Free models stay listed (zero price is a real price).
        assert!(models.contains_key("vendor/free-model"));
    }

    #[test]
    fn parse_rejects_unusable_bodies() {
        assert!(parse_openrouter_json("not json").is_none());
        assert!(parse_openrouter_json("{}").is_none());
        assert!(parse_openrouter_json(r#"{"data": []}"#).is_none());
    }
}
