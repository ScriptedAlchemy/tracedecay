import { describe, expect, it } from "vitest";

import {
  TraceDecayAbortError,
  TraceDecayAuthenticationError,
  TraceDecayDisconnectedError,
  TraceDecayMalformedResponseError,
} from "../src/client";
import { silent, withDaemon, type Handler } from "./support/daemon";

function rejected(promise: Promise<unknown>): Promise<unknown> {
  return promise.then(
    () => new Error("expected the public SDK call to reject"),
    (error: unknown) => error,
  );
}

async function untilReceived(requests: readonly unknown[], count = 1): Promise<void> {
  while (requests.length < count) await new Promise((resolve) => setTimeout(resolve, 5));
}

/** Sends headers and a body prefix, then holds the connection open. */
function endless(status: number, contentType: string, prefix: string): Handler {
  return (_request, response) => {
    response.writeHead(status, { "content-type": contentType });
    response.write(prefix);
  };
}

describe("JSON response ownership", () => {
  it.each([
    { label: "Error", reason: new Error("caller stopped before headers") },
    { label: "null", reason: null },
  ])("preserves the caller's $label abort reason before response headers", async ({ reason }) => {
    await withDaemon([silent], async (daemon) => {
      const controller = new AbortController();
      const result = rejected(daemon.client().operations.workflow_list_definitions({}, { signal: controller.signal }));
      await untilReceived(daemon.requests);
      controller.abort(reason);
      const error = await result;
      expect(error).toBeInstanceOf(TraceDecayAbortError);
      expect((error as TraceDecayAbortError).cause).toBe(reason);
      await daemon.closed(0);
      expect(daemon.requests).toHaveLength(1);
    });
  });

  it.each(["operation", "cancellation"] as const)("interrupts a delayed %s body on abort", async (entrypoint) => {
    const status = entrypoint === "cancellation" ? 202 : 200;
    await withDaemon([endless(status, "application/json", '{"unfinished":')], async (daemon) => {
      const client = daemon.client();
      const controller = new AbortController();
      const reason = new Error("caller stopped during body consumption");
      const result = rejected(entrypoint === "operation"
        ? client.operations.workflow_list_definitions({}, { signal: controller.signal })
        : client.cancelOperation("request.operation", { signal: controller.signal }));
      await untilReceived(daemon.requests);
      await new Promise((resolve) => setTimeout(resolve, 20));
      controller.abort(reason);
      const error = await result;
      expect(error).toBeInstanceOf(TraceDecayAbortError);
      expect((error as TraceDecayAbortError).cause).toBe(reason);
      await daemon.closed(0);
    });
  });

  it.each([401, 403] as const)("refuses %i from its head without reading the refused body", async (status) => {
    await withDaemon([endless(status, "application/json", "untrusted refused content")], async (daemon) => {
      const error = await rejected(daemon.client().operations.workflow_list_definitions({}));
      expect(error).toBeInstanceOf(TraceDecayAuthenticationError);
      expect((error as TraceDecayAuthenticationError).status).toBe(status);
      expect((error as Error).message).not.toContain("untrusted refused content");
      await daemon.closed(0);
    });
  });

  it("refuses a cancellation-route authentication failure from its head", async () => {
    await withDaemon([endless(403, "application/json", "")], async (daemon) => {
      const error = await rejected(daemon.client().cancelOperation("request.operation"));
      expect(error).toBeInstanceOf(TraceDecayAuthenticationError);
      expect((error as TraceDecayAuthenticationError).status).toBe(403);
    });
  });

  it("refuses a wrong media type before waiting for its endless body", async () => {
    await withDaemon([endless(200, "text/plain", "not a JSON transport")], async (daemon) => {
      expect(await rejected(daemon.client().operations.workflow_list_definitions({})))
        .toBeInstanceOf(TraceDecayMalformedResponseError);
      await daemon.closed(0);
    });
  });

  it("keeps completely received malformed JSON distinct from a disconnected body", async () => {
    const incomplete: Handler = (_request, response) => {
      response.writeHead(200, { "content-type": "application/json" });
      response.end('{"incomplete":');
    };
    await withDaemon([incomplete], async (daemon) => {
      const error = await rejected(daemon.client().operations.workflow_list_definitions({}));
      expect(error).toBeInstanceOf(TraceDecayMalformedResponseError);
      expect(error).not.toBeInstanceOf(TraceDecayDisconnectedError);
    });
  });

  it("preserves every field of a successful cancellation response", async () => {
    const accepted: Handler = (_request, response) => {
      response.writeHead(202, { "content-type": "application/json" });
      // Multibyte text split across writes must decode intact.
      const bytes = Buffer.from(JSON.stringify({ status: "requested", future_field: true, detail: "界🧪" }));
      for (const byte of bytes) response.write(Buffer.of(byte));
      response.end();
    };
    await withDaemon([accepted], async (daemon) => {
      await expect(daemon.client().cancelOperation("request.operation"))
        .resolves.toEqual({ status: "requested", future_field: true, detail: "界🧪" });
      expect(daemon.requests.map((request) => [request.method, request.url])).toEqual([
        ["POST", "/projects/project.sdk/application/operations/request.operation/cancel"],
      ]);
    });
  });

  it("refuses a cancellation acknowledgement whose HTTP status disagrees with it", async () => {
    const mismatched: Handler = (_request, response) => {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ status: "requested" }));
    };
    await withDaemon([mismatched], async (daemon) => {
      expect(await rejected(daemon.client().cancelOperation("request.operation")))
        .toBeInstanceOf(TraceDecayMalformedResponseError);
    });
  });

  it("reports a truncated effectful response as disconnected without resending the request", async () => {
    const truncated: Handler = (_request, response) => {
      response.writeHead(200, { "content-type": "application/json", "content-length": "4096" });
      response.write('{"kind":"success","value":');
      setTimeout(() => response.destroy(), 20);
    };
    await withDaemon([truncated], async (daemon) => {
      const error = await rejected(daemon.client().operations.workflow_activate_definition({
        definition_id: "workflow.fixture", definition_version: 1, expected_revision: 1,
      }));
      expect(error).toBeInstanceOf(TraceDecayDisconnectedError);
      expect(daemon.requests.map((request) => [request.method, request.url])).toEqual([
        ["POST", "/projects/project.sdk/application/workflow/activate-definition"],
      ]);
    });
  });
});
