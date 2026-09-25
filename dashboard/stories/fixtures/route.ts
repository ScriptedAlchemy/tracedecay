/**
 * Playwright network interception backed by the same fixtures MSW serves
 * (`data.ts`). Registering this on a page routes every `/api/**` request to a
 * fixture payload, so the visual audit renders each surface without a live
 * daemon. Kept separate from the MSW handlers because Playwright intercepts at
 * the browser network layer (page.route), not via a service worker.
 */
import type { Page, Route } from '@playwright/test';
import { lookupFixture } from './data.ts';

/** Requests no fixture modelled, as `METHOD /path?query`, in arrival order. */
export type UnmatchedRequests = readonly string[];

async function fulfillApi(route: Route, unmatched: string[]): Promise<void> {
  const request = route.request();
  const url = new URL(request.url());

  // The daemon event stream: answer with an empty, closed event-stream so the
  // app settles into its "offline" liveness state instead of hanging on a
  // pending EventSource. EventSource will retry; that is harmless for a
  // short-lived screenshot window.
  if (url.pathname === '/api/events') {
    await route.fulfill({
      status: 200,
      contentType: 'text/event-stream',
      headers: { 'cache-control': 'no-cache', connection: 'keep-alive' },
      body: ':ok\n\n',
    });
    return;
  }

  const payload = lookupFixture(url.pathname, url.search);
  if (payload === undefined) {
    // Answered the way the daemon answers an unbound route, and recorded so the
    // run fails: a surface reading a path nobody modelled is being audited
    // against a state the daemon would never produce.
    unmatched.push(`${request.method()} ${url.pathname}${url.search}`);
    await route.fulfill({ status: 404, contentType: 'text/plain', body: 'no fixture' });
    return;
  }
  await route.fulfill({
    status: 200,
    contentType: 'application/json',
    body: JSON.stringify(payload),
  });
}

/** Intercept all `/api/**` traffic on the page and serve fixtures. The returned
 * list fills with every request no fixture answered. */
export async function installApiFixtures(page: Page): Promise<UnmatchedRequests> {
  const unmatched: string[] = [];
  await page.route('**/api/**', (route) => fulfillApi(route, unmatched));
  return unmatched;
}
