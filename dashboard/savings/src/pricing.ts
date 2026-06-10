/**
 * Model-price resolution + cost math for the Savings & Cost tab.
 *
 * The backend serves a raw OpenRouter price table (slug → USD per MTok) from
 * `GET /api/plugins/savings/pricing`; this module fuzzy-maps transcript model
 * ids (Cursor/Claude/Codex spellings like `claude-fable-5-thinking-high`,
 * `claude-opus-4-8-thinking-max`, `gpt-5.3-codex-high`) onto OpenRouter slugs
 * and computes dollar costs. Unknown models resolve to `null` — the UI shows
 * "no price data" instead of guessing.
 *
 * Pure module (no React/SDK imports): unit-tested by
 * `dashboard/test/savings-pricing.test.mjs`.
 */

export interface ModelPrice {
  prompt_per_mtok: number;
  completion_per_mtok: number;
  cache_read_per_mtok?: number | null;
  cache_write_per_mtok?: number | null;
}

export type PriceTable = Record<string, ModelPrice>;

export interface TokenCounts {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
}

export interface ResolvedPrice {
  slug: string;
  price: ModelPrice;
}

/**
 * Manual alias table for transcript ids whose OpenRouter slug cannot be
 * derived mechanically. Keys are lowercase transcript ids (matched after
 * effort/date-suffix stripping too), values are OpenRouter slugs.
 */
export const MODEL_ALIASES: Record<string, string> = {
  // CLI reference spellings without a minor version — pin to the slug the
  // current generation actually bills at.
  "claude-sonnet-4": "anthropic/claude-sonnet-4.6",
  "claude-opus-4": "anthropic/claude-opus-4.6",
  "claude-haiku-4": "anthropic/claude-haiku-4.5",
  // Older dash-separated Anthropic ids.
  "claude-3-5-sonnet": "anthropic/claude-3.5-sonnet",
  "claude-3-5-haiku": "anthropic/claude-3.5-haiku",
  "claude-3-opus": "anthropic/claude-3-opus",
};

/** Vendor prefixes tried for bare (unprefixed) transcript model ids. */
const PROVIDER_PREFIXES = [
  "anthropic",
  "openai",
  "google",
  "x-ai",
  "moonshotai",
  "deepseek",
  "mistralai",
  "meta-llama",
  "qwen",
  "z-ai",
  "amazon",
  "cohere",
];

/**
 * Effort / reasoning-mode suffix tokens appended by agent hosts (Cursor's
 * `-thinking-high`, `-xhigh`, `-max`, …) that never appear in OpenRouter
 * slugs. Stripped iteratively from the right. Capability tiers that DO
 * change the price (`mini`, `nano`, `pro`, `codex`, `chat`, `lite`, `flash`)
 * are deliberately absent.
 */
const EFFORT_SUFFIXES = new Set([
  "thinking",
  "think",
  "high",
  "xhigh",
  "x-high",
  "extra-high",
  // Compound suffixes (`-extra-high`, `-x-high`) strip one dash-token at a
  // time, so their leading halves must be strippable on their own.
  "extra",
  "x",
  "medium",
  "low",
  "minimal",
  "max",
  "latest",
]);

/** `claude-opus-4-8` → `claude-opus-4.8` (digit-dash-digit → digit-dot-digit). */
export function dotVersion(id: string): string {
  let out = id;
  let prev = "";
  // Repeat for multi-part versions like 4-8-1 → 4.8.1.
  while (out !== prev) {
    prev = out;
    out = out.replace(/(\d)-(\d)/g, "$1.$2");
  }
  return out;
}

/** Strip trailing `-YYYYMMDD` / `-YYYY-MM-DD` release-date suffixes. */
function stripDateSuffix(id: string): string {
  return id.replace(/-(20\d{6}|20\d{2}-\d{2}-\d{2})$/, "");
}

/** `claude-4.6-sonnet` → `claude-sonnet-4.6` (family/version reorder). */
function reorderClaude(id: string): string | null {
  const match = id.match(/^claude-([\d][\d.]*)-(opus|sonnet|haiku|fable)$/);
  return match ? `claude-${match[2]}-${match[1]}` : null;
}

/**
 * Ordered candidate slugs for a transcript model id, most specific first.
 * Exported for tests; `resolveModel` walks this list against the table.
 */
export function normalizeCandidates(modelId: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  const push = (candidate: string | null | undefined) => {
    if (!candidate || seen.has(candidate)) return;
    seen.add(candidate);
    out.push(candidate);
    const dotted = dotVersion(candidate);
    if (dotted !== candidate && !seen.has(dotted)) {
      seen.add(dotted);
      out.push(dotted);
    }
    const reordered = reorderClaude(dotted);
    if (reordered && !seen.has(reordered)) {
      seen.add(reordered);
      out.push(reordered);
    }
  };

  let base = modelId.trim().toLowerCase();
  if (!base) return [];
  push(base);
  base = stripDateSuffix(base);
  push(base);

  // Iteratively strip trailing effort tokens: claude-fable-5-thinking-xhigh
  // → claude-fable-5-thinking → claude-fable-5.
  let stripped = base;
  for (;;) {
    const idx = stripped.lastIndexOf("-");
    if (idx <= 0) break;
    const tail = stripped.slice(idx + 1);
    if (!EFFORT_SUFFIXES.has(tail)) break;
    stripped = stripped.slice(0, idx);
    push(stripped);
  }
  return out;
}

