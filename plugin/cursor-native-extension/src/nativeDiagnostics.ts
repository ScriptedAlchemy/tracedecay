export const MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT = 100;
export const MAX_NATIVE_DIAGNOSTIC_MESSAGE_BYTES = 512;
export const MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT = 32;

const MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES = 256;
const SAFE_DIAGNOSTIC_DATA_FIELDS = ["category", "href", "kind", "ruleId", "url"] as const;
const PACKAGED_TRACEDECAY_BINARY = "__TRACEDECAY_BIN__";

export interface NativePosition {
  line: number;
  character: number;
}

export interface NativeRange {
  start: NativePosition;
  end: NativePosition;
}

export interface NativeDiagnosticInput {
  range: NativeRange;
  severity?: number;
  code?: string | number;
  source?: string;
  message: string;
  data?: unknown;
}

export interface NativeDiagnostic {
  range: NativeRange;
  severity?: number;
  code?: string | number;
  source: string;
  message: string;
  data: Record<string, boolean | number | string> | null;
}

export interface NativeDiagnosticsPayload {
  uri: string;
  version: number;
  diagnostics: NativeDiagnostic[];
}

export type NativeWorkspaceAdmission<Root> =
  | { state: "supported"; root: Root }
  | {
      state: "unavailable";
      reason: "workspace_untrusted" | "workspace_root_count";
      expectedRootCount: 1;
      actualRootCount: number;
    };

export function admitNativeWorkspace<Root>(
  trusted: boolean,
  roots: readonly Root[],
): NativeWorkspaceAdmission<Root> {
  if (!trusted) {
    return {
      state: "unavailable",
      reason: "workspace_untrusted",
      expectedRootCount: 1,
      actualRootCount: roots.length,
    };
  }
  if (roots.length !== 1) {
    return {
      state: "unavailable",
      reason: "workspace_root_count",
      expectedRootCount: 1,
      actualRootCount: roots.length,
    };
  }
  return { state: "supported", root: roots[0]! };
}

export function traceDecayInitializationOptions(): {
  tracedecay: { context: true; mode: "cursor-native" };
} {
  return {
    tracedecay: {
      context: true,
      mode: "cursor-native",
    },
  };
}

export function toLspDiagnosticSeverity(
  vscodeSeverity: number,
): 1 | 2 | 3 | 4 | undefined {
  switch (vscodeSeverity) {
    case 0:
      return 1;
    case 1:
      return 2;
    case 2:
      return 3;
    case 3:
      return 4;
    default:
      return undefined;
  }
}

export function resolveInstalledTraceDecayBinary(
  configuredBinary: string | undefined,
  environmentBinary: string | undefined,
  packagedBinary = PACKAGED_TRACEDECAY_BINARY,
): string {
  for (const candidate of [
    configuredBinary,
    environmentBinary,
    packagedBinary.startsWith("__TRACEDECAY_") ? undefined : packagedBinary,
  ]) {
    const binary = candidate?.trim();
    if (binary !== undefined && binary.length > 0) {
      return binary;
    }
  }
  return "tracedecay";
}

export function limitAdmittedNativeDiagnosticDocuments<T>(
  documents: readonly T[],
  isAdmitted: (document: T) => boolean,
): T[] {
  return documents
    .filter(isAdmitted)
    .slice(0, MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT);
}

/** Split admitted documents into per-event batches at the notification cap. */
export function batchAdmittedNativeDiagnosticDocuments<T>(
  documents: readonly T[],
  isAdmitted: (document: T) => boolean,
): T[][] {
  const admitted = documents.filter(isAdmitted);
  const batches: T[][] = [];
  for (
    let offset = 0;
    offset < admitted.length;
    offset += MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT
  ) {
    batches.push(
      admitted.slice(offset, offset + MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT),
    );
  }
  return batches;
}

export function createNativeDiagnosticsPayload(
  uri: string,
  version: number,
  diagnostics: readonly NativeDiagnosticInput[],
): NativeDiagnosticsPayload {
  return {
    uri,
    version: Number.isSafeInteger(version) ? version : 0,
    diagnostics: diagnostics
      .slice(0, MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT)
      .map((diagnostic) => {
        const code =
          diagnostic.code === undefined
            ? undefined
            : truncateUtf8(
                String(diagnostic.code),
                MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES,
              );
        const message =
          truncateUtf8(diagnostic.message, MAX_NATIVE_DIAGNOSTIC_MESSAGE_BYTES) ||
          "Cursor diagnostic";
        return {
          range: diagnostic.range,
          ...(diagnostic.severity === undefined ? {} : { severity: diagnostic.severity }),
          ...(code === undefined || code.length === 0 ? {} : { code }),
          source: truncateUtf8(
            diagnostic.source || "cursor",
            MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES,
          ),
          message,
          data: sanitizeDiagnosticData(diagnostic.data),
        };
      }),
  };
}

function sanitizeDiagnosticData(data: unknown): Record<string, boolean | number | string> | null {
  if (!isRecord(data)) {
    return null;
  }

  const safe: Record<string, boolean | number | string> = {};
  for (const field of SAFE_DIAGNOSTIC_DATA_FIELDS) {
    const value = data[field];
    if (typeof value === "boolean") {
      safe[field] = value;
    } else if (typeof value === "number" && Number.isFinite(value)) {
      safe[field] = value;
    } else if (typeof value === "string") {
      safe[field] = truncateUtf8(value, MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES);
    }
  }

  return Object.keys(safe).length === 0 ? null : safe;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function truncateUtf8(value: string, maximumBytes: number): string {
  if (Buffer.byteLength(value, "utf8") <= maximumBytes) {
    return value;
  }

  let result = "";
  let byteLength = 0;
  for (const character of value) {
    const characterBytes = Buffer.byteLength(character, "utf8");
    if (byteLength + characterBytes > maximumBytes) {
      break;
    }
    result += character;
    byteLength += characterBytes;
  }
  return result;
}
