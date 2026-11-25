import {
  OPERATIONS,
  type AvailableOperationDescriptor,
  type OperationName,
  type RequestFor,
  type ResponseFor,
} from "./operations";
import {
  loadNativeBinding,
  nativeFailure,
  platformPackageName,
  type NativeCall,
  type NativeClient,
  type NativeFailure,
  type NativeStream,
} from "./native";
import type { ApplicationProblemRecord, HttpProblemEnvelope, HttpSseEvent } from "./types";

export interface ClientOptions {
  baseUrl: string;
  projectId: string;
  token: string;
  origin?: string;
  mcp?: McpToolAdapter;
  /**
   * Absolute path of the platform addon, for hosts that ship it outside
   * `node_modules` (an offline plugin install places it beside the daemon
   * binary). Omit it to load the installed `@tracedecay/sdk-<platform>`
   * package.
   */
  addon?: string;
}

/** Adapter owned by the caller's MCP host for generated MCP tool bindings. */
export interface McpToolAdapter {
  callTool(
    toolName: string,
    request: unknown,
    options: { signal?: AbortSignal },
  ): Promise<unknown>;
}

export interface PageOptions {
  size?: number;
  cursor?: string;
}

export interface OperationRequestOptions {
  page?: PageOptions;
  signal?: AbortSignal;
  /** Absolute UTC deadline in microseconds, forwarded to daemon admission. */
  deadlineMicros?: number;
  /** Stable replay identity required by operations whose descriptor says so. */
  requestId?: string;
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
  /** Aborting closes the stream's connection; it does not cancel the operation. */
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

function problemError(status: number, envelope: HttpProblemEnvelope): TraceDecayProblemError {
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

/** Map one binding failure onto the public error taxonomy. */
function typedError(failure: NativeFailure, signal: AbortSignal | undefined): Error {
  const status = failure.status ?? undefined;
  switch (failure.kind) {
    case "interrupted":
      return new TraceDecayAbortError(signal?.reason);
    case "transport":
      return new TraceDecayDisconnectedError(failure.message);
    case "authentication":
      return new TraceDecayAuthenticationError(status === 403 ? 403 : 401);
    case "problem":
      return problemError(status ?? 0, failure.envelope as HttpProblemEnvelope);
    case "protocol":
      return new TraceDecayMalformedResponseError(failure.message, { status });
    case "invalid_configuration":
    case "invalid_request":
    case "unsupported_transport":
      return new TraceDecayProtocolError(failure.message);
  }
}

function rethrow(error: unknown, signal: AbortSignal | undefined): never {
  const failure = nativeFailure(error);
  throw failure === undefined ? error : typedError(failure, signal);
}

/** Run one native exchange, interrupting its I/O when `signal` aborts. */
async function runCall(call: NativeCall, signal: AbortSignal | undefined): Promise<unknown> {
  if (signal?.aborted) throw new TraceDecayAbortError(signal.reason);
  const interrupt = () => call.interrupt();
  signal?.addEventListener("abort", interrupt, { once: true });
  try {
    return JSON.parse(await call.run()) as unknown;
  } catch (error) {
    return rethrow(error, signal);
  } finally {
    signal?.removeEventListener("abort", interrupt);
  }
}

function unsignedDecimal(value: number | bigint | string): string {
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || value < 0) {
      throw new TraceDecayProtocolError("resume nextSequence must be an unsigned safe integer");
    }
    return String(value);
  }
  if (typeof value === "bigint") {
    if (value < 0n) throw new TraceDecayProtocolError("resume nextSequence must be unsigned");
    return value.toString();
  }
  if (!/^[0-9]+$/u.test(value)) {
    throw new TraceDecayProtocolError("resume nextSequence must be unsigned decimal");
  }
  return value;
}

/** Preserve supplied controls across JSON's non-finite-number coercion. */
function optionalSafeInteger(value: number | undefined, name: string): void {
  if (value !== undefined && !Number.isSafeInteger(value)) {
    throw new TraceDecayProtocolError(`${name} must be a safe integer`);
  }
}

export class TraceDecayClient {
  readonly operations: OperationMethods;

  readonly #options: ClientOptions;
  #native: NativeClient | undefined;

  constructor(options: ClientOptions) {
    this.#options = options;
    const methods: Record<
      string,
      (request: unknown, requestOptions?: OperationRequestOptions) => Promise<unknown>
    > = {};
    for (
      const descriptor of OPERATIONS as readonly AvailableOperationDescriptor<string, unknown, unknown>[]
    ) {
      methods[descriptor.operation] = (request, requestOptions = {}) =>
        descriptor.transport.kind === "http"
          ? this.#requestHttp(descriptor.operationId, request, requestOptions)
          : this.#requestMcp(descriptor, request, requestOptions);
    }
    this.operations = Object.freeze(methods) as OperationMethods;
  }

