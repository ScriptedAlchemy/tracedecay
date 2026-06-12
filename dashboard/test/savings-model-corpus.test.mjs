/**
 * Model-id corpus test: every DISTINCT model id observed on this machine
 * (session store `session_messages.model` + raw Cursor transcript `model`
 * fields) must resolve against the REAL bundled OpenRouter price table —
 * not a toy fixture — to the correct slug, or to null only for models with
 * no OpenRouter listing (Cursor-proprietary ids like `composer-*`).
 */

import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { readFileSync } from "node:fs";

import { importBundledModule } from "./helpers/module-loader.mjs";

const pricingPath = path.resolve(process.cwd(), "savings/src/pricing.ts");
const pricing = await importBundledModule(pricingPath);

/** Build a PriceTable from the bundled fallback snapshot, mirroring the
 * Rust parser (`savings_pricing::parse_openrouter_json`): per-token USD
 * strings × 1e6, skip floating `~vendor/model-latest` aliases, require
 * prompt + completion. */
function loadBundledTable() {
  const fallbackPath = path.resolve(
    process.cwd(),
    "../src/dashboard/model_prices_fallback.json",
  );
  const parsed = JSON.parse(readFileSync(fallbackPath, "utf8"));
  const table = {};
  for (const entry of parsed.data ?? []) {
    const id = entry.id;
    if (!id || id.startsWith("~") || !entry.pricing) continue;
    const prompt = Number(entry.pricing.prompt);
    const completion = Number(entry.pricing.completion);
    if (!Number.isFinite(prompt) || !Number.isFinite(completion)) continue;
    table[id] = {
      prompt_per_mtok: prompt * 1_000_000,
      completion_per_mtok: completion * 1_000_000,
    };
  }
  return table;
}

const TABLE = loadBundledTable();

/**
 * The real corpus: raw id → expected OpenRouter slug (null = genuinely
 * unlisted, must show "no price data"). Sources annotated per id.
 */
const REAL_CORPUS = [
  // session_messages.model (the 7 ids the savings work found)
  ["claude-fable-5-thinking-high", "anthropic/claude-fable-5"],
  ["claude-fable-5-thinking-xhigh", "anthropic/claude-fable-5"],
  ["claude-opus-4-8-thinking-max", "anthropic/claude-opus-4.8"],
  ["composer-2.5-fast", null], // Cursor-proprietary, not on OpenRouter
  ["gpt-5.5-extra-high", "openai/gpt-5.5"],
  ["gpt-5.5-high", "openai/gpt-5.5"],
  ["gpt-5.5-medium", "openai/gpt-5.5"],
  // additional ids from raw Cursor transcript `model` fields
  ["claude-4.6-sonnet-medium-thinking", "anthropic/claude-sonnet-4.6"],
  ["claude-fable-5-thinking-max", "anthropic/claude-fable-5"],
  ["claude-opus-4-8-thinking-high", "anthropic/claude-opus-4.8"],
  ["claude-opus-4-8-thinking-xhigh", "anthropic/claude-opus-4.8"],
  ["gpt-5.3-codex-high", "openai/gpt-5.3-codex"],
  ["gpt-5.4-medium", "openai/gpt-5.4"],
  ["grok-build-0.1", "x-ai/grok-build-0.1"],
  ["kimi-k2.5", "moonshotai/kimi-k2.5"],
];

test("bundled price table carries every slug the corpus expects", () => {
  const expectedSlugs = [
    ...new Set(REAL_CORPUS.map(([, slug]) => slug).filter(Boolean)),
  ];
  for (const slug of expectedSlugs) {
    assert.ok(TABLE[slug], `bundled fallback table is missing ${slug}`);
  }
});

for (const [rawId, expectedSlug] of REAL_CORPUS) {
  test(`real id resolves: ${rawId} → ${expectedSlug ?? "no price data"}`, () => {
    const hit = pricing.resolveModel(rawId, TABLE);
    if (expectedSlug === null) {
      assert.equal(hit, null, `${rawId} must show "no price data", got ${hit?.slug}`);
    } else {
      assert.ok(hit, `${rawId} failed to resolve`);
      assert.equal(hit.slug, expectedSlug);
      assert.ok(hit.price.prompt_per_mtok > 0, `${expectedSlug} has no prompt price`);
      assert.ok(
        hit.price.completion_per_mtok > 0,
        `${expectedSlug} has no completion price`,
      );
    }
  });
}

// ---------------------------------------------------------- ambiguity guards

test("capability tiers survive effort stripping (mini is never stripped)", () => {
  // gpt-5.4-mini is a distinct cheaper listing; stripping it to gpt-5.4
  // would misprice it.
  assert.equal(pricing.resolveModel("gpt-5.4-mini", TABLE).slug, "openai/gpt-5.4-mini");
  // -high IS stripped on the very same family.
  assert.equal(pricing.resolveModel("gpt-5.4-high", TABLE).slug, "openai/gpt-5.4");
});

test("exact slug match wins before suffix stripping (fast variants)", () => {
  // claude-opus-4.8-fast is a separately priced listing; it must not be
  // collapsed onto claude-opus-4.8.
  assert.equal(
    pricing.resolveModel("claude-opus-4.8-fast", TABLE).slug,
    "anthropic/claude-opus-4.8-fast",
  );
  assert.equal(
    pricing.resolveModel("claude-opus-4-8-fast", TABLE).slug,
    "anthropic/claude-opus-4.8-fast",
  );
});

test("pro tier is preserved", () => {
  assert.equal(pricing.resolveModel("gpt-5.5-pro", TABLE).slug, "openai/gpt-5.5-pro");
});
