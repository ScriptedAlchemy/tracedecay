//! Tokenizer-backed token counting for the Savings & Cost tab.
//!
//! Cost estimation has three quality tiers (best wins per message):
//!
//! 1. **actual** — the transcript recorded real usage data.
//! 2. **tokenized** — no usage data, but the stored text is counted with a
//!    real BPE tokenizer (tiktoken). Exact for OpenAI-family models
//!    (`o200k_base` / `cl100k_base` per family); for other vendors
//!    (Claude/Gemini have no public tokenizer) `o200k_base` serves as a
//!    much-better-than-chars/4 approximation and is labeled as such.
//! 3. **estimated** — the legacy `(len+3)/4` chars/4 heuristic, used when
//!    the `token-counting` feature is compiled out (or a count failed).
//!
//! Counting 15k+ stored messages per request would be far too slow, so
//! counts are cached at two levels keyed by `(provider, message_id)` with a
//! `text_len` guard: an in-process map on [`TokenCountCache`], persisted in
//! the `dashboard_token_counts` sidecar table of the **global accounting
//! DB** (dashboard scope — the session-store schema is never touched).
//! A background warm task runs at dashboard startup so the first paint of
//! the Savings tab doesn't pay the initial counting cost.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::util::{qmarks, query_rows};
use super::DashboardState;
use crate::global_db::TokenCountUpsert;

#[cfg(feature = "token-counting")]
use tiktoken_rs::{cl100k_base_singleton, o200k_base_singleton};

/// Per-message provenance + token columns, derived once and reused by every
/// savings aggregate. Usage fields accept both the Anthropic
/// (`input_tokens`/`output_tokens`) and `OpenAI` (`prompt_tokens`/
/// `completion_tokens`) transcript shapes; `json_valid` guards keep one
/// malformed `metadata_json` row from failing the whole query.
pub(super) const MESSAGE_TOKENS_CTE: &str = "
    SELECT provider,
           message_id,
           session_id,
           role,
           timestamp,
           TRIM(COALESCE(model, '')) AS model,
           LENGTH(COALESCE(text, '')) AS msg_len,
           (LENGTH(COALESCE(text, '')) + 3) / 4 AS est_tokens,
           CASE WHEN json_valid(metadata_json) THEN
               CAST(COALESCE(json_extract(metadata_json, '$.usage.input_tokens'),
                             json_extract(metadata_json, '$.usage.prompt_tokens')) AS INTEGER)
           END AS usage_in,
           CASE WHEN json_valid(metadata_json) THEN
               CAST(COALESCE(json_extract(metadata_json, '$.usage.output_tokens'),
                             json_extract(metadata_json, '$.usage.completion_tokens')) AS INTEGER)
           END AS usage_out,
           CASE WHEN json_valid(metadata_json) THEN
               CAST(json_extract(metadata_json, '$.usage.cache_read_input_tokens') AS INTEGER)
           END AS usage_cache_read,
           CASE WHEN json_valid(metadata_json) THEN
               CAST(json_extract(metadata_json, '$.usage.cache_creation_input_tokens') AS INTEGER)
           END AS usage_cache_write
    FROM session_messages";

/// Which BPE vocabulary a model id maps to, and whether the resulting count
/// is exact (the model's real tokenizer) or a labeled approximation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModelEncoder {
    pub name: &'static str,
    pub exact: bool,
}

pub(crate) const O200K: &str = "o200k_base";
pub(crate) const CL100K: &str = "cl100k_base";

/// Maps a transcript model id to its tokenizer.
///
/// `OpenAI` families are exact: GPT-5 / GPT-4o / GPT-4.1 / GPT-4.5,
/// o-series, codex, and gpt-oss use `o200k_base`; legacy GPT-4 / GPT-3.5 /
/// embeddings use `cl100k_base`. Everything else (Claude, Gemini, Grok, …)
/// has no public tokenizer, so `o200k_base` is used as an approximation
/// with `exact: false` so the UI can label it honestly.
pub(crate) fn encoder_for_model(model: &str) -> ModelEncoder {
    let id = model.trim().to_ascii_lowercase();
    let exact_o200k = id.starts_with("gpt-5")
        || id.starts_with("gpt-4o")
        || id.starts_with("gpt-4.1")
        || id.starts_with("gpt-4.5")
        || id.starts_with("gpt-oss")
        || id.starts_with("chatgpt")
        || id.starts_with("codex")
        || matches!(id.as_str(), "o1" | "o3" | "o4")
        || ["o1-", "o3-", "o4-"].iter().any(|p| id.starts_with(p));
    if exact_o200k {
        return ModelEncoder {
            name: O200K,
            exact: true,
        };
    }
    if id.starts_with("gpt-4") || id.starts_with("gpt-3.5") || id.starts_with("text-embedding") {
        return ModelEncoder {
            name: CL100K,
            exact: true,
        };
    }
    ModelEncoder {
        name: O200K,
        exact: false,
    }
}

