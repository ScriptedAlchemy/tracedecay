import { createBrowserRouter } from 'react-router';
import { RouteChunkBoundary, type RouteChunkLoader } from './RouteChunkBoundary';
import { Shell } from './shell/Shell';

function page<T extends string>(path: T, label: string, load: RouteChunkLoader) {
  return { path, label, load } as const;
}

// The fourteen workspaces, each its own lazy code-split chunk: the shell stays
// light and a surface loads on first navigation. All fourteen read real routes.
// What has not changed is the rule the original Work gate enforced: a surface
// renders what its contract answered, and never substitutes fixture or
// browser-owned state for a read that did not land.
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
  page('work', 'Work', () =>
    import('../workspaces/work/WorkPage.tsx').then((m) => ({ default: m.WorkPage }))),
  page('workflows', 'Workflows', () =>
    import('../workspaces/workflows/WorkflowsPage.tsx').then((m) => ({ default: m.WorkflowsPage }))),
] as const;

export const router = createBrowserRouter([
  {
    path: '/',
    element: <Shell />,
    children: [
      {
        index: true,
        element: <RouteChunkBoundary load={WORKSPACES[0].load} />,
      },
      ...WORKSPACES.map(({ path, load }) => ({
        path,
        element: <RouteChunkBoundary load={load} />,
      })),
    ],
  },
]);
