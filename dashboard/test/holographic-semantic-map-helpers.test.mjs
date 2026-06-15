import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const hitTestPath = path.resolve(process.cwd(), "holographic/src/semanticMap/hitTest.ts");
const densityPath = path.resolve(process.cwd(), "holographic/src/semanticMap/density.ts");
const hitTest = await importBundledModule(hitTestPath);
const density = await importBundledModule(densityPath);

function placed(factId, x, y) {
  return {
    point: { fact_id: factId, x: 0, y: 0, category: "test", retrieval_count: 1 },
    x,
    y,
    r: 4,
    color: "#fff",
  };
}

test("buildGrid bins points by cell and findNearest crosses neighboring cells", () => {
  const points = [placed(1, 10, 10), placed(2, 47, 47), placed(3, 52, 52)];
  const grid = hitTest.buildGrid(points);

  assert.deepEqual(grid.get("0,0"), [0, 1]);
  assert.deepEqual(grid.get("1,1"), [2]);

  const nearBoundary = hitTest.findNearest(points, grid, 49, 49, 8);
  assert.equal(nearBoundary?.point.fact_id, 2);

  const crossCell = hitTest.findNearest(points, grid, 50, 50, 8);
  assert.equal(crossCell?.point.fact_id, 3);

  const none = hitTest.findNearest(points, grid, 140, 140, 10);
  assert.equal(none, null);
});

test("buildDensity returns no cells for empty input", () => {
  assert.deepEqual(density.buildDensity([], 320, 180), []);
});

test("buildDensity caps opacities and spreads density beyond a single occupied cell", () => {
  const cluster = [
    placed(1, 60, 60),
    placed(2, 62, 58),
    placed(3, 64, 63),
    placed(4, 59, 61),
    placed(5, 61, 64),
    placed(6, 63, 62),
  ];

  const cells = density.buildDensity(cluster, 300, 180);
  assert.ok(cells.length > 1);
  assert.ok(cells.every((cell) => cell.opacity > 0));
  assert.ok(cells.every((cell) => cell.opacity <= 0.34));
  assert.ok(cells.some((cell) => cell.opacity === 0.34));
});
