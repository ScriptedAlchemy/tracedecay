import { constants } from "node:fs";
import { access, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRsbuild, loadConfig } from "@rsbuild/core";
import { createRslib } from "@rslib/core";

// Builds the committed `embedded/` artifacts the plugin bundle launches
// (`plugin/mcp.json` → `node ./chatgpt-extension/embedded/server.mjs`) so an
// installed plugin needs no workspace install. Output is deterministic: no
// hashes, no source maps, every dependency bundled. `--check` only compares.
const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const embeddedDir = path.join(packageRoot, "embedded");
const check = process.argv.slice(2).includes("--check");

const outDir = await mkdtemp(path.join(tmpdir(), "tracedecay-chatgpt-ext-"));
let appHtml;
let serverJs;
try {
  const { content: appConfig } = await loadConfig({ cwd: packageRoot, path: path.join(packageRoot, "rsbuild.config.ts") });
  const app = await createRsbuild({
    cwd: packageRoot,
    rsbuildConfig: { ...appConfig, output: { ...appConfig.output, distPath: { root: path.join(outDir, "app") }, cleanDistPath: false }, logLevel: "warn" },
  });
  await app.build();
  const appOut = path.join(outDir, "app");
  const html = await readFile(path.join(appOut, "app.html"), "utf8");
  if (!html.includes("<script") || /<script[^>]+src=/u.test(html) || /<link[^>]+rel="stylesheet"/u.test(html)) {
    throw new Error("app.html is not self-contained: external script or stylesheet references remain");
  }
  const leftovers = (await readdir(appOut, { recursive: true })).filter((entry) => /\.(js|css)$/u.test(entry));
  if (leftovers.length > 0) {
    throw new Error(`app bundle emitted external chunks that an MCP App host cannot load: ${leftovers.join(", ")}`);
  }
  appHtml = Buffer.from(html, "utf8");

  const rslib = await createRslib({
    cwd: packageRoot,
    config: {
      root: packageRoot,
      logLevel: "warn",
      lib: [
        {
          format: "esm",
          syntax: ["node 20"],
          bundle: true,
          autoExternal: false,
          source: { entry: { server: path.join(packageRoot, "src", "server", "main.ts") } },
          output: {
            target: "node",
            distPath: { root: path.join(outDir, "server") },
            cleanDistPath: false,
            filename: { js: "[name].mjs" },
            minify: true,
            sourceMap: false,
            legalComments: "none",
          },
        },
      ],
    },
  });
  await rslib.build();
  serverJs = await readFile(path.join(outDir, "server", "server.mjs"));
  const serverLeftovers = (await readdir(path.join(outDir, "server"), { recursive: true })).filter((entry) => entry !== "server.mjs");
  if (serverLeftovers.length > 0) {
    throw new Error(`server bundle emitted extra files: ${serverLeftovers.join(", ")}`);
  }
} finally {
  await rm(outDir, { recursive: true, force: true });
}
if (appHtml.length === 0 || serverJs.length === 0) throw new Error("build produced an empty artifact");

const targets = [
  ["app.html", appHtml],
  ["server.mjs", serverJs],
];
if (check) {
  for (const [name, bytes] of targets) {
    const target = path.join(embeddedDir, name);
    await access(target, constants.R_OK);
    if (!(await readFile(target)).equals(bytes)) {
      throw new Error(`embedded/${name} is stale; run pnpm run build and commit the result`);
    }
  }
  process.stdout.write("embedded artifacts are current\n");
} else {
  await mkdir(embeddedDir, { recursive: true });
  for (const [name, bytes] of targets) await writeFile(path.join(embeddedDir, name), bytes);
  process.stdout.write(`built embedded/app.html (${appHtml.length} bytes) and embedded/server.mjs (${serverJs.length} bytes)\n`);
}
