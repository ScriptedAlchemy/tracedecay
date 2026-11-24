import { spawn } from "node:child_process";
import { once } from "node:events";

import { describe, expect, it } from "vitest";

import {
  TraceDecayAbortError,
  TraceDecayDisconnectedError,
  TraceDecayProtocolError,
  type TraceDecayClient,
} from "../src/client";
import { addonPath, heldEvents, silent, withDaemon, type Handler } from "./support/daemon";

const OPEN = "event: open\ndata: " + JSON.stringify({
  event: "open",
  data: {
    correlation_id: "request.operation",
    frontier: { next_sequence: 0, retained_from_sequence: 0, resume_token: "resume" },
  },
}) + "\n\n";

function item(sequence: number): string {
  return `event: item\nid: ${sequence}\ndata: ${JSON.stringify({ event: "item", data: { sequence, item: { padding: "p".repeat(256) } } })}\n\n`;
}

const listDefinitions = (client: TraceDecayClient, signal?: AbortSignal) =>
  client.operations.workflow_list_definitions({}, signal === undefined ? {} : { signal });

/** Counts event-loop turns while `work` is pending. */
async function loopTurnsDuring(work: Promise<unknown>): Promise<number> {
  let turns = 0;
  const timer = setInterval(() => { turns += 1; }, 5);
  try {
    await work.catch(() => undefined);
  } finally {
    clearInterval(timer);
  }
  return turns;
}

