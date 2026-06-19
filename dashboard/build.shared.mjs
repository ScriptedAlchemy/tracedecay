import { createRsbuild, rspack } from "@rsbuild/core";
import { pluginReact } from "@rsbuild/plugin-react";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs/promises";

export const dashboardRoot = path.dirname(fileURLToPath(import.meta.url));

const require = createRequire(path.join(dashboardRoot, "package.json"));
const EXTENSIONS = [".tsx", ".ts", ".jsx", ".js", ".json"];

export const EMBEDDED_DIST_FILES = [
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
];

export const HERMES_WRAPPER_DIST_FILES = [
  "hermes-wrapper/dist/index.js",
  "hermes-wrapper/dist/holographic.js",
  "hermes-wrapper/dist/lcm.js",
  "hermes-wrapper/dist/graph.js",
  "hermes-wrapper/dist/savings.js",
  "hermes-wrapper/dist/style.css",
];

function rsbuildEntry(importPath) {
  return { import: importPath, html: false };
}

function applySingleBundleOutput(config, bannerLabel) {
  config.optimization = {
    ...(config.optimization || {}),
    splitChunks: false,
    runtimeChunk: false,
  };
  config.performance = { ...(config.performance || {}), hints: false };

  if (bannerLabel) {
    config.plugins.push(
      new rspack.BannerPlugin({
        banner: `tracedecay ${bannerLabel} dashboard plugin - bundled with Rsbuild/Rspack. Do not edit; see src/.`,
        entryOnly: true,
      }),
    );
  }
}

function createBundleConfig({ entryName, entry, outDir, filename, alias = {}, bannerLabel }) {
  return {
    root: dashboardRoot,
    mode: "production",
    source: {
      entry: { [entryName]: rsbuildEntry(entry) },
      define: { "process.env.NODE_ENV": JSON.stringify("production") },
    },
    resolve: {
      alias,
      extensions: EXTENSIONS,
    },
    output: {
      distPath: { root: outDir, js: "." },
      filename: { js: filename },
      filenameHash: false,
      cleanDistPath: true,
      legalComments: "none",
      minify: true,
    },
    performance: {
      chunkSplit: { strategy: "all-in-one" },
      printFileSize: false,
    },
    plugins: [pluginReact()],
    tools: {
      rspack(config) {
        applySingleBundleOutput(config, bannerLabel);
      },
    },
  };
}

export function createShellBuildConfig() {
  return createBundleConfig({
    entryName: "shell",
    entry: "./shell/src/main.jsx",
    outDir: path.join(dashboardRoot, "shell/dist"),
    filename: "shell.js",
  });
}

export function createPluginBuildConfig(
  dir,
  bannerLabel,
  { shimDir = path.join(dashboardRoot, "lib") } = {},
) {
  return createBundleConfig({
    entryName: "index",
    entry: `./${dir}/src/entry.tsx`,
    outDir: path.join(dashboardRoot, dir, "dist"),
    filename: "index.js",
    bannerLabel,
    alias: {
      "react$": path.join(shimDir, "react-shim.ts"),
      "react/jsx-runtime$": path.join(shimDir, "jsx-runtime.ts"),
      "react/jsx-dev-runtime$": path.join(shimDir, "jsx-runtime.ts"),
    },
  });
}

export function createDashboardDevConfig({ apiTarget, host, port }) {
  return {
    root: dashboardRoot,
    source: {
      entry: { index: "./dev/main.tsx" },
    },
    html: {
      template: "./dev/index.html",
      title: "tracedecay dashboard (dev)",
    },
    server: {
      host,
      port,
      proxy: {
        "/api": {
          target: apiTarget,
          changeOrigin: true,
        },
      },
    },
    plugins: [pluginReact()],
  };
}

export async function runRsbuildConfig(rsbuildConfig) {
  const rsbuild = await createRsbuild({ cwd: dashboardRoot, rsbuildConfig });
  const result = await rsbuild.build();
  const stats = result.stats;
  if (stats?.hasErrors?.()) {
    const info = stats.toJson({ all: false, errors: true });
    throw new Error(info.errors.map((error) => error.message).join("\n"));
  }
  await result.close?.();
  return stats;
}

export async function buildShell() {
  await runRsbuildConfig(createShellBuildConfig());
  await fs.copyFile(
    path.join(dashboardRoot, "shell/src/styles.css"),
    path.join(dashboardRoot, "shell/dist/shell.css"),
  );
}

