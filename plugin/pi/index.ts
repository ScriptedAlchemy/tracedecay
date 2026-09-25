/**
 * TraceDecay integration for Pi.
 *
 * Deployed by `tracedecay install --agent pi` into
 * `~/.pi/agent/extensions/tracedecay/` and refreshed by the receipt-backed
 * host lifecycle. Mirrors the Codex / Cursor plugin surface inside Pi:
 *   - model-callable code-graph tools (CLI bridge over `tracedecay tool`)
 *   - lifecycle hooks (session start, index sync after edits, savings at end)
 *   - / commands for status, sync, and version
 *
 * The tracedecay daemon owns the shared graph; tools and hooks run the
 * supported CLI, never direct database reads.
 */

import { execFile } from "node:child_process";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

/** Marker for install-time rendering and uninstall ownership checks. */
const TD_EXTENSION_MARKER = "TraceDecayPiExtension";
const TD_BIN = "__TRACEDECAY_BIN__";
const TOOL_TIMEOUT_MS = 120_000;
const SYNC_TIMEOUT_MS = 300_000;
const MAX_OUTPUT_CHARS = 120_000;
const SYNC_DEBOUNCE_MS = 45_000;

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
// Tool definitions (the tracedecay MCP tool surface, bridged through the CLI)
// ---------------------------------------------------------------------------

const OPT_STRING = Type.Optional(Type.String());
const OPT_INT = Type.Optional(Type.Integer());
const OPT_BOOL = Type.Optional(Type.Boolean());
const OPT_STRINGS = Type.Optional(Type.Array(Type.String()));

interface GraphToolSpec {
  /** Tracedecay tool name (the MCP tool name). */
  tool: string;
  name: string;
  label: string;
  description: string;
  promptSnippet: string;
  parameters: ReturnType<typeof Type.Object>;
}

