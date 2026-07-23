/**
 * Contracts codegen — deterministic TypeScript + Zod generation from
 * Rust-exported JSON Schemas.
 *
 * TRANSITIONAL: the live wire boundary (`src/contracts/generated.ts`) is
 * currently hand-maintained against the Rust serde model in
 * `src/dashboard/read_model.rs`, because the real schemars JSON Schema export
 * is not yet wired. This generator + its fixture
 * (`codegen/schemas/dashboard-contracts.schema.json`, kept in lockstep with
 * read_model.rs) are the harness that will own that file once the export
 * lands. Until then it emits to a preview path (see `OUTPUT_FILES`) so a stray
 * `contracts:generate` cannot clobber the hand-maintained boundary.
 *
 * Design goals (binding, per docs/plans/tracedecay-v2/11-dashboard-frontend.md
 * §"Typed presentation contracts"):
 *   - The emitted file exposes an `assertNever` helper so the closed unions
 *     (e.g. the 17-value domain-state enum) stay exhaustively switchable.
 *   - Unknown internally-tagged `oneOf` variants decode to a typed
 *     `unsupported_schema` branch instead of throwing; malformed *known*
 *     variants still produce a typed decode error (never a silent downgrade).
 *   - Generation is deterministic: stable (alphabetical) ordering, no
 *     timestamps, no host/env state — so diffs are reviewable.
 *
 * The generator implements the focused subset of JSON Schema that the exported
 * dashboard schemas use (objects, refs, string/number/bool/null scalars,
 * nullable scalars via `["t","null"]`, string enums, arrays, `anyOf` unions,
 * and internally-tagged `oneOf` unions), plus two `x-generic*` extension keys
 * used to carry the `DashboardEnvelope<T>` generic across the schema boundary
 * (schemars monomorphizes generics; the fixture marks the generic explicitly
 * so the emitted TS stays generic).
 */

export interface JsonSchema {
  $id?: string;
  $ref?: string;
  title?: string;
  description?: string;
  schemaRevision?: string;
  type?: string | string[];
  properties?: Record<string, JsonSchema>;
  required?: string[];
  items?: JsonSchema;
  enum?: unknown[];
  const?: unknown;
  oneOf?: JsonSchema[];
  anyOf?: JsonSchema[];
  $defs?: Record<string, JsonSchema>;
  definitions?: Record<string, JsonSchema>;
  ["x-generic"]?: string;
  ["x-generic-ref"]?: string;
}

export interface GeneratedContracts {
  /** Map of relative output path -> file content. */
  files: Record<string, string>;
}

// Preview output paths. The live boundary (`src/contracts/generated.ts`) is
// hand-maintained against read_model.rs today; this harness writes a preview
// alongside the codegen so it can be diffed without overwriting that file.
const GENERATED_FILE = "codegen/.preview/contracts.generated.ts";
const INDEX_FILE = "codegen/.preview/index.ts";

const HEADER = [
  "// GENERATED PREVIEW by dashboard/codegen — not the live wire boundary.",
  "// Source: dashboard/codegen/schemas/*.schema.json (kept in lockstep with",
  "// src/dashboard/read_model.rs). The live boundary at src/contracts/generated.ts",
  "// is hand-maintained until the real schemars export replaces this harness.",
  "// Deterministic output (stable ordering, no timestamps).",
].join("\n");

/** Return the `$defs`/`definitions` map from a bundle, sorted by key. */
function collectDefs(bundle: JsonSchema): Array<[string, JsonSchema]> {
  const defs = bundle.$defs ?? bundle.definitions ?? {};
  return Object.keys(defs)
    .sort()
    .map((name) => [name, defs[name] as JsonSchema]);
}

function refName(ref: string): string {
  const parts = ref.split("/");
  return parts[parts.length - 1] as string;
}

function quote(value: string): string {
  return JSON.stringify(value);
}

/** Discriminant detection for tagged `oneOf` unions. */
function discriminantOf(variant: JsonSchema): { key: string; value: string } | null {
  const props = variant.properties ?? {};
  for (const key of Object.keys(props).sort()) {
    const prop = props[key] as JsonSchema;
    if (Array.isArray(prop.enum) && prop.enum.length === 1 && typeof prop.enum[0] === "string") {
      return { key, value: prop.enum[0] };
    }
    if (typeof prop.const === "string") {
      return { key, value: prop.const };
    }
  }
  return null;
}

function isTaggedUnion(schema: JsonSchema): boolean {
  if (!Array.isArray(schema.oneOf) || schema.oneOf.length === 0) return false;
  return schema.oneOf.every((v) => discriminantOf(v) !== null);
}

// ---------------------------------------------------------------------------
// Type/decoder resolution for a single (possibly nested) schema.
// Returns TS type text and Zod expression text.
// `generic` supplies the payload param name + schema identifier when inside a
// generic type (DashboardEnvelope<TPayload>).
// ---------------------------------------------------------------------------