/// `true` when the binary was built with the `token-counting` feature.
pub(crate) fn counting_available() -> bool {
    cfg!(feature = "token-counting")
}

/// Counts `text` with the BPE the model maps to. The singletons decode the
/// embedded vocabularies lazily, so the first call pays the init cost and
/// builds without the feature never do.
#[cfg(feature = "token-counting")]
pub(crate) fn count_text_tokens(text: &str, model: &str) -> i64 {
    let bpe = match encoder_for_model(model).name {
        CL100K => cl100k_base_singleton(),
        _ => o200k_base_singleton(),
    };
    bpe.encode_ordinary(text).len() as i64
}

#[cfg(not(feature = "token-counting"))]
pub(crate) fn count_text_tokens(_text: &str, _model: &str) -> i64 {
    0
}

/// Legacy chars/4 estimate, matching the SQL `(LENGTH(text)+3)/4`.
fn chars_estimate(len: i64) -> i64 {
    (len + 3) / 4
}

#[derive(Debug, Clone, Copy)]
struct CachedCount {
    text_len: i64,
    tokens: i64,
}

/// Cached non-usage overlay plus the `session_messages` fingerprint it was
/// built from.
struct OverlayCache {
    /// `(COUNT(*), MAX(rowid))` of `session_messages` at build time. Inserts
    /// change both; deletes change the count. (In-place row rewrites that
    /// keep count and rowid are not detected — ingest only inserts.)
    fingerprint: (i64, i64),
    overlay: Arc<Vec<MessageTokens>>,
}

/// Process-lifetime token-count cache shared by all savings endpoints.
pub(crate) struct TokenCountCache {
    map: Mutex<HashMap<(String, String), CachedCount>>,
    hydrated: AtomicBool,
    /// Last built non-usage overlay; `/overview`, `/sessions`, and `/models`
    /// all need it, so without this every Savings-tab interaction re-ran the
    /// full `session_messages` scan + fold three times.
    overlay: tokio::sync::Mutex<Option<OverlayCache>>,
}

impl TokenCountCache {
    pub(crate) fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            hydrated: AtomicBool::new(false),
            overlay: tokio::sync::Mutex::new(None),
        }
    }
}

/// One stored message without transcript usage data, carrying its
/// best-available token count.
#[derive(Debug, Clone)]
pub(crate) struct MessageTokens {
    pub provider: String,
    pub session_id: String,
    /// Normalized like the SQL CTE: `""` when no model id was recorded.
    pub model: String,
    pub role: String,
    pub timestamp: Option<i64>,
    pub tokens: i64,
    /// `true` when `tokens` came from the BPE (tier 2), `false` for the
    /// chars/4 fallback (tier 3).
    pub tokenized: bool,
}

