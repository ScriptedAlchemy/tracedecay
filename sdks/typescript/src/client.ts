import {
  OPERATIONS,
  type AvailableOperationDescriptor,
  type HttpOperationDescriptor,
  type OperationName,
  type RequestFor,
  type ResponseFor,
  type ResultFor,
} from "./operations";
import {
  applyHttpRequestControls,
  type OperationRequestOptions,
  type PageOptions,
} from "./request-control";
export type { OperationRequestOptions, PageOptions } from "./request-control";
import {
  OPERATION_TERMINATIONS,
  applicationProblemBindingMatches,
  applicationProblemStatusMatches,
  factStoreCurateReceiptMatches,
  isHttpProblemEnvelope,
  type ApplicationProblemRecord,
  type HttpProblemEnvelope,
  type HttpSseEvent,
  type HttpSuccessEnvelope,
  type OperationReceipt,
} from "./types";

type FetchImplementation = typeof globalThis.fetch;

export interface ClientOptions {
  baseUrl: string;
  projectId: string;
  token: string;
  origin?: string;
  fetch?: FetchImplementation;
  mcp?: McpToolAdapter;
}

/** Adapter owned by the caller's MCP host for generated MCP tool bindings. */
export interface McpToolAdapter {
  callTool(
    toolName: string,
    request: unknown,
    options: { signal?: AbortSignal },
  ): Promise<unknown>;
}

export type OperationMethod<Name extends OperationName> = (
  request: RequestFor<Name>,
  options?: OperationRequestOptions,
) => Promise<ResponseFor<Name>>;

export type OperationMethods = {
  readonly [Name in OperationName]: OperationMethod<Name>;
};

export interface OperationStreamResume {
  resumeToken: string;
  /** Next undelivered sequence, not the open frame's publication watermark. */
  nextSequence: number | bigint | string;
}

export interface OperationStreamOptions {
  resume?: OperationStreamResume;
  signal?: AbortSignal;
  /**
   * Explicit opt-in; reconnection is disabled by default. Each reconnect
   * attempt, including one that fails to connect, spends this budget. Only
   * transport failures are retried, never the first connection. After the
   * budget is spent the generator throws one `TraceDecayDisconnectedError`.
   */
  maxReconnects?: number;
}

export interface OperationStreamEvent {
  event: string;
  id: string | null;
  data: HttpSseEvent<unknown>;
}

export interface OperationCancellation {
  status: "requested" | "already_requested" | "already_terminal";
  [key: string]: unknown;
}

export class TraceDecayTransportError extends Error {
  readonly cause: unknown;

  constructor(message: string, cause?: unknown) {
    super(message);
    this.name = new.target.name;
    this.cause = cause;
  }
}

export class TraceDecayAbortError extends TraceDecayTransportError {
  constructor(cause?: unknown) {
    super("the caller aborted the request", cause);
  }
}

export class TraceDecayDisconnectedError extends TraceDecayTransportError {}

export class TraceDecayAuthenticationError extends TraceDecayTransportError {
  readonly status: 401 | 403;

  constructor(status: 401 | 403) {
    super(
      status === 401
        ? "the daemon rejected HTTP authentication"
        : "the daemon rejected the HTTP origin",
    );
    this.status = status;
  }
}

export class TraceDecayProtocolError extends Error {
  readonly status?: number;
  readonly payload?: unknown;

  constructor(message: string, options: { status?: number; payload?: unknown } = {}) {
    super(message);
    this.name = new.target.name;
    this.status = options.status;
    this.payload = options.payload;
  }
}

export class TraceDecayMalformedResponseError extends TraceDecayProtocolError {}

export class TraceDecayProblemError extends Error {
  readonly status: number;
  readonly envelope: HttpProblemEnvelope;
  readonly problem: ApplicationProblemRecord;

  constructor(status: number, envelope: HttpProblemEnvelope) {
    super(`${envelope.problem.kind}/${envelope.problem.code}: ${envelope.problem.message}`);
    this.name = new.target.name;
    this.status = status;
    this.envelope = envelope;
    this.problem = envelope.problem;
  }
}

export class TraceDecayDeniedError extends TraceDecayProblemError {}
export class TraceDecayUnavailableError extends TraceDecayProblemError {}
export class TraceDecayUnsupportedError extends TraceDecayProblemError {}
export class TraceDecayStaleError extends TraceDecayProblemError {}
export class TraceDecayConflictError extends TraceDecayProblemError {}
export class TraceDecayPartialEffectError extends TraceDecayProblemError {}
export class TraceDecayResetRequiredError extends TraceDecayProblemError {}
export class TraceDecaySaturatedError extends TraceDecayProblemError {}
export class TraceDecayCancelledError extends TraceDecayProblemError {}
export class TraceDecayTimedOutError extends TraceDecayProblemError {}
export class TraceDecayInvalidRequestError extends TraceDecayProblemError {}

