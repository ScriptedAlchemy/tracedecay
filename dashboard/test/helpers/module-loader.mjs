import { build } from "esbuild";

function restoreGlobals(previous) {
  for (const [key, prior] of previous.entries()) {
    if (prior.exists) {
      globalThis[key] = prior.value;
    } else {
      delete globalThis[key];
    }
  }
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

  try {
    const result = await build({
      entryPoints: [entryPath],
      bundle: true,
      format: "esm",
      platform: "browser",
      target: "es2022",
      write: false,
      logLevel: "silent",
    });
    const code = result.outputFiles[0]?.text;
    if (!code) {
      throw new Error(`esbuild produced no output for ${entryPath}`);
    }
    const encoded = Buffer.from(code).toString("base64");
    return import(`data:text/javascript;base64,${encoded}#${encodeURIComponent(entryPath)}`);
  } finally {
    restoreGlobals(previous);
  }
}