fn row_str(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Builds the non-usage overlay: every stored message lacking transcript
/// usage data, with cached-or-computed token counts. Returns `None` when no
/// session store is being served (callers fall back to the SQL estimates).
///
/// The result is cached on [`TokenCountCache`] keyed by a cheap
/// `(COUNT(*), MAX(rowid))` fingerprint of `session_messages`; the cache
/// lock is held across a rebuild so the three savings endpoints firing
/// concurrently share one scan instead of racing three.
pub(crate) async fn non_usage_message_tokens(
    state: &DashboardState,
) -> Option<Arc<Vec<MessageTokens>>> {
    let conn = state.lcm_conn.as_ref()?;

    let fingerprint = overlay_fingerprint(conn).await?;
    let mut cached = state.token_counts.overlay.lock().await;
    if let Some(existing) = cached.as_ref() {
        if existing.fingerprint == fingerprint {
            return Some(existing.overlay.clone());
        }
    }

    let overlay = Arc::new(build_overlay(state, conn).await?);
    *cached = Some(OverlayCache {
        fingerprint,
        overlay: overlay.clone(),
    });
    Some(overlay)
}

/// `(COUNT(*), MAX(rowid))` of `session_messages` — see [`OverlayCache`].
async fn overlay_fingerprint(conn: &libsql::Connection) -> Option<(i64, i64)> {
    let rows = query_rows(
        conn,
        "SELECT COUNT(*) AS count, COALESCE(MAX(rowid), 0) AS max_rowid FROM session_messages",
        (),
    )
    .await
    .ok()?;
    let row = rows.first()?;
    Some((
        row.get("count").and_then(Value::as_i64).unwrap_or(0),
        row.get("max_rowid").and_then(Value::as_i64).unwrap_or(0),
    ))
}

async fn build_overlay(
    state: &DashboardState,
    conn: &libsql::Connection,
) -> Option<Vec<MessageTokens>> {
    // Metadata only — text never leaves SQLite unless a count is missing.
    let sql = format!(
        "SELECT provider, message_id, session_id, role, timestamp, model, msg_len
         FROM ({MESSAGE_TOKENS_CTE}) WHERE usage_in IS NULL AND usage_out IS NULL"
    );
    let rows = query_rows(conn, &sql, ()).await.ok()?;

    hydrate_cache(state).await;

    // Resolve cache hits and collect misses without holding the lock
    // across any await point.
    let mut misses: Vec<(String, String, String, i64)> = Vec::new();
    {
        let map = state
            .token_counts
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for row in &rows {
            let provider = row_str(row, "provider");
            let message_id = row_str(row, "message_id");
            let len = row.get("msg_len").and_then(Value::as_i64).unwrap_or(0);
            let key = (provider, message_id);
            let stale = map.get(&key).is_none_or(|c| c.text_len != len);
            if stale && counting_available() && len > 0 {
                misses.push((key.0, key.1, row_str(row, "model"), len));
            }
        }
    }

    if !misses.is_empty() {
        count_and_store(state, conn, misses).await;
    }

    let map = state
        .token_counts
        .map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let overlay = rows
        .iter()
        .map(|row| {
            let provider = row_str(row, "provider");
            let message_id = row_str(row, "message_id");
            let len = row.get("msg_len").and_then(Value::as_i64).unwrap_or(0);
            let cached = map
                .get(&(provider.clone(), message_id))
                .filter(|c| c.text_len == len);
            MessageTokens {
                provider,
                session_id: row_str(row, "session_id"),
                model: row_str(row, "model"),
                role: row_str(row, "role"),
                timestamp: row.get("timestamp").and_then(Value::as_i64),
                tokens: cached.map_or_else(|| chars_estimate(len), |c| c.tokens),
                tokenized: cached.is_some(),
            }
        })
        .collect();
    Some(overlay)
}

/// One-time hydrate of the in-memory map from the sidecar table.
async fn hydrate_cache(state: &DashboardState) {
    if state.token_counts.hydrated.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(gdb) = state.savings_db.as_deref() else {
        return;
    };
    if !gdb.ensure_token_count_cache().await {
        return;
    }
    let persisted = gdb.load_token_counts(&state.lcm_db_path).await;
    let mut map = state
        .token_counts
        .map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (provider, message_id, text_len, tokens) in persisted {
        map.insert((provider, message_id), CachedCount { text_len, tokens });
    }
}

/// Fetches the text of `misses` in per-provider chunks, counts off the async
/// runtime, then updates both cache levels.
///
/// Chunks are keyed `(provider, message_id)` so the lookup can use the
/// table's composite primary key — a `message_id IN (…)` filter alone cannot,
/// and full-scanned the text-heavy table once per 200-row chunk (a 15k-message
/// first warm paid ~75 full scans).
async fn count_and_store(
    state: &DashboardState,
    conn: &libsql::Connection,
    mut misses: Vec<(String, String, String, i64)>,
) {
    const CHUNK: usize = 200;
    let mut computed: Vec<TokenCountUpsert> = Vec::with_capacity(misses.len());

    misses.sort_by(|a, b| a.0.cmp(&b.0));
    for chunk in misses
        .chunk_by(|a, b| a.0 == b.0)
        .flat_map(|group| group.chunks(CHUNK))
    {
        let placeholders = qmarks(chunk.len());
        let sql = format!(
            "SELECT provider, message_id, COALESCE(text, '') AS text
             FROM session_messages WHERE provider = ? AND message_id IN ({placeholders})"
        );
        let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 1);
        params.push(libsql::Value::Text(chunk[0].0.clone()));
        params.extend(
            chunk
                .iter()
                .map(|(_, message_id, _, _)| libsql::Value::Text(message_id.clone())),
        );
        let Ok(rows) = query_rows(conn, &sql, libsql::params_from_iter(params)).await else {
            continue;
        };
        let mut texts: HashMap<(String, String), String> = rows
            .iter()
            .map(|row| {
                (
                    (row_str(row, "provider"), row_str(row, "message_id")),
                    row_str(row, "text"),
                )
            })
            .collect();

        let batch: Vec<(String, String, String, i64, String)> = chunk
            .iter()
            .filter_map(|(provider, message_id, model, len)| {
                texts
                    .remove(&(provider.clone(), message_id.clone()))
                    .map(|text| {
                        (
                            provider.clone(),
                            message_id.clone(),
                            model.clone(),
                            *len,
                            text,
                        )
                    })
            })
            .collect();

        // BPE encoding is CPU-bound; keep it off the async worker threads.
        let counted = tokio::task::spawn_blocking(move || {
            batch
                .into_iter()
                .map(
                    |(provider, message_id, model, len, text)| TokenCountUpsert {
                        token_count: count_text_tokens(&text, &model),
                        encoder: encoder_for_model(&model).name,
                        provider,
                        message_id,
                        text_len: len,
                    },
                )
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        computed.extend(counted);
    }

    if computed.is_empty() {
        return;
    }
    if let Some(gdb) = state.savings_db.as_deref() {
        gdb.save_token_counts(&state.lcm_db_path, &computed).await;
    }
    let mut map = state
        .token_counts
        .map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for row in computed {
        map.insert(
            (row.provider, row.message_id),
            CachedCount {
                text_len: row.text_len,
                tokens: row.token_count,
            },
        );
    }
}

/// Detached warm-up so the first Savings-tab request finds a hot cache.
pub(crate) fn spawn_warm(state: DashboardState) {
    if !counting_available() {
        return;
    }
    tokio::spawn(async move {
        let _ = non_usage_message_tokens(&state).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_families_are_exact() {
        for model in [
            "gpt-5.3-codex-high",
            "gpt-5",
            "gpt-4o",
            "gpt-4.1-mini",
            "o3-mini",
            "o1",
            "codex-mini",
            "gpt-oss-120b",
        ] {
            let enc = encoder_for_model(model);
            assert_eq!(enc.name, O200K, "{model}");
            assert!(enc.exact, "{model} should be exact");
        }
        for model in ["gpt-4", "gpt-3.5-turbo", "text-embedding-3-small"] {
            let enc = encoder_for_model(model);
            assert_eq!(enc.name, CL100K, "{model}");
            assert!(enc.exact, "{model} should be exact");
        }
    }

    #[test]
    fn other_vendors_are_labeled_approximate() {
        for model in [
            "claude-fable-5-thinking-xhigh",
            "claude-opus-4-8-thinking-max",
            "gemini-3-pro",
            "grok-build-0.1",
            "kimi-k2.5",
            "composer-2.5-fast",
            "",
        ] {
            let enc = encoder_for_model(model);
            assert_eq!(enc.name, O200K, "{model}");
            assert!(!enc.exact, "{model} must be labeled approximate");
        }
        // "opus" must not be mistaken for the o-series prefix match.
        assert!(!encoder_for_model("opus-large").exact);
    }

    #[cfg(feature = "token-counting")]
    #[test]
    fn bpe_counts_diverge_from_chars4() {
        let text = "fn main() { println!(\"hello tokenizer world\"); }";
        let bpe = count_text_tokens(text, "gpt-5");
        assert!(bpe > 0);
        // Code-heavy text tokenizes denser than chars/4 predicts; the exact
        // value is vocabulary-dependent, so only sanity-bound it.
        assert!(bpe <= text.len() as i64);
        let cl = count_text_tokens(text, "gpt-4");
        assert!(cl > 0);
    }
}
