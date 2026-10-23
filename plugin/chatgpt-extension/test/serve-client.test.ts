import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { DaemonFailure, ServeSession } from "../src/server/serve-client.js";

const PACKAGE_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const NODE_BINARY = process.env.JS_BINARY__NODE_BINARY ?? process.execPath;
const FAKE_SERVE = path.join(PACKAGE_ROOT, "test", "fake-serve.mjs");
const MAX_RESPONSE_HANDLE_BYTES = 1024 * 1024;

const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

function session(): ServeSession {
  const serve = new ServeSession({
    binary: FAKE_SERVE,
    projectRoot: null,
    env: { ...process.env, PATH: `${path.dirname(NODE_BINARY)}${path.delimiter}${process.env.PATH ?? ""}` },
    cwd: PACKAGE_ROOT,
  });
  cleanups.push(() => serve.close());
  return serve;
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
