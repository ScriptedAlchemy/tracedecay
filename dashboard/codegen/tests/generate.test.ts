import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { generateContracts, type JsonSchema, OUTPUT_FILES } from "../src/generate.ts";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const SCHEMA_DIR = resolve(HERE, "..", "schemas");
const DASHBOARD_ROOT = resolve(HERE, "..", "..");

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
      "interface LegalActionRef",
      "interface Scope",
      "interface Versions",
    ].map((needle) => generated.indexOf(needle));
    expect(order.every((i) => i >= 0)).toBe(true);
    const sorted = [...order].sort((a, b) => a - b);
    expect(order).toEqual(sorted);
  });

  it("emits an assertNever exhaustiveness helper", () => {
    const { files } = generateContracts(bundles);
    expect(files[OUTPUT_FILES.GENERATED_FILE]!).toContain("export function assertNever(value: never): never");
  });

  it("emits the discriminated domain union with a synthesized unsupported_schema branch", () => {
    const { files } = generateContracts(bundles);
    const generated = files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).toContain('kind: "unsupported_schema";');
    // 15 authored variants are present; unsupported_schema is NOT authored in the schema.
    const schema = bundles.find((b) => (b.$defs ?? {}).DashboardDomainState)!;
    const authored = (schema.$defs?.DashboardDomainState?.oneOf ?? []).length;
    expect(authored).toBe(15);
  });

  it("emits a decoder factory for the generic DashboardEnvelope<T>", () => {
    const { files } = generateContracts(bundles);
    const generated = files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).toContain("export interface DashboardEnvelope<TPayload>");
    expect(generated).toContain("export function DashboardEnvelopeSchema<TPayload>(");
    expect(generated).toContain("payload: payloadSchema,");
  });

  it("matches the committed on-disk output (check-mode parity)", () => {
    const { files } = generateContracts(bundles);
    for (const [rel, content] of Object.entries(files)) {
      const onDisk = readFileSync(join(DASHBOARD_ROOT, rel), "utf8");
      expect(onDisk, `${rel} is stale; run npm run contracts:generate`).toBe(content);
    }
  });

  it("maps a minimal synthetic tagged-union bundle to the unsupported fallback shape", () => {
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
