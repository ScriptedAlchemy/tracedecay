/**
 * A `Response` whose headers have already arrived and whose body stays open
 * until `signal` aborts, then fails the way a browser fails a cancelled body
 * read: the stream errors with the abort reason and `response.json()` rejects
 * with it. This is the shape a request takes when the caller cancels after
 * `fetch` resolved but before the body was consumed.
 */
export function responseWithBodyPendingUntilAbort(
  status: number,
  signal: AbortSignal | null | undefined,
): Response {
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      if (signal?.aborted === true) {
        controller.error(signal.reason);
        return;
      }
      signal?.addEventListener('abort', () => controller.error(signal.reason));
    },
  });
  return new Response(body, { status, headers: { 'content-type': 'application/json' } });
}
