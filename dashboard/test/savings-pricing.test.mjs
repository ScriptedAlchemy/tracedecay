import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const pricingPath = path.resolve(process.cwd(), "savings/src/pricing.ts");
const pricing = await importBundledModule(pricingPath);

/** Slice of real OpenRouter slugs (per-MTok USD) for resolution tests. */
const TABLE = {
  "anthropic/claude-fable-5": { prompt_per_mtok: 10, completion_per_mtok: 50, cache_read_per_mtok: 1, cache_write_per_mtok: 12.5 },
  "anthropic/claude-opus-4.8": { prompt_per_mtok: 5, completion_per_mtok: 25 },
  "anthropic/claude-opus-4.8-fast": { prompt_per_mtok: 7.5, completion_per_mtok: 37.5 },
  "anthropic/claude-sonnet-4.6": { prompt_per_mtok: 3, completion_per_mtok: 15 },
  "anthropic/claude-3.5-sonnet": { prompt_per_mtok: 3, completion_per_mtok: 15 },
  "openai/gpt-5.5": { prompt_per_mtok: 5, completion_per_mtok: 30, cache_read_per_mtok: 0.5 },
  "openai/gpt-5.5-mini": { prompt_per_mtok: 0.6, completion_per_mtok: 2.4 },
  "openai/gpt-5.3-codex": { prompt_per_mtok: 1.75, completion_per_mtok: 14 },
  "google/gemini-3.5-flash": { prompt_per_mtok: 0.3, completion_per_mtok: 2.5 },
  "x-ai/grok-4.3": { prompt_per_mtok: 3, completion_per_mtok: 15 },
};

// ---------------------------------------------------------------- aliases

test("manual alias table maps versionless reference ids", () => {
  const hit = pricing.resolveModel("claude-sonnet-4", TABLE);
  assert.equal(hit.slug, "anthropic/claude-sonnet-4.6");
});

test("aliases apply after suffix stripping too", () => {
  const hit = pricing.resolveModel("claude-3-5-sonnet-20241022", TABLE);
  assert.equal(hit.slug, "anthropic/claude-3.5-sonnet");
});

// ---------------------------------------------------- fuzzy normalization

test("cursor thinking/effort suffixes are stripped", () => {
  assert.equal(
    pricing.resolveModel("claude-fable-5-thinking-high", TABLE).slug,
    "anthropic/claude-fable-5",
  );
  assert.equal(
    pricing.resolveModel("claude-fable-5-thinking-xhigh", TABLE).slug,
    "anthropic/claude-fable-5",
  );
  assert.equal(
    pricing.resolveModel("gpt-5.3-codex-high", TABLE).slug,
    "openai/gpt-5.3-codex",
  );
  assert.equal(pricing.resolveModel("gpt-5.5-medium", TABLE).slug, "openai/gpt-5.5");
  // Compound effort suffixes strip across dashes.
  assert.equal(pricing.resolveModel("gpt-5.5-extra-high", TABLE).slug, "openai/gpt-5.5");
  assert.equal(pricing.resolveModel("gpt-5.5-x-high", TABLE).slug, "openai/gpt-5.5");
});

test("dash version numbers normalize to dots (claude-opus-4-8-thinking-max)", () => {
  assert.equal(
    pricing.resolveModel("claude-opus-4-8-thinking-max", TABLE).slug,
    "anthropic/claude-opus-4.8",
  );
  assert.equal(pricing.dotVersion("claude-opus-4-8-1"), "claude-opus-4.8.1");
});

test("claude version/family reorder (claude-4.6-sonnet)", () => {
  assert.equal(
    pricing.resolveModel("claude-4.6-sonnet", TABLE).slug,
    "anthropic/claude-sonnet-4.6",
  );
  // Effort/thinking suffix tokens strip in either order.
  assert.equal(
    pricing.resolveModel("claude-4.6-sonnet-medium-thinking", TABLE).slug,
    "anthropic/claude-sonnet-4.6",
  );
  assert.equal(
    pricing.resolveModel("claude-4.6-sonnet-thinking-medium", TABLE).slug,
    "anthropic/claude-sonnet-4.6",
  );
});

test("vendor prefixes are tried for bare ids; prefixed ids match exactly", () => {
  assert.equal(pricing.resolveModel("grok-4.3", TABLE).slug, "x-ai/grok-4.3");
  assert.equal(
    pricing.resolveModel("anthropic/claude-fable-5", TABLE).slug,
    "anthropic/claude-fable-5",
  );
});

