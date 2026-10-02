import { createServer, type Server } from "node:http";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { createRsbuild } from "@rsbuild/core";
import { DaemonBridge } from "../../src/server/bridge.js";
import { createExtensionServer, type ServerAssets } from "../../src/server/main.js";
import { deepLinkPath, type ViewState } from "../../src/shared/view.js";
import { BILLING_SOURCE, DaemonFixture, INDEX_SOURCE, REPORT_SOURCE, type FixtureRepo } from "../fixture.js";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const screenshots = path.join(packageRoot, "test-results", "screenshots");

let fixture: DaemonFixture;
let billingProjectId: string;
let billing: FixtureRepo;
let bridge: DaemonBridge;
let client: Client;
let assets: ServerAssets;
let staticServer: Server;
let baseUrl: string;

test.beforeAll(async () => {
  assets = {
    html: await readFile(path.join(packageRoot, "embedded", "app.html"), "utf8"),
    iconSvg: await readFile(path.join(packageRoot, "assets", "icon.svg"), "utf8"),
  };
  fixture = await DaemonFixture.create();
  await fixture.startDaemon();
  billing = await fixture.createRepo("billing", {
    "src/billing.ts": BILLING_SOURCE,
    "src/report.ts": REPORT_SOURCE,
    "src/index.ts": INDEX_SOURCE,
  });
  fixture.initProject(billing.root);
  bridge = new DaemonBridge({ binary: fixture.binary, profileRoot: fixture.profileRoot, env: fixture.env, cwd: fixture.home });
  client = await connectClient(bridge);
  const project = (await bridge.listProjects()).find((candidate) => candidate.project_root === billing.root);
  if (project === undefined) throw new Error("billing project was not registered");
  billingProjectId = project.project_id;

  const harnessDist = path.join(packageRoot, "test-results", "harness");
  const rsbuild = await createRsbuild({
    cwd: packageRoot,
    rsbuildConfig: {
      source: { entry: { harness: "./test/browser/harness.ts" } },
      html: { template: "./test/browser/harness.html", inject: "body" },
      output: { distPath: { root: harnessDist }, filenameHash: false, inlineScripts: true, sourceMap: { js: false } },
      performance: { chunkSplit: { strategy: "all-in-one" } },
      tools: { rspack: { output: { asyncChunks: false } } },
      logLevel: "warn",
    },
  });
  await rsbuild.build();
  const harnessHtml = await readFile(path.join(harnessDist, "harness.html"), "utf8");

  staticServer = createServer((request, response) => {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    if (url.pathname === "/harness.html") {
      response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      response.end(harnessHtml);
    } else if (url.pathname === "/app.html") {
      response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      response.end(assets.html);
    } else {
      response.writeHead(404);
      response.end();
    }
  });
  await new Promise<void>((resolve) => staticServer.listen(0, "127.0.0.1", resolve));
  const address = staticServer.address();
  if (typeof address !== "object" || address === null) throw new Error("static server did not bind");
  baseUrl = `http://127.0.0.1:${address.port}`;
});

test.afterAll(async () => {
  await client?.close();
  await bridge?.close();
  if (staticServer !== undefined) await new Promise<void>((resolve) => staticServer.close(() => resolve()));
  await fixture?.destroy();
});

async function connectClient(viaBridge: DaemonBridge): Promise<Client> {
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  const server = createExtensionServer({ bridge: viaBridge, assets });
  await server.connect(serverTransport);
  const mcp = new Client({ name: "playwright-host", version: "0.0.0" });
  await mcp.connect(clientTransport);
  return mcp;
}

async function callTool(name: string, args: Record<string, unknown>, via: Client = client): Promise<CallToolResult> {
  return (await via.callTool({ name, arguments: args })) as CallToolResult;
}

async function openHost(page: Page): Promise<void> {
  page.on("pageerror", (error) => console.error("[page error]", error.message));
  page.on("console", (message) => {
    if (message.type() === "error" || message.type() === "warning") console.error(`[console ${message.type()}]`, message.text());
  });
  await page.exposeFunction("__mcpCall", (name: string, args: Record<string, unknown>) => callTool(name, args));
  await page.goto(`${baseUrl}/harness.html`);
  await page.evaluate(() => window.__host.ready);
}

function app(page: Page) {
  return page.frameLocator("#app");
}

async function pushToolResult(page: Page, result: CallToolResult): Promise<void> {
  await page.evaluate((payload) => window.__host.sendToolResult(payload), result);
}

