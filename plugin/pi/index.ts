/**
 * TraceDecay integration for Pi.
 *
 * Deployed by `tracedecay install --agent pi` into
 * `~/.pi/agent/extensions/tracedecay/` and refreshed by the receipt-backed
 * host lifecycle. Mirrors the Codex / Cursor plugin surface inside Pi:
 *   - model-callable code-graph tools (CLI bridge over `tracedecay tool`)
 *   - an index-sync lifecycle hook after mutating tool calls
 *   - / commands for status, sync, and version
 *
 * Tool schemas are NOT hand-written here. The installer renders
 * `schemas.json` from the same generated MCP catalog authority the Hermes
 * plugin consumes (name, description, JSON Schema parameters, `read_only`),
 * and this extension derives every TypeBox schema from it at load time. The
 * `tracedecay_tool` passthrough is gated to the catalog's read-only subset
 * and rejects any name that is not a bare snake_case catalog identifier, so a
 * model-driven call can never reach a mutation tool through this surface.
 *
 * The tracedecay daemon owns the shared graph; tools and hooks run the
 * supported CLI, never direct database reads.
 */

import { execFile } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import {
  CORE_TOOL_NAMES,
  parseCatalog,
  resolvePassthroughTool,
  jsonSchemaToTypeBox,
  toolArguments,
  type CatalogTool,
} from "./lib";

/** Marker for install-time rendering and uninstall ownership checks. */
const TD_EXTENSION_MARKER = "TraceDecayPiExtension";
const TD_BIN = "__TRACEDECAY_BIN__";
const TOOL_TIMEOUT_MS = 120_000;
const SYNC_TIMEOUT_MS = 300_000;
const MAX_OUTPUT_CHARS = 120_000;
const SYNC_DEBOUNCE_MS = 45_000;

// ---------------------------------------------------------------------------
// Catalog loading (the installer-rendered generated authority)
// ---------------------------------------------------------------------------

function loadCatalog(): Map<string, CatalogTool> {
  const here =
    typeof __dirname !== "undefined"
      ? __dirname
      : new URL(".", import.meta.url).pathname;
  const raw = readFileSync(join(here, "schemas.json"), "utf-8");
  return new Map(parseCatalog(raw).map((tool) => [tool.name, tool]));
}

/** Human labels and prompt snippets for the curated first-class tools. */
const CORE_TOOL_LABELS: Record<string, [string, string]> = {
  status: ["TraceDecay Status", "Code-graph counts and freshness for the active project"],
  active_project: ["TraceDecay Active Project", "Resolved TraceDecay project id, root, and branch identity"],
  context: ["TraceDecay Context", "AI-ready semantic context for a task description"],
  search: ["TraceDecay Symbol Search", "Ranked symbol search over the project code graph"],
  grep: ["TraceDecay Content Grep", "Graph-enriched literal/regex content search"],
  files: ["TraceDecay Files", "Indexed project file listing"],
  find_exact_symbol: ["TraceDecay Exact Symbol", "Exact-name symbol lookup, O(log n) index probe"],
  source_outline: ["TraceDecay Source Outline", "Symbol outline of one file"],
  source_body: ["TraceDecay Source Body", "Current source body for a symbol node id"],
  node: ["TraceDecay Node", "Detailed information for one graph node id"],
  callers: ["TraceDecay Callers", "Callers / call sites of a symbol node id"],
  callees: ["TraceDecay Callees", "Outgoing calls of a symbol node id"],
  impact: ["TraceDecay Impact", "Dependent-symbol impact radius of a node id"],
  diff_context: ["TraceDecay Diff Context", "Semantic diff context for changed file paths"],
  affected: ["TraceDecay Affected Tests", "Affected-test selection for changed files"],
  test_map: ["TraceDecay Test Map", "Which tests cover a symbol or file"],
  diagnostics: ["TraceDecay Diagnostics", "Retained compiler diagnostics for the indexed generation"],
  diagnose: ["TraceDecay Diagnose", "Map raw rustc stderr diagnostics to graph nodes"],
};

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

interface RunResult {
  ok: boolean;
  text: string;
}

function runCli(
  args: string[],
  cwd: string,
  timeoutMs: number,
  signal?: AbortSignal
): Promise<RunResult> {
  return new Promise((resolve) => {
    const child = execFile(
      TD_BIN,
      args,
      { cwd, timeout: timeoutMs, maxBuffer: 8 * 1024 * 1024, windowsHide: true },
      (error, stdout, stderr) => {
        if (error) {
          const detail = (stderr || stdout || "").trim() || String(error.message);
          resolve({ ok: false, text: detail });
          return;
        }
        resolve({ ok: true, text: (stdout || stderr || "").trim() });
      }
    );
    if (signal) {
      const onAbort = () => child.kill("SIGTERM");
      if (signal.aborted) {
        child.kill("SIGTERM");
      } else {
        signal.addEventListener("abort", onAbort, { once: true });
      }
    }
  });
}

function truncateOutput(text: string): string {
  if (text.length <= MAX_OUTPUT_CHARS) return text;
  return (
    text.slice(0, MAX_OUTPUT_CHARS) +
    `\n\n[truncated by pi tracedecay extension: ${text.length - MAX_OUTPUT_CHARS} more characters omitted]`
  );
}

