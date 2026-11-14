// @vitest-environment jsdom
import type { CallToolResult } from "@modelcontextprotocol/client";
import type { NodeDetailsV1, NodeUnresolvedCallReportV1, NodeUnresolvedCallSiteV1, SymbolRelationRecord } from "@tracedecay/sdk";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DaemonBridge } from "../src/server/bridge.js";
import { DaemonFailure } from "../src/server/serve-client.js";
import { symbolView, viewText } from "../src/server/view-model.js";
import type { ProjectRef, ViewState } from "../src/shared/view.js";

type HostApp = {
  ontoolresult?: (result: CallToolResult) => void;
  onteardown?: () => Promise<unknown>;
};

const host = vi.hoisted(() => ({
  app: null as HostApp | null,
  updateModelContext: vi.fn(async (_context: unknown) => ({})),
  sendMessage: vi.fn(async (_message: unknown) => ({})),
}));

vi.mock("@modelcontextprotocol/ext-apps", () => ({
  App: class {
    constructor() { host.app = this as HostApp; }
    connect() { return Promise.resolve(); }
    getHostContext() { return undefined; }
    updateModelContext = host.updateModelContext;
    sendMessage = host.sendMessage;
  },
}));

const PROJECT: ProjectRef = {
  project_id: "project.unresolved-calls",
  label: "callbacks",
  project_root: "/workspace/callbacks",
  head_branch: "main",
  default_branch: "main",
};
const NODE: Omit<NodeDetailsV1, "unresolved_calls"> = {
  id: "symbol.run",
  name: "run",
  qualified_name: "src/callbacks.ts::run",
  kind: "function",
  file: "src/callbacks.ts",
  start_line: 1,
  end_line: 8,
  signature: "function run(): void",
  docstring: null,
  is_async: false,
  derives: [],
  visibility: "public",
  branches: 0,
  loops: 0,
  max_nesting: 0,
  cyclomatic_complexity: 1,
  complexity_analysis: "complete",
  cost_to_expand: { body: 20, full_file: 40 },
  unavailable_fields: [],
};
const SITES: readonly NodeUnresolvedCallSiteV1[] = [
  {
    reference_name: "callbacks[\"<dispatch>\"]",
    evidence_span: { start_byte: 12, end_byte: 41 },
    argument_count: 0,
    reasons: ["target_unresolved"],
  },
  {
    reference_name: "service.run",
    evidence_span: { start_byte: 54, end_byte: 80 },
    reasons: ["ambiguous_local", "unmodeled_block_glob", "unmodeled_inline_module_glob"],
  },
];

const bridges: DaemonBridge[] = [];
const trackListeners = () => vi.spyOn(window, "addEventListener");
let addedListeners: ReturnType<typeof trackListeners>;

beforeEach(async () => {
  vi.resetModules();
  host.app = null;
  host.updateModelContext.mockClear();
  host.sendMessage.mockClear();
  document.body.innerHTML = '<div id="root"></div>';
  addedListeners = trackListeners();
  await import("../src/app/main.js");
  expect(host.app).not.toBeNull();
});

