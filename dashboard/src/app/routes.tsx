import { createBrowserRouter, Navigate, useLocation, type RouteObject } from 'react-router';
import { CHANNELS, type ChannelView, type ViewPath } from './channels.ts';
import { LEGACY_PATHS, legacyDestination, type LegacyPath } from './legacyRoutes.ts';
import { RouteChunkBoundary } from './RouteChunkBoundary';
import { FamilyLayout } from './shell/FamilyLayout';
import { Shell } from './shell/Shell';
import { VIEW_CHUNKS } from './viewChunks.ts';

function view(path: ViewPath) {
  return <RouteChunkBoundary load={VIEW_CHUNKS[path]} />;
}

/** Carries a retired path to its new home, keeping the query string (project
 * scope included) and the hash. Replaces the entry so Back leaves the
 * dashboard's own history instead of bouncing off the old path. */
function Redirect({ from }: { from: LegacyPath }) {
  const { search, hash } = useLocation();
  const destination = legacyDestination(from, search);
  return <Navigate to={{ pathname: destination.pathname, search: destination.search, hash }} replace />;
}

/** Path of a view relative to its family route. */
function relative(familyPath: string, viewPath: string): string {
  return viewPath.slice(familyPath.length + 1);
}

export function dashboardRoutes(): RouteObject[] {
  return [
    {
      path: '/',
      element: <Shell />,
      children: [
        // `/` is the retired Brain path, so it takes the same redirect and keeps its
        // query string, project scope included.
        { index: true, element: <Redirect from="brain" /> },
        ...CHANNELS.map((channel): RouteObject => {
          const [first, ...rest]: readonly ChannelView[] = channel.views;
          if (first === undefined) throw new Error(`channel ${channel.path} lists no view`);
          if (rest.length === 0) return { path: channel.path, element: view(first.path as ViewPath) };
          return {
            path: channel.path,
            element: <FamilyLayout channel={channel} />,
            children: [
              { index: true, element: view(first.path as ViewPath) },
              ...rest.map((member) => ({
                path: relative(channel.path, member.path),
                element: view(member.path as ViewPath),
              })),
            ],
          };
        }),
        ...(Object.keys(LEGACY_PATHS) as LegacyPath[]).map((from) => ({
          path: from,
          element: <Redirect from={from} />,
        })),
      ],
    },
  ];
}

export const router = createBrowserRouter(dashboardRoutes());
