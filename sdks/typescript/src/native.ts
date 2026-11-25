import { createRequire } from "node:module";

/**
 * The Node-API binding built from `crates/tracedecay-sdk-node`. It owns every
 * HTTP lifecycle rule; this module only locates and loads it.
 */
export interface NativeBinding {
  NativeClient: new (configJson: string) => NativeClient;
}

export interface NativeClient {
  prepareOperation(operationId: string, requestJson: string, optionsJson: string): NativeCall;
  prepareCancel(operationId: string): NativeCall;
  prepareStream(operationId: string, optionsJson: string): NativeStream;
}

export interface NativeCall {
  run(): Promise<string>;
  interrupt(): void;
}

export interface NativeStream {
  next(): Promise<string | null>;
  close(): void;
}

/** Error payload the binding encodes in `Error.message`. */
export interface NativeFailure {
  kind:
    | "invalid_configuration"
    | "invalid_request"
    | "transport"
    | "unsupported_transport"
    | "authentication"
    | "protocol"
    | "problem"
    | "interrupted";
  message: string;
  status?: number | null;
  envelope?: unknown;
}

/**
 * The published platform package for this host, following the napi-rs
 * convention. Linux distinguishes glibc from musl because their addons are
 * not interchangeable.
 */
export function platformPackageName(): string {
  const { platform, arch } = process;
  if (platform === "linux") {
    const report = process.report?.getReport() as { header?: { glibcVersionRuntime?: string } } | undefined;
    const libc = report?.header?.glibcVersionRuntime === undefined ? "musl" : "gnu";
    return `@tracedecay/sdk-linux-${arch}-${libc}`;
  }
  if (platform === "win32") return `@tracedecay/sdk-win32-${arch}-msvc`;
  return `@tracedecay/sdk-${platform}-${arch}`;
}

const loaded = new Map<string, NativeBinding>();

/**
 * Load the binding from an explicit addon path (hosts that ship it beside the
 * daemon binary, such as offline plugin installs) or from the installed
 * platform package.
 */
export function loadNativeBinding(addon: string | undefined): NativeBinding {
  const require = createRequire(import.meta.url);
  const specifier = addon ?? platformPackageName();
  const cached = loaded.get(specifier);
  if (cached !== undefined) return cached;
  const binding = require(specifier) as NativeBinding;
  loaded.set(specifier, binding);
  return binding;
}

export function nativeFailure(error: unknown): NativeFailure | undefined {
  if (!(error instanceof Error)) return undefined;
  try {
    const value = JSON.parse(error.message) as unknown;
    if (typeof value === "object" && value !== null && "kind" in value && "message" in value) {
      return value as NativeFailure;
    }
  } catch {
    // Not a binding failure; the caller rethrows the original error.
  }
  return undefined;
}