  /** The binding loads on first HTTP use, so MCP-only hosts never need it. */
  #client(): NativeClient {
    if (this.#native === undefined) {
      let binding;
      try {
        binding = loadNativeBinding(this.#options.addon);
      } catch (cause) {
        throw new TraceDecayTransportError(
          `the @tracedecay/sdk native binding ${this.#options.addon ?? platformPackageName()} is not installed`,
          cause,
        );
      }
      const { NativeClient } = binding;
      const { baseUrl, projectId, token, origin } = this.#options;
      try {
        this.#native = new NativeClient(JSON.stringify({ baseUrl, projectId, token, origin }));
      } catch (error) {
        rethrow(error, undefined);
      }
    }
    return this.#native;
  }

  async #requestHttp(
    operationId: string,
    request: unknown,
    { signal, ...controls }: OperationRequestOptions,
  ): Promise<unknown> {
    let call: NativeCall;
    try {
      optionalSafeInteger(controls.deadlineMicros, "deadlineMicros");
      optionalSafeInteger(controls.page?.size, "page size");
      const requestJson = JSON.stringify(request, (_key, value: unknown) => {
        if (typeof value === "number" && !Number.isFinite(value)) {
          throw new TraceDecayProtocolError("request numbers must be finite");
        }
        return value;
      });
      call = this.#client().prepareOperation(
        operationId,
        requestJson,
        JSON.stringify(controls),
      );
    } catch (error) {
      return rethrow(error, signal);
    }
    return runCall(call, signal);
  }

  async #requestMcp(
    descriptor: AvailableOperationDescriptor<string, unknown, unknown>,
    request: unknown,
    options: OperationRequestOptions,
  ): Promise<unknown> {
    if (descriptor.transport.kind !== "mcp_tool") {
      throw new TraceDecayProtocolError(`${descriptor.operation} is not an MCP tool binding`);
    }
    if (
      options.page !== undefined || options.deadlineMicros !== undefined ||
      options.requestId !== undefined
    ) {
      throw new TraceDecayProtocolError(
        `${descriptor.operation} does not accept HTTP transport options`,
      );
    }
    const decodedRequest = descriptor.decodeRequest(request);
    const mcp = this.#options.mcp;
    if (mcp === undefined) {
      throw new TraceDecayTransportError(`${descriptor.operation} requires an MCP tool adapter`);
    }
    let response: unknown;
    try {
      response = await mcp.callTool(descriptor.transport.toolName, decodedRequest, {
        signal: options.signal,
      });
    } catch (cause) {
      if (options.signal?.aborted) {
        throw new TraceDecayAbortError(options.signal.reason ?? cause);
      }
      throw new TraceDecayTransportError(`MCP tool ${descriptor.transport.toolName} failed`, cause);
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

  /**
   * Watch an operation's canonical events. Leaving the loop early or aborting
   * releases the connection; neither cancels the operation.
   */
  streamOperation(
    operationId: string,
    options: OperationStreamOptions = {},
  ): AsyncGenerator<OperationStreamEvent> {
    let native: NativeStream | undefined;
    const iterator = this.#streamOperation(operationId, options, (stream) => { native = stream; });
    const returnFrom = iterator.return.bind(iterator);
    const throwInto = iterator.throw.bind(iterator);
    // Async generators queue return/throw behind an outstanding next(). Close
    // its native read first so disposal cannot wait forever for another event.
    iterator.return = (value) => {
      native?.close();
      return returnFrom(value);
    };
    iterator.throw = (error) => {
      native?.close();
      return throwInto(error);
    };
    return iterator;
  }

  async *#streamOperation(
    operationId: string,
    options: OperationStreamOptions,
    own: (stream: NativeStream | undefined) => void,
  ): AsyncGenerator<OperationStreamEvent> {
    const { signal, resume, maxReconnects } = options;
    if (signal?.aborted) throw new TraceDecayAbortError(signal.reason);
    let stream: NativeStream;
    try {
      optionalSafeInteger(maxReconnects, "maxReconnects");
      stream = this.#client().prepareStream(
        operationId,
        JSON.stringify({
          resumeToken: resume?.resumeToken,
          nextSequence: resume === undefined ? undefined : unsignedDecimal(resume.nextSequence),
          maxReconnects,
        }),
      );
    } catch (error) {
      rethrow(error, signal);
    }
    own(stream);
    const close = () => stream.close();
    signal?.addEventListener("abort", close, { once: true });
    try {
      for (;;) {
        let text: string | null;
        try {
          text = await stream.next();
        } catch (error) {
          rethrow(error, signal);
        }
        if (text === null) return;
        yield JSON.parse(text) as OperationStreamEvent;
      }
    } finally {
      signal?.removeEventListener("abort", close);
      stream.close();
      own(undefined);
    }
  }

  async cancelOperation(
    operationId: string,
    options: Pick<OperationRequestOptions, "signal"> = {},
  ): Promise<OperationCancellation> {
    let call: NativeCall;
    try {
      call = this.#client().prepareCancel(operationId);
    } catch (error) {
      return rethrow(error, options.signal);
    }
    return (await runCall(call, options.signal)) as OperationCancellation;
  }
}

export function createClient(options: ClientOptions): TraceDecayClient {
  return new TraceDecayClient(options);
}
