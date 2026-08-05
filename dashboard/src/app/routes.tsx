import { Suspense, lazy } from 'react';
import { createBrowserRouter } from 'react-router';
import { Shell } from './shell/Shell';

/** Chunk-load fallback: same geometry as page headers (zero CLS). */
function ChunkFallback() {
  return (
    <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
      <span className="text-sm font-semibold tracking-tight text-text-muted">Loading…</span>
    </div>
  );
}

function page<T extends string>(path: T, label: string, load: () => Promise<{ default: () => React.JSX.Element }>) {
  return { path, label, Page: lazy(load) } as const;
}

// Each workspace is its own lazy code-split chunk: the shell stays light and a
// surface loads on first navigation. A surface is listed only while its
// production routes are mounted.
export const WORKSPACES = [
  page('brain', 'Brain', () =>
    import('../workspaces/brain/BrainPage.tsx').then((m) => ({ default: m.BrainPage }))),
  page('explorer', 'Explorer', () =>
    import('../workspaces/explorer/ExplorerPage.tsx').then((m) => ({ default: m.ExplorerPage }))),
  page('loom', 'Loom', () =>
    import('../workspaces/loom/LoomPage.tsx').then((m) => ({ default: m.LoomPage }))),
  page('sessions', 'Sessions', () =>
    import('../workspaces/sessions/SessionsPage.tsx').then((m) => ({ default: m.SessionsPage }))),
  page('agents', 'Agents', () =>
    import('../workspaces/agents/AgentsPage.tsx').then((m) => ({ default: m.AgentsPage }))),
  page('code', 'Code', () =>
    import('../workspaces/code/CodePage.tsx').then((m) => ({ default: m.CodePage }))),
  page('knowledge', 'Knowledge', () =>
    import('../workspaces/knowledge/KnowledgePage.tsx').then((m) => ({ default: m.KnowledgePage }))),
  page('delivery', 'Delivery', () =>
    import('../workspaces/delivery/DeliveryPage.tsx').then((m) => ({ default: m.DeliveryPage }))),
  page('automations', 'Automations', () =>
    import('../workspaces/automations/AutomationsPage.tsx').then((m) => ({ default: m.AutomationsPage }))),
  page('observatory', 'Observatory', () =>
    import('../workspaces/observatory/ObservatoryPage.tsx').then((m) => ({ default: m.ObservatoryPage }))),
  page('costs', 'Costs', () =>
    import('../workspaces/costs/CostsPage.tsx').then((m) => ({ default: m.CostsPage }))),
  page('settings', 'Settings', () =>
    import('../workspaces/settings/SettingsPage.tsx').then((m) => ({ default: m.SettingsPage }))),
] as const;

/** Brain is the index surface: the all-projects aggregate. */
const BrainIndex = WORKSPACES[0].Page;

export const router = createBrowserRouter([
  {
    path: '/',
    element: <Shell />,
    children: [
      {
        index: true,
        element: (
          <Suspense fallback={<ChunkFallback />}>
            <BrainIndex />
          </Suspense>
        ),
      },
      ...WORKSPACES.map(({ path, Page }) => ({
        path,
        element: (
          <Suspense fallback={<ChunkFallback />}>
            <Page />
          </Suspense>
        ),
      })),
    ],
  },
]);
