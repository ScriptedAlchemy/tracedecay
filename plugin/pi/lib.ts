/**
 * Pure, dependency-free helpers shared by the TraceDecay Pi extension
 * (`index.ts`) and its unit tests (`lib.test.ts`).
 *
 * Everything in this file is deterministic over its inputs: no process, file,
 * or network access, so the tests run under plain `bun test` without a
 * node_modules tree.
 */

export interface CatalogTool {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  read_only: boolean;
}

/** Bare catalog names only: snake_case identifiers, never flags or paths. */
export const TOOL_NAME_PATTERN = /^[a-z_]+$/;

/** The curated set of catalog tools registered as first-class pi tools. */
export const CORE_TOOL_NAMES: readonly string[] = [
  "status",
  "active_project",
  "context",
  "search",
  "grep",
  "files",
  "find_exact_symbol",
  "source_outline",
  "source_body",
  "node",
  "callers",
  "callees",
  "impact",
  "diff_context",
  "affected",
  "test_map",
  "diagnostics",
  "diagnose",
];

/**
 * Validate a passthrough tool name and return its catalog entry.
 *
 * The passthrough is gated to the catalog's read-only subset so prompt-driven
 * calls can never reach a mutation tool without a host approval surface; the
 * operator's mutation workflow is the tracedecay CLI.
 */
export function resolvePassthroughTool(
  toolName: unknown,
  byName: ReadonlyMap<string, CatalogTool>
): CatalogTool {
  if (typeof toolName !== "string" || !TOOL_NAME_PATTERN.test(toolName)) {
    throw new Error(
      `tracedecay_tool: invalid tool name ${JSON.stringify(toolName)}; ` +
        "pass a bare catalog name like context or call_chain"
    );
  }
  const catalogTool = byName.get(toolName);
  if (!catalogTool) {
    throw new Error(
      `tracedecay_tool: unknown tool ${toolName}; it is not in the shipped tracedecay catalog`
    );
  }
  if (!catalogTool.read_only) {
    throw new Error(
      `tracedecay_tool: ${toolName} is not read-only; use the tracedecay CLI for tools that mutate state`
    );
  }
  return catalogTool;
}

/** Parse the installer-rendered schemas.json into catalog entries. */
export function parseCatalog(raw: string): CatalogTool[] {
  const parsed = JSON.parse(raw) as unknown;
  if (!Array.isArray(parsed)) {
    throw new Error("pi tracedecay extension: schemas.json must be an array");
  }
  return parsed.flatMap((entry) => {
    if (
      !entry ||
      typeof entry !== "object" ||
      typeof (entry as Record<string, unknown>).name !== "string" ||
      typeof (entry as Record<string, unknown>).description !== "string" ||
      typeof (entry as Record<string, unknown>).parameters !== "object"
    ) {
      return [];
    }
    const record = entry as Record<string, unknown>;
    // The generated catalog keys every tool by its MCP name
    // (`tracedecay_search`); the CLI bridge spells the bare tool name
    // (`search`), so the prefix is stripped here and every gate below
    // reasons over bare names.
    const name = (record.name as string).replace(/^tracedecay_/, "");
    if (!TOOL_NAME_PATTERN.test(name)) {
      return [];
    }
    return [
      {
        name,
        description: record.description as string,
        parameters: record.parameters as Record<string, unknown>,
        read_only: record.read_only === true,
      },
    ];
  });
}

type TypeBoxStatic = {
  String: (options?: Record<string, unknown>) => unknown;
  Integer: (options?: Record<string, unknown>) => unknown;
  Number: (options?: Record<string, unknown>) => unknown;
  Boolean: (options?: Record<string, unknown>) => unknown;
  Literal: (value: unknown) => unknown;
  Union: (members: unknown[]) => unknown;
  Array: (items: unknown) => unknown;
  Object: (properties: Record<string, unknown>) => unknown;
  Optional: (schema: unknown) => unknown;
  Unknown: (options?: Record<string, unknown>) => unknown;
};

/**
 * Translate a catalog JSON Schema object into TypeBox definitions.
 *
 * `TypeBox` is injected so this module stays dependency-free and the
 * translation is unit-testable with a recording stub.
 */
export function jsonSchemaToTypeBox(
  schema: unknown,
  TypeBox: TypeBoxStatic
): unknown {
  if (!schema || typeof schema !== "object") {
    return TypeBox.Unknown({});
  }
  const record = schema as Record<string, unknown>;
  const description =
    typeof record.description === "string" ? { description: record.description } : {};
  switch (record.type) {
    case "string": {
      const enumValues = Array.isArray(record.enum) ? record.enum : [];
      if (enumValues.length === 1) {
        return TypeBox.Literal(enumValues[0]);
      }
      if (enumValues.length > 1) {
        return TypeBox.Union(enumValues.map((value) => TypeBox.Literal(value)));
      }
      return TypeBox.String(description);
    }
    case "integer":
      return TypeBox.Integer(description);
    case "number":
      return TypeBox.Number(description);
    case "boolean":
      return TypeBox.Boolean(description);
    case "array": {
      const items =
        record.items && typeof record.items === "object"
          ? jsonSchemaToTypeBox(record.items, TypeBox)
          : TypeBox.Unknown({});
      return TypeBox.Array(items);
    }
    case "object": {
      const properties =
        record.properties && typeof record.properties === "object"
          ? (record.properties as Record<string, unknown>)
          : {};
      const required = new Set(
        Array.isArray(record.required)
          ? (record.required as unknown[]).filter(
              (name): name is string => typeof name === "string"
            )
          : []
      );
      const translated: Record<string, unknown> = {};
      for (const [name, propertySchema] of Object.entries(properties)) {
        const translatedSchema = jsonSchemaToTypeBox(propertySchema, TypeBox);
        translated[name] = required.has(name)
          ? translatedSchema
          : TypeBox.Optional(translatedSchema);
      }
      return TypeBox.Object(translated);
    }
    default:
      return TypeBox.Unknown({});
  }
}

/**
 * The argument mapping for one tool call: every registered tool forwards its
 * own validated parameters verbatim, so required keys (such as
 * `find_exact_symbol.name`) always reach the bridge.
 */
export function toolArguments(
  toolName: string,
  params: Record<string, unknown>
): Record<string, unknown> {
  void toolName;
  return params;
}