const toolSpecs: GraphToolSpec[] = [
  {
    tool: "status",
    name: "tracedecay_status",
    label: "TraceDecay Status",
    description:
      "Return a compact summary of the active project's code graph: symbol counts, freshness, and indexing state. Run before other graph tools when the index state is unknown.",
    promptSnippet: "Code-graph counts and freshness for the active project",
    parameters: Type.Object({
      include_branch_diagnostics: OPT_BOOL,
      include_session_ingest: OPT_BOOL,
    }),
  },
  {
    tool: "active_project",
    name: "tracedecay_active_project",
    label: "TraceDecay Active Project",
    description:
      "Return the resolved active TraceDecay project context: project id, root, scope prefix, branch identity, and store paths. Use instead of guessing from repo-local markers.",
    promptSnippet: "Resolved TraceDecay project id, root, and branch identity",
    parameters: Type.Object({}),
  },
  {
    tool: "context",
    name: "tracedecay_context",
    label: "TraceDecay Context",
    description:
      "Build AI-ready context for a task description: relevant symbols, relationships, and up to three memory matches, optionally with code snippets. Use for broad questions that need relationship synthesis.",
    promptSnippet: "AI-ready semantic context for a task description",
    parameters: Type.Object({
      task: Type.String({ description: "Task description to build context for" }),
      max_tokens: OPT_INT,
      include_code: OPT_BOOL,
      include_memory: OPT_BOOL,
      lexical_anchors: OPT_STRINGS,
      max_code_blocks: OPT_INT,
    }),
  },
  {
    tool: "search",
    name: "tracedecay_search",
    label: "TraceDecay Symbol Search",
    description:
      "Exact and lexical code search over the active project's code graph: find symbols by name, identifier fragment, signature, path, or phrase through ranked exact and lexical routes. Use for symbol discovery; use tracedecay_grep for literal content search.",
    promptSnippet: "Ranked symbol search over the project code graph",
    parameters: Type.Object({
      query: Type.String({ description: "Symbol name, fragment, signature, path, or phrase" }),
      limit: OPT_INT,
      lexical_anchors: OPT_STRINGS,
      prefer_symbol: OPT_BOOL,
    }),
  },
  {
    tool: "grep",
    name: "tracedecay_grep",
    label: "TraceDecay Content Grep",
    description:
      "Literal or regex content search over text files in the working tree (respects .gitignore), graph-enriched: each hit resolves its enclosing symbol. Use for string literals, config keys, and error messages; use tracedecay_search for symbol names.",
    promptSnippet: "Graph-enriched literal/regex content search",
    parameters: Type.Object({
      pattern: Type.String({ description: "Literal or regex content to search for" }),
      path_glob: OPT_STRING,
      context_lines: OPT_INT,
      case_sensitive: OPT_BOOL,
      fixed_strings: OPT_BOOL,
      max_results: OPT_INT,
    }),
  },
  {
    tool: "files",
    name: "tracedecay_files",
    label: "TraceDecay Files",
    description: "List indexed project files to explore file structure without reading file contents.",
    promptSnippet: "Indexed project file listing",
    parameters: Type.Object({
      path: OPT_STRING,
      pattern: OPT_STRING,
      layout: Type.Optional(Type.Union([Type.Literal("flat"), Type.Literal("grouped")])),
    }),
  },
  {
    tool: "find_exact_symbol",
    name: "tracedecay_find_exact_symbol",
    label: "TraceDecay Exact Symbol",
    description:
      "Return every node whose name equals the given bare identifier via a single index probe. The cheapest lookup when the symbol name is already known; no fuzzy matching.",
    promptSnippet: "Exact-name symbol lookup, O(log n) index probe",
    parameters: Type.Object({
      name: Type.String({ description: "Exact bare symbol name (no ::, no glob)" }),
      limit: OPT_INT,
    }),
  },
  {
    tool: "source_outline",
    name: "tracedecay_source_outline",
    label: "TraceDecay Source Outline",
    description: "List indexed symbols in one project-relative file without reading their bodies.",
    promptSnippet: "Symbol outline of one file",
    parameters: Type.Object({
      file: Type.String({ description: "Project-relative file path" }),
    }),
  },
  {
    tool: "source_body",
    name: "tracedecay_source_body",
    label: "TraceDecay Source Body",
    description: "Read the current source body and line range for a symbol node id returned by another graph read.",
    promptSnippet: "Current source body for a symbol node id",
    parameters: Type.Object({
      node_id: Type.String({ description: "Symbol node id from search, outline, or another graph read" }),
    }),
  },
  {
    tool: "node",
    name: "tracedecay_node",
    label: "TraceDecay Node",
    description: "Retrieve detailed information about a single graph node by its id.",
    promptSnippet: "Detailed information for one graph node id",
    parameters: Type.Object({
      node_id: Type.String(),
    }),
  },
  {
    tool: "callers",
    name: "tracedecay_callers",
    label: "TraceDecay Callers",
    description:
      "Find references, usages, and call sites of a known symbol node id up to maximum_depth (default 3). Coverage is partial when a call target cannot be resolved exactly.",
    promptSnippet: "Callers / call sites of a symbol node id",
    parameters: Type.Object({
      node_id: Type.String(),
      maximum_depth: OPT_INT,
    }),
  },
  {
    tool: "callees",
    name: "tracedecay_callees",
    label: "TraceDecay Callees",
    description:
      "List outgoing calls of a known symbol node id up to maximum_depth (default 3). Trait-method callees also return concrete impls, tagged dispatch_via_trait.",
    promptSnippet: "Outgoing calls of a symbol node id",
    parameters: Type.Object({
      node_id: Type.String(),
      maximum_depth: OPT_INT,
    }),
  },
  {
    tool: "impact",
    name: "tracedecay_impact",
    label: "TraceDecay Impact",
    description: "Compute the impact radius of a node: all symbols that directly or indirectly depend on it.",
    promptSnippet: "Dependent-symbol impact radius of a node id",
    parameters: Type.Object({
      node_id: Type.String(),
      max_depth: OPT_INT,
    }),
  },
  {
    tool: "diff_context",
    name: "tracedecay_diff_context",
    label: "TraceDecay Diff Context",
    description:
      "Given changed file paths, return semantic context: which symbols were modified, what depends on them, and affected tests. Run before reviewing a raw diff.",
    promptSnippet: "Semantic diff context for changed file paths",
    parameters: Type.Object({
      files: Type.Array(Type.String(), { description: "Changed file paths" }),
      depth: OPT_INT,
    }),
  },
  {
    tool: "affected",
    name: "tracedecay_affected",
    label: "TraceDecay Affected Tests",
    description:
      "Find test files affected by changed source files via dependency graph traversal. Run before broad test suites.",
    promptSnippet: "Affected-test selection for changed files",
    parameters: Type.Object({
      files: Type.Array(Type.String(), { description: "Changed file paths" }),
      depth: OPT_INT,
      filter: OPT_STRING,
    }),
  },
  {
    tool: "test_map",
    name: "tracedecay_test_map",
    label: "TraceDecay Test Map",
    description:
      "Map source symbols to their test functions by walking the call graph up to depth 3. Static attribution: a listed test may be a direct or transitive caller.",
    promptSnippet: "Which tests cover a symbol or file",
    parameters: Type.Object({
      file: OPT_STRING,
      node_id: OPT_STRING,
    }),
  },
  {
    tool: "diagnostics",
    name: "tracedecay_diagnostics",
    label: "TraceDecay Diagnostics",
    description:
      "Read retained diagnostics for the current indexed generation, scoped to the workspace. Does not run a compiler; use the project build for fresh post-edit results.",
    promptSnippet: "Retained compiler diagnostics for the indexed generation",
    parameters: Type.Object({
      maximum_diagnostics: OPT_INT,
    }),
  },
  {
    tool: "diagnose",
    name: "tracedecay_diagnose",
    label: "TraceDecay Diagnose",
    description:
      "Parse raw cargo check / clippy stderr and map each diagnostic to the smallest containing graph node with callers pre-attached. Pass the full stderr capture, unfiltered.",
    promptSnippet: "Map raw rustc stderr diagnostics to graph nodes",
    parameters: Type.Object({
      cargo_output: Type.String({ description: "Raw stderr from cargo check / clippy / rustc" }),
      include_callers: OPT_BOOL,
    }),
  },
  {
    tool: "tracedecay_tool",
    name: "tracedecay_tool",
    label: "TraceDecay Tool (any)",
    description:
      "Invoke any other tracedecay graph tool by name with its JSON arguments, through the same daemon the specific tools use. Use only when a registered tracedecay_* tool does not cover the operation (for example branch, call_chain, coupling, module_api, or work tree queries).",
    promptSnippet: "Passthrough to any remaining tracedecay tool",
    parameters: Type.Object({
      tool: Type.String({ description: "Exact tracedecay tool name" }),
      args: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
    }),
  },
];

