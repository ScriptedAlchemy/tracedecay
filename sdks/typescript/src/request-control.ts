import { APPLICATION_REQUEST_ID_HEADER } from "./operations";

const MAX_REQUEST_ID_BYTES = 512;
const UTF8_ENCODER = new TextEncoder();

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

interface RequestControlDescriptor {
  operation: string;
  requestIdControl: "server_minted" | "required";
}

function canonicalRequestId(value: string): boolean {
  return value.length > 0 && value.trim() === value &&
    UTF8_ENCODER.encode(value).byteLength <= MAX_REQUEST_ID_BYTES &&
    !/\p{Cc}/u.test(value);
}

export function applyHttpRequestControls(
  headers: Headers,
  descriptor: RequestControlDescriptor,
  options: OperationRequestOptions,
): string | undefined {
  if (
    options.deadlineMicros !== undefined &&
    (!Number.isSafeInteger(options.deadlineMicros) || options.deadlineMicros <= 0)
  ) return "deadlineMicros must be a positive safe integer";
  if (descriptor.requestIdControl === "required" && options.requestId === undefined) {
    return `${descriptor.operation} requires a stable requestId replay handle`;
  }
  if (descriptor.requestIdControl === "server_minted" && options.requestId !== undefined) {
    return `${descriptor.operation} uses a server-minted request ID`;
  }
  if (options.requestId !== undefined && !canonicalRequestId(options.requestId)) {
    return "requestId is not a canonical request identity";
  }
  if (options.deadlineMicros !== undefined) {
    headers.set("x-tracedecay-deadline-micros", String(options.deadlineMicros));
  }
  if (options.requestId !== undefined) {
    headers.set(APPLICATION_REQUEST_ID_HEADER, options.requestId);
  }
  return undefined;
}
