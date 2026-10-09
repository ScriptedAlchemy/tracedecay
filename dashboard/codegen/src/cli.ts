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
import { basename, dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { checkContractTypes } from "./typecheck.ts";
import { generateContracts, OUTPUT_FILES, type JsonSchema } from "./generate.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const DASHBOARD_ROOT = resolve(HERE, "..", "..");
const REPOSITORY_ROOT = resolve(DASHBOARD_ROOT, "..");
const RUST_SCHEMA_FILE = "codegen/schemas/dashboard-contracts.schema.json";
const SDK_SOURCE_DIR = "sdks/typescript/src";
const SDK_RUST_OPERATIONS_FILE = "crates/tracedecay-sdk/src/operations.rs";

function bazelRun(target: string, args: string[]): void {
  const command = ["run", target, "--", ...args];
  const result = spawnSync("bazel", command, { cwd: REPOSITORY_ROOT, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`bazel ${command.join(" ")} failed with status ${result.status ?? "unknown"}`);
  }
}

function exportRustBundle(): { bundle: JsonSchema; source: string } {
  const temporaryDirectory = mkdtempSync(join(tmpdir(), "tracedecay-dashboard-contracts-"));
  const output = join(temporaryDirectory, "dashboard.schema.json");
  try {
    bazelRun("//sdks/codegen:dashboard_schema", [output]);
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
    bazelRun("//sdks/codegen:generate", [temporaryRoot]);
    return {
      ...Object.fromEntries(
        readdirSync(join(temporaryRoot, SDK_SOURCE_DIR)).map((name) => [
          `${SDK_SOURCE_DIR}/${name}`,
          readFileSync(join(temporaryRoot, SDK_SOURCE_DIR, name), "utf8"),
        ]),
      ),
      [SDK_RUST_OPERATIONS_FILE]: readFileSync(
        join(temporaryRoot, SDK_RUST_OPERATIONS_FILE),
        "utf8",
      ),
    };
  } finally {
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
}

/** Validate decoder implementations once at generation, outside dashboard checks. */
function checkDecoderTypes(files: Record<string, string>): void {
  // Keep staging beneath dashboard so zod resolves from the same installation.
  const directory = mkdtempSync(join(DASHBOARD_ROOT, ".contract-check-"));
  try {
    for (const file of Object.values(OUTPUT_FILES)) {
      const source = files[file];
      if (source === undefined) throw new Error(`missing generated contract: ${file}`);
      writeFileSync(join(directory, basename(file)), source.replace("// @ts-nocheck\n", ""));
    }
    checkContractTypes(directory, ["decoders.ts", "generated.ts"]);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

/** Every generated output, keyed by its repository-relative path. */
function generatedOutputs(): Record<string, string> {
  const exported = exportRustBundle();
  const { files } = generateContracts([exported.bundle]);
  checkDecoderTypes(files);
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
