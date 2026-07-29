import { EnvelopeSchema, type WireDomainState, type WireEnvelope } from '../../contracts/wire.ts';
import type { WireSchema } from './wireSchema.ts';

/** Result of an envelope fetch. Transport failures become truthful domain
 * states rather than exceptions: the UI always has a state to render. */
export type EnvelopeResult<T> =
  | { outcome: 'envelope'; envelope: WireEnvelope<T> }
  | { outcome: 'transport'; state: WireDomainState; detail?: string };

/** Fetches and decodes a DashboardEnvelopeV1<T> from a daemon API route.
 * - network failure → offline
 * - non-2xx → error (detail = http status)
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
  if (!response.ok) {
    return { outcome: 'transport', state: 'error', detail: `HTTP ${response.status}` };
  }
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    return { outcome: 'transport', state: 'unsupported_schema' };
  }
  const parsed = EnvelopeSchema(payloadSchema).safeParse(body);
  if (!parsed.success) {
    return { outcome: 'transport', state: 'unsupported_schema' };
  }
  return { outcome: 'envelope', envelope: parsed.data as WireEnvelope<T> };
}