interface SseFrame {
  event: string;
  id: string | null;
  data: string;
  retryDelayMs?: number;
}

interface StreamFrontier {
  resumeToken?: string;
}

const TERMINAL_EVENTS: ReadonlySet<string> = new Set(OPERATION_TERMINATIONS);
const MAX_PAGE_SIZE = 1_000;
const MAX_OPAQUE_CURSOR_BYTES = 4_096;
const MAX_RESUME_TOKEN_BYTES = 4_096;
const MAX_REQUEST_ID_BYTES = 512;
// Match the Rust SDK's raw-line and retained event/id/data policy. Accepted
// identifiers (512 bytes), test names (1 KiB), frontiers and terminal receipts
// fit comfortably within 64 KiB, including JSON escaping. Raw line endings
// count too; one additional byte detects overflow without retaining it.
const MAX_SSE_FRAME_BYTES = 64 * 1024;
const MAX_U64 = 18_446_744_073_709_551_615n;
const UTF8_ENCODER = new TextEncoder();

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isHttpOperationDescriptor<Request, Result>(
  descriptor: AvailableOperationDescriptor<string, Request, Result>,
): descriptor is HttpOperationDescriptor<string, Request, Result> {
  return descriptor.transport.kind === "http";
}

function isSafeUnsignedInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value);
}

function isBoundedOpaqueString(value: unknown, maximumBytes: number): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.trim() === value &&
    UTF8_ENCODER.encode(value).byteLength <= maximumBytes &&
    !/\p{Cc}/u.test(value)
  );
}

function hasMediaType(response: Response, expected: string): boolean {
  const contentType = response.headers.get("content-type");
  return (
    contentType !== null &&
    contentType.split(";", 1)[0]?.trim().toLowerCase() === expected
  );
}

function isReceipt(value: unknown): value is OperationReceipt {
  if (
    !isRecord(value) ||
    !isSafeInteger(value.started_at) ||
    !isSafeInteger(value.ended_at) ||
    value.ended_at < value.started_at ||
    !("effective_deadline" in value) ||
    (value.cancellation !== null && !isRecord(value.cancellation)) ||
    !isRecord(value.budget) ||
    !isSafeUnsignedInteger(value.budget.units_consumed) ||
    !isSafeUnsignedInteger(value.budget.bytes_consumed) ||
    !isSafeUnsignedInteger(value.budget.elapsed_micros) ||
    typeof value.termination !== "string"
  ) {
    return false;
  }
  return (
    (value.termination !== "cancelled" && value.termination !== "timed_out") ||
    isRecord(value.cancellation)
  );
}

// Page structure (counts, bounded sort contract, tagged cursor variants) is the
// generated decoder's authority; only receipt and payload semantics remain here.
function isDecodedSuccessEnvelope(value: unknown): boolean {
  if (
    !isRecord(value) ||
    !isRecord(value.outcome) ||
    !isRecord(value.outcome.value) ||
    !("payload" in value.outcome.value) ||
    !isReceipt(value.outcome.value.execution)
  ) {
    return false;
  }
  if (value.outcome.outcome === "evidence") {
    return (
      value.outcome.value.execution.termination !== "completed" ||
      value.outcome.value.payload !== null
    );
  }
  return value.outcome.outcome === "preview" || value.outcome.outcome === "effect";
}

function validatePageOptions(page: PageOptions | undefined): void {
  if (
    page?.size !== undefined &&
    (!Number.isSafeInteger(page.size) ||
      page.size < 1 ||
      page.size > MAX_PAGE_SIZE)
  ) {
    throw new TraceDecayProtocolError(
      `page size must be an integer between 1 and ${MAX_PAGE_SIZE}`,
    );
  }
  if (
    page?.cursor !== undefined &&
    !isBoundedOpaqueString(page.cursor, MAX_OPAQUE_CURSOR_BYTES)
  ) {
    throw new TraceDecayProtocolError("page cursor is not a canonical opaque cursor");
  }
}

function normalizeUnsignedDecimal(
  value: number | bigint | string,
  field: string,
): string {
  let parsed: bigint;
  if (typeof value === "number") {
    if (!isSafeUnsignedInteger(value)) {
      throw new TraceDecayProtocolError(`${field} must be an unsigned safe integer`);
    }
    parsed = BigInt(value);
  } else if (typeof value === "bigint") {
    parsed = value;
  } else if (/^[0-9]+$/.test(value)) {
    parsed = BigInt(value);
  } else {
    throw new TraceDecayProtocolError(`${field} must be unsigned decimal`);
  }
  if (parsed < 0n || parsed > MAX_U64) {
    throw new TraceDecayProtocolError(`${field} exceeds the canonical u64 range`);
  }
  return parsed.toString();
}

