import { DashboardEnvelopeV1Schema, type DashboardDomainStateV1, type DashboardEnvelopeV1 } from '../../contracts/generated.ts';
import type { WireSchema } from './wireSchema.ts';
import { readOnlyScopeRefusal } from '../scope/store.ts';

/** Result of an envelope fetch. Transport failures become truthful domain
 * states rather than exceptions: the UI always has a state to render. */
export type EnvelopeResult<T> =
  | { outcome: 'envelope'; envelope: DashboardEnvelopeV1<T> }
  | { outcome: 'transport'; state: DashboardDomainStateV1; detail?: string };

/** Fetches and decodes a DashboardEnvelopeV1<T> from a daemon API route.
 * - network failure → offline
 * - project gateway read-only refusal → locked (detail = the daemon's reason)
 * - other non-2xx → error (detail = http status)
 * - undecodable body → unsupported_schema (never a crash, never fake-empty)
 */
export async function fetchEnvelope<T>(
  url: string,
  payloadSchema: WireSchema<T>,
  init?: RequestInit,
): Promise<EnvelopeResult<T>> {
  let response: Response;
  try {
    response = await fetch(url, { headers: { accept: 'application/json' }, ...init });
  } catch {
    return { outcome: 'transport', state: 'offline' };
  }
  // A write refused because its project is not the active one is `locked`, not
  // `error`: the request was well-formed and the daemon healthy, and the
  // remedy is to change scope rather than to retry. This result type is keyed
  // by domain state rather than by a transport outcome, so the refusal is
  // reported in that vocabulary — `locked` is the taxonomy's word for a
  // surface that will not accept a change — and carries the daemon's sentence.
  if (response.status === 405) {
    const refusal = readOnlyScopeRefusal(await response.json().catch(() => null));
    if (refusal) {
      return { outcome: 'transport', state: 'locked', detail: refusal.detail };
    }
    return { outcome: 'transport', state: 'error', detail: 'HTTP 405' };
  }
  if (!response.ok) {
    return { outcome: 'transport', state: 'error', detail: `HTTP ${response.status}` };
  }
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    return { outcome: 'transport', state: 'unsupported_schema' };
  }
  const parsed = DashboardEnvelopeV1Schema(payloadSchema).safeParse(body);
  if (!parsed.success) {
    return { outcome: 'transport', state: 'unsupported_schema' };
  }
  return { outcome: 'envelope', envelope: parsed.data as DashboardEnvelopeV1<T> };
}
