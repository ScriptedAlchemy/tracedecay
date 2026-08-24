import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const canvasPath = path.resolve(process.cwd(), "graph/src/canvasHelpers.ts");
const canvas = await importBundledModule(canvasPath);

function makeTheme() {
  return {
    accents: {
      amber: "#f7c76a",
      blue: "#7aa7ff",
      pink: "#ff7ab6",
      green: "#67e8a9",
      muted: "#a8c8c0",
    },
  };
}

test("withAlpha converts six-digit hex colors and leaves non-hex values untouched", () => {
  assert.equal(canvas.withAlpha("#75f4d2", 0.5), "rgba(117, 244, 210, 0.5)");
  assert.equal(canvas.withAlpha("var(--ts-cyan)", 0.5), "var(--ts-cyan)");
});

test("edgeStyle maps known edge kinds to themed colors and falls back for unknown kinds", () => {
  assert.deepEqual(canvas.edgeStyle("calls", makeTheme()), {
    color: "rgba(247, 199, 106, 0.55)",
    dash: [],
    width: 1.4,
  });

  assert.deepEqual(canvas.edgeStyle("mystery_edge", makeTheme()), {
    color: "rgba(168, 200, 192, 0.3)",
    dash: [],
    width: 1,
  });
});

test("fitCameraToNodes centers and scales the camera to the simulated bounds", () => {
  const camera = { x: 999, y: -999, k: 0.01 };
  const sim = {
    nodes: [
      { id: "a", x: -50, y: -20, radius: 10 },
      { id: "b", x: 50, y: 20, radius: 10 },
    ],
  };

  canvas.fitCameraToNodes({ width: 400, height: 200 }, sim, camera);

  assert.equal(camera.x, 0);
  assert.equal(camera.y, 0);
  assert.ok(Math.abs(camera.k - 2.9333333333333336) < 1e-9, `camera.k=${camera.k}`);
});

test("fitCameraToNodes smooth mode lerps toward the target framing instead of snapping", () => {
  const camera = { x: 100, y: -50, k: 1 };
  const sim = {
    nodes: [
      { id: "a", x: -50, y: -20, radius: 10 },
      { id: "b", x: 50, y: 20, radius: 10 },
    ],
  };

  canvas.fitCameraToNodes({ width: 400, height: 200 }, sim, camera, true);

  assert.equal(camera.x, 80);
  assert.equal(camera.y, -40);
  assert.ok(Math.abs(camera.k - 1.3866666666666667) < 1e-9, `camera.k=${camera.k}`);
});

test("zoomCameraAtPoint keeps the world point under the cursor fixed while updating camera zoom", () => {
  const camera = { x: 25, y: -10, k: 1 };
  const rect = { left: 10, top: 20, width: 300, height: 180 };
  const point = { x: 220, y: 110 };

  const before = canvas.toWorldPoint(camera, rect, point);
  canvas.zoomCameraAtPoint(camera, rect, point, 2.4);
  const after = canvas.toWorldPoint(camera, rect, point);

  assert.ok(Math.abs(before.x - after.x) < 1e-9, `${before.x} !== ${after.x}`);
  assert.ok(Math.abs(before.y - after.y) < 1e-9, `${before.y} !== ${after.y}`);
  assert.equal(camera.k, 2.4);
});

test("hitTestNode and neighborhoodIds cover nearest-node hover and direct-neighbor expansion", () => {
  const rect = { left: 0, top: 0, width: 200, height: 200 };
  const camera = { x: 0, y: 0, k: 1 };
  const nodes = [
    { id: "a", x: 0, y: 0, radius: 10 },
    { id: "b", x: 40, y: 0, radius: 10 },
    { id: "c", x: 0, y: 60, radius: 10 },
  ];
  const edges = [
    { source: "a", target: "b" },
    { source: "c", target: "a" },
  ];

  const hovered = canvas.hitTestNode(nodes, camera, rect, { x: 101, y: 96 });
  assert.equal(hovered?.id, "a");
  assert.equal(canvas.hitTestNode(nodes, camera, rect, { x: 190, y: 190 }), null);
  assert.deepEqual([...canvas.neighborhoodIds("a", edges)].sort(), ["a", "b", "c"]);
});
