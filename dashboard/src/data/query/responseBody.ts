/** Sentinel for a body that was not JSON at all, kept distinct from a body
 * that decoded to `null` — the second is a legal body that must still fail
 * the caller's schema rather than being mistaken for a decode failure. */
export const UNDECODABLE: unique symbol = Symbol('undecodable');

/**
 * Decode a response body as JSON without laundering a cancellation.
 *
 * `response.json()` rejects identically for a malformed body and for a body
 * whose consumption was aborted after the headers arrived. Only the second is
 * the caller's own doing — a scope change or an SSE invalidation abandoning a
 * read — and it must stay a rejection: React Query recognises the abort and
 * leaves the cache entry untouched, whereas a fabricated `UNDECODABLE` would
 * be written into the abandoned scope as a schema mismatch nobody observed.
 * Every fetch seam therefore threads its request signal through here.
 */
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

function isAbortError(err: unknown): boolean {
  return err instanceof Error && err.name === 'AbortError';
}