/**
 * Resolve a transcript model id to an OpenRouter slug + price.
 * Order: manual aliases (raw id, then each normalized candidate) → exact
 * table hits → vendor-prefixed table hits. Returns `null` for unknown
 * models — callers must surface "no price data", never a guessed price.
 */
export function resolveModel(
  modelId: string | null | undefined,
  table: PriceTable,
): ResolvedPrice | null {
  if (!modelId) return null;
  const candidates = normalizeCandidates(modelId);
  if (!candidates.length) return null;

  const bySlug = (slug: string): ResolvedPrice | null =>
    Object.prototype.hasOwnProperty.call(table, slug)
      ? { slug, price: table[slug] }
      : null;

  for (const candidate of candidates) {
    const alias = MODEL_ALIASES[candidate];
    if (alias) {
      const hit = bySlug(alias);
      if (hit) return hit;
    }
  }
  for (const candidate of candidates) {
    const exact = bySlug(candidate);
    if (exact) return exact;
    if (!candidate.includes("/")) {
      for (const prefix of PROVIDER_PREFIXES) {
        const hit = bySlug(`${prefix}/${candidate}`);
        if (hit) return hit;
      }
    }
  }
  return null;
}

/**
 * Dollar cost of a token batch at a resolved price. Cache reads/writes use
 * their dedicated rates when the model has them, otherwise the prompt rate
 * (a conservative upper bound — cached reads are never billed above input).
 */
export function costUsd(price: ModelPrice, tokens: TokenCounts): number {
  const cacheRead = tokens.cache_read_tokens ?? 0;
  const cacheWrite = tokens.cache_write_tokens ?? 0;
  const cacheReadRate = price.cache_read_per_mtok ?? price.prompt_per_mtok;
  const cacheWriteRate = price.cache_write_per_mtok ?? price.prompt_per_mtok;
  return (
    (tokens.input_tokens * price.prompt_per_mtok +
      tokens.output_tokens * price.completion_per_mtok +
      cacheRead * cacheReadRate +
      cacheWrite * cacheWriteRate) /
    1_000_000
  );
}

/** Token-aggregate row as served by the savings API (per session-model). */
export interface ApiTokenRow {
  model: string | null;
  cost_basis: "actual" | "estimated" | "mixed";
  actual: TokenCounts;
  estimated: { input_tokens: number; output_tokens: number };
}

export interface RowCost {
  /** Dollar cost, or null when the model has no price data. */
  usd: number | null;
  /** Portion backed by transcript usage records. */
  actual_usd: number | null;
  /** Portion computed from chars/4 token estimates. */
  estimated_usd: number | null;
  resolved: ResolvedPrice | null;
}

/** Cost of one API token row, split by provenance. */
export function rowCost(row: ApiTokenRow, table: PriceTable): RowCost {
  const resolved = resolveModel(row.model, table);
  if (!resolved) {
    return { usd: null, actual_usd: null, estimated_usd: null, resolved: null };
  }
  const actual = costUsd(resolved.price, row.actual);
  const estimated = costUsd(resolved.price, {
    input_tokens: row.estimated.input_tokens,
    output_tokens: row.estimated.output_tokens,
  });
  return {
    usd: actual + estimated,
    actual_usd: actual,
    estimated_usd: estimated,
    resolved,
  };
}

export interface CostSummary {
  /** Sum over rows whose model had price data. */
  priced_usd: number;
  actual_usd: number;
  estimated_usd: number;
  priced_rows: number;
  /** Distinct model labels without price data (null model → "unknown"). */
  unpriced_models: string[];
}

/** Aggregate cost over many API token rows, tracking unpriced models. */
export function summarizeCosts(rows: ApiTokenRow[], table: PriceTable): CostSummary {
  const summary: CostSummary = {
    priced_usd: 0,
    actual_usd: 0,
    estimated_usd: 0,
    priced_rows: 0,
    unpriced_models: [],
  };
  const unpriced = new Set<string>();
  for (const row of rows) {
    const cost = rowCost(row, table);
    if (cost.usd === null) {
      unpriced.add(row.model || "unknown");
      continue;
    }
    summary.priced_usd += cost.usd;
    summary.actual_usd += cost.actual_usd ?? 0;
    summary.estimated_usd += cost.estimated_usd ?? 0;
    summary.priced_rows += 1;
  }
  summary.unpriced_models = [...unpriced].sort();
  return summary;
}

/**
 * Reference rate used to express saved tokens in dollars: saved tokens are
 * context the model never had to read, so they are valued at a flagship
 * *input* rate (same convention as `tokensave gain`, which uses the Claude
 * Sonnet input price).
 */
export const SAVINGS_REFERENCE_MODEL = "claude-sonnet-4";

export function savedTokensUsd(savedTokens: number, table: PriceTable): number | null {
  const resolved = resolveModel(SAVINGS_REFERENCE_MODEL, table);
  if (!resolved) return null;
  return (savedTokens * resolved.price.prompt_per_mtok) / 1_000_000;
}
