/**
 * TraceDecay integration for Pi.
 *
 * Deployed by `tracedecay install --agent pi` into
 * `~/.pi/agent/extensions/tracedecay/` and refreshed by the receipt-backed
 * host lifecycle:
 *   - one model-callable tool per TraceDecay catalog tool, generated from the
 *     installer-rendered `schemas.json` and bridged over `tracedecay tool`
 *   - `session_start` / `agent_end` forwarded to `tracedecay hook-pi-event`
 *   - index sync after edits, and `/tracedecay{,-sync,-version}` commands
 *
 * The tracedecay daemon owns the shared graph; tools and hooks run the
 * supported CLI, never direct database reads.
 */

import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { ExtensionAPI, ExtensionContext, ToolDefinition } from "@earendil-works/pi-coding-agent";

/** Marker for install-time rendering and ownership checks. */
const TD_EXTENSION_MARKER = "TraceDecayPiExtension";
const TD_BIN = "__TRACEDECAY_BIN__";
const TOOL_TIMEOUT_MS = 120_000;
const HOOK_TIMEOUT_MS = 10_000;
const SYNC_TIMEOUT_MS = 300_000;
const MAX_OUTPUT_CHARS = 120_000;
const SYNC_DEBOUNCE_MS = 45_000;
/** `tracedecay` exits with `EX_UNAVAILABLE` when no daemon serves the profile. */
export const DAEMON_UNREACHABLE_EXIT_CODE = 69;
/** Catalog tool names are fixed identifiers; nothing else may reach argv. */
const TOOL_NAME = /^tracedecay_[a-z0-9_]+$/;

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

export interface RunResult {
  /** Process exit status; `null` when the child was killed or never spawned. */
  code: number | null;
  stdout: string;
  stderr: string;
}

export interface RunOptions {
  cwd: string;
  timeoutMs: number;
  signal?: AbortSignal;
  /** Written to the child's stdin, which is then closed either way. */
  input?: string;
  executable?: string;
}

export type Runner = (args: string[], options: RunOptions) => Promise<RunResult>;

export const runCli: Runner = (args, options) =>
  new Promise((resolve) => {
    const child = execFile(
      options.executable ?? TD_BIN,
      args,
      {
        cwd: options.cwd,
        timeout: options.timeoutMs,
        maxBuffer: 8 * 1024 * 1024,
        windowsHide: true,
        signal: options.signal,
      },
      (error, stdout, stderr) => {
        const code =
          error === null ? 0 : typeof error.code === "number" ? error.code : null;
        resolve({ code, stdout: String(stdout ?? ""), stderr: String(stderr ?? "") });
      },
    );
    // Every tracedecay entrypoint that reads stdin reads it to EOF, so an
    // open pipe would hold the child until the timeout kills it.
    child.stdin?.on("error", () => undefined);
    child.stdin?.end(options.input);
  });

function failureDetail(result: RunResult): string {
  const detail = (result.stderr || result.stdout).trim();
  if (detail) return detail;
  return result.code === null ? "process was killed or could not start" : `exit status ${result.code}`;
}

function truncateOutput(text: string): string {
  if (text.length <= MAX_OUTPUT_CHARS) return text;
  return (
    text.slice(0, MAX_OUTPUT_CHARS) +
    `\n\n[truncated by pi tracedecay extension: ${text.length - MAX_OUTPUT_CHARS} more characters omitted]`
  );
}

// ---------------------------------------------------------------------------
// Catalog tools
// ---------------------------------------------------------------------------

/** One entry of the installer-generated `schemas.json`. */
export interface CatalogToolSchema {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  read_only: boolean;
}

export function parseToolSchemas(json: string): CatalogToolSchema[] {
  const parsed: unknown = JSON.parse(json);
  if (!Array.isArray(parsed)) {
    throw new Error("tracedecay schemas.json is not an array");
  }
  return parsed.map((entry: Record<string, unknown>) => {
    const { name, description, parameters, read_only } = entry ?? {};
    if (typeof name !== "string" || !TOOL_NAME.test(name)) {
      throw new Error(`tracedecay schemas.json has an invalid tool name: ${JSON.stringify(name)}`);
    }
    if (
      typeof description !== "string" ||
      typeof parameters !== "object" ||
      parameters === null ||
      typeof read_only !== "boolean"
    ) {
      throw new Error(`tracedecay schemas.json entry ${name} is malformed`);
    }
    return { name, description, parameters: parameters as Record<string, unknown>, read_only };
  });
}

