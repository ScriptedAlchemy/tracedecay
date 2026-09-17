import type { ReactNode } from 'react';
import { createBrowserRouter } from 'react-router';
import { EventsProvider } from '../data/sse/useEvents.tsx';
import { scopedUrl, useScope } from '../data/scope/store.ts';
import {
  applyEmbeddedPublicPath,
  dashboardRouterBasename,
  HERMES_EMBED_BASENAME,
} from './embedBasename.ts';
import { RouteChunkBoundary } from './RouteChunkBoundary';
import { WORKSPACES, router as defaultRouter } from './routes';
import { Shell } from './shell/Shell';

export {
  applyEmbeddedPublicPath,
  dashboardRouterBasename,
  HERMES_EMBED_BASENAME,
};

/** Same tree as `router` in `routes.tsx`, with the Hermes embed basename. */
export function createDashboardRouter(basename: string) {
  const indexWorkspace = WORKSPACES[0];
  if (indexWorkspace === undefined) {
    throw new Error('dashboard workspace list is empty');
  }
  return createBrowserRouter(
    [
      {
        path: '/',
        element: <Shell />,
        children: [
          {
            index: true,
            element: <RouteChunkBoundary load={indexWorkspace.load} />,
          },
          ...WORKSPACES.map(({ path, load }) => ({
            path,
            element: <RouteChunkBoundary load={load} />,
          })),
        ],
      },
    ],
    { basename },
  );
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
