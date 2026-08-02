/**
 * The shadowing gate.
 *
 * `src/contracts/generated.ts` is the one wire boundary. Nothing else in the
 * dashboard may export a name it already exports, because at an import site a
 * shadow and the original are indistinguishable — the only difference is which
 * module the line happens to name, and both look equally correct in review.
 *
 * This is not hypothetical. Explorer declared `ExplorerQueryRunV1Schema`,
 * `ExplorerSourceProgressV1Schema` and `ExplorerSourceIdV1Schema` by hand under
 * exactly the generated names, and drifted stricter than Rust: `freshness` was
 * a five-member `z.enum` against a plain `z.string()`, so the first new
 * freshness value from the server would have failed the parse and taken the
 * whole Explorer result down. `dashboard/e2e/axe-explorer.ts` imported the
 * shadow, so the accessibility harness had been checking a shape the server
 * never promised. Observatory did the same thing one module over, extending the
 * wrong payload and pinning a producer count at `.length(5)`.
 *
 * `npm run contracts:check` cannot catch this: it verifies that the generated
 * files match fresh Rust output, and a shadow declared somewhere else leaves
 * those files untouched. Nor can an uncontracted-route gate — these routes ARE
 * contracted. The problem is shadowing, not absence, so it needs its own check.
 *
 * Scope: exported names. A module-local const that happens to collide is
 * invisible at an import site and is not what broke; catching those would mean
 * rejecting ordinary words like `Deadline` and `Coverage` that the catalog also
 * happens to export.
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const DASHBOARD_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const GENERATED_FILE = join(DASHBOARD_ROOT, "src/contracts/generated.ts");

/** The generated module and the two thin re-export paths in front of it. These
 * are allowed to carry generated names; they ARE the boundary. */
const BOUNDARY_FILES = new Set([
  "src/contracts/generated.ts",
  "src/contracts/index.ts",
]);

const SEARCH_ROOTS = ["src", "e2e", "stories", "codegen"];

function sourceFiles(root: string): string[] {
  const absolute = join(DASHBOARD_ROOT, root);
  let entries: string[];
  try {
    entries = readdirSync(absolute);
  } catch {
    return [];
  }
  return entries.flatMap((entry) => {
    const path = join(absolute, entry);
    if (entry === "node_modules") return [];
    if (root === "codegen" && entry === ".preview") return [];
    if (statSync(path).isDirectory()) return sourceFiles(join(root, entry));
    return /\.tsx?$/.test(entry) ? [join(root, entry)] : [];
  });
}

/** Names the generated module exports, as values or as types. */
function generatedExports(): Set<string> {
  const source = readFileSync(GENERATED_FILE, "utf8");
  const names = new Set<string>();
  for (const match of source.matchAll(
    /^export\s+(?:declare\s+)?(?:const|let|var|function|class|type|interface|enum)\s+([A-Za-z_$][\w$]*)/gm,
  )) {
    names.add(match[1]!);
  }
  return names;
}

/** Names a module exports: direct declarations plus every form of re-export
 * clause, including renames (`export { A as B }` exports `B`, not `A`). */
function exportedNames(source: string): string[] {
  const names: string[] = [];
  for (const match of source.matchAll(
    /^export\s+(?:declare\s+)?(?:async\s+)?(?:const|let|var|function|class|type|interface|enum)\s+([A-Za-z_$][\w$]*)/gm,
  )) {
    names.push(match[1]!);
  }
  for (const clause of source.matchAll(/^export\s+(?:type\s+)?\{([^}]*)\}/gms)) {
    for (const specifier of clause[1]!.split(",")) {
      const parts = specifier
        .replace(/\btype\s+/g, "")
        .trim()
        .split(/\s+as\s+/);
      const exported = parts[parts.length - 1]?.trim();
      if (exported && /^[A-Za-z_$][\w$]*$/.test(exported) && exported !== "default") {
        names.push(exported);
      }
    }
  }
  return names;
}

describe("generated wire names are not shadowed", () => {
  it("exports every generated name from exactly one module", () => {
    const generated = generatedExports();
    // Sanity: a regex that silently matched nothing would make this gate pass
    // for the wrong reason, which is the failure mode of a check like this.
    expect(generated.size).toBeGreaterThan(100);
    expect(generated.has("ExplorerQueryRunV1Schema")).toBe(true);
    expect(generated.has("StorageFindingsPayloadV1Schema")).toBe(true);

    const offences: string[] = [];
    for (const root of SEARCH_ROOTS) {
      for (const file of sourceFiles(root)) {
        const relativePath = relative(".", file).split("\\").join("/");
        if (BOUNDARY_FILES.has(relativePath)) continue;
        const source = readFileSync(join(DASHBOARD_ROOT, file), "utf8");
        for (const name of exportedNames(source)) {
          if (generated.has(name)) offences.push(`${relativePath} exports ${name}`);
        }
      }
    }

    expect(
      offences,
      `These modules export a name that src/contracts/generated.ts already exports, so an ` +
        `import of that name is ambiguous at the call site. Import it from ` +
        `src/contracts/generated.ts instead of re-declaring or re-exporting it:\n  ` +
        offences.join("\n  "),
    ).toEqual([]);
  });

  it("keeps wire shapes out of per-workspace contract modules", () => {
    // The convention that produced the shadows: a file in each workspace whose
    // only job was declaring wire shapes, sitting at a shorter import path than
    // the generated barrel with nothing to stop it reusing a generated name.
    // Every route these declared is now modelled in Rust, so they are read from
    // `src/contracts/generated.ts` and there is nowhere else for a wire shape to
    // live. A route Rust does not model yet is a backend gap to report, not a
    // shape to hand-write.
    const strays = sourceFiles("src/workspaces").filter((file) =>
      /\/contracts\.tsx?$/.test(file),
    );
    expect(
      strays,
      "Per-workspace contracts modules are not a thing any more. Import the " +
        "generated schema from src/contracts/generated.ts.",
    ).toEqual([]);
  });
});
