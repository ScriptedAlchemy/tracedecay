import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render } from '@testing-library/react';
import { vi } from 'vitest';
import { MemoryRouter, useLocation } from 'react-router';
import type { DeliveryInboxV1, DeliveryOverviewV1 } from '../contracts/generated.ts';
import { DeliveryPage } from '../workspaces/delivery/DeliveryPage.tsx';
import { fixtureEnvelope } from './fixtureEnvelope.ts';

/** Prints the router's current search string so a test can assert URL state. */
export function LocationProbe() {
  return <output data-testid="location">{useLocation().search}</output>;
}

export function serveRoutes(routes: Record<string, { status: number; body: unknown }>) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    const hit = Object.entries(routes).find(([path]) => url.includes(path));
    const { status, body } = hit?.[1] ?? { status: 404, body: { status: 'not_found' } };
    return {
      ok: status >= 200 && status < 300,
      status,
      json: async () => body,
    } as Response;
  });
}

/**
 * Mount the Delivery channel against a served inbox and, optionally, one
 * project-scoped overview answered for every `/api/projects/{id}/delivery/overview`
 * read. Callers stub nothing else; `vi.unstubAllGlobals()` in `afterEach`
 * restores `fetch`.
 */
export function renderDelivery(
  payload: DeliveryInboxV1,
  options: {
    domainState?: string;
    route?: string;
    overview?: DeliveryOverviewV1;
    overviewStatus?: number;
    overviewDomainState?: string;
  } = {},
) {
  const fetchMock = serveRoutes({
    '/api/delivery/inbox': {
      status: 200,
      body: fixtureEnvelope(payload, options.domainState ?? 'ready'),
    },
    ...(options.overview === undefined
      ? {}
      : {
          '/delivery/overview': {
            status: options.overviewStatus ?? 200,
            body: fixtureEnvelope(options.overview, options.overviewDomainState ?? 'ready'),
          },
        }),
  });
  vi.stubGlobal('fetch', fetchMock);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[options.route ?? '/delivery']}>
        <DeliveryPage />
        <LocationProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return { ...view, fetchMock };
}

export const PR_42 = 'project.alpha%3Agithub%3A42';
export const PR_43 = 'project.alpha%3Agithub%3A43';
export const PR_8 = 'project.beta%3Agithub%3A8';
