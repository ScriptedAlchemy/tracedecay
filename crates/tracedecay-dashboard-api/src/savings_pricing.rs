//! Model pricing for the Savings & Cost dashboard tab.
//!
//! Price source order (cheapest sufficient wins, never blocks a request on
//! the network):
//!
//! 1. **Cached `OpenRouter` fetch** at `~/.tracedecay/model-prices.json`
//!    (override with `TRACEDECAY_MODEL_PRICES_PATH`). Served immediately even
//!    when stale; a background refresh re-fetches at most once per process
//!    when the file is older than 24h.
//! 2. **Bundled static snapshot** (`model_prices_fallback.json`, a curated
//!    subset of the live response) so the tab works offline / on first run.
//!
//! `TRACEDECAY_OFFLINE=1` skips the network entirely (cache/fallback only).
//!
//! The table is served raw to the UI (`GET /api/plugins/savings/pricing`);
//! fuzzy model-id → `OpenRouter`-slug resolution lives in the frontend
//! (`dashboard/savings/src/pricing.ts`), which labels unknown models as
//! "no price data" instead of guessing.
//!
//! # Two pricing tables exist on purpose — know which one you are reading
//!
//! This table prices **client-side estimates in the Savings & Cost tab**
//! only. Server-side cost accounting (`tracedecay cost` / `tracedecay gain` /
//! the `turns` table the dashboard reports as `cost_basis: "actual"`) is
//! priced by `accounting/pricing.rs` — a separate Claude-only `LiteLLM`
//! table with its own cache. The two sources can quote different USD for
//! the same model, so dashboard estimates and `tracedecay gain` output are
//! not guaranteed to match to the cent.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tracedecay_usecases::remote_json_cache::{
    cache_is_stale as cache_file_is_stale, file_mtime_unix, refresh_cached_json,
};

/// `OpenRouter` public model list (pricing metadata needs no authentication).
const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// Timeout for the background pricing fetch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// Cache TTL before a background refresh is attempted: 24 hours.
pub const CACHE_TTL_SECS: i64 = 86_400;

/// Set to `1` to disable all network access for pricing.
const OFFLINE_ENV: &str = "TRACEDECAY_OFFLINE";

/// Overrides the on-disk cache path (tests use a temp file).
const CACHE_PATH_ENV: &str = "TRACEDECAY_MODEL_PRICES_PATH";

/// Curated static snapshot of the `OpenRouter` response (same JSON shape).
const FALLBACK_JSON: &str = include_str!("model_prices_fallback.json");

/// USD per million tokens for one `OpenRouter` model.
#[derive(Debug, Clone, PartialEq)]
// The shared postfix is the unit; these names are the API contract the
// frontend price table consumes verbatim.
#[allow(clippy::struct_field_names)]
pub struct ModelPrice {
    pub prompt_per_mtok: f64,
    pub completion_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
}

/// A loaded pricing table plus provenance for honest UI labeling.
pub struct PriceTable {
    /// `OpenRouter` slug (e.g. `anthropic/claude-fable-5`) → per-MTok prices.
    pub models: BTreeMap<String, ModelPrice>,
    /// `"cache"` (disk copy of a live fetch) or `"fallback"` (bundled snapshot).
    pub source: &'static str,
    /// Unix mtime of the cache file backing the table (None for the snapshot).
    pub fetched_at: Option<i64>,
}

fn cache_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(CACHE_PATH_ENV)
        && !path.is_empty()
    {
        return Some(PathBuf::from(path));
    }
    let home = dirs::home_dir()?;
    Some(home.join(".tracedecay").join("model-prices.json"))
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
pub fn parse_openrouter_json(body: &str) -> Option<BTreeMap<String, ModelPrice>> {
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

/// Loads the current pricing table: disk cache first (served even when
/// stale), bundled snapshot otherwise. Cheap enough to call per request —
/// the dashboard is a local single-user server.
pub fn load_table() -> PriceTable {
    if let Some(path) = cache_path()
        && let Ok(body) = std::fs::read_to_string(&path)
        && let Some(models) = parse_openrouter_json(&body)
    {
        return PriceTable {
            models,
            source: "cache",
            fetched_at: file_mtime_unix(&path),
        };
    }
    PriceTable {
        models: parse_openrouter_json(FALLBACK_JSON).unwrap_or_default(),
        source: "fallback",
        fetched_at: None,
    }
}

/// True when the disk cache is missing or older than the TTL.
fn cache_is_stale() -> bool {
    cache_file_is_stale(cache_path().as_deref(), CACHE_TTL_SECS)
}

/// Fetches fresh pricing from `OpenRouter` and writes the cache file.
/// Best-effort: validates the payload before writing, returns `false` on any
/// failure (offline, timeout, bad body, unwritable cache). The
/// fetch/validate/write mechanism is shared with the root crate's `LiteLLM`
/// table — see `remote_json_cache`; only the source, timeout, parser and
/// cache path are ours.
fn refresh_pricing_blocking() -> bool {
    let Some(path) = cache_path() else {
        return false;
    };
    let agent = crate::cloud::agent_with_timeout(FETCH_TIMEOUT);
    refresh_cached_json(&agent, OPENROUTER_MODELS_URL, &path, |body| {
        parse_openrouter_json(body).is_some()
    })
}

/// Kicks off at most one background pricing refresh per process, and only
/// when the cache is stale and networking is allowed. Requests keep serving
/// the cached/static table while this runs — the fetch never blocks anyone.
pub fn ensure_background_refresh() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if offline() || !cache_is_stale() {
        return;
    }
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::task::spawn_blocking(|| {
        if refresh_pricing_blocking() {
            tracing::info!(provider = "openrouter", "refreshed savings model prices");
        }
    });
}

/// JSON payload for `GET /api/plugins/savings/pricing`.
pub fn pricing_payload() -> Value {
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
