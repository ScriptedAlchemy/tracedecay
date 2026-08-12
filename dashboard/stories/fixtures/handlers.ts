/**
 * MSW request handlers for the dashboard `/api` surfaces. These are the
 * canonical fixture mocks (plan 11a): they answer every workspace's GET calls
 * from `data.ts` without a running daemon. They are used directly by MSW in
 * node/jsdom test contexts (`setupServer(...handlers)`), and the same fixture
 * data backs the Playwright route interceptor used by the visual audit
 * (`route.ts`), so both transports stay in lockstep.
 *
 * The second half of the file is the fault-injection side: the same server,
 * with one or every route made to fail, so DOM tests can drive a page through
 * a real transport failure instead of a hand-stubbed `fetch`.
 */
import { http, HttpResponse, type JsonBodyType, type RequestHandler } from 'msw';
import { setupServer, type SetupServer } from 'msw/node';
import { resolveFixture } from './data.ts';

/** Catch-all GET for /api/** — resolves the pathname to its fixture payload. */
export const handlers = [
  http.get('*/api/events', () =>
    // The event stream is intentionally empty in fixtures: the app degrades to
    // its "offline" liveness state, which is a valid surface to audit.
    new HttpResponse('', {
      status: 200,
      headers: { 'content-type': 'text/event-stream' },
    }),
  ),
  http.get('*/api/*', ({ request }) => {
    const url = new URL(request.url);
    return HttpResponse.json(resolveFixture(url.pathname, url.search) as JsonBodyType);
  }),
  http.post('*/api/automation/run/memory-curator', ({ request }) => {
    const url = new URL(request.url);
    return HttpResponse.json(resolveFixture(url.pathname, url.search) as JsonBodyType);
  }),
  // The Work routes are the one family the dashboard reads with POST — they are
  // nested onto the application router, whose reads take a request body. Scoped
  // to `/api/work/*` rather than to `/api/*` on purpose: a POST catch-all would
  // turn every unmodelled command in every other workspace's MSW test from a
  // loud unhandled-request error into a silent empty body.
  http.post('*/api/work/*', ({ request }) => {
    const url = new URL(request.url);
    return HttpResponse.json(resolveFixture(url.pathname, url.search) as JsonBodyType);
  }),
];

/* ==========================================================================
 * HTTP fault injection (plan 11, "MSW covers HTTP/SSE faults")
 *
 * The transport failures a workspace read can actually suffer, named after
 * what went wrong on the wire rather than after the state they happen to
 * produce today. Tests assert the mapping from one to the other, so encoding
 * the expected state here would make those assertions circular.
 * ======================================================================= */

export type HttpFault =
  /** The daemon answered, and its handler failed. */
  | 'server_error'
  /** The route is not bound — a renamed or unbuilt API surface. */
  | 'not_found'
  /** No identity: the caller is not authenticated. */
  | 'unauthorized'
  /** A known identity without permission for this scope. */
  | 'forbidden'
  /** No answer at all: daemon down, socket refused, DNS gone. */
  | 'network_error'
  /** 200 with a body that is not JSON. A proxy or dev server answering an
   * unbound `/api` route with the SPA's own `index.html` produces exactly
   * this, which is why it is HTML here and not random bytes. */
  | 'malformed_body'
  /** 200 with well-formed JSON the build's decoder rejects — the daemon moved
   * ahead of this bundle. A list where every schema expects an object is the
   * cheapest shape that no workspace decoder accepts. */
  | 'unsupported_shape';

const SPA_INDEX_HTML =
  '<!doctype html><html><head><title>TraceDecay</title></head><body><div id="root"></div></body></html>';

/** The response (or transport failure) for one fault. */
export function faultResponse(fault: HttpFault): Response {
  switch (fault) {
    case 'server_error':
      return new HttpResponse('handler panicked', { status: 500 });
    case 'not_found':
      return new HttpResponse('no route', { status: 404 });
    case 'unauthorized':
      return new HttpResponse('missing credentials', { status: 401 });
    case 'forbidden':
      return new HttpResponse('scope not granted', { status: 403 });
    case 'network_error':
      return HttpResponse.error();
    case 'malformed_body':
      return new HttpResponse(SPA_INDEX_HTML, {
        status: 200,
        headers: { 'content-type': 'text/html' },
      });
    case 'unsupported_shape':
      return HttpResponse.json([{ unexpected_row: true }]);
    default: {
      const exhaustive: never = fault;
      throw new Error(`unhandled fault: ${String(exhaustive)}`);
    }
  }
}

// A runtime override that makes one route fail. `pathPattern` is an MSW path
// such as `'*/api/automation/jobs'`; pass the handler to `server.use(...)`,
// which prepends it ahead of the fixture catch-all for the rest of the test.
export function faultHandler(pathPattern: string, fault: HttpFault): RequestHandler {
  return http.get(pathPattern, () => faultResponse(fault));
}

/** Every `/api` GET fails the same way — the whole daemon is in one bad state. */
export function allRoutesFail(fault: HttpFault): RequestHandler {
  return faultHandler('*/api/*', fault);
}

/**
 * A `setupServer` bound to the fixture handlers, so an un-faulted route answers
 * with the same payload the visual audit sees. Call `listen`/`resetHandlers`/
 * `close` from the test file's own lifecycle hooks; unhandled requests throw,
 * so a route a page reads but the fixtures do not model fails loudly instead of
 * silently reaching the network.
 */
export function fixtureServer(): SetupServer {
  return setupServer(...handlers);
}
