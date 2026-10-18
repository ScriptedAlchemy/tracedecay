import { describe, expect, it, vi } from "vitest";

import {
  createClient,
  TraceDecayDisconnectedError,
  TraceDecayMalformedResponseError,
} from "../src/client";

const OPEN = [
  "event: open",
  'data: {"event":"open","data":{"correlation_id":"request.operation","frontier":{"next_sequence":0,"retained_from_sequence":0,"resume_token":"resume"}}}',
  "",
  "",
].join("\n");

function rejectingCleanupFixture(content = OPEN, contentType = "text/event-stream") {
  const cleanupError = new Error("underlying transport could not close");
  const cancel = vi.fn(async () => { throw cleanupError; });
  const response = new Response(new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(content));
    },
    cancel,
  }), { headers: { "content-type": contentType } });
  const fetch = vi.fn(async () => response);
  const client = createClient({
    baseUrl: "http://127.0.0.1:4317",
    projectId: "project.sdk",
    token: "sdk-secret",
    fetch,
  });
  return { client, response, cancel, cleanupError, fetch };
}

describe("operation stream cleanup failures", () => {
  it("reports a typed transport failure when early return cannot close its body", async () => {
    const fixture = rejectingCleanupFixture();
    const iterator = fixture.client.streamOperation("request.operation");
    expect((await iterator.next()).value?.event).toBe("open");

    const failure = await iterator.return(undefined).catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(TraceDecayDisconnectedError);
    expect((failure as TraceDecayDisconnectedError).cause).toBe(fixture.cleanupError);
    expect(fixture.cancel).toHaveBeenCalledTimes(1);
    expect(fixture.fetch).toHaveBeenCalledTimes(1);
    expect(fixture.response.body?.locked).toBe(false);
    expect(await iterator.next()).toEqual({ done: true, value: undefined });
  });

  it("propagates failed cleanup through a consumer's break", async () => {
    const fixture = rejectingCleanupFixture();
    const consume = async () => {
      for await (const _event of fixture.client.streamOperation("request.operation")) {
        break;
      }
    };
    await expect(consume()).rejects.toBeInstanceOf(TraceDecayDisconnectedError);
    expect(fixture.cancel).toHaveBeenCalledTimes(1);
    expect(fixture.fetch).toHaveBeenCalledTimes(1);
  });

  it("does not replace a response validation error with a cleanup error", async () => {
    const fixture = rejectingCleanupFixture(OPEN, "text/plain");
    const failure = await fixture.client.streamOperation("request.operation")
      .next().catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(TraceDecayMalformedResponseError);
    expect((failure as Error).message).toBe(
      "the daemon opened an operation stream with an invalid media type",
    );
    expect(fixture.cancel).toHaveBeenCalledTimes(1);
    expect(fixture.response.body?.locked).toBe(false);
  });

  it("does not replace a malformed SSE payload with a cleanup error", async () => {
    const fixture = rejectingCleanupFixture("event: open\ndata: {invalid\n\n");
    const failure = await fixture.client.streamOperation("request.operation")
      .next().catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(TraceDecayMalformedResponseError);
    expect((failure as Error).message).toBe("the daemon returned non-JSON data");
    expect(fixture.cancel).toHaveBeenCalledTimes(1);
    expect(fixture.response.body?.locked).toBe(false);
  });
});
