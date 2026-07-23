import { createBrowserRouter } from 'react-router';
import { Shell } from './shell/Shell';
import { WorkspacePlaceholder } from './shell/WorkspacePlaceholder';
import { ObservatoryPage } from '../workspaces/observatory/ObservatoryPage.tsx';

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
      { index: true, element: <WorkspacePlaceholder workspace="brain" /> },
      ...WORKSPACES.map((w) => ({
        path: w.path,
        element:
          w.path === 'observatory' ? (
            <ObservatoryPage />
          ) : (
            <WorkspacePlaceholder workspace={w.path} />
          ),
      })),
    ],
  },
]);