function loadToolSchemas(): CatalogToolSchema[] {
  const path = join(dirname(fileURLToPath(import.meta.url)), "schemas.json");
  return parseToolSchemas(readFileSync(path, "utf8"));
}

/** Mirrors the Hermes bridge: any explicit project selector targets a registered project. */
export function hasExplicitProjectSelector(args: Record<string, unknown>): boolean {
  const selector = args.project_selector;
  const selectorFields =
    typeof selector === "object" && selector !== null ? (selector as Record<string, unknown>) : {};
  return [
    args.project_id,
    args.project_path,
    selectorFields.path,
    selectorFields.project_path,
    selectorFields.project_id,
  ].some((value) => typeof value === "string" && value.length > 0);
}

/**
 * Run one catalog tool. A stopped daemon may be an operator hold, so this
 * passive client reports it as unavailable instead of starting one.
 */
export async function runCatalogTool(
  name: string,
  args: Record<string, unknown>,
  cwd: string,
  signal: AbortSignal | undefined,
  run: Runner = runCli,
): Promise<string> {
  const result = await run(["tool", name, "--args", "-"], {
    cwd,
    timeoutMs: TOOL_TIMEOUT_MS,
    signal,
    input: JSON.stringify(args),
  });
  if (result.code === DAEMON_UNREACHABLE_EXIT_CODE) {
    throw new Error(`${name} unavailable: no TraceDecay daemon is serving. ${failureDetail(result)}`);
  }
  if (result.code !== 0) {
    throw new Error(`${name} failed: ${failureDetail(result)}`);
  }
  return truncateOutput((result.stdout || result.stderr).trim());
}

type ApprovalContext = Pick<ExtensionContext, "hasUI" | "ui">;

/**
 * Mutating tools run only with the operator's approval, and never against an
 * explicitly selected project: only catalog read-only tools may be routed at
 * another registered project, as in the Hermes bridge.
 */
export async function admitCatalogTool(
  schema: CatalogToolSchema,
  args: Record<string, unknown>,
  ctx: ApprovalContext,
): Promise<void> {
  if (schema.read_only) return;
  if (hasExplicitProjectSelector(args)) {
    throw new Error(`${schema.name} does not permit a cross-project mutating selector`);
  }
  if (!ctx.hasUI) {
    throw new Error(`${schema.name} mutates project state and needs interactive approval`);
  }
  const approved = await ctx.ui.confirm(
    `Allow ${schema.name}?`,
    `${schema.name} changes project state.\n\n${truncateOutput(JSON.stringify(args, null, 2))}`,
  );
  if (!approved) {
    throw new Error(`${schema.name} was not approved`);
  }
}

function toolLabel(name: string): string {
  return `TraceDecay ${name.slice("tracedecay_".length).replaceAll("_", " ")}`;
}

export function registerCatalogTools(
  pi: Pick<ExtensionAPI, "registerTool">,
  schemas: CatalogToolSchema[],
  run: Runner = runCli,
): void {
  for (const schema of schemas) {
    pi.registerTool({
      name: schema.name,
      label: toolLabel(schema.name),
      description: schema.description,
      parameters: schema.parameters as unknown as ToolDefinition["parameters"],
      async execute(_toolCallId, params, signal, _onUpdate, ctx) {
        const args = (params ?? {}) as Record<string, unknown>;
        await admitCatalogTool(schema, args, ctx);
        const text = await runCatalogTool(schema.name, args, ctx.cwd, signal, run);
        return { content: [{ type: "text" as const, text }], details: { tool: schema.name } };
      },
    });
  }
}

// ---------------------------------------------------------------------------
// Lifecycle hooks
// ---------------------------------------------------------------------------

export type PiLifecycleEvent = "session_start" | "agent_end";

export function lifecyclePayload(
  event: PiLifecycleEvent,
  sessionId: string,
  cwd: string,
  reason?: string,
): Record<string, string> {
  return {
    hook_event_name: event,
    id: randomUUID(),
    session_id: sessionId,
    cwd,
    ...(reason === undefined ? {} : { reason }),
  };
}