function problemError(
  status: number,
  envelope: HttpProblemEnvelope,
): TraceDecayProblemError {
  switch (envelope.problem.kind) {
    case "not_found_or_not_authorized":
      return new TraceDecayDeniedError(status, envelope);
    case "unavailable":
      return new TraceDecayUnavailableError(status, envelope);
    case "unsupported":
      return new TraceDecayUnsupportedError(status, envelope);
    case "stale":
      return new TraceDecayStaleError(status, envelope);
    case "conflict":
      return new TraceDecayConflictError(status, envelope);
    case "partial_effect":
      return new TraceDecayPartialEffectError(status, envelope);
    case "reset_required":
      return new TraceDecayResetRequiredError(status, envelope);
    case "saturated":
      return new TraceDecaySaturatedError(status, envelope);
    case "cancelled":
      return new TraceDecayCancelledError(status, envelope);
    case "timed_out":
      return new TraceDecayTimedOutError(status, envelope);
    case "invalid_request":
      return new TraceDecayInvalidRequestError(status, envelope);
    default:
      return new TraceDecayProblemError(status, envelope);
  }
}

function parseJson(text: string, status: number): unknown {
  try {
    return JSON.parse(text) as unknown;
  } catch (cause) {
    throw new TraceDecayMalformedResponseError("the daemon returned non-JSON data", {
      status,
      payload: { text, cause },
    });
  }
}

function incrementDecimal(value: string): string {
  return (BigInt(value) + 1n).toString();
}

async function waitForReconnect(
  delayMs: number,
  signal?: AbortSignal,
): Promise<void> {
  if (signal?.aborted) {
    throw new TraceDecayAbortError(signal.reason);
  }
  await new Promise<void>((resolve, reject) => {
    const complete = () => {
      signal?.removeEventListener("abort", abort);
      resolve();
    };
    const timer = setTimeout(complete, delayMs);
    const abort = () => {
      clearTimeout(timer);
      reject(new TraceDecayAbortError(signal?.reason));
    };
    signal?.addEventListener("abort", abort, { once: true });
  });
}

async function* parseSse(
  body: ReadableStream<Uint8Array>,
  initialLastEventId?: string,
): AsyncGenerator<SseFrame> {
  const reader = body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  const lineBuffer = new Uint8Array(MAX_SSE_FRAME_BYTES);
  let lineBytes = 0;
  let previousLineBytes = 0;
  let skipLf = false;
  let eventType = "";
  let eventBytes = 0;
  let lastEventId = initialLastEventId ?? "";
  let idBytes = UTF8_ENCODER.encode(lastEventId).byteLength;
  let data: string[] = [];
  let dataBytes = 0;
  let pendingFields = false;
  let retryDelayMs: number | undefined;

  const oversized = () => new TraceDecayMalformedResponseError(
    `the daemon returned an SSE line or frame exceeding ${MAX_SSE_FRAME_BYTES} bytes`,
    { payload: { limitBytes: MAX_SSE_FRAME_BYTES } },
  );
  const decodeUtf8 = (bytes: Uint8Array, stream: boolean): string => {
    try {
      return decoder.decode(bytes, { stream });
    } catch (cause) {
      throw new TraceDecayMalformedResponseError(
        "the daemon returned malformed UTF-8 event-stream data",
        { payload: cause },
      );
    }
  };

  const consumeLine = (line: string): SseFrame | null => {
    if (line === "") {
      pendingFields = false;
      eventBytes = 0;
      if (data.length === 0) {
        eventType = "";
        return null;
      }
      const result = {
        event: eventType === "" ? "message" : eventType,
        id: lastEventId === "" ? null : lastEventId,
        data: data.join("\n"),
        ...(retryDelayMs === undefined ? {} : { retryDelayMs }),
      };
      eventType = "";
      data = [];
      dataBytes = 0;
      return result;
    }
    if (line.startsWith(":")) {
      return null;
    }
    pendingFields = true;
    const colon = line.indexOf(":");
    const field = colon < 0 ? line : line.slice(0, colon);
    let fieldValue = colon < 0 ? "" : line.slice(colon + 1);
    if (fieldValue.startsWith(" ")) {
      fieldValue = fieldValue.slice(1);
    }
    const valueBytes = UTF8_ENCODER.encode(fieldValue).byteLength;
    if (
      (field === "event" || field === "id" || field === "data") &&
      eventBytes + idBytes + dataBytes + valueBytes + 1 > MAX_SSE_FRAME_BYTES
    ) {
      // Keep the Rust client's conservative replacement allowance and count
      // the separator before extending data. Persistent IDs retain their
      // charge across empty frames and dispatch boundaries.
      throw oversized();
    }
    switch (field) {
      case "event":
        eventType = fieldValue;
        eventBytes = valueBytes;
        break;
      case "id":
        if (!fieldValue.includes("\0")) {
          lastEventId = fieldValue;
          idBytes = valueBytes;
        }
        break;
      case "data":
        dataBytes += valueBytes + (data.length === 0 ? 0 : 1);
        data.push(fieldValue);
        break;
      case "retry":
        if (/^[0-9]+$/.test(fieldValue)) {
          const parsed = BigInt(fieldValue);
          retryDelayMs =
            parsed > 2_147_483_647n ? 2_147_483_647 : Number(parsed);
        }
        break;
      default:
        break;
    }
    return null;
  };

  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) {
        decodeUtf8(lineBuffer.subarray(0, lineBytes), false);
        break;
      }
      // Inspect each incoming byte once and copy only into the bounded line.
      // A transport may supply a huge chunk; never decode or retain it whole.
      for (let index = 0; index < chunk.value.length; index += 1) {
        const byte = chunk.value[index];
        if (skipLf) {
          skipLf = false;
          if (byte === 10) {
            if (previousLineBytes === MAX_SSE_FRAME_BYTES) throw oversized();
            continue;
          }
        }
        if (lineBytes === MAX_SSE_FRAME_BYTES) throw oversized();
        lineBuffer[lineBytes++] = byte;
        if (byte !== 10 && byte !== 13) continue;
        // Decode the delimiter too, so a malformed code point cannot straddle
        // lines. One decoder preserves BOM handling for the whole stream.
        const line = decodeUtf8(lineBuffer.subarray(0, lineBytes), true).slice(0, -1);
        previousLineBytes = lineBytes;
        lineBytes = 0;
        skipLf = byte === 13;
        const frame = consumeLine(line);
        if (frame !== null) {
          yield frame;
        }
      }
    }
    if (lineBytes > 0 || pendingFields || data.length > 0) {
      throw new TraceDecayDisconnectedError(
        "operation stream ended with an incomplete SSE event",
      );
    }
  } catch (cause) {
    if (
      cause instanceof TraceDecayProtocolError ||
      cause instanceof TraceDecayTransportError
    ) {
      throw cause;
    }
    throw new TraceDecayDisconnectedError(
      "operation stream transport was interrupted",
      cause,
    );
  } finally {
    reader.releaseLock();
  }
}

