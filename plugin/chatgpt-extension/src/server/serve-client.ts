import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { McpError, type CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import type { Failure, FailureKind } from "../shared/view.js";

/**
 * One `tracedecay serve` stdio session. The daemon scopes project tools to
 * the `--path` the session was started with, so the bridge keeps one session
 * per authorized project plus one project-less session for registry tools.
 */
export class DaemonFailure extends Error {
  readonly failure: Failure;
  constructor(failure: Failure, options?: ErrorOptions) {
    super(failure.message, options);
    this.name = "DaemonFailure";
    this.failure = failure;
  }
}

export function toFailure(error: unknown): Failure {
  if (error instanceof DaemonFailure) return error.failure;
  if (error instanceof McpError) {
    const data = error.data;
    const code =
      typeof data === "object" && data !== null && "code" in data && typeof data.code === "string"
        ? data.code
        : `jsonrpc_${error.code}`;
    return { kind: classifyCode(code), code, message: stripMcpPrefix(error.message) };
  }
  if (error instanceof Error) {
    return { kind: "protocol", code: error.name, message: error.message };
  }
  return { kind: "protocol", code: "unknown", message: String(error) };
}

function stripMcpPrefix(message: string): string {
  return message.replace(/^MCP error -?\d+: /u, "");
}

export function classifyCode(code: string): FailureKind {
  if (code.includes("not_authorized") || code === "denied" || code.startsWith("authorization")) return "denied";
  if (code.includes("not_found") || code === "project_required") return "not_found";
  if (code.includes("stale")) return "stale";
  if (code.includes("unavailable") || code.includes("warming") || code.includes("saturated")) return "unavailable";
  if (code.includes("invalid") || code.includes("malformed")) return "invalid_request";
  if (code.includes("disconnected") || code.includes("socket")) return "disconnected";
  return "protocol";
}

const ResponseHandleSchema = z.object({
  handle: z.string(),
  original_chars: z.number().int().nonnegative(),
  preview: z.string(),
  preview_chars: z.number().int().nonnegative(),
  truncated: z.literal(true),
});

const RetrievePageSchema = z.object({
  content: z.string(),
  offset: z.number().int().nonnegative(),
  next_offset: z.number().int().nonnegative().nullable().optional(),
  has_more: z.boolean(),
});

const RetrieveMissingSchema = z.object({
  handle: z.string(),
  reason_code: z.string(),
});

export type ServeSessionOptions = {
  readonly binary: string;
  readonly projectRoot: string | null;
  readonly env: NodeJS.ProcessEnv;
  readonly cwd: string;
};

// The serve child needs home/config resolution, locale, temp dirs, and any
// `TRACEDECAY_*` overrides — nothing else. The host process env can carry
// unrelated credentials, so the child inherits an allowlist, not the whole
// environment.
const CHILD_ENV_EXACT = new Set([
  "PATH",
  "HOME",
  "USERPROFILE",
  "HOMEDRIVE",
  "HOMEPATH",
  "XDG_CONFIG_HOME",
  "XDG_DATA_HOME",
  "LANG",
  "LC_ALL",
  "LC_CTYPE",
  "TMPDIR",
  "TEMP",
  "TMP",
  "SystemRoot",
  "SystemDrive",
  "APPDATA",
  "LOCALAPPDATA",
  "RUST_LOG",
  "RUST_BACKTRACE",
  "RUST_LIB_BACKTRACE",
  "HTTP_PROXY",
  "HTTPS_PROXY",
  "ALL_PROXY",
  "NO_PROXY",
  "http_proxy",
  "https_proxy",
  "all_proxy",
  "no_proxy",
]);
const CHILD_ENV_PREFIX = "TRACEDECAY_";

export function childEnv(env: NodeJS.ProcessEnv): Record<string, string> {
  const child: Record<string, string> = {};
  for (const [key, value] of Object.entries(env)) {
    if (value === undefined) continue;
    if (CHILD_ENV_EXACT.has(key) || key.startsWith(CHILD_ENV_PREFIX)) child[key] = value;
  }
  return child;
}

export class ServeSession {
  readonly #client: Client;
  readonly #transport: StdioClientTransport;
  #connected: Promise<void> | null = null;
  #closed = false;

  constructor(private readonly options: ServeSessionOptions) {
    const args = ["serve"];
    if (options.projectRoot !== null) args.push("--path", options.projectRoot);
    this.#transport = new StdioClientTransport({
      command: options.binary,
      args,
      env: childEnv(options.env),
      cwd: options.cwd,
      // A piped stderr nobody reads fills the OS buffer and blocks the
      // daemon. stdout is the MCP channel, so stderr goes to ours.
      stderr: "inherit",
    });
    this.#client = new Client({ name: "tracedecay-chatgpt-extension", version: "0.0.0" });
    this.#transport.onclose = () => {
      this.#closed = true;
    };
  }

  get closed(): boolean {
    return this.#closed;
  }

  async connect(): Promise<void> {
    if (this.#connected === null) {
      this.#connected = this.#client.connect(this.#transport).catch((error: unknown) => {
        this.#closed = true;
        throw new DaemonFailure(
          {
            kind: "disconnected",
            code: "serve_spawn_failed",
            message: `Cannot start ${this.options.binary} serve: ${error instanceof Error ? error.message : String(error)}`,
          },
          { cause: error },
        );
      });
    }
    await this.#connected;
  }

  serverInfo(): { name: string; version: string } | null {
    const info = this.#client.getServerVersion();
    return info === undefined ? null : { name: info.name, version: info.version };
  }

  async listToolNames(): Promise<readonly string[]> {
    await this.connect();
    const listed = await this.#client.listTools();
    return listed.tools.map((tool) => tool.name);
  }

  /**
   * Calls one daemon tool with `format: "json"` and returns the decoded JSON
   * payload, reassembling bounded response handles through
   * `tracedecay_retrieve` so callers always see the complete payload.
   */
  async callJson(toolName: string, args: Record<string, unknown>, signal?: AbortSignal): Promise<unknown> {
    await this.connect();
    if (this.#closed) {
      throw new DaemonFailure({ kind: "disconnected", code: "serve_closed", message: "tracedecay serve exited" });
    }
    const result = await this.#client.callTool({ name: toolName, arguments: { ...args, format: "json" } }, undefined, {
      ...(signal === undefined ? {} : { signal }),
    });
    let payload = payloadOf(result as CallToolResult, toolName);
    const handle = ResponseHandleSchema.safeParse(payload);
    if (handle.success) {
      payload = parseJson(await this.#retrieveAll(handle.data.handle, signal), toolName);
    }
    return payload;
  }

  async #retrieveAll(handle: string, signal?: AbortSignal): Promise<string> {
    let offset = 0;
    let assembled = "";
    for (;;) {
      const result = await this.#client.callTool(
        { name: "tracedecay_retrieve", arguments: { handle, offset, format: "json" } },
        undefined,
        { ...(signal === undefined ? {} : { signal }) },
      );
      const page = payloadOf(result as CallToolResult, "tracedecay_retrieve");
      const parsed = RetrievePageSchema.safeParse(page);
      if (!parsed.success) {
        const missing = RetrieveMissingSchema.safeParse(page);
        throw new DaemonFailure({
          kind: missing.success ? "unavailable" : "protocol",
          code: missing.success ? missing.data.reason_code : "retrieve_page_malformed",
          message: missing.success
            ? `response handle ${handle} is no longer available (${missing.data.reason_code})`
            : `tracedecay_retrieve returned an unexpected page shape for ${handle}`,
        });
      }
      assembled += parsed.data.content;
      if (!parsed.data.has_more || parsed.data.next_offset === null || parsed.data.next_offset === undefined) {
        return assembled;
      }
      offset = parsed.data.next_offset;
    }
  }

  async close(): Promise<void> {
    this.#closed = true;
    await this.#client.close();
  }
}

// The daemon returns the payload as the first text item and may append
// further text items (for example a `tracedecay_metrics` accounting line).
function textOf(result: CallToolResult): string {
  for (const item of result.content) if (item.type === "text") return item.text;
  return "";
}

const ProblemSchema = z.object({ problem: z.object({ kind: z.string(), code: z.string(), message: z.string() }) });

// The daemon refuses a call only through `structuredContent.problem`. It also
// sets isError on typed outcomes such as a not-found node, whose payload the
// view model renders, so those still decode. The code matches failureFromSdk.
function payloadOf(result: CallToolResult, toolName: string): unknown {
  const problem = result.isError === true ? ProblemSchema.safeParse(result.structuredContent) : null;
  if (problem?.success) {
    const { kind, code, message } = problem.data.problem;
    const qualified = `${kind}/${code}`;
    throw new DaemonFailure({ kind: classifyCode(qualified), code: qualified, message });
  }
  return parseJson(textOf(result), toolName);
}

function parseJson(text: string, toolName: string): unknown {
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new DaemonFailure(
      {
        kind: "protocol",
        code: "non_json_tool_result",
        message: `${toolName} did not return JSON: ${text.slice(0, 200)}`,
      },
      { cause: error },
    );
  }
}
