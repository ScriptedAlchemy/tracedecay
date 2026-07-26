/**
 * MSW request handlers for the dashboard `/api` surfaces. These are the
 * canonical fixture mocks (plan 11a): they answer every workspace's GET calls
 * from `data.ts` without a running daemon. They are used directly by MSW in
 * node/jsdom test contexts (`setupServer(...handlers)`), and the same fixture
 * data backs the Playwright route interceptor used by the visual audit
 * (`route.ts`), so both transports stay in lockstep.
 */
import { http, HttpResponse, type JsonBodyType } from 'msw';
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
];
