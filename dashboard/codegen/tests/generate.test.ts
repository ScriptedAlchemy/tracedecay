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
      "const ActorIdSchema",
      "const CancellationObservationSchema",
      "const DashboardAuthorizationV1Schema",
      "const DashboardCoverageV1Schema",
      "const DashboardDomainStateV1Schema",
      "interface DashboardEnvelopeV1",
      "const DashboardFreshnessV1Schema",
      "const DashboardLegalActionKindV1Schema",
      "const DashboardLegalActionRefV1Schema",
      "const DashboardScopeV1Schema",
      "const DashboardTimeV1Schema",
      "const DashboardVersionV1Schema",
      "const DashboardWatermarkV1Schema",
      "const DeadlineSchema",
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
    expect(generated).toContain("export type DashboardDomainStateV1 =");
    expect(generated).toContain("export const DashboardDomainStateV1Schema");
    // `unsupported` (server-emitted backend-gap state) and `unsupported_schema`
    // (undecodable schema) are BOTH present and distinct.
    expect(generated).toMatch(/"unsupported"/);
    expect(generated).toMatch(/"unsupported_schema"/);
    const schema = bundles[0]?.$defs?.DashboardDomainStateV1;
    const values = (schema?.oneOf ?? []).flatMap((part) => [
      ...(part.enum ?? []),
      ...(part.const === undefined ? [] : [part.const]),
    ]);
    expect(values).toHaveLength(17);
    expect(values).toContain("unsupported");
    expect(values).toContain("unsupported_schema");
  });

  it("emits a decoder factory for the generic DashboardEnvelope<T>", () => {
    const { files } = generateContracts(bundles);
    const generated = files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).toContain("export interface DashboardEnvelopeV1<TPayload>");
    expect(generated).toContain("export function DashboardEnvelopeV1Schema<TPayload>(");
    expect(generated).toContain("payload: payloadSchema,");
    expect(generated).not.toMatch(/DashboardEnvelopeV1\d+Schema/);
    // The exact scope + authorization shapes from read_model.rs are carried.
    expect(generated).toContain("store_root");
    expect(generated).toContain("outcome");
  });

  it("emits only Rust-owned contract names", () => {
    const { files } = generateContracts(bundles);
    const generated = files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).not.toContain("export const DashboardEnvelopeV1Schema =");
    expect(generated).not.toContain("export type DashboardEnvelopeV1");
    expect(generated).not.toContain("export const AnalyticsOverviewPayloadSchema =");
    expect(generated).not.toContain("export type AnalyticsOverviewPayload =");
    expect(generated).not.toContain("export const DoctorEffectReceiptSchema =");
  });

  it("emits the live index that re-exports the generated contract", () => {
    const { files } = generateContracts(bundles);
    expect(files[OUTPUT_FILES.INDEX_FILE]!).toContain('export * from "./generated";');
  });

  it("maps a synthetic tagged union without inventing variants", () => {
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
    expect(generated).toContain('z.discriminatedUnion("kind"');
    expect(generated).not.toContain('kind: "unsupported_schema";');
    expect(generated).toContain('WIRE_SCHEMA_REVISION = "test.1"');
  });
});
