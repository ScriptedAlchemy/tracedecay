import {
  OPERATIONS,
  type OperationDescriptor,
  type OperationName,
  type RequestFor,
  type ResultFor,
} from "./operations";
import type {
  ApplicationProblemRecord,
  HttpProblemEnvelope,
  HttpSseEvent,
  HttpSuccessEnvelope,
  OperationReceipt,
} from "./types";

type FetchImplementation = typeof globalThis.fetch;

export interface ClientOptions {
  baseUrl: string;
  projectId: string;
  token: string;
  origin?: string;
  fetch?: FetchImplementation;
}

export interface PageOptions {
  size?: number;
  cursor?: string;
}

export interface OperationRequestOptions {
  page?: PageOptions;
  signal?: AbortSignal;
}

export type OperationMethod<Name extends OperationName> = (
  request: RequestFor<Name>,
  options?: OperationRequestOptions,
) => Promise<HttpSuccessEnvelope<ResultFor<Name>>>;

export type OperationMethods = {
  readonly [Name in OperationName]: OperationMethod<Name>;
};

export interface OperationStreamResume {
  resumeToken: string;
  nextSequence: number | bigint | string;
}

export interface OperationStreamOptions {
  resume?: OperationStreamResume;
  signal?: AbortSignal;
  /** Explicit opt-in; transport reconnection is disabled by default. */
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
  nextSequence: string;
  resumeToken?: string;
}

const TERMINAL_EVENTS = new Set([
  "completed",
  "cancelled",
  "timed_out",
  "failed",
  "partial",
  "effect_unknown",
]);
const MAX_PAGE_SIZE = 1_000;
const MAX_OPAQUE_CURSOR_BYTES = 4_096;
const MAX_RESUME_TOKEN_BYTES = 4_096;
const MAX_REQUEST_ID_BYTES = 512;
const MAX_U64 = 18_446_744_073_709_551_615n;
const UTF8_ENCODER = new TextEncoder();

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
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

function isContract(value: unknown): boolean {
  return (
    isRecord(value) &&
    isBoundedOpaqueString(value.schema_id, 512) &&
    isSafeUnsignedInteger(value.schema_revision) &&
    value.schema_revision > 0
  );
}

function isDiagnostic(value: unknown): boolean {
  return (
    isRecord(value) &&
    typeof value.code === "string" &&
    typeof value.message === "string"
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

function isPageState(value: unknown): boolean {
  if (
    !isRecord(value) ||
    !isBoundedOpaqueString(value.sort_contract_id, 512) ||
    !isSafeUnsignedInteger(value.sort_revision) ||
    value.sort_revision === 0 ||
    (value.total !== null && !isSafeUnsignedInteger(value.total)) ||
    !isSafeUnsignedInteger(value.returned) ||
    (value.cursor !== null &&
      !isBoundedOpaqueString(value.cursor, MAX_OPAQUE_CURSOR_BYTES)) ||
    (value.expires_at !== null && !isSafeInteger(value.expires_at))
  ) {
    return false;
  }
  return value.total === null || value.returned <= value.total;
}

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
      isPageState(value.outcome.value.page) &&
      (value.outcome.value.execution.termination !== "completed" ||
        value.outcome.value.payload !== null)
    );
  }
  return value.outcome.outcome === "preview" || value.outcome.outcome === "effect";
}