interface ResolveCtx {
  genericParam?: string;
  genericSchemaVar?: string;
}

function resolveType(schema: JsonSchema, ctx: ResolveCtx): { ts: string; zod: string } {
  if (schema["x-generic-ref"]) {
    return {
      ts: ctx.genericParam ?? schema["x-generic-ref"],
      zod: ctx.genericSchemaVar ?? "z.unknown()",
    };
  }

  if (schema.$ref) {
    const name = refName(schema.$ref);
    return { ts: name, zod: `z.lazy(() => ${name}Schema)` };
  }

  if (schema.const !== undefined) {
    return { ts: quote(String(schema.const)), zod: `z.literal(${quote(String(schema.const))})` };
  }

  if (Array.isArray(schema.enum)) {
    const strings = schema.enum.filter((v): v is string => typeof v === "string");
    if (strings.length === schema.enum.length && strings.length > 0) {
      const sorted = [...strings].sort();
      const tsUnion = sorted.map(quote).join(" | ");
      if (sorted.length === 1) {
        return { ts: quote(sorted[0] as string), zod: `z.literal(${quote(sorted[0] as string)})` };
      }
      const zodList = sorted.map(quote).join(", ");
      return { ts: tsUnion, zod: `z.enum([${zodList}])` };
    }
  }

  if (isTaggedUnion(schema)) {
    return resolveTaggedUnion(schema, ctx);
  }

  if (Array.isArray(schema.anyOf)) {
    const parts = schema.anyOf.map((s) => resolveType(s, ctx));
    return {
      ts: parts.map((p) => p.ts).join(" | "),
      zod: `z.union([${parts.map((p) => p.zod).join(", ")}])`,
    };
  }

  const types = Array.isArray(schema.type) ? schema.type : schema.type ? [schema.type] : [];
  const nullable = types.includes("null");
  const primary = types.find((t) => t !== "null");

  if (primary === "object" || (!primary && schema.properties)) {
    const obj = resolveObject(schema, ctx);
    return applyNullable(obj, nullable);
  }
  if (primary === "array") {
    const inner = resolveType(schema.items ?? {}, ctx);
    return applyNullable({ ts: `Array<${inner.ts}>`, zod: `z.array(${inner.zod})` }, nullable);
  }
  if (primary === "string") {
    return applyNullable({ ts: "string", zod: "z.string()" }, nullable);
  }
  if (primary === "integer") {
    return applyNullable({ ts: "number", zod: "z.number().int()" }, nullable);
  }
  if (primary === "number") {
    return applyNullable({ ts: "number", zod: "z.number()" }, nullable);
  }
  if (primary === "boolean") {
    return applyNullable({ ts: "boolean", zod: "z.boolean()" }, nullable);
  }
  if (nullable && !primary) {
    // A null-only schema (e.g. the `{ "type": "null" }` arm of a nullable ref).
    return { ts: "null", zod: "z.null()" };
  }

  return { ts: "unknown", zod: "z.unknown()" };
}

function applyNullable(t: { ts: string; zod: string }, nullable: boolean): { ts: string; zod: string } {
  if (!nullable) return t;
  return { ts: `${t.ts} | null`, zod: `${t.zod}.nullable()` };
}

function resolveObject(schema: JsonSchema, ctx: ResolveCtx): { ts: string; zod: string } {
  const props = schema.properties ?? {};
  const required = new Set(schema.required ?? []);
  const keys = Object.keys(props).sort();

  const tsFields: string[] = [];
  const zodFields: string[] = [];
  for (const key of keys) {
    const resolved = resolveType(props[key] as JsonSchema, ctx);
    const optional = !required.has(key);
    tsFields.push(`  ${key}${optional ? "?" : ""}: ${resolved.ts};`);
    zodFields.push(`  ${key}: ${optional ? `${resolved.zod}.optional()` : resolved.zod},`);
  }

  const ts = tsFields.length ? `{\n${tsFields.join("\n")}\n}` : "Record<string, never>";
  const zod = zodFields.length ? `z.object({\n${zodFields.join("\n")}\n})` : "z.object({})";
  return { ts, zod };
}

// ---------------------------------------------------------------------------
// Tagged union: discriminated + exhaustive; unknown tag -> unsupported_schema.
// ---------------------------------------------------------------------------

