import { decodeJsonBody, fetchJsonResponse, UNDECODABLE } from './responseBody.ts';
import type { WireSchema } from './wireSchema.ts';
import { readOnlyScopeRefusal, type ReadOnlyScopeRefusal } from '../scope/store.ts';

/** Legacy payload routes publish the schema itself, not a DashboardEnvelope.
 * Keep their canonical unavailable responses distinct from transport errors. */
export type PayloadResult<T> =
  | { outcome: 'ok'; data: T }
  | {
      outcome: 'unavailable';
      httpStatus: number;
      status: string;
      reason: string | null;
      data: T;
    }
  | { outcome: 'offline' }
  | { outcome: 'unauthorized' }
  | { outcome: 'denied' }
  | { outcome: 'error'; detail: string }
  | { outcome: 'unsupported_schema' };

/** The project gateway's read-only refusal belongs to writes, not reads. */
export type PayloadWriteResult<T> =
  | PayloadResult<T>
  | { outcome: 'read_only_scope'; refusal: ReadOnlyScopeRefusal };

/** Only a canonical discriminant may turn a 404/503 into a typed condition;
 * an arbitrary proxy error accepted by an open schema is not that evidence. */
function canonicalFailure(body: unknown): { status: string; reason: string | null } | null {
  if (typeof body !== 'object' || body === null) return null;
  const record = body as Record<string, unknown>;
  const status = record['status'];
  if (typeof status !== 'string' || status === 'ok') return null;
  const error = record['error'];
  return { status, reason: typeof error === 'string' && error !== '' ? error : null };
}

async function readPayloadResponse<T>(
  url: string,
  schema: WireSchema<T>,
  init?: RequestInit,
): Promise<PayloadWriteResult<T>> {
  const opened = await fetchJsonResponse(url, init);
  if (opened.outcome !== 'response') return opened;
  const { response } = opened;
  if (response.status === 405) {
    const refusal = readOnlyScopeRefusal(await decodeJsonBody(response, init?.signal));
    if (refusal) return { outcome: 'read_only_scope', refusal };
    return { outcome: 'error', detail: 'HTTP 405' };
  }
  // These published routes use 404 for absent IDs and 503 for unavailable
  // registries. Their typed response must survive the non-2xx status.
  if (response.status === 404 || response.status === 503) {
    const body = await decodeJsonBody(response, init?.signal);
    const canonical = body === UNDECODABLE ? null : canonicalFailure(body);
    if (canonical !== null) {
      const parsed = schema.safeParse(body);
      if (!parsed.success) return { outcome: 'unsupported_schema' };
      return {
        outcome: 'unavailable',
        httpStatus: response.status,
        status: canonical.status,
        reason: canonical.reason,
        data: parsed.data,
      };
    }
    return { outcome: 'error', detail: `HTTP ${response.status}` };
  }
  if (!response.ok) {
    return { outcome: 'error', detail: `HTTP ${response.status}` };
  }
  const body = await decodeJsonBody(response, init?.signal);
  if (body === UNDECODABLE) return { outcome: 'unsupported_schema' };
  const parsed = schema.safeParse(body);
  if (!parsed.success) return { outcome: 'unsupported_schema' };
  return { outcome: 'ok', data: parsed.data };
}

/** Reads preserve the existing generic-error presentation if a caller sends
 * a mutation. Write controls use fetchPayloadWrite for a typed scope refusal. */
export async function fetchPayload<T>(
  url: string,
  schema: WireSchema<T>,
  init?: RequestInit,
): Promise<PayloadResult<T>> {
  const result = await readPayloadResponse(url, schema, init);
  return result.outcome === 'read_only_scope'
    ? { outcome: 'error', detail: result.refusal.detail }
    : result;
}

export function fetchPayloadWrite<T>(
  url: string,
  schema: WireSchema<T>,
  init: RequestInit,
): Promise<PayloadWriteResult<T>> {
  return readPayloadResponse(url, schema, init);
}
