import { readFileSync } from "node:fs";

import { describe, expect, it, vi } from "vitest";

import {
  createClient,
  TraceDecayMalformedResponseError,
  type OperationStreamEvent,
} from "../src/client";

const ENCODER = new TextEncoder();
const FIXTURES = JSON.parse(readFileSync(new URL(
  "../../../crates/tracedecay-sdk/tests/fixtures/sse-framing.json",
  import.meta.url,
), "utf8")) as {
  limit_bytes: number;
  cases: Array<{
    name: string;
    outcome: "oversized" | "valid" | "valid_open";
    prefix: string;
    repeat: string;
    count: number;
    suffix: string;
  }>;
};
const LIMIT = FIXTURES.limit_bytes;
const OPEN = "event: open\ndata: " + JSON.stringify({
  event: "open",
  data: {
    correlation_id: "request.operation",
    frontier: { next_sequence: 0, retained_from_sequence: 0, resume_token: "resume" },
  },
}) + "\n\n";
const TERMINAL = "event: completed\nid: 0\ndata: " + JSON.stringify({
  event: "completed",
  data: {
    sequence: 0,
    terminal: {
      termination: "completed",
      receipt: {
        started_at: 1, ended_at: 2, effective_deadline: { expires_at: 3 },
        cancellation: null,
        budget: { units_consumed: 1, bytes_consumed: 1, elapsed_micros: 1 },
        termination: "completed",
      },
    },
  },
}) + "\n\n";

function controlledStream(chunks: Iterable<Uint8Array>, cancellationError?: Error) {
  const source = chunks[Symbol.iterator]();
  let pulls = 0;
  let cancelled = 0;
  let requests = 0;
  const body = new ReadableStream<Uint8Array>({
    pull(controller) {
      pulls += 1;
      const chunk = source.next();
      if (chunk.done) {
        controller.error(new Error("the client read past the supplied frames"));
      } else {
        controller.enqueue(chunk.value);
      }
    },
    cancel() {
      cancelled += 1;
      if (cancellationError !== undefined) throw cancellationError;
    },
  }, { highWaterMark: 0 });
  const response = new Response(body, { headers: { "content-type": "text/event-stream" } });
  const client = createClient({
    baseUrl: "http://127.0.0.1:4317", projectId: "project.sdk", token: "sdk-secret",
    fetch: async () => {
      requests += 1;
      if (requests > 1) throw new Error("unexpected reconnect");
      return response;
    },
  });
  const events: OperationStreamEvent[] = [];
  const iterator = client.streamOperation("request.operation", { maxReconnects: 3 });
  return {
    events,
    iterator,
    async collect() {
      for await (const event of iterator) events.push(event);
      return events;
    },
    state: () => ({ pulls, cancelled, requests, locked: body.locked }),
  };
}

function* fragment(bytes: Uint8Array, size: number): Generator<Uint8Array> {
  for (let offset = 0; offset < bytes.length; offset += size) {
    yield bytes.subarray(offset, offset + size);
  }
}

