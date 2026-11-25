import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { TraceDecayMalformedResponseError, type OperationStreamEvent } from "../src/client";
import { withDaemon, type Handler } from "./support/daemon";

const ENCODER = new TextEncoder();
// The fixture is shared with the Rust client suite: both transports run the
// same native framing, and this proves the Node path reaches it intact.
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

/**
 * Writes `chunks` as separate socket writes, then holds the connection open:
 * a refused frame must end the stream without waiting for the peer.
 */
function chunked(chunks: readonly Uint8Array[]): Handler {
  return async (_request, response) => {
    response.writeHead(200, { "content-type": "text/event-stream" });
    for (const chunk of chunks) {
      response.write(chunk);
      await new Promise((resolve) => setImmediate(resolve));
    }
  };
}

function split(bytes: Uint8Array, size: number): Uint8Array[] {
  const chunks: Uint8Array[] = [];
  for (let offset = 0; offset < bytes.length; offset += size) chunks.push(bytes.subarray(offset, offset + size));
  return chunks;
}

async function stream(
  chunks: readonly Uint8Array[],
): Promise<{ delivered: OperationStreamEvent[]; failure: unknown; connections: number }> {
  let result: { delivered: OperationStreamEvent[]; failure: unknown; connections: number } | undefined;
  await withDaemon([chunked(chunks)], async (daemon) => {
    const delivered: OperationStreamEvent[] = [];
    let failure: unknown;
    try {
      for await (const event of daemon.client().streamOperation("request.operation", { maxReconnects: 3 })) {
        delivered.push(event);
      }
    } catch (error) {
      failure = error;
    }
    // Refusal or completion releases the connection the daemon still holds.
    await daemon.closed(0);
    result = { delivered, failure, connections: daemon.requests.length };
  });
  if (result === undefined) throw new Error("fixture daemon did not run");
  return result;
}

describe("bounded operation SSE framing", () => {
  it.each(FIXTURES.cases.filter((fixture) => fixture.outcome === "oversized"))(
    "refuses $name without delivery or reconnect",
    async (fixture) => {
      const frame = ENCODER.encode(fixture.prefix + fixture.repeat.repeat(fixture.count) + fixture.suffix);
      const { delivered, failure, connections } = await stream([ENCODER.encode(OPEN), frame]);
      expect(failure).toBeInstanceOf(TraceDecayMalformedResponseError);
      expect(delivered.map((event) => event.event)).toEqual(["open"]);
      expect(connections).toBe(1);
    },
  );

  it.each(FIXTURES.cases.filter((fixture) => fixture.outcome !== "oversized"))(
    "accepts $name across arbitrary write boundaries",
    async (fixture) => {
      const frame = fixture.prefix + fixture.repeat.repeat(fixture.count) + fixture.suffix;
      const body = (fixture.outcome === "valid_open" ? "" : OPEN) + frame + TERMINAL;
      const { delivered, failure } = await stream(split(ENCODER.encode(body), 127));
      expect(failure).toBeUndefined();
      expect(delivered.map((event) => event.event)).toEqual(["open", "completed"]);
    },
  );

  it("refuses one oversized write without waiting for its line to end", async () => {
    const { failure, connections } = await stream([
      ENCODER.encode(OPEN), ENCODER.encode(":" + "x".repeat(LIMIT * 4)),
    ]);
    expect(failure).toBeInstanceOf(TraceDecayMalformedResponseError);
    expect(connections).toBe(1);
  });

  it("counts a CRLF split across writes as part of the bounded raw line", async () => {
    const { failure } = await stream([
      ENCODER.encode(OPEN), ENCODER.encode(":" + "x".repeat(LIMIT - 2) + "\r"), ENCODER.encode("\n"),
    ]);
    expect(failure).toBeInstanceOf(TraceDecayMalformedResponseError);
  });

  it("refuses oversized combined fields before a dispatch delimiter", async () => {
    const { failure } = await stream([
      ENCODER.encode(OPEN),
      ENCODER.encode(`event: ${"e".repeat(32_768)}\nid: ${"1".repeat(32_768)}\n`),
    ]);
    expect(failure).toBeInstanceOf(TraceDecayMalformedResponseError);
  });

  it("keeps a frame's ID across an empty frame and charges it to the next frame", async () => {
    const inherited = await stream(split(ENCODER.encode(
      OPEN + "id: 0\n\n: heartbeat\r\n\r\n" + TERMINAL.replace("id: 0\n", ""),
    ), 1));
    expect(inherited.delivered.map((event) => event.id)).toEqual([null, "0"]);

    const charged = await stream([
      ENCODER.encode(OPEN),
      ENCODER.encode(`id: ${"1".repeat(40_000)}\n\nevent: ${"e".repeat(25_000)}\ndata: ${"d".repeat(1000)}\n`),
    ]);
    expect(charged.failure).toBeInstanceOf(TraceDecayMalformedResponseError);
  });

  it("keeps malformed UTF-8 split across writes a non-retryable protocol failure", async () => {
    const { failure, connections } = await stream([
      ENCODER.encode(OPEN), ENCODER.encode("data: "), Uint8Array.of(0xc3), Uint8Array.of(0x0a, 0xa9, 0x0a),
    ]);
    expect(failure).toBeInstanceOf(TraceDecayMalformedResponseError);
    expect(connections).toBe(1);
  });

  it("decodes multibyte UTF-8 split inside a code point", async () => {
    const bytes = ENCODER.encode(OPEN.replace("\"resume\"", "\"résumé🙂\"") + TERMINAL);
    const { delivered, failure } = await stream(split(bytes, 1));
    expect(failure).toBeUndefined();
    expect(delivered[0]?.data.data).toMatchObject({ frontier: { resume_token: "résumé🙂" } });
  });
});
