/**
 * Playwright network interception backed by the same fixtures MSW serves
 * (`data.ts`). Registering this on a page routes every `/api/**` request to a
 * fixture payload, so the visual audit renders each surface without a live
 * daemon. Kept separate from the MSW handlers because Playwright intercepts at
 * the browser network layer (page.route), not via a service worker.
 */
import type { Page, Route } from '@playwright/test';
import { resolveFixture } from './data.ts';

async function fulfillApi(route: Route): Promise<void> {
  const url = new URL(route.request().url());

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

  await route.fulfill({
    status: 200,
    contentType: 'application/json',
    body: JSON.stringify(resolveFixture(url.pathname, url.search)),
  });
}

/** Intercept all `/api/**` traffic on the page and serve fixtures. */
export async function installApiFixtures(page: Page): Promise<void> {
  await page.route('**/api/**', fulfillApi);
}
