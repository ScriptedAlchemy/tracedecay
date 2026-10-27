import { Client, InMemoryTransport, type CallToolResult } from "@modelcontextprotocol/client";
import {
  TraceDecayAbortError,
  TraceDecayMalformedResponseError,
  type OperationApplicationProjectListResult,
  type PublicCodeProject,
} from "@tracedecay/sdk";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DaemonAuthorityRecord } from "../src/server/authority.js";
import { DaemonBridge } from "../src/server/bridge.js";
import { createExtensionServer } from "../src/server/main.js";
import { RegistryClient } from "../src/server/registry-client.js";
import { DaemonFailure, ServeSession } from "../src/server/serve-client.js";
import { projectsView, viewText } from "../src/server/view-model.js";
import type { ProjectListing, ViewState } from "../src/shared/view.js";

const ASSETS = { html: "<!doctype html><html></html>", iconSvg: "<svg xmlns='http://www.w3.org/2000/svg'/>" };
const PROJECT: PublicCodeProject = {
  project_id: "proj_older",
  label: "older-billing",
  project_root: "/isolated-profile/older-billing",
  canonical_root: "/isolated-profile/older-billing",
  display_root: "/isolated-profile/older-billing",
  created_at: 1,
  last_seen_at: 2,
  head_branch: "main",
  default_branch: "main",
};
const AUTHORITY: DaemonAuthorityRecord = {
  pid: process.pid,
  process_run_id: "isolated-registry-session",
  started_at_unix_secs: 1,
  epoch: 1,
  version: "test",
  endpoint: null,
  http_application_endpoint: null,
  auth_token: "isolated-fixture-token",
  profile_root: "/isolated-profile",
};
const cleanups: Array<() => Promise<void>> = [];

afterEach(async () => {
  try {
    for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
  } finally {
    vi.restoreAllMocks();
  }
});

function listing(overrides: Partial<OperationApplicationProjectListResult> = {}): OperationApplicationProjectListResult {
  return {
    status: "ok",
    title: "Registered projects",
    registry_path: "/isolated-profile/global.db",
    limit: 10,
    truncated: false,
    summary: { project_count: 1, repo_count: 1, truncated: false },
    project_tree: [],
    projects: [PROJECT],
    ...overrides,
  };
}

function bridge(): DaemonBridge {
  const value = new DaemonBridge({ binary: "/unused-tracedecay", profileRoot: AUTHORITY.profile_root, cwd: AUTHORITY.profile_root, env: {} });
  vi.spyOn(value, "authority").mockResolvedValue({ kind: "available", record: AUTHORITY });
  vi.spyOn(value, "daemonState").mockResolvedValue({ state: "connected", pid: AUTHORITY.pid, version: AUTHORITY.version, profile_root: AUTHORITY.profile_root });
  cleanups.push(() => value.close());
  return value;
}

async function client(via: DaemonBridge): Promise<Client> {
  const server = createExtensionServer({ bridge: via, assets: ASSETS });
  const [clientSide, serverSide] = InMemoryTransport.createLinkedPair();
  await server.connect(serverSide);
  const value = new Client({ name: "project-discovery-test", version: "0.0.0" });
  await value.connect(clientSide);
  cleanups.push(async () => {
    await value.close();
    await server.close();
  });
  return value;
}

function projectView(result: CallToolResult): Extract<ViewState, { page: "projects" }> {
  expect(result.isError ?? false, JSON.stringify(result)).toBe(false);
  const value = result.structuredContent as ViewState;
  if (value?.page !== "projects") throw new Error(`expected project view: ${JSON.stringify(result)}`);
  return value;
}

function ready(view: Extract<ViewState, { page: "projects" }>): ProjectListing {
  if (view.projects.state !== "ready") throw new Error(`expected project listing: ${JSON.stringify(view)}`);
  return view.projects.data;
}