export async function buildPlugin(
  dir,
  bannerLabel,
  { shimDir = path.join(dashboardRoot, "lib"), tailwind = false, primitives = false } = {},
) {
  await runRsbuildConfig(createPluginBuildConfig(dir, bannerLabel, { shimDir }));
  const distCss = path.join(dashboardRoot, dir, "dist/style.css");
  await fs.mkdir(path.dirname(distCss), { recursive: true });
  if (tailwind) {
    await compileTailwindCss(path.join(dashboardRoot, dir, "src"), distCss);
  } else {
    await fs.copyFile(path.join(dashboardRoot, dir, "src/styles.css"), distCss);
  }
  if (primitives) {
    const [prim, plugin] = await Promise.all([
      fs.readFile(path.join(dashboardRoot, "lib/primitives.css"), "utf8"),
      fs.readFile(distCss, "utf8"),
    ]);
    await fs.writeFile(distCss, `${prim}\n${plugin}`, "utf8");
  }
}

export async function buildHolographicPlugin() {
  await buildPlugin("holographic", "holographic-memory", {
    shimDir: path.join(dashboardRoot, "holographic/src"),
    tailwind: true,
  });
}

export async function compileHolographicTailwindCss() {
  await compileTailwindCss(
    path.join(dashboardRoot, "holographic/src"),
    path.join(dashboardRoot, "holographic/dist/style.css"),
  );
}

export async function compileTailwindCss(srcDir, outFile) {
  const { compile } = require("@tailwindcss/node");
  const { Scanner } = require("@tailwindcss/oxide");
  const input = await fs.readFile(path.join(srcDir, "styles.css"), "utf8");
  const compiler = await compile(input, { base: dashboardRoot, onDependency: () => {} });
  const sources =
    compiler.root === "none"
      ? []
      : compiler.root === null
        ? [{ base: srcDir, pattern: "**/*", negated: false }]
        : [{ ...compiler.root, negated: false }];
  const scanner = new Scanner({ sources: sources.concat(compiler.sources ?? []) });
  const candidates = scanner.scan();
  let css = compiler.build(candidates);
  css = stripTopLevelAtLayer(css, "theme");
  css = stripTopLevelAtLayer(css, "base");
  css = `@layer hermes-plugin{\n${css}\n}`;
  css = minifyCss(css);
  await fs.mkdir(path.dirname(outFile), { recursive: true });
  await fs.writeFile(outFile, css, "utf8");
}

export function minifyCss(css) {
  return css
    .replace(/\/\*[^*]*\*+(?:[^/*][^*]*\*+)*\//g, "")
    .replace(/\s+/g, " ")
    .replace(/\s*([{}:;,>])\s*/g, "$1")
    .replace(/;}/g, "}")
    .trim();
}

export function stripTopLevelAtLayer(css, name) {
  const re = new RegExp(`@layer\\s+${name}\\s*\\{`, "g");
  let out = css;
  let match;
  while ((match = re.exec(out)) !== null) {
    const idx = match.index;
    let i = idx + match[0].length;
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

export async function buildHermesWrapper() {
  const dist = path.join(dashboardRoot, "hermes-wrapper/dist");
  await fs.mkdir(dist, { recursive: true });
  await fs.copyFile(path.join(dashboardRoot, "hermes-wrapper/src/entry.js"), path.join(dist, "index.js"));
  await fs.copyFile(path.join(dashboardRoot, "holographic/dist/index.js"), path.join(dist, "holographic.js"));
  await fs.copyFile(path.join(dashboardRoot, "lcm/dist/index.js"), path.join(dist, "lcm.js"));
  await fs.copyFile(path.join(dashboardRoot, "graph/dist/index.js"), path.join(dist, "graph.js"));
  await fs.copyFile(path.join(dashboardRoot, "savings/dist/index.js"), path.join(dist, "savings.js"));
  const css = await Promise.all([
    fs.readFile(path.join(dashboardRoot, "hermes-wrapper/src/wrapper.css"), "utf8"),
    fs.readFile(path.join(dashboardRoot, "holographic/dist/style.css"), "utf8"),
    fs.readFile(path.join(dashboardRoot, "lcm/dist/style.css"), "utf8"),
    fs.readFile(path.join(dashboardRoot, "graph/dist/style.css"), "utf8"),
    fs.readFile(path.join(dashboardRoot, "savings/dist/style.css"), "utf8"),
  ]);
  await fs.writeFile(path.join(dist, "style.css"), css.join("\n"), "utf8");
}

export async function logBuiltFiles(files) {
  for (const file of files) {
    const stat = await fs.stat(path.join(dashboardRoot, file));
    console.log(`✓ ${file}  ${(stat.size / 1024).toFixed(1)} KB`);
  }
}
