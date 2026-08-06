//! Canonical all-provider model pricing shared by costs, CLI, MCP, and HTTP.
//!
//! Reads are deterministic and side-effect-free. The authority is the bundled
//! `OpenRouter` snapshot in `model_prices_fallback.json`; its content digest is
//! the pricing revision. Updating prices is an explicit source update, never a
//! request-triggered network fetch or an unregistered home-directory cache.
//!
//! Unknown models remain unpriced; callers expose that absence instead of
//! treating it as zero dollars.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Curated static snapshot of the `OpenRouter` response (same JSON shape).
const FALLBACK_JSON: &str = include_str!("model_prices_fallback.json");
static PRICE_TABLE: OnceLock<PriceTable> = OnceLock::new();

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
    /// Whether the bundled authority parsed into at least one usable row.
    pub available: bool,
    /// Stable reader-facing source label.
    pub source: &'static str,
    /// SHA-256 content identity of the bundled source snapshot.
    pub revision: String,
}

fn normalized_model(value: &str) -> String {
    value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn provider_vendor(provider: &str) -> Option<&'static str> {
    match provider.to_ascii_lowercase().as_str() {
        "claude" | "anthropic" => Some("anthropic"),
        "codex" | "openai" => Some("openai"),
        "gemini" | "google" => Some("google"),
        "grok" | "xai" => Some("x-ai"),
        "kimi" | "moonshot" => Some("moonshotai"),
        _ => None,
    }
}

/// Resolves a native provider/model identity against the one all-provider
/// table. Exact slugs win; otherwise the longest normalized model prefix wins
/// within the provider's vendor namespace.
pub fn resolve_model_price<'a>(
    table: &'a PriceTable,
    provider: &str,
    model: &str,
) -> Option<&'a ModelPrice> {
    if !table.available {
        return None;
    }
    let vendor = provider_vendor(provider)?;
    if let Some((model_vendor, _)) = model.split_once('/')
        && model_vendor != vendor
    {
        return None;
    }
    if let Some(exact) = table.models.get(model).filter(|_| {
        model
            .split_once('/')
            .is_some_and(|(model_vendor, _)| model_vendor == vendor)
    }) {
        return Some(exact);
    }
    let normalized = normalized_model(model);
    table
        .models
        .iter()
        .filter(|(slug, _)| {
            slug.split_once('/')
                .is_some_and(|(prefix, _)| prefix == vendor)
        })
        .filter_map(|(slug, price)| {
            let candidate = normalized_model(slug);
            (normalized == candidate || normalized.starts_with(&candidate))
                .then_some((candidate.len(), price))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, price)| price)
}

/// Prices one exact provider usage delta. Any missing required counter or
/// price component keeps the result unavailable instead of fabricating zero.
pub fn cost_of_usage(
    table: &PriceTable,
    provider: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
) -> Option<f64> {
    let price = resolve_model_price(table, provider, model)?;
    let optional_component = |tokens: Option<u64>, rate: Option<f64>| match (tokens, rate) {
        (Some(tokens), Some(rate)) => Some(tokens as f64 * rate),
        (Some(0) | None, None) => Some(0.0),
        _ => None,
    };
    let (prompt_tokens, cache_read_cost) =
        if matches!(provider.to_ascii_lowercase().as_str(), "codex" | "openai") {
            let cached = cache_read_tokens?;
            let uncached = input_tokens.checked_sub(cached)?;
            let cache_rate = price.cache_read_per_mtok?;
            (uncached, cached as f64 * cache_rate)
        } else {
            (
                input_tokens,
                optional_component(cache_read_tokens, price.cache_read_per_mtok)?,
            )
        };
    let per_million = prompt_tokens as f64 * price.prompt_per_mtok
        + output_tokens as f64 * price.completion_per_mtok
        + cache_read_cost
        + optional_component(cache_write_tokens, price.cache_write_per_mtok)?;
    let cost = per_million / 1_000_000.0;
    cost.is_finite().then_some(cost)
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

/// Returns the deterministic bundled pricing authority.
///
/// Parsing and content hashing happen once per process. The bundled bytes are
/// immutable, so rebuilding the same map on each hook or dashboard read would
/// add latency without providing fresher pricing.
pub fn load_table() -> &'static PriceTable {
    PRICE_TABLE.get_or_init(|| {
        let (available, models) = match parse_openrouter_json(FALLBACK_JSON) {
            Some(models) => (true, models),
            None => (false, BTreeMap::new()),
        };
        PriceTable {
            available,
            models,
            source: "bundled",
            revision: format!(
                "sha256:{}",
                hex::encode(Sha256::digest(FALLBACK_JSON.as_bytes()))
            ),
        }
    })
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
    let (model_count, models, error) = if table.available {
        (
            json!(table.models.len()),
            Value::Object(models),
            Value::Null,
        )
    } else {
        (Value::Null, Value::Null, json!("bundled_pricing_invalid"))
    };
    json!({
        "source": table.source,
        "revision": table.revision,
        "fetched_at": Value::Null,
        "offline": true,
        "cache_path": Value::Null,
        "model_count": model_count,
        "models": models,
        "error": error,
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
    fn canonical_resolver_prices_claude_and_codex_from_one_table() {
        let table = load_table();
        assert!(table.available);
        assert!(table.revision.starts_with("sha256:"));
        assert!(resolve_model_price(&table, "claude", "claude-sonnet-4-6-20260801").is_some());
        assert!(resolve_model_price(&table, "codex", "gpt-5.3-codex").is_some());
        assert!(resolve_model_price(&table, "claude", "openai/gpt-5.3-codex").is_none());
    }

    #[test]
    fn unavailable_model_price_is_none_not_zero_cost() {
        let table = load_table();
        assert_eq!(
            cost_of_usage(
                &table,
                "unknown-provider",
                "unknown-model",
                1_000,
                100,
                None,
                None,
            ),
            None
        );
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

    #[test]
    fn codex_cached_input_is_not_charged_twice() {
        let table = PriceTable {
            models: BTreeMap::from([(
                "openai/gpt-fixture".to_owned(),
                ModelPrice {
                    prompt_per_mtok: 10.0,
                    completion_per_mtok: 20.0,
                    cache_read_per_mtok: Some(2.0),
                    cache_write_per_mtok: None,
                },
            )]),
            available: true,
            source: "fixture",
            revision: "fixture".to_owned(),
        };

        let cost =
            cost_of_usage(&table, "codex", "gpt-fixture", 1_000, 100, Some(400), None).unwrap();
        assert!((cost - 0.0088).abs() < f64::EPSILON);
        assert_eq!(
            cost_of_usage(&table, "codex", "gpt-fixture", 100, 0, Some(101), None,),
            None
        );
        assert_eq!(
            cost_of_usage(&table, "codex", "gpt-fixture", 100, 0, None, None),
            None
        );
    }
}
