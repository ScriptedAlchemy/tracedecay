import { rspack } from "@rspack/core";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

function restoreGlobals(previous) {
  for (const [key, prior] of previous.entries()) {
    if (prior.exists) {
      globalThis[key] = prior.value;
    } else {
      delete globalThis[key];
    }
  }
}

function runRspack(config) {
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

export async function importBundledModule(entryPath, { globals = {} } = {}) {
  const previous = new Map();
  for (const [key, value] of Object.entries(globals)) {
    previous.set(key, {
      exists: Object.prototype.hasOwnProperty.call(globalThis, key),
      value: globalThis[key],
    });
    globalThis[key] = value;
  }

  const outDir = mkdtempSync(path.join(tmpdir(), "td-module-loader-"));
  try {
    await runRspack({
      mode: "development",
      entry: { bundled: entryPath },
      experiments: { outputModule: true },
      output: {
        module: true,
        library: { type: "module" },
        path: outDir,
        filename: "bundled.mjs",
      },
      resolve: { extensions: [".tsx", ".ts", ".jsx", ".js", ".json"] },
      module: {
        rules: [
          {
            test: /\.(tsx?|jsx)$/,
            exclude: /node_modules/,
            use: {
              loader: "builtin:swc-loader",
              options: {
                jsc: {
                  parser: {
                    syntax: "typescript",
                    tsx: true,
                  },
                  transform: { react: { runtime: "automatic" } },
                },
              },
            },
          },
        ],
      },
      // The tests need real React elements (e.g. sdk.jsx's `import React`),
      // so bundle `react` from node_modules rather than externalizing it.
      optimization: { minimize: false, splitChunks: false, runtimeChunk: false },
      performance: { hints: false },
      stats: { preset: "errors-only" },
    });

    const outFile = path.join(outDir, "bundled.mjs");
    return await import(pathToFileURL(outFile).href + "?t=" + Date.now());
  } finally {
    restoreGlobals(previous);
    try {
      rmSync(outDir, { recursive: true, force: true });
    } catch {
      // best-effort cleanup
    }
  }
}
