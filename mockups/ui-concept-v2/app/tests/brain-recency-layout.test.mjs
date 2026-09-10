import assert from "node:assert/strict";
import test from "node:test";
import { recencyPeerOffsets } from "../src/brain/recencyLayout.ts";

const peers = (count, recency = "<5min") =>
  Array.from({ length: count }, (_, i) => ({
    id: `project-${String(i).padStart(3, "0")}`,
    recency,
  }));

const entries = (offsets) => [...offsets].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);

// Node >=22.6: node --experimental-strip-types --test tests/brain-recency-layout.test.mjs
// Synthetic records exercise geometry only; they are not admitted activity.
test("an empty registry has no peer offsets", () => {
  assert.equal(recencyPeerOffsets([], 80).size, 0);
});

test("a lone project stays at the center of its own recency column", () => {
  const projects = [
    { id: "recent", recency: "<5min" },
    { id: "older", recency: "1d-1w" },
  ];
  assert.deepEqual(entries(recencyPeerOffsets(projects, 80)), [["older", 0], ["recent", 0]]);
});

test("two peers remain inside their column at narrow aperture widths", () => {
  for (const apertureWidth of [316, 500, 800, 1000, 1600]) {
    const columnWidth = Math.max(200, apertureWidth - 86 - 30) / 8;
    const center = 86 + columnWidth / 2;
    for (const offset of recencyPeerOffsets(peers(2), columnWidth).values()) {
      const x = center + offset;
      assert.ok(x > 86 && x < 86 + columnWidth,
        `aperture=${apertureWidth}: center ${x} escaped its recency column`);
    }
  }
});

test("dense peers stay distinct and within the central 80 percent of a column", () => {
  for (const count of [2, 3, 6, 100]) {
    for (const columnWidth of [25, 64, 85.5, 200]) {
      const offsets = [...recencyPeerOffsets(peers(count), columnWidth).values()];
      assert.equal(new Set(offsets).size, count);
      for (const offset of offsets) {
        assert.ok(Math.abs(offset) <= columnWidth * 0.4 + 1e-9,
          `${count} peers, column=${columnWidth}: offset ${offset} exceeds the band`);
      }
      assert.ok(Math.abs(offsets.reduce((sum, x) => sum + x, 0)) < 1e-8);
    }
  }
});

test("the same identities retain their positions when registry order changes", () => {
  const projects = peers(6);
  const expected = entries(recencyPeerOffsets(projects, 85.5));
  assert.deepEqual(entries(recencyPeerOffsets([...projects].reverse(), 85.5)), expected);
  assert.deepEqual(entries(recencyPeerOffsets([...projects.slice(2), ...projects.slice(0, 2)], 85.5)), expected);
});

test("projects in another recency bucket do not change existing offsets", () => {
  const projects = peers(3);
  const base = recencyPeerOffsets(projects, 85.5);
  const mixed = recencyPeerOffsets([
    { id: "unrelated", recency: "1d-1w" },
    ...projects,
  ], 85.5);
  for (const project of projects) assert.equal(mixed.get(project.id), base.get(project.id));
  assert.equal(mixed.get("unrelated"), 0);
});

test("layout never sorts or mutates the registry input in place", () => {
  const projects = Object.freeze(peers(3).reverse().map(Object.freeze));
  const before = projects.map((p) => ({ ...p }));
  recencyPeerOffsets(projects, 85.5);
  assert.deepEqual(projects, before);
});

test("wide columns retain the existing maximum 112px peer spacing", () => {
  const offsets = [...recencyPeerOffsets(peers(3), 1000).values()];
  assert.deepEqual(offsets, [-112, 0, 112]);
});
