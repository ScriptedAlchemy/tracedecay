import { AsyncLocalStorage } from "node:async_hooks";
import { StdioClientTransport } from "@modelcontextprotocol/client/stdio";
import { Client, ProtocolError, SdkError, type CallToolResult } from "@modelcontextprotocol/client";
import { z } from "zod";
import type { Failure, FailureKind } from "../shared/view.js";

const REPEATED_QUERY_HINT_PREFIX = "tracedecay_repeated_query:";
const MAX_RESPONSE_HANDLE_BYTES = 1024 * 1024;
const MAX_RESPONSE_HANDLE_PAGES = 1024;
const SafeOffsetSchema = z.int().nonnegative();

/**
 * Collects the repeated-query hints `tracedecay serve` attaches to the daemon
 * reads made while handling one explorer tool call. Serve keeps the hint
 * window for its session, and the explorer holds one serve session per
 * project for its host, so the explorer relays serve's hint instead of
 * keeping a window of its own.
 */
export const relayedHints = new AsyncLocalStorage<string[]>();

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

function isResponseHandleCandidate(payload: unknown): boolean {
  return typeof payload === "object" && payload !== null && !Array.isArray(payload) &&
    "handle" in payload && "truncated" in payload && payload.truncated === true;
}

function unicodeScalarCount(value: string): number | null {
  let count = 0;
  for (let index = 0; index < value.length; index += 1) {
    const current = value.charCodeAt(index);
    if (current >= 0xd800 && current <= 0xdbff) {
      const following = value.charCodeAt(index + 1);
      if (following < 0xdc00 || following > 0xdfff || Number.isNaN(following)) return null;
      index += 1;
    } else if (current >= 0xdc00 && current <= 0xdfff) {
      return null;
    }
    count += 1;
  }
  return count;
}

function protocolFailure(code: string, message: string): DaemonFailure {
  return new DaemonFailure({ kind: "protocol", code, message });
}

function budgetFailure(code: string, message: string): DaemonFailure {
  return new DaemonFailure({ kind: "unavailable", code, message });
}

export function toFailure(error: unknown): Failure {
  if (error instanceof DaemonFailure) return error.failure;
  if (error instanceof ProtocolError) {
    const data = error.data;
    const code =
      typeof data === "object" && data !== null && "code" in data && typeof data.code === "string"
        ? data.code
        : `jsonrpc_${error.code}`;
    return { kind: classifyCode(code), code, message: error.message };
  }
  if (error instanceof SdkError) {
    const code = `sdk_${error.code.toLowerCase()}`;
    return { kind: classifyCode(code), code, message: error.message };
  }
  if (error instanceof Error) {
    return { kind: "protocol", code: error.name, message: error.message };
  }
  return { kind: "protocol", code: "unknown", message: String(error) };
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
  handle: z.string().min(1),
  original_chars: SafeOffsetSchema,
  preview: z.string(),
  preview_chars: SafeOffsetSchema,
  truncated: z.literal(true),
});

const RetrievePageSchema = z.object({
  handle: z.string().min(1),
  expired: z.literal(false),
  original_chars: SafeOffsetSchema,
  total_chars: SafeOffsetSchema,
  content: z.string(),
  offset: SafeOffsetSchema,
  next_offset: SafeOffsetSchema.nullable().optional(),
  has_more: z.boolean(),
});

