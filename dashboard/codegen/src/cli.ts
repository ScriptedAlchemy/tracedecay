/**
 * Contracts codegen CLI (transitional preview harness).
 *
 *   tsx codegen/src/cli.ts generate   # write the preview under codegen/.preview/
 *   tsx codegen/src/cli.ts --check    # fail (exit 1) if the preview is stale
 *
 * The live wire boundary (`src/contracts/generated.ts`) is hand-maintained
 * against `src/dashboard/read_model.rs` today; this CLI emits a preview so the
 * generated shape can be diffed without overwriting the hand-maintained file.
 * `--check` regenerates in memory and compares byte-for-byte against the
 * preview, so a schema change that was not regenerated is caught. When the real
 * schemars export lands, repoint `OUTPUT_FILES` back at `src/contracts/*`.
 */
import { readFileSync, writeFileSync, mkdirSync, existsSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { generateContracts, type JsonSchema } from "./generate.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const DASHBOARD_ROOT = resolve(HERE, "..", "..");
const SCHEMA_DIR = resolve(HERE, "..", "schemas");

function loadBundles(): JsonSchema[] {
  const files = readdirSync(SCHEMA_DIR)
    .filter((f) => f.endsWith(".schema.json"))
    .sort();
  return files.map((f) => JSON.parse(readFileSync(join(SCHEMA_DIR, f), "utf8")) as JsonSchema);
}

function run(): number {
  const mode = process.argv.includes("--check") ? "check" : "generate";
  const bundles = loadBundles();
  const { files } = generateContracts(bundles);

  if (mode === "check") {
    let stale = false;
    for (const [rel, content] of Object.entries(files)) {
      const abs = join(DASHBOARD_ROOT, rel);
      const current = existsSync(abs) ? readFileSync(abs, "utf8") : null;
      if (current !== content) {
        stale = true;
        process.stderr.write(`stale contracts output: ${rel}\n`);
      }
    }
    if (stale) {
      process.stderr.write("Run `npm run contracts:generate` and commit the result.\n");
      return 1;
    }
    process.stdout.write("contracts up to date\n");
    return 0;
  }

  for (const [rel, content] of Object.entries(files)) {
    const abs = join(DASHBOARD_ROOT, rel);
    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, content);
    process.stdout.write(`wrote ${rel}\n`);
  }
  return 0;
}

process.exit(run());
