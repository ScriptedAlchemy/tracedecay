/**
 * The one body decoder every fetch seam shares.
 *
 * Two readings must never be confused: a body that is not JSON, and a body
 * whose read this dashboard cancelled. `response.json()` rejects for both, and
 * the catch-all decoders this replaced reported the second as the first — a
 * cancellation laundered into a schema mismatch. The contract cases each seam
 * adds on top (payload, envelope, registry) live beside those seams.
 */
import { describe, expect, it } from 'vitest';

import { decodeJsonBody, UNDECODABLE } from './responseBody.ts';
import { responseWithBodyPendingUntilAbort } from '../../test/pendingBody.ts';

describe('decodeJsonBody', () => {
  it.each([
    ['a malformed body', 'not json', UNDECODABLE],
    // Legal JSON `null` is a body, not a decode failure: the caller's schema
    // decides what to make of it.
    ['a legal null body', 'null', null],
    ['an object body', '{"status":"ok"}', { status: 'ok' }],
  ])('decodes %s', async (_name, text, expected) => {
    expect(await decodeJsonBody(new Response(text), null)).toEqual(expected);
  });

  it('rethrows when the request signal aborted the body read', async () => {
    const controller = new AbortController();
    const pending = decodeJsonBody(
      responseWithBodyPendingUntilAbort(200, controller.signal),
      controller.signal,
    );
    controller.abort();
    await expect(pending).rejects.toThrow(/abort/i);
  });

  it('rethrows a caller-named abort reason, keyed on the signal rather than the error', async () => {
    const controller = new AbortController();
    const reason = new Error('scope changed');
    const pending = decodeJsonBody(
      responseWithBodyPendingUntilAbort(200, controller.signal),
      controller.signal,
    );
    controller.abort(reason);
    await expect(pending).rejects.toBe(reason);
  });

  it('rethrows an AbortError even with no signal to consult', async () => {
    const abort = new DOMException('The operation was aborted.', 'AbortError');
    const response = new Response(
      new ReadableStream<Uint8Array>({
        start(controller) {
          controller.error(abort);
        },
      }),
    );
    await expect(decodeJsonBody(response, null)).rejects.toBe(abort);
  });

  it('reports a non-abort stream failure as undecodable, not as a cancellation', async () => {
    const response = new Response(
      new ReadableStream<Uint8Array>({
        start(controller) {
          controller.error(new TypeError('connection reset'));
        },
      }),
    );
    expect(await decodeJsonBody(response, new AbortController().signal)).toBe(UNDECODABLE);
  });
});