const RetrieveMissingSchema = z.object({
  handle: z.string().min(1),
  expired: z.boolean().nullable().optional(),
  content: z.null().optional(),
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
  readonly #transportClosed: Promise<void>;
  #connected: Promise<void> | null = null;
  #closing: Promise<void> | null = null;
  #childStarted = false;
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
    this.#transportClosed = new Promise((resolve) => {
      this.#transport.onclose = () => {
        this.#closed = true;
        resolve();
      };
    });
  }

  get closed(): boolean {
    return this.#closed;
  }

  async connect(): Promise<void> {
    this.#assertOpen();
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
      // Client.connect starts stdio synchronously. A failed handshake can
      // clear its PID before the SDK's unawaited child cleanup finishes.
      this.#childStarted = this.#transport.pid !== null;
    }
    await this.#connected;
    this.#assertOpen();
  }

  serverInfo(): { name: string; version: string } | null {
    const info = this.#client.getServerVersion();
    return info === undefined ? null : { name: info.name, version: info.version };
  }

  async listToolNames(): Promise<readonly string[]> {
    await this.connect();
    this.#assertOpen();
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
    this.#assertOpen();
    const result = await this.#callTool({ name: toolName, arguments: { ...args, format: "json" } }, signal);
    for (const item of result.content) {
      if (item.type === "text" && item.text.startsWith(REPEATED_QUERY_HINT_PREFIX)) relayedHints.getStore()?.push(item.text);
    }
    let payload = payloadOf(result, toolName);
    if (isResponseHandleCandidate(payload)) {
      const handle = ResponseHandleSchema.safeParse(payload);
      if (!handle.success) {
        throw protocolFailure("response_handle_malformed", "truncated response metadata is malformed");
      }
      if (unicodeScalarCount(handle.data.preview) !== handle.data.preview_chars) {
        throw protocolFailure("response_handle_malformed", "truncated response preview length is inconsistent");
      }
      if (handle.data.original_chars > MAX_RESPONSE_HANDLE_BYTES) {
        throw budgetFailure("response_handle_too_large", `response handle exceeds the ${MAX_RESPONSE_HANDLE_BYTES}-byte reassembly budget`);
      }
      payload = parseJson(await this.#retrieveAll(handle.data, signal), toolName);
    }
    return payload;
  }

  async #retrieveAll(handle: z.infer<typeof ResponseHandleSchema>, signal?: AbortSignal): Promise<string> {
    let offset = 0;
    let retainedBytes = 0;
    const chunks: string[] = [];
    for (;;) {
      if (chunks.length === MAX_RESPONSE_HANDLE_PAGES) {
        throw budgetFailure("response_handle_page_limit", `response handle exceeds the ${MAX_RESPONSE_HANDLE_PAGES}-page reassembly limit`);
      }
      const result = await this.#callTool(
        { name: "tracedecay_retrieve", arguments: { handle: handle.handle, offset, format: "json" } },
        signal,
      );
      const page = payloadOf(result, "tracedecay_retrieve");
      const parsed = RetrievePageSchema.safeParse(page);
      if (!parsed.success) {
        const missing = RetrieveMissingSchema.safeParse(page);
        if (missing.success && missing.data.handle === handle.handle) {
          throw new DaemonFailure({
            kind: "unavailable",
            code: missing.data.reason_code,
            message: `response handle ${handle.handle} is no longer available (${missing.data.reason_code})`,
          });
        }
        throw new DaemonFailure({
          kind: "protocol",
          code: "retrieve_page_malformed",
          message: `tracedecay_retrieve returned an unexpected page shape for ${handle.handle}`,
        });
      }
      const data = parsed.data;
      const scalarCount = unicodeScalarCount(data.content);
      const expectedNextOffset = scalarCount === null ? null : offset + scalarCount;
      if (
        data.handle !== handle.handle ||
        data.original_chars !== handle.original_chars ||
        data.total_chars !== handle.original_chars ||
        data.offset !== offset ||
        expectedNextOffset === null ||
        !Number.isSafeInteger(expectedNextOffset) ||
        expectedNextOffset > handle.original_chars
      ) {
        throw protocolFailure("retrieve_page_malformed", `tracedecay_retrieve returned inconsistent page metadata for ${handle.handle}`);
      }

      const pageBytes = Buffer.byteLength(data.content, "utf8");
      if (pageBytes > MAX_RESPONSE_HANDLE_BYTES - retainedBytes) {
        throw budgetFailure("response_handle_too_large", `response handle exceeds the ${MAX_RESPONSE_HANDLE_BYTES}-byte reassembly budget`);
      }
      retainedBytes += pageBytes;
      chunks.push(data.content);

      if (data.has_more) {
        if (
          data.next_offset === null || data.next_offset === undefined ||
          data.next_offset <= offset || data.next_offset !== expectedNextOffset ||
          expectedNextOffset >= handle.original_chars
        ) {
          throw protocolFailure("retrieve_page_malformed", `tracedecay_retrieve returned an invalid continuation for ${handle.handle}`);
        }
        offset = data.next_offset;
        continue;
      }
      if ((data.next_offset !== null && data.next_offset !== undefined) || expectedNextOffset !== handle.original_chars) {
        throw protocolFailure("retrieve_page_malformed", `tracedecay_retrieve returned an incomplete terminal page for ${handle.handle}`);
      }
      return chunks.join("");
    }
  }

  async #callTool(params: { readonly name: string; readonly arguments: Record<string, unknown> }, signal?: AbortSignal): Promise<CallToolResult> {
    this.#assertOpen();
    if (signal?.aborted) throw signal.reason ?? new DOMException("The operation was aborted", "AbortError");
    try {
      const result = await this.#client.callTool(params, { ...(signal === undefined ? {} : { signal }) });
      if (signal?.aborted) throw signal.reason ?? new DOMException("The operation was aborted", "AbortError");
      return result;
    } catch (error) {
      if (signal?.aborted) throw signal.reason ?? error;
      throw error;
    }
  }

  #assertOpen(): void {
    if (this.#closed) {
      throw new DaemonFailure({ kind: "disconnected", code: "serve_closed", message: "tracedecay serve exited" });
    }
  }

  close(): Promise<void> {
    this.#closed = true;
    this.#closing ??= this.#close();
    return this.#closing;
  }

  async #close(): Promise<void> {
    // Abort an active handshake before joining it. The transport clears its
    // PID before the child exits, so retain the actual close notification.
    const transportExited = this.#childStarted || this.#transport.pid !== null ? this.#transportClosed : null;
    const [transportResult] = await Promise.allSettled([
      (async () => {
        await this.#transport.close();
        await transportExited;
      })(),
      this.#connected,
    ]);
    // A pending connect may attach/start the transport after shutdown began.
    // Close the client after it settles, even when the first cleanup failed.
    const clientTransportExited = this.#childStarted || this.#transport.pid !== null ? this.#transportClosed : null;
    const [clientResult] = await Promise.allSettled([
      (async () => {
        await this.#client.close();
        await clientTransportExited;
      })(),
    ]);
    const errors = [transportResult, clientResult].flatMap((result) =>
      result.status === "rejected" ? [result.reason] : [],
    );
    if (errors.length === 1) throw errors[0];
    if (errors.length > 1) throw new AggregateError(errors, "Failed to close tracedecay serve session");
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
