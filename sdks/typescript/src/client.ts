// Handwritten lifecycle façade for the TraceDecay public SDK (PR18).
//
// Generated files in this package (`operations.ts`, `types.ts`) carry the
// wire contracts; this module owns lifecycle semantics: authentication,
// scope attestation, paging/cursors, SSE resume, cancellation, idempotency,
// and typed errors. It never invents state: every partial, unavailable, or
// unknown outcome surfaces explicitly, mapped from the canonical envelope.

import type {
  ApplicationProblemRecord,
  HttpEnvelope,
  OperationReceipt,
  RetryDirective,
  SafeDiagnostic,
} from "./types";
import { WorkflowNamespace } from "./workflow";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

export class TraceDecayProblemError extends Error {
  readonly problem: ApplicationProblemRecord;

  constructor(problem: ApplicationProblemRecord) {
    super(`${problem.kind}/${problem.code}: ${problem.message}`);
    this.name = new.target.name;
    this.problem = problem;
  }
}

export class StaleCursorError extends TraceDecayProblemError {}
export class ScopeError extends TraceDecayProblemError {}
export class CapabilityError extends TraceDecayProblemError {}
export class UnavailableError extends TraceDecayProblemError {}
export class SaturatedError extends TraceDecayProblemError {}
export class ConflictError extends TraceDecayProblemError {}
export class UnsupportedVersionError extends TraceDecayProblemError {}
export class CancelledError extends TraceDecayProblemError {}
export class OperationTimeoutError extends TraceDecayProblemError {}

/** Transport-level failure with no problem envelope (remote unavailable). */
export class DisconnectedError extends Error {
  readonly operation: string;
  readonly cause?: unknown;

  constructor(operation: string, cause?: unknown) {
    super(`transport failure invoking ${operation}`);
    this.name = "DisconnectedError";
    this.operation = operation;
    this.cause = cause;
  }
}

/** The server reported an outcome shaped differently than its contract. */
export class MalformedEventError extends Error {
  readonly raw: string;

  constructor(raw: string, detail: string) {
    super(`malformed event: ${detail}`);
    this.name = "MalformedEventError";
    this.raw = raw;
  }
}

/** The operation completed partially; the partial evidence is preserved. */
export class PartialResultError<T> extends Error {
  readonly partial: T;
  readonly reason: SafeDiagnostic;

  constructor(partial: T, reason: SafeDiagnostic) {
    super(`partial result: ${reason.code}: ${reason.message}`);
    this.name = "PartialResultError";
    this.partial = partial;
    this.reason = reason;
  }
}

/** Client abort was delivered; the operation's cancellation observation rides along when known. */
export class RequestAbortedError extends Error {
  constructor() {
    super("request aborted by caller");
    this.name = "RequestAbortedError";
  }
}

type ProblemKind = ApplicationProblemRecord["kind"];

const PROBLEM_ERROR_MAP: Partial<Record<string, new (problem: ApplicationProblemRecord) => TraceDecayProblemError>> = {
  stale: StaleCursorError,
  not_found_or_not_authorized: ScopeError,
  conflict: ConflictError,
  unavailable: UnavailableError,
  saturated: SaturatedError,
  unsupported: UnsupportedVersionError,
  cancelled: CancelledError,
  timed_out: OperationTimeoutError,
};

