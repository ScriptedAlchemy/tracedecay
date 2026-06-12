/**
 * Build the holographic-memory dashboard plugin bundle.
 *
 *   node build.mjs
 *
 * Produces:
 *   dist/index.js   IIFE bundle (esbuild). React + react/jsx-runtime are
 *                   externalized onto the host SDK via the `src/react-shim.ts`
 *                   and `src/jsx-runtime.ts` shims (esbuild `alias`), so the
 *                   plugin shares the host dashboard's single React instance.
 *                   @observablehq/plot, d3-force, and lucide-react ARE bundled
 *                   (they are not on the plugin SDK).
 *   dist/style.css  Tailwind utilities for this plugin's markup, compiled with
 *                   the Tailwind v4 engine against a mirror of the host theme.
 *                   The emitted `@layer theme` (:root vars) and `@layer base`
 *                   (preflight) blocks are stripped so the file never clobbers
 *                   host theme vars or resets host elements.
 *
 * Tooling (esbuild, @tailwindcss/node, @tailwindcss/oxide) is resolved from the
 * sibling `web/node_modules` checkout.
 */

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs/promises";

const dashboardDir = path.dirname(fileURLToPath(import.meta.url));
const srcDir = path.join(dashboardDir, "src");
const distDir = path.join(dashboardDir, "dist");
const webDir = path.resolve(dashboardDir, "../../../../web");

const require = createRequire(path.join(webDir, "package.json"));
const esbuild = require("esbuild");

async function buildJS() {
  const result = await esbuild.build({
    absWorkingDir: webDir,
    entryPoints: [path.join(srcDir, "entry.tsx")],
    outfile: path.join(distDir, "index.js"),
    bundle: true,
    format: "iife",
    platform: "browser",
    target: ["es2020"],
    // The plugin src lives outside web/, so esbuild can't reach web/node_modules
    // by walking up from the entry. Point it there for bare imports
    // (@observablehq/plot, d3-force, lucide-react).
    nodePaths: [path.join(webDir, "node_modules")],
    jsx: "automatic",
    minify: true,
    legalComments: "none",
    define: { "process.env.NODE_ENV": '"production"' },
    // Externalize React onto the host SDK; bundle everything else.
    alias: {
      react: path.join(srcDir, "react-shim.ts"),
      "react/jsx-runtime": path.join(srcDir, "jsx-runtime.ts"),
      "react/jsx-dev-runtime": path.join(srcDir, "jsx-runtime.ts"),
    },
    banner: {
      js: "/* Hermes holographic-memory dashboard plugin — bundled with esbuild. Do not edit; see src/. */",
    },
    metafile: true,
    logLevel: "warning",
  });
  return result;
}

/** Remove a top-level `@layer <name> { ... }` block via brace matching. */
function stripTopLevelAtLayer(css, name) {
  const marker = `@layer ${name} {`;
  let out = css;
  for (;;) {
    const idx = out.indexOf(marker);
    if (idx === -1) break;
    let i = idx + marker.length;
    let depth = 1;
    while (i < out.length && depth > 0) {
      const ch = out[i];
      if (ch === "{") depth++;
      else if (ch === "}") depth--;
      i++;
    }
    out = out.slice(0, idx) + out.slice(i);
  }
  return out;
}

async function buildCSS() {
  const { compile } = require("@tailwindcss/node");
  const { Scanner } = require("@tailwindcss/oxide");

  const input = await fs.readFile(path.join(srcDir, "styles.css"), "utf8");
  const compiler = await compile(input, {
    // Resolve `@import "tailwindcss"` from web/node_modules.
    base: webDir,
    onDependency: () => {},
  });

  // Scan THIS plugin's source for class candidates (not the whole web app).
  const sources =
    compiler.root === "none"
      ? []
      : compiler.root === null
        ? [{ base: srcDir, pattern: "**/*", negated: false }]
        : [{ ...compiler.root, negated: false }];
  const scanner = new Scanner({ sources: sources.concat(compiler.sources ?? []) });
  const candidates = scanner.scan();

  let css = compiler.build(candidates);
  // Drop host-owned layers: theme (:root vars) + base (preflight reset).
  css = stripTopLevelAtLayer(css, "theme");
  css = stripTopLevelAtLayer(css, "base");
  // Minify with esbuild rather than Tailwind's lightningcss `optimize`, which
  // strips the `@supports (color: color-mix())` progressive-enhancement blocks
  // and collapses our themed colors to their plain fallback.
  css = (await esbuild.transform(css, { loader: "css", minify: true })).code;

  // Confine the whole plugin sheet to the host's `hermes-plugin` cascade layer
  // (declared in web/src/index.css as ...,components,hermes-plugin,utilities).
  // This sheet is injected as a plain <link> AFTER the host app CSS, so its
  // base utilities (.flex/.grid/.hidden/...) would otherwise share the host's
  // `utilities` layer and — being later in document order — override the host's
  // responsive variants (.lg:hidden / .lg:flex / .lg:sticky), breaking the
  // sidebar/menu layout. Ranked below host `utilities` but above `base`, the
  // plugin's own pages stay styled (they beat preflight) while the host layout
  // stays authoritative. Inner @layer blocks nest under hermes-plugin.
  css = `@layer hermes-plugin{${css}}`;

  await fs.mkdir(distDir, { recursive: true });
  await fs.writeFile(path.join(distDir, "style.css"), css, "utf8");
  return css.length;
}

async function main() {
  await fs.mkdir(distDir, { recursive: true });
  const [, cssBytes] = await Promise.all([buildJS(), buildCSS()]);
  const jsStat = await fs.stat(path.join(distDir, "index.js"));
  console.log(`✓ dist/index.js  ${(jsStat.size / 1024).toFixed(1)} KB`);
  console.log(`✓ dist/style.css ${(cssBytes / 1024).toFixed(1)} KB`);
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
