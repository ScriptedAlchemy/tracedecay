import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const lib = (name) => path.resolve(process.cwd(), "lib", name);

const { qs } = await importBundledModule(lib("qs.ts"));
const { fmt, short } = await importBundledModule(lib("format.ts"));
const { makeSequence } = await importBundledModule(lib("sequence.ts"));

test("qs serializes defined params and skips empty ones", () => {
  assert.equal(qs({}), "");
  assert.equal(qs({ q: undefined, range: "" }), "");
  assert.equal(qs({ q: "fn main", limit: 20, offset: 0 }), "?q=fn+main&limit=20&offset=0");
});

test("fmt groups integers and zero-fills nullish input", () => {
  assert.equal(fmt(1234567), Number(1234567).toLocaleString());
  assert.equal(fmt(undefined), "0");
});

test("short clips long text with an ellipsis", () => {
  assert.equal(short("abc", 5), "abc");
  assert.equal(short(null, 5), "");
  assert.equal(short("abcdef", 5), "abcd…");
  assert.equal(short("abcdef", 5).length, 5);
});

test("makeSequence drops superseded tickets", () => {
  const seq = makeSequence();
  const first = seq.next();
  assert.ok(seq.isCurrent(first));
  const second = seq.next();
  assert.ok(!seq.isCurrent(first));
  assert.ok(seq.isCurrent(second));
});

test("makeSequence invalidate cancels all outstanding tickets", () => {
  const seq = makeSequence();
  const ticket = seq.next();
  seq.invalidate();
  assert.ok(!seq.isCurrent(ticket));
  // The next request after an invalidation is current again.
  assert.ok(seq.isCurrent(seq.next()));
});