// ---------------------------------------------------------------------------
// Extension factory
// ---------------------------------------------------------------------------

export default function tracedecayExtension(pi: ExtensionAPI) {
  void TD_EXTENSION_MARKER;
  for (const spec of toolSpecs) {
    pi.registerTool({
      name: spec.name,
      label: spec.label,
      description: spec.description,
      promptSnippet: spec.promptSnippet,
      promptGuidelines: [
        "Prefer the specific tracedecay_* tool that matches the task over raw file reads or grep.",
        "Reuse returned node ids and continuation handles across tracedecay_* calls.",
        "An empty index result does not prove absence; check tracedecay_status when coverage looks partial.",
      ],
      parameters: spec.parameters,
      async execute(_toolCallId, params, signal, _onUpdate, ctx) {
        const { tool, name, ...rest } = params as Record<string, unknown> & {
          tool: string;
          name: string;
        };
        const target = spec.tool === "tracedecay_tool" ? tool : spec.tool;
        const args = spec.tool === "tracedecay_tool" ? (rest.args ?? {}) : rest;
        const text = await runGraphTool(
          target,
          args as Record<string, unknown>,
          ctx.cwd,
          signal ?? undefined
        );
        return { content: [{ type: "text" as const, text }], details: { tool: target } };
      },
    });
  }

  // --- Lifecycle hooks (mirror the Codex/Cursor hook set) -----------------

  let lastSyncAt = 0;

  pi.on("session_start", () => {
    // Register the session with the tracedecay daemon (LCM ingest). Fire and
    // forget; failures are not surfaced to the user.
    void runCli(["hook-codex-session-start"], process.cwd(), 10_000);
  });

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

  pi.on("agent_end", (_event, ctx) => {
    // Print session token savings like the Codex Stop hook.
    void runCli(["hook-stop"], ctx.cwd, 10_000).then((result) => {
      if (result.ok && result.text && result.text !== "{}") {
        ctx.ui.notify(`tracedecay: ${result.text}`, "info");
      }
    });
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
          schema_convergence?: { status?: string };
        };
        const graph = parsed.graph_statistics;
        const lines = [
          `project: ${parsed.project_root ?? "unknown"}`,
          `graph: ${graph?.state ?? "unknown"}${graph?.reason ? ` (${graph.reason})` : ""}`,
          `open: ${parsed.project_open?.state ?? "ok"}`,
        ].join(" · ");
        ctx.ui.notify(`tracedecay ${lines}`, graph?.state === "unavailable" ? "error" : "info");
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
