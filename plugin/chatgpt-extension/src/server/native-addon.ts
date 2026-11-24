import { existsSync, realpathSync } from "node:fs";
import path from "node:path";

/** File name of the @tracedecay/sdk binding inside a release archive. */
const ADDON = "tracedecay_sdk_node.node";

/**
 * The SDK binding shipped beside the daemon binary in its release archive.
 * The embedded server is one bundled file with no node_modules, so offline
 * plugin installs load the binding the daemon install already delivered for
 * this platform. Returns undefined when the install carries none; the SDK
 * then reports the missing binding as a typed transport failure on first use.
 */
export function addonBesideBinary(binary: string, env: NodeJS.ProcessEnv): string | undefined {
  const executable = resolveExecutable(binary, env);
  if (executable === undefined) return undefined;
  const candidate = path.join(path.dirname(realpathSync(executable)), ADDON);
  return existsSync(candidate) ? candidate : undefined;
}

function resolveExecutable(binary: string, env: NodeJS.ProcessEnv): string | undefined {
  if (binary.includes("/") || binary.includes("\\")) return existsSync(binary) ? binary : undefined;
  const extensions = process.platform === "win32" ? (env.PATHEXT ?? ".EXE").split(";") : [""];
  for (const directory of (env.PATH ?? "").split(path.delimiter)) {
    if (directory === "") continue;
    for (const extension of extensions) {
      const candidate = path.join(directory, binary + extension);
      if (existsSync(candidate)) return candidate;
    }
  }
  return undefined;
}
