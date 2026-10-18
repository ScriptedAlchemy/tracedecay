import { dashboardFetch } from '../auth/credential.ts';

/** Distinguish invalid JSON from the valid JSON value `null`. */
export const UNDECODABLE: unique symbol = Symbol('undecodable');

export type JsonResponse =
  | { outcome: 'response'; response: Response }
  | { outcome: 'offline' }
  | { outcome: 'unauthorized' }
  | { outcome: 'denied' };

/** Shared request/authentication policy. Domain envelopes remain the caller's
 * authority; a non-2xx response may still carry a useful typed reading. */
export async function fetchJsonResponse(
  url: string,
  init?: RequestInit,
): Promise<JsonResponse> {
  const headers = new Headers(init?.headers);
  if (!headers.has('accept')) headers.set('accept', 'application/json');
  let response: Response;
  try {
    response = await dashboardFetch(url, { ...init, headers });
  } catch (err) {
    // Cancellation is not evidence that the daemon is offline.
    if (init?.signal?.aborted === true || isAbortError(err)) throw err;
    return { outcome: 'offline' };
  }
  if (response.status === 401) return { outcome: 'unauthorized' };
  if (response.status === 403) return { outcome: 'denied' };
  return { outcome: 'response', response };
}

/** Decode JSON while preserving cancellation during body consumption. A
 * cancelled query must not cache an invented schema or connectivity failure. */
export async function decodeJsonBody(
  response: Response,
  signal: AbortSignal | null | undefined,
): Promise<unknown> {
  try {
    return await response.json();
  } catch (err) {
    if (signal?.aborted === true || isAbortError(err)) throw err;
    return UNDECODABLE;
  }
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isAbortError(err: unknown): boolean {
  return err instanceof Error && err.name === 'AbortError';
}
