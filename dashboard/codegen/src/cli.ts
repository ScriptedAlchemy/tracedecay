/**
 * Contracts codegen CLI.
 *
 *   tsx codegen/src/cli.ts generate   # write dashboard/src/contracts/*
 *   tsx codegen/src/cli.ts --check    # fail (exit 1) if on-disk output is stale
 *
 * `--check` is the reviewable-diff gate: it regenerates in memory and compares
 * byte-for-byte against the committed files, so a schema change that was not
 * regenerated cannot merge silently.
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
