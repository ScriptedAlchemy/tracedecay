/** Generate/check the live frontend contract from fresh Rust schemars output. */
import {
  readFileSync,
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

function exportRustBundle(): { bundle: JsonSchema; source: string } {
  const temporaryDirectory = mkdtempSync(join(tmpdir(), "tracedecay-dashboard-contracts-"));
  const output = join(temporaryDirectory, "dashboard.schema.json");
  try {
    const result = spawnSync(
      "cargo",
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
      {
        cwd: REPOSITORY_ROOT,
        env: {
          ...process.env,
          TRACEDECAY_SKIP_DASHBOARD_BUILD: "1",
          [SCHEMA_OUTPUT_ENV]: output,
        },
        stdio: "inherit",
      },
    );
    if (result.error) throw result.error;
    if (result.status !== 0) {
      throw new Error(`Rust contract schema export failed with status ${result.status ?? "unknown"}`);
    }
    const source = readFileSync(output, "utf8");
    return { bundle: JSON.parse(source) as JsonSchema, source };
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true });
  }
}

function run(): number {
  const mode = process.argv.includes("--check") ? "check" : "generate";
  const exported = exportRustBundle();
  const { files } = generateContracts([exported.bundle]);
  files[RUST_SCHEMA_FILE] = exported.source;

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
