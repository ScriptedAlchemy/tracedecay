import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { generateContracts, type JsonSchema, OUTPUT_FILES } from "../src/generate.ts";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const SCHEMA_DIR = resolve(HERE, "..", "schemas");

function loadBundles(): JsonSchema[] {
  return readdirSync(SCHEMA_DIR)
    .filter((f) => f.endsWith(".schema.json"))
    .sort()
    .map((f) => JSON.parse(readFileSync(join(SCHEMA_DIR, f), "utf8")) as JsonSchema);
}

describe("contracts generator", () => {
  const bundles = loadBundles();

  it("is deterministic: identical bundles produce byte-identical output", () => {
    const a = generateContracts(bundles);
    const b = generateContracts(bundles);
    expect(a.files).toEqual(b.files);
  });

  it("owns the live contract boundary instead of an unused preview", () => {
    expect(OUTPUT_FILES.GENERATED_FILE).toBe("src/contracts/generated.ts");
    expect(OUTPUT_FILES.INDEX_FILE).toBe("src/contracts/index.ts");
  });

  it("emits no timestamps or host/env state (reviewable diffs)", () => {
    const { files } = generateContracts(bundles);
    const generated = files[OUTPUT_FILES.GENERATED_FILE]!;
    // No ISO timestamps, epoch millis, or absolute machine paths.
    expect(generated).not.toMatch(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/);
    expect(generated).not.toMatch(/\/fast\/|\/home\/|\/Users\//);
    expect(generated).not.toMatch(/Date\.now|new Date/);
  });

  it("sorts named defs alphabetically (stable ordering)", () => {
    const { files } = generateContracts(bundles);
    const generated = files[OUTPUT_FILES.GENERATED_FILE]!;
    const order = [
      "interface Authorization",
      "interface Coverage",
      "type DashboardDomainState",
      "interface DashboardEnvelope",
      "interface FindingPayload",
      "interface Freshness",
      "type LegalActionKind",
      "interface LegalActionRef",
      "interface Scope",
      "interface Time",
      "interface Version",
      "interface Watermark",
    ].map((needle) => generated.indexOf(needle));
    expect(order.every((i) => i >= 0)).toBe(true);
    const sorted = [...order].sort((a, b) => a - b);
    expect(order).toEqual(sorted);
  });

  it("emits an assertNever exhaustiveness helper", () => {
    const { files } = generateContracts(bundles);
    expect(files[OUTPUT_FILES.GENERATED_FILE]!).toContain(
      "export function assertNever(value: never): never",
    );
  });

  it("emits the closed 17-value domain-state string enum (read_model.rs parity)", () => {
    const { files } = generateContracts(bundles);
    const generated = files[OUTPUT_FILES.GENERATED_FILE]!;
    // Flat string enum, not a `{ kind }` tagged union.
    expect(generated).toContain("export type DashboardDomainState =");
    expect(generated).toContain("export const DashboardDomainStateSchema");
    // `unsupported` (server-emitted backend-gap state) and `unsupported_schema`
    // (undecodable schema) are BOTH present and distinct.
    expect(generated).toMatch(/"unsupported"/);
    expect(generated).toMatch(/"unsupported_schema"/);
    const schema = bundles.find((b) => (b.$defs ?? {}).DashboardDomainState)!;
    const values = schema.$defs?.DashboardDomainState?.enum ?? [];
    expect(values).toHaveLength(17);
    expect(values).toContain("unsupported");
    expect(values).toContain("unsupported_schema");
  });

  it("emits a decoder factory for the generic DashboardEnvelope<T>", () => {
    const { files } = generateContracts(bundles);
    const generated = files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).toContain("export interface DashboardEnvelope<TPayload>");
    expect(generated).toContain("export function DashboardEnvelopeSchema<TPayload>(");
    expect(generated).toContain("payload: payloadSchema,");
    // The exact scope + authorization shapes from read_model.rs are carried.
    expect(generated).toContain("store_root");
    expect(generated).toContain("outcome");
  });

  it("emits a preview index that re-exports the generated preview module", () => {
    const { files } = generateContracts(bundles);
    expect(files[OUTPUT_FILES.INDEX_FILE]!).toContain('export * from "./contracts.generated";');
  });

  it("maps a minimal synthetic tagged-union bundle to the unsupported fallback shape", () => {
    // The generator still supports internally-tagged `oneOf` unions with a
    // synthesized `unsupported_schema` branch (exercised here even though the
    // read_model.rs domain state is now a flat string enum).
    const bundle: JsonSchema = {
      schemaRevision: "test.1",
      $defs: {
        Signal: {
          oneOf: [
            {
              type: "object",
              properties: { kind: { type: "string", enum: ["up"] } },
              required: ["kind"],
            },
            {
              type: "object",
              properties: { kind: { type: "string", enum: ["down"] }, by: { type: "integer" } },
              required: ["kind", "by"],
            },
          ],
        },
      },
    };
    const { files } = generateContracts([bundle]);
    const generated = files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).toContain("export type Signal =");
    expect(generated).toContain('kind: "unsupported_schema";');
    expect(generated).toContain('SCHEMA_REVISION = "test.1"');
  });
});
