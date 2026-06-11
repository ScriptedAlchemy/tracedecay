import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import vm from "node:vm";
import { readFile } from "node:fs/promises";

const lcmPath = path.resolve(process.cwd(), "lcm/src/index.js");

async function loadLcmExports() {
  const source = await readFile(lcmPath, "utf8");
  const instrumented = source.replace(
    /\}\)\(\);\s*$/,
    `
globalThis.__LCM_TEST_EXPORTS__ = {
  stripMd,
  summaryTitle,
  sessionLabel,
  sessionTail,
  parseLeadingJSON,
  ratioStr,
  mergeRows,
  mergeSearchPayload,
  TimelineChart
};
})();
`,
  );

  if (instrumented === source) {
    throw new Error("Failed to instrument lcm/src/index.js for tests");
  }

  const noop = () => {};
  const context = {
    window: {
      __HERMES_PLUGIN_SDK__: {
        React: {
          createElement: (type, props, ...children) => ({
            type,
            props: { ...(props || {}), children: children.length <= 1 ? children[0] : children },
          }),
        },
        hooks: {
          useEffect: noop,
          useMemo: (fn) => fn(),
          useState: (value) => [value, noop],
          useCallback: (fn) => fn,
        },
        utils: {},
      },
      __HERMES_PLUGINS__: {
        register: noop,
      },
    },
  };
  context.globalThis = context;

  vm.runInNewContext(instrumented, context, { filename: "lcm/src/index.js" });
  const exports = context.__LCM_TEST_EXPORTS__;
  if (!exports) {
    throw new Error("LCM test exports were not captured");
  }
  return exports;
}

const lcm = await loadLcmExports();

test("stripMd flattens markdown syntax into readable plain text", () => {
  const input = `
# Heading
**Bold** and *italic* with [link text](https://example.com).
\`inline\`
\`\`\`js
const hidden = true;
\`\`\`
- item
> quote
`;
  assert.equal(lcm.stripMd(input), "Heading Bold and italic with link text. inline item quote");
});

test("summaryTitle prefers heading, then bold, then first sentence", () => {
  assert.equal(lcm.summaryTitle("## Release Notes\nParagraph"), "Release Notes");
  assert.equal(lcm.summaryTitle("Intro **Strong Title** tail"), "Strong Title");
  assert.equal(
    lcm.summaryTitle("first sentence ends here. second sentence"),
    "first sentence ends here.",
  );
});

test("sessionLabel and sessionTail parse dashboard session IDs", () => {
  const id = "20260529_011608_ab12cd";
  assert.equal(lcm.sessionLabel(id), "2026-05-29 01:16");
  assert.equal(lcm.sessionTail(id), "ab12cd");
  assert.equal(lcm.sessionLabel("manual-session-id"), "manual-session-id");
  assert.equal(lcm.sessionTail("manual-session-id"), "");
});

test("parseLeadingJSON extracts JSON payload and trailing notes", () => {
  const parsed = lcm.parseLeadingJSON('{"count":2,"items":[1,2]}\n[use offset=120]');
  const normalized = JSON.parse(JSON.stringify(parsed));
  assert.deepEqual(normalized, {
    value: { count: 2, items: [1, 2] },
    rest: "[use offset=120]",
  });
  assert.equal(lcm.parseLeadingJSON("not-json"), null);
});

test("ratioStr returns dash for empty output and rounded ratio otherwise", () => {
  assert.equal(lcm.ratioStr(100, 0), "\u2014");
  assert.equal(lcm.ratioStr(99, 33), `3${"\u00d7"}`);
  assert.equal(lcm.ratioStr(10, 6), `1.7${"\u00d7"}`);
});

test("mergeRows appends new rows and dedupes by id", () => {
  const merged = lcm.mergeRows(
    [{ store_id: 1, body: "a" }, { store_id: 2, body: "b" }],
    [{ store_id: 2, body: "b-dup" }, { store_id: 3, body: "c" }],
    "store_id",
  );
  assert.deepEqual(merged.map((row) => row.store_id), [1, 2, 3]);
  // The first-seen row wins on id collisions.
  assert.equal(merged[1].body, "b");
});

test("mergeSearchPayload appends match rows but takes scalars from the new page", () => {
  const prev = {
    engine: "fts",
    total: { messages: 448, summary_nodes: 0 },
    matches: {
      messages: [{ store_id: 1 }, { store_id: 2 }],
      summary_nodes: [{ node_id: 10 }],
    },
  };
  const next = {
    engine: "fts",
    total: { messages: 450, summary_nodes: 1 },
    matches: {
      messages: [{ store_id: 2 }, { store_id: 3 }],
      summary_nodes: [{ node_id: 11 }],
    },
  };
  const merged = lcm.mergeSearchPayload(prev, next);
  assert.deepEqual(merged.total, { messages: 450, summary_nodes: 1 });
  assert.deepEqual(merged.matches.messages.map((row) => row.store_id), [1, 2, 3]);
  assert.deepEqual(merged.matches.summary_nodes.map((row) => row.node_id), [10, 11]);
});

function flattenText(node, out = []) {
  if (node == null) return out;
  if (typeof node === "string" || typeof node === "number") {
    out.push(String(node));
    return out;
  }
  if (Array.isArray(node)) {
    node.forEach((child) => flattenText(child, out));
    return out;
  }
  if (node.props) flattenText(node.props.children, out);
  return out;
}

test("TimelineChart drops null buckets and reports undated messages honestly", () => {
  // A NULL-timestamp bucket from an older server must never render as a bar.
  const rendered = lcm.TimelineChart({
    buckets: [
      { bucket: null, count: 500 },
      { bucket: "2026-06-10", count: 3 },
      { bucket: "2026-06-11", count: 7 },
    ],
    nodeBuckets: [],
    undatedCount: 500,
  });
  const bars = rendered.props.children[0];
  assert.equal(bars.props.children.length, 2);
  const text = flattenText(rendered).join(" ");
  assert.match(text, /500 undated messages not shown/);
});

test("TimelineChart explains all-undated stores instead of showing a fake bar", () => {
  const empty = lcm.TimelineChart({ buckets: [], nodeBuckets: [], undatedCount: 42 });
  assert.match(flattenText(empty).join(" "), /42 stored messages have no timestamp/);
  const noData = lcm.TimelineChart({ buckets: [], nodeBuckets: [], undatedCount: 0 });
  assert.match(flattenText(noData).join(" "), /No timeline data/);
});

test("mergeSearchPayload passes the page through when there is no previous payload", () => {
  const next = { total: { messages: 1 }, matches: { messages: [{ store_id: 9 }] } };
  assert.equal(lcm.mergeSearchPayload(null, next), next);
  // Missing matches on either side degrade to empty arrays, not crashes.
  // (JSON round-trip normalizes vm-realm arrays for deepEqual.)
  const degenerate = lcm.mergeSearchPayload({ matches: undefined }, { matches: undefined });
  assert.deepEqual(JSON.parse(JSON.stringify(degenerate.matches)), {
    messages: [],
    summary_nodes: [],
  });
});