function decodeSse(
  frame: SseFrame,
  status: number,
  expectedCorrelationId: string,
): { event: HttpSseEvent<unknown>; sequence?: string; lastSequence?: string; frontier?: StreamFrontier } {
  const value = parseJson(frame.data, status);
  if (
    !isRecord(value) ||
    typeof value.event !== "string" ||
    value.event !== frame.event ||
    !isRecord(value.data)
  ) {
    throw new TraceDecayMalformedResponseError(
      "the daemon returned a malformed SSE event",
      { status, payload: value },
    );
  }
  const eventName = value.event;
  const eventData = value.data;

  if (eventName === "open") {
    const frontier = eventData.frontier;
    if (
      eventData.correlation_id !== expectedCorrelationId ||
      !isRecord(frontier) ||
      !isSafeUnsignedInteger(frontier.next_sequence) ||
      !isSafeUnsignedInteger(frontier.retained_from_sequence) ||
      frontier.retained_from_sequence > frontier.next_sequence ||
      (frontier.resume_token !== null &&
        typeof frontier.resume_token !== "string")
    ) {
      throw new TraceDecayMalformedResponseError(
        "the daemon returned a malformed SSE open frontier",
        { status, payload: value },
      );
    }
    const event: HttpSseEvent<unknown> = {
      ...value,
      event: eventName,
      data: eventData,
    };
    return {
      event,
      frontier: {
        ...(typeof frontier.resume_token === "string"
          ? { resumeToken: frontier.resume_token }
          : {}),
      },
    };
  }

  if (!isSafeUnsignedInteger(eventData.sequence) || frame.id === null) {
    throw new TraceDecayMalformedResponseError(
      "the daemon returned an SSE event without a valid sequence",
      { status, payload: value },
    );
  }
  const sequence = String(eventData.sequence);
  if (frame.id !== sequence) {
    throw new TraceDecayMalformedResponseError(
      "the SSE event ID does not match its canonical sequence",
      { status, payload: value },
    );
  }
  if (eventName === "item" && !("item" in eventData)) {
    throw new TraceDecayMalformedResponseError("the SSE item payload is missing", {
      status,
      payload: value,
    });
  }
  if (
    eventName === "progress" &&
    (!isSafeUnsignedInteger(eventData.completed) ||
      (eventData.total !== null && !isSafeUnsignedInteger(eventData.total)))
  ) {
    throw new TraceDecayMalformedResponseError("the SSE progress payload is malformed", {
      status,
      payload: value,
    });
  }
  if (eventName === "resume_gap") {
    const gap = eventData.gap;
    if (
      !isRecord(gap) ||
      !isSafeUnsignedInteger(gap.first_missing_sequence) ||
      !isSafeUnsignedInteger(gap.last_missing_sequence) ||
      gap.first_missing_sequence !== eventData.sequence ||
      gap.first_missing_sequence > gap.last_missing_sequence ||
      !isRecord(gap.frontier) ||
      !isSafeUnsignedInteger(gap.frontier.next_sequence) ||
      !isSafeUnsignedInteger(gap.frontier.retained_from_sequence) ||
      gap.frontier.retained_from_sequence > gap.frontier.next_sequence ||
      gap.last_missing_sequence >= gap.frontier.next_sequence ||
      (gap.frontier.resume_token !== null &&
        typeof gap.frontier.resume_token !== "string")
    ) {
      throw new TraceDecayMalformedResponseError("the SSE resume gap is malformed", {
        status,
        payload: value,
      });
    }
  }
  if (TERMINAL_EVENTS.has(eventName)) {
    const terminal = eventData.terminal;
    if (!isRecord(terminal) || !isReceipt(terminal.receipt)) {
      throw new TraceDecayMalformedResponseError("the SSE terminal event is malformed", {
        status,
        payload: value,
      });
    }
    if (
      terminal.termination !== eventName ||
      terminal.receipt.termination !== eventName
    ) {
      throw new TraceDecayMalformedResponseError(
        "the SSE terminal event is inconsistent",
        { status, payload: value },
      );
    }
  }
  const event: HttpSseEvent<unknown> = {
    ...value,
    event: eventName,
    data: eventData,
  };
  if (eventName === "resume_gap") {
    const gap = eventData.gap as Record<string, unknown>;
    const frontier = gap.frontier as Record<string, unknown>;
    return {
      event,
      sequence,
      lastSequence: String(gap.last_missing_sequence),
      frontier: {
        ...(typeof frontier.resume_token === "string"
          ? { resumeToken: frontier.resume_token }
          : {}),
      },
    };
  }
  return { event, sequence };
}

