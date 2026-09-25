import { expect, test } from "bun:test";

import {
  jsonSchemaToTypeBox,
  parseCatalog,
  resolvePassthroughTool,
  toolArguments,
  type CatalogTool,
} from "./lib";

function catalogEntry(overrides: Partial<CatalogTool> = {}): CatalogTool {
  return {
    name: "context",
    description: "Build AI-ready context",
    parameters: {
      type: "object",
      properties: {
        task: { type: "string", description: "Task description" },
        max_tokens: { type: "integer" },
      },
      required: ["task"],
    },
    read_only: true,
    ...overrides,
  };
}

/** A recording TypeBox stub: returns tagged plain objects for inspection. */
function recordingTypeBox() {
  return {
    String: (o?: Record<string, unknown>) => ({ t: "string", ...o }),
    Integer: (o?: Record<string, unknown>) => ({ t: "integer", ...o }),
    Number: (o?: Record<string, unknown>) => ({ t: "number", ...o }),
    Boolean: (o?: Record<string, unknown>) => ({ t: "boolean", ...o }),
    Literal: (value: unknown) => ({ t: "literal", value }),
    Union: (members: unknown[]) => ({ t: "union", members }),
    Array: (items: unknown) => ({ t: "array", items }),
    Object: (properties: Record<string, unknown>) => ({ t: "object", properties }),
    Optional: (schema: unknown) => ({ t: "optional", schema }),
    Unknown: (o?: Record<string, unknown>) => ({ t: "unknown", ...o }),
  };
}

test("required catalog keys always reach the bridge (find_exact_symbol.name)", () => {
  // Regression: the extension once destructured { tool, name, ...rest } from
  // every call, silently dropping find_exact_symbol's required `name`.
  const params = { name: "reserve_stock", limit: 3 };
  expect(toolArguments("find_exact_symbol", params)).toEqual(params);
  expect(toolArguments("grep", { pattern: "x" })).toEqual({ pattern: "x" });
});

test("passthrough only admits read-only catalog tools with bare names", () => {
  const readOnly = catalogEntry({ name: "context", read_only: true });
  const mutating = catalogEntry({ name: "str_replace", read_only: false });
  const byName = new Map([
    [readOnly.name, readOnly],
    [mutating.name, mutating],
  ]);

  expect(resolvePassthroughTool("context", byName).name).toBe("context");
  expect(() => resolvePassthroughTool("str_replace", byName)).toThrow(/not read-only/);
  expect(() => resolvePassthroughTool("unknown_tool", byName)).toThrow(/unknown tool/);
  expect(() => resolvePassthroughTool("--project=/tmp", byName)).toThrow(/invalid tool name/);
  expect(() => resolvePassthroughTool("context; rm -rf", byName)).toThrow(/invalid tool name/);
  expect(() => resolvePassthroughTool(42, byName)).toThrow(/invalid tool name/);
});

test("schemas.json parsing keeps only well-formed catalog entries", () => {
  const parsed = parseCatalog(
    JSON.stringify([
      catalogEntry(),
      { name: "broken", description: "no parameters field" },
      { name: 7, description: "not a string name", parameters: {} },
      "not an object",
      null,
      catalogEntry({ name: "grep", read_only: false }),
    ])
  );
  expect(parsed.map((entry) => entry.name)).toEqual(["context", "grep"]);
  expect(parsed[1].read_only).toBe(false);
  expect(() => parseCatalog("{")).toThrow();
  expect(() => parseCatalog("[]")).not.toThrow();
});

test("JSON schema translates to TypeBox shapes with required/optional split", () => {
  const TypeBox = recordingTypeBox();
  const translated = jsonSchemaToTypeBox(catalogEntry().parameters, TypeBox) as {
    properties: Record<string, { t?: string; schema?: { t?: string } }>;
  };
  const { task, max_tokens } = translated.properties;
  expect(task.t).toBe("string");
  expect(task.description).toBe("Task description");
  expect(max_tokens.t).toBe("optional");
  expect(max_tokens.schema?.t).toBe("integer");

  const stringEnum = jsonSchemaToTypeBox(
    { type: "string", enum: ["markdown", "json"] },
    TypeBox
  ) as { t: string; members: { value: string }[] };
  expect(stringEnum.t).toBe("union");
  expect(stringEnum.members.map((member) => member.value)).toEqual(["markdown", "json"]);

  const single = jsonSchemaToTypeBox({ type: "string", enum: ["only"] }, TypeBox) as {
    t: string;
    value: string;
  };
  expect(single.t).toBe("literal");
  expect(single.value).toBe("only");

  const array = jsonSchemaToTypeBox(
    { type: "array", items: { type: "string" } },
    TypeBox
  ) as { t: string; items: { t?: string } };
  expect(array.t).toBe("array");
  expect(array.items.t).toBe("string");
});