function mapProblem(problem: ApplicationProblemRecord): TraceDecayProblemError {
  // Missing-capability conflicts read as capability errors; everything else
  // uses the kind mapping, and unknown future kinds stay a typed base error.
  if (problem.kind === "conflict" && problem.code.includes("capability")) {
    return new CapabilityError(problem);
  }
  const ctor = PROBLEM_ERROR_MAP[problem.kind as ProblemKind];
  return ctor ? new ctor(problem) : new TraceDecayProblemError(problem);
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

export interface ClientScope {
  projectId: string;
  repositoryId: string;
  worktreeId: string;
  reference?: string;
}

export interface ClientOptions {
  baseUrl: string;
  token?: string;
  bindingId?: string;
  scope?: ClientScope;
  /** Injectable for tests; defaults to global fetch. */
  fetch?: typeof fetch;
  /** Extra headers (e.g. scope attestation negotiated with the daemon). */
  headers?: Record<string, string>;
}

export interface InvokeOptions {
  idempotencyKey?: string;
  signal?: AbortSignal;
  /** Maximum directive-honoring retries for this call (default 2). */
  maxDirectiveRetries?: number;
}

export interface InvokeResult<T> {
  value: T;
  receipt?: OperationReceipt;
  requestId: string;
}

export interface Page<T> {
  items: T[];
  nextCursor: string | null;
}

export type StreamItem =
  | { type: "event"; event: unknown }
  | { type: "gap"; frontier: unknown; detail: SafeDiagnostic | null }
  | { type: "terminated"; reason: string | null };

interface OperationRef {
  operation: string;
  route: string;
}

const IDEMPOTENCY_HEADER = "x-tracedecay-idempotency-key";
const BINDING_HEADER = "x-tracedecay-binding-id";
const SCOPE_HEADER_PREFIX = "x-tracedecay-scope-";

export class TraceDecayClient {
  private readonly baseUrl: string;
  private readonly token?: string;
  private readonly bindingId?: string;
  private readonly scope?: ClientScope;
  private readonly extraHeaders: Record<string, string>;
  private readonly doFetch: typeof fetch;

  /** Workflow definition builder + run lifecycle (PR17/PR18 surface). */
  readonly workflow: WorkflowNamespace;

  constructor(options: ClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.token = options.token;
    this.bindingId = options.bindingId;
    this.scope = options.scope;
    this.extraHeaders = options.headers ?? {};
    const injected = options.fetch;
    this.doFetch = injected ?? ((...args) => fetch(...args));
    this.workflow = new WorkflowNamespace(this);
  }

  private headers(idempotencyKey?: string): Record<string, string> {
    const headers: Record<string, string> = {
      "content-type": "application/json",
      accept: "application/json",
      ...this.extraHeaders,
    };
    if (this.token) headers.authorization = `Bearer ${this.token}`;
    if (this.bindingId) headers[BINDING_HEADER] = this.bindingId;
    if (idempotencyKey) headers[IDEMPOTENCY_HEADER] = idempotencyKey;
    if (this.scope) {
      headers[`${SCOPE_HEADER_PREFIX}project`] = this.scope.projectId;
      headers[`${SCOPE_HEADER_PREFIX}repository`] = this.scope.repositoryId;
      headers[`${SCOPE_HEADER_PREFIX}worktree`] = this.scope.worktreeId;
      if (this.scope.reference) headers[`${SCOPE_HEADER_PREFIX}reference`] = this.scope.reference;
    }
    return headers;
  }

  private async rawPost(
    ref: OperationRef,
    body: unknown,
    options: InvokeOptions,
  ): Promise<HttpEnvelope<unknown>> {
    let response: Response;
    try {
      response = await this.doFetch(`${this.baseUrl}${ref.route}`, {
        method: "POST",
        headers: this.headers(options.idempotencyKey),
        body: JSON.stringify(body ?? {}),
        signal: options.signal ?? null,
      });
    } catch (error) {
      if (options.signal?.aborted) throw new RequestAbortedError();
      throw new DisconnectedError(ref.operation, error);
    }
    let envelope: HttpEnvelope<unknown>;
    try {
      envelope = (await response.json()) as HttpEnvelope<unknown>;
    } catch {
      throw new MalformedEventError(await response.text().catch(() => ""), `non-JSON response (HTTP ${response.status})`);
    }
    return envelope;
  }

  private static problemFrom(envelope: HttpEnvelope<unknown>): ApplicationProblemRecord | null {
    const value = (envelope as { value?: { problem?: ApplicationProblemRecord } }).value;
    return value?.problem ?? null;
  }

  private async sleep(ms: number, signal?: AbortSignal): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(resolve, ms);
      signal?.addEventListener("abort", () => {
        clearTimeout(timer);
        reject(new RequestAbortedError());
      }, { once: true });
    });
  }

  /**
   * Invoke one operation. Retries happen only as the envelope's
   * RetryDirective demands; `after_revalidate`/`after_reconcile` always
   * surface to the caller rather than being silently honored.
   */
  async invoke<T>(ref: OperationRef, params?: unknown, options: InvokeOptions = {}): Promise<InvokeResult<T>> {
    const maxRetries = options.maxDirectiveRetries ?? 2;
    let attempt = 0;
    for (;;) {
      if (options.signal?.aborted) throw new RequestAbortedError();
      const envelope = await this.rawPost(ref, params, options);
      if (envelope.kind === "success") {
        const value = (envelope as { value: Record<string, unknown> }).value;
        return {
          value: value.value as T,
          receipt: value.receipt as OperationReceipt | undefined,
          requestId: String(value.request_id ?? ""),
        };
      }
      const problem = TraceDecayClient.problemFrom(envelope);
      if (!problem) {
        // Unknown envelope kind: surfaced, never coerced to success.
        throw new MalformedEventError(JSON.stringify(envelope), `unknown envelope kind ${String(envelope.kind)}`);
      }
      const directive: RetryDirective = problem.retry;
      const retryable = problem.retryable && attempt < maxRetries;
      if (retryable && directive === "same_request") {
        attempt += 1;
        continue;
      }
      if (retryable && directive === "after_delay" && typeof problem.retry_after_millis === "number") {
        attempt += 1;
        await this.sleep(problem.retry_after_millis, options.signal);
        continue;
      }
      throw mapProblem(problem);
    }
  }

  /** Async iteration over a cursor-paged operation. Stale cursors fail typed. */
  async *page<T>(ref: OperationRef, params?: unknown, options: InvokeOptions = {}): AsyncGenerator<T[], Page<T>["nextCursor"]> {
    let cursor: string | null = null;
    for (;;) {
      const result: InvokeResult<Page<T>> = await this.invoke<Page<T>>(ref, { ...(params as object ?? {}), cursor }, options);
      const items = Array.isArray(result.value.items) ? result.value.items : [];
      yield items;
      cursor = result.value.nextCursor ?? null;
      if (cursor === null) return null;
    }
  }

  /**
   * Async iteration over an SSE operation. On transport disconnect the
   * stream reconnects with the last observed frontier; gaps surface as
   * explicit items and are never silently filled.
   */
  async *stream(ref: OperationRef, params?: unknown, options: InvokeOptions & { resumeFrom?: string } = {}): AsyncGenerator<StreamItem> {
    let frontier = options.resumeFrom ?? null;
    for (;;) {
      let response: Response;
      try {
        response = await this.doFetch(`${this.baseUrl}${ref.route}`, {
          method: "POST",
          headers: { ...this.headers(options.idempotencyKey), accept: "text/event-stream" },
          body: JSON.stringify({ ...(params as object ?? {}), resume_from: frontier }),
          signal: options.signal ?? null,
        });
      } catch (error) {
        if (options.signal?.aborted) throw new RequestAbortedError();
        throw new DisconnectedError(ref.operation, error);
      }
      if (!response.ok || !response.body) {
        const text = await response.text().catch(() => "");
        try {
          const envelope = JSON.parse(text) as HttpEnvelope<unknown>;
          const problem = TraceDecayClient.problemFrom(envelope);
          if (problem) throw mapProblem(problem);
        } catch (error) {
          if (error instanceof TraceDecayProblemError) throw error;
        }
        throw new MalformedEventError(text, `SSE open failed (HTTP ${response.status})`);
      }

      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      let disconnected = false;
      for (;;) {
        const { done, value } = await reader.read().catch((error: unknown) => {
          disconnected = true;
          return { done: true, value: undefined as Uint8Array | undefined };
        });
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let newline: number;
        while ((newline = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, newline).trim();
          buffer = buffer.slice(newline + 1);
          if (!line || line.startsWith(":")) continue;
          if (!line.startsWith("data:")) {
            throw new MalformedEventError(line, "non-data SSE field");
          }
          const payload = line.slice(5).trim();
          let event: { kind?: string; frontier?: unknown; diagnostic?: SafeDiagnostic | null; reason?: string | null };
          try {
            event = JSON.parse(payload) as typeof event;
          } catch {
            throw new MalformedEventError(payload, "SSE data is not JSON");
          }
          if (event.frontier !== undefined) frontier = JSON.stringify(event.frontier);
          if (event.kind === "gap") {
            yield { type: "gap", frontier: event.frontier ?? null, detail: event.diagnostic ?? null };
          } else if (event.kind === "terminated") {
            yield { type: "terminated", reason: event.reason ?? null };
            return;
          } else {
            yield { type: "event", event };
          }
        }
      }
      if (!disconnected) return;
      // Transport dropped without a termination frame: reconnect from the
      // last frontier instead of silently ending the stream.
    }
  }

  /** Best-effort cancellation of an in-flight request. */
  async cancel(requestId: string, route = "/v1/operations/cancel"): Promise<void> {
    await this.rawPost({ operation: "cancel", route }, { request_id: requestId }, {});
  }

  /**
   * Resume via a host handoff token. The token authority (PR14 Work /
   * PR16 remote) is not frozen yet; this path pins the envelope behavior
   * only and may be re-pointed when the authority lands.
   */
  async resume<T>(handoffToken: string, route = "/v1/operations/resume"): Promise<InvokeResult<T>> {
    return this.invoke<T>({ operation: "resume", route }, { handoff_token: handoffToken }, {});
  }
}

export function createClient(options: ClientOptions): TraceDecayClient {
  return new TraceDecayClient(options);
}