export class TraceDecayClient {
  readonly operations: OperationMethods;

  private readonly projectRoot: string;
  private readonly applicationRoot: string;
  private readonly token: string;
  private readonly origin: string;
  private readonly doFetch: FetchImplementation;
  private readonly mcp: McpToolAdapter | undefined;

  constructor(options: ClientOptions) {
    const baseUrl = new URL(options.baseUrl);
    if (
      (baseUrl.protocol !== "http:" && baseUrl.protocol !== "https:") ||
      baseUrl.username !== "" ||
      baseUrl.password !== "" ||
      baseUrl.search !== "" ||
      baseUrl.hash !== ""
    ) {
      throw new TraceDecayProtocolError(
        "baseUrl must be an absolute HTTP URL without credentials, query, or fragment",
      );
    }
    const baseRoot = baseUrl.toString().replace(/\/+$/u, "");
    this.projectRoot = `${baseRoot}/projects/${encodeURIComponent(options.projectId)}`;
    this.applicationRoot = `${this.projectRoot}/application`;
    this.token = options.token;
    this.origin = options.origin ?? baseUrl.origin;
    this.doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.mcp = options.mcp;

    const methods: Record<
      string,
      (
        request: unknown,
        requestOptions?: OperationRequestOptions,
      ) => Promise<unknown>
    > = {};
    for (
      const descriptor of OPERATIONS as readonly AvailableOperationDescriptor<
        string,
        unknown,
        unknown
      >[]
    ) {
      methods[descriptor.operation] = (request, requestOptions) =>
        this.#requestOperation<unknown, unknown>(
          descriptor,
          request,
          requestOptions,
        );
    }
    this.operations = Object.freeze(methods) as OperationMethods;
  }

  private headers(
    accept: "application/json" | "text/event-stream",
  ): Headers {
    return new Headers({
      accept,
      authorization: `Bearer ${this.token}`,
      origin: this.origin,
    });
  }

  private operationUrl(route: string, page?: PageOptions): URL {
    validatePageOptions(page);
    const url = new URL(`${this.projectRoot}${route}`);
    if (page?.size !== undefined) {
      url.searchParams.set("page_size", String(page.size));
    }
    if (page?.cursor !== undefined) {
      url.searchParams.set("cursor", page.cursor);
    }
    return url;
  }

  private lifecycleUrl(operationId: string, suffix: "events" | "cancel"): URL {
    if (!isBoundedOpaqueString(operationId, MAX_REQUEST_ID_BYTES)) {
      throw new TraceDecayProtocolError(
        "operationId is not a canonical request handle",
      );
    }
    return new URL(
      `${this.applicationRoot}/operations/${encodeURIComponent(operationId)}/${suffix}`,
    );
  }

  private async fetchResponse(
    url: URL,
    init: RequestInit,
    signal?: AbortSignal,
  ): Promise<Response> {
    try {
      return await this.doFetch(url, init);
    } catch (cause) {
      if (signal?.aborted) {
        throw new TraceDecayAbortError(signal.reason);
      }
      throw new TraceDecayDisconnectedError(
        `transport failure requesting ${url.pathname}`,
        cause,
      );
    }
  }

  private async readJson(
    response: Response,
    expectedBinding?: string,
    expectedRequestId?: string,
    signal?: AbortSignal,
  ): Promise<unknown> {
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    let fullyRead = false;
    let cancellation: Promise<void> | undefined;
    const cancel = () => {
      cancellation ??= reader?.cancel(signal?.reason) ??
        response.body?.cancel(signal?.reason) ?? Promise.resolve();
      return cancellation;
    };
    const onAbort = () => {
      // The finally block joins this same cleanup while preserving the abort.
      void cancel().catch(() => undefined);
    };
    try {
      if (signal?.aborted) throw new TraceDecayAbortError(signal.reason);
      if (response.status === 401 || response.status === 403) {
        throw new TraceDecayAuthenticationError(response.status);
      }
      if (!hasMediaType(response, "application/json")) {
        throw new TraceDecayMalformedResponseError(
          "the daemon returned JSON with an invalid media type",
          { status: response.status },
        );
      }
      let text = "";
      try {
        reader = response.body?.getReader();
        signal?.addEventListener("abort", onAbort, { once: true });
        if (signal?.aborted) throw new TraceDecayAbortError(signal.reason);
        if (reader !== undefined) {
          const decoder = new TextDecoder();
          for (;;) {
            const { done, value } = await reader.read();
            if (signal?.aborted) throw new TraceDecayAbortError(signal.reason);
            if (done) break;
            text += decoder.decode(value, { stream: true });
          }
          text += decoder.decode();
        }
        fullyRead = true;
      } catch (cause) {
        if (signal?.aborted) throw new TraceDecayAbortError(signal.reason);
        throw new TraceDecayDisconnectedError(
          "transport failure reading the daemon response body",
          cause,
        );
      }
      // Complete malformed documents are protocol failures, not transport loss.
      const envelope = parseJson(text, response.status);
      if (isRecord(envelope) && envelope.kind === "problem") {
        const problemEnvelope = envelope.value;
        if (!isHttpProblemEnvelope(problemEnvelope)) {
          throw new TraceDecayMalformedResponseError(
            "the daemon returned a malformed problem envelope",
            { status: response.status, payload: problemEnvelope },
          );
        }
        const validBinding =
          expectedBinding === undefined ||
          applicationProblemBindingMatches(problemEnvelope, [expectedBinding]);
        if (
          response.ok ||
          !validBinding ||
          (expectedRequestId !== undefined && problemEnvelope.request_id !== expectedRequestId) ||
          !applicationProblemStatusMatches(response.status, problemEnvelope.problem.kind)
        ) {
          throw new TraceDecayMalformedResponseError(
            "the daemon returned a malformed problem envelope",
            { status: response.status, payload: problemEnvelope },
          );
        }
        throw problemError(response.status, problemEnvelope);
      }
      return envelope;
    } finally {
      signal?.removeEventListener("abort", onAbort);
      try {
        if (!fullyRead) await cancel();
      } catch {
        // Every incomplete-body exit already carries an abort, transport, or
        // response refusal. Cleanup must not replace that primary failure.
      } finally {
        reader?.releaseLock();
      }
    }
  }

  async #requestOperation<Request, Result>(
    descriptor: AvailableOperationDescriptor<string, Request, Result>,
    request: unknown,
    options: OperationRequestOptions = {},
  ): Promise<HttpSuccessEnvelope<Result> | Result> {
    const decodedRequest = descriptor.decodeRequest(request);
    if (!isHttpOperationDescriptor(descriptor)) {
      if (
        options.page !== undefined || options.deadlineMicros !== undefined ||
        options.requestId !== undefined
      ) {
        throw new TraceDecayProtocolError(
          `${descriptor.operation} does not accept HTTP transport options`,
        );
      }
      if (this.mcp === undefined) {
        throw new TraceDecayTransportError(
          `${descriptor.operation} requires an MCP tool adapter`,
        );
      }
      let response: unknown;
      try {
        response = await this.mcp.callTool(
          descriptor.transport.toolName,
          decodedRequest,
          { signal: options.signal },
        );
      } catch (cause) {
        if (options.signal?.aborted) {
          throw new TraceDecayAbortError(options.signal.reason ?? cause);
        }
        throw new TraceDecayTransportError(
          `MCP tool ${descriptor.transport.toolName} failed`,
          cause,
        );
      }
      try {
        return descriptor.decodeResult(response);
      } catch (cause) {
        throw new TraceDecayMalformedResponseError(
          `MCP tool ${descriptor.transport.toolName} returned an invalid ${descriptor.operation} result`,
          { payload: { response, cause } },
        );
      }
    }

    const url = this.operationUrl(descriptor.transport.route, options.page);
    const body = JSON.stringify(decodedRequest);
    if (body === undefined) {
      throw new TraceDecayProtocolError(
        `${descriptor.operation} request decoder returned a non-JSON value`,
      );
    }
    const headers = this.headers("application/json");
    headers.set("content-type", "application/json");
    const requestControlError = applyHttpRequestControls(headers, descriptor, options);
    if (requestControlError !== undefined) {
      throw new TraceDecayProtocolError(requestControlError);
    }
    const response = await this.fetchResponse(
      url,
      {
        method: descriptor.transport.method,
        headers,
        body,
        signal: options.signal,
      },
      options.signal,
    );
    const envelope = await this.readJson(
      response,
      descriptor.bindingId,
      options.requestId,
      options.signal,
    );
    if (!isRecord(envelope) || envelope.kind !== "success") {
      throw new TraceDecayMalformedResponseError(
        "the daemon returned an unknown HTTP envelope",
        { status: response.status, payload: envelope },
      );
    }
    if (!response.ok) {
      throw new TraceDecayMalformedResponseError(
        "the daemon returned success with a failing HTTP status",
        { status: response.status, payload: envelope },
      );
    }
    try {
      const decoded = descriptor.decodeSuccess(envelope.value);
      if (!isDecodedSuccessEnvelope(decoded)) {
        throw new TypeError("decoded application success invariants are invalid");
      }
      if (
        options.requestId !== undefined &&
        decoded.request_id !== options.requestId
      ) {
        throw new TypeError("daemon returned a different application request ID");
      }
      if (
        descriptor.resultSemantics === "fact_store_curate_receipt" &&
        !await factStoreCurateReceiptMatches(decodedRequest, decoded)
      ) {
        throw new TypeError("automatic curation receipt invariants are invalid");
      }
      return decoded;
    } catch (cause) {
      throw new TraceDecayMalformedResponseError(
        `the daemon returned an invalid ${descriptor.operation} success envelope`,
        { status: response.status, payload: { envelope: envelope.value, cause } },
      );
    }
  }

  async *streamOperation(
    operationId: string,
    options: OperationStreamOptions = {},
  ): AsyncGenerator<OperationStreamEvent> {
    let resumeToken = options.resume?.resumeToken;
    let nextSequence =
      options.resume?.nextSequence === undefined
        ? undefined
        : normalizeUnsignedDecimal(
            options.resume.nextSequence,
            "resume nextSequence",
          );
    if (
      resumeToken !== undefined &&
      !isBoundedOpaqueString(resumeToken, MAX_RESUME_TOKEN_BYTES)
    ) {
      throw new TraceDecayProtocolError(
        "resumeToken is not a canonical stream resume token",
      );
    }
    const maxReconnects = options.maxReconnects ?? 0;
    if (!Number.isSafeInteger(maxReconnects) || maxReconnects < 0) {
      throw new TraceDecayProtocolError("maxReconnects must be a non-negative integer");
    }
    let reconnects = 0;
    let reconnectDelayMs = 0;

    for (;;) {
      if (options.signal?.aborted) {
        throw new TraceDecayAbortError(options.signal.reason);
      }
      const url = this.lifecycleUrl(operationId, "events");
      if (nextSequence !== undefined) {
        url.searchParams.set("next_sequence", nextSequence);
      }
      if (resumeToken !== undefined) {
        url.searchParams.set("resume_token", resumeToken);
      }
      let response: Response;
      try {
        response = await this.fetchResponse(
          url,
          {
            method: "GET",
            headers: this.headers("text/event-stream"),
            signal: options.signal,
          },
          options.signal,
        );
      } catch (cause) {
        // The first connection is not retried. A failed reconnect spends budget
        // like any other disconnect and keeps the last accepted frontier.
        if (
          reconnects === 0 ||
          reconnects >= maxReconnects ||
          !(cause instanceof TraceDecayDisconnectedError)
        ) {
          throw cause;
        }
        reconnects += 1;
        await waitForReconnect(reconnectDelayMs, options.signal);
        continue;
      }
      let streamFailed = false;
      try {
        if (!response.ok || response.body === null) {
          await this.readJson(response, undefined, undefined, options.signal);
          throw new TraceDecayMalformedResponseError(
            "the daemon did not open an SSE stream",
            { status: response.status },
          );
        }
        if (!hasMediaType(response, "text/event-stream")) {
          throw new TraceDecayMalformedResponseError(
            "the daemon opened an operation stream with an invalid media type",
            { status: response.status },
          );
        }

        try {
          for await (const frame of parseSse(response.body)) {
            if (frame.retryDelayMs !== undefined) {
              reconnectDelayMs = frame.retryDelayMs;
            }
            const decoded = decodeSse(frame, response.status, operationId);
            if (decoded.sequence !== undefined) {
              const expected = nextSequence ?? "0";
              const next = incrementDecimal(decoded.lastSequence ?? decoded.sequence);
              const terminal = TERMINAL_EVENTS.has(frame.event);
              if (
                !terminal &&
                BigInt(decoded.sequence) < BigInt(expected)
              ) {
                continue;
              }
              // The authority repeats its terminal receipt when a subscriber
              // resumes exactly one past it. Deliver that completion once.
              if (decoded.sequence !== expected && !(terminal && next === expected)) {
                throw new TraceDecayMalformedResponseError(
                  "the daemon returned a non-contiguous SSE sequence",
                  { status: response.status, payload: decoded.event },
                );
              }
              nextSequence = next;
            } else {
              // An open frontier describes published history, not delivered
              // history. Only a sequenced event or gap advances this cursor.
              nextSequence ??= "0";
            }
            if (decoded.frontier !== undefined) {
              resumeToken = decoded.frontier.resumeToken;
            }
            yield {
              event: frame.event,
              id: decoded.sequence ?? null,
              data: decoded.event,
            };
            if (TERMINAL_EVENTS.has(frame.event)) {
              return;
            }
          }
        } catch (cause) {
          streamFailed = true;
          if (cause instanceof TraceDecayProtocolError) {
            throw cause;
          }
          if (options.signal?.aborted) {
            throw new TraceDecayAbortError(options.signal.reason ?? cause);
          }
          if (reconnects >= maxReconnects) {
            if (cause instanceof TraceDecayDisconnectedError) {
              throw cause;
            }
            throw new TraceDecayDisconnectedError(
              `operation stream disconnected after ${reconnects} reconnects`,
              cause,
            );
          }
        }
      } catch (cause) {
        streamFailed = true;
        throw cause;
      } finally {
        // Closing a watcher must close its HTTP body without cancelling the operation.
        // parseSse has already released its reader when this outer cleanup runs.
        try {
          await response.body?.cancel();
        } catch (cause) {
          // Do not mask a primary failure (including a retryable disconnect).
          // Otherwise disposal must report that its transport did not close.
          if (!streamFailed) {
            throw new TraceDecayDisconnectedError(
              "failed to close operation stream transport",
              cause,
            );
          }
        }
      }

      if (reconnects >= maxReconnects) {
        throw new TraceDecayDisconnectedError(
          `operation stream ended before a terminal event after ${reconnects} reconnects`,
        );
      }
      if (
        nextSequence === undefined ||
        resumeToken === undefined
      ) {
        throw new TraceDecayDisconnectedError(
          "operation stream ended before exposing a resumable frontier",
        );
      }
      reconnects += 1;
      await waitForReconnect(reconnectDelayMs, options.signal);
    }
  }

  async cancelOperation(
    operationId: string,
    options: Pick<OperationRequestOptions, "signal"> = {},
  ): Promise<OperationCancellation> {
    const response = await this.fetchResponse(
      this.lifecycleUrl(operationId, "cancel"),
      {
        method: "POST",
        headers: this.headers("application/json"),
        signal: options.signal,
      },
      options.signal,
    );
    const value = await this.readJson(response, undefined, undefined, options.signal);
    if (!isRecord(value)) {
      throw new TraceDecayMalformedResponseError(
        "the daemon returned a malformed cancellation response",
        { status: response.status, payload: value },
      );
    }
    const status = value.status;
    const canonicalStatus =
      status === "requested" ||
      status === "already_requested" ||
      status === "already_terminal";
    const canonicalHttpStatus =
      (status === "requested" && response.status === 202) ||
      ((status === "already_requested" || status === "already_terminal") &&
        response.status === 200);
    if (!response.ok || !canonicalStatus || !canonicalHttpStatus) {
      throw new TraceDecayMalformedResponseError(
        "the daemon returned a malformed cancellation response",
        { status: response.status, payload: value },
      );
    }
    return { ...value, status };
  }
}

export function createClient(options: ClientOptions): TraceDecayClient {
  return new TraceDecayClient(options);
}
