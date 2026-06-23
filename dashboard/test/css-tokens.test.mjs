import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import {
  compileTailwindCss,
  dashboardRoot,
  minifyCss,
  prepareTailwindPluginCss,
} from "../build.shared.mjs";

// Regression guard: a previous build stripped the entire `@layer theme`, which
// dropped Tailwind's structural tokens (`--spacing`, `--text-*`, `--ease-*`) and
// silently collapsed every spacing utility. The fix only removes `--color-*`
// declarations and keeps the structural tokens. These assertions FAIL against
// the old whole-layer-strip behavior because the structural tokens would be gone.
test("prepareTailwindPluginCss drops --color-* but keeps structural tokens", () => {
  const css = `
@layer theme {
  :root, :host {
    --color-red-500: #f00;
    --color-blue-500: oklch(0.5 0.1 200);
    --spacing: 0.25rem;
    --text-xs: 0.75rem;
    --ease-out: cubic-bezier(0, 0, 0.2, 1);
  }
}
`;

  const out = minifyCss(prepareTailwindPluginCss(css));

  // Theme colors are removed so host colors win.
  assert.equal(out.includes("--color-red-500"), false);
  assert.equal(out.includes("--color-blue-500"), false);

  // Structural tokens must survive (these would be gone under the regression).
  assert.equal(out.includes("--spacing:0.25rem"), true);
  assert.equal(out.includes("--text-xs:0.75rem"), true);
  assert.equal(out.includes("--ease-out:cubic-bezier(0,0,0.2,1)"), true);
});

test("compiled holographic CSS defines --spacing end-to-end", async () => {
  const outFile = path.join(
    await fs.mkdtemp(path.join(os.tmpdir(), "tracedecay-css-tokens-")),
    "style.css",
  );
  try {
    await compileTailwindCss(path.join(dashboardRoot, "holographic/src"), outFile);
    const css = await fs.readFile(outFile, "utf8");
    assert.match(
      css,
      /--spacing:/,
      "compiled holographic CSS must define --spacing so spacing utilities resolve",
    );
  } finally {
    await fs.rm(path.dirname(outFile), { recursive: true, force: true });
  }
});
