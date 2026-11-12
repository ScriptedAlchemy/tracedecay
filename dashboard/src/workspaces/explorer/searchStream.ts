/**
 * One Explorer search, read off one response.
 *
 * The request owns the search: the server publishes each source's result as it
 * finishes and one final frame, and closing the response (the abort signal)
 * stops the reads it still had open. There is no run to look up afterwards, so
 * a superseded search is dropped by aborting it, not by polling or cancelling
 * an identity.
 */
import {
  ExplorerStreamEventV1Schema,
  type DashboardDomainStateV1,
  type ExplorerQueryRequestV1,
  type ExplorerStreamEventV1,
} from '../../contracts/generated.ts';
import { decodeJsonBody, fetchJsonResponse } from '../../data/query/responseBody.ts';
import { readOnlyScopeRefusal } from '../../data/scope/store.ts';
import { readSseFrames } from '../../data/sse/readFrames.ts';

/** Why a search could not be read to its close. `interrupted` is a response
 * that opened and then ended before the final frame; everything else is a
 * failure to open one. */
export type SearchStreamFailure =
  | { readonly outcome: 'transport'; readonly state: DashboardDomainStateV1; readonly detail?: string }
  | { readonly outcome: 'interrupted' };

/**
 * Open a search and hand each decoded frame to `onEvent` in arrival order.
 * Resolves `null` once the response closes after its final frame. A cancelled
 * `signal` rejects with the abort, never with a fabricated failure.
 */
export async function streamExplorerSearch(
  url: string,
  request: ExplorerQueryRequestV1,
  signal: AbortSignal,
  onEvent: (event: ExplorerStreamEventV1) => void,
): Promise<SearchStreamFailure | null> {
  const init: RequestInit = {
    method: 'POST',
    headers: { accept: 'text/event-stream', 'content-type': 'application/json' },
    body: JSON.stringify(request),
    signal,
  };
  const opened = await fetchJsonResponse(url, init);
  if (opened.outcome !== 'response') return { outcome: 'transport', state: opened.outcome };
  const { response } = opened;
  if (!response.ok || response.body === null) {
    if (response.status === 405) {
      const refusal = readOnlyScopeRefusal(await decodeJsonBody(response, signal));
      if (refusal) return { outcome: 'transport', state: 'locked', detail: refusal.detail };
    }
    return { outcome: 'transport', state: 'error', detail: `HTTP ${response.status}` };
  }
  if (response.headers.get('content-type')?.split(';')[0]?.trim().toLowerCase() !== 'text/event-stream') {
    await response.body.cancel();
    return { outcome: 'transport', state: 'unsupported_schema' };
  }

  try {
    for await (const frame of readSseFrames(response.body)) {
      if (frame.data === '') continue;
      let json: unknown;
      try {
        json = JSON.parse(frame.data);
      } catch {
        return { outcome: 'transport', state: 'unsupported_schema' };
      }
      const parsed = ExplorerStreamEventV1Schema.safeParse(json);
      if (!parsed.success || frame.type !== parsed.data.event) {
        return { outcome: 'transport', state: 'unsupported_schema' };
      }
      onEvent(parsed.data);
      if (parsed.data.event === 'final') return null;
    }
  } catch (err) {
    if (signal.aborted) throw err;
    return { outcome: 'interrupted' };
  }
  return { outcome: 'interrupted' };
}
