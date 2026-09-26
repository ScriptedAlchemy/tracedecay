/**
 * Generate/check every checked-in contract output from fresh Rust output: the
 * dashboard contracts (schemars) and the TypeScript SDK sources (the canonical
 * operation registry).
 */
import {
  readFileSync,
  readdirSync,
  writeFileSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  existsSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { generateContracts, type JsonSchema } from "./generate.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const DASHBOARD_ROOT = resolve(HERE, "..", "..");
const REPOSITORY_ROOT = resolve(DASHBOARD_ROOT, "..");
const SCHEMA_OUTPUT_ENV = "TRACEDECAY_DASHBOARD_CONTRACT_SCHEMA_OUT";
const RUST_SCHEMA_FILE = "codegen/schemas/dashboard-contracts.schema.json";
const SDK_SOURCE_DIR = "sdks/typescript/src";

function cargo(args: string[], cwd: string, env: NodeJS.ProcessEnv = process.env): void {
  const result = spawnSync("cargo", args, { cwd, env, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`cargo ${args.join(" ")} failed with status ${result.status ?? "unknown"}`);
  }
}

function exportRustBundle(): { bundle: JsonSchema; source: string } {
  const temporaryDirectory = mkdtempSync(join(tmpdir(), "tracedecay-dashboard-contracts-"));
  const output = join(temporaryDirectory, "dashboard.schema.json");
  try {
    cargo(
      [
        "test",
        "--quiet",
        "-p",
        "tracedecay-dashboard-api",
        "--lib",
        "contract_schema::tests::writes_dashboard_contract_schema",
        "--",
        "--ignored",
        "--exact",
      ],
      REPOSITORY_ROOT,
      { ...process.env, [SCHEMA_OUTPUT_ENV]: output },
    );
    const source = readFileSync(output, "utf8");
    return { bundle: JSON.parse(source) as JsonSchema, source };
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true });
  }
}

/** Runs the SDK generator against a scratch root and returns what it wrote. */
function exportSdkSources(): Record<string, string> {
  const temporaryRoot = mkdtempSync(join(tmpdir(), "tracedecay-sdk-contracts-"));
  try {
    // Cargo reads .cargo/config.toml from its working directory, and the SDK
    // codegen workspace has its own pnpm-vendored sources and lockfile.
    cargo(
      ["run", "--quiet", "--locked", "--bin", "generate", "--", temporaryRoot],
      join(REPOSITORY_ROOT, "sdks", "codegen"),
    );
    return Object.fromEntries(
      readdirSync(join(temporaryRoot, SDK_SOURCE_DIR)).map((name) => [
        `${SDK_SOURCE_DIR}/${name}`,
        readFileSync(join(temporaryRoot, SDK_SOURCE_DIR, name), "utf8"),
      ]),
    );
  } finally {
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
}

/** Every generated output, keyed by its repository-relative path. */
function generatedOutputs(): Record<string, string> {
  const exported = exportRustBundle();
  const { files } = generateContracts([exported.bundle]);
  files[RUST_SCHEMA_FILE] = exported.source;
  const outputs: Record<string, string> = {};
  for (const [rel, content] of Object.entries(files)) outputs[`dashboard/${rel}`] = content;
  return { ...outputs, ...exportSdkSources() };
}

function run(): number {
  const mode = process.argv.includes("--check") ? "check" : "generate";
  const files = generatedOutputs();

  if (mode === "check") {
    let stale = false;
    for (const [rel, content] of Object.entries(files)) {
      const abs = join(REPOSITORY_ROOT, rel);
      const current = existsSync(abs) ? readFileSync(abs, "utf8") : null;
      if (current !== content) {
        stale = true;
        process.stderr.write(`stale contracts output: ${rel}\n`);
      }
    }
    if (stale) {
      process.stderr.write("Run `pnpm --dir dashboard run contracts:generate` and commit the result.\n");
      return 1;
    }
    process.stdout.write("contracts up to date\n");
    return 0;
  }

  for (const [rel, content] of Object.entries(files)) {
    const abs = join(REPOSITORY_ROOT, rel);
    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, content);
    process.stdout.write(`wrote ${rel}\n`);
  }
  return 0;
}

process.exit(run());
