/**
 * Build every dashboard artifact served by `tracedecay dashboard`.
 *
 *   npm install && npm run build      (from dashboard/)
 *
 * Outputs:
 *   shell/dist/shell.js + shell.css   Standalone host shell.
 *   holographic/dist/index.js         Holographic-memory plugin bundle.
 *   graph/dist/index.js               Code graph explorer plugin bundle.
 *   savings/dist/index.js             Savings plugin bundle.
 *   lcm/dist/index.js + style.css     Copied from lcm/src.
 *   hermes-wrapper/dist/*             Combined Hermes dashboard plugin.
 *
 * The Rust binary embeds these dist files at compile time
 * (src/dashboard/assets.rs), so run this before `cargo build` when the UI
 * changed.
 */

import { rspack } from "@rspack/core";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import esbuild from "esbuild";
import path from "node:path";
import fs from "node:fs/promises";

const root = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(path.join(root, "package.json"));

function swcRule(syntax, test) {
  const isTs = syntax === "typescript";
  return {
    test,
    exclude: /node_modules/,
    use: {
      loader: "builtin:swc-loader",
      options: {
        jsc: {
          parser: isTs
            ? { syntax: "typescript", tsx: true }
            : { syntax: "ecmascript", jsx: true },
          transform: { react: { runtime: "automatic" } },
        },
        env: { targets: "defaults" },
      },
    },
  };
}

function run(config) {
  return new Promise((resolve, reject) => {
    rspack(config, (err, stats) => {
      if (err) return reject(err);
      if (stats.hasErrors()) {
        const info = stats.toJson({ all: false, errors: true });
        return reject(new Error(info.errors.map((e) => e.message).join("\n")));
      }
      resolve(stats);
    });
  });
}

const RULES = [swcRule("ecmascript", /\.(jsx|js)$/), swcRule("typescript", /\.(tsx|ts)$/)];

function shellConfig() {
  return {
    mode: "production",
    context: root,
    entry: { shell: "./shell/src/main.jsx" },
    output: {
      path: path.join(root, "shell/dist"),
      filename: "shell.js",
      clean: true,
    },
    resolve: { extensions: [".jsx", ".js", ".json", ".ts", ".tsx"] },
    module: { rules: RULES },
    optimization: { minimize: true, splitChunks: false, runtimeChunk: false },
    performance: { hints: false },
  };
}

async function buildShell() {
  await run(shellConfig());
  await fs.copyFile(
    path.join(root, "shell/src/styles.css"),
    path.join(root, "shell/dist/shell.css"),
  );
}

/**
 * Builds one plugin bundle with React externalized onto the host SDK via shims.
 */
function pluginConfig(dir, shimDir, bannerLabel) {
  const srcDir = path.join(root, dir, "src");
  return {
    mode: "production",
    context: root,
    target: "web",
    entry: { index: path.join(srcDir, "entry.tsx") },
    output: {
      path: path.join(root, dir, "dist"),
      filename: "index.js",
      clean: true,
    },
    resolve: {
      extensions: [".tsx", ".ts", ".jsx", ".js", ".json"],
      alias: {
        "react$": path.join(shimDir, "react-shim.ts"),
        "react/jsx-runtime$": path.join(shimDir, "jsx-runtime.ts"),
        "react/jsx-dev-runtime$": path.join(shimDir, "jsx-runtime.ts"),
      },
    },
    module: { rules: RULES },
    optimization: { minimize: true, splitChunks: false, runtimeChunk: false },
    performance: { hints: false },
    plugins: [
      new rspack.BannerPlugin({
        banner: `tracedecay ${bannerLabel} dashboard plugin - bundled with Rspack. Do not edit; see src/.`,
        entryOnly: true,
      }),
    ],
  };
}

async function buildPlugin(
  dir,
  bannerLabel,
  { shimDir = path.join(root, "lib"), tailwind = false } = {},
) {
  await run(pluginConfig(dir, shimDir, bannerLabel));
  if (tailwind) {
    await compileTailwindCss(path.join(root, dir, "src"), path.join(root, dir, "dist/style.css"));
  } else {
    await fs.copyFile(
      path.join(root, dir, "src/styles.css"),
      path.join(root, dir, "dist/style.css"),
    );
  }
}

/**
 * Compile a plugin stylesheet with real Tailwind v4 (programmatic Oxide scan +
 * @tailwindcss/node compile). Mirrors the proven build.from-hermes.mjs path:
 *
 *   - scan the plugin src for class candidates;
 *   - strip @layer theme + @layer base so the plugin never clobbers the host's
 *     :root vars or preflight (utilities resolve --color-* against the host);
 *   - confine the sheet to the host's `hermes-plugin` cascade layer;
 *   - minify with esbuild (preserves @supports color-mix blocks that
 *     lightningcss would strip).
 */
async function compileTailwindCss(srcDir, outFile) {
  const { compile } = require("@tailwindcss/node");
  const { Scanner } = require("@tailwindcss/oxide");
  const input = await fs.readFile(path.join(srcDir, "styles.css"), "utf8");
  const compiler = await compile(input, { base: root, onDependency: () => {} });
  const scanner = new Scanner({ sources: [{ base: srcDir, pattern: "**/*", negated: false }] });
  const candidates = scanner.scan();
  let css = compiler.build(candidates);
  css = stripTopLevelAtLayer(css, "theme");
  css = stripTopLevelAtLayer(css, "base");
  css = `@layer hermes-plugin{\n${css}\n}`;
  css = (await esbuild.transform(css, { loader: "css", minify: true })).code;
  await fs.writeFile(outFile, css, "utf8");
}

/** Remove a top-level `@layer <name> { ... }` block via brace matching.
 *  Matches `@layer name{` or `@layer name {` (any whitespace). */
function stripTopLevelAtLayer(css, name) {
  const re = new RegExp(`@layer\\s+${name}\\s*\\{`, "g");
  let out = css;
  let m;
  while ((m = re.exec(out)) !== null) {
    const idx = m.index;
    let i = idx + m[0].length;
    let depth = 1;
    while (i < out.length && depth > 0) {
      const ch = out[i];
      if (ch === "{") depth++;
      else if (ch === "}") depth--;
      i++;
    }
    out = out.slice(0, idx) + out.slice(i);
    re.lastIndex = idx;
  }
  return out;
}

async function copyLcm() {
  const dist = path.join(root, "lcm/dist");
  await fs.mkdir(dist, { recursive: true });
  await fs.copyFile(path.join(root, "lcm/src/index.js"), path.join(dist, "index.js"));
  await fs.copyFile(path.join(root, "lcm/src/style.css"), path.join(dist, "style.css"));
}

/**
 * Builds the combined Hermes plugin from the child dashboard bundles.
 */
async function buildHermesWrapper() {
  const dist = path.join(root, "hermes-wrapper/dist");
  await fs.mkdir(dist, { recursive: true });
  await fs.copyFile(path.join(root, "hermes-wrapper/src/entry.js"), path.join(dist, "index.js"));
  await fs.copyFile(path.join(root, "holographic/dist/index.js"), path.join(dist, "holographic.js"));
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
      tailwind: true,
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