test("more specific slugs win over stripped ones", () => {
  // Exact -fast variant must match before stripping could reach the base id.
  assert.equal(
    pricing.resolveModel("claude-opus-4.8-fast", TABLE).slug,
    "anthropic/claude-opus-4.8-fast",
  );
  // Capability tiers are never stripped: mini stays mini.
  assert.equal(pricing.resolveModel("gpt-5.5-mini", TABLE).slug, "openai/gpt-5.5-mini");
});

test("unknown models resolve to null, never a guess", () => {
  assert.equal(pricing.resolveModel("composer-2.5-fast", TABLE), null);
  assert.equal(pricing.resolveModel("totally-made-up-model", TABLE), null);
  assert.equal(pricing.resolveModel("", TABLE), null);
  assert.equal(pricing.resolveModel(null, TABLE), null);
});

// ---------------------------------------------------------------- cost math

test("costUsd: per-MTok math with cache rates", () => {
  const price = TABLE["anthropic/claude-fable-5"];
  const usd = pricing.costUsd(price, {
    input_tokens: 1_000_000,
    output_tokens: 100_000,
    cache_read_tokens: 2_000_000,
    cache_write_tokens: 400_000,
  });
  // 1M*$10 + 0.1M*$50 + 2M*$1 + 0.4M*$12.5 = 10 + 5 + 2 + 5 = 22
  assert.ok(Math.abs(usd - 22) < 1e-9, `got ${usd}`);
});

test("costUsd: missing cache rates fall back to the prompt rate", () => {
  const price = TABLE["anthropic/claude-opus-4.8"];
  const usd = pricing.costUsd(price, {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 1_000_000,
  });
  assert.ok(Math.abs(usd - 5) < 1e-9, `got ${usd}`);
});

test("rowCost splits actual vs estimated portions", () => {
  const row = {
    model: "gpt-5.5-high",
    cost_basis: "mixed",
    actual: { input_tokens: 1_000_000, output_tokens: 0 },
    estimated: { input_tokens: 0, output_tokens: 1_000_000 },
  };
  const cost = pricing.rowCost(row, TABLE);
  assert.equal(cost.resolved.slug, "openai/gpt-5.5");
  assert.ok(Math.abs(cost.actual_usd - 5) < 1e-9);
  assert.ok(Math.abs(cost.estimated_usd - 30) < 1e-9);
  assert.ok(Math.abs(cost.usd - 35) < 1e-9);
});

test("rowCost: unpriced model yields null cost", () => {
  const row = {
    model: "composer-2.5-fast",
    cost_basis: "estimated",
    actual: { input_tokens: 0, output_tokens: 0 },
    estimated: { input_tokens: 500, output_tokens: 500 },
  };
  const cost = pricing.rowCost(row, TABLE);
  assert.equal(cost.usd, null);
  assert.equal(cost.resolved, null);
});

test("summarizeCosts aggregates priced rows and tracks unpriced models", () => {
  const rows = [
    {
      model: "claude-fable-5-thinking-high",
      cost_basis: "estimated",
      actual: { input_tokens: 0, output_tokens: 0 },
      estimated: { input_tokens: 1_000_000, output_tokens: 0 },
    },
    {
      model: "composer-2.5-fast",
      cost_basis: "estimated",
      actual: { input_tokens: 0, output_tokens: 0 },
      estimated: { input_tokens: 999, output_tokens: 999 },
    },
    {
      model: null,
      cost_basis: "estimated",
      actual: { input_tokens: 0, output_tokens: 0 },
      estimated: { input_tokens: 10, output_tokens: 10 },
    },
  ];
  const summary = pricing.summarizeCosts(rows, TABLE);
  assert.equal(summary.priced_rows, 1);
  assert.ok(Math.abs(summary.priced_usd - 10) < 1e-9);
  assert.deepEqual(summary.unpriced_models, ["composer-2.5-fast", "unknown"]);
});

test("savedTokensUsd values savings at the Sonnet input rate", () => {
  const usd = pricing.savedTokensUsd(2_000_000, TABLE);
  assert.ok(Math.abs(usd - 6) < 1e-9, `got ${usd}`); // 2M * $3/MTok
  assert.equal(pricing.savedTokensUsd(1, {}), null);
});
