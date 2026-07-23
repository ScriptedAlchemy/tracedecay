import { createBrowserRouter } from 'react-router';
import { Shell } from './shell/Shell';
import { WorkspacePlaceholder } from './shell/WorkspacePlaceholder';
import { ObservatoryPage } from '../workspaces/observatory/ObservatoryPage.tsx';
import { BrainPage } from '../workspaces/brain/BrainPage.tsx';
import { SessionsPage } from '../workspaces/sessions/SessionsPage.tsx';
import { KnowledgePage } from '../workspaces/knowledge/KnowledgePage.tsx';
import { CodePage } from '../workspaces/code/CodePage.tsx';
import { CostsPage } from '../workspaces/costs/CostsPage.tsx';
import { AutomationsPage } from '../workspaces/automations/AutomationsPage.tsx';
import { SettingsPage } from '../workspaces/settings/SettingsPage.tsx';
import { ExplorerPage } from '../workspaces/explorer/ExplorerPage.tsx';
import { LoomPage } from '../workspaces/loom/LoomPage.tsx';
import { AgentsPage } from '../workspaces/agents/AgentsPage.tsx';
import { DeliveryPage } from '../workspaces/delivery/DeliveryPage.tsx';

const WIRED: Record<string, () => React.JSX.Element> = {
  brain: BrainPage,
  explorer: ExplorerPage,
  loom: LoomPage,
  agents: AgentsPage,
  sessions: SessionsPage,
  knowledge: KnowledgePage,
  code: CodePage,
  delivery: DeliveryPage,
  costs: CostsPage,
  automations: AutomationsPage,
  observatory: ObservatoryPage,
  settings: SettingsPage,
};

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
      { index: true, element: <BrainPage /> },
      ...WORKSPACES.map((w) => {
        const Wired = WIRED[w.path];
        return {
          path: w.path,
          element: Wired ? <Wired /> : <WorkspacePlaceholder workspace={w.path} />,
        };
      }),
    ],
  },
]);
