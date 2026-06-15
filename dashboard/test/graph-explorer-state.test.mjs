import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

import { importBundledModule } from "./helpers/module-loader.mjs";

const statePath = path.resolve(process.cwd(), "graph/src/explorerState.ts");
const state = await importBundledModule(statePath);

function node(id, kind, filePath, extra = {}) {
  return {
    id,
    kind,
    name: extra.name || id,
    qualified_name: extra.qualified_name || id,
    file_path: filePath,
    ...extra,
  };
}

function edge(source, target, kind = "calls", extra = {}) {
  return { source, target, kind, ...extra };
}

test("mergeNodesInto keeps prior fields while overlaying refreshed node data", () => {
  const previous = new Map([
    [
      "fn:alpha",
      node("fn:alpha", "function", "src/alpha.rs", {
        signature: "fn alpha()",
        doc: "old doc",
        degree: 2,
      }),
    ],
  ]);

  const merged = state.mergeNodesInto(previous, [
    node("fn:alpha", "function", "src/alpha.rs", {
      degree: 7,
      visibility: "pub",
    }),
    node("type:beta", "struct", "src/beta.rs", { degree: 1 }),
  ]);

  assert.equal(merged.size, 2);
  assert.deepEqual(merged.get("fn:alpha"), {
    id: "fn:alpha",
    kind: "function",
    name: "fn:alpha",
    qualified_name: "fn:alpha",
    file_path: "src/alpha.rs",
    signature: "fn alpha()",
    doc: "old doc",
    degree: 7,
    visibility: "pub",
  });
  assert.deepEqual(previous.get("fn:alpha")?.degree, 2);
});

test("applyGraphFilters intersects family, language, and directory filters while culling orphan edges", () => {
  const nodes = [
    node("a", "function", "src/core/a.rs"),
    node("b", "method", "src/core/b.rs"),
    node("c", "class", "web/c.ts"),
    node("d", "function", "scripts/d.py"),
  ];
  const edges = [
    edge("a", "b"),
    edge("a", "c"),
    edge("b", "d"),
  ];

  const visible = state.applyGraphFilters(nodes, edges, {
    kindFilters: new Set(["fn"]),
    langFilters: new Set(["rust"]),
    dirScope: "src/core/",
  });

  assert.deepEqual(
    visible.nodes.map((entry) => entry.id),
    ["a", "b"],
  );
  assert.deepEqual(visible.edges, [edge("a", "b")]);
});

test("deriveChipOptions returns sorted unique families and languages from loaded nodes", () => {
  const options = state.deriveChipOptions([
    node("a", "class", "web/a.tsx"),
    node("b", "function", "src/b.rs"),
    node("c", "method", "src/c.rs"),
    node("d", "field", "pkg/d.py"),
  ]);

  assert.deepEqual(options, {
    families: ["fn", "type", "value"],
    languages: ["python", "rust", "typescript"],
  });
});

test("toggleStringSet adds and removes values without mutating the original set", () => {
  const original = new Set(["rust"]);
  const added = state.toggleStringSet(original, "fn");
  const removed = state.toggleStringSet(added, "rust");

  assert.deepEqual([...original], ["rust"]);
  assert.deepEqual([...added].sort(), ["fn", "rust"]);
  assert.deepEqual([...removed], ["fn"]);
});

test("appendFocusHistory de-duplicates revisits and keeps only the latest eight entries", () => {
  const previous = Array.from({ length: 8 }, (_, index) => ({
    id: `n${index}`,
    name: `Node ${index}`,
  }));

  const deduped = state.appendFocusHistory(previous, { id: "n3", name: "Node 3 revisited" });
  assert.deepEqual(deduped.map((entry) => entry.id), ["n0", "n1", "n2", "n4", "n5", "n6", "n7", "n3"]);
  assert.equal(deduped.at(-1)?.name, "Node 3 revisited");

  const appended = state.appendFocusHistory(deduped, { id: "n9", name: "Node 9" });
  assert.deepEqual(appended.map((entry) => entry.id), ["n1", "n2", "n4", "n5", "n6", "n7", "n3", "n9"]);
});