describe("canonical project discovery", () => {
  it("uses the generated project-search binding and preserves the registry result", async () => {
    const response = listing({ query: "older-billing", truncated: true, summary: { project_count: 1, repo_count: 1, truncated: true } });
    const call = vi.fn().mockResolvedValue(response);
    const registry = new RegistryClient(call);
    const controller = new AbortController();
    await expect(registry.projectSearch({ query: "older-billing", limit: 10 }, controller.signal)).resolves.toEqual(response);
    expect(call).toHaveBeenCalledExactlyOnceWith("tracedecay_project_search", { query: "older-billing", limit: 10 }, controller.signal);
  });

  it("rejects malformed search results through the generated decoder", async () => {
    const call = vi.fn().mockResolvedValue({ ...listing(), truncated: "unknown" });
    const registry = new RegistryClient(call);
    await expect(registry.projectSearch({ query: "older", limit: 10 })).rejects.toBeInstanceOf(TraceDecayMalformedResponseError);
  });

  it("keeps cancellation typed at the registry boundary", async () => {
    const reason = new Error("replaced project query");
    const controller = new AbortController();
    const registry = new RegistryClient(async (_name, _request, signal) => {
      expect(signal).toBe(controller.signal);
      controller.abort(reason);
      throw reason;
    });
    await expect(registry.projectSearch({ query: "older", limit: 10 }, controller.signal)).rejects.toMatchObject({
      name: TraceDecayAbortError.name,
      cause: reason,
    });
  });

  it("does not dispatch an already cancelled registry request and preserves a null reason", async () => {
    const call = vi.fn().mockResolvedValue(listing());
    const controller = new AbortController();
    controller.abort(null);
    await expect(new RegistryClient(call).projectSearch({ query: "older", limit: 10 }, controller.signal))
      .rejects.toMatchObject({ name: TraceDecayAbortError.name, cause: null });
    expect(call).not.toHaveBeenCalled();
  });

  it("rejects a late result from a registry caller that ignored cancellation", async () => {
    const controller = new AbortController();
    const registry = new RegistryClient(async () => {
      controller.abort(null);
      return listing();
    });
    await expect(registry.projectSearch({ query: "older", limit: 10 }, controller.signal))
      .rejects.toMatchObject({ name: TraceDecayAbortError.name, cause: null });
  });

  it("keeps recent-list completeness metadata through the bridge, tool, and text view", async () => {
    const response = listing({
      projects: Array.from({ length: 10 }, (_, index) => ({ ...PROJECT, project_id: `proj_${index}` })),
      truncated: true, summary: { project_count: 10, repo_count: 9, truncated: true },
    });
    const call = vi.spyOn(ServeSession.prototype, "callJson").mockResolvedValue(response);
    const mcp = await client(bridge());
    const result = await mcp.callTool({ name: "tracedecay_workspace", arguments: {} }) as CallToolResult;
    const view = projectView(result);
    expect(view.query).toBe("");
    expect(ready(view)).toMatchObject({
      limit: 10,
      truncated: true,
      summary: response.summary,
    });
    expect(ready(view).projects).toHaveLength(10);
    expect(ready(view).projects[0]).toMatchObject({ project_id: "proj_0", project_root: PROJECT.project_root });
    expect(call).toHaveBeenCalledWith("tracedecay_project_list", { limit: 10 }, expect.any(AbortSignal));
    expect(viewText(view)).toMatch(/more|partial|truncated/iu);
  });

  it("searches the registry with a bounded request and returns exact project identities", async () => {
    const response = listing({ query: "older-billing" });
    const call = vi.spyOn(ServeSession.prototype, "callJson").mockResolvedValue(response);
    const mcp = await client(bridge());
    const result = await mcp.callTool({ name: "tracedecay_search_projects", arguments: { query: "  older-billing  " } }) as CallToolResult;
    const view = projectView(result);
    expect(view.query).toBe("older-billing");
    expect(ready(view).projects).toEqual([{
      project_id: PROJECT.project_id, label: PROJECT.label, project_root: PROJECT.project_root,
      head_branch: "main", default_branch: "main",
    }]);
    expect(call).toHaveBeenCalledExactlyOnceWith("tracedecay_project_search", { query: "older-billing", limit: 10 }, expect.any(AbortSignal));
    expect(viewText(view)).toContain("older-billing");
    const advertised = (await mcp.listTools()).tools.find((tool) => tool.name === "tracedecay_search_projects");
    expect(advertised?._meta).toMatchObject({ ui: { visibility: ["app"] } });
    expect(advertised?.annotations?.readOnlyHint).toBe(true);
  });

  it("clearing a query returns the bounded recent list", async () => {
    const call = vi.spyOn(ServeSession.prototype, "callJson").mockResolvedValue(listing());
    const mcp = await client(bridge());
    const view = projectView(await mcp.callTool({ name: "tracedecay_search_projects", arguments: { query: "   " } }) as CallToolResult);
    expect(view.query).toBe("");
    expect(call).toHaveBeenCalledExactlyOnceWith("tracedecay_project_list", { limit: 10 }, expect.any(AbortSignal));
  });

  it("distinguishes an empty registry from no search matches", async () => {
    vi.spyOn(ServeSession.prototype, "callJson").mockResolvedValue(listing({
      projects: [], summary: { project_count: 0, repo_count: 0, truncated: false },
    }));
    const via = bridge();
    const recent = await projectsView(via);
    const searched = await projectsView(via, undefined, "missing-project");
    expect(recent).toMatchObject({ page: "projects", query: "", projects: { state: "empty" } });
    expect(searched).toMatchObject({ page: "projects", query: "missing-project", projects: { state: "empty" } });
    expect(viewText(recent)).toMatch(/No projects are registered/u);
    expect(viewText(searched)).toMatch(/No .*match/iu);
    expect(viewText(searched)).not.toContain("tracedecay init");
  });

  it("retains truncated search metadata and says the matches are partial", async () => {
    const response = listing({
      projects: Array.from({ length: 10 }, (_, index) => ({ ...PROJECT, project_id: `proj_${index}` })),
      query: "billing", truncated: true, summary: { project_count: 10, repo_count: 8, truncated: true },
    });
    vi.spyOn(ServeSession.prototype, "callJson").mockResolvedValue(response);
    const view = await projectsView(bridge(), undefined, "billing");
    if (view.page !== "projects") throw new Error("expected projects");
    expect(ready(view)).toMatchObject({ limit: 10, truncated: true, summary: response.summary });
    expect(viewText(view)).toMatch(/more|partial|truncated/iu);
  });

  it("keeps registry failure distinct from an empty search", async () => {
    const failure = { kind: "unavailable" as const, code: "registry_unavailable", message: "Registry is unavailable" };
    vi.spyOn(ServeSession.prototype, "callJson").mockRejectedValue(new DaemonFailure(failure));
    const view = await projectsView(bridge(), undefined, "billing");
    expect(view).toMatchObject({ page: "projects", query: "billing", projects: { state: "failed", failure } });
    expect(viewText(view)).toContain(failure.message);
    expect(viewText(view)).not.toMatch(/No .*match/iu);
  });

  it("does not dispatch a search when profile authority denies admission", async () => {
    const via = bridge();
    const failure = { kind: "disconnected" as const, code: "daemon_authority_absent", message: "No daemon authority exists" };
    vi.spyOn(via, "authority").mockResolvedValue({ kind: "absent", failure });
    const call = vi.spyOn(ServeSession.prototype, "callJson");
    await expect(via.searchProjects("billing")).rejects.toMatchObject({ failure });
    expect(call).not.toHaveBeenCalled();
  });

  it("forwards host cancellation to the pending registry request", async () => {
    let resolveEntered!: () => void;
    const entered = new Promise<void>((resolve) => { resolveEntered = resolve; });
    let registrySignal: AbortSignal | undefined;
    const call = vi.spyOn(ServeSession.prototype, "callJson").mockImplementation(async (_name, _request, signal) => {
      registrySignal = signal;
      return await new Promise((_resolve, reject) => {
        if (signal === undefined) throw new Error("registry request lost its cancellation signal");
        signal.addEventListener("abort", () => reject(signal.reason), { once: true });
        resolveEntered();
      });
    });
    const mcp = await client(bridge());
    const controller = new AbortController();
    const request = mcp.callTool({ name: "tracedecay_search_projects", arguments: { query: "older" } }, { signal: controller.signal });
    // Observe rejection before abort so an immediately delivered cancellation
    // cannot become an unhandled rejection while the test checks downstream.
    const outcome = Promise.allSettled([request]);
    await Promise.race([
      entered,
      request.then(() => { throw new Error("search completed without entering the registry request"); }),
    ]);
    controller.abort(new Error("superseded by another project query"));
    expect(await outcome).toMatchObject([{ status: "rejected" }]);
    await expect.poll(() => registrySignal?.aborted).toBe(true);
    expect(call).toHaveBeenCalledExactlyOnceWith("tracedecay_project_search", { query: "older", limit: 10 }, registrySignal);
  });
});
