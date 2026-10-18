import type { ViewPath } from './channels.ts';
import type { RouteChunkLoader } from './RouteChunkBoundary';

// One lazy code-split chunk per view: the shell stays light and a surface
// loads on first navigation. A surface renders what its contract answered, and
// never substitutes fixture or browser-owned state for a read that did not
// land. The paths come from `channels.ts`, the one place that lists the five
// task families and their views, so a view cannot be routed that the rail and
// the family tab strip do not know.
export const VIEW_CHUNKS: Record<ViewPath, RouteChunkLoader> = {
  code: () =>
    import('../workspaces/code/CodePage.tsx').then((m) => ({ default: m.CodePage })),
  'code/delivery': () =>
    import('../workspaces/delivery/DeliveryPage.tsx').then((m) => ({ default: m.DeliveryPage })),
  memory: () =>
    import('../workspaces/brain/BrainPage.tsx').then((m) => ({ default: m.BrainPage })),
  'memory/explorer': () =>
    import('../workspaces/explorer/ExplorerPage.tsx').then((m) => ({ default: m.ExplorerPage })),
  'memory/knowledge': () =>
    import('../workspaces/knowledge/KnowledgePage.tsx').then((m) => ({ default: m.KnowledgePage })),
  'memory/sessions': () =>
    import('../workspaces/sessions/SessionsPage.tsx').then((m) => ({ default: m.SessionsPage })),
  'memory/loom': () =>
    import('../workspaces/loom/LoomPage.tsx').then((m) => ({ default: m.LoomPage })),
  'memory/agents': () =>
    import('../workspaces/agents/AgentsPage.tsx').then((m) => ({ default: m.AgentsPage })),
  work: () =>
    import('../workspaces/work/WorkPage.tsx').then((m) => ({ default: m.WorkPage })),
  'work/workflows': () =>
    import('../workspaces/workflows/WorkflowsPage.tsx').then((m) => ({ default: m.WorkflowsPage })),
  operations: () =>
    import('../workspaces/observatory/ObservatoryPage.tsx').then((m) => ({ default: m.ObservatoryPage })),
  'operations/automations': () =>
    import('../workspaces/automations/AutomationsPage.tsx').then((m) => ({ default: m.AutomationsPage })),
  'operations/costs': () =>
    import('../workspaces/costs/CostsPage.tsx').then((m) => ({ default: m.CostsPage })),
  settings: () =>
    import('../workspaces/settings/SettingsPage.tsx').then((m) => ({ default: m.SettingsPage })),
};
