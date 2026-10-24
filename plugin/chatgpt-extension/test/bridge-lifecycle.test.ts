import { Client } from "@modelcontextprotocol/client";
import { StdioClientTransport } from "@modelcontextprotocol/client/stdio";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { setImmediate } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AuthorityLookup, DaemonAuthorityRecord } from "../src/server/authority.js";
import { DaemonBridge } from "../src/server/bridge.js";
import { RegistryClient } from "../src/server/registry-client.js";
import { ServeSession } from "../src/server/serve-client.js";

const PACKAGE_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const NODE_BINARY = process.env.JS_BINARY__NODE_BINARY ?? process.execPath;
const PROJECT_ID = "proj_fixture";
const CLOSED = { failure: expect.objectContaining({ kind: "unavailable", code: "bridge_closed" }) };
const cleanups: Array<() => Promise<void>> = [];

afterEach(async () => {
  try {
    for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
  } finally {
    vi.restoreAllMocks();
  }
});

function deferred<T = void>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((accept) => { resolve = accept; });
  return { promise, resolve };
}

async function fixture(expectedCloseError?: Error) {
  const root = await mkdtemp(path.join(tmpdir(), "td-bridge-lifecycle-"));
  const authority: DaemonAuthorityRecord = {
    pid: process.pid,
    process_run_id: "fixture-run-a",
    started_at_unix_secs: 1,
    epoch: 1,
    version: "fixture",
    endpoint: null,
    http_application_endpoint: "127.0.0.1:1",
    auth_token: "isolated-fixture-token",
    profile_root: root,
  };
  const writeAuthority = (record: DaemonAuthorityRecord) =>
    writeFile(path.join(root, "daemon-authority.json"), JSON.stringify(record));
  await writeAuthority(authority);
  const bridge = new DaemonBridge({
    binary: path.join(PACKAGE_ROOT, "test", "fake-serve.mjs"),
    cwd: root,
    profileRoot: root,
    env: {
      PATH: `${path.dirname(NODE_BINARY)}${path.delimiter}${process.env.PATH ?? ""}`,
      HOME: root,
      USERPROFILE: root,
      TMPDIR: root,
      TRACEDECAY_DATA_DIR: root,
      TRACEDECAY_TEST_PROJECT_ROOT: root,
    },
  });
  cleanups.push(async () => {
    try {
      if (expectedCloseError === undefined) await bridge.close();
      else await expect(bridge.close()).rejects.toBe(expectedCloseError);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
  return { bridge, root, authority, writeAuthority };
}

describe("bridge shutdown", () => {
  it("refuses every read after close without starting a child or an HTTP request", async () => {
    const { bridge } = await fixture();
    const start = vi.spyOn(StdioClientTransport.prototype, "start");
    const fetch = vi.spyOn(globalThis, "fetch");
    await bridge.close();
    const reads = [
      bridge.authority(), bridge.daemonState(), bridge.listProjects(), bridge.searchProjects("query"), bridge.resolveProject(PROJECT_ID),
      bridge.status(PROJECT_ID), bridge.search(PROJECT_ID, "query", 10), bridge.node(PROJECT_ID, "n1"),
      bridge.impact(PROJECT_ID, "n1", 1), bridge.callers(PROJECT_ID, "n1", 1),
      bridge.callees(PROJECT_ID, "n1", "generation", 1), bridge.symbolSearch(PROJECT_ID, "query"),
    ];
    const results = await Promise.allSettled(reads);
    expect(results).toEqual(reads.map(() => ({ status: "rejected", reason: expect.objectContaining(CLOSED) })));
    expect(start).not.toHaveBeenCalled();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("refuses a registry lookup that resumes after close has completed", async () => {
    const { bridge, authority } = await fixture();
    const entered = deferred();
    const lookup = deferred<AuthorityLookup>();
    vi.spyOn(bridge, "authority").mockImplementation(() => {
      entered.resolve();
      return lookup.promise;
    });
    const start = vi.spyOn(StdioClientTransport.prototype, "start");
    const read = bridge.listProjects();
    const result = Promise.allSettled([read]);
    await entered.promise;
    try {
      await bridge.close();
    } finally {
      lookup.resolve({ kind: "available", record: authority });
    }
    expect(await result).toEqual([{ status: "rejected", reason: expect.objectContaining(CLOSED) }]);
    expect(start).not.toHaveBeenCalled();
  });

  it("does not admit a project acquisition after a paused registry resolution", async () => {
    const { bridge, root } = await fixture();
    const entered = deferred();
    const resume = deferred();
    vi.spyOn(bridge, "resolveProject").mockImplementation(async () => {
      entered.resolve();
      await resume.promise;
      return { project_id: PROJECT_ID, label: "fixture", project_root: root, head_branch: null, default_branch: null };
    });
    const authority = vi.spyOn(bridge, "authority");
    const start = vi.spyOn(StdioClientTransport.prototype, "start");
    const result = Promise.allSettled([bridge.node(PROJECT_ID, "n1")]);
    await entered.promise;
    try {
      await bridge.close();
    } finally {
      resume.resolve();
    }
    expect(await result).toEqual([{ status: "rejected", reason: expect.objectContaining(CLOSED) }]);
    expect(authority).not.toHaveBeenCalled();
    expect(start).not.toHaveBeenCalled();
  });

  it("refuses HTTP dispatch when close starts after the project handle resolves", async () => {
    const { bridge, root, authority } = await fixture();
    vi.spyOn(bridge, "resolveProject").mockResolvedValue({
      project_id: PROJECT_ID, label: "fixture", project_root: root, head_branch: null, default_branch: null,
    });
    vi.spyOn(bridge, "authority").mockResolvedValue({ kind: "available", record: authority });
    const fetch = vi.spyOn(globalThis, "fetch").mockRejectedValue(new Error("HTTP dispatch escaped shutdown"));
    const signal = new AbortController().signal;
    const remove = signal.removeEventListener;
    let shutdown: Promise<void> | undefined;
    const removed = vi.spyOn(signal, "removeEventListener").mockImplementation(function (this: AbortSignal, ...args) {
      remove.apply(this, args);
      // withSignal has released its waiter; close before the public HTTP read resumes.
      queueMicrotask(() => { shutdown = bridge.close(); });
    });
    await expect(bridge.symbolSearch(PROJECT_ID, "query", signal)).rejects.toMatchObject(CLOSED);
    expect(removed).toHaveBeenCalledTimes(1);
    expect(shutdown).toBeDefined();
    expect(fetch).not.toHaveBeenCalled();
    await shutdown;
  });

  it("drains an admitted acquisition before reporting shutdown complete", async () => {
    const { bridge, root, authority } = await fixture();
    vi.spyOn(bridge, "resolveProject").mockResolvedValue({
      project_id: PROJECT_ID, label: "fixture", project_root: root, head_branch: null, default_branch: null,
    });
    const entered = deferred();
    const lookup = deferred<AuthorityLookup>();
    vi.spyOn(bridge, "authority").mockImplementation(() => {
      entered.resolve();
      return lookup.promise;
    });
    const start = vi.spyOn(StdioClientTransport.prototype, "start");
    const read = Promise.allSettled([bridge.node(PROJECT_ID, "n1")]);
    await entered.promise;
    let settled = false;
    const shutdown = bridge.close().then(() => { settled = true; });
    try {
      await setImmediate();
      expect(settled).toBe(false);
    } finally {
      lookup.resolve({ kind: "available", record: authority });
    }
    await shutdown;
    expect(await read).toEqual([{ status: "rejected", reason: expect.objectContaining(CLOSED) }]);
    expect(start).not.toHaveBeenCalled();
  });

  it("retains a failed registry session while concurrent retries and shutdown join its cleanup", async () => {
    const { bridge, authority } = await fixture();
    vi.spyOn(bridge, "authority").mockResolvedValue({ kind: "available", record: authority });
    const start = vi.spyOn(StdioClientTransport.prototype, "start");
    const calls = vi.spyOn(ServeSession.prototype, "callJson");
    vi.spyOn(Client.prototype, "request").mockRejectedValueOnce(new Error("initialize refused"));
    await expect(bridge.listProjects()).rejects.toMatchObject({ failure: { code: "serve_spawn_failed" } });
    const registry = calls.mock.contexts[0];
    expect(registry).toBeInstanceOf(ServeSession);
    const release = deferred();
    const originalClose = ServeSession.prototype.close;
    let retirement: Promise<void> | undefined;
    vi.spyOn(ServeSession.prototype, "close").mockImplementation(function (this: ServeSession) {
      if (this !== registry) return originalClose.call(this);
      retirement ??= release.promise.then(() => originalClose.call(this));
      return retirement;
    });
    const retries = Promise.allSettled([bridge.listProjects(), bridge.listProjects()]);
    let settled = false;
    let shutdown: Promise<void> | undefined;
    try {
      await setImmediate();
      expect(start).toHaveBeenCalledTimes(1);
      expect(retirement).toBeDefined();
      shutdown = bridge.close().then(() => { settled = true; });
      await setImmediate();
      expect(settled).toBe(false);
    } finally {
      release.resolve();
      await shutdown;
      await retirement;
    }
    expect(await retries).toEqual([
      { status: "rejected", reason: expect.objectContaining(CLOSED) },
      { status: "rejected", reason: expect.objectContaining(CLOSED) },
    ]);
    expect(start).toHaveBeenCalledTimes(1);
  });

  it("shares project acquisition despite one cancelled waiter and replaces a restarted daemon session", async () => {
    const { bridge, authority, writeAuthority } = await fixture();
    const start = vi.spyOn(StdioClientTransport.prototype, "start");
    const project = await bridge.resolveProject(PROJECT_ID);
    const resolve = vi.spyOn(bridge, "resolveProject").mockResolvedValue(project);
    const entered = deferred();
    const lookup = deferred<AuthorityLookup>();
    const authorityRead = vi.spyOn(bridge, "authority").mockImplementation(() => {
      entered.resolve();
      return lookup.promise;
    });
    const controller = new AbortController();
    const reason = new Error("only the first waiter cancelled");
    const first = Promise.allSettled([bridge.node(PROJECT_ID, "n1", controller.signal)]);
    const second = bridge.node(PROJECT_ID, "n2");
    try {
      await entered.promise;
      expect(authorityRead).toHaveBeenCalledTimes(1);
      controller.abort(reason);
      expect(await first).toEqual([{ status: "rejected", reason }]);
    } finally {
      lookup.resolve({ kind: "available", record: authority });
    }
    await expect(second).resolves.toMatchObject({ status: "not_found", node_id: "n2" });
    resolve.mockRestore();
    authorityRead.mockRestore();
    expect(start).toHaveBeenCalledTimes(2);
    await bridge.node(PROJECT_ID, "n3");
    expect(start).toHaveBeenCalledTimes(2);
    await writeAuthority({ ...authority, process_run_id: "fixture-run-b" });
    await expect(bridge.node(PROJECT_ID, "n4")).resolves.toMatchObject({ status: "not_found", node_id: "n4" });
    expect(start).toHaveBeenCalledTimes(3);
  });

  it("shares close completion and waits for all sessions when one cleanup fails", async () => {
    const failure = new Error("project cleanup failed");
    const { bridge } = await fixture(failure);
    await bridge.node(PROJECT_ID, "n1");
    const failed = deferred();
    const held = deferred();
    const release = deferred();
    const originalClose = ServeSession.prototype.close;
    let calls = 0;
    vi.spyOn(ServeSession.prototype, "close").mockImplementation(async function (this: ServeSession) {
      const index = calls++;
      await originalClose.call(this);
      if (index === 0) {
        failed.resolve();
        throw failure;
      }
      held.resolve();
      await release.promise;
    });
    const first = bridge.close();
    const second = bridge.close();
    let settled = false;
    const result = Promise.allSettled([first]).then((value) => { settled = true; return value; });
    const repeated = Promise.allSettled([second]);
    try {
      await Promise.all([failed.promise, held.promise]);
      // Drain promise continuations after both cleanup branches reach their barriers.
      await setImmediate();
      expect(settled).toBe(false);
      expect(second).toBe(first);
      await expect(bridge.listProjects()).rejects.toMatchObject(CLOSED);
    } finally {
      release.resolve();
    }
    expect(await result).toEqual([{ status: "rejected", reason: failure }]);
    expect(await repeated).toEqual([{ status: "rejected", reason: failure }]);
    expect(bridge.close()).toBe(first);
    expect(calls).toBe(2);
  });

  it.each(["forgotten", "restarted"] as const)("keeps a %s project's retiring session owned until its cleanup settles", async (retirement) => {
    const failure = new Error("retiring project cleanup failed");
    const { bridge, root, authority, writeAuthority } = await fixture(failure);
    await bridge.node(PROJECT_ID, "n1");
    if (retirement === "forgotten") {
      vi.spyOn(RegistryClient.prototype, "projectContext").mockResolvedValueOnce({
        status: "not_found", project: null, aliases: [], stores: [], registry_path: path.join(root, "global.db"),
      });
    } else {
      await writeAuthority({ ...authority, process_run_id: "fixture-run-b" });
    }
    const entered = deferred();
    const release = deferred();
    const originalClose = ServeSession.prototype.close;
    let retiring: ServeSession | undefined;
    let retiringClose: Promise<void>;
    vi.spyOn(ServeSession.prototype, "close").mockImplementation(function (this: ServeSession) {
      if (retiring === undefined) {
        retiring = this;
        retiringClose = (async () => {
          await originalClose.call(this);
          entered.resolve();
          await release.promise;
          throw failure;
        })();
      }
      return this === retiring ? retiringClose : originalClose.call(this);
    });
    const read = Promise.allSettled([retirement === "forgotten" ? bridge.resolveProject(PROJECT_ID) : bridge.node(PROJECT_ID, "n2")]);
    await entered.promise;
    let settled = false;
    const closing = Promise.allSettled([bridge.close()]).then((value) => { settled = true; return value; });
    try {
      await setImmediate();
      expect(settled).toBe(false);
    } finally {
      release.resolve();
    }
    expect(await read).toEqual([{ status: "rejected", reason: failure }]);
    expect(await closing).toEqual([{ status: "rejected", reason: failure }]);
  });
});
