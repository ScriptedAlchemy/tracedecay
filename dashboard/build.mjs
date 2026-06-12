/**
 * Build every dashboard artifact served by `tracedecay dashboard`.
 *
 *   npm install && npm run build      (from dashboard/)
 *
 * Outputs:
 *   shell/dist/shell.js + shell.css   Standalone host shell (bundles React 19,
 *                                     exposes a Hermes-compatible plugin SDK on
 *                                     window, loads the plugin bundles below).
 *   holographic/dist/index.js         Holographic-memory plugin bundle, rebuilt
 *                                     from holographic/src (esbuild IIFE; React
 *                                     externalized onto the host SDK via shims,
 *                                     exactly like the original Hermes build).
 *   holographic/dist/style.css        Copied from holographic/src/styles.css
 *                                     (hand-rolled token stylesheet).
 *   lcm/dist/index.js + style.css     Copied from lcm/src (hand-written,
 *                                     unbundled JS — no build step needed).
 *   graph/dist/index.js + style.css   Code graph explorer plugin bundle
 *                                     (esbuild IIFE; React externalized).
 *
 * The Rust binary embeds these dist files at compile time (src/dashboard/assets.rs),
 * so run this before `cargo build` when the UI changed.
 */

import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs/promises";
import esbuild from "esbuild";

const root = path.dirname(fileURLToPath(import.meta.url));

async function buildShell() {
  await esbuild.build({
    entryPoints: [path.join(root, "shell/src/main.jsx")],
    outfile: path.join(root, "shell/dist/shell.js"),
    bundle: true,
    format: "iife",
    platform: "browser",
    target: ["es2020"],
    jsx: "automatic",
    minify: true,
    legalComments: "none",
    define: { "process.env.NODE_ENV": '"production"' },
    logLevel: "warning",
  });
  await fs.copyFile(
    path.join(root, "shell/src/styles.css"),
    path.join(root, "shell/dist/shell.css"),
  );
}

/**
 * Builds one plugin bundle (`<dir>/src/entry.tsx` → `<dir>/dist/index.js`),
 * externalizing React onto the host SDK (Hermes or the standalone shell) via
 * the shims; everything else (@observablehq/plot, d3-force, lucide-react) is
 * bundled.
 *
 * Shims default to the shared `lib/` copies. holographic/ overrides with its
 * own in-tree shims: that source mirrors the upstream Hermes plugin
 * byte-for-byte (see build.from-hermes.mjs) and must stay self-contained.
 */
async function buildPlugin(dir, bannerLabel, { shimDir = path.join(root, "lib") } = {}) {
  const srcDir = path.join(root, dir, "src");
  await esbuild.build({
    entryPoints: [path.join(srcDir, "entry.tsx")],
    outfile: path.join(root, dir, "dist/index.js"),
    bundle: true,
    format: "iife",
    platform: "browser",
    target: ["es2020"],
    jsx: "automatic",
    minify: true,
    legalComments: "none",
    define: { "process.env.NODE_ENV": '"production"' },
    alias: {
      react: path.join(shimDir, "react-shim.ts"),
      "react/jsx-runtime": path.join(shimDir, "jsx-runtime.ts"),
      "react/jsx-dev-runtime": path.join(shimDir, "jsx-runtime.ts"),
    },
    banner: {
      js: `/* tracedecay ${bannerLabel} dashboard plugin — bundled with esbuild. Do not edit; see src/. */`,
    },
    logLevel: "warning",
  });
  await fs.copyFile(
    path.join(srcDir, "styles.css"),
    path.join(root, dir, "dist/style.css"),
  );
}

async function copyLcm() {
  const dist = path.join(root, "lcm/dist");
  await fs.mkdir(dist, { recursive: true });
  await fs.copyFile(path.join(root, "lcm/src/index.js"), path.join(dist, "index.js"));
  await fs.copyFile(path.join(root, "lcm/src/style.css"), path.join(dist, "style.css"));
}

/**
 * The Hermes wrapper plugin reuses the exact bundles above: its dist gets the
 * wrapper entry (registers the combined "tracedecay" tab), copies of
 * the child bundles, and a concatenated stylesheet. Deploy by copying
 * hermes-wrapper/{manifest.json,plugin_api.py,dist} into
 * hermes-agent/plugins/hermes_intelligence/dashboard/.
 */
async function buildHermesWrapper() {
  const dist = path.join(root, "hermes-wrapper/dist");
  await fs.mkdir(dist, { recursive: true });
  await fs.copyFile(
    path.join(root, "hermes-wrapper/src/entry.js"),
    path.join(dist, "index.js"),
  );
  await fs.copyFile(
    path.join(root, "holographic/dist/index.js"),
    path.join(dist, "holographic.js"),
  );
  await fs.copyFile(path.join(root, "lcm/dist/index.js"), path.join(dist, "lcm.js"));
  await fs.copyFile(path.join(root, "graph/dist/index.js"), path.join(dist, "graph.js"));
  await fs.copyFile(path.join(root, "savings/dist/index.js"), path.join(dist, "savings.js"));
  const css = await Promise.all([
    fs.readFile(path.join(root, "hermes-wrapper/src/wrapper.css"), "utf8"),
    fs.readFile(path.join(root, "holographic/dist/style.css"), "utf8"),
    fs.readFile(path.join(root, "lcm/dist/style.css"), "utf8"),
    fs.readFile(path.join(root, "graph/dist/style.css"), "utf8"),
    fs.readFile(path.join(root, "savings/dist/style.css"), "utf8"),
  ]);
  await fs.writeFile(path.join(dist, "style.css"), css.join("\n"), "utf8");
}

async function main() {
  await fs.mkdir(path.join(root, "shell/dist"), { recursive: true });
  await Promise.all([
    buildShell(),
    buildPlugin("holographic", "holographic-memory", {
      shimDir: path.join(root, "holographic/src"),
    }),
    buildPlugin("graph", "code graph"),
    buildPlugin("savings", "savings & cost"),
    copyLcm(),
  ]);
  await buildHermesWrapper();
  for (const f of [
    "shell/dist/shell.js",
    "shell/dist/shell.css",
    "holographic/dist/index.js",
    "holographic/dist/style.css",
    "lcm/dist/index.js",
    "lcm/dist/style.css",
    "graph/dist/index.js",
    "graph/dist/style.css",
    "savings/dist/index.js",
    "savings/dist/style.css",
    "hermes-wrapper/dist/index.js",
    "hermes-wrapper/dist/graph.js",
    "hermes-wrapper/dist/savings.js",
    "hermes-wrapper/dist/style.css",
  ]) {
    const st = await fs.stat(path.join(root, f));
    console.log(`✓ ${f}  ${(st.size / 1024).toFixed(1)} KB`);
  }
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
