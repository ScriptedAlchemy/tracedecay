import { once } from "node:events";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { isAbsolute, resolve } from "node:path";

import { createClient, type ClientOptions, type TraceDecayClient } from "../../src/client";

/** One request the fixture daemon received, with its fully read body. */
export interface RecordedRequest {
  method: string;
  url: string;
  headers: IncomingMessage["headers"];
  body: string;
}

export type Handler = (request: RecordedRequest, response: ServerResponse) => void | Promise<void>;

export interface FixtureDaemon {
  baseUrl: string;
  requests: RecordedRequest[];
  /** Resolves when the client closed the connection of the nth request. */
  closed(index: number): Promise<void>;
  client(options?: Partial<ClientOptions>): TraceDecayClient;
}

/**
 * The binding the Bazel test target built. Tests pass it explicitly, the same
 * path an offline host uses.
 */
export function addonPath(): string {
  const addon = process.env.TRACEDECAY_SDK_ADDON;
  if (addon === undefined || addon === "") {
    throw new Error("TRACEDECAY_SDK_ADDON must name the built tracedecay_sdk_node.node");
  }
  if (isAbsolute(addon)) return addon;
  // Bazel passes a runfiles-root path; rules_js runs tests from the package.
  const runfiles = process.env.JS_BINARY__RUNFILES;
  if (runfiles === undefined) throw new Error("a relative TRACEDECAY_SDK_ADDON needs Bazel runfiles");
  return resolve(runfiles, "_main", addon);
}

/**
 * A real loopback HTTP daemon that answers request n with handler n. A request
 * past the script answers 599 so an unexpected reconnect is visible.
 */
export async function withDaemon(
  handlers: readonly Handler[],
  run: (daemon: FixtureDaemon) => Promise<void>,
): Promise<void> {
  const requests: RecordedRequest[] = [];
  const closures: Promise<void>[] = [];
  const server = createServer((incoming, response) => {
    const index = requests.length;
    const chunks: Buffer[] = [];
    const recorded: RecordedRequest = {
      method: incoming.method ?? "",
      url: incoming.url ?? "",
      headers: incoming.headers,
      body: "",
    };
    requests.push(recorded);
    closures[index] = new Promise((resolve) => response.once("close", () => resolve()));
    incoming.on("data", (chunk: Buffer) => chunks.push(chunk));
    incoming.on("end", () => {
      recorded.body = Buffer.concat(chunks).toString("utf8");
      const handler = handlers[index];
      if (handler === undefined) {
        response.writeHead(599).end();
        return;
      }
      void Promise.resolve(handler(recorded, response)).catch((error: unknown) => {
        response.destroy(error instanceof Error ? error : new Error(String(error)));
      });
    });
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (address === null || typeof address === "string") throw new Error("fixture has no TCP address");
  const baseUrl = `http://127.0.0.1:${address.port}`;
  try {
    await run({
      baseUrl,
      requests,
      closed: (index) => closures[index] ?? Promise.reject(new Error(`request ${index} never arrived`)),
      client: (options = {}) =>
        createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "isolated-test-token",
          addon: addonPath(),
          ...options,
        }),
    });
  } finally {
    const closed = once(server, "close");
    server.closeAllConnections();
    server.close();
    await closed;
  }
}

export function json(status: number, value: unknown): Handler {
  return (_request, response) => {
    response.writeHead(status, { "content-type": "application/json" });
    response.end(JSON.stringify(value));
  };
}

export function events(body: string): Handler {
  return (_request, response) => {
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.end(body);
  };
}

/** Sends `body` and then holds the connection open until the client leaves. */
export function heldEvents(body: string): Handler {
  return (_request, response) => {
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.write(body);
  };
}

/** Never answers; the client must interrupt or the test times out. */
export const silent: Handler = () => undefined;
