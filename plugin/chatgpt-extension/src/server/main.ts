import { readFile } from "node:fs/promises";
import { createServer as createHttpServer, type IncomingMessage, type ServerResponse } from "node:http";
import { parseArgs } from "node:util";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { resolveProfileRoot } from "./authority.js";
import { DaemonBridge } from "./bridge.js";
import { registerTraceDecayExtension } from "./register.js";

export const SERVER_INFO = { name: "tracedecay-chatgpt-extension", title: "TraceDecay", version: "0.0.0" } as const;

export type ServerAssets = { readonly html: string; readonly iconSvg: string };

export async function loadAssets(): Promise<ServerAssets> {
  const [html, iconSvg] = await Promise.all([
    readFile(new URL("./app.html", import.meta.url), "utf8"),
    readFile(new URL("../assets/icon.svg", import.meta.url), "utf8"),
  ]);
  return { html, iconSvg };
}

export type CreateServerOptions = {
  readonly bridge: DaemonBridge;
  readonly assets: ServerAssets;
};

export function createExtensionServer({ bridge, assets }: CreateServerOptions): McpServer {
  const server = new McpServer({
    ...SERVER_INFO,
    icons: [{ src: `data:image/svg+xml,${encodeURIComponent(assets.iconSvg)}`, mimeType: "image/svg+xml" }],
  });
  registerTraceDecayExtension({ server, bridge, html: assets.html, iconSvg: assets.iconSvg });
  return server;
}

export function bridgeFromEnvironment(env: NodeJS.ProcessEnv, binary: string | undefined): DaemonBridge {
  return new DaemonBridge({
    binary: binary ?? env.TRACEDECAY_BIN ?? "tracedecay",
    profileRoot: resolveProfileRoot(env),
    env,
    cwd: process.cwd(),
  });
}

export type LoopbackHttpServer = {
  readonly url: string;
  close(): Promise<void>;
};

/**
 * Streamable HTTP on a loopback address only. Each request gets its own
 * stateless transport and McpServer over the shared bridge; DNS-rebinding
 * protection pins the accepted Host header to the bound address.
 */
export async function serveLoopbackHttp(createServer: () => McpServer, host: string, port: number): Promise<LoopbackHttpServer> {
  if (host !== "127.0.0.1" && host !== "localhost" && host !== "::1") {
    throw new Error(`--http must bind a loopback address, got ${host}`);
  }
  let boundPort = port;
  const http = createHttpServer((request, response) => {
    void handleHttp(createServer, request, response, host, boundPort).catch((error: unknown) => {
      if (!response.headersSent) {
        response.writeHead(500, { "content-type": "application/json" });
      }
      response.end(JSON.stringify({ jsonrpc: "2.0", error: { code: -32603, message: String(error) }, id: null }));
    });
  });
  await new Promise<void>((resolve, reject) => {
    http.once("error", reject);
    http.listen(port, host, () => resolve());
  });
  const address = http.address();
  if (typeof address !== "object" || address === null) throw new Error("loopback HTTP server did not report a bound address");
  boundPort = address.port;
  return {
    url: `http://${host}:${boundPort}/mcp`,
    close: () =>
      new Promise<void>((resolve, reject) => {
        http.close((error) => (error === undefined ? resolve() : reject(error)));
        http.closeAllConnections();
      }),
  };
}

async function handleHttp(createServer: () => McpServer, request: IncomingMessage, response: ServerResponse, host: string, port: number): Promise<void> {
  const url = new URL(request.url ?? "/", `http://${host}:${port}`);
  if (url.pathname !== "/mcp") {
    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: "not_found" }));
    return;
  }
  const transport = new StreamableHTTPServerTransport({
    sessionIdGenerator: undefined,
    enableJsonResponse: true,
    enableDnsRebindingProtection: true,
    allowedHosts: [`${host}:${port}`, `localhost:${port}`, `127.0.0.1:${port}`, `[::1]:${port}`],
  });
  const server = createServer();
  response.on("close", () => {
    void transport.close().finally(() => server.close());
  });
  await server.connect(transport);
  await transport.handleRequest(request, response);
}

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      http: { type: "string" },
      binary: { type: "string" },
    },
  });
  const bridge = bridgeFromEnvironment(process.env, values.binary);
  const assets = await loadAssets();
  const createServer = () => createExtensionServer({ bridge, assets });
  const shutdown = async () => {
    await bridge.close();
  };
  process.once("SIGINT", () => void shutdown().finally(() => process.exit(0)));
  process.once("SIGTERM", () => void shutdown().finally(() => process.exit(0)));
  if (values.http !== undefined) {
    const [host, portText] = splitHostPort(values.http);
    const port = Number.parseInt(portText, 10);
    if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error(`invalid --http port: ${portText}`);
    const listening = await serveLoopbackHttp(createServer, host, port);
    process.stderr.write(`tracedecay-chatgpt-extension listening on ${listening.url}\n`);
    return;
  }
  const server = createServer();
  server.server.onclose = () => void shutdown();
  await server.connect(new StdioServerTransport());
}

function splitHostPort(value: string): [string, string] {
  const index = value.lastIndexOf(":");
  if (index === -1) return ["127.0.0.1", value];
  return [value.slice(0, index), value.slice(index + 1)];
}

const invokedDirectly = process.argv[1] !== undefined && import.meta.url === new URL(`file://${process.argv[1]}`).href;
if (invokedDirectly) {
  main().catch((error: unknown) => {
    process.stderr.write(`${error instanceof Error ? (error.stack ?? error.message) : String(error)}\n`);
    process.exit(1);
  });
}
