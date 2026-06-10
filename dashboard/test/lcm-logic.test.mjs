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
  ratioStr
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
