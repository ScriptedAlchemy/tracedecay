import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { readFile, stat, writeFile } from "node:fs/promises";
import { request as httpRequest } from "node:http";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { DaemonBridge } from "../src/server/bridge.js";
import { createExtensionServer, serveLoopbackHttp } from "../src/server/main.js";
import { UI_RESOURCE_URI } from "../src/server/register.js";
import { isViewState, type ViewState } from "../src/shared/view.js";
import {
  BILLING_SOURCE,
  DaemonFixture,
  INDEX_SOURCE,
  REPORT_SOURCE,
  SECOND_PROJECT_SOURCE,
  waitFor,
  type FixtureRepo,
} from "./fixture.js";

const ASSETS = { html: "<!doctype html><html><body data-test-asset>app</body></html>", iconSvg: "<svg xmlns='http://www.w3.org/2000/svg'/>" };

let fixture: DaemonFixture;
let billing: FixtureRepo;
let shipping: FixtureRepo;
let bridge: DaemonBridge;
let client: Client;
let billingProjectId: string;
let shippingProjectId: string;

function view(result: CallToolResult): ViewState {
  expect(result.isError ?? false).toBe(false);
  const structured = result.structuredContent;
  if (!isViewState(structured)) throw new Error(`tool returned no view state: ${JSON.stringify(result)}`);
  return structured;
}

async function call(name: string, args: Record<string, unknown> = {}, via: Client = client): Promise<ViewState> {
  return view((await via.callTool({ name, arguments: args })) as CallToolResult);
}

function rawPost(url: string, hostHeader: string, body: string, authorization?: string): Promise<{ status: number; body: string }> {
  const target = new URL(url);
  return new Promise((resolve, reject) => {
    const request = httpRequest(
      {
        host: target.hostname,
        port: target.port,
        path: target.pathname,
        method: "POST",
        headers: {
          host: hostHeader,
          "content-type": "application/json",
          accept: "application/json, text/event-stream",
          "content-length": Buffer.byteLength(body),
          ...(authorization === undefined ? {} : { authorization }),
        },
      },
      (response) => {
        let text = "";
        response.setEncoding("utf8");
        response.on("data", (chunk: string) => (text += chunk));
        response.on("end", () => resolve({ status: response.statusCode ?? 0, body: text }));
      },
    );
    request.on("error", reject);
    request.end(body);
  });
}

async function newClient(viaBridge: DaemonBridge = bridge): Promise<Client> {
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  const server = createExtensionServer({ bridge: viaBridge, assets: ASSETS });
  await server.connect(serverTransport);
  const created = new Client({ name: "chatgpt-host-sim", version: "0.0.0" });
  await created.connect(clientTransport);
  return created;
}

beforeAll(async () => {
  fixture = await DaemonFixture.create();
  await fixture.startDaemon();
  billing = await fixture.createRepo("billing", {
    "src/billing.ts": BILLING_SOURCE,
    "src/report.ts": REPORT_SOURCE,
    "src/index.ts": INDEX_SOURCE,
  });
  shipping = await fixture.createRepo("shipping", { "src/shipping.ts": SECOND_PROJECT_SOURCE });
  fixture.initProject(billing.root);
  fixture.initProject(shipping.root);
  bridge = new DaemonBridge({ binary: fixture.binary, profileRoot: fixture.profileRoot, env: fixture.env, cwd: fixture.home });
  client = await newClient();
});

afterAll(async () => {
  await client?.close();
  await bridge?.close();
  await fixture?.destroy();
});

