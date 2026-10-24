import { once } from "node:events";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { setImmediate } from "node:timers/promises";

import { describe, expect, it, vi } from "vitest";

import {
  createClient,
  TraceDecayAbortError,
  TraceDecayAuthenticationError,
  TraceDecayDisconnectedError,
  TraceDecayMalformedResponseError,
} from "../src/client";

function deferred<T = void>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((accept) => { resolve = accept; });
  return { promise, resolve };
}

function rejected(promise: Promise<unknown>): Promise<unknown> {
  return promise.then(
    () => new Error("expected the public SDK call to reject"),
    (error: unknown) => error,
  );
}

function clientFor(fetch: typeof globalThis.fetch, baseUrl = "http://127.0.0.1:4317") {
  return createClient({ baseUrl, projectId: "project.sdk", token: "isolated-test-token", fetch });
}

async function withServer(
  handler: (request: IncomingMessage, response: ServerResponse) => void,
  run: (baseUrl: string, requests: IncomingMessage[]) => Promise<void>,
): Promise<void> {
  const requests: IncomingMessage[] = [];
  const server = createServer((request, response) => {
    requests.push(request);
    request.resume();
    handler(request, response);
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (address === null || typeof address === "string") throw new Error("fixture has no TCP address");
  try {
    await run(`http://127.0.0.1:${address.port}`, requests);
  } finally {
    const closed = once(server, "close");
    server.closeAllConnections();
    server.close();
    await closed;
  }
}

describe("JSON response ownership", () => {
  it.each([
    { label: "Error", reason: new Error("caller stopped before headers") },
    { label: "null", reason: null },
  ])("preserves the caller's $label abort reason before response headers", async ({ reason }) => {
    const received = deferred();
    await withServer(() => received.resolve(), async (baseUrl, requests) => {
      const controller = new AbortController();
      const client = clientFor(globalThis.fetch, baseUrl);
      const result = rejected(client.operations.workflow_list_definitions({}, { signal: controller.signal }));
      try {
        await Promise.race([received.promise, result.then((error) => { throw error; })]);
        controller.abort(reason);
        const error = await result;
        expect(error).toBeInstanceOf(TraceDecayAbortError);
        expect((error as TraceDecayAbortError).cause).toBe(reason);
        expect(requests).toHaveLength(1);
      } finally {
        controller.abort(reason);
      }
    });
  });

  it.each(["operation", "cancellation"] as const)("preserves an abort during a delayed %s body", async (entrypoint) => {
    await withServer((_request, response) => {
      response.writeHead(entrypoint === "cancellation" ? 202 : 200, { "content-type": "application/json" });
      response.write('{"unfinished":');
    }, async (baseUrl, requests) => {
      const controller = new AbortController();
      const reason = new Error("caller stopped during body consumption");
      const received = deferred<Response>();
      const client = clientFor(async (input, init) => {
        const response = await fetch(input, init);
        received.resolve(response);
        return response;
      }, baseUrl);
      const result = rejected(entrypoint === "operation"
        ? client.operations.workflow_list_definitions({}, { signal: controller.signal })
        : client.cancelOperation("request.operation", { signal: controller.signal }));
      try {
        const response = await Promise.race([received.promise, result.then((error) => { throw error; })]);
        // Let the SDK acquire its body reader after headers resolve.
        await setImmediate();
        expect(response.body?.locked).toBe(true);
        controller.abort(reason);
        const error = await result;
        expect(error).toBeInstanceOf(TraceDecayAbortError);
        expect((error as TraceDecayAbortError).cause).toBe(reason);
        expect(response.body?.locked).toBe(false);
        expect(requests).toHaveLength(1);
      } finally {
        controller.abort(reason);
      }
    });
  });

  it.each([
    { label: "object", reason: { code: "caller_cancelled_body" } },
    { label: "null", reason: null },
  ])("owns injected-body cancellation with a $label reason even when cleanup rejects", async ({ reason }) => {
    const controller = new AbortController();
    const cleanupFailure = new Error("underlying cleanup failed");
    const cancel = vi.fn(async () => { throw cleanupFailure; });
    let body!: ReadableStreamDefaultController<Uint8Array>;
    const response = new Response(new ReadableStream<Uint8Array>({
      start(stream) { body = stream; },
      cancel,
    }), { headers: { "content-type": "application/json" } });
    const client = clientFor(async () => response);
    let settled = false;
    const result = rejected(client.operations.workflow_list_definitions({}, { signal: controller.signal }))
      .then((error) => { settled = true; return error; });
    try {
      await setImmediate();
      expect(response.body?.locked).toBe(true);
      controller.abort(reason);
      await setImmediate();
      expect(settled).toBe(true);
      const error = await result;
      expect(error).toBeInstanceOf(TraceDecayAbortError);
      expect((error as TraceDecayAbortError).cause).toBe(reason);
      expect(cancel).toHaveBeenCalledTimes(1);
      expect(response.body?.locked).toBe(false);
    } finally {
      // Release the fixture even if an implementation ignores the signal.
      if (!settled) body.error(new Error("fixture released an unowned body"));
      await result;
    }
  });

  it("classifies body I/O failure as disconnection with its original cause", async () => {
    const cause = new TypeError("body transport terminated");
    const response = new Response(new ReadableStream<Uint8Array>({ start(controller) { controller.error(cause); } }), {
      headers: { "content-type": "application/json" },
    });
    const client = clientFor(async () => response);
    const error = await rejected(client.operations.workflow_list_definitions({}));
    expect(error).toBeInstanceOf(TraceDecayDisconnectedError);
    expect((error as TraceDecayDisconnectedError).cause).toBe(cause);
    expect(response.body?.locked).toBe(false);
  });

  it.each([401, 403] as const)("cancels an unread %i body without consuming refused content", async (status) => {
    const cancel = vi.fn();
    const response = new Response(new ReadableStream<Uint8Array>({
      start(controller) { controller.enqueue(new TextEncoder().encode("untrusted refused content")); },
      cancel,
    }), { status, headers: { "content-type": "application/json" } });
    const client = clientFor(async () => response);
    try {
      const error = await rejected(client.operations.workflow_list_definitions({}));
      expect(error).toBeInstanceOf(TraceDecayAuthenticationError);
      expect((error as TraceDecayAuthenticationError).status).toBe(status);
      expect((error as Error).message).not.toContain("untrusted refused content");
      expect(cancel).toHaveBeenCalledTimes(1);
      expect(response.body?.locked).toBe(false);
    } finally {
      await response.body?.cancel().catch(() => undefined);
    }
  });

  it("preserves cancellation-route authentication failure when unread-body cleanup rejects", async () => {
    const cleanupFailure = new Error("cleanup must not replace authentication");
    const cancel = vi.fn(async () => { throw cleanupFailure; });
    const response = new Response(new ReadableStream<Uint8Array>({ cancel }), {
      status: 403, headers: { "content-type": "application/json" },
    });
    const client = clientFor(async () => response);
    try {
      const error = await rejected(client.cancelOperation("request.operation"));
      expect(error).toBeInstanceOf(TraceDecayAuthenticationError);
      expect((error as TraceDecayAuthenticationError).status).toBe(403);
      expect(cancel).toHaveBeenCalledTimes(1);
      expect(response.body?.locked).toBe(false);
    } finally {
      await response.body?.cancel().catch(() => undefined);
    }
  });

  it("refuses a wrong media type before waiting for its endless body", async () => {
    const cancel = vi.fn();
    let body!: ReadableStreamDefaultController<Uint8Array>;
    const response = new Response(new ReadableStream<Uint8Array>({
      start(controller) { body = controller; controller.enqueue(new TextEncoder().encode("not a JSON transport")); },
      cancel,
    }), { headers: { "content-type": "text/plain" } });
    const client = clientFor(async () => response);
    let settled = false;
    const result = rejected(client.operations.workflow_list_definitions({}))
      .then((error) => { settled = true; return error; });
    try {
      await setImmediate();
      expect(settled).toBe(true);
      expect(await result).toBeInstanceOf(TraceDecayMalformedResponseError);
      expect(cancel).toHaveBeenCalledTimes(1);
      expect(response.body?.locked).toBe(false);
    } finally {
      if (!settled) body.close();
      await result;
    }
  });

  it("keeps completely received malformed JSON distinct from a disconnected body", async () => {
    const response = new Response('{"incomplete":', { headers: { "content-type": "application/json" } });
    const client = clientFor(async () => response);
    const error = await rejected(client.operations.workflow_list_definitions({}));
    expect(error).toBeInstanceOf(TraceDecayMalformedResponseError);
    expect(error).not.toBeInstanceOf(TraceDecayDisconnectedError);
    expect(response.bodyUsed).toBe(true);
  });

  it("consumes a successful cancellation response and preserves its fields", async () => {
    const response = Response.json({ status: "requested", future_field: true }, { status: 202 });
    const client = clientFor(async () => response);
    await expect(client.cancelOperation("request.operation")).resolves.toEqual({ status: "requested", future_field: true });
    expect(response.bodyUsed).toBe(true);
  });

  it("decodes a leading BOM and multibyte JSON text split across byte chunks", async () => {
    const value = { status: "requested", detail: "界🧪" };
    const bytes = new TextEncoder().encode(`\uFEFF${JSON.stringify(value)}`);
    const response = new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        for (const byte of bytes) controller.enqueue(Uint8Array.of(byte));
        controller.close();
      },
    }), { status: 202, headers: { "content-type": "application/json" } });
    const client = clientFor(async () => response);
    await expect(client.cancelOperation("request.operation")).resolves.toEqual(value);
  });

  it("preserves Response.text replacement semantics for malformed UTF-8 inside JSON text", async () => {
    const encoder = new TextEncoder();
    const response = new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoder.encode('{"status":"requested","detail":"'));
        controller.enqueue(Uint8Array.of(0xc3));
        controller.enqueue(Uint8Array.of(0x28));
        controller.enqueue(encoder.encode('"}'));
        controller.close();
      },
    }), { status: 202, headers: { "content-type": "application/json" } });
    const client = clientFor(async () => response);
    await expect(client.cancelOperation("request.operation")).resolves.toEqual({ status: "requested", detail: "\uFFFD(" });
  });

  it("reports a truncated effectful response as disconnected without resending the request", async () => {
    let reply!: ServerResponse;
    await withServer((_request, response) => {
      reply = response;
      response.writeHead(200, { "content-type": "application/json", "content-length": "4096" });
      response.write('{"kind":"success","value":');
    }, async (baseUrl, requests) => {
      const received = deferred<Response>();
      const client = clientFor(async (input, init) => {
        const response = await fetch(input, init);
        received.resolve(response);
        return response;
      }, baseUrl);
      const result = rejected(client.operations.workflow_activate_definition({
        definition_id: "workflow.fixture", definition_version: 1, expected_revision: 1,
      }));
      const response = await Promise.race([received.promise, result.then((error) => { throw error; })]);
      reply.destroy();
      const error = await result;
      expect(error).toBeInstanceOf(TraceDecayDisconnectedError);
      expect((error as TraceDecayDisconnectedError).cause).toBeInstanceOf(TypeError);
      expect(response.body?.locked).toBe(false);
      expect(requests).toHaveLength(1);
      expect(requests[0].method).toBe("POST");
      expect(requests[0].url).toBe("/projects/project.sdk/application/workflow/activate-definition");
    });
  });
});