function resolveTaggedUnion(schema: JsonSchema, ctx: ResolveCtx): { ts: string; zod: string } {
  const variants = (schema.oneOf ?? [])
    .map((v) => ({ disc: discriminantOf(v) as { key: string; value: string }, schema: v }))
    .sort((a, b) => a.disc.value.localeCompare(b.disc.value));

  const discKey = variants[0]?.disc.key ?? "kind";

  const tsVariants = variants.map((v) => resolveObject(v.schema, ctx).ts);
  // Append the synthesized unsupported_schema branch.
  tsVariants.push(`{\n  ${discKey}: "unsupported_schema";\n  raw: unknown;\n}`);
  const ts = tsVariants.map((t) => `\n  | ${t.replace(/\n/g, "\n  ")}`).join("");

  const variantEntries = variants
    .map((v) => `    ${quote(v.disc.value)}: ${resolveObject(v.schema, ctx).zod},`)
    .join("\n");

  const zod = [
    "z.custom().transform((rawInput, ctx) => {",
    "    const variants: Record<string, z.ZodTypeAny> = {",
    variantEntries,
    "    };",
    `    const disc = typeof rawInput === "object" && rawInput !== null`,
    `      ? (rawInput as Record<string, unknown>)[${quote(discKey)}]`,
    "      : undefined;",
    `    if (typeof disc !== "string" || !(disc in variants)) {`,
    `      return { ${discKey}: "unsupported_schema" as const, raw: rawInput };`,
    "    }",
    "    const parsed = variants[disc]!.safeParse(rawInput);",
    "    if (!parsed.success) {",
    "      for (const issue of parsed.error.issues) ctx.addIssue(issue);",
    "      return z.NEVER;",
    "    }",
    "    return parsed.data;",
    "  })",
  ].join("\n");

  return { ts, zod };
}

// ---------------------------------------------------------------------------
// Named def emission.
// ---------------------------------------------------------------------------

function emitNamedDef(name: string, schema: JsonSchema): string {
  const genericParam = schema["x-generic"];

  if (genericParam) {
    // Generic type + decoder factory (DashboardEnvelope<TPayload>).
    const schemaVar = "payloadSchema";
    const ctx: ResolveCtx = { genericParam, genericSchemaVar: schemaVar };
    const obj = resolveObject(schema, ctx);
    const desc = schema.description ? `/** ${schema.description} */\n` : "";
    return [
      `${desc}export interface ${name}<${genericParam}> ${obj.ts.replace(/^\{/, "{").replace(/: /g, ": ")}`,
      "",
      `export function ${name}Schema<${genericParam}>(`,
      `  ${schemaVar}: z.ZodType<${genericParam}>,`,
      `): z.ZodType<${name}<${genericParam}>> {`,
      `  return ${obj.zod.replace(/\n/g, "\n  ")} as unknown as z.ZodType<${name}<${genericParam}>>;`,
      "}",
    ].join("\n");
  }

  const ctx: ResolveCtx = {};

  if (isTaggedUnion(schema)) {
    const union = resolveTaggedUnion(schema, ctx);
    const desc = schema.description ? `/** ${schema.description} */\n` : "";
    return [
      `${desc}export type ${name} =${union.ts};`,
      "",
      `export const ${name}Schema: z.ZodType<${name}> = ${union.zod} as unknown as z.ZodType<${name}>;`,
    ].join("\n");
  }

  const resolved = resolveType(schema, ctx);
  const desc = schema.description ? `/** ${schema.description} */\n` : "";
  const isObject =
    schema.type === "object" || (!schema.type && schema.properties !== undefined);
  if (isObject) {
    return [
      `${desc}export interface ${name} ${resolved.ts}`,
      "",
      `export const ${name}Schema: z.ZodType<${name}> = ${resolved.zod};`,
    ].join("\n");
  }
  return [
    `${desc}export type ${name} = ${resolved.ts};`,
    "",
    `export const ${name}Schema: z.ZodType<${name}> = ${resolved.zod};`,
  ].join("\n");
}

// ---------------------------------------------------------------------------
// Bundle -> files.
// ---------------------------------------------------------------------------

export function generateContracts(bundles: JsonSchema[]): GeneratedContracts {
  const defs: Array<[string, JsonSchema]> = [];
  const revisions: string[] = [];
  for (const bundle of bundles) {
    if (bundle.schemaRevision) revisions.push(bundle.schemaRevision);
    defs.push(...collectDefs(bundle));
  }
  defs.sort((a, b) => a[0].localeCompare(b[0]));

  const schemaRevision = revisions.sort().join("+") || "unversioned";

  const blocks: string[] = [];
  blocks.push(HEADER);
  blocks.push(`import { z } from "zod";`);
  blocks.push(
    [
      "/**",
      " * Exhaustiveness helper for discriminated-union switches. A `default`",
      " * branch that calls `assertNever(value)` fails to compile if a union",
      " * member is left unhandled.",
      " */",
      "export function assertNever(value: never): never {",
      "  throw new Error(`Unhandled discriminated union member: ${JSON.stringify(value)}`);",
      "}",
    ].join("\n"),
  );
  blocks.push(`export const SCHEMA_REVISION = ${quote(schemaRevision)} as const;`);

  for (const [name, schema] of defs) {
    blocks.push(emitNamedDef(name, schema));
  }

  const generated = blocks.join("\n\n") + "\n";

  const index = [
    HEADER,
    "",
    `export * from "./contracts.generated";`,
    "",
  ].join("\n");

  return {
    files: {
      [GENERATED_FILE]: generated,
      [INDEX_FILE]: index,
    },
  };
}

export const OUTPUT_FILES = { GENERATED_FILE, INDEX_FILE };