test("global entrypoint: connect project → search → symbol → graph → evidence to chat", async ({ page }) => {
  await openHost(page);
  const ui = app(page);
  await expect(ui.getByTestId("loading")).toContainText("Waiting for TraceDecay");

  await pushToolResult(page, await callTool("tracedecay_workspace", {}));
  await expect(ui.getByTestId("daemon-badge")).toHaveText("connected");
  const projectRow = ui.getByTestId("project-row").filter({ hasText: billing.root });
  await expect(projectRow).toHaveCount(1);
  await page.screenshot({ path: path.join(screenshots, "01-projects.png"), fullPage: true });

  await projectRow.getByTestId("project-open").click();
  await expect(ui.getByTestId("crumb-project")).toBeVisible();
  await expect(ui.getByTestId("results-empty")).toContainText("Type a symbol name");

  await ui.getByTestId("search-input").fill("outstandingTotal");
  await ui.getByTestId("search-submit").click();
  const hit = ui.getByTestId("search-hit").filter({ hasText: "outstandingTotal" }).first();
  await expect(hit).toBeVisible();
  await expect(ui.getByTestId("freshness-badge")).toBeVisible();
  await expect(ui.getByTestId("provenance-commit")).toHaveText(billing.head());
  await page.screenshot({ path: path.join(screenshots, "02-search.png"), fullPage: true });

  await hit.getByTestId("symbol-link").click();
  await expect(ui.getByTestId("symbol-qualified-name")).toContainText("outstandingTotal");
  await expect(ui.getByTestId("callees-row").filter({ hasText: "computeTotal" })).toHaveCount(1);
  await expect(ui.getByTestId("callers-row").filter({ hasText: "renderBillingReport" })).toHaveCount(1);
  await expect(ui.getByTestId("graph-node")).toHaveCount(3);
  await expect(ui.getByTestId("coverage-badge")).toHaveText("full");
  await expect(ui.getByTestId("deep-link")).toContainText(`/projects/${billingProjectId}/symbols/`);
  await page.screenshot({ path: path.join(screenshots, "03-symbol.png"), fullPage: true });

  const modelContext = await page.evaluate(() => window.__host.record.modelContext);
  expect(modelContext.length).toBeGreaterThanOrEqual(2);
  const latest = JSON.stringify(modelContext.at(-1));
  expect(latest).toContain("outstandingTotal");
  expect(latest).toContain(billing.head());

  await ui.getByTestId("send-evidence").click();
  await expect.poll(async () => page.evaluate(() => window.__host.record.messages.length)).toBe(1);
  const message = JSON.stringify(await page.evaluate(() => window.__host.record.messages[0]));
  expect(message).toContain("outstandingTotal");
  expect(message).toContain(billing.head());

  await ui.getByTestId("toggle-display-mode").click();
  await expect.poll(async () => page.evaluate(() => window.__host.record.displayModes)).toEqual(["fullscreen"]);
  await expect(ui.getByTestId("toggle-display-mode")).toHaveText("Collapse");

  await ui.getByTestId("graph-node").filter({ hasText: "computeTotal" }).click();
  await expect(ui.getByTestId("symbol-qualified-name")).toContainText("computeTotal");
  await expect(ui.getByTestId("callers-row")).toHaveCount(1);
});

test("deep link from the host opens the symbol directly", async ({ page }) => {
  await openHost(page);
  const search = await callTool("tracedecay_search_code", { project_id: billingProjectId, query: "renderBillingReport" });
  const view = search.structuredContent as Extract<ViewState, { page: "search" }>;
  if (view.results.state !== "ready") throw new Error(JSON.stringify(view));
  const target = view.results.data.hits.find((item) => item.name === "renderBillingReport");
  if (target === undefined) throw new Error("renderBillingReport missing from search");

  await page.evaluate((url) => window.__host.sendDeepLink(url), deepLinkPath({ kind: "symbol", project_id: billingProjectId, node_id: target.node_id }));
  await expect(app(page).getByTestId("symbol-qualified-name")).toContainText("renderBillingReport");
  await expect(app(page).getByTestId("callees-row")).toHaveCount(2);
  await page.screenshot({ path: path.join(screenshots, "04-deep-link.png"), fullPage: true });
});

test("denied, not-found, and disconnected states render truthfully", async ({ page }) => {
  await openHost(page);
  const ui = app(page);

  await pushToolResult(page, await callTool("tracedecay_thread_panel", {}));
  await expect(ui.getByTestId("daemon-badge")).toHaveText("connected");
  await expect(ui.getByTestId("project-row").filter({ hasText: billing.root })).toHaveCount(1);

  await pushToolResult(page, await callTool("tracedecay_search_code", { project_id: "proj_not_registered", query: "anything" }));
  await expect(ui.getByTestId("page-failure-kind")).toHaveText("denied");
  await expect(ui.getByTestId("denied-help")).toBeVisible();
  await page.screenshot({ path: path.join(screenshots, "05-denied.png"), fullPage: true });

  await pushToolResult(page, await callTool("tracedecay_inspect_symbol", { project_id: billingProjectId, node_id: "symbol.v1.sha256:0000" }));
  await expect(ui.getByTestId("symbol-failed-kind")).toHaveText("not_found");
  await expect(ui.getByTestId("callers-failed-kind")).toBeVisible();

  const offline = new DaemonBridge({ binary: fixture.binary, profileRoot: path.join(fixture.root, "no-profile"), env: fixture.env, cwd: fixture.home });
  const offlineClient = await connectClient(offline);
  try {
    await pushToolResult(page, await callTool("tracedecay_workspace", {}, offlineClient));
    await expect(ui.getByTestId("daemon-state-kind")).toHaveText("disconnected");
    await expect(ui.getByTestId("projects-failed-kind")).toHaveText("disconnected");
    await page.screenshot({ path: path.join(screenshots, "06-disconnected.png"), fullPage: true });
  } finally {
    await offlineClient.close();
    await offline.close();
  }
});