/** Run a graph tool; start the daemon once and retry when it is down. */
async function runGraphTool(
  tool: string,
  params: Record<string, unknown>,
  cwd: string,
  signal?: AbortSignal
): Promise<string> {
  const attempt = () =>
    runCli(["tool", tool, "--args", JSON.stringify(params)], cwd, TOOL_TIMEOUT_MS, signal);
  let result = await attempt();
  if (
    !result.ok &&
    /daemon/i.test(result.text) &&
    /(socket|not available|unavailable|restarting)/i.test(result.text)
  ) {
    await runCli(["daemon", "start"], cwd, 60_000, signal);
    result = await attempt();
  }
  if (!result.ok) {
    throw new Error(`tracedecay ${tool} failed: ${result.text}`);
  }
  return truncateOutput(result.text);
}

// ---------------------------------------------------------------------------
// Extension factory
// ---------------------------------------------------------------------------

export default function tracedecayExtension(pi: ExtensionAPI) {
  void TD_EXTENSION_MARKER;
  const catalog = loadCatalog();

  for (const name of CORE_TOOL_NAMES) {
    const catalogTool = catalog.get(name);
    if (!catalogTool) continue;
    const [label, snippet] = CORE_TOOL_LABELS[name] ?? [`TraceDecay ${name}`, ""];
    pi.registerTool({
      name: `tracedecay_${name}`,
      label,
      description: catalogTool.description,
      ...(snippet ? { promptSnippet: snippet } : {}),
      promptGuidelines: [
        "Prefer the specific tracedecay_* tool that matches the task over raw file reads or grep.",
        "Reuse returned node ids and continuation handles across tracedecay_* calls.",
        "An empty index result does not prove absence; check tracedecay_status when coverage looks partial.",
      ],
      parameters: jsonSchemaToTypeBox(catalogTool.parameters, Type),
      async execute(_toolCallId, params, signal, _onUpdate, ctx) {
        const args = toolArguments(name, params as Record<string, unknown>);
        const text = await runGraphTool(name, args, ctx.cwd, signal ?? undefined);
        return { content: [{ type: "text" as const, text }], details: { tool: name } };
      },
    });
  }

  // Passthrough for the remaining catalog, gated to read-only tools only.
  pi.registerTool({
    name: "tracedecay_tool",
    label: "TraceDecay Tool (read-only)",
    description:
      "Invoke a read-only tracedecay catalog tool by bare name with its JSON arguments, through the same daemon the specific tools use. Mutation tools are rejected by this surface; use the tracedecay CLI for those.",
    promptSnippet: "Passthrough to any remaining read-only tracedecay tool",
    parameters: Type.Object({
      tool: Type.String({ description: "Exact read-only tracedecay tool name" }),
      args: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
    }),
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      const { tool: requested, args } = params as {
        tool: unknown;
        args: Record<string, unknown> | undefined;
      };
      const catalogTool = resolvePassthroughTool(requested, catalog);
      const text = await runGraphTool(
        catalogTool.name,
        toolArguments(catalogTool.name, args ?? {}),
        ctx.cwd,
        signal ?? undefined
      );
      return {
        content: [{ type: "text" as const, text }],
        details: { tool: catalogTool.name },
      };
    },
  });

  // --- Lifecycle hook (no host-identity borrowing) -------------------------

  let lastSyncAt = 0;

  pi.on("tool_result", (event, ctx) => {
    // Keep the index current after mutating tool calls, debounced, like the
    // Codex PostToolUse hook matcher (Bash | apply_patch).
    if (event.toolName !== "bash" && event.toolName !== "edit" && event.toolName !== "write") {
      return;
    }
    if (event.isError) return;
    const now = Date.now();
    if (now - lastSyncAt < SYNC_DEBOUNCE_MS) return;
    lastSyncAt = now;
    void runCli(["sync"], ctx.cwd, SYNC_TIMEOUT_MS);
  });

  // --- Slash commands ------------------------------------------------------

  pi.registerCommand("tracedecay", {
    description: "Show tracedecay project status (graph counts, freshness)",
    handler: async (_args, ctx) => {
      const result = await runCli(["status", "--json"], ctx.cwd, TOOL_TIMEOUT_MS, ctx.signal ?? undefined);
      if (!result.ok) {
        ctx.ui.notify(`tracedecay status failed: ${result.text}`, "error");
        return;
      }
      try {
        const parsed = JSON.parse(result.text) as {
          project_root?: string;
          graph_statistics?: { state?: string; reason?: string; summary?: unknown };
          project_open?: { state?: string; detail?: string };
        };
        const graph = parsed.graph_statistics;
        const openState = parsed.project_open?.state ?? "unknown";
        const lines = [
          `project: ${parsed.project_root ?? "unknown"}`,
          `graph: ${graph?.state ?? "unknown"}${graph?.reason ? ` (${graph.reason})` : ""}`,
          `open: ${openState}`,
        ].join(" · ");
        ctx.ui.notify(
          `tracedecay ${lines}`,
          graph?.state === "unavailable" || openState === "stalled" ? "error" : "info"
        );
      } catch {
        ctx.ui.notify(truncateOutput(result.text), "info");
      }
    },
  });

  pi.registerCommand("tracedecay-sync", {
    description: "Run an incremental tracedecay sync for the current project",
    handler: async (_args, ctx) => {
      const result = await runCli(["sync"], ctx.cwd, SYNC_TIMEOUT_MS, ctx.signal ?? undefined);
      if (result.ok) {
        ctx.ui.notify("tracedecay sync complete", "info");
      } else {
        ctx.ui.notify(`tracedecay sync failed: ${result.text}`, "error");
      }
    },
  });

  pi.registerCommand("tracedecay-version", {
    description: "Show the tracedecay binary and daemon versions",
    handler: async (_args, ctx) => {
      const result = await runCli(["--version"], ctx.cwd, 10_000);
      ctx.ui.notify(result.ok ? result.text : `tracedecay: ${result.text}`, result.ok ? "info" : "error");
    },
  });
}
