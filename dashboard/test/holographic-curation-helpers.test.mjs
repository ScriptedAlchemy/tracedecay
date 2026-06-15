import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const formatPath = path.resolve(process.cwd(), "holographic/src/curation/format.ts");
const riskPath = path.resolve(process.cwd(), "holographic/src/curation/risk.ts");
const format = await importBundledModule(formatPath);
const risk = await importBundledModule(riskPath);

test("describe formats entity merges with names and fallback ids", () => {
  assert.equal(
    format.describe({
      op: "entity_merge",
      loser_entity: 12,
      winner_entity: 34,
      loser_name: "Old Persona",
      winner_name: "Canonical Persona",
    }),
    "Merge entity Old Persona (#12) → Canonical Persona (#34)",
  );

  assert.equal(
    format.describe({
      op: "entity_prune",
      entity_id: 55,
    }),
    "Prune junk entity #55",
  );
});

test("describe includes merge similarity and reflect supersedes list", () => {
  assert.equal(
    format.describe({ op: "merge", loser: 8, winner: 3, similarity: 0.92 }),
    "Merge #8 → #3 (sim 0.92)",
  );
  assert.equal(
    format.describe({ op: "reflect", supersedes: [4, 7] }),
    "Reflect (replaces #4, #7)",
  );
});

test("tag helpers split, diff, and flag bookkeeping tags", () => {
  assert.deepEqual(format.splitTags(" alpha, beta ,, gamma "), ["alpha", "beta", "gamma"]);
  assert.deepEqual(format.diffTags("alpha, beta, cat:people", "beta, gamma, target:42"), {
    oldTags: ["alpha", "beta", "cat:people"],
    newTags: ["beta", "gamma", "target:42"],
    kept: ["beta"],
    removed: ["alpha", "cat:people"],
    added: ["gamma", "target:42"],
  });
  assert.equal(format.isBookkeepingTag("cat:people"), true);
  assert.equal(format.isBookkeepingTag("target:42"), true);
  assert.equal(format.isBookkeepingTag("topic:real"), false);
});

test("history and oplog formatters preserve empty and invalid values", () => {
  assert.equal(format.formatHistoryTime(), "");
  assert.equal(format.formatHistoryTime("not-a-date"), "not-a-date");
  assert.equal(format.formatOplogTime(0), "");

  const formatted = format.formatHistoryTime("2026-06-14T12:34:56Z");
  assert.equal(typeof formatted, "string");
  assert.notEqual(formatted, "");
  assert.notEqual(formatted, "2026-06-14T12:34:56Z");

  assert.equal(
    format.formatOplogTime(1718368496),
    format.formatHistoryTime(new Date(1718368496 * 1000).toISOString()),
  );
});

test("risk helpers classify actions and bucket unknown operations into other", () => {
  assert.equal(risk.actionRisk("retag"), "low");
  assert.equal(risk.actionRisk("entity_merge"), "medium");
  assert.equal(risk.actionRisk("delete"), "high");
  assert.equal(risk.actionRisk("mystery_op"), "review");

  assert.equal(risk.riskClass("high"), "border-destructive/30 bg-destructive/10 text-destructive");
  assert.equal(risk.riskClass("review"), "border-border bg-secondary/50 text-text-tertiary");

  const grouped = risk.groupActions([
    { op: "delete" },
    { op: "entity_classify" },
    { op: "retag" },
    { op: "mystery_op" },
  ]);

  assert.deepEqual(
    grouped.map((group) => [group.key, group.actions.map((action) => action.op)]),
    [
      ["fact_cleanup", ["delete"]],
      ["entity_cleanup", ["entity_classify"]],
      ["organization", ["retag"]],
      ["reflections", []],
      ["other", ["mystery_op"]],
    ],
  );
});
