import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const viewPath = path.resolve(process.cwd(), "graph/src/defaultView.ts");
const view = await importBundledModule(viewPath);

const labelPath = path.resolve(process.cwd(), "graph/src/labelLayout.ts");
const labels = await importBundledModule(labelPath);

function box(id, { priority = 4, degree = 0, left = 0, top = 0, width = 60, height = 12, sticky } = {}) {
  return { id, priority, degree, left, top, right: left + width, bottom: top + height, sticky };
}

test("default view limits stay within the canvas budget", () => {
  const { limit_nodes, limit_edges } = view.DEFAULT_VIEW_LIMITS;
  // ~60-120 nodes: informative but inside the soft cap / 60fps budget.
  assert.ok(limit_nodes >= 60 && limit_nodes <= 120, `limit_nodes=${limit_nodes}`);
  // Backend caps: 250 nodes / 500 edges.
  assert.ok(limit_nodes <= 250 && limit_edges <= 500);
  assert.ok(limit_edges >= limit_nodes, "edge budget should not starve the node budget");
});

test("empty index is the only state that asks the user to index", () => {
  const message = view.canvasEmptyMessage({ indexedNodes: 0, loadedNodes: 0, loading: false });
  // Real CLI flow: `tracedecay init` creates the index, `tracedecay sync` refreshes it.
  assert.match(message, /tracedecay init/);
  assert.match(message, /tracedecay sync/);
  assert.doesNotMatch(message, /tracedecay index/);
  // Even while a request is in flight, a 0-node index reports itself as empty.
  assert.equal(
    view.canvasEmptyMessage({ indexedNodes: 0, loadedNodes: 0, loading: true }),
    message,
  );
});

test("filters hiding every loaded node keep the filter copy", () => {
  assert.match(
    view.canvasEmptyMessage({ indexedNodes: 500, loadedNodes: 100, loading: false }),
    /hidden by the current filters/,
  );
});

test("loading copy while the default slice or overview is in flight", () => {
  assert.match(
    view.canvasEmptyMessage({ indexedNodes: 500, loadedNodes: 0, loading: true }),
    /Loading the project graph/,
  );
  assert.match(
    view.canvasEmptyMessage({ indexedNodes: null, loadedNodes: 0, loading: false }),
    /Loading the project graph/,
  );
});

test("search hint only as a fallback when nothing loaded on a non-empty index", () => {
  assert.match(
    view.canvasEmptyMessage({ indexedNodes: 500, loadedNodes: 0, loading: false }),
    /search a symbol above/,
  );
});

test("label cap scales with viewport area and never starves", () => {
  assert.equal(labels.labelCapForArea(1280, 900), 38);
  assert.equal(labels.labelCapForArea(420, 700), 9);
  assert.equal(labels.labelCapForArea(100, 100), 6);
});

test("overlapping labels collapse to the highest-degree one", () => {
  // A hub-spoke pile-up: every spoke label overlaps the hub label.
  const chosen = labels.selectLabels(
    [
      box("hub", { degree: 50, left: 0, top: 0 }),
      box("spoke-a", { degree: 3, left: 10, top: 4 }),
      box("spoke-b", { degree: 2, left: 20, top: 8 }),
      box("far-spoke", { degree: 1, left: 300, top: 300 }),
    ],
    10,
  );
  assert.deepEqual(chosen, ["hub", "far-spoke"]);
});

test("priority outranks degree", () => {
  const chosen = labels.selectLabels(
    [
      box("hub", { priority: 4, degree: 900, left: 0, top: 0 }),
      box("selected", { priority: 1, degree: 1, left: 10, top: 4 }),
    ],
    10,
  );
  assert.deepEqual(chosen, ["selected"]);
});

test("sticky labels always render and bypass the cap", () => {
  const chosen = labels.selectLabels(
    [
      box("hovered", { priority: 0, sticky: true, left: 0, top: 0 }),
      box("colliding-hub", { priority: 4, degree: 99, left: 5, top: 5 }),
      box("free", { priority: 4, degree: 1, left: 200, top: 200 }),
    ],
    0,
  );
  // Cap of zero: only the sticky hover survives.
  assert.deepEqual(chosen, ["hovered"]);
});

test("the cap bounds non-sticky labels", () => {
  const spread = Array.from({ length: 20 }, (_, index) =>
    box(`n${index}`, { degree: 20 - index, left: index * 100, top: index * 40 }),
  );
  const chosen = labels.selectLabels(spread, 5);
  assert.equal(chosen.length, 5);
  assert.deepEqual(chosen, ["n0", "n1", "n2", "n3", "n4"]);
});