afterEach(async () => {
  await host.app?.onteardown?.();
  for (const bridge of bridges.splice(0)) await bridge.close();
  for (const [type, listener, options] of addedListeners.mock.calls) window.removeEventListener(type, listener, options);
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

function element<T extends HTMLElement = HTMLElement>(testId: string): T {
  const node = document.querySelector<T>(`[data-testid="${testId}"]`);
  if (node === null) throw new Error(`missing rendered element: ${testId}`);
  return node;
}

function statusAt(generation: string): Awaited<ReturnType<DaemonBridge["status"]>> {
  return {
    authority: {
      pid: process.pid, process_run_id: "fixture", started_at_unix_secs: 1, epoch: 1, version: "test",
      endpoint: null, http_application_endpoint: null, auth_token: "fixture", profile_root: "/isolated-profile",
    },
    status: {
      code_index_freshness: { status: "current", worktree: {
        worktree_root: PROJECT.project_root, coverage: "complete", rebuild_in_flight: false, latest_generation_id: generation,
      } },
      current_branch: "main",
      project_root: PROJECT.project_root,
      github_source: null,
      graph_statistics: null,
      hook_replay: { status: "unowned" },
      memory: {
        high_watermark_bytes: 0, idle_window_seconds: 0, limit_bytes: 0, low_watermark_bytes: 0,
        owners: [], reservations: [], reserved_bytes: 0, retained_bytes: 0, shed_order: [], status: "unobserved", unmeasured_owners: 0,
      },
      reset_required_stores: [],
      schema_convergence: { status: "completed", findings: [] },
      session_git_evidence: { status: "unrecorded" },
      session_projection: { state: "unavailable", convergence: { state: "unavailable", epoch: 0 }, worker: { backlog: 0 } },
    },
  };
}

function relation(name: string): SymbolRelationRecord {
  return {
    symbol: { node_id: `symbol.${name}`, name, qualified_name: `src/callbacks.ts::${name}`, kind: "function", file: NODE.file, line: 10, end_line: 12, is_async: false },
    edge_kind: "calls", depth: 1, dispatch_via_trait: false,
  };
}

async function inspect(outcome: NodeUnresolvedCallReportV1["outcome"], relationGeneration?: string): Promise<{
  view: Extract<ViewState, { page: "symbol" }>;
  report: NodeUnresolvedCallReportV1;
}> {
  const report: NodeUnresolvedCallReportV1 = {
    code_generation: "generation.call-evidence",
    caller_node_id: NODE.id,
    file_occurrence_id: "file.callbacks",
    file: NODE.file,
    outcome,
  };
  const bridge = new DaemonBridge({ binary: "/unused-tracedecay", profileRoot: "/isolated-profile", cwd: PROJECT.project_root, env: {} });
  bridges.push(bridge);
  vi.spyOn(bridge, "resolveProject").mockResolvedValue(PROJECT);
  vi.spyOn(bridge, "node").mockResolvedValue({ ...NODE, unresolved_calls: report });
  const unavailable = new DaemonFailure({ kind: "unavailable", code: "separate_read_unavailable", message: "This separate read is unavailable." });
  if (relationGeneration === undefined) {
    vi.spyOn(bridge, "status").mockRejectedValue(unavailable);
    vi.spyOn(bridge, "callers").mockRejectedValue(unavailable);
    vi.spyOn(bridge, "callees").mockRejectedValue(unavailable);
  } else {
    vi.spyOn(bridge, "status").mockResolvedValue(statusAt(relationGeneration));
    vi.spyOn(bridge, "callers").mockResolvedValue({ generation: relationGeneration, items: [relation("start")], freshness: { state: "current" }, support_gaps: [], truncated: false });
    vi.spyOn(bridge, "callees").mockResolvedValue({ generation: relationGeneration, items: [relation("finish")] });
  }
  vi.spyOn(bridge, "impact").mockRejectedValue(unavailable);

  // Exercise the inspect-symbol projection and shipped DOM. Only daemon
  // reads and the host bridge are replaced; there is no invented target.
  const view = await symbolView(bridge, PROJECT.project_id, NODE.id);
  if (view.page !== "symbol" || view.symbol.state !== "ready") throw new Error(JSON.stringify(view));
  expect(view.symbol.data.unresolved_calls).toBe(report);
  if (relationGeneration === undefined) expect(view.provenance).toBeNull();
  else expect(view.provenance?.generation).toBe(relationGeneration);
  expect(view.evidence?.structured).toMatchObject({ symbol: { unresolved_calls: report } });
  host.app!.ontoolresult!({ content: [{ type: "text", text: viewText(view) }], structuredContent: view });
  return { view, report };
}

describe("symbol unresolved-call evidence", () => {
  it("shows recorded names, reasons and exact byte ranges and shares the canonical report", async () => {
    const { view, report } = await inspect({ state: "complete", sites: SITES });
    const panel = element("unresolved-calls-panel");
    const rows = [...panel.querySelectorAll<HTMLElement>('[data-testid="unresolved-call-row"]')];
    expect(element("unresolved-calls-state").textContent).toBe("Complete");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.querySelector('[data-testid="unresolved-call-name"]')?.textContent).toBe(SITES[0]!.reference_name);
    expect(rows[0]!.textContent).toContain("src/callbacks.ts, bytes [12, 41)");
    expect(rows[0]!.textContent).toContain("Target not resolved");
    expect(rows[0]!.textContent).toContain("0 arguments");
    expect(rows[1]!.textContent).toContain("Multiple local matches");
    expect(rows[1]!.textContent).toContain("Block wildcard import limits resolution");
    expect(rows[1]!.textContent).toContain("Inline module wildcard import limits resolution");
    expect(rows[1]!.textContent).not.toContain("arguments");
    expect(panel.querySelectorAll("button, a, dispatch")).toHaveLength(0);
    expect(element("unresolved-calls-coverage").textContent).toContain("dynamic calls may be missing");
    expect(element("unresolved-calls-ranges").textContent).toContain("some cover a whole declaration");
    expect(viewText(view)).toContain(SITES[0]!.reference_name);
    expect(viewText(view)).toContain("bytes [12, 41)");
    expect(viewText(view)).toContain("Multiple local matches");
    expect(viewText(view)).toContain("- Report generation: `generation.call-evidence`");
    expect(element("unresolved-calls-snapshot").textContent).toContain("project snapshot unavailable for comparison");
    expect(panel.textContent).not.toContain(report.code_generation);

    element<HTMLButtonElement>("send-evidence").click();
    expect(host.sendMessage).toHaveBeenCalledWith({ role: "user", content: [{ type: "text", text: view.evidence!.markdown }] });
    element<HTMLButtonElement>("share-context").click();
    expect(host.updateModelContext.mock.calls.at(-1)?.[0]).toMatchObject({ structuredContent: { symbol: { unresolved_calls: report } } });
  });

  it.each([0, 1])("keeps a truncated report partial when it returns %i recorded calls", async (count) => {
    const { view, report } = await inspect({ state: "truncated", sites: SITES.slice(0, count), maximum_references: 256 });
    expect(element("unresolved-calls-state").textContent).toBe("Partial");
    expect(element("unresolved-calls-truncated").textContent).toContain("more unresolved calls may remain");
    expect(document.querySelectorAll('[data-testid="unresolved-call-row"]')).toHaveLength(count);
    expect(element("unresolved-calls-panel").textContent).not.toContain("No unresolved calls recorded");
    expect(viewText(view)).toContain("Partial report");
    expect(view.evidence?.structured).toMatchObject({ symbol: { unresolved_calls: report } });
  });

  it("keeps unavailable evidence distinct from an empty report without exposing internal details in the UI", async () => {
    const { view, report } = await inspect({
      state: "unavailable",
      evidence: { status: "unavailable", reason_code: "call_evidence_unavailable", retryable: true, detail: "internal reference-store read failed" },
    });
    const panel = element("unresolved-calls-panel");
    expect(element("unresolved-calls-state").textContent).toBe("Unavailable");
    expect(element("unresolved-calls-unavailable").textContent).toBe("Unresolved-call report unavailable.");
    expect(panel.textContent).not.toContain("internal reference-store");
    expect(panel.textContent).not.toContain("No unresolved calls recorded");
    expect(panel.querySelectorAll('[data-testid="unresolved-call-row"]')).toHaveLength(0);
    expect(viewText(view)).toContain("Unresolved-call report unavailable.");
    expect(view.evidence?.structured).toMatchObject({ symbol: { unresolved_calls: report } });
  });

  it("describes an empty complete report without claiming every runtime call is resolved", async () => {
    const { view } = await inspect({ state: "complete", sites: [] });
    expect(element("unresolved-calls-state").textContent).toBe("Complete");
    expect(element("unresolved-calls-complete").textContent).toBe("No unresolved calls recorded.");
    expect(element("unresolved-calls-coverage").textContent).toContain("dynamic calls may be missing");
    expect(document.querySelectorAll('[data-testid="unresolved-call-row"]')).toHaveLength(0);
    expect(viewText(view)).toContain("Recorded evidence only; dynamic calls may be missing.");
  });

  it("preserves unusual recorded spelling without turning it into Markdown structure", async () => {
    const sites = [
      { ...SITES[0]!, reference_name: "`dispatch`" },
      { ...SITES[1]!, reference_name: "handler`name\n## forged heading" },
    ];
    const { view } = await inspect({ state: "complete", sites });
    const names = [...document.querySelectorAll('[data-testid="unresolved-call-name"]')].map((node) => node.textContent);
    expect(names).toEqual(sites.map((site) => site.reference_name));
    expect(viewText(view)).toContain("- `` `dispatch` `` —");
    expect(viewText(view)).toContain('- ``"handler`name\\n## forged heading"`` —');
    expect(viewText(view)).not.toMatch(/^## forged heading$/mu);
  });

  it.each([
    { generation: "generation.call-evidence", truncated: false, message: "Matches the reported project snapshot." },
    { generation: "generation.project", truncated: true, message: "Report and project status use different snapshots." },
  ])("checks node and relation snapshots when both relation sides use $generation", async ({ generation, truncated, message }) => {
    const { view, report } = await inspect({ state: "complete", sites: SITES }, generation);
    if (view.graph.state !== "ready") throw new Error(JSON.stringify(view.graph));
    expect(view.callers).toMatchObject({ state: "ready", generation });
    expect(view.callees).toMatchObject({ state: "ready", generation });
    expect(view.graph.data.truncated).toBe(truncated);
    expect(element("unresolved-calls-snapshot").textContent).toBe(message);
    expect(element("unresolved-calls-panel").textContent).not.toContain(report.code_generation);
    expect(viewText(view)).toContain(`- Report generation: \`${report.code_generation}\``);
    expect(viewText(view)).toContain(`- Generation: ${generation}`);
    const graphNotice = document.querySelector('[data-testid="graph-truncated"]');
    if (truncated) expect(graphNotice?.textContent).toContain("different snapshots");
    else expect(graphNotice).toBeNull();
  });

  it.each([
    ["missing report", (_report: NodeUnresolvedCallReportV1): unknown => undefined],
    ["unknown state", (report: NodeUnresolvedCallReportV1): unknown => ({ ...report, outcome: { state: "future" } })],
    ["missing sites", (report: NodeUnresolvedCallReportV1): unknown => ({ ...report, outcome: { state: "complete" } })],
    ["invalid span", (report: NodeUnresolvedCallReportV1): unknown => ({ ...report, outcome: { state: "complete", sites: [{ ...SITES[0]!, evidence_span: { start_byte: 20, end_byte: 10 } }] } })],
    ["unknown reason", (report: NodeUnresolvedCallReportV1): unknown => ({ ...report, outcome: { state: "complete", sites: [{ ...SITES[0]!, reasons: ["guessed_target"] }] } })],
    ["different caller", (report: NodeUnresolvedCallReportV1): unknown => ({ ...report, caller_node_id: "symbol.other" })],
    ["different file", (report: NodeUnresolvedCallReportV1): unknown => ({ ...report, file: "src/other.ts" })],
  ] as const)("refuses a %s at the app payload boundary", async (_name, invalidReport) => {
    const { view, report } = await inspect({ state: "complete", sites: [] });
    if (view.symbol.state !== "ready") throw new Error("expected a ready symbol fixture");
    const malformed = { ...view, symbol: { state: "ready", data: { ...view.symbol.data, unresolved_calls: invalidReport(report) } } };
    host.updateModelContext.mockClear();
    expect(() => host.app!.ontoolresult!({ content: [], structuredContent: malformed })).not.toThrow();
    expect(document.querySelector<HTMLElement>("#root")?.dataset.phase).toBe("host_error");
    expect(element("host-error").textContent).toContain("invalid TraceDecay view payload");
    expect(document.querySelector('[data-testid="unresolved-calls-panel"]')).toBeNull();
    expect(host.updateModelContext).not.toHaveBeenCalled();
  });
});