/** Deliver one lifecycle event to `hook-pi-event`; resolves to its guidance. */
export async function dispatchLifecycle(
  payload: Record<string, string>,
  run: Runner = runCli,
): Promise<string | undefined> {
  const result = await run(["hook-pi-event"], {
    cwd: payload.cwd,
    timeoutMs: HOOK_TIMEOUT_MS,
    input: JSON.stringify(payload),
  });
  const guidance = result.stdout.trim();
  return result.code === 0 && guidance.length > 0 ? guidance : undefined;
}

// ---------------------------------------------------------------------------
// Extension factory
// ---------------------------------------------------------------------------

export default function tracedecayExtension(pi: ExtensionAPI) {
  void TD_EXTENSION_MARKER;
  registerCatalogTools(pi, loadToolSchemas());

  let lastSyncAt = 0;

  pi.on("session_start", (event, ctx) => {
    const payload = lifecyclePayload(
      "session_start",
      ctx.sessionManager.getSessionId(),
      ctx.cwd,
      event.reason,
    );
    void dispatchLifecycle(payload);
  });

  pi.on("tool_result", (event, ctx) => {
    // Keep the index current after mutating tool calls, debounced.
    if (event.toolName !== "bash" && event.toolName !== "edit" && event.toolName !== "write") {
      return;
    }
    if (event.isError) return;
    const now = Date.now();
    if (now - lastSyncAt < SYNC_DEBOUNCE_MS) return;
    lastSyncAt = now;
    void runCli(["sync"], { cwd: ctx.cwd, timeoutMs: SYNC_TIMEOUT_MS });
  });

  pi.on("agent_end", (_event, ctx) => {
    const payload = lifecyclePayload("agent_end", ctx.sessionManager.getSessionId(), ctx.cwd);
    void dispatchLifecycle(payload).then((guidance) => {
      if (guidance) ctx.ui.notify(`tracedecay: ${guidance}`, "info");
    });
  });

  // --- Slash commands ------------------------------------------------------

  pi.registerCommand("tracedecay", {
    description: "Show tracedecay project status (graph counts, freshness)",
    handler: async (_args, ctx) => {
      const result = await runCli(["status", "--json"], {
        cwd: ctx.cwd,
        timeoutMs: TOOL_TIMEOUT_MS,
        signal: ctx.signal ?? undefined,
      });
      if (result.code !== 0) {
        ctx.ui.notify(`tracedecay status failed: ${failureDetail(result)}`, "error");
        return;
      }
      let parsed: {
        project_root?: string;
        graph_statistics?: { state?: string; reason?: string };
        project_open?: { state?: string };
      };
      try {
        parsed = JSON.parse(result.stdout);
      } catch {
        ctx.ui.notify(truncateOutput(result.stdout.trim()), "info");
        return;
      }
      const graph = parsed.graph_statistics;
      const lines = [
        `project: ${parsed.project_root ?? "unreported"}`,
        `graph: ${graph?.state ?? "unreported"}${graph?.reason ? ` (${graph.reason})` : ""}`,
        `open: ${parsed.project_open?.state ?? "unreported"}`,
      ].join(" · ");
      ctx.ui.notify(`tracedecay ${lines}`, graph?.state === "unavailable" ? "error" : "info");
    },
  });

  pi.registerCommand("tracedecay-sync", {
    description: "Run an incremental tracedecay sync for the current project",
    handler: async (_args, ctx) => {
      const result = await runCli(["sync"], {
        cwd: ctx.cwd,
        timeoutMs: SYNC_TIMEOUT_MS,
        signal: ctx.signal ?? undefined,
      });
      if (result.code === 0) {
        ctx.ui.notify("tracedecay sync complete", "info");
      } else {
        ctx.ui.notify(`tracedecay sync failed: ${failureDetail(result)}`, "error");
      }
    },
  });

  pi.registerCommand("tracedecay-version", {
    description: "Show the tracedecay binary version",
    handler: async (_args, ctx) => {
      const result = await runCli(["--version"], { cwd: ctx.cwd, timeoutMs: 10_000 });
      ctx.ui.notify(
        result.code === 0 ? result.stdout.trim() : `tracedecay: ${failureDetail(result)}`,
        result.code === 0 ? "info" : "error",
      );
    },
  });
}
