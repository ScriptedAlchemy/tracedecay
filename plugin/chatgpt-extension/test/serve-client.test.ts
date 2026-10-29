import path from "node:path";
import { fileURLToPath } from "node:url";
import { Client, ProtocolError } from "@modelcontextprotocol/client";
import { StdioClientTransport } from "@modelcontextprotocol/client/stdio";
import { TraceDecayTransportError } from "@tracedecay/sdk";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DaemonFailure, ServeSession } from "../src/server/serve-client.js";
import { failureFromSdk } from "../src/server/bridge.js";

const PACKAGE_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const NODE_BINARY = process.env.JS_BINARY__NODE_BINARY ?? process.execPath;
const FAKE_SERVE = path.join(PACKAGE_ROOT, "test", "fake-serve.mjs");
const MAX_RESPONSE_HANDLE_BYTES = 1024 * 1024;

const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  try {
    for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
  } finally {
    vi.restoreAllMocks();
  }
});

function createSession(): ServeSession {
  return new ServeSession({
    binary: FAKE_SERVE,
    projectRoot: null,
    env: { ...process.env, PATH: `${path.dirname(NODE_BINARY)}${path.delimiter}${process.env.PATH ?? ""}` },
    cwd: PACKAGE_ROOT,
  });
}

function session(): ServeSession {
  const serve = createSession();
  cleanups.push(() => serve.close());
  return serve;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => { resolve = settle; });
  return { promise, resolve };
}

async function rejection(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  throw new Error("expected the call to reject");
}

async function retrieveOffsets(serve: ServeSession, handle: string): Promise<number[]> {
  const result = await serve.callJson("retrieve_calls", { handle });
  return (result as { readonly offsets: number[] }).offsets;
}

describe("daemon route failures", () => {
  it("preserves a retired response's typed reason through the SDK transport", () => {
    const error = new ProtocolError(-32603, "retained server retired before response completion", {
      reason_code: "project_server_response_revoked",
      retryable: true,
    });
    expect(failureFromSdk(new TraceDecayTransportError("MCP request failed", error))).toEqual({
      kind: "stale",
      code: "project_server_response_revoked",
      message: error.message,
    });
  });

  it("keeps route denials distinct from generic protocol errors", () => {
    expect(failureFromSdk(new ProtocolError(-32602, "not authorized", {
      reason_code: "application_surface_not_found_or_not_authorized",
      retryable: false,
    }))).toEqual({ kind: "denied", code: "application_surface_not_found_or_not_authorized", message: "not authorized" });
    expect(failureFromSdk(new ProtocolError(-32603, "unknown failure"))).toEqual({
      kind: "protocol", code: "jsonrpc_-32603", message: "unknown failure",
    });
  });
});

