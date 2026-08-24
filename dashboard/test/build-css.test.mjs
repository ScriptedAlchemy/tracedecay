import test from "node:test";
import assert from "node:assert/strict";

import { minifyCss, prepareTailwindPluginCss } from "../build.shared.mjs";

test("prepareTailwindPluginCss removes theme colors without dropping structural tokens", () => {
  const css = `
@layer theme, base, utilities;
@layer theme {
  :root, :host {
    --color-red-500: #f00;
    --spacing: .25rem;
  }
  @supports (color: oklch(0 0 0)) {
    :root {
      --color-blue-500: oklch(0.5 0.1 200);
      --ease-fluid: cubic-bezier(.3, 0, 0, 1);
    }
  }
}
@layer base {
  *, ::before, ::after { box-sizing: border-box; }
}
@layer utilities {
  .mt-4 { margin-top: calc(var(--spacing) * 4); }
}
@media (min-width: 48rem) {
  @layer base {
    .nested-base-layer { display: block; }
  }
}
`;

  const out = minifyCss(prepareTailwindPluginCss(css));

  assert.equal(out.includes("--color-red-500"), false);
  assert.equal(out.includes("--color-blue-500"), false);
  assert.equal(out.includes("@layer theme,base,utilities"), true);
  assert.equal(out.includes("--spacing:.25rem"), true);
  assert.equal(out.includes("--ease-fluid:cubic-bezier(.3,0,0,1)"), true);
  assert.equal(out.includes("box-sizing:border-box"), false);
  assert.equal(out.includes(".mt-4{margin-top:calc(var(--spacing) * 4)}"), true);
  assert.equal(out.includes(".nested-base-layer{display:block}"), true);
});
