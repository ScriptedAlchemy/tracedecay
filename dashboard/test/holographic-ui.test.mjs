import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const uiPath = path.resolve(process.cwd(), "holographic/src/ui.ts");
const ui = await importBundledModule(uiPath);

test("truncate preserves short values", () => {
  assert.equal(ui.truncate("short", 10), "short");
});

test("truncate clips long values with a trailing ellipsis", () => {
  assert.equal(ui.truncate("abcdefghij", 5), "abcd\u2026");
});
