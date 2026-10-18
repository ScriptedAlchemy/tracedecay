import { DashboardEnvelopeV1Schema, type DashboardDomainStateV1, type DashboardEnvelopeV1 } from '../../contracts/generated.ts';
import { decodeJsonBody, fetchJsonResponse } from './responseBody.ts';
import type { WireSchema } from './wireSchema.ts';
import { readOnlyScopeRefusal } from '../scope/store.ts';

/** Transport failures are presentation states; caller cancellation remains a
 * rejection so abandoned queries cannot cache fabricated server failures. */
export type EnvelopeResult<T> =
  | { outcome: 'envelope'; envelope: DashboardEnvelopeV1<T> }
  | { outcome: 'transport'; state: DashboardDomainStateV1; detail?: string; responseReceived?: true };

/** Dashboard envelopes retain domain state even on non-2xx responses. Unlike
 * legacy payload routes, an absent payload can itself be a typed reading. */
export async function fetchEnvelope<T>(
  url: string,
  payloadSchema: WireSchema<T>,
  init?: RequestInit,
): Promise<EnvelopeResult<T>> {
  const opened = await fetchJsonResponse(url, init);
  if (opened.outcome !== 'response') {
    return { outcome: 'transport', state: opened.outcome };
  }
  const { response } = opened;
  if (response.status === 405) {
    const refusal = readOnlyScopeRefusal(await decodeJsonBody(response, init?.signal));
    if (refusal) {
      return { outcome: 'transport', state: 'locked', detail: refusal.detail };
    }
    return { outcome: 'transport', state: 'error', detail: 'HTTP 405' };
  }
  // A warming graph can answer 503 with a complete, useful typed envelope.
  const decoded = decodeEnvelopeBody<T>(
    payloadSchema,
    await decodeJsonBody(response, init?.signal),
  );
  if (decoded) return decoded;
  return response.ok
    ? { outcome: 'transport', state: 'unsupported_schema' }
    : { outcome: 'transport', state: 'error', detail: `HTTP ${response.status}` };
}

/** Decode the outer envelope before the payload. A null payload preserves
 * the daemon's absence/refusal state, rather than inventing a schema error. */
function decodeEnvelopeBody<T>(
  payloadSchema: WireSchema<T>,
  body: unknown,
): EnvelopeResult<T> | null {
  const parsed = DashboardEnvelopeV1Schema(payloadSchema.nullable()).safeParse(body);
  if (!parsed.success) {
    return null;
  }
  const envelope = parsed.data;
  if (envelope.payload === null) {
    return {
      outcome: 'transport',
      state: envelope.domain_state,
      detail: envelope.coverage.omission_reasons[0],
      responseReceived: true,
    };
  }
  return { outcome: 'envelope', envelope: envelope as DashboardEnvelopeV1<T> };
}
