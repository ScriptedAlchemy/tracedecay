import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { z, type ZodTypeAny } from "zod";
import { generateContracts, type JsonSchema, OUTPUT_FILES } from "../src/generate.ts";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const SCHEMA_DIR = resolve(HERE, "..", "schemas");

function loadBundles(): JsonSchema[] {
  return readdirSync(SCHEMA_DIR)
    .filter((f) => f.endsWith(".schema.json"))
    .sort()
    .map((f) => JSON.parse(readFileSync(join(SCHEMA_DIR, f), "utf8")) as JsonSchema);
}

function emittedPropertyDecoder(generated: string, property: string): ZodTypeAny {
  const match = generated.match(new RegExp(`^  ${property}: (.+),$`, "m"));
  if (!match?.[1]) {
    throw new Error(`generated decoder is missing property ${property}`);
  }
  const build = new Function("z", `return (${match[1]});`) as (zod: typeof z) => ZodTypeAny;
  return build(z);
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
      "const AnalyticsAgentsPayloadV1Schema",
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
      "const DeliveryCiTimelineV1Schema",
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

  it("recognizes monomorphized envelopes by structure rather than generated name", () => {
    const bundle = structuredClone(bundles[0]!);
    const defs = bundle.$defs!;
    defs.FeedbackEnvelopeInstantiation = defs.DashboardEnvelopeV12!;
    delete defs.DashboardEnvelopeV12;

    const generated = generateContracts([bundle]).files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).not.toContain("FeedbackEnvelopeInstantiationSchema");
  });

  it("does not hide a distinct Rust contract that shares the envelope name prefix", () => {
    const bundle = structuredClone(bundles[0]!);
    bundle.$defs!.DashboardEnvelopeV1Metadata = {
      type: "object",
      properties: { description: { type: "string" } },
      required: ["description"],
    };

    const generated = generateContracts([bundle]).files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).toContain("export const DashboardEnvelopeV1MetadataSchema");
  });

  it("rejects an emitted contract that still references an omitted envelope instance", () => {
    const bundle = structuredClone(bundles[0]!);
    bundle.$defs!.EnvelopeConsumer = {
      $ref: "#/$defs/DashboardEnvelopeV12",
    };

    expect(() => generateContracts([bundle])).toThrow(
      "EnvelopeConsumer references omitted generated definition DashboardEnvelopeV12",
    );
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

  it("preserves closed-object admission while leaving open objects extensible", () => {
    const bundle: JsonSchema = {
      schemaRevision: "test.1",
      $defs: {
        ClosedReading: {
          type: "object",
          properties: { status: { type: "string" } },
          required: ["status"],
          additionalProperties: false,
        },
        OpenReading: {
          type: "object",
          properties: { status: { type: "string" } },
          required: ["status"],
        },
      },
    };

    const generated = generateContracts([bundle]).files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).toContain(
      "export const ClosedReadingSchema = z.object({\n  status: z.string(),\n}).strict();",
    );
    expect(generated).toContain(
      "export const OpenReadingSchema = z.object({\n  status: z.string(),\n});",
    );
  });

  it("emits declared integer bounds without constraining unbounded integers", () => {
    const bundle = {
      schemaRevision: "test.1",
      $defs: {
        IntegerBounds: {
          type: "object",
          properties: {
            bounded: { type: "integer", minimum: 0, maximum: 10 },
            lower_only: { type: "integer", minimum: 1 },
            unbounded: { type: "integer" },
            upper_only: { type: "integer", maximum: 99 },
          },
          required: ["bounded", "lower_only", "unbounded", "upper_only"],
        },
      },
    } as JsonSchema;

    const generated = generateContracts([bundle]).files[OUTPUT_FILES.GENERATED_FILE]!;
    expect(generated).toContain("bounded: z.number().int().min(0).max(10),");
    expect(generated).toContain("lower_only: z.number().int().min(1),");
    expect(generated).toContain("unbounded: z.number().int(),");
    expect(generated).toContain("upper_only: z.number().int().max(99),");
  });

  it("preserves exclusive numeric bounds as strict decoder limits", () => {
    const bundle = {
      schemaRevision: "test.1",
      $defs: {
        ExclusiveBounds: {
          type: "object",
          properties: {
            integer: { type: "integer", exclusiveMinimum: 0, exclusiveMaximum: 10 },
            number: { type: "number", exclusiveMinimum: -1.5, exclusiveMaximum: 1.5 },
          },
          required: ["integer", "number"],
        },
      },
    } as JsonSchema;

    const generated = generateContracts([bundle]).files[OUTPUT_FILES.GENERATED_FILE]!;
    const integer = emittedPropertyDecoder(generated, "integer");
    const number = emittedPropertyDecoder(generated, "number");

    expect(generated).toContain("integer: z.number().int().gt(0).lt(10),");
    expect(generated).toContain("number: z.number().gt(-1.5).lt(1.5),");
    expect(integer.safeParse(0).success).toBe(false);
    expect(integer.safeParse(1).success).toBe(true);
    expect(integer.safeParse(9).success).toBe(true);
    expect(integer.safeParse(10).success).toBe(false);
    expect(number.safeParse(-1.5).success).toBe(false);
    expect(number.safeParse(0).success).toBe(true);
    expect(number.safeParse(1.5).success).toBe(false);
  });

  it("enforces safe JavaScript integers for wide Rust formats only", () => {
    const bundle = {
      schemaRevision: "test.1",
      $defs: {
        IntegerFormats: {
          type: "object",
          properties: {
            nullable_optional: { type: ["integer", "null"], format: "int64" },
            plain: { type: "integer" },
            platform: { type: "integer", format: "uint", minimum: 1, maximum: 100 },
            signed: { type: "integer", format: "int64" },
            uint32: { type: "integer", format: "uint32", minimum: 0, maximum: 4_294_967_295 },
            unsigned: { type: "integer", format: "uint64", minimum: 0 },
          },
          required: ["plain", "platform", "signed", "uint32", "unsigned"],
        },
      },
    } as JsonSchema;

    const generated = generateContracts([bundle]).files[OUTPUT_FILES.GENERATED_FILE]!;
    const nullableOptional = emittedPropertyDecoder(generated, "nullable_optional");
    const plain = emittedPropertyDecoder(generated, "plain");
    const platform = emittedPropertyDecoder(generated, "platform");
    const signed = emittedPropertyDecoder(generated, "signed");
    const uint32 = emittedPropertyDecoder(generated, "uint32");
    const unsigned = emittedPropertyDecoder(generated, "unsigned");
    const unsafeInteger = 9_007_199_254_740_992;

    expect(generated).toContain("signed: z.number().int().safe(),");
    expect(generated).toContain("unsigned: z.number().int().safe().min(0),");
    expect(generated).toContain("platform: z.number().int().safe().min(1).max(100),");
    expect(generated).toContain(
      "nullable_optional: z.number().int().safe().nullable().optional(),",
    );
    expect(generated).toContain("uint32: z.number().int().min(0).max(4294967295),");
    expect(generated).toContain("plain: z.number().int(),");

    expect(signed.safeParse(Number.MAX_SAFE_INTEGER).success).toBe(true);
    expect(signed.safeParse(Number.MIN_SAFE_INTEGER).success).toBe(true);
    expect(signed.safeParse(unsafeInteger).success).toBe(false);
    expect(signed.safeParse(-unsafeInteger).success).toBe(false);

    expect(unsigned.safeParse(Number.MAX_SAFE_INTEGER).success).toBe(true);
    expect(unsigned.safeParse(-1).success).toBe(false);
    expect(unsigned.safeParse(unsafeInteger).success).toBe(false);

    expect(platform.safeParse(1).success).toBe(true);
    expect(platform.safeParse(100).success).toBe(true);
    expect(platform.safeParse(0).success).toBe(false);
    expect(platform.safeParse(101).success).toBe(false);

    expect(nullableOptional.safeParse(undefined).success).toBe(true);
    expect(nullableOptional.safeParse(null).success).toBe(true);
    expect(nullableOptional.safeParse(unsafeInteger).success).toBe(false);

    expect(uint32.safeParse(4_294_967_295).success).toBe(true);
    expect(plain.safeParse(unsafeInteger).success).toBe(true);
  });
});
