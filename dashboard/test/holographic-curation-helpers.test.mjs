import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const formatPath = path.resolve(process.cwd(), "holographic/src/curation/format.ts");
const format = await importBundledModule(formatPath);

test("history formatter preserves empty and invalid values", () => {
  assert.equal(format.formatHistoryTime(), "");
  assert.equal(format.formatHistoryTime(null), "");
  assert.equal(format.formatHistoryTime("not-a-date"), "not-a-date");

  const formatted = format.formatHistoryTime("2026-06-14T12:34:56Z");
  assert.equal(typeof formatted, "string");
  assert.notEqual(formatted, "");
  assert.notEqual(formatted, "2026-06-14T12:34:56Z");
});

test("oplog formatter preserves empty and invalid values", () => {
  assert.equal(format.formatOplogTime(), "--");
  assert.equal(format.formatOplogTime(null), "--");
  assert.equal(format.formatOplogTime("not-a-date"), "not-a-date");

  const formatted = format.formatOplogTime(1718368496);
  assert.equal(typeof formatted, "string");
  assert.notEqual(formatted, "");
  assert.notEqual(formatted, "1718368496");
});
