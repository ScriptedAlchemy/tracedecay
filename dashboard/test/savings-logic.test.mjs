import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const logicPath = path.resolve(process.cwd(), "savings/src/logic.ts");
const logic = await importBundledModule(logicPath);

test("fmtTokens scales units", () => {
  assert.equal(logic.fmtTokens(0), "0");
  assert.equal(logic.fmtTokens(1234), "1,234");
  assert.equal(logic.fmtTokens(53_200), "53.2k");
  assert.equal(logic.fmtTokens(47_442_289), "47.4M");
  assert.equal(logic.fmtTokens(2_100_000_000), "2.1B");
  assert.equal(logic.fmtTokens(null), "0");
});

test("fmtUsd adapts precision and handles missing values", () => {
  assert.equal(logic.fmtUsd(1234.56), "$1,235");
  assert.equal(logic.fmtUsd(12.345), "$12.35");
  assert.equal(logic.fmtUsd(0.0123), "$0.012");
  assert.equal(logic.fmtUsd(0.000123), "$0.000123");
  assert.equal(logic.fmtUsd(0), "$0");
  assert.equal(logic.fmtUsd(null), "—");
  assert.equal(logic.fmtUsd(undefined), "—");
});

test("sharePct is safe for zero totals", () => {
  assert.equal(logic.sharePct(1, 4), 25);
  assert.equal(logic.sharePct(5, 0), 0);
});

test("cleanTitle strips prompt wrappers and clips", () => {
  assert.equal(
    logic.cleanTitle("<timestamp>Thursday</timestamp> <user_query> Build the tab"),
    "Thursday Build the tab",
  );
  assert.equal(logic.cleanTitle("  "), "(untitled session)");
  assert.equal(logic.cleanTitle(null), "(untitled session)");
  const clipped = logic.cleanTitle("x".repeat(300), 50);
  assert.ok(clipped.length <= 50);
  assert.ok(clipped.endsWith("…"));
});

test("fillDailySeries fills gaps and sums duplicate days", () => {
  const day = 1_765_000_800 - (1_765_000_800 % 86_400);
  const series = logic.fillDailySeries(
    [
      { day, saved: 10 },
      { day: day + 2 * 86_400, saved: 5 },
      { day, saved: 7 },
    ],
    (row) => row.saved,
  );
  assert.deepEqual(series, [
    { day, value: 17 },
    { day: day + 86_400, value: 0 },
    { day: day + 2 * 86_400, value: 5 },
  ]);
  assert.deepEqual(logic.fillDailySeries([], () => 0), []);
});

test("fillDailySeries refuses to explode on absurd ranges", () => {
  const series = logic.fillDailySeries(
    [
      { day: 0, value: 1 },
      { day: 1_765_000_800, value: 2 },
    ],
    (row) => row.value,
  );
  assert.equal(series.length, 2);
});

test("projectLabel shortens paths", () => {
  assert.equal(logic.projectLabel("/home/zack/projects/tokensave"), "tokensave");
  assert.equal(logic.projectLabel(""), "(unknown)");
});

test("basis labels distinguish actual from estimated", () => {
  assert.equal(logic.BASIS_LABELS.actual, "actual (from transcript usage)");
  assert.equal(logic.BASIS_LABELS.estimated, "estimated (computed)");
  assert.ok(logic.BASIS_LABELS.mixed.includes("usage"));
});
