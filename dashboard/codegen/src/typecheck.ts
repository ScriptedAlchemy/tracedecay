import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);
const compiler = join(dirname(require.resolve("typescript/package.json")), "bin", "tsc");

export function checkContractTypes(directory: string, files: string[]): void {
  const config = join(directory, "tsconfig.json");
  writeFileSync(config, JSON.stringify({
    compilerOptions: {
      strict: true,
      noEmit: true,
      skipLibCheck: true,
      target: "ES2022",
      module: "ESNext",
      moduleResolution: "Bundler",
      allowImportingTsExtensions: true,
      exactOptionalPropertyTypes: true,
      lib: ["ES2022"],
      types: [],
    },
    files,
  }));
  const result = spawnSync(process.execPath, [compiler, "--project", config, "--pretty", "false"], {
    cwd: directory,
    encoding: "utf8",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`Contract typecheck failed (${result.status ?? result.signal}):\n${result.stdout}${result.stderr}`);
  }
}