describe("serve session shutdown", () => {
  it("refuses connection, calls, and tool discovery after closing an unused session", async () => {
    const start = vi.spyOn(StdioClientTransport.prototype, "start");
    const serve = session();
    await serve.close();

    const results = await Promise.allSettled([
      serve.connect(),
      serve.callJson("plain_read", {}),
      serve.listToolNames(),
    ]);
    expect(results).toHaveLength(3);
    expect(start).not.toHaveBeenCalled();
    for (const result of results) {
      expect(result.status).toBe("rejected");
      if (result.status === "rejected") {
        expect(result.reason).toBeInstanceOf(DaemonFailure);
        expect(result.reason.failure.code).toBe("serve_closed");
      }
    }
  });

  it("joins concurrent close calls while the owned child is being closed", async () => {
    const serve = session();
    await serve.connect();
    const entered = deferred<StdioClientTransport>();
    const release = deferred<void>();
    const originalClose = StdioClientTransport.prototype.close;
    const close = vi.spyOn(StdioClientTransport.prototype, "close").mockImplementation(async function (this: StdioClientTransport) {
      entered.resolve(this);
      await release.promise;
      await originalClose.call(this);
    });
    const first = serve.close();
    const transport = await entered.promise;
    let childExited = false;
    const onclose = transport.onclose;
    transport.onclose = () => { childExited = true; onclose?.(); };
    const second = serve.close();
    try {
      expect(serve.closed).toBe(true);
      expect(transport.pid).not.toBeNull();
      expect(second).toBe(first);
      expect(close).toHaveBeenCalledTimes(1);
      await expect(serve.listToolNames()).rejects.toMatchObject({ failure: { code: "serve_closed" } });
    } finally {
      release.resolve();
      await Promise.all([first, second]);
    }
    expect(childExited).toBe(true);
    expect(transport.pid).toBeNull();
  });

  it("keeps a failed cleanup visible to every close caller", async () => {
    // No connection is opened: the injected failure concerns cleanup ownership.
    const serve = createSession();
    const entered = deferred<void>();
    const release = deferred<void>();
    const error = new Error("session cleanup failed");
    const originalClose = Client.prototype.close;
    const close = vi.spyOn(Client.prototype, "close")
      .mockImplementationOnce(async () => {
        entered.resolve();
        await release.promise;
        throw error;
      })
      .mockImplementation(originalClose);
    const first = serve.close();
    await entered.promise;
    const second = serve.close();
    release.resolve();
    expect(await Promise.allSettled([first, second])).toEqual([
      { status: "rejected", reason: error },
      { status: "rejected", reason: error },
    ]);
    await expect(serve.close()).rejects.toBe(error);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it("settles an in-progress connection before shutdown completes", async () => {
    const spawned = deferred<StdioClientTransport>();
    const releaseStart = deferred<void>();
    const transportClosed = deferred<void>();
    const originalStart = StdioClientTransport.prototype.start;
    const originalClose = StdioClientTransport.prototype.close;
    vi.spyOn(StdioClientTransport.prototype, "start").mockImplementation(async function (this: StdioClientTransport) {
      await originalStart.call(this);
      spawned.resolve(this);
      await releaseStart.promise;
    });
    vi.spyOn(StdioClientTransport.prototype, "close").mockImplementation(async function (this: StdioClientTransport) {
      await originalClose.call(this);
      transportClosed.resolve();
    });
    const serve = session();
    const order: string[] = [];
    const connecting = serve.connect().then(
      () => { order.push("connected"); return null; },
      (error: unknown) => { order.push("connection refused"); return error; },
    );
    const transport = await spawned.promise;
    expect(transport.pid).not.toBeNull();
    let childExited = false;
    const onclose = transport.onclose;
    transport.onclose = () => { childExited = true; onclose?.(); };
    const closing = serve.close().then(() => { order.push("closed"); });
    try {
      await transportClosed.promise;
      releaseStart.resolve();
      const [error] = await Promise.all([connecting, closing]);
      expect(error).toBeInstanceOf(DaemonFailure);
      expect(order).toEqual(["connection refused", "closed"]);
      expect(childExited).toBe(true);
      expect(transport.pid).toBeNull();
    } finally {
      releaseStart.resolve();
      await Promise.allSettled([connecting, closing]);
    }
  });

  it("drains a child started by a connection already accepted before shutdown", async () => {
    const entered = deferred<void>();
    const release = deferred<void>();
    const originalConnect = Client.prototype.connect;
    vi.spyOn(Client.prototype, "connect").mockImplementation(async function (this: Client, transport, options) {
      entered.resolve();
      await release.promise;
      await originalConnect.call(this, transport, options);
    });
    let transport: StdioClientTransport | null = null;
    let childExited = false;
    const originalStart = StdioClientTransport.prototype.start;
    vi.spyOn(StdioClientTransport.prototype, "start").mockImplementation(async function (this: StdioClientTransport) {
      transport = this;
      const onclose = this.onclose;
      this.onclose = () => { childExited = true; onclose?.(); };
      await originalStart.call(this);
    });
    const serve = session();
    const connecting = serve.connect().then(() => null, (error: unknown) => error);
    await entered.promise;
    const closing = serve.close();
    try {
      expect(transport).toBeNull();
      release.resolve();
      const [error] = await Promise.all([connecting, closing]);
      expect(error).toBeInstanceOf(DaemonFailure);
      expect((error as DaemonFailure).failure.code).toBe("serve_closed");
      expect(childExited).toBe(true);
      expect(transport).toMatchObject({ pid: null });
    } finally {
      release.resolve();
      await Promise.allSettled([connecting, closing]);
    }
  });

  it("waits for child exit when the SDK already started handshake-failure cleanup", async () => {
    const handshakeError = new Error("initialize refused");
    vi.spyOn(Client.prototype, "request").mockRejectedValueOnce(handshakeError);
    const spawned = deferred<StdioClientTransport>();
    const exited = deferred<void>();
    let childExited = false;
    const originalStart = StdioClientTransport.prototype.start;
    vi.spyOn(StdioClientTransport.prototype, "start").mockImplementation(async function (this: StdioClientTransport) {
      const onclose = this.onclose;
      this.onclose = () => { childExited = true; exited.resolve(); onclose?.(); };
      await originalStart.call(this);
      spawned.resolve(this);
    });
    const serve = session();
    try {
      await expect(serve.connect()).rejects.toMatchObject({
        failure: { code: "serve_spawn_failed" },
        cause: handshakeError,
      });
      const transport = await spawned.promise;
      expect(transport.pid).toBeNull();
      expect(childExited).toBe(false);
      await serve.close();
      expect(childExited).toBe(true);
    } finally {
      await exited.promise;
    }
  });
});

describe("response handle reassembly", () => {
  it("reconstructs multipage JSON using Unicode scalar offsets", async () => {
    const serve = session();
    await expect(serve.callJson("response_handle", { handle: "valid" })).resolves.toEqual({ items: ["界🧪", "ok"] });
    const offsets = await retrieveOffsets(serve, "valid");
    expect(offsets.length).toBeGreaterThan(1);
    expect(offsets[0]).toBe(0);
    expect(offsets.every((offset, index) => index === 0 || offset > (offsets[index - 1] ?? -1))).toBe(true);
  }, 15_000);

  it("reconstructs a near-budget response whose Unicode byte length exceeds its scalar count", async () => {
    const serve = session();
    const result = await serve.callJson("response_handle", { handle: "near_limit_unicode" }) as { padding: string };
    const bytes = Buffer.byteLength(JSON.stringify(result), "utf8");
    expect(bytes).toBeGreaterThan(1_000_000);
    expect(bytes).toBeLessThan(MAX_RESPONSE_HANDLE_BYTES);
    expect(Array.from(result.padding)).toHaveLength(250_000);
    expect(result.padding).toBe("🧪".repeat(250_000));
    expect((await retrieveOffsets(serve, "near_limit_unicode")).length).toBeGreaterThan(1);
  }, 15_000);

  it.each([
    ["offset_mismatch", 1],
    ["repeated", 1],
    ["decreasing", 2],
    ["missing_continuation", 1],
    ["unsafe_offset", 1],
    ["terminal_continuation", 1],
    ["continuation_at_end", 1],
    ["incomplete_terminal", 1],
    ["length_mismatch", 1],
    ["unsafe_length", 0],
  ])("rejects malformed continuation metadata for %s before another page", async (handle, expectedRequests) => {
    const serve = session();
    const name = String(handle);
    const error = await rejection(serve.callJson("response_handle", { handle: name }));
    expect(error).toBeInstanceOf(DaemonFailure);
    expect((await retrieveOffsets(serve, name)).length).toBe(expectedRequests);
  }, 15_000);

  it("rejects an advertised response beyond the reassembly budget before retrieval", async () => {
    const serve = session();
    const error = await rejection(serve.callJson("response_handle", { handle: "oversized_length" }));
    expect(error).toBeInstanceOf(DaemonFailure);
    expect((error as DaemonFailure).failure.code).toBe("response_handle_too_large");
    expect(await retrieveOffsets(serve, "oversized_length")).toEqual([]);
  });

  it("rejects cumulative UTF-8 content before retaining an over-budget page", async () => {
    const serve = session();
    const error = await rejection(serve.callJson("response_handle", { handle: "oversized_bytes" }));
    expect(error).toBeInstanceOf(DaemonFailure);
    expect((error as DaemonFailure).failure.code).toBe("response_handle_too_large");
    const offsets = await retrieveOffsets(serve, "oversized_bytes");
    expect(offsets.length).toBeGreaterThan(1);
    expect(offsets.length).toBeLessThan(1_024);
  }, 30_000);

  it("bounds the number of retrieve calls even when each page advances one scalar", async () => {
    const serve = session();
    const error = await rejection(serve.callJson("response_handle", { handle: "page_limit" }));
    expect(error).toBeInstanceOf(DaemonFailure);
    expect((error as DaemonFailure).failure.code).toBe("response_handle_page_limit");
    expect(await retrieveOffsets(serve, "page_limit")).toHaveLength(1_024);
  }, 30_000);

  it.each([
    ["missing", "handle_not_found"],
    ["expired", "handle_expired"],
  ])("preserves the %s handle outcome", async (handle, reasonCode) => {
    const serve = session();
    const name = String(handle);
    const error = await rejection(serve.callJson("response_handle", { handle: name }));
    expect(error).toBeInstanceOf(DaemonFailure);
    expect((error as DaemonFailure).failure).toMatchObject({ kind: "unavailable", code: String(reasonCode) });
  });

  it("stops after cancellation during a later page and preserves the abort reason", async () => {
    const serve = session();
    const controller = new AbortController();
    const reason = new DOMException("request cancelled", "AbortError");
    const pending = serve.callJson("response_handle", { handle: "cancelled" }, controller.signal);
    const deadline = Date.now() + 5_000;
    let offsets: number[] = [];
    while (Date.now() < deadline) {
      offsets = await retrieveOffsets(serve, "cancelled");
      if (offsets.length >= 2) break;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    expect(offsets).toHaveLength(2);
    controller.abort(reason);
    expect(await rejection(pending)).toBe(reason);
    await new Promise((resolve) => setTimeout(resolve, 300));
    expect(await retrieveOffsets(serve, "cancelled")).toHaveLength(2);
  }, 15_000);
});