describe("bounded operation SSE framing", () => {
  it.each(FIXTURES.cases.filter((fixture) => fixture.outcome === "oversized"))(
    "refuses $name without reading on or reconnecting",
    async (fixture) => {
      const bytes = ENCODER.encode(fixture.prefix + fixture.repeat.repeat(fixture.count) + fixture.suffix);
      const stream = controlledStream([ENCODER.encode(OPEN), bytes]);
      await expect(stream.collect()).rejects.toMatchObject({
        name: "TraceDecayMalformedResponseError",
        message: `the daemon returned an SSE line or frame exceeding ${LIMIT} bytes`,
        payload: { limitBytes: LIMIT },
      });
      expect(stream.events.map((event) => event.event)).toEqual(["open"]);
      expect(stream.state()).toEqual({ pulls: 2, cancelled: 1, requests: 1, locked: false });
      expect(await stream.iterator.next()).toEqual({ done: true, value: undefined });
    },
  );

  it.each(FIXTURES.cases.filter((fixture) => fixture.outcome !== "oversized"))(
    "accepts $name across transport boundaries",
    async (fixture) => {
      const frame = fixture.prefix + fixture.repeat.repeat(fixture.count) + fixture.suffix;
      const body = (fixture.outcome === "valid_open" ? "" : OPEN) + frame + TERMINAL;
      const stream = controlledStream(fragment(ENCODER.encode(body), 127));
      expect((await stream.collect()).map((event) => event.event)).toEqual(["open", "completed"]);
      expect(stream.state()).toMatchObject({ cancelled: 1, requests: 1, locked: false });
    },
  );

  it("preserves the size failure when cancellation also fails", async () => {
    const stream = controlledStream([
      ENCODER.encode(OPEN), ENCODER.encode(":" + "x".repeat(LIMIT)),
    ], new Error("cancel failed"));
    await expect(stream.collect()).rejects.toMatchObject({
      name: "TraceDecayMalformedResponseError", payload: { limitBytes: LIMIT },
    });
    expect(stream.state()).toEqual({ pulls: 2, cancelled: 1, requests: 1, locked: false });
  });

  it("counts a split CRLF as part of the bounded raw line", async () => {
    const stream = controlledStream([
      ENCODER.encode(OPEN), ENCODER.encode(":" + "x".repeat(LIMIT - 2) + "\r"),
      ENCODER.encode("\n"),
    ]);
    await expect(stream.collect()).rejects.toMatchObject({ payload: { limitBytes: LIMIT } });
    expect(stream.state()).toEqual({ pulls: 3, cancelled: 1, requests: 1, locked: false });
  });

  it("refuses oversized combined fields before a dispatch delimiter", async () => {
    const stream = controlledStream([
      ENCODER.encode(OPEN),
      ENCODER.encode(`event: ${"e".repeat(32_768)}\nid: ${"1".repeat(32_768)}\n`),
    ]);
    await expect(stream.collect()).rejects.toMatchObject({ payload: { limitBytes: LIMIT } });
    expect(stream.state()).toEqual({ pulls: 2, cancelled: 1, requests: 1, locked: false });
  });

  it("preserves the last event ID across empty frames without retaining old data", async () => {
    const body = OPEN + "id: 0\n\n: heartbeat\r\n\r\n" + TERMINAL.replace("id: 0\n", "");
    const stream = controlledStream(fragment(ENCODER.encode(body), 1));
    expect((await stream.collect()).map((event) => event.id)).toEqual([null, "0"]);
  });

  it("keeps malformed split UTF-8 a non-retryable protocol failure", async () => {
    const stream = controlledStream([
      ENCODER.encode(OPEN), ENCODER.encode("data: "), Uint8Array.of(0xc3),
      Uint8Array.of(0x0a, 0xa9, 0x0a),
    ]);
    await expect(stream.collect()).rejects.toBeInstanceOf(TraceDecayMalformedResponseError);
    expect(stream.state()).toMatchObject({ cancelled: 1, requests: 1, locked: false });
  });

  it("strips only the initial BOM and preserves later field prefixes", async () => {
    const body = "\uFEFF" + OPEN + "id: 0\n\n\uFEFFid: 1\n\n" + TERMINAL.replace("id: 0\n", "");
    const stream = controlledStream(fragment(ENCODER.encode(body), 1));
    expect((await stream.collect()).map((event) => event.id)).toEqual([null, "0"]);
  });

  it("bounds decoding even when a transport supplies one huge chunk", async () => {
    const stream = controlledStream([
      ENCODER.encode(OPEN), ENCODER.encode(":" + "x".repeat(LIMIT * 4)),
    ]);
    const decoding = vi.spyOn(TextDecoder.prototype, "decode");
    try {
      await expect(stream.collect()).rejects.toMatchObject({ payload: { limitBytes: LIMIT } });
      const decodedBytes = decoding.mock.calls.map(([input]) => input?.byteLength ?? 0);
      expect(decodedBytes.length).toBeGreaterThan(0);
      expect(Math.max(...decodedBytes)).toBeLessThanOrEqual(LIMIT);
      expect(decodedBytes.reduce((sum, bytes) => sum + bytes, 0)).toBeLessThan(LIMIT);
      expect(stream.state()).toEqual({ pulls: 2, cancelled: 1, requests: 1, locked: false });
    } finally {
      decoding.mockRestore();
    }
  });

  it("inspects fragmented partial lines once per byte", async () => {
    for (const length of [4096, 8192]) {
      const bytes = ENCODER.encode(OPEN + ":" + "x".repeat(length) + "\n" + TERMINAL);
      let inspected = 0;
      function* observedChunks() {
        for (const chunk of fragment(bytes, 1)) {
          // Observe real byte access without adding counters or test hooks to
          // the parser. The backing bytes and public stream path are unchanged.
          yield new Proxy(chunk, {
            get(target, key) {
              if (typeof key === "string" && /^[0-9]+$/.test(key)) inspected += 1;
              const value = Reflect.get(target, key, target);
              return typeof value === "function" ? value.bind(target) : value;
            },
          });
        }
      }
      const stream = controlledStream(observedChunks());
      expect((await stream.collect()).map((event) => event.event)).toEqual(["open", "completed"]);
      expect(inspected).toBe(bytes.length);
    }
  });

  it("continues accounting for an inherited ID after an empty frame", async () => {
    const stream = controlledStream([
      ENCODER.encode(OPEN),
      ENCODER.encode(`id: ${"1".repeat(40_000)}\n\nevent: ${"e".repeat(25_000)}\ndata: ${"d".repeat(1000)}\n`),
    ]);
    await expect(stream.collect()).rejects.toMatchObject({ payload: { limitBytes: LIMIT } });
    expect(stream.state()).toEqual({ pulls: 2, cancelled: 1, requests: 1, locked: false });
  });
});
