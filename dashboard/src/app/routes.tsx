import { Suspense, lazy } from 'react';
import { createBrowserRouter } from 'react-router';
import { Shell } from './shell/Shell';
import { WorkspacePlaceholder } from './shell/WorkspacePlaceholder';

// One lazy, code-split chunk per workspace (plan 11): the shell stays light
// and each surface loads on first navigation.
const WIRED: Record<string, React.LazyExoticComponent<() => React.JSX.Element>> = {
  brain: lazy(() => import('../workspaces/brain/BrainPage.tsx').then((m) => ({ default: m.BrainPage }))),
  explorer: lazy(() => import('../workspaces/explorer/ExplorerPage.tsx').then((m) => ({ default: m.ExplorerPage }))),
  loom: lazy(() => import('../workspaces/loom/LoomPage.tsx').then((m) => ({ default: m.LoomPage }))),
  agents: lazy(() => import('../workspaces/agents/AgentsPage.tsx').then((m) => ({ default: m.AgentsPage }))),
  sessions: lazy(() => import('../workspaces/sessions/SessionsPage.tsx').then((m) => ({ default: m.SessionsPage }))),
  knowledge: lazy(() => import('../workspaces/knowledge/KnowledgePage.tsx').then((m) => ({ default: m.KnowledgePage }))),
  code: lazy(() => import('../workspaces/code/CodePage.tsx').then((m) => ({ default: m.CodePage }))),
  delivery: lazy(() => import('../workspaces/delivery/DeliveryPage.tsx').then((m) => ({ default: m.DeliveryPage }))),
  costs: lazy(() => import('../workspaces/costs/CostsPage.tsx').then((m) => ({ default: m.CostsPage }))),
  automations: lazy(() => import('../workspaces/automations/AutomationsPage.tsx').then((m) => ({ default: m.AutomationsPage }))),
  observatory: lazy(() => import('../workspaces/observatory/ObservatoryPage.tsx').then((m) => ({ default: m.ObservatoryPage }))),
  settings: lazy(() => import('../workspaces/settings/SettingsPage.tsx').then((m) => ({ default: m.SettingsPage }))),
};

const BrainIndex = WIRED['brain']!;

/** Chunk-load fallback: same geometry as page headers (zero CLS). */
function ChunkFallback() {
  return (
    <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
      <span className="text-sm font-semibold tracking-tight text-text-muted">Loading…</span>
    </div>
  );
}

// The twelve PR14 workspaces (plan 11). Each becomes a lazy route module as
// its slice ships; until then the designed placeholder renders its truthful
// pending state — never a blank page (plan: no navigation stubs at ship time;
// placeholders exist only during the build-out).
export const WORKSPACES = [
  { path: 'brain', label: 'Brain' },
  { path: 'explorer', label: 'Explorer' },
  { path: 'loom', label: 'Loom' },
  { path: 'sessions', label: 'Sessions' },
  { path: 'agents', label: 'Agents' },
  { path: 'code', label: 'Code' },
  { path: 'knowledge', label: 'Knowledge' },
  { path: 'delivery', label: 'Delivery' },
  { path: 'automations', label: 'Automations' },
  { path: 'observatory', label: 'Observatory' },
  { path: 'costs', label: 'Costs' },
  { path: 'settings', label: 'Settings' },
] as const;

export type WorkspacePath = (typeof WORKSPACES)[number]['path'];

export const router = createBrowserRouter([
  {
    path: '/',
    element: <Shell />,
    children: [
      {
        index: true,
        element: (
          <Suspense fallback={<ChunkFallback />}>
            {WIRED['brain'] ? <BrainIndex /> : <WorkspacePlaceholder workspace="brain" />}
          </Suspense>
        ),
      },
      ...WORKSPACES.map((w) => {
        const Wired = WIRED[w.path];
        return {
          path: w.path,
          element: Wired ? (
            <Suspense fallback={<ChunkFallback />}>
              <Wired />
            </Suspense>
          ) : (
            <WorkspacePlaceholder workspace={w.path} />
          ),
        };
      }),
    ],
  },
]);