describe("ChatGPT extension against a live TraceDecay daemon", () => {
  it("initializes and advertises the shared UI resource with global and thread entrypoints", async () => {
    expect(client.getServerVersion()?.name).toBe("tracedecay-chatgpt-extension");
    const tools = await client.listTools();
    const byName = new Map(tools.tools.map((tool) => [tool.name, tool]));
    expect([...byName.keys()].sort()).toEqual([
      "search_mentions",
      "tracedecay_inspect_symbol",
      "tracedecay_list_projects",
      "tracedecay_search_code",
      "tracedecay_thread_panel",
      "tracedecay_workspace",
    ]);
    const workspace = byName.get("tracedecay_workspace")!;
    const thread = byName.get("tracedecay_thread_panel")!;
    expect(workspace._meta).toMatchObject({ ui: { resourceUri: UI_RESOURCE_URI }, "openai/ui": { entrypoints: [{ type: "global" }] } });
    expect(thread._meta).toMatchObject({ ui: { resourceUri: UI_RESOURCE_URI }, "openai/ui": { entrypoints: [{ type: "thread" }] } });
    expect(byName.get("tracedecay_list_projects")!._meta).toMatchObject({ ui: { visibility: ["app"] } });
    for (const tool of tools.tools) expect(tool.annotations?.readOnlyHint ?? tool.name === "search_mentions").toBeTruthy();

    const resources = await client.listResources();
    const ui = resources.resources.find((resource) => resource.uri === UI_RESOURCE_URI);
    expect(ui?.mimeType).toBe("text/html;profile=mcp-app");
    const read = await client.readResource({ uri: UI_RESOURCE_URI });
    expect(read.contents[0]).toMatchObject({
      mimeType: "text/html;profile=mcp-app",
      text: ASSETS.html,
      _meta: { "openai/ui": { availableDisplayModes: ["inline", "fullscreen"] } },
    });
  });

  it("runs the packaged embedded server over stdio exactly as plugin/mcp.json launches it", async () => {
    const packageRoot = path.resolve(import.meta.dirname, "..");
    const transport = new StdioClientTransport({
      command: process.execPath,
      args: [path.join(packageRoot, "embedded", "server.mjs")],
      env: Object.fromEntries(Object.entries(fixture.env).filter((entry): entry is [string, string] => typeof entry[1] === "string")),
      cwd: fixture.home,
      stderr: "pipe",
    });
    const packaged = new Client({ name: "packaged-host", version: "0.0.0" });
    await packaged.connect(transport);
    try {
      expect(packaged.getServerVersion()?.name).toBe("tracedecay-chatgpt-extension");
      const tools = await packaged.listTools();
      expect(tools.tools.map((tool) => tool.name).sort()).toEqual([
        "search_mentions",
        "tracedecay_inspect_symbol",
        "tracedecay_list_projects",
        "tracedecay_search_code",
        "tracedecay_thread_panel",
        "tracedecay_workspace",
      ]);
      const ui = await packaged.readResource({ uri: UI_RESOURCE_URI });
      const content = ui.contents[0]!;
      if (!("text" in content)) throw new Error("expected text resource");
      expect(content.text).toBe(await readFile(path.join(packageRoot, "embedded", "app.html"), "utf8"));
      expect(content.text).toContain("<script");
      const result = (await packaged.callTool({ name: "tracedecay_workspace", arguments: {} })) as CallToolResult;
      const state = result.structuredContent;
      if (!isViewState(state) || state.page !== "projects") throw new Error(JSON.stringify(state));
      expect(state.daemon.state).toBe("connected");
      if (state.projects.state !== "ready") throw new Error(JSON.stringify(state.projects));
      expect(state.projects.data.map((project) => project.project_root).sort()).toEqual([billing.root, shipping.root].sort());
    } finally {
      await packaged.close();
    }
  });

  it("lists only projects registered in the isolated profile", async () => {
    const state = await call("tracedecay_workspace");
    if (state.page !== "projects") throw new Error(`unexpected page ${state.page}`);
    expect(state.daemon.state).toBe("connected");
    if (state.projects.state !== "ready") throw new Error(`projects ${state.projects.state}: ${JSON.stringify(state.projects)}`);
    const roots = state.projects.data.map((project) => project.project_root).sort();
    expect(roots).toEqual([billing.root, shipping.root].sort());
    billingProjectId = state.projects.data.find((project) => project.project_root === billing.root)!.project_id;
    shippingProjectId = state.projects.data.find((project) => project.project_root === shipping.root)!.project_id;
    expect(billingProjectId).toMatch(/^proj_/u);
    expect(shippingProjectId).not.toBe(billingProjectId);
  });

  it("searches, inspects a symbol, and reports exact provenance (search → symbol → graph)", async () => {
    const search = await waitFor("search results for outstandingTotal", async () => {
      const state = await call("tracedecay_search_code", { project_id: billingProjectId, query: "outstandingTotal" });
      if (state.page !== "search") throw new Error(`unexpected page ${state.page}`);
      return state.results.state === "ready" ? state : null;
    });
    if (search.page !== "search" || search.results.state !== "ready") throw new Error("unreachable");
    const hit = search.results.data.hits.find((candidate) => candidate.qualified_name === "src/billing.ts::outstandingTotal");
    expect(hit, JSON.stringify(search.results.data.hits)).toBeDefined();
    expect(search.provenance).not.toBeNull();
    expect(search.provenance!.commit).toBe(billing.head());
    expect(search.provenance!.project_root).toBe(billing.root);
    expect(search.provenance!.generation).toMatch(/\S/u);
    expect(search.provenance!.freshness.state).toBe("current");
    expect(search.provenance!.coverage.recall).toBe("full");
    expect(search.provenance!.branch).toBe("main");

    const symbol = await call("tracedecay_inspect_symbol", { project_id: billingProjectId, node_id: hit!.node_id });
    if (symbol.page !== "symbol") throw new Error(`unexpected page ${symbol.page}: ${JSON.stringify(symbol)}`);
    if (symbol.symbol.state !== "ready") throw new Error(JSON.stringify(symbol.symbol));
    expect(symbol.symbol.data).toMatchObject({ name: "outstandingTotal", file: "src/billing.ts", start_line: 7, end_line: 9 });
    if (symbol.callers.state !== "ready") throw new Error(JSON.stringify(symbol.callers));
    expect(symbol.callers.data.map((item) => item.symbol.qualified_name)).toEqual(["src/report.ts::renderBillingReport"]);
    if (symbol.callees.state !== "ready") throw new Error(JSON.stringify(symbol.callees));
    expect(symbol.callees.data.map((item) => item.symbol.name)).toContain("computeTotal");
    if (symbol.impact.state !== "ready") throw new Error(JSON.stringify(symbol.impact));
    expect(symbol.impact.data.complete).toBe(true);
    expect(symbol.impact.data.nodes.map((node) => `${node.depth}:${node.name}`).sort()).toEqual(
      ["1:renderBillingReport", "2:main", "2:renderEmptyReport"].sort(),
    );
    if (symbol.graph.state !== "ready") throw new Error(JSON.stringify(symbol.graph));
    expect(symbol.graph.data.nodes.map((node) => node.role).sort()).toEqual(["callee", "caller", "focus"]);
    expect(symbol.graph.data.edges.length).toBe(2);
    expect(symbol.evidence).not.toBeNull();
    expect(symbol.evidence!.markdown).toContain(`- Commit: ${billing.head()}`);
    expect(symbol.evidence!.markdown).toContain("src/report.ts::renderBillingReport");
    expect(symbol.evidence!.markdown).toContain(`- Generation: ${search.provenance!.generation}`);
    expect(symbol.evidence!.structured).toMatchObject({ project: { project_id: billingProjectId } });
    const text = (await client.callTool({ name: "tracedecay_inspect_symbol", arguments: { project_id: billingProjectId, node_id: hit!.node_id } })) as CallToolResult;
    expect(text.content[0]).toMatchObject({ type: "text", text: symbol.evidence!.markdown });
  });

  it("denies unregistered project ids with a typed denied state", async () => {
    const state = await call("tracedecay_search_code", { project_id: "proj_0000000000000000", query: "anything" });
    expect(state).toMatchObject({ page: "failure", project: null, failure: { kind: "denied", code: "project_not_registered" } });
  });

  it("isolates projects: node ids from one project do not resolve in another", async () => {
    const search = await call("tracedecay_search_code", { project_id: billingProjectId, query: "computeTotal" });
    if (search.page !== "search" || search.results.state !== "ready") throw new Error(JSON.stringify(search));
    const foreign = search.results.data.hits[0]!.node_id;
    const state = await call("tracedecay_inspect_symbol", { project_id: shippingProjectId, node_id: foreign });
    if (state.page !== "symbol") throw new Error(JSON.stringify(state));
    expect(state.project.project_id).toBe(shippingProjectId);
    expect(state.symbol).toMatchObject({ state: "failed", failure: { kind: "not_found", code: "node_not_found" } });
    expect(state.provenance?.project_root).toBe(shipping.root);
    expect(state.evidence).toBeNull();
    const crossSearch = await waitFor("shipping project search", async () => {
      const result = await call("tracedecay_search_code", { project_id: shippingProjectId, query: "shippingQuote" });
      return result.page === "search" && result.results.state === "ready" ? result : null;
    });
    if (crossSearch.page !== "search" || crossSearch.results.state !== "ready") throw new Error("unreachable");
    expect(crossSearch.results.data.hits.some((hit) => hit.file.includes("billing"))).toBe(false);
    const shippingOnly = await call("tracedecay_search_code", { project_id: shippingProjectId, query: "outstandingTotal" });
    if (shippingOnly.page !== "search") throw new Error(JSON.stringify(shippingOnly));
    if (shippingOnly.results.state === "ready") {
      expect(shippingOnly.results.data.hits.map((hit) => hit.qualified_name)).not.toContain("src/billing.ts::outstandingTotal");
    } else {
      expect(shippingOnly.results.state).toBe("empty");
    }
  });

  it("reports a refreshed commit and new callers after the source changes", async () => {
    const before = billing.head();
    await writeFile(
      path.join(billing.root, "src/billing.ts"),
      `${BILLING_SOURCE}
export function taxTotal(invoices: Invoice[]): number {
  return Math.round(computeTotal(invoices) * 0.2);
}
`,
    );
    const after = fixture.commit(billing.root, "add taxTotal");
    expect(after).not.toBe(before);
    fixture.sync(billing.root);
    const search = await waitFor("taxTotal to be indexed", async () => {
      const state = await call("tracedecay_search_code", { project_id: billingProjectId, query: "taxTotal" });
      if (state.page !== "search" || state.results.state !== "ready") return null;
      return state.results.data.hits.some((hit) => hit.name === "taxTotal") && state.provenance?.commit === after ? state : null;
    });
    if (search.page !== "search" || search.results.state !== "ready") throw new Error("unreachable");
    expect(search.provenance!.commit).toBe(after);
    const compute = await call("tracedecay_search_code", { project_id: billingProjectId, query: "computeTotal" });
    if (compute.page !== "search" || compute.results.state !== "ready") throw new Error(JSON.stringify(compute));
    const computeHit = compute.results.data.hits.find((hit) => hit.name === "computeTotal")!;
    const symbol = await waitFor("computeTotal callers to include taxTotal", async () => {
      const state = await call("tracedecay_inspect_symbol", { project_id: billingProjectId, node_id: computeHit.node_id });
      if (state.page !== "symbol" || state.callers.state !== "ready") return null;
      return state.callers.data.some((item) => item.symbol.name === "taxTotal") ? state : null;
    });
    if (symbol.page !== "symbol" || symbol.callers.state !== "ready") throw new Error("unreachable");
    expect(symbol.callers.data.map((item) => item.symbol.name).sort()).toEqual(["outstandingTotal", "taxTotal"]);
    expect(symbol.provenance?.commit).toBe(after);
  });

  it("offers composer mentions as symbol resource links that read back as provenance markdown", async () => {
    const result = (await client.callTool({ name: "search_mentions", arguments: { query: "renderBilling" } })) as CallToolResult;
    const structured = result.structuredContent as { items: Array<{ type: string; uri: string; name: string }> };
    expect(structured.items.length).toBeGreaterThan(0);
    const item = structured.items.find((candidate) => candidate.name === "src/report.ts::renderBillingReport");
    expect(item, JSON.stringify(structured.items)).toBeDefined();
    expect(item!.type).toBe("resource_link");
    expect(item!.uri).toMatch(new RegExp(`^tracedecay://projects/${billingProjectId}/symbols/`, "u"));
    const read = await client.readResource({ uri: item!.uri });
    const content = read.contents[0]!;
    expect(content.mimeType).toBe("text/markdown");
    if (!("text" in content)) throw new Error("expected text resource");
    expect(content.text).toContain("## function `src/report.ts::renderBillingReport`");
    expect(content.text).toContain(`- Commit: ${billing.head()}`);
    expect(content._meta).toMatchObject({ "openai/deepLink": expect.stringContaining(`/projects/${billingProjectId}/symbols/`) });
  });

  it("serves the same server over loopback Streamable HTTP and rejects foreign Host headers", async () => {
    const createServer = () => createExtensionServer({ bridge, assets: ASSETS });
    const token = "test-loopback-token";
    const http = await serveLoopbackHttp(createServer, "127.0.0.1", 0, token);
    try {
      const transport = new StreamableHTTPClientTransport(new URL(http.url), {
        requestInit: { headers: { authorization: `Bearer ${token}` } },
      });
      const httpClient = new Client({ name: "http-host-sim", version: "0.0.0" });
      await httpClient.connect(transport);
      const tools = await httpClient.listTools();
      expect(tools.tools.map((tool) => tool.name)).toContain("tracedecay_workspace");
      const state = view((await httpClient.callTool({ name: "tracedecay_list_projects", arguments: {} })) as CallToolResult);
      expect(state.page).toBe("projects");
      await httpClient.close();
      const host = new URL(http.url).host;
      const missingAuth = await rawPost(http.url, host, JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping" }));
      expect(missingAuth.status).toBe(401);
      const wrongAuth = await rawPost(http.url, host, JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping" }), "Bearer wrong");
      expect(wrongAuth.status).toBe(401);
      const rebinding = await rawPost(http.url, "evil.example", JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }), `Bearer ${token}`);
      expect(rebinding.status).toBe(403);
      const sameOrigin = await rawPost(http.url, host, JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping" }), `Bearer ${token}`);
      expect(sameOrigin.status).toBe(200);
      await expect(serveLoopbackHttp(createServer, "0.0.0.0", 0, token)).rejects.toThrow(/loopback/u);
    } finally {
      await http.close();
    }
  });

  it("issues a generated loopback token through a 0600 file, never through stderr", async () => {
    const packageRoot = path.resolve(import.meta.dirname, "..");
    const child = spawn(
      process.execPath,
      [path.join(packageRoot, "embedded", "server.mjs"), "--http", "127.0.0.1:0"],
      {
        env: Object.fromEntries(
          Object.entries(fixture.env).filter((entry): entry is [string, string] => typeof entry[1] === "string"),
        ),
        cwd: fixture.home,
        stdio: ["ignore", "ignore", "pipe"],
      },
    );
    let stderr = "";
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString("utf8");
    });
    try {
      await waitFor("loopback token path on stderr", async () =>
        /bearer token written to .+ \(send as Authorization/u.test(stderr) ? stderr : null,
      );
      const url = /listening on (http:\/\/\S+)/u.exec(stderr)?.[1];
      const tokenPath = /bearer token written to (.+?) \(send/u.exec(stderr)?.[1];
      if (url === undefined || tokenPath === undefined) throw new Error(`unparsable server output: ${stderr}`);
      const token = (await readFile(tokenPath, "utf8")).trim();
      expect(token.length).toBeGreaterThan(0);
      expect(stderr).not.toContain(token);
      if (process.platform !== "win32") {
        expect((await stat(tokenPath)).mode & 0o777).toBe(0o600);
      }
      const ping = await rawPost(url, new URL(url).host, JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping" }), `Bearer ${token}`);
      expect(ping.status).toBe(200);
    } finally {
      child.kill("SIGTERM");
      await once(child, "exit");
    }
  });

  it("reports disconnected states truthfully: absent profile, unstartable serve binary, dead daemon", async () => {
    const emptyProfile = new DaemonBridge({ binary: fixture.binary, profileRoot: path.join(fixture.root, "no-such-profile"), env: fixture.env, cwd: fixture.home });
    const absent = await call("tracedecay_workspace", {}, await newClient(emptyProfile));
    expect(absent).toMatchObject({
      page: "projects",
      daemon: { state: "disconnected", failure: { kind: "disconnected", code: "daemon_authority_absent" } },
      projects: { state: "failed", failure: { kind: "disconnected", code: "daemon_authority_absent" } },
    });

    const missingBinary = new DaemonBridge({ binary: path.join(fixture.root, "missing-tracedecay"), profileRoot: fixture.profileRoot, env: fixture.env, cwd: fixture.home });
    const unstartable = await call("tracedecay_workspace", {}, await newClient(missingBinary));
    expect(unstartable).toMatchObject({
      page: "projects",
      daemon: { state: "connected" },
      projects: { state: "failed", failure: { kind: "disconnected", code: "serve_spawn_failed" } },
    });
    await missingBinary.close();

    const before = await fixture.readAuthority();
    await bridge.close();
    await fixture.stopDaemon();
    const stale = await fixture.readAuthority();
    expect(stale?.pid).toBe(before?.pid);
    const dead = await bridge.daemonState();
    expect(dead).toMatchObject({ state: "disconnected", failure: { kind: "disconnected", code: "daemon_exited" } });
    const direct = await call("tracedecay_inspect_symbol", { project_id: billingProjectId, node_id: "symbol.v1.sha256:0000" });
    if (direct.page === "failure") {
      expect(["disconnected", "unavailable"]).toContain(direct.failure.kind);
    } else {
      // `tracedecay serve` relaunched the daemon: the view must name the new process, not the dead one.
      if (direct.page !== "symbol") throw new Error(JSON.stringify(direct));
      expect(direct.provenance?.authority.daemon_pid).not.toBe(before?.pid);
      expect(direct.symbol).toMatchObject({ state: "failed", failure: { kind: "not_found" } });
    }
  });
});
