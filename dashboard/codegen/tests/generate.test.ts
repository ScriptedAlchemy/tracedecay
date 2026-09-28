import { describe, it, expect } from "vitest";
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { z, type ZodTypeAny } from "zod";
import ts from "typescript";
import { generateContracts, type JsonSchema, OUTPUT_FILES } from "../src/generate.ts";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const SCHEMA_DIR = resolve(HERE, "..", "schemas");

function loadBundles(): JsonSchema[] {
  return readdirSync(SCHEMA_DIR)
    .filter((f) => f.endsWith(".schema.json"))
    .sort()
    .map((f) => JSON.parse(readFileSync(join(SCHEMA_DIR, f), "utf8")) as JsonSchema);
}

function contractText(files: Record<string, string>): string {
  return [
    files[OUTPUT_FILES.TYPES_FILE],
    files[OUTPUT_FILES.DECODERS_FILE],
    files[OUTPUT_FILES.GENERATED_FILE],
  ].join("\n");
}

function diagnosticText(diagnostics: readonly ts.Diagnostic[]): string[] {
  return diagnostics.map((diagnostic) =>
    ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n"),
  );
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

  it("emits no timestamps or host/env state (reviewable diffs)", () => {
    const { files } = generateContracts(bundles);
    const generated = contractText(files);
    // No ISO timestamps, epoch millis, or absolute machine paths.
    expect(generated).not.toMatch(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/);
    expect(generated).not.toMatch(/\/fast\/|\/home\/|\/Users\//);
    expect(generated).not.toMatch(/Date\.now|new Date/);
  });

  it("sorts named defs alphabetically (stable ordering)", () => {
    const { files } = generateContracts(bundles);
    const types = files[OUTPUT_FILES.TYPES_FILE]!;
    const decoders = files[OUTPUT_FILES.DECODERS_FILE]!;
    const typeOrder = [
      "export type ActorId ",
      "export type AnalyticsAgentsPayloadV1 ",
      "export type DashboardAuthorizationV1 ",
      "export type DashboardCoverageV1 ",
      "export type DashboardDomainStateV1 ",
      "export interface DashboardEnvelopeV1<",
      "export type DashboardFreshnessV1 ",
      "export type DashboardLegalActionKindV1 ",
      "export type DashboardLegalActionRefV1 ",
      "export type DashboardScopeV1 ",
      "export type DashboardTimeV1 ",
      "export type DashboardVersionV1 ",
      "export type DashboardWatermarkV1 ",
      "export type DeliveryCiTimelineV1 ",
    ].map((needle) => types.indexOf(needle));
    const decoderOrder = [
      "const ActorIdSchema",
      "const AnalyticsAgentsPayloadV1Schema",
      "const DashboardAuthorizationV1Schema",
      "const DashboardCoverageV1Schema",
      "const DashboardDomainStateV1Schema",
      "const DashboardFreshnessV1Schema",
      "const DashboardLegalActionKindV1Schema",
      "const DashboardLegalActionRefV1Schema",
      "const DashboardScopeV1Schema",
      "const DashboardTimeV1Schema",
      "const DashboardVersionV1Schema",
      "const DashboardWatermarkV1Schema",
      "const DeliveryCiTimelineV1Schema",
    ].map((needle) => decoders.indexOf(needle));
    for (const order of [typeOrder, decoderOrder]) {
      expect(order.every((i) => i >= 0)).toBe(true);
      expect(order).toEqual([...order].sort((a, b) => a - b));
    }
  });

  it("emits an assertNever exhaustiveness helper", () => {
    const { files } = generateContracts(bundles);
    expect(contractText(files)).toContain(
      "export function assertNever(value: never): never",
    );
  });

  it("emits the closed 17-value domain-state string enum (read_model.rs parity)", () => {
    const { files } = generateContracts(bundles);
    const generated = contractText(files);
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
    const generated = contractText(files);
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

    const generated = contractText(generateContracts([bundle]).files);
    expect(generated).not.toContain("FeedbackEnvelopeInstantiationSchema");
  });

  it("does not hide a distinct Rust contract that shares the envelope name prefix", () => {
    const bundle = structuredClone(bundles[0]!);
    bundle.$defs!.DashboardEnvelopeV1Metadata = {
      type: "object",
      properties: { description: { type: "string" } },
      required: ["description"],
    };

    const generated = contractText(generateContracts([bundle]).files);
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
    const generated = contractText(files);
    expect(generated).not.toContain("export const DashboardEnvelopeV1Schema =");
    expect(generated).not.toContain("export type DashboardEnvelopeV1");
    expect(generated).not.toContain("export const AnalyticsOverviewPayloadSchema =");
    expect(generated).not.toContain("export type AnalyticsOverviewPayload =");
    expect(generated).not.toContain("export const DoctorEffectReceiptSchema =");
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
    const generated = contractText(files);
    expect(generated).toContain('z.discriminatedUnion("kind"');
    expect(generated).not.toContain('kind: "unsupported_schema";');
    expect(generated).toContain('WIRE_SCHEMA_REVISION = "test.1"');
  });

  it("typechecks consumer code and accepts only decoder-valid fixtures", async () => {
    const bundle: JsonSchema = {
      schemaRevision: "test.1",
      $defs: {
        Node: {
          type: "object",
          additionalProperties: false,
          properties: {
            id: { type: "string" },
            child: { $ref: "#/$defs/Node" },
            label: { type: ["string", "null"] },
            metadata: {},
          },
          required: ["id", "label", "metadata"],
        },
        ClosedReading: {
          type: "object",
          additionalProperties: false,
          properties: { status: { type: "string" } },
          required: ["status"],
        },
        OpenReading: {
          type: "object",
          properties: { status: { type: "string" } },
          required: ["status"],
        },
        DashboardDomainStateV1: { enum: ["ready", "unsupported_schema"] },
        Status: { enum: ["ready", "pending"] },
        Choice: { oneOf: [{ const: "yes" }, { const: "no" }] },
        Result: {
          oneOf: [
            {
              type: "object",
              properties: { kind: { const: "ok" }, node: { $ref: "#/$defs/Node" } },
              required: ["kind", "node"],
            },
            {
              type: "object",
              properties: { kind: { const: "missing" } },
              required: ["kind"],
            },
          ],
        },
      },
    };
    const files = generateContracts([bundle]).files;
    const directory = mkdtempSync(join(HERE, ".contract-fixture-"));
    const typesPath = join(directory, "types.ts");
    const decodersPath = join(directory, "decoders.ts");
    const consumerPath = join(directory, "consumer.ts");
    try {
      writeFileSync(typesPath, files[OUTPUT_FILES.TYPES_FILE]!);
      writeFileSync(join(directory, "generated.ts"), files[OUTPUT_FILES.GENERATED_FILE]!);
      // Production dashboard checks skip decoder expressions. This fixture
      // removes that skip so an annotation/initializer mismatch fails here.
      writeFileSync(
        decodersPath,
        files[OUTPUT_FILES.DECODERS_FILE]!.replace("// @ts-nocheck\n", ""),
      );
      writeFileSync(consumerPath, `
import { z } from "zod";
import {
  assertNever,
  ChoiceSchema,
  ClosedReadingSchema,
  DashboardDomainStateV1Schema,
  NodeSchema,
  OpenReadingSchema,
  ResultSchema,
  StatusSchema,
  type Choice,
  type ClosedReading,
  type Node,
  type OpenReading,
  type Result,
  type Status,
} from "./generated.ts";

const catchInput: z.input<typeof DashboardDomainStateV1Schema> = 42;
const valid: Node = { id: "root", label: null, child: { id: "leaf", label: "ok" } };
const decoded: Node = NodeSchema.parse(valid);
const inferred: z.infer<typeof NodeSchema> = valid;
// @ts-expect-error required nullable fields cannot be omitted
const missing: Node = { id: "root" };
// @ts-expect-error a recursive child must satisfy the same contract
const invalidChild: Node = { id: "root", label: null, child: { id: 42 } };
// @ts-expect-error the decoder output retains the literal enum
const invalidStatus: z.infer<typeof StatusSchema> = "other";
// @ts-expect-error a closed reading still requires its declared field
const closedMissing: ClosedReading = {};
const closed: ClosedReading = { status: "ok" };
const open: OpenReading = { status: "ok" };
const extended: Node & { extra: number } = NodeSchema.extend({ extra: z.number() }).parse({});
const field: string = NodeSchema.shape.id.parse("id");
const status: Status = StatusSchema.options[0];
const choice: Choice = ChoiceSchema.options[0].parse("yes");
function unwrap(result: Result): Node | undefined {
  switch (result.kind) {
    case "ok": return result.node;
    case "missing": return undefined;
    default: return assertNever(result);
  }
}
export { catchInput, decoded, inferred, missing, invalidChild, invalidStatus, closedMissing, closed, open, extended, field, status, choice, unwrap };
`);
      const options: ts.CompilerOptions = {
        strict: true, noEmit: true, skipLibCheck: true,
        target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext,
        moduleResolution: ts.ModuleResolutionKind.Bundler,
        allowImportingTsExtensions: true,
        exactOptionalPropertyTypes: true,
      };
      const program = ts.createProgram([decodersPath, consumerPath], options);
      expect(diagnosticText(ts.getPreEmitDiagnostics(program))).toEqual([]);

      const javascript = ts.transpileModule(files[OUTPUT_FILES.DECODERS_FILE]!, {
        compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
      }).outputText;
      const runtimePath = join(directory, "runtime.js");
      writeFileSync(runtimePath, javascript);
      const runtime = await import(pathToFileURL(runtimePath).href) as Record<string, ZodTypeAny>;
      const node = {
        id: "root",
        label: null,
        metadata: { any: true },
        child: { id: "leaf", label: "ok", metadata: 1 },
      };
      expect(runtime.NodeSchema!.parse(node)).toEqual(node);
      expect(runtime.NodeSchema!.safeParse({ id: "root", metadata: 1 }).success).toBe(false);
      expect(runtime.NodeSchema!.safeParse({ ...node, extra: true }).success).toBe(false);
      expect(runtime.NodeSchema!.safeParse({
        ...node,
        child: { id: 42, label: "ok", metadata: 1 },
      }).success).toBe(false);
      expect(runtime.ClosedReadingSchema!.parse({ status: "ok" })).toEqual({ status: "ok" });
      expect(runtime.ClosedReadingSchema!.safeParse({ status: "ok", extra: 1 }).success).toBe(false);
      expect(runtime.OpenReadingSchema!.parse({ status: "ok", extra: 1 })).toEqual({ status: "ok" });
      expect(runtime.OpenReadingSchema!.safeParse({}).success).toBe(false);
      expect(runtime.StatusSchema!.parse("ready")).toBe("ready");
      expect(runtime.StatusSchema!.safeParse("other").success).toBe(false);
      expect(runtime.ChoiceSchema!.parse("yes")).toBe("yes");
      expect(runtime.ChoiceSchema!.safeParse("maybe").success).toBe(false);
      expect(runtime.ResultSchema!.parse({ kind: "missing" })).toEqual({ kind: "missing" });
      expect(runtime.ResultSchema!.safeParse({ kind: "other" }).success).toBe(false);
      expect(runtime.DashboardDomainStateV1Schema!.parse("ready")).toBe("ready");
      expect(runtime.DashboardDomainStateV1Schema!.parse(42)).toBe("unsupported_schema");
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
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

    const generated = contractText(generateContracts([bundle]).files);
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

    const generated = contractText(generateContracts([bundle]).files);
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

    const generated = contractText(generateContracts([bundle]).files);
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
