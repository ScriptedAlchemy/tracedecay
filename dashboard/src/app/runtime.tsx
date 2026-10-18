import type { ReactNode } from 'react';
import { createBrowserRouter } from 'react-router';
import { EventsProvider } from '../data/sse/useEvents.tsx';
import { scopedUrl, useScope } from '../data/scope/store.ts';
import {
  applyEmbeddedPublicPath,
  dashboardRouterBasename,
  HERMES_EMBED_BASENAME,
} from './embedBasename.ts';
import { dashboardRoutes, router as defaultRouter } from './routes';

export {
  applyEmbeddedPublicPath,
  dashboardRouterBasename,
  HERMES_EMBED_BASENAME,
};

/** Same tree as `router` in `routes.tsx`, with the Hermes embed basename. */
function createDashboardRouter(basename: string) {
  return createBrowserRouter(dashboardRoutes(), { basename });
}

export function dashboardRouterForPath(pathname: string) {
  const basename = dashboardRouterBasename(pathname);
  return basename === undefined ? defaultRouter : createDashboardRouter(basename);
}

/**
 * One EventSource for the selected dashboard scope. `connectEvents` derives
 * delivery acknowledgements from the same URL, so both the stream and its
 * receipts follow `/api/projects/{id}/…` when a project is selected.
 */
export function ScopedEventsProvider({ children }: { children: ReactNode }) {
  const scope = useScope((s) => s.scope);
  const eventsUrl = scopedUrl(scope, '/api/events');
  return <EventsProvider url={eventsUrl}>{children}</EventsProvider>;
}