function isProblemEnvelope(value: unknown): value is HttpProblemEnvelope {
  if (
    !isRecord(value) ||
    (value.binding_id !== undefined && typeof value.binding_id !== "string") ||
    !isContract(value.contract) ||
    !isBoundedOpaqueString(value.request_id, MAX_REQUEST_ID_BYTES) ||
    !isRecord(value.problem)
  ) {
    return false;
  }
  const problem = value.problem;
  return (
    problem.revision === 1 &&
    typeof problem.kind === "string" &&
    typeof problem.code === "string" &&
    typeof problem.message === "string" &&
    (problem.diagnostic === null || isDiagnostic(problem.diagnostic)) &&
    typeof problem.owning_layer === "string" &&
    typeof problem.terminality === "string" &&
    typeof problem.retryable === "boolean" &&
    typeof problem.retry === "string" &&
    (problem.retry_scope === null || typeof problem.retry_scope === "string") &&
    (problem.retry_after_millis === null ||
      isSafeUnsignedInteger(problem.retry_after_millis)) &&
    (problem.cancellation_stage === null ||
      typeof problem.cancellation_stage === "string") &&
    problem.request_id === value.request_id &&
    isBoundedOpaqueString(problem.trace_id, MAX_REQUEST_ID_BYTES) &&
    Array.isArray(problem.details) &&
    problem.details.every(isDiagnostic) &&
    Array.isArray(problem.legal_actions) &&
    problem.legal_actions.every((action) => typeof action === "string") &&
    "coverage" in problem
  );
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

function incrementDecimal(value: string): string | undefined {
  return /^[0-9]+$/.test(value) ? (BigInt(value) + 1n).toString() : undefined;
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
  let buffer = "";
  let eventType = "";
  let lastEventId = initialLastEventId ?? "";
  let data: string[] = [];
  let pendingFields = false;
  let retryDelayMs: number | undefined;

  const consumeLine = (line: string): SseFrame | null => {
    if (line === "") {
      pendingFields = false;
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
    switch (field) {
      case "event":
        eventType = fieldValue;
        break;
      case "id":
        if (!fieldValue.includes("\0")) {
          lastEventId = fieldValue;
        }
        break;
      case "data":
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

  const lines = function* (atEnd: boolean): Generator<string> {
    for (;;) {
      let boundary = -1;
      for (let index = 0; index < buffer.length; index += 1) {
        if (buffer[index] === "\r" || buffer[index] === "\n") {
          boundary = index;
          break;
        }
      }
      if (boundary < 0) {
        return;
      }
      if (buffer[boundary] === "\r" && boundary === buffer.length - 1 && !atEnd) {
        return;
      }
      const line = buffer.slice(0, boundary);
      const width =
        buffer[boundary] === "\r" && buffer[boundary + 1] === "\n" ? 2 : 1;
      buffer = buffer.slice(boundary + width);
      yield line;
    }
  };

  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) {
        buffer += decoder.decode();
        for (const line of lines(true)) {
          const frame = consumeLine(line);
          if (frame !== null) {
            yield frame;
          }
        }
        break;
      }
      buffer += decoder.decode(chunk.value, { stream: true });
      for (const line of lines(false)) {
        const frame = consumeLine(line);
        if (frame !== null) {
          yield frame;
        }
      }
    }
    if (buffer.length > 0 || pendingFields || data.length > 0) {
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
    throw new TraceDecayMalformedResponseError(
      "the daemon returned malformed UTF-8 event-stream data",
      { payload: cause },
    );
  } finally {
    reader.releaseLock();
  }
}

function decodeSse(
  frame: SseFrame,
  status: number,
): { event: HttpSseEvent<unknown>; sequence?: string; frontier?: StreamFrontier } {
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
      typeof eventData.correlation_id !== "string" ||
      !isRecord(frontier) ||
      !isSafeUnsignedInteger(frontier.next_sequence) ||
      !isSafeUnsignedInteger(frontier.retained_from_sequence) ||
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
        nextSequence: String(frontier.next_sequence),
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
      gap.first_missing_sequence > gap.last_missing_sequence ||
      !isRecord(gap.frontier) ||
      !isSafeUnsignedInteger(gap.frontier.next_sequence) ||
      !isSafeUnsignedInteger(gap.frontier.retained_from_sequence) ||
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
  return { event, sequence };
}

export class TraceDecayClient {
  readonly operations: OperationMethods;

  private readonly projectRoot: string;
  private readonly applicationRoot: string;
  private readonly token: string;
  private readonly origin: string;
  private readonly doFetch: FetchImplementation;

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

    const methods: Record<
      string,
      (
        request: unknown,
        requestOptions?: OperationRequestOptions,
      ) => Promise<HttpSuccessEnvelope<unknown>>
    > = {};
    for (
      const descriptor of OPERATIONS as readonly OperationDescriptor<
        string,
        unknown,
        unknown
      >[]
    ) {
      methods[descriptor.operation] = (request, requestOptions) =>
        this.requestOperation<unknown, unknown>(
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
        throw new TraceDecayAbortError(signal.reason ?? cause);
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
  ): Promise<unknown> {
    if (response.status === 401 || response.status === 403) {
      throw new TraceDecayAuthenticationError(response.status);
    }
    if (!hasMediaType(response, "application/json")) {
      const payload = await response.text();
      throw new TraceDecayMalformedResponseError(
        "the daemon returned JSON with an invalid media type",
        { status: response.status, payload },
      );
    }
    const envelope = parseJson(await response.text(), response.status);
    if (isRecord(envelope) && envelope.kind === "problem") {
      const problemEnvelope = envelope.value;
      if (!isProblemEnvelope(problemEnvelope)) {
        throw new TraceDecayMalformedResponseError(
          "the daemon returned a malformed problem envelope",
          { status: response.status, payload: problemEnvelope },
        );
      }
      const validBinding =
        expectedBinding === undefined ||
        (problemEnvelope.problem.kind === "not_found_or_not_authorized"
          ? problemEnvelope.binding_id === undefined
          : problemEnvelope.binding_id === expectedBinding);
      if (response.ok || !validBinding) {
        throw new TraceDecayMalformedResponseError(
          "the daemon returned a malformed problem envelope",
          { status: response.status, payload: problemEnvelope },
        );
      }
      throw problemError(response.status, problemEnvelope);
    }
    return envelope;
  }

  private async requestOperation<Request, Result>(
    descriptor: OperationDescriptor<string, Request, Result>,
    request: unknown,
    options: OperationRequestOptions = {},
  ): Promise<HttpSuccessEnvelope<Result>> {
    const url = this.operationUrl(descriptor.route, options.page);
    const decodedRequest = descriptor.decodeRequest(request);
    const body = JSON.stringify(decodedRequest);
    if (body === undefined) {
      throw new TraceDecayProtocolError(
        `${descriptor.operation} request decoder returned a non-JSON value`,
      );
    }
    const headers = this.headers("application/json");
    headers.set("content-type", "application/json");
    const response = await this.fetchResponse(
      url,
      {
        method: descriptor.method,
        headers,
        body,
        signal: options.signal,
      },
      options.signal,
    );
    const envelope = await this.readJson(response, descriptor.bindingId);
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
    let lastSequence =
      nextSequence === undefined || nextSequence === "0"
        ? undefined
        : (BigInt(nextSequence) - 1n).toString();
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
      const response = await this.fetchResponse(
        url,
        {
          method: "GET",
          headers: this.headers("text/event-stream"),
          signal: options.signal,
        },
        options.signal,
      );
      if (!response.ok || response.body === null) {
        await this.readJson(response);
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
          const decoded = decodeSse(frame, response.status);
          if (decoded.frontier !== undefined) {
            nextSequence = decoded.frontier.nextSequence;
            resumeToken = decoded.frontier.resumeToken;
          }
          if (decoded.sequence !== undefined) {
            if (
              lastSequence !== undefined &&
              BigInt(decoded.sequence) <= BigInt(lastSequence)
            ) {
              continue;
            }
            const expected =
              lastSequence === undefined ? undefined : incrementDecimal(lastSequence);
            if (expected !== undefined && decoded.sequence !== expected) {
              throw new TraceDecayMalformedResponseError(
                "the daemon returned a non-contiguous SSE sequence",
                { status: response.status, payload: decoded.event },
              );
            }
            lastSequence = decoded.sequence;
            nextSequence = incrementDecimal(decoded.sequence);
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
    const value = await this.readJson(response);
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