describe("native lifecycle ownership", () => {
  it("keeps the event loop running while a request waits on the network", async () => {
    const delayed: Handler = (_request, response) => {
      setTimeout(() => response.writeHead(503).end(), 300);
    };
    await withDaemon([delayed], async (daemon) => {
      const turns = await loopTurnsDuring(listDefinitions(daemon.client()));
      // A blocked loop would run zero turns; 300 ms at 5 ms leaves wide margin.
      expect(turns).toBeGreaterThan(10);
    });
  });

  it("interrupts an in-flight request on abort and closes its connection", async () => {
    await withDaemon([silent], async (daemon) => {
      const controller = new AbortController();
      const reason = new Error("caller stopped");
      const request = listDefinitions(daemon.client(), controller.signal);
      // Wait until the daemon holds the request, so the abort hits live I/O.
      while (daemon.requests.length === 0) await new Promise((resolve) => setTimeout(resolve, 5));
      controller.abort(reason);
      const failure = await request.catch((error: unknown) => error);
      expect(failure).toBeInstanceOf(TraceDecayAbortError);
      expect((failure as TraceDecayAbortError).cause).toBe(reason);
      await daemon.closed(0);
    });
  });

  it("refuses an already-aborted signal without contacting the daemon", async () => {
    await withDaemon([], async (daemon) => {
      const controller = new AbortController();
      controller.abort();
      await expect(listDefinitions(daemon.client(), controller.signal)).rejects.toBeInstanceOf(TraceDecayAbortError);
      expect(daemon.requests).toHaveLength(0);
    });
  });

  it("releases a watcher's connection on early return without cancelling the operation", async () => {
    await withDaemon([heldEvents(OPEN)], async (daemon) => {
      const iterator = daemon.client().streamOperation("request.operation");
      expect((await iterator.next()).value?.event).toBe("open");
      await iterator.return(undefined);
      await daemon.closed(0);
      expect(await iterator.next()).toEqual({ done: true, value: undefined });
      expect(daemon.requests.map((request) => request.method)).toEqual(["GET"]);
    });
  });

  it("interrupts a pending stream read on abort", async () => {
    await withDaemon([heldEvents(OPEN)], async (daemon) => {
      const controller = new AbortController();
      const iterator = daemon.client().streamOperation("request.operation", { signal: controller.signal });
      expect((await iterator.next()).value?.event).toBe("open");
      const pending = iterator.next();
      controller.abort();
      await expect(pending).rejects.toBeInstanceOf(TraceDecayAbortError);
      await daemon.closed(0);
      expect(daemon.requests).toHaveLength(1);
    });
  });

  it("applies backpressure to the daemon while the consumer is not reading", async () => {
    let refusedAfter: number | undefined;
    const flood: Handler = (_request, response) => {
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.write(OPEN);
      // Write until the socket refuses more; an eager reader would drain
      // everything into memory and the write would never be refused.
      for (let sequence = 0; sequence < 200_000; sequence += 1) {
        if (!response.write(item(sequence))) {
          refusedAfter = sequence;
          return;
        }
      }
    };
    await withDaemon([flood], async (daemon) => {
      const iterator = daemon.client().streamOperation("request.operation");
      expect((await iterator.next()).value?.event).toBe("open");
      await new Promise((resolve) => setTimeout(resolve, 200));
      expect(refusedAfter).toBeDefined();
      // The kernel buffers on both sides bound what was produced, far below
      // the 200k events (~60 MB) an unbounded reader would accept.
      expect(refusedAfter).toBeLessThan(50_000);
      await iterator.return(undefined);
      await daemon.closed(0);
    });
  });

  it("lets the process exit on its own once native calls settle", async () => {
    await withDaemon([silent, heldEvents(OPEN)], async (daemon) => {
      // The child exits only if no native call, stream, or runtime thread
      // still holds the event loop open; it never calls process.exit.
      const script = `
        const { NativeClient } = require(${JSON.stringify(addonPath())});
        const client = new NativeClient(JSON.stringify({
          baseUrl: ${JSON.stringify(daemon.baseUrl)}, projectId: "project.sdk", token: "t",
        }));
        (async () => {
          const call = client.prepareOperation("operation.workflow.list_definitions", "{}", "{}");
          const pending = call.run().catch((error) => JSON.parse(error.message).kind);
          setTimeout(() => call.interrupt(), 50);
          const stream = client.prepareStream("request.operation", "{}");
          const opened = JSON.parse(await stream.next()).event;
          stream.close();
          console.log(JSON.stringify({ interrupted: await pending, opened }));
        })();
      `;
      const child = spawn(process.execPath, ["-e", script], { stdio: ["ignore", "pipe", "inherit"] });
      let output = "";
      child.stdout.on("data", (chunk: Buffer) => { output += chunk.toString("utf8"); });
      const timer = setTimeout(() => child.kill("SIGKILL"), 10_000);
      const [code, signal] = (await once(child, "exit")) as [number | null, NodeJS.Signals | null];
      clearTimeout(timer);
      expect(signal, "the child had to be killed: a native handle kept it alive").toBeNull();
      expect(code).toBe(0);
      expect(JSON.parse(output)).toEqual({ interrupted: "interrupted", opened: "open" });
      await daemon.closed(0);
      await daemon.closed(1);
    });
  });

  it("classifies a refused connection as disconnection", async () => {
    let baseUrl = "";
    await withDaemon([], async (daemon) => { baseUrl = daemon.baseUrl; });
    const { createClient } = await import("../src/client");
    const { addonPath } = await import("./support/daemon");
    const client = createClient({ baseUrl, projectId: "project.sdk", token: "t", addon: addonPath() });
    await expect(client.operations.workflow_list_definitions({})).rejects.toBeInstanceOf(TraceDecayDisconnectedError);
  });

  it("refuses invalid options and stream cursors before transport", async () => {
    await withDaemon([], async (daemon) => {
      const client = daemon.client();
      await expect(client.operations.workflow_list_definitions({}, { deadlineMicros: 0 }))
        .rejects.toBeInstanceOf(TraceDecayProtocolError);
      await expect(client.operations.workflow_list_definitions({}, { page: { size: 1_001 } }))
        .rejects.toBeInstanceOf(TraceDecayProtocolError);
      await expect(client.operations.application_fact_store_curate({}))
        .rejects.toBeInstanceOf(TraceDecayProtocolError);
      for (const nextSequence of [-1, 1.5, "18446744073709551616", "0x1"] as const) {
        await expect(
          client.streamOperation("request.operation", { resume: { resumeToken: "resume", nextSequence } }).next(),
        ).rejects.toBeInstanceOf(TraceDecayProtocolError);
      }
      await expect(client.streamOperation("request.operation", { maxReconnects: -1 }).next())
        .rejects.toBeInstanceOf(TraceDecayProtocolError);
      expect(daemon.requests).toHaveLength(0);
    });
  });
});

describe("native binding resolution", () => {
  it("reports a missing binding as a typed transport failure on first HTTP use", async () => {
    const { createClient, TraceDecayTransportError: TransportError } = await import("../src/client");
    const client = createClient({
      baseUrl: "http://127.0.0.1:1", projectId: "project.sdk", token: "t",
      addon: "/nonexistent/tracedecay_sdk_node.node",
    });
    await expect(client.operations.workflow_list_definitions({})).rejects.toBeInstanceOf(TransportError);
  });
});
