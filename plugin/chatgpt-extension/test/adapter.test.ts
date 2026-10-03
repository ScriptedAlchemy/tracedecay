import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { mkdtemp, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import type { DaemonBridge } from "../src/server/bridge.js";
import { createExtensionServer } from "../src/server/main.js";
import { DaemonFailure, ServeSession } from "../src/server/serve-client.js";

const PACKAGE_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const FAKE_SERVE = path.join(PACKAGE_ROOT, "test", "fake-serve.mjs");
const ASSETS = { html: "<!doctype html><html></html>", iconSvg: "<svg xmlns='http://www.w3.org/2000/svg'/>" };

const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

function fakeSession(): ServeSession {
  const session = new ServeSession({ binary: FAKE_SERVE, projectRoot: null, env: process.env, cwd: PACKAGE_ROOT });
  cleanups.push(() => session.close());
  return session;
}

async function rejection(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  throw new Error("expected the call to reject");
}

describe("embedded server entry", () => {
  it("serves when launched through a symlinked path", async () => {
    const dir = await mkdtemp(path.join(tmpdir(), "td-explorer-link-"));
    cleanups.push(() => rm(dir, { recursive: true, force: true }));
    const link = path.join(dir, "server.mjs");
    await symlink(path.join(PACKAGE_ROOT, "embedded", "server.mjs"), link);

    const client = new Client({ name: "symlink-test", version: "0.0.0" });
    await client.connect(new StdioClientTransport({ command: process.execPath, args: [link], cwd: dir }));
    cleanups.push(() => client.close());

    expect(client.getServerVersion()?.name).toBe("tracedecay-chatgpt-extension");
    const tools = await client.listTools();
    expect(tools.tools.map((tool) => tool.name)).toContain("tracedecay_workspace");
  }, 30_000);
});

describe("serve session", () => {
  it("keeps serving while the daemon writes heavily to stderr", async () => {
    await expect(fakeSession().callJson("chatty", {})).resolves.toEqual({ ok: true });
  }, 15_000);

  it("raises the daemon's problem record when a tool result is an error", async () => {
    const error = await rejection(fakeSession().callJson("refuses_with_problem", {}));
    expect(error).toBeInstanceOf(DaemonFailure);
    expect((error as DaemonFailure).failure).toEqual({
      kind: "denied",
      code: "not_found_or_not_authorized/project_not_registered",
      message: "project proj_0000 is not registered",
    });
  });

  it("still decodes a typed outcome the daemon flags as an error", async () => {
    await expect(fakeSession().callJson("node_not_found", {})).resolves.toEqual({
      status: "not_found",
      reason_code: "node_not_found",
      node_id: "n1",
      message: "Node not found: n1",
    });
  });
});

describe("composer mentions", () => {
  const projects = [
    { project_id: "proj_a", label: "billing" },
    { project_id: "proj_b", label: "shipping" },
  ];

  async function mentions(symbolSearch: (projectId: string) => Promise<unknown>): Promise<CallToolResult> {
    const bridge = { listProjects: async () => projects, symbolSearch } as unknown as DaemonBridge;
    const server = createExtensionServer({ bridge, assets: ASSETS });
    const [clientSide, serverSide] = InMemoryTransport.createLinkedPair();
    await server.connect(serverSide);
    const client = new Client({ name: "mentions-test", version: "0.0.0" });
    await client.connect(clientSide);
    cleanups.push(async () => {
      await client.close();
      await server.close();
    });
    return (await client.callTool({ name: "search_mentions", arguments: { query: "render" } })) as CallToolResult;
  }

  it("reports failed lookups instead of an empty match list", async () => {
    const result = await mentions(async (projectId) => {
      throw new DaemonFailure({ kind: "unavailable", code: "index_warming", message: `${projectId} is warming` });
    });
    expect(result.isError).toBe(true);
    const text = JSON.stringify(result.content);
    expect(text).toContain("billing: proj_a is warming");
    expect(text).toContain("shipping: proj_b is warming");
  });

  it("returns an empty match list when every lookup succeeds without matches", async () => {
    const result = await mentions(async () => ({ items: [] }));
    expect(result.isError ?? false).toBe(false);
    expect(result.structuredContent).toEqual({ items: [] });
  });
});
